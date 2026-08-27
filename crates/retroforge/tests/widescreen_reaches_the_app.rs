//! **Decoded widescreen reaches the running app** (ticket W11-03,
//! FR-ENH-004).
//!
//! The engine for this existed and was tested long before a user could
//! invoke it: `rf_enhance::widescreen` is 441 lines with its own suite,
//! and `WidescreenPolicies` was referenced by NOTHING in
//! `crates/retroforge/src`. `CoreCommand` had one enhancement variant.
//! The Enhance panel's "widescreen_decoded" checkbox set a field that
//! round-tripped to disk and changed nothing anyone could see.
//!
//! **So this test drives `RetroForgeApp`, not the library.** A test that
//! called `WidescreenPolicies::decide_all` directly would stay green
//! forever while the product did nothing — which is precisely the state
//! this ticket found.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WIDESCREEN_WIDTH, WINDOW_SIZE};

const SNES: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// One test, not two, and the reason is in the `unsafe` block below:
/// `set_var` is process-global, so two tests in one binary racing to set
/// `RETROFORGE_CONFIG_DIR` is a data race that shows up as one of them
/// mysteriously never receiving a frame. The existing SNES app test has
/// the same "only test in this binary" note for the same reason.
fn boot() -> (Harness<'static, RetroForgeApp>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("retroforge_ws_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let snes = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES);
    assert!(snes.exists(), "SNES fixture missing: {}", snes.display());
    let rom: PathBuf = dir.join("rf-scroller-s.sfc");
    std::fs::copy(&snes, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    // The app boots PAUSED; without this no frame is ever produced and
    // every assertion below fails for a reason that has nothing to do
    // with widescreen.
    harness.state_mut().resume_for_test();
    run_frames(&mut harness, 30, Duration::from_secs(30));
    (harness, rom)
}

/// **Criterion 2: a refusal is surfaced, not swallowed.**
///
/// bsnes-hd's lesson is that widening the wrong layer looks like a bug in
/// the GAME — a status bar smeared across the margins. So the policy
/// declines those layers, and the app must be able to say which and why.
/// A decision the user cannot see is the same as one that was ignored.
fn assert_refusals_are_surfaced(harness: &mut Harness<'_, RetroForgeApp>) {
    harness.state_mut().set_widescreen(true);
    let target = harness.state().frame_count_for_test() + 6;
    run_frames(harness, target, Duration::from_secs(20));

    let decisions = harness.state().widescreen_decisions();
    // The fixture does not enable all four backgrounds, so at least one
    // must come back refused — and with a REASON, not merely `false`.
    let refused: Vec<_> = decisions.iter().flatten().collect();
    assert!(
        !refused.is_empty(),
        "at least one of this fixture's four backgrounds is not a scrolling \
         layer, so the policy must decline it: {decisions:?}"
    );
    for why in &refused {
        assert!(
            !why.is_empty(),
            "a refusal must carry a reason a UI can print, got an empty string"
        );
    }
}

#[test]
fn widescreen_widens_the_app_and_surfaces_its_refusals() {
    let (mut harness, _rom) = boot();

    let before = harness
        .state()
        .frame_size_for_test()
        .expect("the SNES core must deliver a frame before widescreen is asked for");
    assert_eq!(
        before.0, 256,
        "a fresh session renders 4:3 — law 6, a fresh install boots in Accuracy Mode"
    );

    // The user asks. This is the whole point: the path from a UI toggle
    // to a wider picture, which did not exist.
    harness.state_mut().set_widescreen(true);
    let target = harness.state().frame_count_for_test() + 8;
    run_frames(&mut harness, target, Duration::from_secs(30));

    let after = harness
        .state()
        .frame_size_for_test()
        .expect("frames must keep arriving");
    assert_eq!(
        after.0, WIDESCREEN_WIDTH,
        "enabling decoded widescreen must widen the frame the app draws \
         (was {before:?}, now {after:?})"
    );
    assert_eq!(after.1, before.1, "widening must not change the height");
    let rgba = harness.state().last_frame_rgba_for_test().expect("a frame");
    assert_eq!(
        rgba.len(),
        after.0 * after.1 * 4,
        "the RGBA buffer must match the reported size"
    );

    assert_refusals_are_surfaced(&mut harness);

    // ...and switching it off restores the accuracy path exactly.
    harness.state_mut().set_widescreen(false);
    let target = harness.state().frame_count_for_test() + 8;
    run_frames(&mut harness, target, Duration::from_secs(30));
    assert_eq!(
        harness.state().frame_size_for_test().map(|s| s.0),
        Some(256),
        "turning widescreen off must return to 4:3, not stay wide"
    );
}
