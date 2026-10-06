//! **Opening the in-game menu pauses the game** (ticket W20-03;
//! `docs/design/UX_WAVE_20.md` §4).
//!
//! Until W20-03, Esc toggled the menu window and the game kept running
//! behind it. These drive the real `RetroForgeApp` with the real Esc key.

use std::path::Path;
use std::time::Duration;

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn press(harness: &mut Harness<'_, RetroForgeApp>, key: egui::Key) {
    for pressed in [true, false] {
        harness.input_mut().events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.run_steps(1);
    }
}

fn frames_advance(harness: &mut Harness<'_, RetroForgeApp>) -> bool {
    let before = harness.state().frame_count_for_test();
    for _ in 0..40 {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.state().frame_count_for_test() > before
}

#[test]
fn esc_pauses_a_running_game_and_leaves_a_paused_one_paused() {
    let dir = std::env::temp_dir().join(format!("retroforge_menu_pause_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&fixture);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    assert!(
        frames_advance(&mut harness),
        "the game must be running first"
    );

    // Running → Esc opens the menu AND pauses.
    press(&mut harness, egui::Key::Escape);
    assert_eq!(harness.state().running_and_menu_for_test(), (false, true));
    // Let any frame already in flight land, then prove nothing more does.
    for _ in 0..10 {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !frames_advance(&mut harness),
        "the core kept running behind the menu"
    );

    // Esc again closes it and resumes, because the menu paused it.
    press(&mut harness, egui::Key::Escape);
    assert_eq!(harness.state().running_and_menu_for_test(), (true, false));
    assert!(
        frames_advance(&mut harness),
        "closing the menu did not resume"
    );

    // A game the PLAYER paused stays paused across open/close.
    harness.state_mut().pause_for_test();
    press(&mut harness, egui::Key::Escape);
    assert_eq!(harness.state().running_and_menu_for_test(), (false, true));
    press(&mut harness, egui::Key::Escape);
    assert_eq!(harness.state().running_and_menu_for_test(), (false, false));
}
