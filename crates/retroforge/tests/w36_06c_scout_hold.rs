//! **W36-06c: what a mod session's level scout holds down** (no ROM needed).
//!
//! The live-enemies mod keeps enemies alive from the level's first frame, so the scout's walking
//! player meets them before the camera has moved (measured in `w36_06c_screens`: with the objects
//! alive its passes end after 1,594 frames and 128 k cells; with their flags held at zero, 2,062
//! frames and 148 k cells). The hold rows are the recipe's object-slot flag bytes.

use std::path::Path;

use retroforge::margin_draw::object_flag_addrs;

#[test]
fn the_scout_holds_every_object_slot_flag_of_the_recipe_at_zero() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/super-mario-bros/recipe.toml");
    let recipe = rf_profiles::load_recipe_file(&path).expect("the SMB recipe parses");
    // `enemy_flag`: five slots from $000F.
    assert_eq!(
        object_flag_addrs(&recipe),
        (0x0F..=0x13).collect::<Vec<u32>>()
    );
}

#[test]
fn a_recipe_without_an_object_table_holds_nothing() {
    assert!(object_flag_addrs(&rf_rebuild::Recipe::default()).is_empty());
}
