//! **The HD-2D lens runs over the 3D Diorama image** (ticket W34-06b;
//! `docs/design/HD2D_PLAN.md` section 3, "Interaction with the lens").
//!
//! W34-06 found the lens only ever fed the flat picture. This pins the fix on
//! the same synthetic fixture `diorama_live_view.rs` uses: with the Diorama
//! on screen, enabling bloom/grade changes the bytes uploaded to the texture
//! (and only those: the raw render, the accuracy frame's input, is
//! untouched), and the lens's 2D shadows do nothing there. Own binary: the
//! process-global config dir is set once.

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
fn the_lens_runs_over_the_3d_image_and_its_2d_shadows_stay_off() {
    let rom = synthetic_rom();
    let hashes = match rf_cart::Cartridge::load(&rom).expect("synthetic NROM must be valid") {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity.normalized
        }
    };
    let dir = std::env::temp_dir().join(format!("retroforge_lens_diorama_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let profiles_dir = dir.join("profiles").join("test-lens-diorama");
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
        eprintln!("SKIP lens_over_diorama: no wgpu adapter in this environment");
        return;
    }
    harness.state_mut().open_rom_path(&rom_path);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    harness.state_mut().set_mode_for_test(Mode::GameAware);
    harness.state_mut().set_diorama_for_test(true);
    harness.state_mut().edit_lens_for_test(|s| {
        s.lens_follow_clock = false;
        s.lens_time_of_day = 80;
    });
    run_frames(&mut harness, 12, std::time::Duration::from_secs(10));
    harness.run_steps(2);
    let raw_hash = harness
        .state()
        .diorama_rgba_hash_for_test()
        .expect("a live Diorama render");

    // No lens effect: the texture is the raw render.
    let (shown, lensed) = harness.state().diorama_shown_hash_for_test().unwrap();
    assert!(!lensed, "no effect is on, so the lens must not run");
    assert_eq!(shown, raw_hash);

    // Shadows alone: the lens runs (it is ticked) but with 2D shadows forced
    // off over the 3D view and no other effect it is the identity.
    harness.state_mut().edit_lens_for_test(|s| {
        s.lens_shadows = true;
        s.lens_shadows_pct = 100;
    });
    run_frames(&mut harness, 20, std::time::Duration::from_secs(10));
    harness.run_steps(2);
    let (shown, lensed) = harness.state().diorama_shown_hash_for_test().unwrap();
    assert!(lensed, "a ticked lens runs over the 3D image");
    assert_eq!(
        shown,
        harness.state().diorama_rgba_hash_for_test().unwrap(),
        "2D lens shadows must not change the 3D picture"
    );

    // Bloom + grade + tilt-shift: the picture the player sees changes, the
    // raw render does not (the lens is presentation only).
    harness.state_mut().edit_lens_for_test(|s| {
        s.lens_bloom = true;
        s.lens_bloom_pct = 80;
        s.lens_grade = true;
        s.lens_grade_pct = 100;
        s.lens_tiltshift = true;
        s.lens_tiltshift_pct = 80;
    });
    run_frames(&mut harness, 30, std::time::Duration::from_secs(10));
    harness.run_steps(2);
    let (shown, lensed) = harness.state().diorama_shown_hash_for_test().unwrap();
    let raw_now = harness.state().diorama_rgba_hash_for_test().unwrap();
    assert!(lensed);
    assert_ne!(shown, raw_now, "bloom and grade must change the 3D picture");

    // Back off: the plain render is what is shown again.
    harness.state_mut().edit_lens_for_test(|s| {
        s.lens_bloom = false;
        s.lens_grade = false;
        s.lens_tiltshift = false;
        s.lens_shadows = false;
    });
    run_frames(&mut harness, 40, std::time::Duration::from_secs(10));
    harness.run_steps(2);
    let (shown, lensed) = harness.state().diorama_shown_hash_for_test().unwrap();
    assert!(!lensed);
    assert_eq!(shown, harness.state().diorama_rgba_hash_for_test().unwrap());
}
