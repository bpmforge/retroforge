//! Ticket W15-06: the App hotkeys (`docs/design/UX_WAVE_15.md` §6) —
//! save state, load state, fast-forward, screenshot, hold-to-peek — as a
//! namespace apart from the per-port game bindings.
//!
//! `crates/retroforge/src/app_bindings.rs` carries the unit-level proof
//! that the two namespaces cannot collide (both directions) and that the
//! defaults round-trip through `crate::bindings_store`. This file is the
//! one scenario that needs a real running app: pressing **F12** must
//! produce a screenshot toast and the PNG pair on disk, end to end
//! through `RetroForgeApp::poll_app_hotkeys` and `write_screenshots`.
//!
//! **One test function.** `RETROFORGE_CONFIG_DIR` is process-global and
//! `RetroForgeApp::new` reads it, so a second test in this binary setting
//! it while this one reads it would race — every other file that opens a
//! real app reaches the same conclusion (`tests/library_selection.rs`,
//! `tests/toasts_and_modals.rs`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

/// Same tiny running NROM `tests/toasts_and_modals.rs` and
/// `tests/library_selection.rs` both build: enough CPU work that the
/// core thread actually reports frames, no dependency on a fetched ROM.
fn write_fixture_rom(dir: &Path, name: &str) -> PathBuf {
    let mut data = vec![0u8; 16 + 0x4000 + 0x2000];
    data[0..4].copy_from_slice(b"NES\x1a");
    data[4] = 1;
    data[5] = 1;
    let prg = &mut data[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x1E, 0x8D, 0x01, 0x20, // LDA #$1E ; STA $2001 (rendering on)
        0xEE, 0x00, 0x00, // INC $0000
        0x4C, 0x05, 0x80, // JMP $8005
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    let path = dir.join(name);
    std::fs::write(&path, &data).expect("write fixture rom");
    path
}

/// Repaint (yielding real wall time to the core thread each pass) until
/// the core has produced `target` frames or `deadline` elapses — the same
/// shape `tests/capture_tour.rs::run_emulated_frames` uses, for the same
/// reason: frame delivery crosses a real channel from a real thread, so
/// a fixed `run_steps` count would be a flake waiting to happen on a
/// loaded CI box.
fn run_until_frame(harness: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while harness.state().frame_count_for_test() < target && start.elapsed() < deadline {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn pressing_f12_writes_a_screenshot_and_raises_a_toast() {
    let dir = std::env::temp_dir().join(format!("retroforge_app_hotkeys_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    // SAFETY: first statement of the only test in this binary, before any
    // app, core thread or harness exists — no concurrent reader.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let rom = write_fixture_rom(&dir, "fixture.nes");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();

    // F12 is the App default for Screenshot (`crate::app_bindings::
    // AppBindings::default`) — confirmed here rather than assumed, since
    // a wrong default would make this whole test vacuous.
    assert_eq!(
        harness
            .state()
            .app_bindings_key_for_test(retroforge::app_bindings::AppAction::Screenshot),
        Some(egui::Key::F12)
    );

    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    run_until_frame(&mut harness, 3, Duration::from_secs(10));
    assert!(
        harness.state().frame_count_for_test() >= 3,
        "the core must have produced frames before F12 has anything to capture"
    );

    assert!(
        !harness.state().has_visible_toast_for_test(),
        "no toast should be up yet — nothing has happened this test"
    );

    // The core thread runs on its own real clock, so `screenshot_pending`
    // can already have been consumed by the time this call returns — the
    // request and the completed capture are not observable as two
    // separate steps from here. Give the core a little more time either
    // way, then check the end state: a toast, and the pair on disk.
    harness.key_press(egui::Key::F12);
    let target = harness.state().frame_count_for_test() + 2;
    run_until_frame(&mut harness, target, Duration::from_secs(10));

    assert!(
        harness.state().has_visible_toast_for_test(),
        "a completed screenshot must raise a toast"
    );

    let shots_dir = dir.join("retroforge").join("screenshots");
    let written: Vec<_> = std::fs::read_dir(&shots_dir)
        .unwrap_or_else(|e| panic!("screenshots dir {} should exist: {e}", shots_dir.display()))
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        written.len(),
        2,
        "the original/enhanced pair (FR-FE-005) must both land under the screenshots directory, \
         not the process's current directory"
    );

    // ---- Ticket W20-04: F11 toggles fullscreen ----------------------
    // (In this test rather than its own: one test per binary, for the
    // `set_var` reason above.)
    assert_eq!(
        harness
            .state()
            .app_bindings_key_for_test(retroforge::app_bindings::AppAction::Fullscreen),
        Some(egui::Key::F11)
    );
    assert_eq!(harness.state().last_fullscreen_request_for_test(), None);
    harness.key_press(egui::Key::F11);
    harness.run_steps(1);
    assert_eq!(
        harness.state().last_fullscreen_request_for_test(),
        Some(true),
        "F11 must ask the window to go fullscreen"
    );
}
