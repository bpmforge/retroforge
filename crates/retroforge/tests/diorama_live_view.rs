//! **The Diorama compositor is wired into the live play view** (ticket
//! W16-13; `docs/design/ENHANCEMENT_WAVE_16.md` §5's "walls pop up" tier
//! two, closing the gap W16-06 filed this ticket for: "compose_diorama is
//! not wired to the live view").
//!
//! `tests/diorama_reaches_the_app.rs` (W16-06) already proves the SETTING
//! reaches the badge; it deliberately uses the default (no-GPU) `Harness`,
//! so it never actually exercises `DioramaPass::render`. This test proves
//! the other half: with a real GPU device (`egui_kittest::wgpu::
//! WgpuTestRenderer`, the same convention `tests/capture_tour.rs` uses),
//! turning Diorama on actually changes what `video_panel` paints, and
//! turning it back off restores the flat view — acceptance criterion 1's
//! "disabling returns to the flat enhanced view the same frame" and the
//! ticket's own required kittest scenario.
//!
//! A separate test BINARY from `diorama_reaches_the_app.rs`, not a second
//! `#[test]` in it: that file's own header claims sole ownership of the
//! process-global `RETROFORGE_CONFIG_DIR`/`RETROFORGE_PROFILES_DIR`
//! `set_var` calls ("first statements of the only test in this binary"),
//! and a second test sharing that global state in the same process is
//! exactly the hazard that convention exists to avoid.
//!
//! Reuses the same synthetic NROM + `metatile_screens`/`[decode.collision]`
//! profile `diorama_reaches_the_app.rs` built, for the identical reason:
//! no shipped fixture/commercial profile pairs a real identity match with
//! a collision table yet (W16-05's own closing note).

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
fn enabling_diorama_changes_the_live_view_texture_and_disabling_restores_the_flat_one() {
    let rom = synthetic_rom();
    let hashes = match rf_cart::Cartridge::load(&rom).expect("synthetic NROM must be valid") {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity.normalized
        }
    };

    let dir = std::env::temp_dir().join(format!("retroforge_diorama_live_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let profiles_dir = dir.join("profiles").join("test-diorama-live");
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

    // A real GPU device (`tests/capture_tour.rs`'s own convention) — the
    // default `Harness` (no `WgpuTestRenderer`) leaves `RetroForgeApp::gpu`
    // `None`, and `refresh_diorama_render` is a no-op without one.
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();

    if !harness.state().gpu_available_for_test() {
        eprintln!(
            "SKIP enabling_diorama_changes_the_live_view_texture_and_disabling_restores_the_flat_one: \
             no wgpu adapter in this environment -- clean skip"
        );
        return;
    }

    harness.state_mut().open_rom_path(&rom_path);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    assert!(
        harness.state().has_decoded_level_for_test(),
        "the synthetic profile must match and decode the synthetic ROM's 1x1 level"
    );

    // Fresh install / Accuracy: no Diorama render at all.
    run_frames(&mut harness, 3, std::time::Duration::from_secs(5));
    assert_eq!(
        harness.state().diorama_rgba_hash_for_test(),
        None,
        "Accuracy mode must never produce a Diorama render (law 6)"
    );

    // Game-Aware AND Diorama on: the live view must now be composited.
    harness.state_mut().set_mode_for_test(Mode::GameAware);
    harness.state_mut().set_diorama_for_test(true);
    run_frames(&mut harness, 10, std::time::Duration::from_secs(10));

    let on_hash = harness.state().diorama_rgba_hash_for_test();
    assert!(
        on_hash.is_some(),
        "Game-Aware + a matching collision profile + Diorama on must produce a live render \
         within a few frames of the core actually running"
    );

    // Turning it back off: acceptance criterion 1's "disabling returns to
    // the flat enhanced view the SAME frame" — one repaint (not gated on
    // the core producing a new frame; `sync_diorama_subscription` runs on
    // every `eframe::App::ui` call) must already show it cleared.
    harness.state_mut().set_diorama_for_test(false);
    harness.run_steps(1);
    assert_eq!(
        harness.state().diorama_rgba_hash_for_test(),
        None,
        "turning Diorama off must clear the render immediately, not just stop refreshing it"
    );
}
