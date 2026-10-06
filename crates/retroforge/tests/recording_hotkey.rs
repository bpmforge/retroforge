//! **F10 records the picture** (ticket W20-14; `docs/design/UX_WAVE_20.md`
//! §5).
//!
//! `crate::recording::Recorder` (W8-03) existed and nothing constructed it.
//! This presses F10, lets the game run, presses F10 again, and checks the
//! file on disk: an APNG whose own `acTL` chunk declares as many frames as
//! the app says it captured.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

/// `num_frames` from the APNG's `acTL` chunk (PNG chunks: 4-byte length,
/// 4-byte type, data, 4-byte CRC; acTL data starts with num_frames, u32 BE).
fn apng_frames(bytes: &[u8]) -> Option<u32> {
    let mut i = 8; // PNG signature
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().ok()?) as usize;
        let kind = &bytes[i + 4..i + 8];
        if kind == b"acTL" {
            return Some(u32::from_be_bytes(bytes[i + 8..i + 12].try_into().ok()?));
        }
        i += 12 + len; // always advances: header + CRC are 12 bytes
    }
    None
}

#[test]
fn f10_twice_writes_an_apng_of_the_frames_in_between() {
    let dir = std::env::temp_dir().join(format!("retroforge_record_{}", std::process::id()));
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
    assert_eq!(
        harness
            .state()
            .app_bindings_key_for_test(retroforge::app_bindings::AppAction::Record),
        Some(egui::Key::F10)
    );
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
    run(&mut harness, 30);

    harness.key_press(egui::Key::F10);
    harness.run_steps(1);
    assert!(
        harness
            .state()
            .osd_texts_for_test()
            .iter()
            .any(|t| t.contains("REC")),
        "a REC card while recording: {:?}",
        harness.state().osd_texts_for_test()
    );
    run(&mut harness, 40);
    harness.key_press(egui::Key::F10);

    let start = Instant::now();
    while harness.state().last_recording_for_test().is_none()
        && start.elapsed() < Duration::from_secs(30)
    {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
    let (path, frames) = harness
        .state()
        .last_recording_for_test()
        .expect("the recording was written");
    assert!(
        frames >= 20,
        "captured {frames} frames over ~40 emulated ones"
    );
    let bytes = std::fs::read(&path).expect("read the recording");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "a PNG");
    assert_eq!(apng_frames(&bytes), Some(frames as u32), "acTL frame count");
    assert!(
        harness
            .state()
            .osd_texts_for_test()
            .iter()
            .any(|t| t.starts_with("Recording saved")),
        "{:?}",
        harness.state().osd_texts_for_test()
    );
}
