//! **The window fits the game** (ticket W21-12; `docs/design/UX_WAVE_21.md`
//! §8). Brad: "can we have the render window ... be exact size of the
//! render so that there is no black boarders?" When a game's first frame
//! is drawn in a window, the app asks for an inner size that leaves no
//! space around the picture — once per game. Real app, real cores.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;

fn fitted_for(fixture: &str, fit: bool) -> (Option<egui::Vec2>, egui::Rect) {
    let mut h = Harness::builder()
        .with_size(egui::vec2(892.0, 902.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut()
        .launch_rom(&Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture));
    // After launch: opening a game loads its video settings.
    h.state_mut().video_settings_mut_for_test().fit_window = fit;
    let start = Instant::now();
    while h.state().frame_count_for_test() < 30 && start.elapsed() < Duration::from_secs(20) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    h.run_steps(3);
    let (rect, _) = h.state().play_rect_for_test().expect("a frame was drawn");
    (h.state().last_window_fit_for_test(), rect)
}

#[test]
fn nes_and_snes_windows_are_sized_to_the_picture() {
    let dir = std::env::temp_dir().join(format!("retroforge_window_fit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let (nes, nes_rect) = fitted_for("../../fixtures/nes/rf-scroller/build/rf-scroller.nes", true);
    let (snes, snes_rect) = fitted_for(
        "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc",
        true,
    );
    let nes = nes.expect("an NES window is fitted");
    let snes = snes.expect("an SNES window is fitted");
    // Same width (both 256 wide at 3x), and the SNES window is 16 lines x3
    // shorter — the picture's own shape, not one box for both.
    assert!(
        (nes.x - nes_rect.width().ceil()).abs() <= 1.0,
        "{nes:?} vs {nes_rect:?}"
    );
    assert!((nes.x - snes.x).abs() <= 1.0);
    assert!(
        ((nes.y - snes.y) - (nes_rect.height() - snes_rect.height())).abs() <= 1.0,
        "the height difference is the pictures' difference: {nes:?} {snes:?}"
    );
    let (off, _) = fitted_for(
        "../../fixtures/nes/rf-scroller/build/rf-scroller.nes",
        false,
    );
    assert!(
        off.is_none(),
        "with the switch off the window is left alone"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
