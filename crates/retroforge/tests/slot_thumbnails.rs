//! **The States window draws each slot's screenshot** (ticket W20-06;
//! `docs/design/UX_WAVE_20.md` §4).
//!
//! Until W20-06 a saved slot showed the word "thumbnail" next to a
//! selectable label; the PNG sidecar `crate::state_slots` writes was never
//! drawn. This saves a real state through the F5 hotkey on the RF-Scroller
//! fixture and asserts the window exposes an image for that slot — and
//! none for an empty one.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn a_saved_slot_shows_its_screenshot_and_an_empty_slot_does_not() {
    let dir = std::env::temp_dir().join(format!("retroforge_slot_thumbs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());
    let rom = dir.join("rf-scroller.nes");
    std::fs::copy(&fixture, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 300.0, WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().launch_rom(&rom);
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 90 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }

    // F5 = Save state to the active slot (Slot 1 by default).
    harness.key_press(egui::Key::F5);
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(10));
        harness.state_mut().open_states_modal();
        harness.run_steps(2);
        if harness.query_by_label("Slot 1 screenshot").is_some()
            || start.elapsed() > Duration::from_secs(10)
        {
            break;
        }
    }
    assert!(
        harness.query_by_label("Slot 1 screenshot").is_some(),
        "a saved slot must show its screenshot as an image"
    );
    assert!(
        harness.query_by_label("Slot 2 screenshot").is_none(),
        "an empty slot has no screenshot to show"
    );
    assert!(
        harness.query_by_label_contains("thumbnail").is_none(),
        "no placeholder words"
    );
}
