//! **W42-04: no duplicate windows.** File > Save states opens the Quick Menu on its Save page (no
//! "States" window exists any more), and the Controls entry point opens Settings on its Controls
//! tab. A pending key rebind swallows Esc instead of closing the sheet.
//!
//! One test function: `RETROFORGE_CONFIG_DIR` is process-global.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::quick_menu::Section;

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(60) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn save_states_and_controls_open_existing_surfaces() {
    let dir = std::env::temp_dir().join(format!("retroforge_w42_04_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let rom = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    let mut h = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run_steps(2);
    h.state_mut().launch_rom(&rom);
    run_frames(&mut h, 10);

    // File > Save states... (the public fn the menu item calls).
    h.state_mut().open_save_states();
    h.run_steps(4);
    assert!(h.state().quick_menu_open_for_test(), "Quick Menu is open");
    assert_eq!(h.state().quick_section_for_test(), Section::Save);
    assert!(
        h.query_all_by_label_contains("Save Slot 1").count() > 0,
        "the Save page lists the slots"
    );
    assert!(
        h.query_by_label("Save states").is_none(),
        "no separate States window"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(!h.state().quick_menu_open_for_test());

    // The Controls entry point: Settings on the Controls tab.
    h.state_mut().open_controls_settings();
    h.run_steps(4);
    assert!(h.state().settings_open_for_test());
    assert!(h.state().settings_on_controls_for_test());
    assert!(
        h.query_by_label("App shortcuts").is_some()
            || h.query_by_label_contains("Player 2").is_some(),
        "the tab carries the app shortcuts and player 2"
    );
    // A pending rebind swallows Esc rather than closing the sheet.
    h.state_mut().begin_rebind_for_test();
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(
        h.state().settings_open_for_test(),
        "Esc only cancelled the rebind"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(
        !h.state().settings_open_for_test(),
        "next Esc closes the sheet"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
