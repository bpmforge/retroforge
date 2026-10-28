//! **Esc closes Settings and nothing else** (ticket W21-13). Both the
//! Settings sheet and the Quick Menu answered the same Esc press, so
//! closing Settings also opened the menu (and paused the game). The
//! same holds when Settings was opened from the Quick Menu, and when its
//! search box has focus (both checked 2026-10-27: not reproducible).

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn one_esc_closes_settings_and_leaves_the_menu_shut() {
    let dir = std::env::temp_dir().join(format!("retroforge_esc_settings_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(892.0, 902.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut()
        .launch_rom(&Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE));
    let start = Instant::now();
    while h.state().frame_count_for_test() < 20 && start.elapsed() < Duration::from_secs(20) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    h.state_mut().show_settings_for_test(true);
    h.run_steps(2);
    assert!(
        h.query_by_label("Close settings").is_some(),
        "self-test: Settings is open"
    );

    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(
        h.query_by_label("Close settings").is_none(),
        "Esc closed Settings"
    );
    assert_eq!(
        h.state().running_and_menu_for_test(),
        (true, false),
        "the same Esc must not open the Quick Menu or pause the game"
    );

    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(
        h.state().running_and_menu_for_test().1,
        "Esc with nothing open still opens the menu"
    );

    // Settings reached from the Quick Menu (rail › Settings › "Open
    // Settings…"): the menu hands over to the sheet, and the Esc that
    // closes the sheet does not bring the menu back.
    h.query_all_by_label_contains("Settings")
        .last()
        .expect("the Quick Menu's Settings section")
        .click();
    h.run_steps(2);
    h.get_by_label("Open Settings\u{2026}").click();
    h.run_steps(3);
    assert!(
        h.query_by_label("Close settings").is_some(),
        "self-test: Settings is open from the Quick Menu"
    );
    assert!(
        !h.state().running_and_menu_for_test().1,
        "self-test: the menu handed over"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(
        h.query_by_label("Close settings").is_none(),
        "Esc closed Settings"
    );
    assert!(
        !h.state().running_and_menu_for_test().1,
        "the Esc that closed Settings must not reopen the Quick Menu"
    );

    // Settings with its search box focused: the text field's own Esc
    // handling must not let the press through to the Quick Menu.
    h.state_mut().show_settings_for_test(true);
    h.run_steps(3);
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(
        h.query_by_label("Close settings").is_none(),
        "Esc closed Settings from the search box"
    );
    assert!(
        !h.state().running_and_menu_for_test().1,
        "the Esc from the search box must not open the Quick Menu"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
