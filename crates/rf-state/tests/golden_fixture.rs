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

/// Every `.rfstate` archived from a previous release must still load
/// (FR-STATE-005; the executed migration drill, R-F3, W5-05).
///
/// # Why this walks a directory instead of naming files
///
/// FR-STATE-005 says fixtures "from each release" must load "forever".
/// A hardcoded list would have to be edited at every release, and the
/// edit that gets forgotten is exactly the one that matters — the
/// obligation is on the archive, so the archive is what gets walked.
/// `scripts/release.sh` writes into it; this reads whatever is there.
///
/// # What it asserts, and what it deliberately does not
///
/// It asserts each archived container **decodes on today's code**, which
/// is the whole of FR-STATE-005: a chunk version that can no longer be
/// migrated fails the build rather than being discovered by a player.
/// It does NOT compare against `golden_container()` — an OLD release's
/// fixture is not expected to match TODAY's logical state, and asserting
/// that would forbid every legitimate state change the project ever
/// makes. The current-release fixture is what
/// [`golden_fixture_loads_on_current_main_and_matches_expected_state`]
/// pins content-wise.
///
/// # Zero archived releases is a PASS, and says so
///
/// At v0 there is no previous release, so this finds nothing. That is
/// recorded rather than silently skipped: a drill that quietly does
/// nothing is how FR-STATE-005 becomes ceremonial, which is the failure
/// R-F3 was written to prevent.
#[test]
fn every_archived_release_fixture_still_loads() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/releases");
    if !root.is_dir() {
        eprintln!("migration drill: no fixtures/releases/ yet — 0 prior releases");
        return;
    }

    let mut checked = 0usize;
    let mut releases = Vec::new();
    let mut stack = vec![root.clone()];
    // Drained by `pop`, so it advances on every iteration by construction
    // (law 8) — there is no index to forget to increment.
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rfstate") {
                continue;
            }
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("archived fixture {} unreadable: {e}", path.display()));
            let (_decoded, warnings) = Container::decode_default(&bytes).unwrap_or_else(|e| {
                panic!(
                    "FR-STATE-005 VIOLATED: {} no longer loads: {e:?}\n  \
                     A released fixture that stops decoding is a migration this \
                     project owes its users, not a fixture to delete.",
                    path.display()
                )
            });
            // Warnings are allowed here and are not on the current-release
            // fixture: an older container legitimately lacks chunks that
            // were added since, and the loader reporting that is correct.
            if !warnings.is_empty() {
                eprintln!(
                    "  {} loaded with {} warning(s)",
                    path.display(),
                    warnings.len()
                );
            }
            releases.push(path.display().to_string());
            checked += 1;
        }
    }

    releases.sort();
    eprintln!("migration drill: {checked} archived fixture(s) still load");
    for r in &releases {
        eprintln!("    {r}");
    }
}
