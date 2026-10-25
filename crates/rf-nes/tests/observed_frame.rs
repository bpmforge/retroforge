//! Ticket W36-09: `run_frame_observed` on the NES core, over a small
//! synthetic cartridge (no ROM needed). The observed frame must leave the
//! machine bit-identical to `run_frame`, report each instruction's atlas
//! function, log the CPU-bus accesses with their values (register writes
//! included, ROM fetches too), and leave the access log off afterwards.

use rf_core_api::{
    BusAccess, CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind, StepFacts,
    StepObserver,
};
use rf_nes::core::NesCore;
use rf_nes::rebuild::{build_atlas, Atlas};

/// `JSR pal; INC $10; JMP loop`; `pal` = write `$3F00` to `$2006`, `$0F` to
/// `$2007`, `RTS`.
fn rom() -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 1;
    d[5] = 1;
    let mut prg = vec![0xEAu8; 0x4000];
    let main = [
        0x20, 0x10, 0x80, // JSR $8010
        0xE6, 0x10, // INC $10
        0x4C, 0x00, 0x80, // JMP $8000
    ];
    let pal = [
        0xA9, 0x3F, 0x8D, 0x06, 0x20, // LDA #$3F; STA $2006
        0xA9, 0x00, 0x8D, 0x06, 0x20, // LDA #$00; STA $2006
        0xA9, 0x0F, 0x8D, 0x07, 0x20, // LDA #$0F; STA $2007
        0x60,
    ];
    prg[..main.len()].copy_from_slice(&main);
    prg[0x10..0x10 + pal.len()].copy_from_slice(&pal);
    prg[0x3FFA..0x3FFC].copy_from_slice(&0x8000u16.to_le_bytes());
    prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    d.extend(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

struct Quiet;
impl CoreSink for Quiet {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn atlas_of(raw: &[u8]) -> Atlas {
    let mut rec = NesCore::from_ines_bytes(raw).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    for _ in 0..2 {
        rec.run_frame(&InputFrame::empty(), &mut Quiet);
    }
    let log = rec.take_coverage().expect("armed");
    let rom = rf_nes::NesRom::from_ines_bytes(raw).expect("rom");
    build_atlas(rom.prg_rom(), &log)
}

#[derive(Default)]
struct Collect {
    steps: u64,
    frames: u32,
    /// (function, access) pairs for register writes.
    reg_writes: Vec<(Option<u32>, BusAccess)>,
    entries: Vec<u32>,
}
impl StepObserver for Collect {
    fn step(&mut self, facts: StepFacts, accesses: &[BusAccess]) {
        self.steps += 1;
        if let Some(f) = facts.entry_of {
            self.entries.push(f);
        }
        for a in accesses {
            if a.write && (0x2000..0x4000).contains(&a.addr) {
                self.reg_writes.push((facts.function, *a));
            }
        }
    }
    fn frame_end(&mut self) {
        self.frames += 1;
    }
}

fn fingerprint(c: &NesCore) -> u64 {
    let v = c.state_view();
    [v.wram, v.vram, v.cgram, v.oam]
        .iter()
        .flat_map(|s| s.iter())
        .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

fn core(raw: &[u8], atlas: &Atlas) -> NesCore {
    let mut c = NesCore::from_ines_bytes(raw).expect("loads");
    c.reset(ResetKind::Hard);
    c.set_rebuild(Some(atlas.clone())).expect("atlas fits");
    c
}

#[test]
fn observed_frame_is_run_frame() {
    let raw = rom();
    let atlas = atlas_of(&raw);
    let (mut plain, mut watched) = (core(&raw, &atlas), core(&raw, &atlas));
    let mut obs = Collect::default();
    for _ in 0..3 {
        plain.run_frame(&InputFrame::empty(), &mut Quiet);
        assert!(watched.run_frame_observed(&InputFrame::empty(), &mut Quiet, &mut obs));
    }
    assert_eq!(fingerprint(&plain), fingerprint(&watched));
    assert_eq!(plain.peek(0x10), watched.peek(0x10));
    assert_eq!(obs.frames, 3);
    assert!(obs.steps > 100, "the program ran: {}", obs.steps);
    // Nothing is left armed: a later normal frame logs nothing.
    watched.run_frame(&InputFrame::empty(), &mut Quiet);
    assert!(watched.bus().access_log().is_empty());
    assert!(!watched.config().watches.observes());
}

#[test]
fn register_writes_carry_their_values_and_the_executing_function() {
    let raw = rom();
    let atlas = atlas_of(&raw);
    let pal = atlas
        .functions
        .iter()
        .position(|f| f.entry_rom == 0x10)
        .expect("palette routine") as u32;
    let mut c = core(&raw, &atlas);
    let mut obs = Collect::default();
    assert!(c.run_frame_observed(&InputFrame::empty(), &mut Quiet, &mut obs));
    let w06: Vec<u8> = obs
        .reg_writes
        .iter()
        .filter(|(_, a)| a.addr == 0x2006)
        .map(|(_, a)| a.value)
        .take(4)
        .collect();
    assert_eq!(w06, [0x3F, 0x00, 0x3F, 0x00], "values, in order");
    assert!(
        obs.reg_writes.iter().all(|(f, _)| *f == Some(pal)),
        "every PPU write was made by the palette routine"
    );
    assert!(obs.entries.contains(&pal), "the routine's entry was seen");
}
