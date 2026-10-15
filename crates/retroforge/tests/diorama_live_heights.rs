//! **The live-screen diorama reaches the play view** (ticket W30-06 part A).
//!
//! A game with no level decoder but a `[heights]` table gets Diorama raised
//! from the BG tilemap on screen. No SNES ROM is opened (none ships in this
//! repo); `RetroForgeApp`'s test hooks deliver a synthetic SNES frame -- a
//! picture, a sprite layer, VRAM, the PPU registers and a BG scroll -- through
//! the same private path a real frame takes (the `tests/mode7_ground_live_view.rs`
//! shape), with a two-tile height table.
//!
//! Proves: the Diorama row becomes available from the table alone; it
//! renders something non-empty on a real `DioramaPass` (headless wgpu, clean
//! skip with no adapter); the walls track the scroll register frame to
//! frame; and Accuracy mode renders nothing at all.

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::core_thread::SnesDebugFrame;
use retroforge::game_settings::Mode;
use retroforge::live_heights::HeightTable;

const W: usize = 256;
const H: usize = 224;
/// Tilemap at word 0x0400 (BG1SC = 0x04), 32x32.
const MAP_WORDS: usize = 0x0400;

fn put_tile(vram: &mut [u8], tx: usize, ty: usize, character: u16) {
    let at = (MAP_WORDS + ty * 32 + tx) * 2;
    vram[at] = character as u8;
    vram[at + 1] = (character >> 8) as u8;
}

/// A mode 1 frame: BG1's map has tile 0x0A4 (tall) and 0x0A5 (low) placed,
/// the picture is a colour ramp, one 8x8 sprite sits at (100, 100).
fn snes_frame(scroll: (u16, u16)) -> (Vec<u8>, Vec<u8>, SnesDebugFrame) {
    let mut vram = vec![0u8; 0x10000];
    for (tx, ty, c) in [(8, 10, 0x0A4), (9, 10, 0x0A4), (12, 12, 0x0A5)] {
        put_tile(&mut vram, tx, ty, c);
    }
    let mut regs = vec![0u8; 0x40];
    regs[0x05] = 1; // BGMODE 1
    regs[0x07] = 0x04; // BG1SC: map at word 0x0400, 32x32
    let mut oam = vec![0u8; 544];
    oam[0] = 100;
    oam[1] = 100;

    let mut rgba = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            rgba.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 90, 255]);
        }
    }
    let mut sprites = vec![0u8; W * H * 4];
    for y in 100..108 {
        for x in 100..108 {
            let at = (y * W + x) * 4;
            sprites[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let snes = SnesDebugFrame {
        vram,
        cgram: vec![0u8; 512],
        oam,
        ppu_regs: regs,
        aram: Vec::new(),
        mode7: rf_snes::ppu::mode7::Mode7::default(),
        hdma_lanes: Vec::new(),
        voices: Vec::new(),
        bg_scroll: [scroll, (0, 0), (0, 0), (0, 0)],
    };
    (rgba, sprites, snes)
}

fn two_tile_table() -> HeightTable {
    HeightTable::new(1, 8, [(0x0A4, 32), (0x0A5, 8)].into_iter().collect())
}

#[test]
fn a_height_table_alone_makes_diorama_available_and_the_walls_track_scrolling() {
    let dir = std::env::temp_dir().join(format!("retroforge_live_heights_{}", std::process::id()));
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
        eprintln!("SKIP live-heights diorama: no wgpu adapter in this environment");
        return;
    }

    // Accuracy mode (the default): nothing is wanted, nothing is rendered,
    // even with a table loaded and the toggle ticked.
    harness
        .state_mut()
        .set_live_heights_for_test(two_tile_table());
    harness.state_mut().set_diorama_for_test(true);
    harness.run_steps(1);
    assert!(!harness.state().diorama_wanted_for_test());
    let (rgba, sprites, snes) = snes_frame((0, 0));
    harness
        .state_mut()
        .inject_snes_live_frame_for_test(rgba, sprites, snes);
    assert_eq!(
        harness.state().diorama_rgba_hash_for_test(),
        None,
        "Accuracy mode must not render a diorama"
    );

    // Game-Aware + the table: the row becomes available with no decoder.
    harness.state_mut().set_mode_for_test(Mode::GameAware);
    harness.run_steps(1);
    assert!(
        harness.state().diorama_wanted_for_test(),
        "a [heights] table must be enough to make Diorama available"
    );
    assert!(!harness.state().has_decoded_level_for_test());

    let (rgba, sprites, snes) = snes_frame((0, 0));
    harness
        .state_mut()
        .inject_snes_live_frame_for_test(rgba, sprites, snes);
    let still = harness
        .state()
        .diorama_rgba_hash_for_test()
        .expect("a live SNES frame must produce a render");
    assert!(
        harness
            .state()
            .diorama_opaque_pixels_for_test()
            .is_some_and(|n| n > 1000),
        "the render must be a picture, not an empty frame"
    );

    // Same frame again: identical (a pure function of the frame).
    let (rgba, sprites, snes) = snes_frame((0, 0));
    harness
        .state_mut()
        .inject_snes_live_frame_for_test(rgba, sprites, snes);
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), Some(still));

    // Scrolled (by whole and by partial tiles): the walls move with it.
    let mut seen = vec![still];
    for scroll in [(8, 0), (3, 0), (3, 5)] {
        let (rgba, sprites, snes) = snes_frame(scroll);
        harness
            .state_mut()
            .inject_snes_live_frame_for_test(rgba, sprites, snes);
        let hash = harness
            .state()
            .diorama_rgba_hash_for_test()
            .expect("still rendering after a scroll");
        assert!(
            !seen.contains(&hash),
            "scroll {scroll:?} must change the picture"
        );
        seen.push(hash);
    }

    // A table that names no tile on screen leaves flat ground: still a
    // render, but not the walled one.
    harness
        .state_mut()
        .set_live_heights_for_test(HeightTable::new(1, 8, [(0x3FF, 64)].into_iter().collect()));
    let (rgba, sprites, snes) = snes_frame((0, 0));
    harness
        .state_mut()
        .inject_snes_live_frame_for_test(rgba, sprites, snes);
    let flat = harness
        .state()
        .diorama_rgba_hash_for_test()
        .expect("flat ground");
    assert_ne!(flat, still, "walls must come from the table");

    // Back to Accuracy: cleared the same repaint.
    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    harness.run_steps(1);
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), None);
}
