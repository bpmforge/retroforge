//! Golden fixture harness (FR-STATE-005: "every released `.rfstate`
//! fixture loads on current main").
//!
//! Deliberately does NOT byte-compare a freshly zstd-compressed re-encode
//! against the checked-in file: libzstd does not guarantee identical
//! compressed output across library versions, so a `cargo update` that
//! bumps `zstd-sys` could break a byte-exact comparison with no rf-state
//! code change. Instead this test proves the thing FR-STATE-005 actually
//! asks for — the checked-in bytes decode successfully today and their
//! *decoded content* matches the same logical state — which is a stable
//! invariant independent of the compressor's exact output bytes.
//!
//! [`encode_is_byte_deterministic_within_a_run`] (in `container.rs`)
//! separately proves that encoding is deterministic for a given process/
//! library version, which is what "golden fixture bytes stable" means in
//! practice.

mod support;

use rf_state::Container;
use support::golden_container;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/golden_v1.rfstate"
);

#[test]
fn golden_fixture_loads_on_current_main_and_matches_expected_state() {
    let bytes = std::fs::read(FIXTURE_PATH).expect("checked-in golden fixture must exist");
    let (decoded, warnings) =
        Container::decode_default(&bytes).expect("golden fixture must still load");
    assert!(
        warnings.is_empty(),
        "golden fixture should load with zero warnings"
    );

    let expected = golden_container().unwrap();
    assert_eq!(decoded.header, expected.header);
    assert_eq!(decoded.chunks(), expected.chunks());
    assert_eq!(
        decoded.state_hash(),
        expected.state_hash(),
        "golden fixture must decode to the same logical machine state as support::golden_container()"
    );
}

/// Regenerates `tests/fixtures/golden_v1.rfstate` from
/// `support::golden_container()`. Not run by default (`cargo test`
/// excludes `#[ignore]`d tests) — run explicitly with
/// `cargo test -p rf-state --test golden_fixture -- --ignored` after a
/// deliberate, reviewed change to the golden container's logical content.
/// A bump like this should be rare and should ship with a version-bump
/// story of its own, per SAVE_STATES.md §2's migration rule.
#[test]
#[ignore = "run explicitly to intentionally regenerate the checked-in fixture"]
fn regenerate_golden_fixture() {
    let bytes = golden_container().unwrap().encode().unwrap();
    std::fs::write(FIXTURE_PATH, bytes).unwrap();
}
