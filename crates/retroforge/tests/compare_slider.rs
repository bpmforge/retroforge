//! **Drag to compare** (ticket W22-04; `docs/design/UX_WAVE_22.md`).
//! Quick Menu › Enhancements shows the paused frame as one picture,
//! original left of a line and what you see right of it; a click moves
//! the line there and the arrow keys nudge it.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::game_settings::Mode;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn the_compare_line_follows_clicks_and_arrow_keys() {
    let dir = std::env::temp_dir().join(format!("retroforge_compare_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(980.0, 1000.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut()
        .launch_rom(&Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE));
    // An enhancement on while frames arrive, so there is a pair to compare.
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_sprite_overlay_for_test(true);
    let start = Instant::now();
    while h.state().frame_count_for_test() < 40 && start.elapsed() < Duration::from_secs(30) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.get_by_label(&Section::Enhancements.rail_text()).click();
    h.run_steps(3);
    h.get_by_label("Compare original and enhanced").click();
    h.run_steps(2);
    let at_click = h.state().compare_cut_for_test();
    assert!(
        (at_click - 0.5).abs() < 0.05,
        "a click in the middle: {at_click}"
    );
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    assert!(
        h.state().compare_cut_for_test() > at_click + 0.03,
        "the arrow key moved the line: {}",
        h.state().compare_cut_for_test()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
