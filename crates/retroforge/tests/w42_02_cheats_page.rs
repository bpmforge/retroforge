//! **W42-02: Cheats is a Quick Menu page.** Open the Quick Menu on Cheats, paste a valid code and
//! a bad line, add: the valid one is listed and the bad one reads "Line 2 ...". The research
//! Game settings window no longer carries a Cheats panel.
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
fn cheats_page_adds_codes_and_game_settings_has_no_cheats_panel() {
    let dir = std::env::temp_dir().join(format!("retroforge_w42_02_page_{}", std::process::id()));
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

    h.state_mut().open_quick_menu_for_test(Section::Cheats);
    h.run_steps(4);
    assert!(
        h.query_by_label("Cheat codes").is_some(),
        "the Cheats page has the paste box"
    );
    h.get_by_label("Cheat codes").click();
    h.run_steps(2);
    h.get_by_label("Cheat codes")
        .type_text("SXIOPO\nnot a code");
    h.run_steps(2);
    h.get_by_label("Add codes").click();
    h.run_steps(3);

    let cheats = &h.state().game_settings_for_test().cheats;
    assert_eq!(cheats.len(), 1, "only the valid code is stored: {cheats:?}");
    assert_eq!(cheats[0].code, "SXIOPO");
    let (_, errors) = h.state().cheat_box_for_test();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("Line 2 isn't a code"), "{errors:?}");
    assert!(
        h.query_by_label_contains("Line 2").is_some(),
        "the bad line is explained on the page"
    );

    // Esc closes the menu; Game settings then keeps no Cheats panel.
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    assert!(!h.state().quick_menu_open_for_test(), "Esc closed the menu");
    h.state_mut().open_game_settings_for_test();
    h.run_steps(4);
    assert!(
        h.query_by_label("Cheat codes").is_none(),
        "Game settings has no Cheat codes box"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
