//! **HD Mode 7 reaches the screen** (ticket W30-02, part B).
//!
//! Drives the real app and core thread on the synthetic Mode 7 cartridge
//! (`mode_invariant::mode7_floor_rom`: no copyrighted data, no toolchain)
//! and reads the size of the frame the app adopted: `scale` times native
//! when the Enhanced-only row is effective, native when it is off, and
//! never HD in Accuracy Mode -- even after the game has shown Mode 7.
//! Pixel content is covered by `rf-snes`'s sampler tests and
//! `rf-renderer`'s compositor tests; this proves the wiring.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;

const NATIVE: (usize, usize) = (256, 224);

/// Run the app until the adopted frame has `want` as its size (or time
/// runs out); `true` when it got there. The core runs on its own thread in
/// wall-clock time, so the UI must yield between steps.
fn reaches(h: &mut Harness<'_, RetroForgeApp>, want: (usize, usize)) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
        if h.state().frame_size_for_test() == Some(want) {
            return true;
        }
    }
    false
}

/// Let `n` more frames arrive, whatever size they are.
fn run_more_frames(h: &mut Harness<'_, RetroForgeApp>, n: u64) {
    let target = h.state().frame_count_for_test() + n;
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(30) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn hd_mode7_scales_the_adopted_frame_only_when_the_row_is_effective() {
    let dir = std::env::temp_dir().join(format!("retroforge_hd_mode7_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let rom = dir.join("rf-mode7-floor.sfc");
    std::fs::write(&rom, retroforge::mode_invariant::mode7_floor_rom()).expect("write ROM");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    assert!(
        harness.state().status().starts_with("Loaded"),
        "ROM did not open: {}",
        harness.state().status()
    );
    harness.state_mut().resume_for_test();

    // Accuracy Mode (a fresh game's mode) with the toggle on: native size.
    harness.state_mut().set_hd_mode7_for_test(true, 2);
    assert!(reaches(&mut harness, NATIVE), "no native frame arrived");
    run_more_frames(&mut harness, 30);
    assert_eq!(harness.state().frame_size_for_test(), Some(NATIVE));

    // Enhanced: once the game has shown Mode 7 the row is effective and
    // the adopted frame is 2x. Then 4x through the slider's setting.
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    assert!(
        reaches(&mut harness, (NATIVE.0 * 2, NATIVE.1 * 2)),
        "Enhanced + HD Mode 7 never produced a 2x frame"
    );
    assert!(
        harness
            .state()
            .badge_breakdown_for_test()
            .iter()
            .any(|l| l.contains("HD Mode 7")),
        "the badge names the effective feature"
    );
    harness.state_mut().set_hd_mode7_for_test(true, 4);
    assert!(
        reaches(&mut harness, (NATIVE.0 * 4, NATIVE.1 * 4)),
        "scale 4 never produced a 4x frame"
    );

    // Off: back to native the next frames.
    harness.state_mut().set_hd_mode7_for_test(false, 4);
    assert!(reaches(&mut harness, NATIVE), "off did not restore native");

    // Back on, then Accuracy Mode: Mode 7 has long since been seen, but
    // Accuracy never produces an HD frame.
    harness.state_mut().set_hd_mode7_for_test(true, 3);
    assert!(
        reaches(&mut harness, (NATIVE.0 * 3, NATIVE.1 * 3)),
        "scale 3 never produced a 3x frame"
    );
    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    assert!(reaches(&mut harness, NATIVE), "Accuracy kept an HD frame");
    run_more_frames(&mut harness, 30);
    assert_eq!(harness.state().frame_size_for_test(), Some(NATIVE));
    assert!(harness.state().crash_message_for_test().is_none());
}
