//! **SNES games get the controller** (ticket W23-01). Until W23-01 a
//! running SNES game received no input at all, and the keyboard map knew
//! only the NES's eight buttons. Holding A (the default for SNES Y) must
//! put Y — and only Y — in the word handed to the SNES.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;

const SNES: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";

#[test]
fn the_keyboard_drives_snes_y() {
    let dir = std::env::temp_dir().join(format!("retroforge_snes_controls_{}", std::process::id()));
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
        .launch_rom(&Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES));
    let start = Instant::now();
    while h.state().frame_count_for_test() < 10 && start.elapsed() < Duration::from_secs(20) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    h.input_mut().events.push(egui::Event::Key {
        key: egui::Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    h.run_steps(2);
    assert_eq!(
        h.state().snes_input_word_for_test(),
        1 << rf_input::SnesButton::Y.bit(),
        "A held: the SNES gets Y"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
