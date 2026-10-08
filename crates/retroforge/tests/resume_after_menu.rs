//! **A paused game can always be resumed** (ticket W22-08). Brad: "when i
//! go to enhancement and pause to get that screen I cant unpause and get
//! back to playing". Leaving the Quick Menu for a window it opened kept
//! the game paused and forgot who paused it, so Esc never resumed it; and
//! Space, documented as pause/resume, did nothing.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn advances(h: &mut Harness<'_, RetroForgeApp>) -> bool {
    let f0 = h.state().frame_count_for_test();
    let start = Instant::now();
    while h.state().frame_count_for_test() < f0 + 10 && start.elapsed() < Duration::from_secs(10) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    h.state().frame_count_for_test() >= f0 + 10
}

#[test]
fn leaving_through_game_settings_then_esc_resumes_and_space_toggles() {
    let dir = std::env::temp_dir().join(format!("retroforge_resume_{}", std::process::id()));
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
    assert!(advances(&mut h), "self-test: the game runs");

    // Brad's path: Esc › Enhancements › Game settings…, close it.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.get_by_label(&Section::Enhancements.rail_text()).click();
    h.run_steps(3);
    h.get_by_label("Game settings\u{2026}").click();
    h.run_steps(3);
    assert_eq!(
        h.state().running_and_menu_for_test(),
        (false, false),
        "still paused behind the window"
    );
    h.state_mut().close_game_settings_for_test();
    h.run_steps(2);

    // Esc opens the menu, Esc closes it: the game must run again.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(
        h.state().running_and_menu_for_test(),
        (true, false),
        "closing the menu resumed it"
    );
    assert!(advances(&mut h), "and frames advance");

    // Space pauses and resumes in play.
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(
        h.state().running_and_menu_for_test(),
        (false, false),
        "Space paused"
    );
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(
        h.state().running_and_menu_for_test(),
        (true, false),
        "Space resumed"
    );
    assert!(advances(&mut h));
    let _ = std::fs::remove_dir_all(&dir);
}
