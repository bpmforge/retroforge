//! **Profile-declared loading fast-forward reaches the app** (ticket
//! W20-17; FR-ENH-008; `rf_enhance::loading`, built in W8-07 and, until
//! W20-17, referenced by nothing in the shell).
//!
//! This proves the APP half: with RF-Scroller's profile (which declares a
//! `[[loading.wait_loops]]`) the "Loading fast-forward" row is offered,
//! counted by the badge only when on in Game-Aware mode, and absent in
//! Accuracy. The CORE half — a matching loop runs unpaced and is reported
//! — is `core_thread::tests::a_matching_wait_loop_runs_unpaced_and_is_reported`
//! on a synthetic ROM, because RF-Scroller's declared loop never matches:
//! sampled at every frame boundary for 120 frames its PC stays in
//! $8012-$91xx (never the declared $C000) and `columns_streamed` stops at
//! 63, not 95 (W20-17 notes; ENHANCEMENT_AUDIT.md).

use std::path::Path;

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn loading_fast_forward_is_offered_by_a_profile_and_counted_only_when_on() {
    let dir = std::env::temp_dir().join(format!("retroforge_loading_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let rom = dir.join("rf-scroller.nes");
    std::fs::copy(&fixture, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    // Configure this game (Game-Aware + the toggle), then boot it fresh.
    harness.state_mut().launch_rom(&rom);
    harness.run_steps(2);
    harness.state_mut().set_mode_for_test(Mode::GameAware);
    harness.state_mut().set_loading_fast_forward_for_test(true);
    assert!(
        harness
            .state()
            .badge_breakdown_for_test()
            .iter()
            .any(|l| l.contains("Loading fast-forward")),
        "the row is effective and counted: {:?}",
        harness.state().badge_breakdown_for_test()
    );
    // Accuracy: the row exists but is not effective, so not counted.
    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    harness.run_steps(1);
    assert!(
        !harness
            .state()
            .badge_breakdown_for_test()
            .iter()
            .any(|l| l.contains("Loading fast-forward")),
        "Accuracy never counts it"
    );
}
