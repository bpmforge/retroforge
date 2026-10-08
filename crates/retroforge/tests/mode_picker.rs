//! **The mode picker** (ticket W22-01; `docs/design/UX_WAVE_22.md`).
//! Quick Menu › Enhancements opens with Original / Enhanced / Game-Aware
//! as one-click choices; picking one changes the open game's mode, and the
//! badge says so in the player's words.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn picking_a_mode_card_changes_the_games_mode() {
    let dir = std::env::temp_dir().join(format!("retroforge_mode_picker_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(980.0, 860.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut()
        .launch_rom(&Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE));
    let start = Instant::now();
    while h.state().frame_count_for_test() < 20 && start.elapsed() < Duration::from_secs(20) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    assert_eq!(
        h.state().status_badge(),
        "NES · Original",
        "law 6: a fresh game is Original"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.get_by_label(&Section::Enhancements.rail_text()).click();
    h.run_steps(2);
    for name in ["Original", "Enhanced", "Game-Aware"] {
        assert!(
            h.query_by_role_and_label(egui::accesskit::Role::RadioButton, name)
                .is_some(),
            "the picker offers {name}"
        );
    }
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Enhanced")
        .click();
    h.run_steps(2);
    assert!(
        h.state().status_badge().starts_with("NES · Enhanced"),
        "the click changed the mode: {}",
        h.state().status_badge()
    );
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Original")
        .click();
    h.run_steps(2);
    assert_eq!(h.state().status_badge(), "NES · Original");
    let _ = std::fs::remove_dir_all(&dir);
}
