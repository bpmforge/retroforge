//! **Settings › Video's scale mode reaches the picture** (ticket W20-01;
//! `docs/design/UX_WAVE_20.md` §4).
//!
//! Until W20-01 the radio was saved and read by nothing: every frame was
//! drawn with `Image::shrink_to_fit` whatever it said. The geometry itself
//! is unit-tested in `crate::play_view`; this drives the real
//! `RetroForgeApp` with a real core so the wiring is what is proved — a
//! mode change must move the rect the picture is actually drawn in.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::play_view::{integer_multiple, DisplayGrid};
use retroforge::settings::{PixelAspect, ScaleMode};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn integer_fit_and_stretch_each_place_the_picture_differently() {
    let dir = std::env::temp_dir().join(format!("retroforge_scale_mode_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    // A window whose central area is between 2x and 3x of 240 lines, so
    // Integer and Fit must disagree.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 700.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&fixture);
    harness.run_steps(2);
    // A ROM opens paused (the debugger's convention); run it.
    harness.state_mut().resume_for_test();
    let start = Instant::now();
    while harness.state().play_rect_for_test().is_none()
        && start.elapsed() < Duration::from_secs(20)
    {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }

    let mut rect_for = |mode, aspect| {
        let v = harness.state_mut().video_settings_mut_for_test();
        v.scale_mode = mode;
        v.pixel_aspect = aspect;
        harness.run_steps(2);
        harness
            .state()
            .play_rect_for_test()
            .expect("a frame was drawn")
    };

    let (integer, (w, h)) = rect_for(ScaleMode::Integer, PixelAspect::Tv);
    #[allow(clippy::cast_precision_loss)]
    let grid = DisplayGrid::for_frame(w as f32, h as f32);
    let k = integer_multiple(integer, grid).expect("Integer mode draws a whole vertical multiple");
    assert!(k >= 2, "{integer:?}");
    assert!(
        (integer.width() / integer.height() - grid.columns * 8.0 / 7.0 / grid.lines).abs() < 1e-3
    );

    let (fit, _) = rect_for(ScaleMode::Fit, PixelAspect::Tv);
    assert!(
        fit.height() > integer.height(),
        "Fit fills more than Integer: {fit:?} vs {integer:?}"
    );
    assert!((fit.width() / fit.height() - integer.width() / integer.height()).abs() < 1e-3);

    let (stretch, _) = rect_for(ScaleMode::Stretch, PixelAspect::Tv);
    assert!(stretch.width() > fit.width() || stretch.height() > fit.height());

    let (square, _) = rect_for(ScaleMode::Integer, PixelAspect::Square);
    assert!((square.width() / square.height() - grid.columns / grid.lines).abs() < 1e-3);
}
