//! **W41-06: with RetroAchievements off** the Achievements section says how to turn it on.

use std::path::Path;

use eframe::egui;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn off_shows_the_empty_state() {
    let dir = std::env::temp_dir().join(format!("retroforge_w41_06_off_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut()
        .launch_rom(&Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE));
    h.run_steps(5);
    h.state_mut()
        .open_quick_menu_for_test(Section::Achievements);
    h.run_steps(3);
    assert!(h
        .query_by_label("Turn on RetroAchievements in Settings")
        .is_some());
    assert!(h.query_by_label_contains("unlocked,").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
