//! **The full-level view is reachable by a user** (ticket W11-02;
//! VISION §2's "whole levels on one screen").
//!
//! Before W11-02 this was the largest gap between what the docs claimed
//! and what the product did. `LevelSession` was real and tested,
//! `SceneLayer::DecodedLevel` was real and tested — and **no pixel
//! producer for it existed anywhere**, so nothing could turn a decoded
//! level into an image. `level_view_demo.rs` asserts on the scene's
//! *shape* and stayed green throughout.
//!
//! This drives `RetroForgeApp`. What it proves:
//!
//! - a profile-matched ROM produces a decoded level in the app;
//! - arming the view makes the CORE report live camera bytes, which is
//!   the half that cannot be faked on this thread — only the core thread
//!   can peek without perturbing the machine;
//! - the camera **moves** when the player does, so the probe is reading
//!   the running game rather than a constant.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn the_decoded_level_and_a_live_camera_reach_the_app() {
    let dir = std::env::temp_dir().join(format!("retroforge_fullevel_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        // The escape hatch W11-02 added, for exactly this: a test's
        // working directory is the crate, not the repository root.
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
    }

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());
    let rom: PathBuf = dir.join("rf-scroller.nes");
    std::fs::copy(&fixture, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);

    assert!(
        harness.state().has_decoded_level_for_test(),
        "the shipped RF-Scroller profile must match the fixture and decode its level; if this \
         fails the profile's [[identity]] has drifted from the ROM"
    );
    assert!(
        harness.state().level_camera_for_test().is_none(),
        "nothing should be probed before the feature is switched on — law 6"
    );

    harness.state_mut().resume_for_test();
    harness.state_mut().set_full_level_view_for_test(true);
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    run_frames(&mut harness, 120, Duration::from_secs(20));

    let early = harness
        .state()
        .level_camera_for_test()
        .expect("with the view armed, the core must report the camera bytes the profile names");

    run_frames(&mut harness, 600, Duration::from_secs(30));
    let late = harness
        .state()
        .level_camera_for_test()
        .expect("the probe must keep reporting");

    // **The discriminating assertion.** A probe that returned zeroes, or
    // a constant, or values from the wrong addresses would satisfy every
    // check above. The camera has to actually track the player.
    assert_ne!(
        early, late,
        "the level camera did not move between frame ~120 and ~600 while holding Right. \
         Either the probe is reading the wrong addresses, or it is not reading the running \
         machine at all."
    );

    // And switching it off stops the probe, rather than leaving the core
    // peeking forever for a feature nobody is looking at.
    harness.state_mut().set_full_level_view_for_test(false);
    harness.run_steps(3);
    assert!(
        harness.state().level_camera_for_test().is_none(),
        "disarming the view must stop the probe"
    );
}
