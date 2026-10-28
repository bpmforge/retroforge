//! **W38-08: lockstep on Mega Man X** (ROM-gated, law 5).
//!
//! The scripted highway-stage play of `w38_08_mmx_common` runs on the plain
//! core (reference) and on the rebuilt core after every frame with zero
//! divergence required: on the unmodified image, then with every hooked
//! function of the recipe live. The vblank hook is counted per frame. Only
//! counts and hashes are printed; no ROM bytes leave the test (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_08_mmx_lockstep -- --ignored --nocapture
//! ```

mod w38_08_mmx_common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use rf_core_api::{CoreSink, EmulatorCore, FunctionEvent, HookCtx, ResetKind};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::lockstep::{Options, Side};
use rf_harness::lockstep_snes::run;
use rf_snes::core::SnesCore;
use w38_08_mmx_common::{highway_script, inputs_of, load_rom, rom_path, NullSink, FILE};

fn atlas_of(raw: &[u8]) -> rf_rebuild::Atlas {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for input in inputs_of(&highway_script()) {
        core.system_mut().bus.joypads.ports[0] = input.ports[0];
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut NullSink);
    }
    build_snes_atlas(raw, &core.take_coverage().expect("armed")).expect("atlas")
}

#[test]
#[ignore = "needs Mega Man X (USA) (RF_SCREENS_ROMS_DIR); minutes in release"]
fn mmx_plain_image_has_zero_divergence() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let atlas = atlas_of(&raw);
    let n = highway_script().len() as u32;
    let r = run(
        &raw,
        &atlas,
        inputs_of(&highway_script()),
        Options::default(),
    )
    .expect("lockstep runs");
    println!(
        "plain image: {} frames, divergence {:?}, {:.3}% fallback ({} instrs)",
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats.fallback_rate() * 100.0,
        r.stats.instructions()
    );
    assert_eq!(r.divergence.as_ref().map(|d| d.frame), None);
    assert_eq!(r.frames_run, n);
}

#[derive(Default)]
struct Seen {
    events: BTreeMap<(u32, bool), u32>,
}

struct Sink {
    seen: Rc<RefCell<Seen>>,
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, _y: u16, _p: &[rf_core_api::PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: rf_core_api::CoreEvent) {}
    fn function_event(&mut self, ev: FunctionEvent, _ctx: &mut dyn HookCtx) {
        let (f, enter) = match ev {
            FunctionEvent::Enter { function } => (function, true),
            FunctionEvent::Exit { function } => (function, false),
        };
        *self.seen.borrow_mut().events.entry((f, enter)).or_default() += 1;
    }
}

#[test]
#[ignore = "needs Mega Man X (USA) (RF_SCREENS_ROMS_DIR); minutes in release"]
fn mmx_recipe_hooks_fire_each_frame_and_keep_lockstep() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/snes/mega-man-x");
    let recipe = rf_profiles::load_recipe_file(&dir.join("recipe.toml")).expect("recipe");
    let atlas = atlas_of(&raw);
    let report = rf_rebuild::match_recipe(&atlas, &recipe);
    assert!(
        report.unmatched.is_empty(),
        "unmatched rows: {:?}",
        report.unmatched
    );
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
    let nmi_ids: Vec<u32> = recipe
        .functions
        .iter()
        .filter(|f| f.entry == Some(rf_rebuild::EntryKind::Nmi))
        .filter_map(|f| {
            report
                .matched
                .iter()
                .find(|m| m.name == f.name)
                .map(|m| m.function)
        })
        .collect();
    println!("{} hooked functions, {} NMI rows", ids.len(), nmi_ids.len());
    for f in &recipe.functions {
        let id = report
            .matched
            .iter()
            .find(|m| m.name == f.name)
            .map(|m| m.function);
        println!(
            "ROW {} id {id:?} entry {:?} insns {:?} at {:#x?}",
            f.name, f.entry, f.insn_count, f.address_hint
        );
    }
    assert!(!nmi_ids.is_empty());
    let seen = Rc::new(RefCell::new(Seen::default()));
    let factory_seen = Rc::clone(&seen);
    let mut hook = |_frame: u32, _side: Side, _core: &mut SnesCore| {};
    let opts = Options {
        function_hooks: ids,
        hook_sink: Some(Box::new(move || {
            *factory_seen.borrow_mut() = Seen::default();
            Box::new(Sink {
                seen: Rc::clone(&factory_seen),
            })
        })),
        before_frame: Some(&mut hook),
        ..Options::default()
    };
    let n = highway_script().len() as u32;
    let r = run(&raw, &atlas, inputs_of(&highway_script()), opts).expect("lockstep runs");
    println!(
        "lockstep with the recipe's hooks: {} frames, divergence {:?}, {:.3}% fallback",
        r.frames_run,
        r.divergence.as_ref().map(|d| d.frame),
        r.stats.fallback_rate() * 100.0
    );
    assert_eq!(r.divergence.as_ref().map(|d| d.frame), None);
    assert_eq!(r.frames_run, n);
    let seen = seen.borrow();
    let nmi_exits: u32 = nmi_ids
        .iter()
        .map(|id| seen.events.get(&(*id, false)).copied().unwrap_or(0))
        .sum();
    println!("NMI-row exits {nmi_exits} over {n} frames");
    for ((f, enter), c) in &seen.events {
        println!(
            "function {f} {} x{c}",
            if *enter { "enter" } else { "exit" }
        );
    }
    assert!(
        nmi_exits * 100 >= n * 95 && nmi_exits <= n + 8,
        "the vblank hooks fire about once per frame: {nmi_exits}"
    );
}
