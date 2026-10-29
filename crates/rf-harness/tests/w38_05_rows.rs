//! **W38-05: the Zelda recipe parses and is graded honestly** (data only, no
//! ROM). The recipe and its widescreen overlay load; every fact an
//! interpolation or margin consumer needs is present (the tick function with
//! both hook points, Link's position, the actor table, the screen index and the
//! scroll byte); every row says it was observed (D-021: no community RAM map, no
//! disassembly) and carries a grade that no lockstep trial or person has
//! contradicted: finder output is never `confirmed` unless a lockstep trial kept
//! it.

use rf_profiles::load_recipe_str;
use rf_rebuild::{EntryKind, Grade, Recipe};

fn load() -> (Recipe, Recipe) {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/nes/legend-of-zelda");
    let base = std::fs::read_to_string(dir.join("recipe.toml")).expect("recipe");
    let overlay = std::fs::read_to_string(dir.join("recipe-widescreen.toml")).expect("overlay");
    let before = load_recipe_str(&base).expect("the base recipe parses");
    let after = load_recipe_str(&format!("{base}\n{overlay}")).expect("the overlay appends");
    (before, after)
}

#[test]
fn the_zelda_recipe_has_the_facts_interpolation_needs() {
    let (r, _) = load();
    let nmi = r.functions.iter().find(|f| f.name == "nmi").expect("nmi");
    assert_eq!(nmi.entry, Some(EntryKind::Nmi));
    assert!(
        nmi.hooks.entry && nmi.hooks.exit,
        "the tick has both hook points"
    );
    assert!(nmi.exact_hash.is_some());
    for name in ["player_move", "actor_move"] {
        let f = r.functions.iter().find(|f| f.name == name).expect(name);
        assert!(
            f.hooks.entry && f.hooks.exit && f.exact_hash.is_some(),
            "{name}"
        );
    }
    for name in ["player_x", "player_y", "screen_index", "scroll_x"] {
        let row = r.ram.iter().find(|x| x.name == name).expect(name);
        assert_eq!(row.grade, Grade::Observed, "{name}");
    }
    let t = r
        .object_tables
        .iter()
        .find(|t| t.name == "actors")
        .expect("table");
    assert!(t.slots >= 3 && t.x_lo.is_some() && t.y.is_some());
    assert_eq!(t.grade, Grade::Observed);
    let px = r.ram.iter().find(|x| x.name == "player_x").unwrap().addr;
    let py = r.ram.iter().find(|x| x.name == "player_y").unwrap().addr;
    assert_eq!(t.base + t.x_lo.unwrap(), px, "slot 0 is Link");
    assert_eq!(t.base + t.y.unwrap(), py);
}

#[test]
fn every_zelda_row_is_observed_and_graded_honestly() {
    let (before, after) = load();
    let clean = |source: &str| {
        let l = source.to_lowercase();
        !l.contains("data crystal") && !l.contains("datacrystal") && !l.contains("disassembl")
    };
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
        assert!(t.source.starts_with("observed:") && clean(&t.source));
    }
    for d in &after.data_regions {
        assert!(clean(&d.source), "{}", d.name);
    }
    // Finder regions are candidates unless the screen experiment confirmed them.
    for d in &after.data_regions {
        if d.grade == Grade::Confirmed {
            assert!(d.source.contains("confirmed by"), "{}", d.name);
        }
    }
    // The overlay: keepers confirmed, the rest candidates (never applied).
    for p in &after.patches {
        assert!(p.source.starts_with("observed:"), "{}", p.name);
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
