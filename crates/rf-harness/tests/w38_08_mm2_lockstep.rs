//! **W38-08: lockstep on Mega Man 2** (ROM-gated, law 5).
//!
//! The scripted forest-stage play of `w38_08_mm2_common` (menus, then a patrol
//! with the health byte held full by a test-side poke applied to BOTH sides)
//! runs on the plain core (reference) and on the rebuilt core after every
//! frame, with zero divergence required: first on the unmodified image, then
//! with every hooked function of the recipe live, then (when the overlay has
//! keepers) on the image with the keepers applied.
//!
//! Only counts and hashes are printed; no ROM bytes leave the test (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_08_mm2_lockstep -- --ignored --nocapture
//! ```

mod w38_08_mm2_common;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use rf_core_api::{CoreSink, EmulatorCore, FunctionEvent, HookCtx, RebuildStats, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options, Side};
use rf_nes::core::NesCore;
use w38_08_mm2_common::{
    forest_script, inputs_of, load_rom, poke, rom_path, NullSink, FILE, PLAY_FROM,
};

fn inputs() -> Vec<rf_core_api::InputFrame> {
    inputs_of(&forest_script())
}

fn frames() -> u32 {
    forest_script().len() as u32
}

fn record(raw: &[u8]) -> rf_core_api::coverage::CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for (f, input) in inputs().iter().enumerate() {
        poke(f, &mut core);
        core.run_frame(input, &mut NullSink);
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
    let dir = std::env::temp_dir().join(format!("rf-w38-08-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut hook = |frame: u32, side: Side, core: &mut NesCore| {
        poke(frame as usize, core);
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
    let r = run(raw, &atlas, inputs(), opts).expect("lockstep runs");
    (
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats,
        prints.len(),
    )
}

#[test]
#[ignore = "needs Mega Man 2 (USA) (RF_SCREENS_ROMS_DIR); about a minute in release"]
fn mm2_plain_image_has_zero_divergence() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let (nframes, div, stats, prints) = lockstep(&raw, "plain");
    println!(
        "plain image: {nframes} frames, divergence {div:?}, {:.3}% fallback ({} instrs), {prints} distinct nametable states",
        stats.fallback_rate() * 100.0,
        stats.instructions()
    );
    assert_eq!(div, None);
    assert_eq!(nframes, frames());
    assert!(prints >= 4, "the script scrolled the playfield ({prints})");
}

/// What the hook sink saw: events per (function id, enter), and Link's
/// position at each exit of the tick function.
#[derive(Default)]
struct Seen {
    events: BTreeMap<(u32, bool), u32>,
    tick_exit: Vec<(u32, u32, (u8, u8))>,
    tick_enter: Vec<(u32, u32, (u8, u8))>,
}

struct Sink {
    seen: Rc<RefCell<Seen>>,
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
        let at = (ctx.peek(self.pos.0), ctx.peek(self.pos.1));
        if enter {
            s.tick_enter.push((f, self.frame.get(), at));
        } else {
            s.tick_exit.push((f, self.frame.get(), at));
        }
    }
}

fn recipe_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/nes/mega-man-2")
}

#[test]
#[ignore = "needs Mega Man 2 (USA) (RF_SCREENS_ROMS_DIR); about a minute in release"]
fn mm2_recipe_hooks_fire_each_frame_and_keep_lockstep() {
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
        poke(frame as usize, core);
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
                pos,
                frame: Rc::clone(&frame_now),
            })
        })),
        before_frame: Some(&mut hook),
        ..Options::default()
    };
    let r = run(&raw, &atlas, inputs(), opts).expect("lockstep runs");
    println!(
        "lockstep with the recipe's hooks: {} frames, divergence {:?}, {:.3}% fallback",
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats.fallback_rate() * 100.0
    );
    assert_eq!(r.divergence.as_ref().map(|d| d.frame), None);
    assert_eq!(r.frames_run, frames());
    let seen = seen.borrow();
    let eof = end_of_frame.borrow();
    let exits: Vec<&(u32, u32, (u8, u8))> = seen.tick_exit.iter().filter(|e| e.0 == tick).collect();
    println!("tick exits {} over {} frames", exits.len(), frames());
    // Per hooked function: how often its exit sees the player's position as it
    // is at the end of that frame (`eof[f + 1]`) while in play. A function whose
    // exit position is final marks the end of the logic frame.
    let mut best = (0u32, 0.0f64);
    let mut by_fn: BTreeMap<u32, (usize, usize, usize)> = BTreeMap::new();
    for (g, f, at) in &seen.tick_exit {
        if *f >= PLAY_FROM {
            let e = by_fn.entry(*g).or_default();
            e.1 += 1;
            e.0 += usize::from(eof.get(*f as usize + 1) == Some(at));
            e.2 += usize::from(eof.get(*f as usize) == Some(at));
        }
    }
    for (g, (same, n, prev)) in &by_fn {
        let frac = *same as f64 / (*n).max(1) as f64;
        println!(
            "function {g}: exit position equals the end of that frame in {frac:.4}, of the previous frame in {:.4}, of {n} exits in play",
            *prev as f64 / (*n).max(1) as f64
        );
        if frac > best.1 {
            best = (*g, frac);
        }
    }
    let mut enter_by: BTreeMap<u32, (usize, usize, usize)> = BTreeMap::new();
    for (g, f, at) in &seen.tick_enter {
        if *f >= PLAY_FROM {
            let e = enter_by.entry(*g).or_default();
            e.1 += 1;
            e.0 += usize::from(eof.get(*f as usize + 1) == Some(at));
            e.2 += usize::from(eof.get(*f as usize) == Some(at));
        }
    }
    for (g, (same, n, prev)) in &enter_by {
        println!(
            "function {g}: ENTER position equals the end of that frame in {:.4}, of the previous frame in {:.4}, of {n} entries in play",
            *same as f64 / (*n).max(1) as f64,
            *prev as f64 / (*n).max(1) as f64
        );
    }
    println!(
        "best end-of-logic candidate: function {} at {:.4}",
        best.0, best.1
    );
    assert!(
        exits.len() * 100 >= frames() as usize * 95 && exits.len() <= frames() as usize + 4,
        "the tick hook fires about once per frame: {}",
        exits.len()
    );
    // Mega Man 2's per-frame logic runs outside the NMI handler (no write to
    // the player or object bytes was seen inside it): it finishes before the
    // vblank, so the position at the NMI's ENTRY is the frame's final one, and
    // at its EXIT it is still the same (the next logic has not run yet).
    let (same, n, _) = enter_by.get(&tick).copied().unwrap_or_default();
    assert!(
        n > 1000 && same * 100 >= n * 99,
        "the position at the NMI entry is final for the frame: {same} of {n}"
    );
    for ((f, enter), n) in &seen.events {
        println!(
            "function {f} {} x{n}",
            if *enter { "enter" } else { "exit" }
        );
    }
}
