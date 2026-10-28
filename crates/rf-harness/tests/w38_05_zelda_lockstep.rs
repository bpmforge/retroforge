//! **W38-05: lockstep on The Legend of Zelda** (ROM-gated, law 5).
//!
//! The scripted play of `w38_05_zelda_common` (file registration, the first
//! cave, then the overworld in three directions) runs on the plain core
//! (reference) and on the rebuilt core after every frame, with zero
//! divergence required: first on the unmodified image, then on the image with
//! the recipe's widescreen keepers applied (when the overlay has any), and with
//! every hooked function of the recipe live (the tick hook reads Link's
//! position at the end of the NMI handler).
//!
//! Only counts and hashes are printed; no ROM bytes leave the test (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_05_zelda_lockstep -- --ignored --nocapture
//! RF_W36_TIER=ir cargo test --release -p rf-harness --test w38_05_zelda_lockstep -- --ignored --nocapture
//! ```

mod w38_05_zelda_common;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use rf_core_api::{CoreSink, EmulatorCore, FunctionEvent, HookCtx, RebuildStats, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options, Side};
use rf_nes::core::NesCore;
use w38_05_zelda_common::{inputs, load_rom, rom_path, NullSink, FILE, FRAMES, PLAY_FROM};

fn record(raw: &[u8]) -> rf_core_api::coverage::CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for input in inputs(FRAMES) {
        core.run_frame(&input, &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

fn rebuilt_tier() -> rf_nes::ExecTier {
    if std::env::var("RF_W36_TIER").is_ok_and(|v| v == "ir") {
        rf_nes::ExecTier::Ir
    } else {
        rf_nes::ExecTier::Interpreter
    }
}

fn nametable_print(core: &NesCore) -> u64 {
    core.state_view()
        .vram
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// Runs the lockstep of one image; returns (frames, divergence, stats, distinct
/// nametable prints sampled every 16 frames).
fn lockstep(raw: &[u8], tag: &str) -> (u32, Option<u32>, RebuildStats, usize) {
    let atlas = build_nes_atlas(raw, &record(raw)).expect("atlas");
    println!("{tag}: atlas {} functions", atlas.functions.len());
    let tier = rebuilt_tier();
    let mut prints: HashSet<u64> = HashSet::new();
    let dir = std::env::temp_dir().join(format!("rf-w38-05-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut hook = |frame: u32, side: Side, core: &mut NesCore| {
        if side == Side::Rebuilt {
            core.set_exec_tier(tier);
            assert_eq!(core.exec_tier(), tier, "the rebuilt core runs the tier");
            if frame.is_multiple_of(16) {
                prints.insert(nametable_print(core));
            }
        }
    };
    let opts = Options {
        bundle_dir: Some(dir),
        before_frame: Some(&mut hook),
        ..Options::default()
    };
    let r = run(raw, &atlas, inputs(FRAMES), opts).expect("lockstep runs");
    (
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats,
        prints.len(),
    )
}

#[test]
#[ignore = "needs The Legend of Zelda (USA) (RF_SCREENS_ROMS_DIR); about a minute in release"]
fn zelda_plain_image_has_zero_divergence() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let (frames, div, stats, prints) = lockstep(&raw, "plain");
    println!(
        "plain image: {frames} frames, divergence {div:?}, {:.3}% fallback ({} instrs), {prints} distinct nametable states",
        stats.fallback_rate() * 100.0,
        stats.instructions()
    );
    assert_eq!(div, None);
    assert_eq!(frames, FRAMES);
    assert!(prints >= 8, "the script crossed screens ({prints})");
}

/// What the hook sink saw: events per (function id, enter), and Link's
/// position at each exit of the tick function.
#[derive(Default)]
struct Seen {
    events: BTreeMap<(u32, bool), u32>,
    tick_exit: Vec<(u32, (u8, u8))>,
}

struct Sink {
    seen: Rc<RefCell<Seen>>,
    tick: u32,
    pos: (u32, u32),
    frame: Rc<std::cell::Cell<u32>>,
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
        if f == self.tick && !enter {
            let at = (ctx.peek(self.pos.0), ctx.peek(self.pos.1));
            s.tick_exit.push((self.frame.get(), at));
        }
    }
}

fn recipe_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/nes/legend-of-zelda")
}

#[test]
#[ignore = "needs The Legend of Zelda (USA) (RF_SCREENS_ROMS_DIR); about a minute in release"]
fn zelda_recipe_hooks_fire_each_frame_and_keep_lockstep() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let recipe = rf_profiles::load_recipe_file(&recipe_dir().join("recipe.toml")).expect("recipe");
    let atlas = build_nes_atlas(&raw, &record(&raw)).expect("atlas");
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
    let id_of = |name: &str| {
        let row = recipe
            .functions
            .iter()
            .position(|f| f.name == name)
            .expect("row");
        report
            .matched
            .iter()
            .find(|m| m.name == recipe.functions[row].name)
            .expect("matched")
            .function
    };
    let tick = id_of("nmi");
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
    let end_of_frame: Rc<RefCell<Vec<(u8, u8)>>> = Rc::default();
    let frame_now: Rc<std::cell::Cell<u32>> = Rc::default();
    let hook_frame = Rc::clone(&frame_now);
    let eof = Rc::clone(&end_of_frame);
    let mut hook = |frame: u32, side: Side, core: &mut NesCore| {
        if side == Side::Rebuilt {
            hook_frame.set(frame);
            if frame == 0 {
                eof.borrow_mut().clear();
            }
            // The state before frame f is the end of frame f - 1.
            eof.borrow_mut().push((core.peek(pos.0), core.peek(pos.1)));
        }
    };
    let opts = Options {
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
    let r = run(&raw, &atlas, inputs(FRAMES), opts).expect("lockstep runs");
    println!(
        "lockstep with the recipe's hooks: {} frames, divergence {:?}, {:.3}% fallback",
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats.fallback_rate() * 100.0
    );
    assert_eq!(r.divergence.as_ref().map(|d| d.frame), None);
    assert_eq!(r.frames_run, FRAMES);
    let seen = seen.borrow();
    let eof = end_of_frame.borrow();
    let exits = &seen.tick_exit;
    println!("tick exits {} over {} frames", exits.len(), FRAMES);
    // The tick hook's position is final for the frame: in play, it equals the
    // state at the end of the frame it fired in (`eof[f + 1]`).
    let in_play: Vec<&(u32, (u8, u8))> = exits.iter().filter(|e| e.0 >= PLAY_FROM).collect();
    let same = in_play
        .iter()
        .filter(|(f, at)| eof.get(*f as usize + 1) == Some(at))
        .count();
    let same_frame = same as f64 / in_play.len().max(1) as f64;
    println!(
        "tick-exit position equals the end of that frame in {same_frame:.4} of {} frames",
        in_play.len()
    );
    assert!(
        exits.len() * 100 >= FRAMES as usize * 95 && exits.len() <= FRAMES as usize + 4,
        "the tick hook fires about once per frame: {}",
        exits.len()
    );
    assert!(
        same_frame >= 0.99,
        "positions at the tick hook are final: {same_frame}"
    );
    for ((f, enter), n) in &seen.events {
        println!(
            "function {f} {} x{n}",
            if *enter { "enter" } else { "exit" }
        );
    }
}
