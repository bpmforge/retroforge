//! **A Lua overlay runs in the app** (ticket W11-04; FR-PLUG-001).
//!
//! `rf_plugin_sdk` shipped a sandbox, a capability model, a budget and a
//! write ledger; `plugins/examples/player-overlay` shipped a working
//! script; `lua_overlay_demo.rs` proved the marker follows the player.
//! All true, and **no path existed to load a script in the application**
//! — there was no menu entry, no host, no memory window, and
//! `crate::script_panel` was called from nowhere at all.
//!
//! The demo drives `ScriptHost` directly. This drives `RetroForgeApp`,
//! because a library-level proof is what let this look finished.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";
const PLUGIN: &str = "../../plugins/examples/player-overlay";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn marker_x(cmds: &[rf_plugin_sdk::sandbox::OverlayCmd]) -> Option<i32> {
    cmds.iter()
        .map(|c| match c {
            rf_plugin_sdk::sandbox::OverlayCmd::Rect { x, .. } => *x,
            rf_plugin_sdk::sandbox::OverlayCmd::Line { x0, .. } => *x0,
        })
        .next()
}

#[test]
fn the_shipped_overlay_script_draws_a_marker_that_follows_the_player() {
    let dir = std::env::temp_dir().join(format!("retroforge_script_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
    }

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join(PLUGIN);
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());
    assert!(
        plugin.exists(),
        "example plugin missing: {}",
        plugin.display()
    );
    let rom: PathBuf = dir.join("rf-scroller.nes");
    std::fs::copy(&fixture, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();

    // Loading needs the profile's labels, which needs the profile to have
    // matched — so this also exercises W11-02's profiles-root fix.
    harness.state_mut().load_script_for_test(&plugin);
    let status = harness
        .state()
        .script_status_for_test()
        .expect("loading a script must report SOMETHING, success or failure");
    assert!(
        status.starts_with("Loaded"),
        "the shipped example plugin failed to load: {status}"
    );

    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    run_frames(&mut harness, 240, Duration::from_secs(20));

    let early = harness.state().script_overlay_for_test();
    assert!(
        !early.is_empty(),
        "the script ran but asked for nothing to be drawn. Either the memory window is not \
         reaching it, or on_frame is never called."
    );
    let early_x = marker_x(&early).expect("the overlay must contain a positioned command");

    run_frames(&mut harness, 900, Duration::from_secs(30));
    let late_x = marker_x(&harness.state().script_overlay_for_test())
        .expect("the overlay must still be redrawn later in the run");

    // **The discriminating assertion**, and the MVP checklist item:
    // "a Lua script draws a live player-position overlay using
    // profile-published labels". A script fed a constant, or a window of
    // zeroes, or the wrong base address would draw a marker that never
    // moves — and would pass every other check here.
    assert_ne!(
        early_x, late_x,
        "the marker did not move between frame ~240 and ~900 while holding Right. The script \
         is not seeing the running game."
    );

    // Unloading stops the core peeking for a script nobody is running.
    harness
        .state_mut()
        .load_script_for_test(Path::new("/nonexistent-plugin-dir"));
    let refused = harness.state().script_status_for_test().unwrap_or_default();
    assert!(
        refused.contains("needs both"),
        "a directory with no manifest must SAY why it was refused, not fail silently: {refused}"
    );
}
