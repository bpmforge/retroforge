//! **W38-04: the widescreen overlay rows parse and are graded honestly** (data
//! only, no ROM). Each game's `recipe-widescreen.toml` is appended to its
//! `recipe.toml`; the result must load, every function a patch names must
//! exist, no two patch rows may edit the same byte of a function, lockstep
//! keepers (`widescreen-objects`) are `confirmed` and everything else
//! (`widescreen-candidates`) is `candidate`, which `resolve_patches` refuses.

use rf_profiles::load_recipe_str;
use rf_rebuild::{Grade, Recipe};

fn load(game: &str) -> (Recipe, Recipe) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let base = std::fs::read_to_string(dir.join(game).join("recipe.toml")).expect("recipe");
    let overlay =
        std::fs::read_to_string(dir.join(game).join("recipe-widescreen.toml")).expect("overlay");
    let before = load_recipe_str(&base).expect("the base recipe parses");
    let after = load_recipe_str(&format!("{base}\n{overlay}")).expect("the overlay appends");
    (before, after)
}

fn check(game: &str) {
    let (before, after) = load(game);
    assert!(
        after.patches.len() > before.patches.len(),
        "{game}: no rows"
    );
    for p in &after.patches[before.patches.len()..] {
        assert!(p.source.starts_with("observed:"), "{}", p.name);
        assert!(
            after.functions.iter().any(|f| f.name == p.function),
            "{}: unknown function {}",
            p.name,
            p.function
        );
        match p.mod_name.as_str() {
            "widescreen-objects" => assert_eq!(p.grade, Grade::Confirmed, "{}", p.name),
            "widescreen-candidates" => assert_eq!(p.grade, Grade::Candidate, "{}", p.name),
            other => panic!("{}: unexpected mod {other}", p.name),
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in &after.patches {
        assert!(
            seen.insert((p.function.clone(), p.offset)),
            "{game}: two rows edit {} byte {}",
            p.function,
            p.offset
        );
    }
    for f in &after.functions[before.functions.len()..] {
        assert!(f.exact_hash.is_some() && f.source.starts_with("observed:"));
    }
}

#[test]
fn the_smb_overlay_loads_and_is_graded() {
    check("nes/super-mario-bros");
    let (_, after) = load("nes/super-mario-bros");
    // The group trial kept four bytes: the three the W36-06 mod already edits
    // (not repeated here) and one more partner it found essential.
    let keepers = after
        .patches
        .iter()
        .filter(|p| p.mod_name == "widescreen-objects")
        .count();
    assert!(keepers >= 1, "{keepers}");
    assert!(after
        .patches
        .iter()
        .any(|p| p.mod_name == "widescreen-candidates"));
}

#[test]
fn the_smw_overlay_loads_and_is_graded() {
    check("snes/super-mario-world");
}
