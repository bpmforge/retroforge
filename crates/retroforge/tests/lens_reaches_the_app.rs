//! **The HD-2D lens reaches the picture, and only the picture** (ticket
//! W30-04; `rf_renderer::lens::LensPass`).
//!
//! Drives the real `RetroForgeApp` on a real GPU (`WgpuTestRenderer`, the
//! `fog_reaches_the_app.rs` convention) with the synthetic many-sprites NES
//! cartridge (`mode_invariant::many_sprites_rom`: no copyrighted data) --
//! an NES game, because the row is generic and must not need an SNES.
//!
//! * In Accuracy the row is neither counted nor drawn: the screen is the
//!   resolved frame byte for byte.
//! * In Enhanced, with an effect on, the row is counted and the screen
//!   differs from the resolved frame -- while the resolved frame itself
//!   (what the core produced, before the lens) is unchanged, and no core
//!   command is sent by turning effects on and off. That is the mode
//!   invariant: the pass runs in the app after the core and has no way to
//!   reach it.
//! * Shadows arm the core's sprite-layer extraction only while they are on.
//!
//! Skips when no GPU adapter exists.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use rf_renderer::lens::Weather;

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

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

/// `(screen hash, resolved-frame hash)` of the latest frame.
fn hashes(harness: &mut Harness<'_, RetroForgeApp>) -> (u64, u64) {
    let display = harness
        .state_mut()
        .hash_display_for_test()
        .expect("a frame was shown");
    let frame = harness
        .state()
        .last_frame_rgba_for_test()
        .map(|f| fnv(&f))
        .expect("a resolved frame");
    (display, frame)
}

fn lens_counted(harness: &Harness<'_, RetroForgeApp>) -> bool {
    harness
        .state()
        .badge_breakdown_for_test()
        .iter()
        .any(|l| l.contains("HD-2D lens"))
}

#[test]
fn the_lens_changes_the_screen_but_never_the_core_or_its_frame() {
    let dir = std::env::temp_dir().join(format!("retroforge_lens_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let rom = dir.join("rf-many-sprites.nes");
    std::fs::write(&rom, retroforge::mode_invariant::many_sprites_rom()).expect("write ROM");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    if !harness.state().gpu_available_for_test() {
        eprintln!("SKIP: no GPU adapter; the lens pass cannot run here");
        return;
    }
    harness.state_mut().launch_rom(&rom);
    let _ = harness.state_mut().hash_display_for_test();
    settle(&mut harness, 30);

    // The baseline: Enhanced, lens off. The screen is the resolved frame.
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    settle(&mut harness, 10);
    let (screen_off, frame_off) = hashes(&mut harness);
    assert!(!lens_counted(&harness), "nothing chosen, nothing counted");
    assert_eq!(screen_off, frame_off, "lens off: the plain picture");
    let commands_before = harness.state().core_commands_sent_for_test();

    // Accuracy never draws or counts the lens, whatever is ticked.
    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    harness.state_mut().edit_lens_for_test(|s| {
        s.lens_grade = true;
        s.lens_grade_pct = 100;
        s.lens_time_of_day = 0;
    });
    settle(&mut harness, 10);
    let (screen, frame) = hashes(&mut harness);
    assert!(!lens_counted(&harness));
    assert_eq!(screen, frame, "Accuracy shows the plain picture");

    // Enhanced + night grade: counted, and the screen is no longer the
    // frame -- but the frame the core made is exactly what it was.
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    settle(&mut harness, 10);
    let commands_enhanced = harness.state().core_commands_sent_for_test();
    let (screen_on, frame_on) = hashes(&mut harness);
    assert!(
        lens_counted(&harness),
        "{:?}",
        harness.state().badge_breakdown_for_test()
    );
    assert_ne!(screen_on, frame_on, "the lens must change what is shown");
    assert_eq!(frame_on, frame_off, "the core's frame is unchanged");

    // Every effect, on and off again, including the weather: still no core
    // command, and the core frame still the same.
    harness.state_mut().edit_lens_for_test(|s| {
        s.lens_bloom = true;
        s.lens_tiltshift = true;
        s.lens_weather = Weather::Rain;
        s.lens_weather_pct = 80;
    });
    settle(&mut harness, 10);
    let (screen_all, frame_all) = hashes(&mut harness);
    assert_ne!(screen_all, screen_on, "more effects, a different picture");
    assert_eq!(frame_all, frame_off, "the core's frame is unchanged");
    assert!(!harness.state().lens_shadows_wanted_for_test());

    // Shadows hold the sprite layer open, and let go when switched off.
    harness.state_mut().edit_lens_for_test(|s| {
        s.lens_grade = false;
        s.lens_bloom = false;
        s.lens_tiltshift = false;
        s.lens_weather = Weather::None;
        s.lens_shadows = true;
        s.lens_shadows_pct = 100;
    });
    settle(&mut harness, 20);
    assert!(harness.state().lens_shadows_wanted_for_test());
    let (screen_shadow, frame_shadow) = hashes(&mut harness);
    assert_ne!(
        screen_shadow, frame_shadow,
        "sprites on screen cast a shadow onto the background"
    );
    assert_eq!(frame_shadow, frame_off, "the core's frame is unchanged");

    harness.state_mut().edit_lens_for_test(|s| {
        s.lens_shadows = false;
    });
    settle(&mut harness, 10);
    assert!(!harness.state().lens_shadows_wanted_for_test());
    let (screen_none, frame_none) = hashes(&mut harness);
    assert!(!lens_counted(&harness), "no effect on, row off");
    assert_eq!(screen_none, frame_none, "back to the plain picture");

    assert_eq!(
        harness.state().core_commands_sent_for_test(),
        commands_enhanced,
        "turning the lens on and off sent the core nothing (layer extraction aside)"
    );
    assert!(commands_enhanced >= commands_before);
    assert!(harness.state().crash_message_for_test().is_none());
}
