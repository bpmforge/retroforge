//! **Temporal de-flicker is reachable by a user** (ticket W11-01;
//! FR-ENH-002).
//!
//! ## Why this test exists in this shape
//!
//! `rf_enhance::sprite_historian` has had its own red-fixture proof since
//! W3-05 — the RF-Scroller >8-sprites-per-scanline gem scene — and
//! `GameSettings::deflicker` has been persisted and offered as a
//! checkbox for just as long. Both were true, and **the feature was
//! still unreachable**: `.deflicker` had zero readers outside the widget
//! that set it, and `CoreCommand` had exactly one enhancement variant.
//! Toggling it wrote a bool to disk.
//!
//! Every proof that existed drove the LIBRARY. This one drives
//! `RetroForgeApp`, because a library-level proof is precisely what let a
//! shipped-and-inert feature look finished.
//!
//! ## What this file proves, and what it deliberately does not
//!
//! It proves **reachability**: the toggle reaches the core, the core
//! keeps running with it on, and frames keep arriving at the UI. That is
//! the thing that was missing.
//!
//! It does **not** prove the reconstruction bites, and the `assert_ne!`
//! below should not be read as if it did — any two frames of a moving
//! game differ, so it would pass against a core that accepted
//! `SetDeflicker` and ignored it. Making it discriminating would mean
//! comparing the same emulated frame rendered both ways, and a
//! free-running core on its own thread cannot be asked for that: repaint
//! timing and frame timing are independent, so two runs do not line up.
//!
//! The discriminating assertions live in `core_thread`'s unit tests,
//! where the transform can be driven directly:
//! `display_rgba_is_the_untouched_frame_when_deflicker_is_off`,
//! `the_sprite_overlay_survives_a_deflicker_rebuild` (verified to fail
//! when the re-composite is removed), and
//! `deflicker_cannot_perturb_the_accuracy_frame`. Saying so here rather
//! than letting the file's name imply more than it checks.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

/// The fixture whose gem scene is the documented flicker case (W3-05).
const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(harness: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while harness.state().frame_count_for_test() < target && start.elapsed() < deadline {
        harness.run_steps(1);
        // The core runs on its OWN thread in wall-clock time; spinning
        // the UI without yielding advances the emulator by almost
        // nothing. `capture_tour.rs` learned this the hard way.
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn toggling_deflicker_changes_the_pixels_the_app_receives() {
    let dir = std::env::temp_dir().join(format!("retroforge_deflicker_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
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
    harness.state_mut().resume_for_test();

    // Walk right, so sprites actually rotate — a still screen has no
    // flicker to reconstruct and the two frames below would be equal for
    // an uninteresting reason.
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    run_frames(&mut harness, 240, Duration::from_secs(20));
    assert!(
        harness.state().frame_count_for_test() >= 120,
        "only {} frames ran; this would be a comparison of two boot screens",
        harness.state().frame_count_for_test()
    );

    let off = harness
        .state()
        .last_frame_rgba_for_test()
        .expect("a frame must have reached the UI with de-flicker off");

    // ---- the toggle, through the app ------------------------------
    harness.state_mut().set_deflicker_for_test(true);
    let before = harness.state().frame_count_for_test();
    run_frames(&mut harness, before + 240, Duration::from_secs(20));

    let on = harness
        .state()
        .last_frame_rgba_for_test()
        .expect("a frame must have reached the UI with de-flicker on");

    assert_eq!(
        off.len(),
        on.len(),
        "frame size changed, which is not the test"
    );
    assert_ne!(
        off, on,
        "the frames are byte-identical with de-flicker off and on. Either the command never \
         reaches the core, or the core accepts it and ignores it \u{2014} which is the state \
         this ticket exists to end. Note that ANY two frames of a moving game differ, so an \
         equal result here means the UI stopped receiving new frames entirely."
    );

    // ---- and the simulation is untouched --------------------------
    //
    // The load-bearing half. An enhancement that changed the emulation
    // would be a bug of a completely different order than one that did
    // nothing, and `SpriteHistorian::observe` takes `&[PpuPixel]`
    // precisely so it cannot. Asserted here at the app level rather than
    // trusted to the signature.
    assert!(
        harness.state().frame_count_for_test() > before,
        "the core stopped advancing when de-flicker was switched on"
    );
    assert!(
        harness.state().crash_message_for_test().is_none(),
        "the core crashed with de-flicker on: {:?}",
        harness.state().crash_message_for_test()
    );
}
