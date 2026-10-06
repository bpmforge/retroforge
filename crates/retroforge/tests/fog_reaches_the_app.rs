//! **The fog pass reaches the picture** (ticket W20-17;
//! `docs/design/UX_WAVE_20.md` §6; `rf_renderer::fog::FogPass`, built in
//! W16-04 and, until W20-17, constructed by nothing in the shell).
//!
//! Drives the real `RetroForgeApp` on a real GPU (`WgpuTestRenderer`, the
//! `shader_chain_applies.rs` convention) with RF-Scroller-S and a copy of
//! its profile whose `[atmosphere]` pin is raised from `shadow` to
//! `active` (the shipped profile stays `shadow`: the fixture has no real
//! fog, see its own comment). In Enhanced mode the fog row is counted and
//! the screen differs from the resolved frame; in Accuracy it is neither.
//! Skips when no GPU adapter exists.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;

const FIXTURE: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";
const PROFILE: &str = "../../profiles/snes/rf-scroller-s/profile.toml";

fn fnv(bytes: &[u8]) -> u64 {
    // Same FNV-1a the app uses for its display fingerprint.
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

/// Whether the screen shows the resolved frame byte-for-byte.
fn exact(harness: &mut Harness<'_, RetroForgeApp>) -> bool {
    let display = harness.state_mut().hash_display_for_test();
    let frame = harness.state().last_frame_rgba_for_test().map(|f| fnv(&f));
    display.is_some() && display == frame
}

fn fog_counted(harness: &Harness<'_, RetroForgeApp>) -> bool {
    harness
        .state()
        .badge_breakdown_for_test()
        .iter()
        .any(|l| l.contains("Atmosphere: fog"))
}

#[test]
fn a_profile_pinned_fog_plane_is_drawn_and_counted_only_outside_accuracy() {
    let dir = std::env::temp_dir().join(format!("retroforge_fog_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let profiles = dir.join("profiles");
    let profile_dir = profiles.join("snes/rf-scroller-s");
    std::fs::create_dir_all(&profile_dir).expect("scratch");
    // SAFETY: first statements of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var("RETROFORGE_PROFILES_DIR", &profiles);
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let shipped = std::fs::read_to_string(manifest.join(PROFILE)).expect("profile");
    assert!(shipped.contains("ladder = \"shadow\""), "the shipped pin");
    std::fs::write(
        profile_dir.join("profile.toml"),
        shipped.replace("ladder = \"shadow\"", "ladder = \"active\""),
    )
    .expect("write profile");
    let rom = dir.join("rf-scroller-s.sfc");
    std::fs::copy(manifest.join(FIXTURE), &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    if !harness.state().gpu_available_for_test() {
        eprintln!("SKIP: no GPU adapter; the fog pass cannot run here");
        return;
    }
    harness.state_mut().launch_rom(&rom);
    let _ = harness.state_mut().hash_display_for_test();
    settle(&mut harness, 30);

    // A fresh game is in Accuracy (law 6): no fog, the exact frame.
    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    settle(&mut harness, 10);
    assert!(!fog_counted(&harness), "Accuracy never counts fog");
    assert!(exact(&mut harness), "Accuracy shows the resolved frame");

    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    settle(&mut harness, 10);
    assert!(
        fog_counted(&harness),
        "the pinned plane makes the fog row effective: {:?}",
        harness.state().badge_breakdown_for_test()
    );
    assert!(!exact(&mut harness), "fog must change what is on screen");

    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    settle(&mut harness, 10);
    assert!(!fog_counted(&harness));
    assert!(
        exact(&mut harness),
        "back to Accuracy: the exact frame again"
    );
}
