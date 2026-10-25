//! Ticket W36-05: function hooks on the NES core, over small synthetic
//! cartridges (no ROM needed). Enter/Exit come from the shadow call stack,
//! an interrupt in the middle of a function returns to the interrupted
//! `pc`, an RTS trick does not pop, an unhooked function and an
//! unconfigured core cost no sink calls, and a read-only hook leaves the
//! machine bit-identical.

use rf_core_api::{
    CoreEvent, CoreSink, CpuRegs, EmulatorCore, FunctionEvent, HookCtx, InputFrame, PpuPixel,
    ResetKind,
};
use rf_nes::core::NesCore;
use rf_nes::rebuild::{build_atlas, Atlas};

fn ines(segments: &[(usize, &[u8])], nmi: u16) -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 1;
    d[5] = 1;
    let mut prg = vec![0xEAu8; 0x4000];
    for (off, bytes) in segments {
        prg[*off..*off + bytes.len()].copy_from_slice(bytes);
    }
    prg[0x3FFA..0x3FFC].copy_from_slice(&nmi.to_le_bytes());
    prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    d.extend(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

/// `JSR f1; JMP loop` with `f1 = INC $10; JSR f2; RTS`, `f2 = INC $11; RTS`.
fn nested_rom() -> Vec<u8> {
    ines(
        &[
            (0x00, &[0x20, 0x10, 0x80, 0x4C, 0x00, 0x80]),
            (0x10, &[0xE6, 0x10, 0x20, 0x20, 0x80, 0x60]),
            (0x20, &[0xE6, 0x11, 0x60]),
        ],
        0x8000,
    )
}

/// `JSR f4` where f4 pushes $804F and RTSes (to $8050), whose RTS then
/// returns to the caller.
fn trick_rom() -> Vec<u8> {
    ines(
        &[
            (0x00, &[0x20, 0x40, 0x80, 0x4C, 0x00, 0x80]),
            (0x40, &[0xA9, 0x80, 0x48, 0xA9, 0x4F, 0x48, 0x60]),
            (0x50, &[0x60]),
        ],
        0x8000,
    )
}

/// NMI on; `JSR slow` forever; `slow` spins ~330k cycles (about 11 frames),
/// so the NMI handler (`INC $12; RTI` at $8060) interrupts it.
fn nmi_rom() -> Vec<u8> {
    ines(
        &[
            (
                0x00,
                &[
                    0xA9, 0x80, 0x8D, 0x00, 0x20, 0x20, 0x20, 0x80, 0x4C, 0x05, 0x80,
                ],
            ),
            (
                0x20,
                &[
                    0xA0, 0x00, 0xA2, 0x00, 0xE8, 0xD0, 0xFD, 0xC8, 0xD0, 0xF8, 0x60,
                ],
            ),
            (0x60, &[0xE6, 0x12, 0x40]),
        ],
        0x8060,
    )
}

struct Quiet;
impl CoreSink for Quiet {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// Records every function event with the `pc` the machine was at.
#[derive(Default)]
struct Recorder {
    events: Vec<(FunctionEvent, u16)>,
}
impl CoreSink for Recorder {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
    fn function_event(&mut self, ev: FunctionEvent, ctx: &mut dyn HookCtx) {
        let pc = match ctx.regs() {
            CpuRegs::Mos6502(r) => r.pc,
            _ => 0,
        };
        self.events.push((ev, pc));
    }
}

fn atlas_of(raw: &[u8], frames: u32) -> Atlas {
    let mut rec = NesCore::from_ines_bytes(raw).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    for _ in 0..frames {
        rec.run_frame(&InputFrame::empty(), &mut Quiet);
    }
    let log = rec.take_coverage().expect("armed");
    let rom = rf_nes::NesRom::from_ines_bytes(raw).expect("rom");
    build_atlas(rom.prg_rom(), &log)
}

fn fid(a: &Atlas, entry_rom: u32) -> u32 {
    a.functions
        .iter()
        .position(|f| f.entry_rom == entry_rom)
        .unwrap_or_else(|| panic!("no function at {entry_rom:#x}")) as u32
}

fn core_with(raw: &[u8], atlas: &Atlas, hooks: &[u32]) -> NesCore {
    let mut c = NesCore::from_ines_bytes(raw).expect("loads");
    c.reset(ResetKind::Hard);
    c.set_rebuild(Some(atlas.clone())).expect("atlas fits");
    c.set_function_hooks(hooks);
    c
}

fn run(core: &mut NesCore, sink: &mut dyn CoreSink, frames: u32) {
    for _ in 0..frames {
        core.run_frame(&InputFrame::empty(), sink);
    }
}

#[test]
fn nested_calls_report_enter_and_exit_in_order_with_the_right_pc() {
    let raw = nested_rom();
    let atlas = atlas_of(&raw, 2);
    let (f1, f2) = (fid(&atlas, 0x10), fid(&atlas, 0x20));
    let mut core = core_with(&raw, &atlas, &[f1, f2]);
    let mut rec = Recorder::default();
    run(&mut core, &mut rec, 3);
    let cycle = [
        (FunctionEvent::Enter { function: f1 }, 0x8010),
        (FunctionEvent::Enter { function: f2 }, 0x8020),
        (FunctionEvent::Exit { function: f2 }, 0x8015),
        (FunctionEvent::Exit { function: f1 }, 0x8003),
    ];
    assert!(rec.events.len() > 400, "{} events", rec.events.len());
    for (i, e) in rec.events.iter().take(400).enumerate() {
        assert_eq!(*e, cycle[i % 4], "event {i}");
    }
}

#[test]
fn an_unhooked_function_emits_nothing_and_no_hooks_means_no_sink_calls() {
    let raw = nested_rom();
    let atlas = atlas_of(&raw, 2);
    let (f1, f2) = (fid(&atlas, 0x10), fid(&atlas, 0x20));
    // Only the inner function hooked: no events for F1, and F2's frame is
    // still found when its RTS lands.
    let mut only_f2 = core_with(&raw, &atlas, &[f2]);
    let mut rec = Recorder::default();
    run(&mut only_f2, &mut rec, 3);
    assert!(rec.events.iter().all(|(e, _)| matches!(
        e,
        FunctionEvent::Enter { function } | FunctionEvent::Exit { function } if *function == f2
    )));
    assert!(rec.events.len() > 400);

    // An atlas but no hooks, and hooks but no atlas: nothing is called.
    let mut none = core_with(&raw, &atlas, &[]);
    let mut rec = Recorder::default();
    run(&mut none, &mut rec, 1);
    assert!(rec.events.is_empty());
    let mut no_atlas = NesCore::from_ines_bytes(&raw).expect("loads");
    no_atlas.reset(ResetKind::Hard);
    no_atlas.set_function_hooks(&[f1]);
    run(&mut no_atlas, &mut rec, 1);
    assert!(rec.events.is_empty(), "hooks need an installed atlas");
    // The ids wait for the atlas.
    no_atlas.set_rebuild(Some(atlas)).expect("fits");
    run(&mut no_atlas, &mut rec, 1);
    assert!(!rec.events.is_empty());
}

#[test]
fn an_rts_trick_does_not_pop_and_the_real_return_does() {
    let raw = trick_rom();
    let atlas = atlas_of(&raw, 2);
    let f4 = fid(&atlas, 0x40);
    let mut core = core_with(&raw, &atlas, &[f4]);
    let mut rec = Recorder::default();
    run(&mut core, &mut rec, 3);
    // The first RTS lands at $8050 (no frame expected it); the second lands
    // at the caller's $8003 and only then does F4 exit.
    let cycle = [
        (FunctionEvent::Enter { function: f4 }, 0x8040),
        (FunctionEvent::Exit { function: f4 }, 0x8003),
    ];
    assert!(rec.events.len() > 400);
    for (i, e) in rec.events.iter().take(400).enumerate() {
        assert_eq!(*e, cycle[i % 2], "event {i}");
    }
}

#[test]
fn an_nmi_in_the_middle_of_a_function_returns_to_the_interrupted_pc() {
    let raw = nmi_rom();
    let atlas = atlas_of(&raw, 40);
    let (slow, nmi) = (fid(&atlas, 0x20), fid(&atlas, 0x60));
    let mut core = core_with(&raw, &atlas, &[slow, nmi]);
    let mut rec = Recorder::default();
    run(&mut core, &mut rec, 40);
    let ev = &rec.events;
    let first_exit_slow = ev
        .iter()
        .position(|(e, _)| *e == FunctionEvent::Exit { function: slow })
        .expect("the slow function finishes");
    let before = &ev[..first_exit_slow];
    assert_eq!(before[0].0, FunctionEvent::Enter { function: slow });
    // Between its entry and its exit the NMI handler ran at least once, and
    // each Exit of the handler is at an address inside the slow loop (the
    // interrupted pc), not at the JSR's return address.
    let handler_exits: Vec<u16> = before
        .iter()
        .filter(|(e, _)| *e == FunctionEvent::Exit { function: nmi })
        .map(|&(_, pc)| pc)
        .collect();
    assert!(!handler_exits.is_empty(), "the NMI fired inside the loop");
    for pc in handler_exits {
        assert!((0x8020..0x802B).contains(&pc), "interrupted pc {pc:#x}");
    }
    assert_eq!(
        before
            .iter()
            .filter(|(e, _)| *e == FunctionEvent::Enter { function: nmi })
            .count(),
        before
            .iter()
            .filter(|(e, _)| *e == FunctionEvent::Exit { function: nmi })
            .count(),
        "every handler entry paired with its exit"
    );
    assert_eq!(ev[first_exit_slow].1, 0x8008, "back at the JSR's return");
}

#[test]
fn a_read_only_hook_leaves_the_machine_bit_identical() {
    let raw = nmi_rom();
    let atlas = atlas_of(&raw, 40);
    let (slow, nmi) = (fid(&atlas, 0x20), fid(&atlas, 0x60));
    let mut hooked = core_with(&raw, &atlas, &[slow, nmi]);
    let mut plain = core_with(&raw, &atlas, &[]);
    let mut rec = Recorder::default();
    run(&mut hooked, &mut rec, 40);
    run(&mut plain, &mut Quiet, 40);
    assert!(!rec.events.is_empty());
    assert_eq!(hooked.bus().ram(), plain.bus().ram());
    assert_eq!(hooked.bus().master_cycle(), plain.bus().master_cycle());
    assert_eq!(hooked.state_view().cpu_regs, plain.state_view().cpu_regs);
}

/// A hook may write work RAM, at no clock cost, and the core refuses what
/// is not RAM.
#[test]
fn a_poke_changes_work_ram_only_and_costs_no_cycles() {
    struct Poker {
        results: Vec<bool>,
        done: bool,
    }
    impl CoreSink for Poker {
        fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
        fn audio(&mut self, _s: &[i16]) {}
        fn event(&mut self, _e: CoreEvent) {}
        fn function_event(&mut self, _ev: FunctionEvent, ctx: &mut dyn HookCtx) {
            if !self.done {
                self.done = true;
                self.results = vec![
                    ctx.poke(0x0300, 0x5A),
                    ctx.poke(0x2000, 0xFF), // PPU register: refused
                    ctx.poke(0x8000, 0xFF), // PRG ROM: refused
                    ctx.poke(0x1_0000, 1),  // not a CPU address
                ];
            }
        }
    }
    let raw = nested_rom();
    let atlas = atlas_of(&raw, 2);
    let f1 = fid(&atlas, 0x10);
    let mut core = core_with(&raw, &atlas, &[f1]);
    let mut p = Poker {
        results: vec![],
        done: false,
    };
    run(&mut core, &mut p, 1);
    assert_eq!(p.results, [true, false, false, false]);
    assert_eq!(core.peek(0x0300), 0x5A);
    let mut clean = core_with(&raw, &atlas, &[]);
    run(&mut clean, &mut Quiet, 1);
    assert_eq!(
        core.bus().master_cycle(),
        clean.bus().master_cycle(),
        "a poke has no clock cost"
    );
}

/// The 6502 shape-hash mask (ticket W36-05): a function containing `JSR`
/// and `LDA abs` into itself still matches after being moved and
/// re-pointed; a changed opcode, or a changed zero-page operand, does not.
#[test]
fn a_real_6502_function_moved_and_repointed_keeps_its_shape_hash() {
    use rf_rebuild::{match_recipe, HookPoints, Recipe, RecipeFunction};
    // f: LDA f ; STA $10 ; JSR f ; RTS   (absolute operands name f itself)
    fn build(at: u16, lda: u8, zp: u8) -> Vec<u8> {
        let [lo, hi] = at.to_le_bytes();
        let off = usize::from(at - 0x8000);
        ines(
            &[
                (0x00, &[0x20, lo, hi, 0x4C, 0x00, 0x80]),
                (off, &[lda, lo, hi, 0x85, zp, 0x20, lo, hi, 0x60]),
            ],
            0x8000,
        )
    }
    // The function calls itself, so run it for a handful of instructions
    // only: coverage needs the instructions executed, not a clean return.
    fn atlas_for(raw: &[u8]) -> Atlas {
        let mut rec = NesCore::from_ines_bytes(raw).expect("loads");
        rec.set_coverage_recording(true);
        rec.reset(ResetKind::Hard);
        let mut sink = Quiet;
        for _ in 0..2 {
            rec.run_frame(&InputFrame::empty(), &mut sink);
        }
        let log = rec.take_coverage().expect("armed");
        build_atlas(
            rf_nes::NesRom::from_ines_bytes(raw).unwrap().prg_rom(),
            &log,
        )
    }
    let at = |a: &Atlas, rom: u32| a.functions.iter().find(|f| f.entry_rom == rom).cloned();
    let original = at(&atlas_for(&build(0x8100, 0xAD, 0x10)), 0x100).expect("function at $8100");
    let row = |f: &rf_rebuild::Function| RecipeFunction {
        name: "f".to_string(),
        shape_hash: f.shape_hash,
        exact_hash: Some(f.byte_hash),
        address_hint: Some(0x8100),
        hooks: HookPoints {
            entry: true,
            exit: false,
        },
        source: "test".to_string(),
    };
    let recipe = Recipe {
        id: "t".to_string(),
        engine: "t".to_string(),
        functions: vec![row(&original)],
        ram: vec![],
    };

    let moved_atlas = atlas_for(&build(0x8300, 0xAD, 0x10));
    let moved = at(&moved_atlas, 0x300).expect("function at $8300");
    assert_ne!(moved.byte_hash, original.byte_hash);
    assert_eq!(moved.shape_hash, original.shape_hash);
    assert_eq!(match_recipe(&moved_atlas, &recipe).matched.len(), 1);

    // LDA abs (AD) -> LDX abs (AE): a different opcode, same length.
    let other_op = atlas_for(&build(0x8100, 0xAE, 0x10));
    assert!(match_recipe(&other_op, &recipe).matched.is_empty());
    // A zero-page operand is the game's own constant and is kept.
    let other_zp = atlas_for(&build(0x8100, 0xAD, 0x11));
    assert!(match_recipe(&other_zp, &recipe).matched.is_empty());
}
