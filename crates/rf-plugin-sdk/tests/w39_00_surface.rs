//! Ticket W39-00: one shipped sample of each plugin kind loads and runs.
//! The logic plugin's real lockstep gate is `crates/retroforge/tests/
//! w39_00_logic_gate.rs` (the SDK cannot name a console core).

use std::path::PathBuf;

use rf_plugin_sdk::sandbox::{MemoryWindow, OverlayCmd};
use rf_plugin_sdk::{
    enable, FactPlugin, PluginKind, RenderPlugin, SurfaceError, SurfaceManifest, SurfaceRefusal,
};
use rf_rebuild::{Grade, RamMeaning, Recipe};

fn dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/samples")
        .join(name)
}

fn read(name: &str, file: &str) -> String {
    std::fs::read_to_string(dir(name).join(file)).unwrap_or_else(|e| panic!("{name}/{file}: {e}"))
}

fn manifest(name: &str) -> SurfaceManifest {
    SurfaceManifest::parse(&read(name, "plugin.toml")).expect("the sample manifest validates")
}

#[test]
fn the_three_samples_declare_their_kinds() {
    assert_eq!(manifest("fact-actor-names").kind, PluginKind::Fact);
    assert_eq!(manifest("render-actor-boxes").kind, PluginKind::Render);
    assert_eq!(manifest("logic-mode-flag").kind, PluginKind::Logic);
}

#[test]
fn the_fact_sample_adds_candidate_rows_and_cannot_demote_a_trusted_one() {
    let m = manifest("fact-actor-names");
    let p = FactPlugin::load(&m, &read("fact-actor-names", "facts.toml")).expect("loads");
    // The base already trusts a different meaning for $0300.
    let base = Recipe {
        id: "sample".into(),
        ram: vec![RamMeaning {
            addr: 0x0300,
            width: 1,
            name: "scratch".into(),
            source: "base".into(),
            grade: Grade::Confirmed,
        }],
        ..Recipe::default()
    };
    let merged = p.apply(&base).expect("no patches");
    assert_eq!(merged.conflicts, vec!["ram 0x0300".to_string()]);
    assert_eq!(merged.recipe.ram[0].name, "scratch", "trusted row kept");
    assert_eq!(merged.added, 1, "the actors table");
    assert_eq!(merged.recipe.object_tables[0].grade, Grade::Candidate);

    // On an empty base both rows come in, as candidates.
    let merged = p.apply(&Recipe::default()).expect("no patches");
    assert_eq!((merged.added, merged.conflicts.len()), (2, 0));
    assert_eq!(merged.recipe.ram[0].grade, Grade::Candidate);
}

#[test]
fn the_render_sample_draws_boxes_for_active_slots_from_recipe_facts() {
    let facts = FactPlugin::load(
        &manifest("fact-actor-names"),
        &read("fact-actor-names", "facts.toml"),
    )
    .unwrap();
    let recipe = facts.apply(&Recipe::default()).unwrap().recipe;
    // actors: x_lo column at $0400, y at $0404, state at $0408; 4 slots.
    let mut bytes = vec![0u8; 0x500];
    for (slot, (x, y, state)) in [(10u8, 20u8, 1u8), (30, 40, 0), (50, 60, 1), (70, 80, 0)]
        .into_iter()
        .enumerate()
    {
        bytes[0x400 + slot] = x;
        bytes[0x404 + slot] = y;
        bytes[0x408 + slot] = state;
    }
    let mut p = RenderPlugin::load(
        &manifest("render-actor-boxes"),
        &read("render-actor-boxes", "main.lua"),
        &recipe,
        MemoryWindow { base: 0, bytes },
    )
    .expect("loads");
    let drawn = p.draw(1);
    assert_eq!(drawn.len(), 2, "two active slots: {drawn:?}");
    assert!(matches!(drawn[0], OverlayCmd::Rect { x: 6, y: 16, .. }));
    assert!(matches!(drawn[1], OverlayCmd::Rect { x: 46, y: 56, .. }));

    // Draw data only: nothing it did is a write.
    assert!(p.host().ledger().is_empty());
    // With no facts the same script draws nothing (and does not fault).
    let mut empty = RenderPlugin::load(
        &manifest("render-actor-boxes"),
        &read("render-actor-boxes", "main.lua"),
        &Recipe::default(),
        MemoryWindow::default(),
    )
    .unwrap();
    assert!(empty.draw(1).is_empty());
}

#[test]
fn loaders_refuse_a_plugin_of_the_wrong_kind() {
    let logic = manifest("logic-mode-flag");
    assert!(matches!(
        FactPlugin::load(&logic, ""),
        Err(SurfaceError::WrongKind { .. })
    ));
    assert!(matches!(
        RenderPlugin::load(&logic, "", &Recipe::default(), MemoryWindow::default()),
        Err(SurfaceError::WrongKind { .. })
    ));
}

#[test]
fn the_logic_sample_is_not_enabled_without_a_lockstep_proof() {
    let logic = manifest("logic-mode-flag");
    assert!(logic.manifest.capabilities.write_memory);
    assert!(matches!(
        enable(&logic, None),
        Err(SurfaceRefusal::NoProof { .. })
    ));
    // Fact and render samples need none.
    assert!(enable(&manifest("fact-actor-names"), None).is_ok());
    assert!(enable(&manifest("render-actor-boxes"), None).is_ok());
}
