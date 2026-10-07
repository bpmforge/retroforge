//! **Library pictures show the game, not its boot frame** (ticket W21-10;
//! `docs/design/UX_WAVE_21.md` §8). Brad's library showed Super Mario
//! Bros. 3 as a grey blob and Super Mario World as "Nintendo Presents":
//! the picture was the first non-flat frame. Quitting now replaces it with
//! the frame on screen. Real app, real core.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_to(h: &mut Harness<'_, RetroForgeApp>, frames: u64) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < frames && start.elapsed() < Duration::from_secs(30) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn quitting_replaces_the_boot_picture_with_the_frame_on_screen() {
    let dir = std::env::temp_dir().join(format!("retroforge_lib_picture_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let rom = dir.join("rf-scroller.nes");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE), &rom).expect("fixture");
    let mut h = Harness::builder()
        .with_size(egui::vec2(800.0, 760.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().launch_rom(&rom);
    run_to(&mut h, 60);
    let hash = h.state().current_game_hash_for_test().expect("hash");
    let boot = h.state_mut().library_picture_for_test(&hash);
    assert!(
        boot.is_some(),
        "self-test: the first-frame capture still fills an empty picture"
    );

    // Hold Right so the scroller moves: the frame at quit differs from
    // the boot capture, which is the change this test exists to see.
    h.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    run_to(&mut h, 200);
    let last = h.state().last_frame_rgba_for_test().expect("a frame");
    h.state_mut().close_rom_for_test();
    h.run_steps(2);
    let after = h
        .state_mut()
        .library_picture_for_test(&hash)
        .expect("picture");
    assert_ne!(
        Some(&after),
        boot.as_ref(),
        "quitting replaced the boot picture"
    );
    let decoded = image::load_from_memory(&after).expect("png").to_rgba8();
    assert_eq!(decoded.as_raw().len(), last.len(), "same frame size");
    assert_eq!(
        decoded.as_raw(),
        &last,
        "the picture is the frame on screen at quit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
