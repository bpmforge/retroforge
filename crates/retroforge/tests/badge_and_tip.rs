//! **The badge explains itself; the tip shows once** (ticket W22-06;
//! `docs/design/UX_WAVE_22.md`).

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_to(h: &mut Harness<'_, RetroForgeApp>, frames: u64) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < frames && start.elapsed() < Duration::from_secs(30) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn the_tip_shows_once_and_the_badge_opens_enhancements() {
    let dir = std::env::temp_dir().join(format!("retroforge_badge_tip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let rom = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().launch_rom(&rom);
    run_to(&mut h, 40);
    h.run_steps(2);
    assert!(
        h.state().enhance_tip_showing_for_test(),
        "the tip shows the first time"
    );
    assert!(h.query_by_label("This game can be enhanced").is_some());
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    assert!(
        !h.state().enhance_tip_showing_for_test(),
        "any key dismisses it"
    );

    // Same game again: not again.
    h.state_mut().close_rom_for_test();
    h.run_steps(2);
    h.state_mut().launch_rom(&rom);
    let start = h.state().frame_count_for_test();
    run_to(&mut h, start + 60);
    h.run_steps(2);
    assert!(
        !h.state().enhance_tip_showing_for_test(),
        "shown once per game"
    );

    // The badge: a click opens Quick Menu › Enhancements.
    h.get_by_label("NES \u{b7} Original").click();
    h.run_steps(3);
    assert!(h.state().running_and_menu_for_test().1, "the menu opened");
    assert!(
        h.query_by_role_and_label(egui::accesskit::Role::RadioButton, "Original")
            .is_some(),
        "on the Enhancements section (its mode picker is showing)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
