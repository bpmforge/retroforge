//! **W38-06: lockstep on Super Metroid** (ROM-gated, law 5).
//!
//! The scripted play of `w38_06_common` (menus, the first rooms, the countdown
//! escape, the planet scene, the first outdoor rooms) runs on the plain core
//! (reference) and on the rebuilt core after every frame, with zero divergence
//! required: first on the unmodified image, then with every hooked function of
//! the recipe live (the tick hook reads the player's position words at the NMI
//! handler's entry and exit), then, when the overlay has keepers, on the image
//! with them applied.
//!
//! Only counts and hashes are printed; no ROM bytes leave the test (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_06_sm_lockstep -- --ignored --nocapture
//! ```

mod w38_06_common;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use rf_core_api::{
    CoreSink, EmulatorCore, FunctionEvent, HookCtx, InputFrame, RebuildStats, ResetKind,
};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::lockstep::{Options, Side};
use rf_snes::core::SnesCore;
use w38_06_common::*;

fn record(raw: &[u8]) -> rf_core_api::coverage::CoverageLog {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for mask in script() {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&InputFrame::empty(), &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

fn recipe_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/snes/super-metroid")
}

fn lockstep(raw: &[u8], tag: &str) -> (u32, Option<u32>, RebuildStats) {
    let atlas = build_snes_atlas(raw, &record(raw)).expect("atlas");
    println!("{tag}: atlas {} functions", atlas.functions.len());
    let dir = std::env::temp_dir().join(format!("rf-w38-06-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let opts = Options::<SnesCore> {
        bundle_dir: Some(dir),
        ..Options::default()
    };
    let r = rf_harness::lockstep_snes::run(raw, &atlas, inputs_of(&script()), opts)
        .expect("lockstep runs");
    (
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats,
    )
}

#[test]
#[ignore = "needs Super Metroid (RF_SCREENS_ROMS_DIR); minutes in release"]
fn super_metroid_plain_image_has_zero_divergence() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let (frames, div, stats) = lockstep(&raw, "plain");
    println!(
        "plain image: {frames} frames, divergence {div:?}, {:.3}% fallback ({} instrs)",
        stats.fallback_rate() * 100.0,
        stats.instructions()
    );
    assert_eq!(div, None);
    assert_eq!(frames as usize, script().len());
}

#[derive(Default)]
struct Seen {
    events: BTreeMap<(u32, bool), u32>,
    /// (frame, position at entry or exit): the tick function's hook points.
    tick_enter: Vec<(u32, (u16, u16))>,
    tick_exit: Vec<(u32, (u16, u16))>,
}

struct Sink {
    seen: Rc<RefCell<Seen>>,
    tick: u32,
    pos: (u32, u32),
    frame: Rc<Cell<u32>>,
}

fn word(ctx: &dyn HookCtx, a: u32) -> u16 {
    u16::from(ctx.peek(a)) | u16::from(ctx.peek(a + 1)) << 8
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, _y: u16, _p: &[rf_core_api::PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: rf_core_api::CoreEvent) {}
    fn function_event(&mut self, ev: FunctionEvent, ctx: &mut dyn HookCtx) {
        let mut s = self.seen.borrow_mut();
        let (f, enter) = match ev {
            FunctionEvent::Enter { function } => (function, true),
            FunctionEvent::Exit { function } => (function, false),
        };
        *s.events.entry((f, enter)).or_default() += 1;
        if f == self.tick {
            let at = (word(ctx, self.pos.0), word(ctx, self.pos.1));
            let fr = self.frame.get();
            if enter {
                s.tick_enter.push((fr, at));
            } else {
                s.tick_exit.push((fr, at));
            }
        }
    }
}

#[test]
#[ignore = "needs Super Metroid (RF_SCREENS_ROMS_DIR); minutes in release"]
fn super_metroid_recipe_hooks_fire_each_frame_and_keep_lockstep() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let recipe = rf_profiles::load_recipe_file(&recipe_dir().join("recipe.toml")).expect("recipe");
    let atlas = build_snes_atlas(&raw, &record(&raw)).expect("atlas");
    let report = rf_rebuild::match_recipe(&atlas, &recipe);
    assert!(
        report.unmatched.is_empty(),
        "unmatched rows: {:?}",
        report.unmatched
    );
    let ram = |name: &str| {
        recipe
            .ram
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("ram row {name}"))
            .addr
    };
    let pos = (ram("player_x"), ram("player_y"));
    let tick = report
        .matched
        .iter()
        .find(|m| m.name == "nmi")
        .expect("matched")
        .function;
    let mut ids: Vec<u32> = recipe
        .functions
        .iter()
        .filter(|f| f.hooks.entry || f.hooks.exit)
        .filter_map(|f| {
            report
                .matched
                .iter()
                .find(|m| m.name == f.name)
                .map(|m| m.function)
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    println!(
        "{} hooked functions, tick = atlas function {tick}",
        ids.len()
    );

    let seen = Rc::new(RefCell::new(Seen::default()));
    let factory_seen = Rc::clone(&seen);
    let end_of_frame: Rc<RefCell<Vec<(u16, u16)>>> = Rc::default();
    let frame_now: Rc<Cell<u32>> = Rc::default();
    let hook_frame = Rc::clone(&frame_now);
    let eof = Rc::clone(&end_of_frame);
    let word_at =
        |core: &SnesCore, a: u32| u16::from(core.peek(a)) | u16::from(core.peek(a + 1)) << 8;
    let mut hook = |frame: u32, side: Side, core: &mut SnesCore| {
        if side != Side::Rebuilt {
            return;
        }
        hook_frame.set(frame);
        if frame == 0 {
            eof.borrow_mut().clear();
        }
        // The state before frame f is the end of frame f - 1.
        eof.borrow_mut()
            .push((word_at(core, pos.0), word_at(core, pos.1)));
    };
    let opts = Options::<SnesCore> {
        function_hooks: ids,
        hook_sink: Some(Box::new(move || {
            *factory_seen.borrow_mut() = Seen::default();
            Box::new(Sink {
                seen: Rc::clone(&factory_seen),
                tick,
                pos,
                frame: Rc::clone(&frame_now),
            })
        })),
        before_frame: Some(&mut hook),
        ..Options::default()
    };
    let frames = script().len();
    let r = rf_harness::lockstep_snes::run(&raw, &atlas, inputs_of(&script()), opts)
        .expect("lockstep runs");
    println!(
        "lockstep with the recipe's hooks: {} frames, divergence {:?}, {:.3}% fallback",
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats.fallback_rate() * 100.0
    );
    assert_eq!(r.divergence.as_ref().map(|d| d.frame), None);
    assert_eq!(r.frames_run as usize, frames);
    let seen = seen.borrow();
    let eof = end_of_frame.borrow();
    println!(
        "tick entries {}, exits {} over {frames} frames",
        seen.tick_enter.len(),
        seen.tick_exit.len()
    );
    // The share of in-play ticks whose position equals the end of that frame.
    let from = play_from() as u32;
    let share = |v: &[(u32, (u16, u16))]| {
        let play: Vec<_> = v.iter().filter(|e| e.0 >= from).collect();
        let same = play
            .iter()
            .filter(|(f, at)| eof.get(*f as usize + 1) == Some(at))
            .count();
        (same as f64 / play.len().max(1) as f64, play.len())
    };
    let (enter_same, n_enter) = share(&seen.tick_enter);
    let (exit_same, n_exit) = share(&seen.tick_exit);
    println!(
        "position at the tick's entry equals the end of that frame in {enter_same:.4} of {n_enter} frames; at its exit in {exit_same:.4} of {n_exit}"
    );
    assert!(
        seen.tick_enter.len() * 100 >= frames * 95 && seen.tick_enter.len() <= frames + 4,
        "the tick hook fires about once per frame: {}",
        seen.tick_enter.len()
    );
    for ((f, enter), n) in &seen.events {
        println!(
            "function {f} {} x{n}",
            if *enter { "enter" } else { "exit" }
        );
    }
}
