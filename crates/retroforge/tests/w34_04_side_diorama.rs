//! **The learned diorama reaches the play view** (ticket W34-04).
//!
//! No ROM is opened and no console is involved: the app is handed a
//! console-free frame -- a picture, a sprite layer, one tile grid, a sprite
//! box and what the height learner reported (`inject_learned_frame_for_test`)
//! -- through the path a real NES or SNES frame takes.
//!
//! Proves: the Diorama row needs only Enhanced mode (D-023: no profile, no
//! painted table), Accuracy mode renders nothing and captures nothing; a
//! side-view game renders upright (a different picture from the same grid
//! read top-down); an orientation nobody knows yet leaves the flat view with
//! a readout that says why; and the readout reports what was learned.
//! Clean skip with no wgpu adapter.

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use retroforge::live_learn::HeightsFrame;
use rf_enhance::heights::{
    Class, HeightGrid, Orientation, OrientationGuess, OrientationSource, SpriteBox, Stats,
    TileCell, TileGrid,
};

const W: usize = 256;
const H: usize = 224;

fn picture() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            rgba.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 90, 255]);
        }
    }
    rgba
}

fn sprite_layer() -> Vec<u8> {
    let mut s = vec![0u8; W * H * 4];
    for y in 150..166 {
        for x in 100..108 {
            let at = (y * W + x) * 4;
            s[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    s
}

/// A 32 x 28 grid of 8 px cells whose bottom three rows are ground and one
/// pillar rises from it.
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
    let at = |c: usize, r: usize| r * usize::from(cols) + c;
    for r in 25..28 {
        for c in 0..32 {
            h.class[at(c, r)] = Class::Wall;
            h.depth[at(c, r)] = 8;
        }
    }
    for r in 21..25 {
        h.class[at(16, r)] = Class::Wall;
        h.depth[at(16, r)] = 8;
    }
    (g, h)
}

fn learned(orientation: Orientation, grid: HeightGrid) -> HeightsFrame {
    HeightsFrame {
        grids: vec![grid],
        foreground: Some(0),
        orientation: OrientationGuess {
            orientation,
            confidence: if orientation == Orientation::Unknown {
                0
            } else {
                3
            },
            source: OrientationSource::Gravity,
        },
        stats: Stats {
            keys: 40,
            drawn: 12,
            solid: 9,
            by_level: [20, 8, 4, 8],
        },
        backdrop_ratio: None,
        player: None,
        frames: 1200,
        level: None,
    }
}

fn frame(harness: &mut Harness<'_, RetroForgeApp>, o: Orientation) {
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
        sprite_layer(),
        vec![g],
        sprites,
        learned(o, h),
    );
}

#[test]
fn learned_heights_stand_a_side_view_level_upright_in_enhanced_mode() {
    let dir = std::env::temp_dir().join(format!("retroforge_w34_04_{}", std::process::id()));
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
        eprintln!("SKIP learned diorama: no wgpu adapter in this environment");
        return;
    }

    // Accuracy (the default): nothing wanted, nothing rendered, even ticked.
    harness.state_mut().set_diorama_for_test(true);
    harness.run_steps(1);
    assert!(!harness.state().diorama_wanted_for_test());
    frame(&mut harness, Orientation::SideView);
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), None);

    // Enhanced, no profile, no painted table: Diorama is available (D-023).
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    harness.run_steps(1);
    assert!(
        harness.state().diorama_wanted_for_test(),
        "Enhanced mode alone must make Diorama available"
    );
    assert_eq!(harness.state().hd2d_unavailable_for_test(), None);

    // Side view: renders, with a picture's worth of pixels.
    frame(&mut harness, Orientation::SideView);
    let side = harness
        .state()
        .diorama_rgba_hash_for_test()
        .expect("a side-view frame must render");
    assert!(harness
        .state()
        .diorama_opaque_pixels_for_test()
        .is_some_and(|n| n > 20_000));
    let note = harness
        .state()
        .heights_numbers_for_test()
        .expect("a report");
    assert_eq!(note.0, "Side");
    assert!(note.1 > 0, "the ground and the pillar are raised: {note:?}");
    let readout = harness.state().heights_readout_for_test().expect("readout");
    assert!(
        readout.contains("tiles learned") && readout.contains("side view"),
        "{readout}"
    );

    // The same frame again is the same picture.
    frame(&mut harness, Orientation::SideView);
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), Some(side));

    // Read top down the same grid is a different picture (the floor plan).
    frame(&mut harness, Orientation::TopDown);
    let top = harness
        .state()
        .diorama_rgba_hash_for_test()
        .expect("a top-down frame must render");
    assert_ne!(top, side, "orientation must change how the grid is drawn");

    // Nobody knows which way the game faces yet: the flat picture stays, and
    // the readout says so instead of guessing.
    frame(&mut harness, Orientation::Unknown);
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), None);
    let readout = harness.state().heights_readout_for_test().expect("readout");
    assert!(readout.contains("flat until"), "{readout}");

    // Back to Accuracy: cleared the same repaint.
    frame(&mut harness, Orientation::SideView);
    assert!(harness.state().diorama_rgba_hash_for_test().is_some());
    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    harness.run_steps(1);
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), None);
}
