//! W39-02: object-aware Diorama. Depth and shadow come from a per-sprite
//! class when a recipe supplies one; with none the mesh is bit-identical to
//! W34's, and the Original pipeline never sees any of it.
//!
//! The scene is synthetic (flat-coloured sprites on a checker floor): no ROM
//! content. Set `RF_W39_SHEET_DIR=<dir>` to also write the contact sheet
//! (`docs/screens/w39` is where it is committed).

use rf_renderer::diorama::{vertices_for_camera_classed, DioramaCamera, DioramaPass};
use rf_renderer::diorama_mesh::{
    build_vertices, build_vertices_classed, build_vertices_oriented,
    build_vertices_oriented_classed, kind, Billboard, DioramaScene,
};
use rf_renderer::diorama_objects::ObjectClass as C;
use rf_renderer::gpu::GpuContext;
use rf_renderer::lens::Weather;
use rf_renderer::lighting::{LightView, Lighting, Sun};
use rf_renderer::original_pipeline::{IndexedFrame, PalettePass};

const COLS: u32 = 12;
const ROWS: u32 = 9;
const TILE: u32 = 16;
const W: u32 = 480;
const H: u32 = 360;

fn gpu_or_skip(name: &str) -> Option<GpuContext> {
    match GpuContext::request_headless() {
        Ok(g) => Some(g),
        Err(e) => {
            if std::env::var_os("CI").is_some() {
                panic!("{name} cannot skip in CI: {e}");
            }
            eprintln!("SKIP {name}: no wgpu adapter ({e})");
            None
        }
    }
}

const CLASSES: [C; 5] = [C::Player, C::Enemy, C::Projectile, C::Pickup, C::Scenery];

struct Scene {
    solid: Vec<u8>,
    depth: Vec<u8>,
    billboards: Vec<Billboard>,
    ground: Vec<u8>,
    sprite: Vec<u8>,
}

/// Five 16x16 sprites side by side in an 80x16 atlas, one colour per class.
fn scene() -> Scene {
    let n = (COLS * ROWS) as usize;
    let mut solid = vec![0u8; n];
    let mut depth = vec![0u8; n];
    for (r, c) in [(1, 2), (1, 3), (2, 2), (2, 3), (7, 9), (7, 10)] {
        solid[(r * COLS + c) as usize] = 1;
        depth[(r * COLS + c) as usize] = 6;
    }
    let (gw, gh) = ((COLS * TILE) as usize, (ROWS * TILE) as usize);
    let mut ground = Vec::with_capacity(gw * gh * 4);
    for y in 0..gh {
        for x in 0..gw {
            let v = if ((x / 16) + (y / 16)) % 2 == 0 {
                190
            } else {
                150
            };
            ground.extend_from_slice(&[v, v + 10, v, 255]);
        }
    }
    let cols = [
        [60u8, 90, 230],
        [220, 50, 50],
        [250, 220, 40],
        [60, 200, 90],
        [140, 100, 60],
    ];
    let mut sprite = vec![0u8; 80 * 16 * 4];
    for (k, col) in cols.iter().enumerate() {
        for y in 0..16usize {
            for x in 0..16usize {
                // A rounded blob so the shape reads against the floor.
                let (dx, dy) = (x as f32 - 7.5, y as f32 - 7.5);
                if dx * dx + dy * dy <= 64.0 {
                    let i = (y * 80 + k * 16 + x) * 4;
                    sprite[i..i + 4].copy_from_slice(&[col[0], col[1], col[2], 255]);
                }
            }
        }
    }
    // A row of five sprites on open floor, a row apart so none hides another.
    let billboards = (0..5)
        .map(|k| Billboard {
            center_x_px: 28.0 + 34.0 * k as f32,
            center_z_px: 84.0 + 4.0 * k as f32,
            width_px: 20.0,
            height_px: 20.0 + 6.0 * (k == 0) as u32 as f32,
            uv: [k as f32 / 5.0, 0.0, (k + 1) as f32 / 5.0, 1.0],
        })
        .collect();
    Scene {
        solid,
        depth,
        billboards,
        ground,
        sprite,
    }
}

fn dscene(s: &Scene) -> DioramaScene<'_> {
    DioramaScene {
        tiles_w: COLS,
        tiles_h: ROWS,
        tile_px: TILE as f32,
        solid: &s.solid,
        depth: &s.depth,
        billboards: &s.billboards,
    }
}

fn render(
    gpu: &GpuContext,
    s: &Scene,
    classes: &[C],
    sun: Option<f32>,
    cam: &DioramaCamera,
) -> Vec<u8> {
    let pass = DioramaPass::new(gpu);
    if let Some(t) = sun {
        pass.set_lighting(Some(Lighting::new(Sun::for_time(
            t,
            Weather::None,
            LightView::TopDown,
        ))));
    }
    let v = vertices_for_camera_classed(&dscene(s), W, H, cam, classes);
    pass.render_camera(
        gpu,
        &v,
        &s.ground,
        COLS * TILE,
        ROWS * TILE,
        &s.sprite,
        80,
        16,
        COLS,
        ROWS,
        TILE,
        W,
        H,
        cam,
    )
    .expect("renders")
}

#[test]
fn no_classes_is_bit_identical_to_the_unclassed_mesh() {
    let s = scene();
    let sc = dscene(&s);
    let plain = build_vertices(&sc);
    assert_eq!(plain, build_vertices_classed(&sc, &[]));
    assert_eq!(plain, build_vertices_classed(&sc, &[C::Unknown; 5]));
    // An enemy keeps the legacy look on purpose.
    assert_eq!(plain, build_vertices_classed(&sc, &[C::Enemy; 5]));
    let o = build_vertices_oriented(&sc, 12.0, [96.0, 300.0]);
    assert_eq!(
        o,
        build_vertices_oriented_classed(&sc, 12.0, [96.0, 300.0], &[])
    );
}

#[test]
fn classes_change_depth_and_shadow_per_sprite() {
    let s = scene();
    let sc = dscene(&s);
    let plain = build_vertices(&sc);
    let classed = build_vertices_classed(&sc, &CLASSES);
    assert_ne!(plain, classed);
    // Scenery casts no shadow: one shadow quad (6 vertices) fewer.
    let shadows = |v: &[rf_renderer::diorama_mesh::Vertex]| {
        v.iter().filter(|x| x.kind == kind::SHADOW).count()
    };
    assert_eq!(shadows(&plain), 30);
    assert_eq!(shadows(&classed), 24);
    // The projectile (index 2) floats: its sprite's lowest vertex is lifted.
    let sprites: Vec<&rf_renderer::diorama_mesh::Vertex> =
        classed.iter().filter(|x| x.kind == kind::SPRITE).collect();
    assert_eq!(sprites.len(), 30);
    let floor_y = |k: usize| {
        sprites[k * 6..k * 6 + 6]
            .iter()
            .map(|v| v.pos[1])
            .fold(f32::MAX, f32::min)
    };
    assert_eq!(floor_y(1), 0.0, "an enemy stands on the floor");
    assert!(floor_y(2) > 0.0, "a projectile floats");
    assert!(
        floor_y(3) > 0.0 && floor_y(3) < floor_y(2),
        "a pickup hovers lower"
    );
}

#[test]
fn an_unclassed_tail_stays_legacy() {
    let s = scene();
    let sc = dscene(&s);
    let all = build_vertices_classed(&sc, &CLASSES);
    // Classes for the first two only: the rest are Unknown (legacy).
    let part = build_vertices_classed(&sc, &CLASSES[..2]);
    let plain = build_vertices(&sc);
    assert_ne!(part, plain, "the player is styled");
    assert_ne!(part, all);
}

#[test]
fn the_classed_builders_read_the_scene_and_nothing_else() {
    // The inputs are shared slices and the output a fresh Vec: a classed build
    // cannot reach the accuracy buffers or the simulation. Pin the part we can
    // observe: the scene is unchanged after a classed build.
    let s = scene();
    let before = (s.solid.clone(), s.depth.clone(), s.billboards.clone());
    let _ = build_vertices_classed(&dscene(&s), &CLASSES);
    assert_eq!((s.solid, s.depth, s.billboards), before);
}

/// Original mode: the palette pass output is the same bytes whether or not a
/// classed Diorama frame was built and rendered around it.
#[test]
fn original_pipeline_output_is_byte_identical_around_a_classed_diorama() {
    let Some(gpu) = gpu_or_skip("original_pipeline_output_is_byte_identical") else {
        return;
    };
    let idx: Vec<u8> = (0..64u32 * 48).map(|i| (i * 7 % 64) as u8).collect();
    let frame = IndexedFrame::new(64, 48, idx);
    let pal = PalettePass::new(&gpu);
    let before = pal.render(&gpu, &frame).expect("palette");
    let s = scene();
    let _ = render(&gpu, &s, &CLASSES, Some(0.3), &DioramaCamera::default());
    let after = pal.render(&gpu, &frame).expect("palette");
    assert_eq!(before, after);
}

#[test]
fn rendering_differs_with_classes_and_matches_without() {
    let Some(gpu) = gpu_or_skip("rendering_differs_with_classes") else {
        return;
    };
    let s = scene();
    let cam = DioramaCamera::default();
    let plain = render(&gpu, &s, &[], Some(0.3), &cam);
    let unknown = render(&gpu, &s, &[C::Unknown; 5], Some(0.3), &cam);
    let classed = render(&gpu, &s, &CLASSES, Some(0.3), &cam);
    assert_eq!(plain, unknown);
    assert_ne!(plain, classed);
}

/// Writes the contact sheet when `RF_W39_SHEET_DIR` is set: the default
/// camera; columns are "no recipe" and
/// "classes from the recipe", both under a morning sun.
#[test]
fn contact_sheet() {
    let Some(dir) = std::env::var_os("RF_W39_SHEET_DIR") else {
        return;
    };
    let Some(gpu) = gpu_or_skip("contact_sheet") else {
        return;
    };
    let s = scene();
    let cams = [DioramaCamera::default()];
    let (sw, sh) = (W * 2, H * cams.len() as u32);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (r, cam) in cams.iter().enumerate() {
        for (c, classes) in [&[][..], &CLASSES[..]].into_iter().enumerate() {
            let img = render(&gpu, &s, classes, Some(0.3), cam);
            for y in 0..H as usize {
                let dst = ((r * H as usize + y) * sw as usize + c * W as usize) * 4;
                let src = y * W as usize * 4;
                sheet[dst..dst + W as usize * 4].copy_from_slice(&img[src..src + W as usize * 4]);
            }
        }
    }
    let png = rf_renderer::png::encode_rgba(&sheet, sw, sh);
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("contact-sheet.png"), png).unwrap();
}
