//! **The play view uses the whole area between the bars** (ticket W21-09;
//! `docs/design/UX_WAVE_21.md` §8). The default panel margin cost a whole
//! integer step: Brad's 892-px-wide window drew NES at 2x (586 px) when 3x
//! (878 px) fits. Real app, real core, his window size.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn an_892_px_window_shows_nes_at_3x() {
    let dir = std::env::temp_dir().join(format!("retroforge_play_area_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(892.0, 902.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().launch_rom(&fixture);
    let start = Instant::now();
    while harness.state().play_rect_for_test().is_none()
        && start.elapsed() < Duration::from_secs(20)
    {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    let (rect, _) = harness
        .state()
        .play_rect_for_test()
        .expect("a frame was drawn");
    assert!(
        (rect.height() - 720.0).abs() < 0.5,
        "Integer scale in an 892x902 window must be 3x (720 px tall): {rect:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
