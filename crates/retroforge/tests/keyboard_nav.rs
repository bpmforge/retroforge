//! **The Quick Menu by keyboard** (ticket W23-03). Brad: "Keyboard
//! should have defaults set and I can navigate". Before W23-03 the arrow
//! keys did nothing in the menu (only Tab moved), and its hints named pad
//! buttons only.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn press(h: &mut Harness<'_, RetroForgeApp>, key: egui::Key) {
    h.key_press(key);
    h.run_steps(3);
}

#[test]
fn arrows_choose_enter_selects_and_hints_show_keys() {
    let dir = std::env::temp_dir().join(format!("retroforge_kbnav_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut()
        .launch_rom(&Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE));
    let start = Instant::now();
    while h.state().frame_count_for_test() < 40 && start.elapsed() < Duration::from_secs(20) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    press(&mut h, egui::Key::Escape);
    assert_eq!(h.state().quick_section_for_test(), Section::Resume);
    assert!(
        h.query_by_label("Choose").is_some(),
        "keyboard hints after keyboard use"
    );

    for _ in 0..4 {
        press(&mut h, egui::Key::ArrowDown);
    }
    assert_eq!(
        h.state().quick_section_for_test(),
        Section::Display,
        "Down x4"
    );
    for _ in 0..4 {
        press(&mut h, egui::Key::ArrowUp);
    }
    assert_eq!(h.state().quick_section_for_test(), Section::Resume);

    // Into the Resume section, Enter on its button: back to playing.
    press(&mut h, egui::Key::ArrowRight);
    press(&mut h, egui::Key::Enter);
    assert_eq!(
        h.state().running_and_menu_for_test(),
        (true, false),
        "Enter on Resume resumed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
