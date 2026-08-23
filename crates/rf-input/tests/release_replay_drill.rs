//! The `.rfreplay` half of the release migration drill (ticket W5-05
//! criterion 4, R-F3; FR-STATE-005's sibling obligation).
//!
//! R-F3 names "`.rfstate`/`.rfreplay`" — both. `rf-state`'s
//! `every_archived_release_fixture_still_loads` covers the state half and
//! cannot cover this one: parsing a replay is `rf-input`'s job, and
//! reaching across for it would have put a cross-crate dependency in a
//! test purely to avoid creating a file. So the replay drill lives where
//! replays live.
//!
//! Same shape and same reasoning as the state drill: it WALKS
//! `fixtures/releases/` rather than naming files, because a hardcoded
//! list needs editing at every release and the edit that gets forgotten
//! is exactly the one that matters.

use rf_input::replay::ReplayLog;

/// Every `.rfreplay` archived from a previous release must still parse.
///
/// Asserts parsing only, deliberately. A replay from an OLD release is
/// not expected to *replay identically* on today's emulator — that would
/// forbid every legitimate accuracy fix the project ever makes, and the
/// determinism suites are what pin replay-vs-record equivalence for the
/// CURRENT build. What FR-STATE-005 asks, and what a user would actually
/// lose, is the file becoming unreadable.
///
/// Zero archived releases is a PASS that says so — a drill that quietly
/// does nothing is how this requirement becomes ceremonial, which is the
/// failure R-F3 exists to prevent.
#[test]
fn every_archived_release_replay_still_parses() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/releases");
    if !root.is_dir() {
        eprintln!("replay drill: no fixtures/releases/ yet — 0 prior releases");
        return;
    }

    let mut found = Vec::new();
    let mut stack = vec![root];
    // Drained by `pop`, so it advances on every iteration by construction
    // (law 8).
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
            if path.extension().is_none_or(|e| e != "rfreplay") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("archived replay {} unreadable: {e}", path.display()));
            ReplayLog::parse(&text).unwrap_or_else(|e| {
                panic!(
                    "R-F3 VIOLATED: {} no longer parses: {e:?}\n  \
                     A released replay that stops loading is a format migration \
                     this project owes its users, not a fixture to delete.",
                    path.display()
                )
            });
            found.push(path.display().to_string());
        }
    }

    found.sort();
    eprintln!(
        "replay drill: {} archived replay(s) still parse",
        found.len()
    );
    for f in &found {
        eprintln!("    {f}");
    }
}
