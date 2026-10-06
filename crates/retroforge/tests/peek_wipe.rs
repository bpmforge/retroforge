//! **Hold-to-peek shows the accuracy-exact frame, with a wipe** (ticket
//! W20-16; `docs/design/UX_WAVE_20.md` §5).
//!
//! Before W20-16 peek only switched the camera: the Original view still
//! drew the resolved frame, with sprite bypass / de-flicker / HD art in it.
//! Now the picture wipes (~200 ms) to the frame rebuilt from the core's
//! own indexed video, and back. In Accuracy mode there is nothing to peek
//! past, so nothing moves. The wipe geometry is unit-tested in
//! `play_view::peek_frame`.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

/// Hold or release the default peek key (Backtick), as a player does.
fn peek_key(harness: &mut Harness<'_, RetroForgeApp>, held: bool) {
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Backtick,
        physical_key: None,
        pressed: held,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[test]
fn peek_wipes_to_the_original_and_back_and_does_nothing_in_accuracy() {
    let dir = std::env::temp_dir().join(format!("retroforge_peek_{}", std::process::id()));
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
    let run = |harness: &mut Harness<'_, RetroForgeApp>, n: u64| {
        let target = harness.state().frame_count_for_test() + n;
        let start = Instant::now();
        while harness.state().frame_count_for_test() < target
            && start.elapsed() < Duration::from_secs(30)
        {
            harness.run_steps(1);
            std::thread::sleep(Duration::from_millis(4));
        }
    };

    // Accuracy: peek does nothing.
    run(&mut harness, 20);
    peek_key(&mut harness, true);
    for _ in 0..10 {
        harness.step();
    }
    assert_eq!(
        harness.state().peek_for_test().0,
        0.0,
        "Accuracy: nothing to peek past"
    );
    peek_key(&mut harness, false);

    // Enhanced with de-flicker: an enhancement is visible.
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    harness.state_mut().set_deflicker_for_test(true);
    let _ = harness.state_mut().hash_display_for_test();
    run(&mut harness, 30);

    // Pause, so the frame under test is fixed; then peek fully in. No
    // new frame arrives while paused, so this also proves the rebuild path.
    harness.state_mut().pause_for_test();
    for _ in 0..10 {
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    let resolved = fnv(&harness.state().last_frame_rgba_for_test().expect("a frame"));
    peek_key(&mut harness, true);
    for _ in 0..20 {
        harness.step();
    }
    let (amount, original) = harness.state().peek_for_test();
    assert_eq!(amount, 1.0, "the wipe completes");
    let original = original.expect("the accuracy-exact frame is held while enhanced");
    assert_eq!(
        harness.state_mut().hash_display_for_test(),
        Some(original),
        "fully peeked: the screen shows the accuracy-exact frame, byte for byte"
    );

    // Release: back to the resolved (enhanced) frame.
    peek_key(&mut harness, false);
    for _ in 0..20 {
        harness.step();
    }
    assert_eq!(harness.state().peek_for_test().0, 0.0, "released");
    assert_eq!(
        harness.state_mut().hash_display_for_test(),
        Some(resolved),
        "released: the resolved frame again"
    );
}
