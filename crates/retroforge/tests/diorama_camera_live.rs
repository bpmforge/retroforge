//! **The diorama camera is presentation only** (ticket W30-05). With a real
//! GPU device, moving the tilt / turn / zoom / follow settings changes the
//! Diorama picture when the Diorama is on, and changes nothing at all about
//! Accuracy mode: no Diorama render exists there and the frame the core
//! hands the UI is byte-identical with the camera at its defaults or moved.
//!
//! Its own test binary for the same reason `diorama_live_view.rs` is: the
//! `RETROFORGE_*` environment variables are process-global.

use std::path::PathBuf;

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;

const PRG_LEN: usize = 0x4000;
const CHR_LEN: usize = 0x2000;

/// Identical layout to `diorama_reaches_the_app.rs`'s own `synthetic_rom`
/// — see that file's header comment for the byte-by-byte breakdown.
fn synthetic_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + PRG_LEN + CHR_LEN];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    let prg = &mut rom[16..16 + PRG_LEN];
    prg[0x0004] = 0x00;
    prg[0x0005] = 0x01;
    prg[0x0006] = 0x00;
    prg[0x0007] = 0b1;
    rom
}

fn profile_toml(sha256: &str) -> String {
    format!(
        r#"
[meta]
profile_version = "0.1"
title = "Diorama live-view kittest fixture"
console = "nes"
region = "ntsc"
sources = ["synthetic, authored for ticket W16-13's kittest -- not a real game"]

[[identity]]
sha256 = "{sha256}"

[decode]
kind = "metatile_screens"

[decode.metatile]
table = 0
size = 4

[decode.screens]
width = 1
height = 1
order = "column_rle"

[decode.collision]
table = 7
bits = "solid"

[[rom_map]]
offset = 4
len = 1
label = "level_column_offset"
type = "table"
source = "synthetic test fixture (this test's own header comment)"

[[rom_map]]
offset = 5
len = 2
label = "level_rle_data"
type = "table"
source = "synthetic test fixture (this test's own header comment)"
"#
    )
}

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: std::time::Duration) {
    let start = std::time::Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

#[test]
fn moving_the_diorama_camera_changes_the_diorama_but_never_the_accuracy_frame() {
    let rom = synthetic_rom();
    let hashes = match rf_cart::Cartridge::load(&rom).expect("synthetic NROM must be valid") {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity.normalized
        }
    };
    let dir = std::env::temp_dir().join(format!("retroforge_diorama_cam_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let profiles_dir = dir.join("profiles").join("test-diorama-camera");
    std::fs::create_dir_all(&profiles_dir).expect("scratch profiles dir");
    std::fs::write(
        profiles_dir.join("profile.toml"),
        profile_toml(&hashes.sha256),
    )
    .expect("write synthetic profile");

    // SAFETY: first statements of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var("RETROFORGE_PROFILES_DIR", dir.join("profiles"));
    }
    let rom_path: PathBuf = dir.join("fixture.nes");
    std::fs::write(&rom_path, &rom).expect("write synthetic ROM");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    if !harness.state().gpu_available_for_test() {
        eprintln!("SKIP diorama camera test: no wgpu adapter in this environment -- clean skip");
        return;
    }
    harness.state_mut().open_rom_path(&rom_path);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    assert!(harness.state().has_decoded_level_for_test());

    // Accuracy, default camera: the reference frame.
    run_frames(&mut harness, 4, std::time::Duration::from_secs(5));
    let reference = harness.state().last_frame_rgba_for_test();
    assert!(reference.is_some(), "the core must have produced a frame");

    // Accuracy, camera moved: no Diorama, and the very same frame.
    harness
        .state_mut()
        .set_diorama_camera_for_test(30, 25, 300, true);
    run_frames(&mut harness, 8, std::time::Duration::from_secs(5));
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), None);
    assert_eq!(
        harness.state().last_frame_rgba_for_test(),
        reference,
        "the camera settings must not touch the Accuracy frame (law 6)"
    );

    // Game-Aware + Diorama: default camera, then the moved one.
    harness
        .state_mut()
        .set_diorama_camera_for_test(52, 0, 100, false);
    harness.state_mut().set_mode_for_test(Mode::GameAware);
    harness.state_mut().set_diorama_for_test(true);
    run_frames(&mut harness, 14, std::time::Duration::from_secs(10));
    let default_view = harness
        .state()
        .diorama_rgba_hash_for_test()
        .expect("Diorama on must render");

    harness
        .state_mut()
        .set_diorama_camera_for_test(30, 25, 300, true);
    let before = harness.state().frame_count_for_test();
    run_frames(&mut harness, before + 6, std::time::Duration::from_secs(10));
    let moved_view = harness
        .state()
        .diorama_rgba_hash_for_test()
        .expect("still rendering with the camera moved");
    assert_ne!(
        default_view, moved_view,
        "tilting, turning, zooming and following must change the Diorama picture"
    );

    // Back to Accuracy: the Diorama is gone and the frame is the reference.
    harness.state_mut().set_diorama_for_test(false);
    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    run_frames(
        &mut harness,
        before + 12,
        std::time::Duration::from_secs(10),
    );
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), None);
    assert_eq!(harness.state().last_frame_rgba_for_test(), reference);
}
