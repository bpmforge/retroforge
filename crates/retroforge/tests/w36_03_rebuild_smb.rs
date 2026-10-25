//! **W36-03: the block executor on a real cartridge** (ROM-gated, law 5).
//!
//! Records World 1-1 with the W36-01 drive script, builds the atlas, then
//! reruns from power-on with the executor off and on side by side and
//! compares CPU registers, WRAM, VRAM/OAM/palette and the video+audio hash
//! after every frame. A second, different script (Right+B with jumps at
//! other frames) runs through the same atlas to exercise dispatch misses.
//! Only counts are printed; no ROM bytes leave the test (clean room, D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w36_03_rebuild_smb -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use rf_core_api::{
    CoreEvent, CoreSink, CpuRegs, EmulatorCore, InputFrame, Mos6502Regs, PpuPixel, ResetKind,
};
use rf_nes::core::NesCore;
use rf_nes::NesRom;

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const B: u8 = 1 << 1;
const RIGHT: u8 = 1 << 7;
const FRAMES: u32 = 500 + 1300;

fn rom_path(file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join("nes").join(file);
    p.exists().then_some(p)
}

struct Hasher(u64);

impl Hasher {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn feed(&mut self, b: u8) {
        self.0 ^= u64::from(b);
        self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
    }
}

impl CoreSink for Hasher {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.feed(y as u8);
        pixels.iter().for_each(|p| self.feed(p.palette_index));
    }
    fn audio(&mut self, samples: &[i16]) {
        samples.iter().for_each(|s| self.feed(*s as u8));
    }
    fn event(&mut self, _event: CoreEvent) {}
}

/// Two Starts: the first leaves the multicart's game-select screen, the
/// second starts a 1-player game on SMB's own title screen. With only the
/// first (the W36-01/02 scripts) the title screen idles into the attract
/// demo, which ignores the pad: those scripts recorded the demo, not play
/// (checked: `$0770` stays 0 and every later input is irrelevant). Then
/// Right with A pressed 12 frames in every 60.
fn script_one(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => RIGHT | A,
        _ => RIGHT,
    }
}

/// Different: run (B) the whole way, jump for 20 frames in every 90 and a
/// short hop at the start of every 150.
fn script_two(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f => {
            let t = f - 500;
            let jump = t % 90 < 20 || t % 150 < 6;
            RIGHT | B | if jump { A } else { 0 }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snap {
    regs: Mos6502Regs,
    wram: u64,
    other: u64,
    video_audio: u64,
}

fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h = Hasher::new();
    parts.iter().for_each(|p| p.iter().for_each(|&b| h.feed(b)));
    h.0
}

fn snap(core: &NesCore, sink: &Hasher) -> Snap {
    let v = core.state_view();
    let CpuRegs::Mos6502(regs) = v.cpu_regs else {
        panic!("6502");
    };
    Snap {
        regs,
        wram: fnv(&[v.wram]),
        other: fnv(&[v.vram, v.oam, v.cgram]),
        video_audio: sink.0,
    }
}

fn power_on(raw: &[u8]) -> NesCore {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core
}

/// Run `script` on a plain core and a rebuilt core in lockstep, asserting
/// identity after every frame. Returns the rebuilt core.
fn compare(
    raw: &[u8],
    atlas: rf_nes::rebuild::Atlas,
    script: fn(u32) -> u8,
    worlds: bool,
) -> NesCore {
    let mut plain = power_on(raw);
    let mut rebuilt = power_on(raw);
    rebuilt.set_rebuild(Some(atlas)).expect("installs");
    let (mut sp, mut sr) = (Hasher::new(), Hasher::new());
    let mut input = InputFrame::empty();
    let (mut start_x, mut best_x) = (0u16, 0u16);
    for frame in 0..FRAMES {
        input.ports[0] = u16::from(script(frame));
        plain.run_frame(&input, &mut sp);
        rebuilt.run_frame(&input, &mut sr);
        assert_eq!(
            snap(&plain, &sp),
            snap(&rebuilt, &sr),
            "diverged at frame {frame}"
        );
        if worlds && frame == 600 {
            // $0770 OperMode 1 = real play (observed, see script_one).
            assert_eq!(rebuilt.peek(0x0770), 1, "not in play at frame 600");
            assert_eq!((rebuilt.peek(0x075F), rebuilt.peek(0x0760)), (0, 0));
            start_x = world_x(&rebuilt);
        }
        if worlds && frame > 600 {
            best_x = best_x.max(world_x(&rebuilt));
        }
    }
    if worlds {
        assert_eq!(
            (rebuilt.peek(0x075F), rebuilt.peek(0x0760)),
            (0, 0),
            "left 1-1"
        );
        println!("Mario world x on the rebuilt path: frame 600 {start_x}, furthest {best_x}");
        assert!(best_x > start_x + 200, "Mario never got going in 1-1");
    }
    rebuilt
}

/// Mario's x in the level: page ($006D) and on-screen x ($0086) are what
/// profiles/nes/super-mario-bros/profile.toml records; scroll adds to it.
fn world_x(core: &NesCore) -> u16 {
    u16::from(core.peek(0x006D)) * 256 + u16::from(core.peek(0x0086))
}

fn report(label: &str, core: &NesCore) {
    let s = core.rebuild_stats().expect("installed");
    println!(
        "{label}: {} instructions ({} observed, {} inferred, {} fallback = {:.3}% fallback), \
         {} interrupt entries, {} indirect checked, {} dispatch misses ({} distinct, {} dropped)",
        s.instructions(),
        s.observed,
        s.inferred,
        s.fallback,
        s.fallback_rate() * 100.0,
        s.interrupt_entries,
        s.indirect_checked,
        s.dispatch_misses,
        s.delta_len,
        s.delta_dropped
    );
    assert!(s.observed > 0 && s.indirect_checked > 0);
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn smb_level_one_is_bit_identical_through_the_rebuilt_path() {
    let Some(path) = rom_path(FILE) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");

    // Record script one.
    let mut rec = NesCore::from_ines_bytes(&raw).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    let mut sink = Hasher::new();
    let mut input = InputFrame::empty();
    for frame in 0..FRAMES {
        input.ports[0] = u16::from(script_one(frame));
        rec.run_frame(&input, &mut sink);
    }
    let log = rec.take_coverage().expect("armed");
    let prg = NesRom::from_ines_bytes(&raw).expect("parses");
    let t = Instant::now();
    let atlas = rf_nes::rebuild::build_atlas(prg.prg_rom(), &log);
    println!("atlas built in {:?}", t.elapsed());

    let one = compare(&raw, atlas.clone(), script_one, true);
    report("script one (same as recorded)", &one);
    println!("script one deltas: {}", one.rebuild_deltas().len());

    let two = compare(&raw, atlas, script_two, false);
    report("script two (Right+B, other jumps)", &two);
    println!(
        "script two at end: world/level {}/{}, mode {}, lives {}, Mario world x {}",
        two.peek(0x075F),
        two.peek(0x0760),
        two.peek(0x0770),
        two.peek(0x075A),
        world_x(&two)
    );
}
