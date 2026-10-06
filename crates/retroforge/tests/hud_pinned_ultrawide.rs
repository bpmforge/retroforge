//! **A profile's HUD is pinned over the ultrawide view** (ticket W20-17;
//! `docs/design/UX_WAVE_20.md` §6; `rf_enhance::hud::HudSeparator`, built
//! in W8-06 and, until W20-17, referenced by nothing in the shell).
//!
//! RF-Scroller's profile declares `[camera.hud]` at the top of the frame.
//! The stitcher leaves HUD rows out of the canvas, so the wide view had no
//! HUD; now, in Game-Aware with the Ultrawide camera, the declared rows
//! are cut from the live frame and composited centred along the top edge.
//! Outside Game-Aware nothing is pinned. Skips when no GPU adapter exists.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn settle(harness: &mut Harness<'_, RetroForgeApp>, frames: u64) {
    let target = harness.state().frame_count_for_test() + frames;
    let start = Instant::now();
    while harness.state().frame_count_for_test() < target
        && start.elapsed() < Duration::from_secs(30)
    {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn the_declared_hud_is_pinned_over_the_ultrawide_view_in_game_aware() {
    let dir = std::env::temp_dir().join(format!("retroforge_hud_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statements of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
    }
    let rom = dir.join("rf-scroller.nes");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE), &rom).expect("copy");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    if !harness.state().gpu_available_for_test() {
        eprintln!("SKIP: no GPU adapter; the ultrawide view cannot compose here");
        return;
    }
    harness.state_mut().launch_rom(&rom);
    settle(&mut harness, 5);
    harness.state_mut().set_ultrawide_for_test();

    // Enhanced is not Game-Aware: the profile's HUD is not acted on.
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    settle(&mut harness, 40);
    assert!(harness.state().hud_band_for_test().is_none());
    assert!(
        harness
            .state()
            .hud_verdict_for_test()
            .is_some_and(|v| v.contains("Game-Aware")),
        "{:?}",
        harness.state().hud_verdict_for_test()
    );

    harness.state_mut().set_mode_for_test(Mode::GameAware);
    harness.state_mut().set_ultrawide_for_test();
    settle(&mut harness, 40);
    let (hw, hh, hud) = harness
        .state()
        .hud_band_for_test()
        .expect("Game-Aware + Ultrawide cuts the declared band");
    assert_eq!(hw, 256);
    assert!(hh >= 1);
    assert!(harness
        .state()
        .hud_verdict_for_test()
        .is_some_and(|v| v.contains("pinned at the edge")));

    // The latest render carries those rows, centred on the top edge. The
    // render can predate the newest band by a frame, so retry briefly.
    let start = Instant::now();
    loop {
        harness.state_mut().set_ultrawide_for_test();
        settle(&mut harness, 2);
        let (rw, _rh, rgba) = harness
            .state()
            .ultrawide_render_for_test()
            .expect("an ultrawide render");
        let (hud_now_w, _, hud_now) = harness.state().hud_band_for_test().expect("band");
        assert_eq!(hud_now_w, hw);
        let x0 = ((rw - hw) / 2) as usize * 4;
        let top_row = &rgba[x0..x0 + hw as usize * 4];
        if top_row == &hud_now[..hw as usize * 4] || top_row == &hud[..hw as usize * 4] {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the ultrawide view's top row never showed the HUD band"
        );
    }
}
