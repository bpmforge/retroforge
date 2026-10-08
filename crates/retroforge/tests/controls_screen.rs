//! **The Controls screen** (ticket W23-02). Brad: "what about settings
//! for controller vs keyboard. Keyboard should have defaults set". Each
//! console's pad is drawn with its bindings; clicking a button and then
//! pressing a key or pad button rebinds it; Restore defaults puts them
//! back.

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use rf_input::{Key, NesButton, PadButton, SnesButton};

fn press(h: &mut Harness<'_, RetroForgeApp>, key: egui::Key) {
    h.key_press(key);
    h.run_steps(2);
}

#[test]
fn keyboard_and_controller_rebinds_for_both_consoles() {
    let dir = std::env::temp_dir().join(format!("retroforge_controls_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().show_settings_for_test(true);
    h.run_steps(2);
    h.get_by_label("Controls").click();
    h.run_steps(2);

    // The keyboard defaults are on the drawn NES pad.
    assert!(h.query_by_label("A: X").is_some(), "NES A is X by default");
    assert!(h.query_by_label("Start: Enter").is_some());

    // NES: A → K.
    h.get_by_label("A: X").click();
    h.run_steps(2);
    press(&mut h, egui::Key::K);
    assert_eq!(h.state().nes_key_for_test(NesButton::A), Some(Key::K));

    // SNES keyboard: the defaults, then Y → Space.
    h.state_mut().set_controls_for_test(true, false);
    h.run_steps(2);
    assert!(h.query_by_label("Y: A").is_some(), "SNES Y is A by default");
    assert!(h.query_by_label("L: Q").is_some());
    h.get_by_label("Y: A").click();
    h.run_steps(2);
    press(&mut h, egui::Key::Space);
    assert_eq!(h.state().snes_key_for_test(SnesButton::Y), Some(Key::Space));

    // A key the app's shortcuts use is refused.
    h.get_by_label("X: S").click();
    h.run_steps(2);
    press(&mut h, egui::Key::Tab);
    assert_eq!(
        h.state().snes_key_for_test(SnesButton::X),
        Some(Key::S),
        "Tab is fast-forward"
    );

    // SNES controller: Y → North.
    h.state_mut().set_controls_for_test(true, true);
    h.run_steps(2);
    h.get_by_label("Y: West").click();
    h.run_steps(2);
    h.state_mut().controls_pad_press_for_test(PadButton::North);
    h.run_steps(2);
    assert!(h.query_by_label("Y: North").is_some());

    // Restore defaults for SNES keyboard.
    h.state_mut().set_controls_for_test(true, false);
    h.run_steps(2);
    h.get_by_label("Restore defaults").click();
    h.run_steps(2);
    assert_eq!(h.state().snes_key_for_test(SnesButton::Y), Some(Key::A));

    // Saved: a new app loads the SNES rebind of the controller.
    drop(h);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    assert_eq!(
        h.state().nes_key_for_test(NesButton::A),
        Some(Key::K),
        "NES rebind persisted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
