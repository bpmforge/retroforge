//! The shipped example plugin must actually load and run in the real
//! sandbox (ticket W4-04, FR-REND-006).
//!
//! Without this, `plugins/examples/` is documentation that has never been
//! executed — the same class of gap as a fetched-but-unrun test ROM
//! (`docs/TESTING.md` §4: "a downloaded ROM is not a tested ROM").

use std::path::PathBuf;

use rf_plugin_sdk::{Budget, Manifest, ScriptHost, ScriptState};

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("plugins/examples/player-overlay")
}

#[test]
fn the_shipped_example_plugin_loads_and_runs_frames() {
    let dir = example_dir();
    let manifest_text =
        std::fs::read_to_string(dir.join("plugin.toml")).expect("the example manifest must exist");
    let source =
        std::fs::read_to_string(dir.join("main.lua")).expect("the example script must exist");

    let manifest = Manifest::parse(&manifest_text).expect("the example manifest must validate");
    assert_eq!(manifest.id, "player-overlay");
    assert!(
        !manifest.license.is_empty() && !manifest.provenance.is_empty(),
        "the shipped example must model the required fields, not omit them"
    );
    assert!(manifest.capabilities.read_memory);
    assert!(manifest.capabilities.draw_overlay);
    assert!(
        !manifest.capabilities.write_memory,
        "an example that asks for the mod-tier capability teaches the wrong habit"
    );

    let mut host =
        ScriptHost::load(manifest, &source, Budget::default()).expect("example script loads");

    // Run enough frames to cross the script's own `frame % 600` branch,
    // so the whole callback body is exercised rather than just its first
    // few lines.
    for frame in 1..=601 {
        host.on_frame(frame);
    }

    assert_eq!(
        host.state(),
        &ScriptState::Running,
        "the shipped example must not fault or get throttled: {:?}",
        host.state()
    );
    assert!(
        host.log.lines().iter().any(|l| l.contains("still running")),
        "the example's own print() branch must have been reached — otherwise this test \
         only proves the file parses, not that it runs: {:?}",
        host.log.lines()
    );
}

/// The example must not be reaching for anything the sandbox forbids —
/// a doc that demonstrates an unavailable API is worse than no doc.
#[test]
fn the_example_script_uses_no_forbidden_globals() {
    let source = std::fs::read_to_string(example_dir().join("main.lua")).unwrap();
    for forbidden in ["io.", "os.", "require(", "dofile(", "loadfile(", "load("] {
        assert!(
            !source.contains(forbidden),
            "the shipped example uses `{forbidden}`, which the sandbox removes"
        );
    }
}
