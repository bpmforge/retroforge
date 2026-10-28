//! Ticket W39-00: a logic plugin that breaks Original mode is blocked.
//!
//! The gate is real shadow lockstep (`rf_harness::lockstep::run`, the same
//! checker W36-05 uses for hooks): the game runs with the plugin loaded in
//! `PlayMode::Original`, the rebuilt side is compared with the plain
//! interpreter, and the result becomes an `OriginalModeProof` that
//! `rf_plugin_sdk::enable` judges. Runs on a small synthetic cartridge (no
//! ROM, law 5); the shipped logic sample is the plugin under test, beside a
//! deliberately bad one that writes in every mode.

use std::path::PathBuf;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options, Report, Side};
use rf_nes::core::NesCore;
use rf_nes::rebuild::{entry, Atlas};
use rf_plugin_sdk::sandbox::MemoryWindow;
use rf_plugin_sdk::surface::recipe_bridge;
use rf_plugin_sdk::{
    enable, Budget, FactPlugin, HookHost, OriginalModeProof, PlayMode, SurfaceManifest,
    SurfaceRefusal,
};
use rf_rebuild::{match_recipe, HookPoints, Recipe, RecipeFunction};

struct Quiet;
impl CoreSink for Quiet {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn sample(name: &str, file: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/samples")
        .join(name)
        .join(file);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// NMI on; `JSR slow` forever; the NMI handler is `INC $12; RTI` at $8060.
fn nmi_rom() -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 1;
    d[5] = 1;
    let mut prg = vec![0xEAu8; 0x4000];
    let put = |prg: &mut Vec<u8>, off: usize, b: &[u8]| prg[off..off + b.len()].copy_from_slice(b);
    put(
        &mut prg,
        0x00,
        &[
            0xA9, 0x80, 0x8D, 0x00, 0x20, 0x20, 0x20, 0x80, 0x4C, 0x05, 0x80,
        ],
    );
    put(
        &mut prg,
        0x20,
        &[
            0xA0, 0x00, 0xA2, 0x00, 0xE8, 0xD0, 0xFD, 0xC8, 0xD0, 0xF8, 0x60,
        ],
    );
    put(&mut prg, 0x60, &[0xE6, 0x12, 0x40]);
    prg[0x3FFA..0x3FFC].copy_from_slice(&0x8060u16.to_le_bytes());
    prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    d.extend(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

/// The atlas function the NMI vector's target starts, as a recipe row.
fn nmi_row(raw: &[u8], atlas: &Atlas) -> RecipeFunction {
    let core = NesCore::from_ines_bytes(raw).expect("loads");
    let vector = u16::from_le_bytes([core.peek(0xFFFA), core.peek(0xFFFB)]);
    let f = atlas
        .functions
        .iter()
        .find(|f| f.entry_cpu == u32::from(vector) && f.entry_kinds & entry::INTERRUPT != 0)
        .expect("the NMI vector's target is an interrupt entry");
    RecipeFunction {
        name: "nmi".to_string(),
        shape_hash: f.shape_hash,
        exact_hash: Some(f.byte_hash),
        address_hint: Some(f.entry_cpu),
        hooks: HookPoints {
            entry: true,
            exit: true,
        },
        source: "observed: the atlas function the NMI vector targets".to_string(),
        ..RecipeFunction::default()
    }
}

/// Lockstep with a hook host per pass, built by `make`.
fn lockstep(
    raw: &[u8],
    atlas: &Atlas,
    recipe: &Recipe,
    frames: usize,
    make: impl Fn() -> HookHost + 'static,
) -> Report {
    let report = match_recipe(atlas, recipe);
    let build = move || {
        let h = make();
        h.resolve(&|n| report.function_of_writable(n));
        h
    };
    let ids = build().function_ids();
    // The ledger's frame number is not needed here: only divergence is read.
    let mut before = |_frame: u32, _side: Side, _core: &mut NesCore| {};
    let opts = Options {
        function_hooks: ids,
        hook_sink: Some(Box::new(move || Box::new(build()) as Box<dyn CoreSink>)),
        before_frame: Some(&mut before),
        ..Options::default()
    };
    run(raw, atlas, vec![InputFrame::empty(); frames], opts).expect("lockstep runs")
}

/// What the shell does with a lockstep report.
fn proof_of(plugin: &SurfaceManifest, r: &Report) -> OriginalModeProof {
    OriginalModeProof {
        plugin_id: plugin.manifest.id.clone(),
        frames: r.frames_run,
        divergence: r
            .divergence
            .as_ref()
            .map(|d| rf_plugin_sdk::surface::ProofDivergence {
                frame: d.frame,
                field: d.fields.first().map_or("?".into(), |f| f.field.to_string()),
            }),
    }
}

struct Fixture {
    raw: Vec<u8>,
    atlas: Atlas,
    recipe: Recipe,
}

fn fixture() -> Fixture {
    let raw = nmi_rom();
    let mut rec = NesCore::from_ines_bytes(&raw).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    for _ in 0..40 {
        rec.run_frame(&InputFrame::empty(), &mut Quiet);
    }
    let atlas = build_nes_atlas(&raw, &rec.take_coverage().expect("armed")).expect("atlas");
    // The recipe: an NMI function row, plus the `mod_flag` RAM fact that the
    // shipped fact plugin contributes (the logic sample finds its address
    // there, not in its own source).
    let base = Recipe {
        id: "sample".to_string(),
        engine: "sample".to_string(),
        functions: vec![nmi_row(&raw, &atlas)],
        ..Recipe::default()
    };
    let facts = FactPlugin::load(
        &SurfaceManifest::parse(&sample("fact-actor-names", "plugin.toml")).unwrap(),
        &sample("fact-actor-names", "facts.toml"),
    )
    .unwrap();
    let recipe = facts.apply(&base).expect("no patches").recipe;
    Fixture { raw, atlas, recipe }
}

fn lua_host(m: &SurfaceManifest, source: String, recipe: &Recipe, mode: PlayMode) -> HookHost {
    let bridge = recipe_bridge(recipe, MemoryWindow::default());
    let h = HookHost::load(m.manifest.clone(), &source, Budget::default(), &bridge)
        .expect("plugin loads");
    h.set_mode(mode);
    h
}

const FRAMES: usize = 60;

#[test]
fn the_logic_sample_passes_the_gate_and_really_acts_in_enhanced_mode() {
    let fx = fixture();
    let m = SurfaceManifest::parse(&sample("logic-mode-flag", "plugin.toml")).unwrap();
    let src = sample("logic-mode-flag", "main.lua");

    // Original mode: zero divergence, so the gate opens.
    let (mc, sc, rc) = (m.clone(), src.clone(), fx.recipe.clone());
    let r = lockstep(&fx.raw, &fx.atlas, &fx.recipe, FRAMES, move || {
        lua_host(&mc, sc.clone(), &rc, PlayMode::Original)
    });
    assert!(r.divergence.is_none(), "original mode: {:?}", r.divergence);
    assert_eq!(r.frames_run as usize, FRAMES);
    let proof = proof_of(&m, &r);
    println!(
        "original-mode proof: {} frames, divergence {:?}",
        proof.frames, proof.divergence
    );
    assert!(enable(&m, Some(&proof)).is_ok());

    // Anti-vacuity: the same plugin in Enhanced mode DOES change the game.
    let (mc, sc, rc) = (m.clone(), src, fx.recipe.clone());
    let r = lockstep(&fx.raw, &fx.atlas, &fx.recipe, FRAMES, move || {
        lua_host(&mc, sc.clone(), &rc, PlayMode::Enhanced)
    });
    let d = r.divergence.expect("enhanced mode must act");
    let wram = d.fields.iter().find(|f| f.field == "wram").expect("wram");
    println!(
        "enhanced mode: divergence frame {} wram offset {:#06x}",
        d.frame, wram.first_offset
    );
    assert_eq!(wram.first_offset, 0x0300, "the recipe's mod_flag");
}

#[test]
fn a_logic_plugin_that_writes_in_original_mode_is_blocked() {
    let fx = fixture();
    // The sample with its mode test deleted: the bad plugin.
    let bad_src = sample("logic-mode-flag", "main.lua")
        .replace("FLAG ~= nil and rf.mode() == \"enhanced\"", "FLAG ~= nil");
    assert!(!bad_src.contains("== \"enhanced\""), "fixture not made bad");
    let m = SurfaceManifest::parse(&sample("logic-mode-flag", "plugin.toml")).unwrap();

    let (mc, rc) = (m.clone(), fx.recipe.clone());
    let r = lockstep(&fx.raw, &fx.atlas, &fx.recipe, FRAMES, move || {
        lua_host(&mc, bad_src.clone(), &rc, PlayMode::Original)
    });
    let proof = proof_of(&m, &r);
    println!("bad plugin proof: {proof:?}");
    let refusal = enable(&m, Some(&proof)).expect_err("must be blocked");
    assert!(
        matches!(&refusal, SurfaceRefusal::BreaksOriginalMode { field, .. } if field == "wram"),
        "{refusal:?}"
    );
    let text = refusal.to_string();
    println!("{text}");
    assert!(text.contains("blocked") && text.contains("Original mode"));
}
