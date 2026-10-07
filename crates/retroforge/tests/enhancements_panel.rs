//! **The player's Enhancements panel** (ticket W20-18;
//! `docs/design/UX_WAVE_20.md` §6).
//!
//! In the Quick Menu: every feature as a card with a plain sentence, a
//! plain requirement, a toggle that works exactly as the Enhance
//! workspace's does (one shared path), and the trust ladder as words.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn texts(harness: &Harness<'_, RetroForgeApp>) -> Vec<String> {
    harness
        .root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            a.label().or_else(|| a.value())
        })
        .collect()
}

#[test]
fn cards_explain_each_feature_and_a_toggle_reaches_the_badge() {
    let dir = std::env::temp_dir().join(format!("retroforge_enh_panel_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 200.0, WINDOW_SIZE[1] + 300.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().launch_rom(&fixture);
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 30 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    harness
        .get_by_label(&Section::Enhancements.rail_text())
        .click();
    harness.run_steps(3);

    let all = texts(&harness);
    for id in ["sprite_overlay", "deflicker", "full_level_view"] {
        let sentence = retroforge::enhance_panel::describe(id);
        assert!(all.iter().any(|t| t == sentence), "card for {id}: {all:?}");
    }
    assert!(
        all.iter().any(|t| t == "Needs Game-Aware mode."),
        "plain requirement: {all:?}"
    );
    assert!(all.iter().any(|t| t == "Learning"), "trust ladder in words");
    assert!(
        !all.iter().any(|t| t.contains("heuristic-gated")),
        "no research jargon"
    );

    // Turn on the sprite-limit bypass from the panel: the badge counts it.
    assert!(!harness.state().status_badge().contains('\u{e2de}'));
    // W21-05: the card's switch, not its title label.
    harness
        .get_by_role_and_label(egui::accesskit::Role::CheckBox, "Sprite-limit bypass")
        .click();
    harness.run_steps(2);
    assert!(
        harness.state().status_badge().contains('\u{e2de}'),
        "the badge must count a feature turned on from the panel: {}",
        harness.state().status_badge()
    );
}
