//! Headless GPU tests for the adjustable diorama camera (ticket W30-05):
//! sprites stand upright as billboards under a tilted camera, and the
//! default camera still renders exactly what `render` always did.

use rf_renderer::diorama::{vertices_for_camera, DioramaCamera, DioramaPass};
use rf_renderer::diorama_mesh::{build_vertices, Billboard, DioramaScene};
use rf_renderer::gpu::GpuContext;

const TILE_PX: u32 = 16;
const TILES: u32 = 6;
const OUT: u32 = 128;

fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
    match GpuContext::request_headless() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            if std::env::var_os("CI").is_some() {
                panic!("{test_name} cannot skip in CI: {e}");
            }
            eprintln!("SKIP {test_name}: no wgpu adapter in this environment ({e})");
            None
        }
    }
}

/// Blue-grey floor: nothing on it reads as the pure-red sprite.
fn ground() -> Vec<u8> {
    let n = (TILES * TILE_PX) as usize;
    [30u8, 50, 90, 255].repeat(n * n)
}

fn sprite_texture() -> Vec<u8> {
    [255u8, 0, 0, 255].repeat(4) // 2x2 opaque red
}

fn scene_with_sprite<'a>(solid: &'a [u8], depth: &'a [u8], b: &'a [Billboard]) -> DioramaScene<'a> {
    DioramaScene {
        tiles_w: TILES,
        tiles_h: TILES,
        tile_px: TILE_PX as f32,
        solid,
        depth,
        billboards: b,
    }
}

fn the_sprite() -> [Billboard; 1] {
    [Billboard {
        center_x_px: 48.0,
        center_z_px: 48.0,
        width_px: 12.0,
        height_px: 24.0,
        uv: [0.0, 0.0, 1.0, 1.0],
    }]
}

fn render(pass: &DioramaPass, gpu: &GpuContext, camera: &DioramaCamera) -> Vec<u8> {
    let (solid, depth) = (vec![0u8; 36], vec![0u8; 36]);
    let bb = the_sprite();
    let scene = scene_with_sprite(&solid, &depth, &bb);
    let verts = vertices_for_camera(&scene, OUT, OUT, camera);
    pass.render_camera(
        gpu,
        &verts,
        &ground(),
        TILES * TILE_PX,
        TILES * TILE_PX,
        &sprite_texture(),
        2,
        2,
        TILES,
        TILES,
        TILE_PX,
        OUT,
        OUT,
        camera,
    )
    .expect("renders")
}

/// Bounding box `(min_x, min_y, max_x, max_y)` of the pure-red pixels.
fn red_bounds(rgba: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        if px[0] > 200 && px[1] < 60 && px[2] < 60 && px[3] > 200 {
            let (x, y) = (i as u32 % OUT, i as u32 / OUT);
            b = Some(match b {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    b
}

fn cam(pitch_deg: f32) -> DioramaCamera {
    DioramaCamera {
        pitch_deg,
        ..DioramaCamera::default()
    }
}

#[test]
fn a_standing_sprite_is_visible_and_taller_at_a_low_pitch_than_a_high_one() {
    let Some(gpu) = gpu_or_skip("a_standing_sprite_is_visible_and_taller_at_a_low_pitch") else {
        return;
    };
    let pass = DioramaPass::new(&gpu);
    let height_at = |pitch: f32| {
        let (_, y0, _, y1) = red_bounds(&render(&pass, &gpu, &cam(pitch)))
            .unwrap_or_else(|| panic!("the sprite must be visible at pitch {pitch}"));
        y1 - y0 + 1
    };
    let (low, high) = (height_at(30.0), height_at(75.0));
    assert!(
        low > high,
        "an upright sprite looks taller from a low camera: {low}px at 30 vs {high}px at 75"
    );
    // Upright, not flat: even from nearly overhead it keeps some height,
    // and from a low camera it is clearly taller than it is deep on the floor.
    assert!(low >= 8, "30 degrees: {low}px");
}

#[test]
fn a_yawed_follow_zoom_camera_still_shows_the_sprite() {
    let Some(gpu) = gpu_or_skip("a_yawed_follow_zoom_camera_still_shows_the_sprite") else {
        return;
    };
    let pass = DioramaPass::new(&gpu);
    let camera = DioramaCamera {
        pitch_deg: 45.0,
        yaw_deg: 25.0,
        zoom: 2.5,
        follow: Some((48.0, 48.0)),
    };
    let rgba = render(&pass, &gpu, &camera);
    let (x0, y0, x1, y1) = red_bounds(&rgba).expect("the followed sprite is in view");
    // Followed point sits at the screen centre, so the sprite straddles it.
    assert!(x0 <= OUT / 2 && x1 >= OUT / 2 - 1, "x {x0}..{x1}");
    assert!(y0 <= OUT / 2 + 8 && y1 + 8 >= OUT / 2, "y {y0}..{y1}");
    // Zoomed in: bigger on screen than the whole-level default view.
    let default_h = {
        let (_, a, _, b) = red_bounds(&render(&pass, &gpu, &DioramaCamera::default())).unwrap();
        b - a + 1
    };
    assert!(y1 - y0 + 1 > default_h);
}

#[test]
fn the_default_camera_renders_exactly_what_render_always_did() {
    let Some(gpu) = gpu_or_skip("the_default_camera_renders_exactly_what_render_always_did") else {
        return;
    };
    let pass = DioramaPass::new(&gpu);
    let (solid, depth) = (vec![0u8; 36], vec![0u8; 36]);
    let bb = the_sprite();
    let scene = scene_with_sprite(&solid, &depth, &bb);
    let legacy = pass
        .render(
            &gpu,
            &build_vertices(&scene),
            &ground(),
            TILES * TILE_PX,
            TILES * TILE_PX,
            &sprite_texture(),
            2,
            2,
            TILES,
            TILES,
            TILE_PX,
            OUT,
            OUT,
        )
        .unwrap();
    assert_eq!(legacy, render(&pass, &gpu, &DioramaCamera::default()));
}
