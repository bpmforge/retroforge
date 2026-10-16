//! **"Sharper Super FX 3D" never applies where it must not** (ticket
//! W31-03): Accuracy Mode (law 6), a game whose profile has no
//! `[superfx_polygons]`, or the toggle off. Wiring only: pixel content is
//! covered by `rf-renderer`'s compositor tests and the ROM-gated proof in
//! `rf-harness`.

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;

#[test]
fn superfx_hd_is_effective_only_in_enhanced_with_a_profile_row_and_the_toggle() {
    let dir = std::env::temp_dir().join(format!("retroforge_superfx_hd_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run_steps(2);
    let app = harness.state_mut();
    for (on, mode, polygons, want) in [
        (true, Mode::Accuracy, true, false),
        (true, Mode::Enhanced, false, false),
        (false, Mode::Enhanced, true, false),
        (true, Mode::Enhanced, true, true),
    ] {
        app.set_superfx_hd_for_test(on, 3, mode, polygons);
        assert_eq!(
            app.superfx_hd_effective_for_test(),
            want,
            "on={on} mode={mode:?} polygons={polygons}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
