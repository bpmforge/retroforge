//! The committed SMB recipe parses and its patch rows are well-formed (data
//! only: no ROM). The ROM-backed resolution test lives in rf-harness
//! (`w36_06_live_enemies`).
use rf_profiles::load_recipe_file;

#[test]
fn the_smb_recipe_has_a_live_enemies_mod_with_sourced_rows() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/super-mario-bros/recipe.toml");
    let r = load_recipe_file(&path).expect("parses");
    assert_eq!(r.mods(), ["live-enemies"]);
    assert_eq!(r.patches.len(), 3);
    for p in &r.patches {
        assert!(p.source.starts_with("observed:"), "{}", p.name);
        let f = r.functions.iter().find(|f| f.name == p.function).unwrap();
        assert!(f.exact_hash.is_some());
    }
    for f in &r.functions {
        assert!(f.source.starts_with("observed:"), "{}", f.name);
    }
}
