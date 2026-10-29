//! Driving the object-table finder on a real cartridge (ticket W38-02).
//!
//! The finder ([`rf_rebuild::objtable`]) works on a [`Trace`]: for every frame,
//! work RAM, the OAM the PPU holds and the playfield scroll. Sampling at the
//! end of a frame is wrong for games whose logic runs inside the NMI handler:
//! the sample would land mid-update, with some objects already moved and the
//! OAM still the previous frame's. So a frame is stepped an instruction at a
//! time and RAM is copied **at the moment the OAM changes**, i.e. right after
//! the upload (NES `$4014` DMA, nesdev "PPU OAM"; SNES DMA to `$2104`,
//! fullsnes "OAM"). At that instant the RAM is exactly the state the sprites
//! were built from. Plain stepping; no atlas or observer is needed.
//!
//! Only offsets, strides and counts leave a caller (law 5, D-021).

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, Step};
use rf_nes::core::NesCore;
use rf_rebuild::objtable::{Frame, Sprite, Trace};
use rf_snes::core::SnesCore;

struct Null;

impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// NES sprites parked off screen sit at `y >= 0xEF` (nesdev "PPU OAM").
const NES_HIDDEN_Y: u8 = 0xEF;
/// The scanline whose scroll stands for the playfield: below a status bar.
const NES_PLAY_LINE: usize = 200;
/// Low work RAM searched on the SNES: where games keep object state.
pub const SNES_RAM_WINDOW: usize = 0x2000;

/// RAM and OAM as they stood right after an OAM upload.
#[derive(Debug, Clone)]
pub struct Capture {
    pub ram: Vec<u8>,
    pub oam: Vec<u8>,
}

/// Step one frame an instruction at a time; the capture is the last time the
/// OAM changed (`None` when it never did).
fn capture_frame<C: EmulatorCore>(
    core: &mut C,
    oam: fn(&C) -> &[u8],
    snap: impl Fn(&C) -> Vec<u8>,
) -> Option<Capture> {
    let mut last = oam(core).to_vec();
    let mut cap = None;
    // Bounded: a frame is a few tens of thousands of instructions; a core
    // that never reports a frame boundary would otherwise hang the caller.
    for _ in 0..2_000_000u32 {
        let r = core.step(Step::Instruction, &mut Null);
        let now = oam(core);
        if now != last.as_slice() {
            last.clear();
            last.extend_from_slice(now);
            cap = Some(Capture {
                ram: snap(core),
                oam: last.clone(),
            });
        }
        if r.frame_complete {
            break;
        }
    }
    cap
}

/// Arm the per-line scroll record (a passive copy of what the PPU computed).
pub fn nes_prepare(core: &mut NesCore) {
    core.bus_mut().set_bg_under_capture(true);
}

/// Run one NES frame.
pub fn nes_step_frame(core: &mut NesCore, input: &InputFrame) -> Option<Capture> {
    core.bus_mut()
        .set_controller_buttons(0, input.ports[0] as u8);
    core.bus_mut()
        .set_controller_buttons(1, input.ports[1] as u8);
    capture_frame(
        core,
        |c| c.bus().oam().as_slice(),
        |c| c.bus().ram().to_vec(),
    )
}

/// The playfield scroll of the frame just run.
#[must_use]
pub fn nes_cam(core: &NesCore) -> (u16, u16) {
    core.bus()
        .line_scroll()
        .get(NES_PLAY_LINE)
        .copied()
        .unwrap_or((0, 0))
}

/// Run one SNES frame (the pad words are latched into the bus first).
pub fn snes_step_frame(core: &mut SnesCore, input: &InputFrame) -> Option<Capture> {
    let ports = &mut core.system_mut().bus.joypads.ports;
    for (slot, word) in ports.iter_mut().zip(input.ports) {
        *slot = word;
    }
    capture_frame(
        core,
        |c| c.system().bus.ppu.oam.as_slice(),
        |c| c.system().bus.wram[..SNES_RAM_WINDOW].to_vec(),
    )
}

/// BG1's scroll (the playfield in the games studied).
#[must_use]
pub fn snes_cam(core: &SnesCore) -> (u16, u16) {
    let bg = &core.system().bus.ppu.bgs[0];
    (bg.hofs, bg.vofs)
}

fn nes_sprites(oam: &[u8]) -> Vec<Sprite> {
    oam.chunks_exact(4)
        .enumerate()
        .filter(|(_, e)| e[0] < NES_HIDDEN_Y)
        .map(|(i, e)| Sprite {
            index: i as u16,
            x: e[3],
            y: e[0],
        })
        .collect()
}

/// The 128 SNES entries decoded with the high table (fullsnes "OAM"): a sprite
/// whose ninth x bit puts it past the right edge, or whose y is below the
/// picture, is not drawn.
fn snes_sprites(oam: &[u8]) -> Vec<Sprite> {
    rf_snes::debug::decode_oam(oam)
        .into_iter()
        .filter(|s| s.y < 224 && (-16..256).contains(&s.x))
        .map(|s| Sprite {
            index: u16::from(s.index),
            x: s.x as u8,
            y: s.y,
        })
        .collect()
}

/// Which console's OAM layout a recorder decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    Nes,
    Snes,
}

/// Builds a [`Trace`] from per-frame captures.
///
/// The scroll that governs the picture a capture's sprites appear in is the
/// one the PPU used for that picture. Which run of the frame loop that is
/// depends on where the core cuts frames relative to the upload, so the
/// pairing is chosen by the caller: `cam_next` pairs a capture with the scroll
/// of the *following* run.
pub struct Recorder {
    pub trace: Trace,
    console: Console,
    cam_next: bool,
    pending: Option<Capture>,
}

impl Recorder {
    #[must_use]
    pub fn new(console: Console, cam_next: bool) -> Self {
        let trace = match console {
            Console::Nes => Trace {
                ram_base: 0,
                wrap: 512,
                frames: Vec::new(),
            },
            Console::Snes => Trace {
                ram_base: 0x7E_0000,
                wrap: 65536,
                frames: Vec::new(),
            },
        };
        Self {
            trace,
            console,
            cam_next,
            pending: None,
        }
    }

    fn finish(&mut self, cap: Capture, cam: (u16, u16)) {
        let sprites = match self.console {
            Console::Nes => nes_sprites(&cap.oam),
            Console::Snes => snes_sprites(&cap.oam),
        };
        self.trace.frames.push(Frame {
            ram: cap.ram,
            oam: cap.oam,
            sprites,
            cam_x: cam.0,
            cam_y: cam.1,
        });
    }

    /// Record one run: its capture and the scroll the run ended with.
    pub fn push(&mut self, cap: Option<Capture>, cam: (u16, u16)) {
        if self.cam_next {
            if let Some(prev) = self.pending.take() {
                self.finish(prev, cam);
            }
            self.pending = cap;
        } else if let Some(c) = cap {
            self.finish(c, cam);
        }
    }
}

/// A trace file for iterating on the finder without replaying the game
/// (little-endian: console tag, then per frame the cam and the three
/// length-prefixed blocks; sprites are rebuilt from the OAM on load).
pub fn save_trace(t: &Trace, console: Console, path: &std::path::Path) -> std::io::Result<()> {
    let mut b = vec![u8::from(console == Console::Snes)];
    b.extend((t.frames.len() as u32).to_le_bytes());
    for f in &t.frames {
        b.extend(f.cam_x.to_le_bytes());
        b.extend(f.cam_y.to_le_bytes());
        for blk in [&f.ram, &f.oam] {
            b.extend((blk.len() as u32).to_le_bytes());
            b.extend(blk.iter());
        }
    }
    std::fs::write(path, b)
}

/// Read a file [`save_trace`] wrote.
pub fn load_trace(path: &std::path::Path) -> std::io::Result<Trace> {
    let b = std::fs::read(path)?;
    let bad = || std::io::Error::other("truncated trace file");
    let mut at = 1usize;
    let mut take = |n: usize| -> std::io::Result<&[u8]> {
        let s = b.get(at..at + n).ok_or_else(bad)?;
        at += n;
        Ok(s)
    };
    let le32 = |s: &[u8]| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize;
    let console = if *b.first().ok_or_else(bad)? == 1 {
        Console::Snes
    } else {
        Console::Nes
    };
    let n = le32(take(4)?);
    let mut rec = Recorder::new(console, false);
    for _ in 0..n {
        let c = take(4)?;
        let cam = (
            u16::from_le_bytes([c[0], c[1]]),
            u16::from_le_bytes([c[2], c[3]]),
        );
        let l = le32(take(4)?);
        let ram = take(l)?.to_vec();
        let l = le32(take(4)?);
        let oam = take(l)?.to_vec();
        rec.push(Some(Capture { ram, oam }), cam);
    }
    Ok(rec.trace)
}
