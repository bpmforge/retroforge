//! **The menu and status bars get out of the way in play** (ticket W20-11;
//! `docs/design/UX_WAVE_20.md` §5).
//!
//! Hidden only while a game is running with nothing else open and the
//! pointer has been still; back on pointer movement near the top or bottom
//! edge, on pause, and with any menu open. Never before a real pointer has
//! moved, which is what keeps every other headless test's bars in place.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn pointer(harness: &mut Harness<'_, RetroForgeApp>, x: f32, y: f32) {
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(x, y)));
    harness.run_steps(1);
}

/// Advance egui's clock (each harness step adds a step's dt) by plenty.
fn wait(harness: &mut Harness<'_, RetroForgeApp>) {
    for _ in 0..60 {
        harness.step();
    }
}

#[test]
fn bars_hide_in_play_and_return_on_edge_pause_and_menu() {
    let dir = std::env::temp_dir().join(format!("retroforge_chrome_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().launch_rom(&fixture);
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 30 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }

    // No pointer has moved: never hides (the guard for every other test).
    wait(&mut harness);
    assert!(
        harness.state().chrome_visible_for_test(),
        "unarmed: bars stay"
    );

    // A pointer moves in the middle, then holds still: the bars hide.
    let (cx, cy) = (WINDOW_SIZE[0] / 2.0, WINDOW_SIZE[1] / 2.0);
    pointer(&mut harness, cx, cy);
    assert!(
        harness.state().chrome_visible_for_test(),
        "just moved: still showing"
    );
    wait(&mut harness);
    assert!(
        !harness.state().chrome_visible_for_test(),
        "still pointer in play: hidden"
    );

    // Pointer to the top edge: back.
    pointer(&mut harness, cx, 8.0);
    assert!(
        harness.state().chrome_visible_for_test(),
        "near the top edge: shown"
    );

    // Hide again, then pause: back, and they stay while paused.
    pointer(&mut harness, cx, cy);
    wait(&mut harness);
    assert!(!harness.state().chrome_visible_for_test());
    harness.state_mut().pause_for_test();
    wait(&mut harness);
    assert!(harness.state().chrome_visible_for_test(), "paused: shown");

    // Running again, hidden again, then the Quick Menu: still hidden —
    // since W21-02 the menu covers the whole window and carries the
    // honesty badge itself.
    harness.state_mut().resume_for_test();
    pointer(&mut harness, cx, cy + 1.0);
    wait(&mut harness);
    assert!(!harness.state().chrome_visible_for_test());
    harness.key_press(egui::Key::Escape);
    harness.run_steps(1);
    assert!(
        !harness.state().chrome_visible_for_test(),
        "menu open: the menu owns the window (W21-02)"
    );
}
