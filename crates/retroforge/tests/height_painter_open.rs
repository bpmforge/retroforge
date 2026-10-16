//! Ticket W30-06: the height painter refuses to open without a running SNES
//! game. Its own binary because `RETROFORGE_CONFIG_DIR` is process-global:
//! sharing a binary with `height_painter_app.rs`'s test raced that variable,
//! and this test then opened the REAL user config (a large library scan
//! kept the UI repainting past `Harness::run`'s step limit).

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

#[test]
fn the_height_painter_needs_a_running_snes_game() {
    let dir = std::env::temp_dir().join(format!("retroforge_painter_open_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch config dir");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_height_painter_for_test();
    assert!(!harness.state().height_painter_open_for_test());
}
