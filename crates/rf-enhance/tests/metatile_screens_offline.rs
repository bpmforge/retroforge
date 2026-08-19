//! Ticket W5-02a's second acceptance criterion, taken literally: the
//! `metatile_screens` family decoded as a **pure function of ROM bytes**,
//! with no emulation anywhere in this file — no `EmuStepper`, no
//! `NesBus`, no `CoreSink`, no frame.
//!
//! ## Vacuity trap, and why the assertions look the way they do
//!
//! A decoder test that checks "48 columns x 14 rows of `u8` came back"
//! passes against a function that returns zeros. So every assertion here
//! pins **content**, against a source that is not the decoder:
//!
//! * The grid's shape and every metatile id are checked against
//!   `fixtures/nes/rf-scroller/src/leveldata.h`'s named constants
//!   (`SCREEN_COLS`, `SCREEN_ROWS`, `METATILE_COUNT`).
//! * The format's own invariant — every column's runs sum to exactly
//!   `SCREEN_ROWS` — is re-derived here **from the raw RLE bytes**,
//!   independently of the decoder, and compared.
//! * `FORMAT.md`'s description of the level's content ("sky above, a
//!   solid ground row at the bottom, and a landmark alternating A/B every
//!   4th column one row above the ground") is asserted as structure, so a
//!   decoder that produced a valid-but-wrong grid fails.
//!
//! ## The ROM is built, not fetched
//!
//! `fixtures/nes/rf-scroller/build.sh` produces it deterministically and
//! `rom.sha256` pins the result; CI builds it in its own step. Absent,
//! these tests skip loudly rather than pass quietly — a decoder suite
//! that silently ran zero assertions is the failure mode this whole file
//! is shaped against.

use std::path::{Path, PathBuf};

use rf_enhance::decode::metatile_screens::{self, Spec};
use rf_enhance::decode::DecodeError;

/// From `fixtures/nes/rf-scroller/src/leveldata.h`.
const SCREEN_COLS: u32 = 48;
const SCREEN_ROWS: u32 = 14;
const METATILE_COUNT: u32 = 4;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/rf-enhance is two levels under the repo root")
        .to_path_buf()
}

/// The fixture ROM, header-stripped.
///
/// **Normalization is not incidental.** Profile `rom_map` offsets are
/// into the normalized image, so handing the decoder a raw iNES file
/// shifts every table by 16 bytes — which does not error, it produces a
/// wrong level that looks like a level. `strips_the_header` below is the
/// test that pins this.
fn normalized_rom() -> Option<Vec<u8>> {
    let path = repo_root().join("fixtures/nes/rf-scroller/build/rf-scroller.nes");
    let raw = std::fs::read(path).ok()?;
    assert_eq!(&raw[0..4], b"NES\x1a", "fixture must be an iNES image");
    Some(raw[16..].to_vec())
}

fn profile_spec() -> Spec {
    let path = repo_root().join("profiles/nes/rf-scroller/profile.toml");
    let profile = rf_profiles::load_file(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .profile;
    metatile_screens::spec_from_profile(&profile).expect("rf-scroller declares metatile_screens")
}

/// Skip locally, **fail on CI**.
///
/// Same rule as `rf-harness`'s `gpu_or_skip` (ticket W3-06): a suite that
/// silently ran zero assertions is indistinguishable from one that
/// passed, and CI is exactly where nobody would notice. CI builds the
/// fixture in its own step before `cargo test --workspace`, so an absent
/// ROM there means that step broke, not that a developer lacks cc65.
fn skip(reason: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "{reason} — but CI builds the fixture before testing, so its absence here means the \
         fixture build step failed and this suite would otherwise have passed having asserted \
         nothing"
    );
    eprintln!("SKIP: {reason} — run fixtures/nes/rf-scroller/build.sh (needs cc65)");
}

/// **Criterion 1 + 2 together: driven by profile data, pure over bytes.**
///
/// The spec is not written here; it is read out of the shipped profile,
/// so a profile whose tables move breaks this test rather than quietly
/// decoding somewhere else.
#[test]
fn decodes_the_fixture_level_from_profile_data_alone() {
    let Some(rom) = normalized_rom() else {
        return skip("fixture ROM not built");
    };
    let spec = profile_spec();

    // The spec itself must match the fixture's own constants — if the
    // profile drifted, everything below would still "pass" against a
    // consistent but wrong grid.
    assert_eq!(spec.width, SCREEN_COLS, "profile width vs leveldata.h");
    assert_eq!(spec.height, SCREEN_ROWS, "profile height vs leveldata.h");

    let level = metatile_screens::decode(&rom, &spec).expect("the fixture level must decode");

    assert_eq!(level.width, SCREEN_COLS);
    assert_eq!(level.height, SCREEN_ROWS);
    assert_eq!(
        level.metatiles.len() as u32,
        SCREEN_COLS * SCREEN_ROWS,
        "every cell must be filled"
    );
    assert_eq!(
        level.metatile_count, METATILE_COUNT,
        "the fixture defines METATILE_COUNT = {METATILE_COUNT} metatiles"
    );

    // Anti-vacuity: a decoder returning zeros would satisfy every shape
    // assertion above.
    let distinct: std::collections::BTreeSet<u8> = level.metatiles.iter().copied().collect();
    assert!(
        distinct.len() >= 3,
        "a level of {} distinct metatile ids is not a decode, it is a fill: {distinct:?}",
        distinct.len()
    );
    for id in &distinct {
        assert!(
            u32::from(*id) < METATILE_COUNT,
            "metatile id {id} is outside the {METATILE_COUNT}-entry table"
        );
    }

    // FORMAT.md's "Level content": a solid ground row at the bottom
    // (metatile row SCREEN_ROWS-1) running the whole width, and sky above
    // it. Structure, not a golden blob — W5-02b owns the goldens.
    let ground_row = SCREEN_ROWS - 1;
    let ground: std::collections::BTreeSet<u8> = (0..SCREEN_COLS)
        .map(|c| level.at(c, ground_row).expect("in bounds"))
        .collect();
    assert_eq!(
        ground.len(),
        1,
        "FORMAT.md says the bottom metatile row is solid ground the whole way across; got \
         {ground:?}"
    );
    let sky = level.at(0, 0).expect("in bounds");
    assert_ne!(
        sky,
        *ground.iter().next().unwrap(),
        "sky at the top and ground at the bottom must not be the same metatile"
    );

    // The landmark row: FORMAT.md says a landmark alternates A/B every
    // 4th column, one row above the ground. So that row must contain more
    // than one id, and they must not all be sky.
    let landmark_row = SCREEN_ROWS - 2;
    let landmarks: std::collections::BTreeSet<u8> = (0..SCREEN_COLS)
        .map(|c| level.at(c, landmark_row).expect("in bounds"))
        .collect();
    assert!(
        landmarks.len() >= 2,
        "the row above the ground carries the alternating landmarks; got {landmarks:?}"
    );

    let collision = level
        .collision
        .as_ref()
        .expect("the profile declares a collision table");
    assert_eq!(collision.len() as u32, METATILE_COUNT);
    assert_eq!(
        level.tiles_for(0).map(<[u8]>::len),
        Some(spec.metatile_size as usize),
        "each metatile resolves to `metatile.size` CHR tile ids"
    );
}

/// **The format's invariant, re-derived from raw bytes rather than
/// trusted.** This walks `level_column_offset`/`level_rle_data` by hand —
/// the decoder is not involved in producing the expectation — and then
/// checks the decoder agrees column for column.
///
/// This is the assertion that would catch an off-by-one in the
/// column-span arithmetic, which is the single most likely bug in a
/// packed-RLE reader and the one a shape check cannot see.
#[test]
fn every_column_matches_an_independent_walk_of_the_raw_rle_bytes() {
    let Some(rom) = normalized_rom() else {
        return skip("fixture ROM not built");
    };
    let spec = profile_spec();
    let level = metatile_screens::decode(&rom, &spec).expect("decodes");

    let offsets = &rom[spec.column_offsets as usize..][..spec.width as usize];
    let data = &rom[spec.level_data as usize..][..spec.level_data_len as usize];

    for col in 0..spec.width as usize {
        let start = usize::from(offsets[col]);
        let end = if col + 1 < offsets.len() {
            usize::from(offsets[col + 1])
        } else {
            data.len()
        };
        let mut expected = Vec::new();
        for pair in data[start..end].chunks_exact(2) {
            expected.extend(std::iter::repeat_n(pair[1], usize::from(pair[0])));
        }
        assert_eq!(
            expected.len() as u32,
            SCREEN_ROWS,
            "FORMAT.md's invariant: column {col}'s runs must sum to exactly {SCREEN_ROWS}"
        );
        assert_eq!(
            level.column(col as u32).expect("in bounds"),
            &expected[..],
            "column {col} disagrees with a hand walk of the raw RLE bytes"
        );
    }
}

/// **Feeding the raw iNES file instead of the normalized image must not
/// look like a successful decode.** This is the trap the module doc
/// warns about: offsets shift by 16 bytes, which is not a crash.
///
/// The assertion is deliberately "either it errors, or it produces a
/// DIFFERENT level" rather than "it errors": whether a 16-byte skew
/// happens to land on data that still satisfies the invariant is a
/// property of this particular ROM, not something the decoder promises.
/// What must never happen is the two agreeing.
#[test]
fn a_raw_ines_image_does_not_silently_decode_as_the_normalized_one() {
    let path = repo_root().join("fixtures/nes/rf-scroller/build/rf-scroller.nes");
    let Ok(raw) = std::fs::read(&path) else {
        return skip("fixture ROM not built");
    };
    let normalized = raw[16..].to_vec();
    let spec = profile_spec();

    let good = metatile_screens::decode(&normalized, &spec).expect("normalized decodes");
    match metatile_screens::decode(&raw, &spec) {
        Err(_) => {} // the honest outcome
        Ok(skewed) => assert_ne!(
            skewed.metatiles, good.metatiles,
            "a 16-byte header skew produced an IDENTICAL level — the decoder is not reading \
             where the profile says it is"
        ),
    }
}

/// **Mutation: break the format's invariant and the decoder must refuse,
/// not repair.** A decoder that padded a short column to `height` would
/// pass every other test in this file while showing a subtly wrong level.
#[test]
fn a_corrupted_run_length_is_refused_rather_than_padded() {
    let Some(rom) = normalized_rom() else {
        return skip("fixture ROM not built");
    };
    let spec = profile_spec();
    metatile_screens::decode(&rom, &spec).expect("baseline decodes");

    // Shorten the first run of column 0 by one row.
    let mut broken = rom.clone();
    let first_pair = spec.level_data as usize + usize::from(rom[spec.column_offsets as usize]);
    assert!(broken[first_pair] > 1, "need a run longer than one row");
    broken[first_pair] -= 1;

    match metatile_screens::decode(&broken, &spec) {
        Err(DecodeError::MalformedLevel(msg)) => {
            assert!(
                msg.contains("column 0") && msg.contains(&SCREEN_ROWS.to_string()),
                "the error must name the column and the expected height: {msg}"
            );
        }
        Err(other) => panic!("wrong error for a short column: {other}"),
        Ok(_) => panic!(
            "a column whose runs sum to {} instead of {SCREEN_ROWS} was accepted — FORMAT.md's \
             invariant is not being enforced, and a level view built on this would be wrong \
             everywhere while looking fine",
            SCREEN_ROWS - 1
        ),
    }
}

/// A profile pointing at tables past the end of the ROM must say so,
/// naming what did not fit — the common case while someone is still
/// writing a profile.
#[test]
fn out_of_range_tables_are_reported_with_the_offending_table_named() {
    let Some(rom) = normalized_rom() else {
        return skip("fixture ROM not built");
    };
    let mut spec = profile_spec();
    spec.level_data = 0xFF_0000;
    match metatile_screens::decode(&rom, &spec) {
        Err(DecodeError::OutOfRange { what, .. }) => {
            assert_eq!(what, "level_rle_data");
        }
        other => panic!("expected an OutOfRange naming level_rle_data, got {other:?}"),
    }
}

/// The family must refuse a profile it cannot drive, naming the missing
/// piece — `MissingRomMapEntry` exists so a profile author is told which
/// `[[rom_map]]` label to add.
#[test]
fn a_profile_without_the_required_rom_map_labels_is_refused_by_name() {
    let path = repo_root().join("profiles/nes/rf-scroller-demo/profile.toml");
    let profile = rf_profiles::load_file(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .profile;
    // The W4-02 demo profile declares `kind = "metatile_screens"` but its
    // rom_map has only `metatile_table` — it was written to prove the
    // schema could EXPRESS the shape, before any decoder existed.
    match metatile_screens::spec_from_profile(&profile) {
        Err(DecodeError::MissingRomMapEntry { label, .. }) => {
            assert_eq!(label, metatile_screens::LABEL_COLUMN_OFFSETS);
        }
        other => panic!("expected MissingRomMapEntry, got {other:?}"),
    }
}
