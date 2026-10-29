//! **W38-06: the Super Metroid recipe parses and is graded honestly** (data
//! only, no ROM). The recipe and its widescreen overlay load; every fact an
//! interpolation or margin consumer needs is present (the tick function with
//! both hook points, the player's position words, the camera); every row says it
//! was observed (D-021: no community RAM map, no disassembly) and carries a
//! grade that no lockstep trial or person has contradicted.

mod w38_06_common;

use rf_profiles::load_recipe_str;
use rf_rebuild::{EntryKind, Grade, Recipe};

fn load() -> (Recipe, Recipe) {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/snes/super-metroid");
    let base = std::fs::read_to_string(dir.join("recipe.toml")).expect("recipe");
    let overlay = std::fs::read_to_string(dir.join("recipe-widescreen.toml")).expect("overlay");
    let before = load_recipe_str(&base).expect("the base recipe parses");
    let after = load_recipe_str(&format!("{base}\n{overlay}")).expect("the overlay appends");
    (before, after)
}

/// Sources that would mean a community map or a disassembly was used.
fn clean(source: &str) -> bool {
    let l = source.to_lowercase();
    ![
        "data crystal",
        "datacrystal",
        "disassembl",
        "kejardon",
        "ram map",
        "rammap",
        "metroid construction",
        "decompil",
    ]
    .iter()
    .any(|bad| l.contains(bad))
}

#[test]
fn the_pinned_script_expands_to_its_length() {
    assert_eq!(w38_06_common::script().len(), 27182);
    assert_eq!(w38_06_common::frames_of("w3 R+B2").len(), 5);
}

#[test]
fn the_super_metroid_recipe_has_the_facts_interpolation_needs() {
    let (r, _) = load();
    let nmi = r.functions.iter().find(|f| f.name == "nmi").expect("nmi");
    assert_eq!(nmi.entry, Some(EntryKind::Nmi));
    assert!(
        nmi.hooks.entry && nmi.hooks.exit,
        "the tick has both hook points"
    );
    assert!(nmi.exact_hash.is_some() && nmi.mode.is_some());
    // The NMI is entered in whichever decode mode the interrupted code was in,
    // and each mode is its own function: every one of them carries both hooks.
    let ticks: Vec<_> = r
        .functions
        .iter()
        .filter(|f| f.entry == Some(EntryKind::Nmi))
        .collect();
    assert!(ticks.len() >= 2, "one row per mode the NMI was entered in");
    assert!(ticks.iter().all(|f| f.hooks.entry && f.hooks.exit));
    for name in ["player_x", "player_y", "camera_x_lo"] {
        let row = r.ram.iter().find(|x| x.name == name).expect(name);
        assert_eq!(row.grade, Grade::Observed, "{name}");
    }
    assert_eq!(
        r.ram.iter().find(|x| x.name == "player_x").unwrap().width,
        2
    );
}

#[test]
fn every_super_metroid_row_is_observed_and_graded_honestly() {
    let (before, after) = load();
    for f in &after.functions {
        assert!(
            f.source.starts_with("observed:") || f.source.starts_with("rule:"),
            "{}: {}",
            f.name,
            f.source
        );
        assert!(clean(&f.source), "{}", f.name);
    }
    for x in &after.ram {
        assert!(
            x.source.starts_with("observed:") && clean(&x.source),
            "{}",
            x.name
        );
    }
    for t in &after.object_tables {
        assert!(clean(&t.source), "{}", t.name);
        assert_ne!(t.grade, Grade::Confirmed, "{}", t.name);
    }
    for d in &after.data_regions {
        assert!(clean(&d.source), "{}", d.name);
        if d.grade == Grade::Confirmed {
            assert!(d.source.contains("confirmed by"), "{}", d.name);
        }
    }
    for p in &after.patches {
        assert!(
            p.source.starts_with("observed:") && clean(&p.source),
            "{}",
            p.name
        );
        assert!(
            after.functions.iter().any(|f| f.name == p.function),
            "{}",
            p.name
        );
        match p.mod_name.as_str() {
            "widescreen-objects" => assert_eq!(p.grade, Grade::Confirmed, "{}", p.name),
            "widescreen-candidates" => assert_eq!(p.grade, Grade::Candidate, "{}", p.name),
            other => panic!("{}: unexpected mod {other}", p.name),
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in &after.patches {
        assert!(seen.insert((p.function.clone(), p.offset)), "{}", p.name);
    }
    assert!(before.patches.is_empty(), "the base recipe patches nothing");
    for f in &after.functions[before.functions.len()..] {
        assert!(f.exact_hash.is_some() && f.source.starts_with("observed:"));
    }
}

#[test]
fn the_source_filter_rejects_community_maps() {
    assert!(!clean("observed, matches Kejardon's RAM map"));
    assert!(!clean("from a Disassembly"));
    assert!(clean("observed: equals a sprite's x"));
}
