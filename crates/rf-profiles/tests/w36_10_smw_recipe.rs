//! The committed SMW recipe parses, every function row carries its entry mode
//! and a machine-name source, and every RAM row is an address the SMW profile
//! records (data only: no ROM). The ROM-backed reproduction test is
//! `crates/retroforge/tests/w36_10_smw_recipe.rs`.
use rf_profiles::load_recipe_file;

fn dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/snes/super-mario-world")
}

#[test]
fn the_smw_recipe_rows_carry_modes_and_the_ram_rows_come_from_the_profile() {
    let r = load_recipe_file(&dir().join("recipe.toml")).expect("parses");
    assert_eq!(r.id, "super-mario-world");
    assert!(r.functions.len() >= 20);
    let mut names: Vec<&str> = r.functions.iter().map(|f| f.name.as_str()).collect();
    for want in ["reset", "nmi"] {
        assert!(names.contains(&want), "{want}");
    }
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), r.functions.len(), "names are unique");
    for f in &r.functions {
        assert!(f.mode.is_some_and(|m| m <= 4), "{} has no mode", f.name);
        assert!(
            f.exact_hash.is_some() && f.address_hint.is_some(),
            "{}",
            f.name
        );
        assert!(f.source.starts_with("rule:"), "{}", f.name);
    }
    assert!(r.patches.is_empty());
    let profile = std::fs::read_to_string(dir().join("profile.toml")).expect("profile");
    let lower = profile.to_lowercase();
    for ram in &r.ram {
        let needle = format!("0x{:06x}", ram.addr);
        assert!(
            lower.contains(&needle),
            "{} ({needle}) not in the profile",
            ram.name
        );
    }
}
