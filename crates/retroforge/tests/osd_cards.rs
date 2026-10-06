//! **On-screen display cards** (ticket W20-12; `docs/design/UX_WAVE_20.md`
//! §5).
//!
//! Save, load, screenshot and fast-forward are acknowledged by a short card
//! over the game picture rather than by a status-bar string (which, for a
//! save, used to read "Saving Slot 1…" forever). Fast-forward, refreshed
//! every frame it is held, must be ONE card, not a stack.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn key(harness: &mut Harness<'_, RetroForgeApp>, key: egui::Key, pressed: bool) {
    harness.input_mut().events.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
}

#[test]
fn save_load_and_fast_forward_show_cards_that_expire() {
    let dir = std::env::temp_dir().join(format!("retroforge_osd_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let rom = dir.join("rf-scroller.nes");
    std::fs::copy(&fixture, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().launch_rom(&rom);
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 60 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }

    // Save (F5): a card, and no lingering "Saving…" status.
    harness.key_press(egui::Key::F5);
    harness.run_steps(1);
    assert_eq!(
        harness.state().osd_texts_for_test(),
        vec!["Saved to Slot 1"]
    );
    assert!(
        !harness.state().status().contains("Saving"),
        "{}",
        harness.state().status()
    );

    // It expires (Context::time advances per harness step; toast.rs doc).
    for _ in 0..200 {
        harness.step();
    }
    assert!(
        harness.state().osd_texts_for_test().is_empty(),
        "the card must expire"
    );

    // Load (F9) — once the save has landed on disk.
    std::thread::sleep(Duration::from_millis(300));
    harness.key_press(egui::Key::F9);
    harness.run_steps(1);
    assert_eq!(harness.state().osd_texts_for_test(), vec!["Loaded Slot 1"]);
    for _ in 0..200 {
        harness.step();
    }

    // Fast-forward held for many frames: exactly one card.
    key(&mut harness, egui::Key::Tab, true);
    for _ in 0..30 {
        harness.run_steps(1);
    }
    let ff: Vec<String> = harness.state().osd_texts_for_test();
    assert_eq!(ff.len(), 1, "one fast-forward card, not a stack: {ff:?}");
    assert!(ff[0].ends_with("Fast-forward"));
    key(&mut harness, egui::Key::Tab, false);
    for _ in 0..200 {
        harness.step();
    }
    assert!(
        harness.state().osd_texts_for_test().is_empty(),
        "it fades after release"
    );
}
