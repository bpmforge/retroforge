//! **The library's Continue hero, shelves and enhancement chips** (ticket
//! W20-20; `docs/design/UX_WAVE_20.md` §6).
//!
//! Play a game and save, go back to the library: it leads with "Continue"
//! for that game, offers Resume from the save, and Resume starts the game
//! AND loads the state. In the grid, the game's card carries the chips its
//! profile actually supports (RF-Scroller: Full level, 3D walls).

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::library::LibraryView;

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
fn continue_resumes_the_last_game_and_cards_show_honest_chips() {
    let dir = std::env::temp_dir().join(format!("retroforge_continue_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let games = dir.join("games");
    std::fs::create_dir_all(&games).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let rom = games.join("RF Scroller.nes");
    std::fs::copy(&fixture, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 200.0, WINDOW_SIZE[1] + 200.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();

    // Play it and save.
    harness.state_mut().launch_rom(&rom);
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 60 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    harness.key_press(egui::Key::F5);
    harness.run_steps(2);
    std::thread::sleep(Duration::from_millis(300));
    harness.state_mut().close_rom_for_test();
    harness
        .state_mut()
        .set_library_roots_for_test(vec![games.clone()]);
    harness
        .state_mut()
        .set_library_view_for_test(LibraryView::Grid);
    harness.run_steps(3);

    let all = texts(&harness);
    assert!(
        all.iter().any(|t| t == "CONTINUE PLAYING"),
        "the hero leads: {all:?}"
    );
    assert!(
        all.iter().any(|t| t == "Full level"),
        "the card's chip: {all:?}"
    );
    assert!(
        all.iter().any(|t| t == "3D walls"),
        "the card's chip: {all:?}"
    );
    assert!(
        !all.iter().any(|t| t == "Loading skip"),
        "no chip for what the fixture cannot do"
    );

    harness.get_by_label("Resume (Slot 1)").click();
    harness.run_steps(2);
    assert!(
        harness.state().running_and_menu_for_test().0,
        "Resume plays the game"
    );
    assert!(
        harness
            .state()
            .osd_texts_for_test()
            .iter()
            .any(|t| t == "Loaded Slot 1"),
        "and loads the save: {:?}",
        harness.state().osd_texts_for_test()
    );
}
