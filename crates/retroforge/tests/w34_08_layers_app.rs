//! **The Diorama's layers and flat HUD reach the play view** (ticket W34-08,
//! D3, D4 and D7).
//!
//! No ROM is opened: the app is handed a console-free frame (picture, sprite
//! layer, one tile grid, the learner's report and the layer map the core
//! thread records) through the path a real frame takes. The picture has a
//! status bar (layer 2, the HUD), a sky (the backdrop), hills (layer 1,
//! scrolling at half the playfield's speed) and ground (layer 0).
//!
//! Proves: the HUD is the same pixels at every pitch and at every zoom (and
//! is not, with layering off: then it is pitched and cropped with the
//! picture); the planes change the picture; a frame whose layer map is not
//! the frame's size falls back to the one-plane picture. Clean skip with no
//! wgpu adapter.

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use retroforge::hud_watch::{HudInfo, LAYER_BACKDROP};
use retroforge::live_learn::{HeightsFrame, LayerInfo};
use rf_enhance::heights::{
    Class, HeightGrid, Orientation, OrientationGuess, OrientationSource, SpriteBox, Stats,
    TileCell, TileGrid,
};

const W: usize = 256;
const H: usize = 224;
const HUD_ROWS: usize = 28;

fn id_at(y: usize) -> u8 {
    match y {
        0..=27 => 2,
        28..=79 => LAYER_BACKDROP,
        80..=169 => 1,
        _ => 0,
    }
}

const COLOUR: [[u8; 3]; 4] = [[150, 110, 60], [40, 160, 70], [250, 250, 20], [0, 0, 0]];
const SKY: [u8; 3] = [90, 150, 230];

fn picture() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            let c = match id_at(y) {
                LAYER_BACKDROP => SKY,
                n => COLOUR[usize::from(n)],
            };
            // A texture so a shift shows.
            let t = ((x / 8 + y / 8) % 2) as u8 * 12;
            rgba.extend_from_slice(&[c[0].saturating_add(t), c[1], c[2], 255]);
        }
    }
    rgba
}

fn ids() -> Vec<u8> {
    (0..H)
        .flat_map(|y| std::iter::repeat_n(id_at(y), W))
        .collect()
}

fn grid() -> (TileGrid, HeightGrid) {
    let (cols, rows) = (32u16, 28u16);
    let n = usize::from(cols) * usize::from(rows);
    let g = TileGrid {
        layer: 0,
        cell_px: 8,
        hud: false,
        exact: true,
        rows_px: (0, 224),
        origin_world: (0, 0),
        offset: (0, 0),
        cols,
        rows,
        cells: vec![TileCell::default(); n],
    };
    let mut h = HeightGrid {
        cols,
        rows,
        cell_px: 8,
        offset: (0, 0),
        class: vec![Class::Unknown; n],
        depth: vec![0; n],
        confidence: vec![3; n],
    };
    for r in 22..28 {
        for c in 0..32 {
            h.class[r * 32 + c] = Class::Wall;
            h.depth[r * 32 + c] = 8;
        }
    }
    (g, h)
}

fn learned(h: HeightGrid, layer_ids: Vec<u8>) -> HeightsFrame {
    HeightsFrame {
        grids: vec![h],
        foreground: Some(0),
        orientation: OrientationGuess {
            orientation: Orientation::SideView,
            confidence: 3,
            source: OrientationSource::Gravity,
        },
        stats: Stats {
            keys: 40,
            drawn: 12,
            solid: 9,
            by_level: [20, 8, 4, 8],
        },
        backdrop_ratio: Some(0.5),
        player: None,
        frames: 1200,
        level: None,
        layers: LayerInfo {
            ids: layer_ids,
            ratios: [Some(1.0), Some(0.5), Some(0.0), None],
            hud: HudInfo {
                layers: [false, false, true, false],
                rows: Vec::new(),
            },
        },
    }
}

fn frame(harness: &mut Harness<'_, RetroForgeApp>, layer_ids: Vec<u8>) {
    let (g, h) = grid();
    let sprites = vec![SpriteBox {
        x: 100,
        y: 150,
        w: 8,
        h: 16,
        behind_bg: false,
        tile: 1,
    }];
    harness.state_mut().inject_learned_frame_for_test(
        picture(),
        vec![0u8; W * H * 4],
        vec![g],
        sprites,
        learned(h, layer_ids),
    );
}

/// The image's first `rows` rows.
fn top(harness: &Harness<'_, RetroForgeApp>, rows: usize) -> Vec<u8> {
    let (px, (w, _)) = harness.state().diorama_pixels_for_test().expect("a frame");
    px[..rows * w as usize * 4].to_vec()
}

#[test]
fn the_hud_is_flat_in_screen_space_and_the_planes_change_the_picture() {
    let dir = std::env::temp_dir().join(format!("retroforge_w34_08_{}", std::process::id()));
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
        eprintln!("SKIP layered diorama: no wgpu adapter in this environment");
        return;
    }
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    harness.state_mut().set_diorama_for_test(true);
    harness.run_steps(1);
    assert!(harness.state().diorama_wanted_for_test());

    // The HUD rows of the image: HUD_ROWS of H, less a row of margin.
    let hud_rows = HUD_ROWS - 2;
    let flat_hud = |harness: &mut Harness<'_, RetroForgeApp>, pitch, zoom, follow| {
        harness
            .state_mut()
            .set_diorama_camera_for_test(pitch, 0, zoom, follow);
        frame(harness, ids());
        top(harness, hud_rows)
    };

    // Layered: the same status bar at every pitch and zoom.
    let a = flat_hud(&mut harness, 30, 100, false);
    let b = flat_hud(&mut harness, 75, 100, false);
    let c = flat_hud(&mut harness, 40, 250, true);
    assert_eq!(a, b, "the HUD does not move with the pitch");
    assert_eq!(a, c, "the HUD is not cropped by a zoomed, following camera");
    let hud_px = &a[..4];
    assert_eq!(
        hud_px[..3],
        [COLOUR[2][0].saturating_add(0), COLOUR[2][1], COLOUR[2][2]],
        "and it is the status bar's own pixels"
    );
    // The rest of the image does move with the pitch.
    let (all_a, _) = {
        harness
            .state_mut()
            .set_diorama_camera_for_test(30, 0, 100, false);
        frame(&mut harness, ids());
        let (p, s) = harness.state().diorama_pixels_for_test().unwrap();
        (p.to_vec(), s)
    };
    let (all_b, _) = {
        harness
            .state_mut()
            .set_diorama_camera_for_test(75, 0, 100, false);
        frame(&mut harness, ids());
        let (p, s) = harness.state().diorama_pixels_for_test().unwrap();
        (p.to_vec(), s)
    };
    assert_ne!(all_a, all_b, "the scene itself is pitched");

    // Layering off: the status bar is part of the picture, pitched with it.
    harness.state_mut().set_diorama_layers_for_test(false);
    let a = flat_hud(&mut harness, 30, 100, false);
    let b = flat_hud(&mut harness, 75, 100, false);
    assert_ne!(a, b, "without layering the HUD moves with the picture");
    let one_plane = harness.state().diorama_rgba_hash_for_test().unwrap();

    // Layering on again: a different picture (planes and sky), and a layer
    // map that is not this frame's size falls back to the one plane.
    harness.state_mut().set_diorama_layers_for_test(true);
    harness
        .state_mut()
        .set_diorama_camera_for_test(75, 0, 100, false);
    frame(&mut harness, ids());
    let layered = harness.state().diorama_rgba_hash_for_test().unwrap();
    assert_ne!(layered, one_plane, "planes change the picture");
    frame(&mut harness, vec![0u8; 16]);
    assert_eq!(
        harness.state().diorama_rgba_hash_for_test(),
        Some(one_plane),
        "a layer map of the wrong size is ignored"
    );
}
