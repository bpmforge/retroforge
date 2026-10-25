//! **The SNES block executor changes nothing it does not announce** (ticket
//! W36-08b; `docs/design/REBUILD_LAB.md`).
//!
//! A synthetic LoROM exercises, in one loop: `REP`/`SEP` width changes,
//! `JSL`/`RTL` across banks, a routine copied to work RAM and called there,
//! `JSR (abs,X)` through a table, NMI and a V-IRQ, and a width taken from a
//! `PLP` (which the atlas cannot know). Record a run, build the atlas, then
//! run the cartridge with the executor off and on and compare everything
//! observable every frame: registers, every savestate region (CPU, PPU, APU
//! with its RAM and DSP, work RAM, VRAM, OAM, CGRAM, SRAM), and hashes of
//! the video and audio. The executor only peeks, so bit identity is the
//! contract; the counters are the other half.

use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore, FunctionEvent, HookCtx, InputFrame, PpuPixel, RebuildStats,
    ResetKind, StateError, StateWriter,
};
use rf_rebuild::{Atlas, Provenance};
use rf_snes::core::SnesCore;
use rf_snes::state::StateRegion;

const FRAMES: usize = 10;
/// WRAM bytes the program reads (`flag`) and the test pokes before frame 0.
const WIDTH_FLAG: usize = 0x11;
const TABLE_INDEX: usize = 0x12;

const SUB_LONG: u32 = 0x01_8000;
const NMI: usize = 0x8200;
const IRQ: usize = 0x8210;

fn put(rom: &mut [u8], addr: usize, code: &[u8]) {
    rom[addr - 0x8000..addr - 0x8000 + code.len()].copy_from_slice(code);
}

/// 64 KiB LoROM (two banks).
fn rom() -> Vec<u8> {
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
        0x78, // SEI
        0x18, // CLC
        0xFB, // XCE            -> native mode
        0xC2, 0x38, // REP #$38
        0xA2, 0xFF, 0x1F, // LDX #$1FFF
        0x9A, // TXS
        0xE2, 0x20, // SEP #$20
        // A routine in work RAM at $0100: INC $0030 ; RTL
        0xA9, 0xEE, 0x8D, 0x00, 0x01, // LDA #$EE ; STA $0100
        0xA9, 0x30, 0x8D, 0x01, 0x01, // LDA #$30 ; STA $0101
        0xA9, 0x00, 0x8D, 0x02, 0x01, // LDA #$00 ; STA $0102
        0xA9, 0x6B, 0x8D, 0x03, 0x01, // LDA #$6B ; STA $0103
        0xA9, 100, 0x8D, 0x09, 0x42, // LDA #100 ; STA $4209  (V-IRQ line)
        0x9C, 0x0A, 0x42, // STZ $420A
        0xA9, 0xA0, 0x8D, 0x00, 0x42, // LDA #$A0 ; STA $4200  (NMI + V-IRQ)
        0x58, // CLI
    ];
    let main = 0x8000 + c.len();
    c.extend_from_slice(&[
        0xC2,
        0x30, // REP #$30
        0x22,
        0x00,
        0x80,
        0x01, // JSL $018000   (16-bit A, 16-bit X)
        0xE2,
        0x30, // SEP #$30
        0x22,
        0x00,
        0x80,
        0x01, // JSL $018000   (8-bit both)
        0x22,
        0x00,
        0x01,
        0x7E, // JSL $7E0100   (work-RAM code)
        0xC2,
        0x30, // REP #$30
        0xAD,
        TABLE_INDEX as u8,
        0x00, // LDA $0012
        0x29,
        0x01,
        0x00, // AND #$0001
        0x0A, // ASL
        0xAA, // TAX
        0xFC,
        0x00,
        0x8F, // JSR ($8F00,X)
        0xE2,
        0x20, // SEP #$20
        0xAD,
        WIDTH_FLAG as u8,
        0x00, // LDA $0011
        0x48, // PHA
        0x28, // PLP            <- the width is whatever the flag says
        // Shared code: valid at either width, decoded differently.
        0xA9,
        0x12,
        0xEA, // LDA #$12 ; NOP   | LDA #$EA12
        0x8D,
        0x50,
        0x00, // STA $0050
        0xA2,
        0x34,
        0xEA, // LDX #$34 ; NOP   | LDX #$EA34
        0x8E,
        0x52,
        0x00, // STX $0052
        0xC2,
        0x30, // REP #$30
        0xCB, // WAI
        0x4C,
        main as u8,
        (main >> 8) as u8, // JMP main
    ]);
    put(&mut rom, 0x8000, &c);
    put(&mut rom, 0x8E00, &[0xEE, 0x40, 0x00, 0x60]); // INC $0040 ; RTS
    put(&mut rom, 0x8E10, &[0xEE, 0x42, 0x00, 0x60]); // INC $0042 ; RTS
    put(&mut rom, 0x8F00, &[0x00, 0x8E, 0x10, 0x8E]);
    put(&mut rom, NMI, &[0xEE, 0x20, 0x00, 0x40]); // INC $0020 ; RTI
    put(
        &mut rom,
        IRQ,
        &[
            0x08, 0x48, // PHP ; PHA
            0xAD, 0x11, 0x42, // LDA $4211   (acknowledge)
            0xEE, 0x21, 0x00, // INC $0021
            0x68, 0x28, 0x40, // PLA ; PLP ; RTI
        ],
    );
    // Bank 1: A = 1 at the width in force (`A9 01 EA` is LDA #$EA01 at 16 bits
    // and LDA #1 ; NOP at 8), STA $0031, RTL.
    rom[0x8000..0x8007].copy_from_slice(&[0xA9, 0x01, 0xEA, 0x8D, 0x31, 0x00, 0x6B]);
    rom
}

fn core() -> SnesCore {
    SnesCore::load(&rom()).expect("loads")
}

/// A core primed the way a scenario wants: the two bytes the program reads.
fn primed(width_flag: u8, table_index: u8) -> SnesCore {
    let mut c = core();
    c.reset(ResetKind::Hard);
    let wram = &mut c.system_mut().bus.wram;
    wram[WIDTH_FLAG] = width_flag;
    wram[TABLE_INDEX] = table_index;
    c
}

#[derive(Default)]
struct Hashes {
    video: u64,
    audio: u64,
    events: Vec<FunctionEvent>,
    enter_regs: Vec<(u8, u16)>,
}

fn fnv(h: &mut u64, b: u8) {
    *h = (*h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
}

impl Hashes {
    fn new() -> Self {
        Self {
            video: 0xcbf2_9ce4_8422_2325,
            audio: 0xcbf2_9ce4_8422_2325,
            ..Self::default()
        }
    }
}

impl CoreSink for Hashes {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        fnv(&mut self.video, y as u8);
        pixels
            .iter()
            .for_each(|p| fnv(&mut self.video, p.palette_index));
    }
    fn audio(&mut self, samples: &[i16]) {
        samples.iter().for_each(|s| fnv(&mut self.audio, *s as u8));
    }
    fn event(&mut self, _e: CoreEvent) {}
    fn function_event(&mut self, ev: FunctionEvent, ctx: &mut dyn HookCtx) {
        self.events.push(ev);
        if let rf_core_api::CpuRegs::Wdc65816(r) = ctx.regs() {
            self.enter_regs.push((r.pbr, r.pc));
        }
    }
}

#[derive(Default)]
struct Buf(Vec<u8>);

impl StateWriter for Buf {
    fn write_all(&mut self, b: &[u8]) -> Result<(), StateError> {
        self.0.extend_from_slice(b);
        Ok(())
    }
}

/// Every savestate region plus the registers: the whole machine.
fn machine(core: &SnesCore) -> Vec<Vec<u8>> {
    let mut out = vec![format!("{:?}", core.system().cpu.pc24()).into_bytes()];
    for region in StateRegion::ALL {
        let mut b = Buf::default();
        core.system().save_region(region, &mut b).expect("saves");
        out.push(b.0);
    }
    out
}

fn record(width_flag: u8, table_index: u8) -> Atlas {
    let mut c = primed(width_flag, table_index);
    c.set_coverage_recording(true);
    let mut sink = Hashes::new();
    for _ in 0..FRAMES {
        c.run_frame(&InputFrame::empty(), &mut sink);
    }
    let log = c.take_coverage().expect("armed");
    assert!(log.instructions_recorded > 100);
    rf_snes::rebuild::build_atlas_from_image(&rom(), &log).expect("atlas")
}

/// Run `FRAMES` frames with the executor off and on from the same state,
/// asserting the machine and the output agree after every frame. Returns the
/// executor's counters and the rebuilt core (for its deltas and trace).
fn lockstep(atlas: Atlas, width_flag: u8, table_index: u8) -> (RebuildStats, SnesCore) {
    let mut plain = primed(width_flag, table_index);
    let mut rebuilt = primed(width_flag, table_index);
    rebuilt.set_rebuild(Some(atlas)).expect("installs");
    rebuilt.set_rebuild_trace(64);
    for frame in 0..FRAMES {
        let (mut hp, mut hr) = (Hashes::new(), Hashes::new());
        plain.run_frame(&InputFrame::empty(), &mut hp);
        rebuilt.run_frame(&InputFrame::empty(), &mut hr);
        assert_eq!(machine(&plain), machine(&rebuilt), "frame {frame}");
        assert_eq!((hp.video, hp.audio), (hr.video, hr.audio), "frame {frame}");
    }
    assert!(plain.rebuild_stats().is_none(), "off means no counters");
    let stats = rebuilt.rebuild_stats().expect("on");
    (stats, rebuilt)
}

#[test]
fn the_recorded_run_replays_identically_with_every_instruction_in_the_atlas() {
    let atlas = record(0x30, 0);
    assert_eq!(atlas.mode_count, 5);
    let (s, _) = lockstep(atlas, 0x30, 0);
    println!(
        "recorded scenario: {s:?} fallback rate {:.4}",
        s.fallback_rate()
    );
    assert!(s.observed > 100, "{s:?}");
    assert_eq!(s.inferred, 0, "a replay runs only what was recorded: {s:?}");
    assert!(
        s.interrupt_entries >= FRAMES as u64,
        "NMI every frame: {s:?}"
    );
    assert!(
        s.fallback_ram > 0,
        "the work-RAM routine is interpreter-only: {s:?}"
    );
    assert_eq!(
        (s.fallback_width, s.fallback_no_node, s.fallback_other),
        (0, 0, 0),
        "{s:?}"
    );
    assert_eq!(s.fallback, s.fallback_ram);
    assert!(s.indirect_checked > 0);
    assert_eq!(s.dispatch_misses, 0, "every landing was recorded: {s:?}");
}

#[test]
fn code_run_at_a_width_the_atlas_never_saw_falls_back_counted_and_stays_identical() {
    // Recorded with 8-bit A and X after the PLP (flag $30) and table slot 0;
    // run with 16-bit (flag $00) and table slot 1.
    let atlas = record(0x30, 0);
    let (s, rebuilt) = lockstep(atlas, 0x00, 1);
    println!(
        "mismatch scenario: {s:?} fallback rate {:.4}",
        s.fallback_rate()
    );
    assert!(s.fallback_width > 0, "{s:?}");
    assert!(
        s.fallback_no_node > 0,
        "table slot 1 is code nobody recorded: {s:?}"
    );
    assert!(
        s.dispatch_misses > 0,
        "JSR (abs,X) landed on an unseen target"
    );
    assert_eq!(
        s.fallback,
        s.fallback_width + s.fallback_no_node + s.fallback_ram + s.fallback_other
    );
    let miss = rebuilt.rebuild_deltas().first().expect("logged");
    assert_eq!(miss.target.cpu, 0x8E10, "{miss:?}");
    // The trace ring names the width mismatches.
    assert!(rebuilt
        .rebuild_trace()
        .iter()
        .any(|e| e.kind == rf_rebuild::TraceKind::WidthMismatch || e.mode != 4));
}

#[test]
fn an_atlas_for_another_cartridge_is_refused() {
    let atlas = record(0x30, 0);
    let mut small = rom();
    small.truncate(0x8000);
    small[0x7FD7] = 5;
    let mut other = SnesCore::load(&small).expect("loads");
    assert!(other.set_rebuild(Some(atlas)).is_err());
    assert!(other.rebuild_stats().is_none());
}

#[test]
fn hooks_see_jsl_rtl_and_interrupt_frames() {
    let atlas = record(0x30, 0);
    let by_entry = |rom: u32, mode: u8| {
        atlas
            .functions
            .iter()
            .position(|f| f.entry_rom == rom && f.entry_mode == mode)
            .unwrap_or_else(|| panic!("no function at {rom:#x} in mode {mode}")) as u32
    };
    // The long-called routine in both widths it was called in (mode 1 =
    // 16-bit A and X, mode 4 = 8-bit both), and the NMI handler (entered at
    // the width of whatever it interrupted: find any).
    let sub16 = by_entry(0x8000, 1);
    let sub8 = by_entry(0x8000, 4);
    let nmi = atlas
        .functions
        .iter()
        .position(|f| f.entry_rom == (NMI - 0x8000) as u32)
        .expect("nmi handler") as u32;
    let mut c = primed(0x30, 0);
    c.set_rebuild(Some(atlas)).expect("installs");
    c.set_function_hooks(&[sub16, sub8, nmi]);
    let mut sink = Hashes::new();
    for _ in 0..FRAMES {
        c.run_frame(&InputFrame::empty(), &mut sink);
    }
    let enters = |f: u32| {
        sink.events
            .iter()
            .filter(|e| **e == FunctionEvent::Enter { function: f })
            .count()
    };
    let exits = |f: u32| {
        sink.events
            .iter()
            .filter(|e| **e == FunctionEvent::Exit { function: f })
            .count()
    };
    // One call of each width per pass through the loop.
    assert!(enters(sub16) >= FRAMES && enters(sub8) >= FRAMES);
    assert!(exits(sub16) + 1 >= enters(sub16) && exits(sub8) + 1 >= enters(sub8));
    assert!(enters(nmi) >= FRAMES - 1 && exits(nmi) + 1 >= enters(nmi));
    // Enter fires before the first instruction: PBR:PC is the entry.
    let first = sink
        .events
        .iter()
        .position(|e| *e == FunctionEvent::Enter { function: sub16 })
        .expect("entered");
    assert_eq!(sink.enter_regs[first], ((SUB_LONG >> 16) as u8, 0x8000));
    // Nothing is left half-open beyond the frame the run stopped in.
    assert!(c.rebuild_shadow_depth() <= 2);
}

#[test]
fn a_state_load_must_drop_the_shadow_stack() {
    let atlas = record(0x30, 0);
    let sub8 = atlas
        .functions
        .iter()
        .position(|f| f.entry_rom == 0x8000 && f.entry_mode == 4)
        .expect("sub") as u32;
    let mut c = primed(0x30, 0);
    c.set_rebuild(Some(atlas)).expect("installs");
    c.set_function_hooks(&[sub8]);
    let mut sink = Hashes::new();
    // Stop mid-call: step instructions until a frame in flight holds a frame.
    for _ in 0..20_000 {
        let _ = c.step(rf_core_api::Step::Instruction, &mut sink);
        if c.rebuild_shadow_depth() > 0 {
            break;
        }
    }
    assert!(c.rebuild_shadow_depth() > 0);
    c.reset_hook_frames();
    assert_eq!(c.rebuild_shadow_depth(), 0);
}

#[test]
fn provenance_is_reported_per_instruction() {
    let atlas = record(0x30, 0);
    assert!(atlas.insns().any(|i| i.prov == Provenance::Observed));
}
