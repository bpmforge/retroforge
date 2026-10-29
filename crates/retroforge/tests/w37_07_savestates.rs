//! **W37-07: savestates and rewind across tiers** (`docs/design/REBUILD_LAB.md`,
//! "W37-07"). A savestate holds machine state only, so it must load into any
//! tier: the plain interpreter (Rebuild off), the rebuilt path, the rebuilt
//! path with a hook armed, and the rebuilt path with a function demoted. This
//! file saves on each tier, loads into each tier, replays, and demands the
//! per-frame machine digest of a straight plain run. It also pins the resume
//! rule (first block on the interpreter after a load, then back to the atlas)
//! and that a call in flight at the save is not reported after the load.
//!
//! Synthetic cartridges here (no ROM bytes, law 5); the `#[ignore]`d tests at
//! the bottom run the same matrix on SMB and SMW from `RF_SCREENS_ROMS_DIR`:
//!
//! ```text
//! cargo test --release -p retroforge --test w37_07_savestates -- --ignored --nocapture
//! ```

// Frame loops index the pad and the reference digests together.
#![allow(clippy::needless_range_loop)]

use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore, FunctionEvent, InputFrame, PpuPixel, RebuildStats,
    ResetKind, StateError, StateWriter,
};
use rf_harness::fuzz::Savestate;
use rf_rebuild::Atlas;
use rf_snes::core::SnesCore;

// ------------------------------------------------------------------ machines

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Console {
    Nes,
    Snes,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tier {
    Off,
    On,
    Hooked,
    Demoted,
}

const TIERS: [Tier; 4] = [Tier::Off, Tier::On, Tier::Hooked, Tier::Demoted];

struct Rig {
    name: &'static str,
    console: Console,
    rom: Vec<u8>,
    atlas: Atlas,
    hook_ids: Vec<u32>,
    demote_ids: Vec<u32>,
    pad: Vec<u16>,
    /// Frames run before the save, and in total.
    n: usize,
    m: usize,
    /// The save must land while a hooked call is in flight.
    in_flight: bool,
}

enum Mach {
    Nes(Box<rf_nes::core::NesCore>),
    Snes(Box<SnesCore>),
}

enum Snap {
    Nes(Savestate),
    Snes(rf_state::Container),
}

#[derive(Default)]
struct Buf(Vec<u8>);

impl StateWriter for Buf {
    fn write_all(&mut self, b: &[u8]) -> Result<(), StateError> {
        self.0.extend_from_slice(b);
        Ok(())
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Default)]
struct Sink {
    events: Vec<FunctionEvent>,
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
    fn function_event(&mut self, ev: FunctionEvent, _ctx: &mut dyn rf_core_api::HookCtx) {
        self.events.push(ev);
    }
}

impl Mach {
    fn power_on(console: Console, rom: &[u8]) -> Self {
        match console {
            Console::Nes => {
                let mut c = rf_nes::core::NesCore::from_ines_bytes(rom).expect("loads");
                c.reset(ResetKind::Hard);
                Mach::Nes(Box::new(c))
            }
            Console::Snes => {
                let mut c = SnesCore::load(rom).expect("loads");
                c.reset(ResetKind::Hard);
                Mach::Snes(Box::new(c))
            }
        }
    }

    fn configure(&mut self, rig: &Rig, tier: Tier) {
        if tier == Tier::Off {
            return;
        }
        let atlas = Some(rig.atlas.clone());
        match self {
            Mach::Nes(c) => c.set_rebuild(atlas).expect("installs"),
            Mach::Snes(c) => c.set_rebuild(atlas).expect("installs"),
        }
        if tier == Tier::Hooked {
            self.core().set_function_hooks(&rig.hook_ids);
        }
        if tier == Tier::Demoted {
            let took = match self {
                Mach::Nes(c) => c.set_rebuild_demoted(&rig.demote_ids),
                Mach::Snes(c) => c.set_rebuild_demoted(&rig.demote_ids),
            };
            assert_eq!(took, rig.demote_ids.len(), "{}: demoted ids took", rig.name);
        }
    }

    fn core(&mut self) -> &mut dyn EmulatorCore {
        match self {
            Mach::Nes(c) => c.as_mut(),
            Mach::Snes(c) => c.as_mut(),
        }
    }

    fn frame(&mut self, mask: u16, sink: &mut Sink) {
        match self {
            Mach::Nes(c) => {
                let mut input = InputFrame::empty();
                input.ports[0] = mask;
                c.run_frame(&input, sink);
            }
            Mach::Snes(c) => {
                c.system_mut().bus.joypads.ports[0] = mask;
                c.run_frame(&InputFrame::empty(), sink);
            }
        }
    }

    /// Registers plus every savestate region, one hash each.
    fn digest_all(&self) -> Vec<u64> {
        match self {
            Mach::Nes(c) => rf_nes::StateRegion::ALL
                .iter()
                .map(|&r| {
                    let mut b = Buf::default();
                    c.bus().save_region(c.cpu(), r, &mut b).expect("saves");
                    fnv(&b.0)
                })
                .collect(),
            Mach::Snes(c) => {
                let mut out = vec![fnv(format!("{:?}", c.system().cpu.pc24()).as_bytes())];
                for r in rf_snes::state::StateRegion::ALL {
                    let mut b = Buf::default();
                    c.system().save_region(r, &mut b).expect("saves");
                    out.push(fnv(&b.0));
                }
                out
            }
        }
    }

    fn capture(&self) -> Snap {
        match self {
            Mach::Nes(c) => Snap::Nes(Savestate::capture(c).expect("captures")),
            Mach::Snes(c) => Snap::Snes(
                retroforge::snes_save_state::save_state(c.system(), [0; 32], 0).expect("saves"),
            ),
        }
    }

    /// Load the way the app does (`EmuStepper::restore`): the state, then the
    /// executor is told.
    fn load(&mut self, snap: &Snap) {
        match (self, snap) {
            (Mach::Nes(c), Snap::Nes(s)) => s.restore(c).expect("loads"),
            (Mach::Snes(c), Snap::Snes(k)) => {
                retroforge::snes_save_state::load_state(c.system_mut(), [0; 32], k).expect("loads");
                c.reset_hook_frames();
            }
            _ => panic!("console mismatch"),
        }
    }

    fn stats(&self) -> Option<RebuildStats> {
        match self {
            Mach::Nes(c) => c.rebuild_stats(),
            Mach::Snes(c) => c.rebuild_stats(),
        }
    }

    fn depth(&self) -> usize {
        match self {
            Mach::Nes(c) => c.rebuild_shadow_depth(),
            Mach::Snes(c) => c.rebuild_shadow_depth(),
        }
    }
}

fn record_atlas(console: Console, rom: &[u8], pad: &[u16]) -> Atlas {
    let mut m = Mach::power_on(console, rom);
    m.core().set_coverage_recording(true);
    let mut sink = Sink::default();
    for &mask in pad {
        m.frame(mask, &mut sink);
    }
    let log = m.core().take_coverage().expect("armed");
    match console {
        Console::Nes => rf_harness::atlas::build_nes_atlas(rom, &log).expect("atlas"),
        Console::Snes => rf_snes::rebuild::build_atlas_from_image(rom, &log).expect("atlas"),
    }
}

// ------------------------------------------------------- synthetic cartridges

/// NROM-128 at `$C000`. The main loop strobes and reads pad 1 into `$11`,
/// calls `slow` (about eleven frames of `DEY`/`DEX` counting, so a frame
/// boundary usually falls inside the call), then bumps `$12`; NMI bumps `$00`.
fn nes_rom() -> Vec<u8> {
    let mut prg = vec![0xEAu8; 0x4000];
    let mut put = |at: usize, code: &[u8]| prg[at..at + code.len()].copy_from_slice(code);
    put(
        0x00,
        &[
            0x78, 0xD8, 0xA2, 0xFF, 0x9A, // SEI CLD LDX #$FF TXS
            0xA9, 0x80, 0x8D, 0x00, 0x20, // NMI on
            // main = $C00A
            0xA9, 0x01, 0x8D, 0x16, 0x40, 0xA9, 0x00, 0x8D, 0x16, 0x40, // strobe
            0xAD, 0x16, 0x40, 0x29, 0x01, 0x18, 0x65, 0x11, 0x85, 0x11, // $11 += A button
            0x20, 0x30, 0xC0, // JSR slow
            0xE6, 0x12, // INC $12
            0x4C, 0x0A, 0xC0, // JMP main
        ],
    );
    put(
        0x30,
        &[
            0xA2, 0xFF, // LDX #$FF
            0xA0, 0xFF, // o: LDY #$FF
            0x88, 0xD0, 0xFD, // i: DEY ; BNE i
            0xCA, 0xD0, 0xF8, // DEX ; BNE o
            0xE6, 0x13, 0x60, // INC $13 ; RTS
        ],
    );
    put(0x100, &[0xE6, 0x00, 0x40]); // NMI: INC $00 ; RTI
    put(0x3FFA, &[0x00, 0xC1, 0x00, 0xC0, 0x00, 0xC1]);
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 1;
    d[5] = 1;
    d.extend(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

fn nes_pad(frames: usize) -> Vec<u16> {
    (0..frames).map(|f| (f as u16 * 0x2D) & 0xFF).collect()
}

const NMI: usize = 0x8200;
const IRQ: usize = 0x8210;

fn put(rom: &mut [u8], addr: usize, code: &[u8]) {
    rom[addr - 0x8000..addr - 0x8000 + code.len()].copy_from_slice(code);
}

/// The W36-08b LoROM (REP/SEP widths, JSL/RTL, work-RAM code, `JSR (abs,X)`,
/// NMI and V-IRQ) plus a long-running `JSL $018010` (a 65,536-pass `INX`
/// loop, several frames) and a read of the joypad auto-read into `$0060`.
fn snes_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x1_0000];
    let base = 0x7FC0;
    rom[base + 0x15] = 0x20;
    rom[base + 0x17] = 6;
    let sum: u16 = 0xBEEF;
    rom[base + 0x1C..base + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    rom[base + 0x1E..base + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[0x7FEA..0x7FEC].copy_from_slice(&(NMI as u16).to_le_bytes());
    rom[0x7FEE..0x7FF0].copy_from_slice(&(IRQ as u16).to_le_bytes());
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());

    let mut c: Vec<u8> = vec![
        0x78, 0x18, 0xFB, 0xC2, 0x38, 0xA2, 0xFF, 0x1F, 0x9A, 0xE2, 0x20, // init
        0xA9, 0xEE, 0x8D, 0x00, 0x01, 0xA9, 0x30, 0x8D, 0x01, 0x01, // RAM routine
        0xA9, 0x00, 0x8D, 0x02, 0x01, 0xA9, 0x6B, 0x8D, 0x03, 0x01, //
        0xA9, 100, 0x8D, 0x09, 0x42, 0x9C, 0x0A, 0x42, // V-IRQ line
        0xA9, 0xA1, 0x8D, 0x00, 0x42, // NMI + V-IRQ + auto-joypad
        0x58, // CLI
    ];
    let main = 0x8000 + c.len();
    c.extend_from_slice(&[
        0xC2,
        0x30, // REP #$30
        0x22,
        0x10,
        0x80,
        0x01, // JSL $018010   (the long routine, 16-bit X)
        0x22,
        0x00,
        0x80,
        0x01, // JSL $018000
        0xE2,
        0x30,
        0x22,
        0x00,
        0x80,
        0x01, // SEP #$30 ; JSL $018000
        0x22,
        0x00,
        0x01,
        0x7E, // JSL $7E0100 (work-RAM code)
        0xC2,
        0x30,
        0xAD,
        0x12,
        0x00,
        0x29,
        0x01,
        0x00,
        0x0A,
        0xAA, // table index
        0xFC,
        0x00,
        0x8F, // JSR ($8F00,X)
        0xE2,
        0x20,
        0xAD,
        0x11,
        0x00,
        0x48,
        0x28, // width from a PLP
        0xA9,
        0x12,
        0xEA,
        0x8D,
        0x50,
        0x00,
        0xA2,
        0x34,
        0xEA,
        0x8E,
        0x52,
        0x00,
        0xAD,
        0x18,
        0x42,
        0x8D,
        0x60,
        0x00, // LDA $4218 ; STA $0060 (either width)
        0xC2,
        0x30,
        0xCB, // REP #$30 ; WAI
        0x4C,
        main as u8,
        (main >> 8) as u8, // JMP main
    ]);
    put(&mut rom, 0x8000, &c);
    put(&mut rom, 0x8E00, &[0xEE, 0x40, 0x00, 0x60]);
    put(&mut rom, 0x8E10, &[0xEE, 0x42, 0x00, 0x60]);
    put(&mut rom, 0x8F00, &[0x00, 0x8E, 0x10, 0x8E]);
    put(&mut rom, NMI, &[0xEE, 0x20, 0x00, 0x40]);
    put(
        &mut rom,
        IRQ,
        &[
            0x08, 0x48, 0xAD, 0x11, 0x42, 0xEE, 0x21, 0x00, 0x68, 0x28, 0x40,
        ],
    );
    // Bank 1: the short sub, and the long one: LDX #0 ; INX ; BNE ; RTL.
    rom[0x8000..0x8007].copy_from_slice(&[0xA9, 0x01, 0xEA, 0x8D, 0x31, 0x00, 0x6B]);
    rom[0x8010..0x8017].copy_from_slice(&[0xA2, 0x00, 0x00, 0xE8, 0xD0, 0xFD, 0x6B]);
    rom
}

fn function_at(atlas: &Atlas, entry_rom: u32) -> u32 {
    atlas
        .functions
        .iter()
        .position(|f| f.entry_rom == entry_rom)
        .unwrap_or_else(|| panic!("no function at {entry_rom:#x}")) as u32
}

/// Build a synthetic rig; `n` is the first frame from 4 on that ends with a
/// hooked call in flight.
fn synthetic(console: Console) -> Rig {
    let (rom, entry, name) = match console {
        Console::Nes => (nes_rom(), 0x30, "synthetic NES"),
        Console::Snes => (snes_rom(), 0x8010, "synthetic SNES"),
    };
    let pad = nes_pad(80);
    let atlas = record_atlas(console, &rom, &pad[..40]);
    let id = function_at(&atlas, entry);
    let mut rig = Rig {
        name,
        console,
        rom,
        atlas,
        hook_ids: vec![id],
        demote_ids: vec![id],
        pad,
        n: 0,
        m: 0,
        in_flight: true,
    };
    let mut m = Mach::power_on(console, &rig.rom);
    m.configure(&rig, Tier::Hooked);
    let mut sink = Sink::default();
    for f in 0..20 {
        m.frame(rig.pad[f], &mut sink);
        if f >= 4 && m.depth() > 0 {
            rig.n = f + 1;
            break;
        }
    }
    assert!(rig.n > 0, "{name}: no frame ends inside the hooked call");
    rig.m = rig.n + 20;
    rig
}

// ------------------------------------------------------------------- the matrix

/// Per frame, the hooked events of a straight hooked run, and the digest of a
/// straight plain run after `f` frames.
fn references(rig: &Rig) -> (Vec<Vec<u64>>, Vec<Vec<FunctionEvent>>) {
    let mut plain = Mach::power_on(rig.console, &rig.rom);
    let mut hooked = Mach::power_on(rig.console, &rig.rom);
    hooked.configure(rig, Tier::Hooked);
    let mut digests = vec![plain.digest_all()];
    let mut events = Vec::new();
    for f in 0..rig.m {
        let mut sink = Sink::default();
        plain.frame(rig.pad[f], &mut Sink::default());
        digests.push(plain.digest_all());
        hooked.frame(rig.pad[f], &mut sink);
        assert_eq!(
            hooked.digest_all(),
            digests[f + 1],
            "{}: hooked tier, frame {f}",
            rig.name
        );
        events.push(sink.events);
    }
    (digests, events)
}

/// The events a straight hooked run delivers after the save, minus the Exits
/// of calls that were already in flight at it (a load drops those frames).
fn expected_after_load(per_frame: &[Vec<FunctionEvent>], n: usize) -> Vec<FunctionEvent> {
    let mut pre: Vec<u32> = Vec::new();
    for ev in per_frame[..n].iter().flatten() {
        match *ev {
            FunctionEvent::Enter { function } => pre.push(function),
            FunctionEvent::Exit { .. } => {
                pre.pop();
            }
        }
    }
    let mut opened_after = 0usize;
    let mut out = Vec::new();
    for ev in per_frame[n..].iter().flatten() {
        match *ev {
            FunctionEvent::Enter { .. } => {
                opened_after += 1;
                out.push(*ev);
            }
            FunctionEvent::Exit { .. } if opened_after > 0 => {
                opened_after -= 1;
                out.push(*ev);
            }
            FunctionEvent::Exit { .. } => {
                pre.pop();
            }
        }
    }
    out
}

/// Exact on the synthetic rigs (strictly nested calls). A real game can return
/// through a pre-save frame while post-load frames are above it (an RTS jump
/// table), which a straight run reports as two Exits and a loaded run as none,
/// so the real rigs check the Enters and that nothing is orphaned.
fn same_events(rig: &Rig, got: &[FunctionEvent], want: &[FunctionEvent], label: &str) {
    if rig.in_flight {
        assert_eq!(got, want, "{label}: hook events after the load");
        return;
    }
    let enters = |v: &[FunctionEvent]| {
        v.iter()
            .filter(|e| matches!(e, FunctionEvent::Enter { .. }))
            .count()
    };
    assert_eq!(enters(got), enters(want), "{label}: Enters after the load");
    assert!(got.len() <= want.len(), "{label}: no extra Exit");
}

fn well_formed(events: &[FunctionEvent]) -> bool {
    let mut open = 0i64;
    for ev in events {
        match ev {
            FunctionEvent::Enter { .. } => open += 1,
            FunctionEvent::Exit { .. } => open -= 1,
        }
        if open < 0 {
            return false;
        }
    }
    true
}

/// Save on every tier, load into every tier, replay, compare with the
/// straight plain run; then rewind in place on each tier.
fn matrix(rig: &Rig) {
    let (digests, events) = references(rig);
    let after = expected_after_load(&events, rig.n);
    let mut resume_len: Option<u64> = None;
    for save_tier in TIERS {
        let mut a = Mach::power_on(rig.console, &rig.rom);
        a.configure(rig, save_tier);
        for f in 0..rig.n {
            a.frame(rig.pad[f], &mut Sink::default());
        }
        let label = format!("{} save {save_tier:?}", rig.name);
        assert_eq!(
            a.digest_all(),
            digests[rig.n],
            "{label}: the save-side machine"
        );
        if save_tier == Tier::Hooked && rig.in_flight {
            assert!(a.depth() > 0, "{label}: the save is mid-call");
        }
        let snap = a.capture();
        for load_tier in TIERS {
            let label = format!("{label} -> load {load_tier:?}");
            let mut b = Mach::power_on(rig.console, &rig.rom);
            b.configure(rig, load_tier);
            // Let b run first so that, hooked, it holds frames of its own.
            for f in 0..4 {
                b.frame(rig.pad[f], &mut Sink::default());
            }
            let before = b.stats();
            b.load(&snap);
            assert_eq!(b.depth(), 0, "{label}: a load drops the in-flight frames");
            assert_eq!(b.digest_all(), digests[rig.n], "{label}: loaded machine");
            let mut sink = Sink::default();
            for f in rig.n..rig.m {
                b.frame(rig.pad[f], &mut sink);
                assert_eq!(b.digest_all(), digests[f + 1], "{label}: replay frame {f}");
            }
            assert!(well_formed(&sink.events), "{label}: {:?}", sink.events);
            if load_tier == Tier::Hooked {
                same_events(rig, &sink.events, &after, &label);
            } else {
                assert!(sink.events.is_empty(), "{label}: no hooks, no events");
            }
            match (before, b.stats()) {
                (None, None) => assert_eq!(load_tier, Tier::Off),
                (Some(b0), Some(b1)) => {
                    assert_ne!(load_tier, Tier::Off);
                    assert_eq!(b0.fallback_after_load, 0, "{label}");
                    let d = b1.fallback_after_load;
                    assert!(d >= 1, "{label}: the first block ran on the interpreter");
                    // One block, whatever the tier: the same length each time.
                    assert_eq!(*resume_len.get_or_insert(d), d, "{label}");
                    assert!(b1.fallback >= d);
                    if load_tier != Tier::Demoted || rig.in_flight {
                        assert!(b1.observed > b0.observed, "{label}: back on the atlas");
                    }
                }
                other => panic!("{label}: stats {other:?}"),
            }
            if load_tier == save_tier {
                // Rewind in place: run ahead of the save, then load it back.
                for f in rig.n..rig.n + 3 {
                    a.frame(rig.pad[f], &mut Sink::default());
                }
                assert_eq!(a.digest_all(), digests[rig.n + 3], "{label}: ahead");
                a.load(&snap);
                let mut sink = Sink::default();
                for f in rig.n..rig.m {
                    a.frame(rig.pad[f], &mut sink);
                    assert_eq!(a.digest_all(), digests[f + 1], "{label}: rewound frame {f}");
                }
                assert!(well_formed(&sink.events), "{label}: {:?}", sink.events);
                if load_tier == Tier::Hooked {
                    same_events(rig, &sink.events, &after, &label);
                }
                // Back to the save for the next load tier.
                a.load(&snap);
            }
        }
    }
    println!("{}: resumed block = {resume_len:?} instructions", rig.name);
}

#[test]
fn nes_saves_load_across_all_tiers_and_replay_to_the_same_hashes() {
    matrix(&synthetic(Console::Nes));
}

#[test]
fn snes_saves_load_across_all_tiers_and_replay_to_the_same_hashes() {
    matrix(&synthetic(Console::Snes));
}

#[test]
fn the_in_flight_call_is_not_reported_after_a_load_but_later_calls_are() {
    for console in [Console::Nes, Console::Snes] {
        let rig = synthetic(console);
        let (_, events) = references(&rig);
        let in_flight = events[..rig.n]
            .iter()
            .flatten()
            .filter(|e| matches!(e, FunctionEvent::Enter { .. }))
            .count()
            - events[..rig.n]
                .iter()
                .flatten()
                .filter(|e| matches!(e, FunctionEvent::Exit { .. }))
                .count();
        assert_eq!(in_flight, 1, "{}: one call in flight at the save", rig.name);
        // The straight run delivers its Exit first; the loaded run never does.
        let straight: Vec<_> = events[rig.n..].iter().flatten().copied().collect();
        assert!(
            matches!(straight[0], FunctionEvent::Exit { .. }),
            "{}",
            rig.name
        );
        let after = expected_after_load(&events, rig.n);
        assert_eq!(
            after.len() + 1,
            straight.len(),
            "{}: exactly one dropped",
            rig.name
        );
        assert!(
            after
                .iter()
                .any(|e| matches!(e, FunctionEvent::Enter { .. })),
            "{}: calls made after the load still report",
            rig.name
        );
    }
}

// ---------------------------------------------- the shipped path: EmuStepper

/// `EmuStepper::snapshot`/`restore` (rewind, run-ahead's shadow machine, the
/// shadow check) across tiers: the state hash after every replayed frame is
/// the plain run's.
#[test]
fn stepper_snapshots_restore_across_tiers_with_the_plain_runs_hashes() {
    use retroforge::stepper::EmuStepper;
    for console in [Console::Nes, Console::Snes] {
        let rig = synthetic(console);
        let open = |tier: Tier| {
            let mut s = match console {
                Console::Nes => EmuStepper::from_ines_bytes(&rig.rom).expect("opens"),
                Console::Snes => EmuStepper::from_snes_bytes(&rig.rom).expect("opens"),
            };
            if tier != Tier::Off {
                s.set_rebuild_atlas(Some(Box::new(rig.atlas.clone())))
                    .expect("installs");
            }
            if tier == Tier::Demoted {
                let key = s
                    .rebuild_function_key(rig.demote_ids[0])
                    .expect("function key");
                assert_eq!(s.set_rebuild_demoted(&[key]), 1);
            }
            s
        };
        let run = |s: &mut EmuStepper, f: usize| {
            let mut input = InputFrame::empty();
            input.ports[0] = rig.pad[f];
            s.latch_and_advance_frame(input, &mut Sink::default());
            s.snapshot().expect("snapshot").state_hash()
        };
        let mut plain = open(Tier::Off);
        let reference: Vec<_> = (0..rig.m).map(|f| run(&mut plain, f)).collect();
        for save in [Tier::Off, Tier::On, Tier::Demoted] {
            let mut a = open(save);
            for f in 0..rig.n {
                assert_eq!(
                    run(&mut a, f),
                    reference[f],
                    "{console:?} {save:?} frame {f}"
                );
            }
            let snap = a.snapshot().expect("snapshot");
            for load in [Tier::Off, Tier::On, Tier::Demoted] {
                let mut b = open(load);
                b.restore(&snap).expect("restores");
                for f in rig.n..rig.m {
                    assert_eq!(
                        run(&mut b, f),
                        reference[f],
                        "{console:?} {save:?}->{load:?} frame {f}"
                    );
                }
                if load != Tier::Off {
                    let s = b.rebuild_stats().expect("on");
                    assert!(s.fallback_after_load >= 1, "{console:?} {load:?}: {s:?}");
                }
            }
            // Rewind: the same stepper goes back to its own snapshot.
            a.restore(&snap).expect("rewinds");
            for f in rig.n..rig.m {
                assert_eq!(
                    run(&mut a, f),
                    reference[f],
                    "{console:?} {save:?} rewind {f}"
                );
            }
        }
    }
}

// ------------------------------------------------------------ real cartridges

mod real {
    use super::*;
    use std::io::Read as _;
    use std::path::PathBuf;

    fn roms_root() -> PathBuf {
        PathBuf::from(
            std::env::var("RF_SCREENS_ROMS_DIR")
                .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string()),
        )
    }

    fn load(dir: &str, file: &str) -> Option<Vec<u8>> {
        let bytes = std::fs::read(roms_root().join(dir).join(file)).ok()?;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).ok()?;
            if entry.is_file() {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).ok()?;
                if rf_cart::Cartridge::load(&buf).is_ok() {
                    return Some(buf);
                }
            }
        }
        None
    }

    /// Hook every called function; demote every fifth function.
    fn rig(console: Console, name: &'static str, rom: Vec<u8>, pad: Vec<u16>, n: usize) -> Rig {
        let m = n + 30;
        assert!(pad.len() >= m, "{name}: script too short");
        let atlas = record_atlas(console, &rom, &pad[..m]);
        let hook_ids: Vec<u32> = atlas
            .functions
            .iter()
            .enumerate()
            .filter(|(_, f)| f.entry_kinds & rf_rebuild::atlas::entry::CALL != 0)
            .map(|(i, _)| i as u32)
            .collect();
        let demote_ids: Vec<u32> = (0..atlas.functions.len() as u32).step_by(5).collect();
        println!(
            "{name}: {} functions, {} hooked, {} demoted",
            atlas.functions.len(),
            hook_ids.len(),
            demote_ids.len()
        );
        Rig {
            name,
            console,
            rom,
            atlas,
            hook_ids,
            demote_ids,
            pad,
            n,
            m,
            in_flight: false,
        }
    }

    const START: u16 = 1 << 3;
    const A: u16 = 1;
    const RIGHT: u16 = 1 << 7;

    /// The W36-03 script: two Starts, then Right with A pressed 12 in 60.
    fn smb_pad(frames: usize) -> Vec<u16> {
        (0..frames as u32)
            .map(|f| match f {
                0..=199 => 0,
                200..=204 | 400..=404 => START,
                205..=399 | 405..=499 => 0,
                f if (f - 500) % 60 < 12 => RIGHT | A,
                _ => RIGHT,
            })
            .collect()
    }

    #[test]
    #[ignore = "needs the SMB multicart ROM (RF_SCREENS_ROMS_DIR); run in release"]
    fn smb_saves_load_across_all_tiers() {
        let Some(rom) = load("nes", "Super Mario Bros. + Duck Hunt (USA).zip") else {
            println!("SKIP: SMB not found");
            return;
        };
        matrix(&rig(Console::Nes, "SMB", rom, smb_pad(900), 700));
    }

    /// Menus to Yoshi's Island 1, then play (the W38-01 token scripts).
    fn smw_pad() -> Vec<u16> {
        const TO_LEVEL: &str = "w200 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 w300 A8 w408 A8 w200 R40 w20 L40 w20 R40 w20 L40 w20 R40 w20 L40 w20 R60 w20 R200 w300 L8 w158 A8 w158";
        const PLAY: &str = "L2 w3 B8 w52 B8 w52 B8 w52 B8 w52 R+B14 R22 R+B14 R22 S6 w100 S6 w60 R+B14 R22 R+B14 R22";
        let mut pad = tokens(TO_LEVEL);
        pad.extend(tokens(PLAY));
        let more = tokens("R+B14 R22");
        for _ in 0..40 {
            pad.extend(&more);
        }
        pad
    }

    fn tokens(text: &str) -> Vec<u16> {
        let bit = |c: &str| -> u16 {
            1 << match c {
                "B" => 15,
                "Y" => 14,
                "E" => 13,
                "S" => 12,
                "U" => 11,
                "D" => 10,
                "L" => 9,
                "R" => 8,
                "A" => 7,
                "X" => 6,
                other => panic!("button {other}"),
            }
        };
        let mut out = Vec::new();
        for tok in text.split_whitespace() {
            let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
                (0u16, n.parse::<usize>().expect("wN"))
            } else {
                let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
                let (b, n) = tok.split_at(split);
                (
                    b.split('+').fold(0u16, |m, c| m | bit(c)),
                    n.parse().expect("N"),
                )
            };
            out.extend(std::iter::repeat_n(mask, n));
        }
        out
    }

    #[test]
    #[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR); run in release"]
    fn smw_saves_load_across_all_tiers() {
        let Some(rom) = load("snes", "Super Mario World (USA).zip") else {
            println!("SKIP: SMW not found");
            return;
        };
        let pad = smw_pad();
        let n = tokens("w200 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 w300 A8 w408 A8 w200 R40 w20 L40 w20 R40 w20 L40 w20 R40 w20 L40 w20 R60 w20 R200 w300 L8 w158 A8 w158").len() + 300;
        matrix(&rig(Console::Snes, "SMW", rom, pad, n));
    }
}
