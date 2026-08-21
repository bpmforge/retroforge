//! The two `room_grid` profiles decode their fixtures end to end
//! (ticket W9-08, criteria 1 and 2).
//!
//! **This is what stops "exercising a decoder family" from being a
//! declaration again.** `profiles/snes/example-mode7` declared
//! `kind = "room_grid"` from W4-02 until this ticket while configuring
//! nothing and decoding nothing — it loaded cleanly the whole time, so
//! the corpus test (criterion 3) would never have caught it. Loading is
//! not exercising.
//!
//! So each profile here is read from disk, resolved into a `Spec`, and
//! run against bytes rebuilt from its fixture's published construction
//! rule. The bytes are rebuilt here rather than read from a file on
//! purpose: if `generate.md`'s rule changes, this test still asserts the
//! old layout and goes red, instead of silently agreeing with whatever
//! the file now contains.

use std::path::{Path, PathBuf};

use rf_enhance::decode::room_grid::{decode, spec_from_profile};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/rf-enhance is two levels under the repo root")
        .to_path_buf()
}

fn load(rel: &str) -> rf_profiles::schema::Profile {
    let path = repo_root().join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));
    let outcome = rf_profiles::load_str(&text)
        .unwrap_or_else(|e| panic!("{} must load: {e}", path.display()));
    assert!(
        outcome.warnings.is_empty(),
        "{} must load WITHOUT WARNINGS (criterion 3): {:?}",
        path.display(),
        outcome.warnings
    );
    outcome.profile
}

/// `fixtures/nes/rf-rooms/generate.md`, rebuilt from the stated rule.
fn rf_rooms_image() -> Vec<u8> {
    let mut image = Vec::new();
    for tile in [0x10u8, 0x00, 0x20] {
        image.extend(std::iter::repeat_n(tile, 16));
    }
    image.extend_from_slice(&[2, 1, 1, 2, 2, 0, 0, 2, 2, 2, 2, 2]);
    image
}

/// `fixtures/snes/rf-rooms-flat/generate.md`, rebuilt from the stated rule.
fn rf_rooms_flat_image() -> Vec<u8> {
    let mut image = Vec::new();
    for tile in 1u8..=6 {
        image.extend(std::iter::repeat_n(tile, 4));
    }
    image
}

#[test]
fn the_indexed_profile_decodes_its_fixture_and_reuses_rooms() {
    // Criterion 1's substance: this is the shape metatile_screens cannot
    // express at all.
    let profile = load("profiles/nes/rf-rooms/profile.toml");
    let spec = spec_from_profile(&profile).expect("rf-rooms resolves");
    let level = decode(&rf_rooms_image(), &spec).expect("rf-rooms decodes");

    assert_eq!(level.level_size(), (16, 12));
    assert_eq!(
        level.rooms.len(),
        3,
        "THREE distinct rooms fill twelve cells — that reuse is the point"
    );
    assert_eq!(level.grid.len(), 12);

    // The documented layout: walls around, floor in the middle band.
    assert_eq!(level.room_at(0, 0), Some(2), "top-left is wall");
    assert_eq!(level.room_at(1, 0), Some(1), "open space");
    assert_eq!(level.room_at(1, 1), Some(0), "floor");
    assert_eq!(level.room_at(3, 2), Some(2), "bottom-right is wall");

    // And the tile bytes match FORMAT.md's table, so a wrong room is
    // visible rather than merely a wrong index.
    assert_eq!(level.tile_at(0, 0), Some(0x20), "wall tile");
    assert_eq!(level.tile_at(4, 4), Some(0x10), "floor tile");
    assert_eq!(level.tile_at(4, 0), Some(0x00), "open tile");
}

#[test]
fn the_unindexed_profile_decodes_its_fixture_in_reading_order() {
    // The family's other half. A profile that got `indexed` wrong would
    // decode garbage rather than erroring, which is why both halves need
    // a profile standing on them.
    let profile = load("profiles/snes/rf-rooms-flat/profile.toml");
    let spec = spec_from_profile(&profile).expect("rf-rooms-flat resolves");
    assert_eq!(spec.index_offset, None, "this half has no index table");
    let level = decode(&rf_rooms_flat_image(), &spec).expect("rf-rooms-flat decodes");

    assert_eq!(level.level_size(), (6, 4));
    assert_eq!(level.rooms.len(), 6, "six distinct rooms, no reuse");
    assert_eq!(level.grid, vec![0, 1, 2, 3, 4, 5], "cell N is room N");

    // Room n is the byte n+1, so a misread room is obvious.
    assert_eq!(level.tile_at(0, 0), Some(0x01));
    assert_eq!(level.tile_at(2, 0), Some(0x02));
    assert_eq!(level.tile_at(4, 2), Some(0x06), "last room, bottom-right");
}

#[test]
fn the_two_profiles_exercise_different_halves_of_the_family() {
    // Anti-vacuity for criterion 1: two profiles of the same shape would
    // pass both tests above while proving only one thing. The ticket's
    // note is explicit that the point is breadth, not count.
    let indexed = spec_from_profile(&load("profiles/nes/rf-rooms/profile.toml")).unwrap();
    let flat = spec_from_profile(&load("profiles/snes/rf-rooms-flat/profile.toml")).unwrap();
    assert!(indexed.index_offset.is_some());
    assert!(flat.index_offset.is_none());
}

#[test]
fn example_mode7_no_longer_declares_a_family_it_does_not_configure() {
    // The rot this ticket retired. From W4-02 until 2026-08-21 this
    // profile declared kind = "room_grid" with no table and no rom_map —
    // it loaded cleanly and decoded nothing, so no test caught it.
    //
    // Asserting the ABSENCE is what keeps it from coming back: a future
    // edit that re-adds an unbacked [decode] fails here.
    let profile = load("profiles/snes/example-mode7/profile.toml");
    assert!(
        profile.decode.is_none(),
        "example-mode7 must not declare a decoder family it cannot configure — \
         it has no fixture with room data behind it"
    );
}
