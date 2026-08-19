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

    // Ticket W5-07: publish a memory map and a window BEFORE loading.
    // The example resolves `rf.profile.addr` once at load — the natural
    // way to use a published map — so labels that arrive afterwards
    // resolve to nil and the script correctly draws nothing. Driving it
    // the way the shell does is what makes this test exercise the script
    // rather than its early-return.
    let bridge = rf_plugin_sdk::sandbox::Bridge::default();
    let mut bytes = vec![0u8; 0x2000];
    bytes[0x29] = 0x40; // player_x low  ($6029)
    bytes[0x2A] = 0x01; // player_x high -> 320
    bytes[0x2B] = 0x20; // camera_x      ($602B)
    bridge.publish(
        rf_plugin_sdk::sandbox::MemoryWindow {
            base: 0x6000,
            bytes,
        },
        [
            ("player_x".to_string(), 0x6029u32),
            ("camera_x".to_string(), 0x602Bu32),
        ]
        .into_iter()
        .collect(),
    );

    let mut host = ScriptHost::load_with_bridge(manifest, &source, Budget::default(), bridge)
        .expect("example script loads");

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
    // The example draws one marker per frame at the published player
    // position — 320, so the rect's left edge is 316. Asserting the
    // POSITION and not merely that something was drawn is what
    // distinguishes a working data path from W4-04's stub, which
    // returned 0 for every read.
    let overlay = host.bridge().take_overlay();
    assert_eq!(overlay.len(), 601, "one marker per frame");
    assert!(
        matches!(
            overlay[0],
            rf_plugin_sdk::sandbox::OverlayCmd::Rect { x: 316, .. }
        ),
        "the marker must sit at the published player_x (320) less half its width: {:?}",
        overlay[0]
    );
    assert!(
        host.log.lines().iter().any(|l| l.contains("player_x 320")),
        "the example's own print() branch must have been reached, with the PUBLISHED value — \
         otherwise this test only proves the file parses, not that it reads: {:?}",
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
