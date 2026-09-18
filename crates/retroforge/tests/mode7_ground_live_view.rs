//! **Mode 7 ground is wired into the live play view** (ticket W16-14;
//! `docs/design/ENHANCEMENT_WAVE_16.md` §9's "live wiring in
//! `refresh_diorama_render`", closing the gap W16-09's close note filed
//! this ticket for).
//!
//! No Mode 7 fixture ROM exists in this repo, and there is no cc65/ca65
//! toolchain on this machine to build one — the same constraint
//! `tests/mode7_plane_golden.rs`'s own header states for `rf-renderer`'s
//! golden. So this drives `RetroForgeApp` through its own test hook
//! (`RetroForgeApp::inject_mode7_frame_for_test`, the same "kittest
//! scenario through the app's test hooks" shape `tests/diorama_live_view.rs`
//! uses for the walls tier) with a synthetic [`rf_core_api::Mode7Frame`]
//! and a small VRAM/CGRAM snapshot, rather than loading a real SNES ROM.
//!
//! Proves: turning "Mode 7 as 3D" on and feeding a synthetic Mode 7 frame
//! actually changes what `video_panel` paints (a real `DioramaPass::
//! render_with_vp` call, via `egui_kittest::wgpu::WgpuTestRenderer`, the
//! same GPU convention `tests/diorama_live_view.rs`/`tests/capture_tour.rs`
//! use), and turning it back off restores the flat view the same frame
//! (acceptance criterion 3's "off returns to flat the same frame").
//!
//! No `RETROFORGE_CONFIG_DIR`/`RETROFORGE_PROFILES_DIR` globals are
//! touched here (no ROM is ever opened), so this file needs none of
//! `diorama_live_view.rs`'s "first statements of the only test in this
//! binary" discipline.

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use rf_core_api::{Mode7Frame, Mode7Registers};

const WIDTH: usize = 64;
const HEIGHT: usize = 64;

/// A small VRAM/CGRAM snapshot with ONE non-transparent Mode 7 tile
/// pixel, the same minimal shape `rf_snes::debug`'s own
/// `render_mode7_plane_rgba_at_density_one_matches_mode7_pixel_and_cgram_rgb`
/// test uses: tilemap (0,0) -> tile 2 (even byte), tile 2's pixel (0,0)
/// -> palette index 0x55 (odd byte), CGRAM entry 0x55 -> pure blue.
fn synthetic_vram_cgram() -> (Vec<u8>, Vec<u8>) {
    let mut vram = vec![0u8; 0x10000];
    vram[0] = 2;
    vram[(2 * 64) * 2 + 1] = 0x55;
    let mut cgram = vec![0u8; 512];
    let word: u16 = 0x1F << 10; // pure blue
    cgram[0x55 * 2] = word as u8;
    cgram[0x55 * 2 + 1] = (word >> 8) as u8;
    (vram, cgram)
}

/// A flat (no HDMA ramp) unity matrix -- top == bottom, exactly the
/// "static frame" shape `rf_snes::debug::mode7_frame`'s own doc gives for
/// the overwhelmingly common case.
fn synthetic_frame() -> Mode7Frame {
    let regs = Mode7Registers {
        a: 256,
        b: 0,
        c: 0,
        d: 256,
        x0: 0,
        y0: 0,
        hofs: 0,
        vofs: 0,
        flip_x: false,
        flip_y: false,
    };
    Mode7Frame {
        top: regs,
        bottom: regs,
        lines: None,
    }
}

#[test]
fn enabling_mode7_ground_changes_the_live_view_and_disabling_restores_the_flat_one() {
    // No ROM is ever opened, but the app's default screen is the library
    // home, which scans ROM folders in the background and keeps
    // requesting repaints while it does -- an isolated, empty scratch
    // config dir (same convention `tests/enhance_workspace.rs` uses for a
    // test that never opens a ROM either) keeps that scan short enough
    // for `Harness::run`'s bounded step count.
    let dir = std::env::temp_dir().join(format!(
        "retroforge_mode7_ground_live_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch config dir");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();

    if !harness.state().gpu_available_for_test() {
        eprintln!(
            "SKIP enabling_mode7_ground_changes_the_live_view_and_disabling_restores_the_flat_one: \
             no wgpu adapter in this environment -- clean skip"
        );
        return;
    }

    // Nothing wanted yet: no render at all.
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), None);

    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    harness.state_mut().set_mode7_ground_for_test(true);

    let (vram, cgram) = synthetic_vram_cgram();
    let sprite_rgba = vec![0u8; WIDTH * HEIGHT * 4];

    // First injection: marks `mode7_seen`, but the row's own
    // effectiveness (and therefore `mode7_ground_wanted`) has not been
    // re-derived yet -- `RetroForgeApp::sync_diorama_subscription` only
    // runs once per repaint, exactly like the real `CoreEvent::Mode7`
    // path would need a frame to land before the setting becomes
    // effective. `refresh_diorama_render` is a no-op here.
    harness.state_mut().inject_mode7_frame_for_test(
        synthetic_frame(),
        vram.clone(),
        cgram.clone(),
        sprite_rgba.clone(),
        WIDTH,
        HEIGHT,
    );
    assert_eq!(
        harness.state().diorama_rgba_hash_for_test(),
        None,
        "the very first frame only marks mode7_seen -- the row becomes effective on the NEXT \
         repaint, same as a real CoreEvent::Mode7 frame would need"
    );

    // A repaint re-derives `mode7_ground_effective()` -- now `Available`
    // (BG mode 7 has been seen) -- and arms `mode7_ground_wanted`.
    harness.run_steps(1);

    // Second injection: now actually renders.
    harness.state_mut().inject_mode7_frame_for_test(
        synthetic_frame(),
        vram,
        cgram,
        sprite_rgba,
        WIDTH,
        HEIGHT,
    );
    let on_hash = harness.state().diorama_rgba_hash_for_test();
    assert!(
        on_hash.is_some(),
        "Enhanced mode + a live Mode 7 frame + the row enabled must produce a render"
    );

    // Turning it off: acceptance criterion 3's "off returns to flat the
    // same frame" -- one repaint (`sync_diorama_subscription` runs every
    // `eframe::App::ui` call) must already show it cleared.
    harness.state_mut().set_mode7_ground_for_test(false);
    harness.run_steps(1);
    assert_eq!(
        harness.state().diorama_rgba_hash_for_test(),
        None,
        "turning Mode 7 ground off must clear the render immediately"
    );
}
