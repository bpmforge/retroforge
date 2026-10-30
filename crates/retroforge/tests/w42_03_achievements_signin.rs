//! **W42-03: Achievements you can turn on** — the Achievements page carries the RetroAchievements
//! switch and sign-in form itself, and Settings keeps them on the Online tab (not Paths).

use std::path::Path;

use eframe::egui;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";
const SWITCH: &str = "RetroAchievements (needs an account)";

#[test]
fn the_achievements_page_turns_it_on_and_settings_has_an_online_tab() {
    let dir = std::env::temp_dir().join(format!("retroforge_w42_03_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 1100.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut()
        .launch_rom(&Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE));
    h.run_steps(5);
    h.state_mut()
        .open_quick_menu_for_test(Section::Achievements);
    h.run_steps(3);
    assert!(h.query_by_label(SWITCH).is_some(), "switch on the page");
    assert!(h.query_by_label("Sign in").is_none());

    h.get_by_label(SWITCH).click();
    h.run_steps(3);
    assert!(h.query_by_label("User").is_some());
    assert!(h.query_by_label("Password").is_some());
    assert!(h.query_by_label("Sign in").is_some());

    // Close the menu (the page would still show the switch under the sheet), then Settings:
    // the Online tab has it, Paths does not.
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    h.state_mut().show_settings_for_test(true);
    h.run_steps(3);
    h.get_by_label("Paths").click();
    h.run_steps(3);
    assert!(h.query_by_label(SWITCH).is_none(), "not on Paths");
    h.get_by_label("Online").click();
    h.run_steps(3);
    assert!(h.query_by_label(SWITCH).is_some(), "on Online");
    assert!(h
        .query_by_label("Fetch box art from the internet")
        .is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
