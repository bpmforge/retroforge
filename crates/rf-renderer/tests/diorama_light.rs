//! W34-06: the sun, Lambert faces and the shadow map in the Diorama pass.
//! One tall box on a flat floor, seen by the default camera; the sun's
//! position comes from the lens's time of day.

use rf_renderer::diorama::{camera_matrices, vertices_for_camera, DioramaCamera, DioramaPass};
use rf_renderer::diorama_mesh::{Billboard, DioramaScene};
use rf_renderer::gpu::GpuContext;
use rf_renderer::lens::Weather;
use rf_renderer::lighting::{LightView, Lighting, Sun};

const COLS: u32 = 24;
const TILE: u32 = 16;
const W: u32 = 512;
const H: u32 = 448;

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

struct Fixture {
    solid: Vec<u8>,
    depth: Vec<u8>,
    billboards: Vec<Billboard>,
    ground: Vec<u8>,
    sprite: Vec<u8>,
    sprite_size: (u32, u32),
}

/// A 24 x 24 grid of 16 px tiles, light grey, with a 2 x 2 box (4 eighths of
/// a tile per eighth: `box_eighths` tall) in the middle.
fn fixture(box_eighths: u8, billboard: bool, sprite_half_clear: bool) -> Fixture {
    let n = (COLS * COLS) as usize;
    let mut solid = vec![0u8; n];
    let mut depth = vec![0u8; n];
    if box_eighths > 0 {
        for (r, c) in [(11, 11), (11, 12), (12, 11), (12, 12)] {
            solid[(r * COLS + c) as usize] = 1;
            depth[(r * COLS + c) as usize] = box_eighths;
        }
    }
    let side = COLS * TILE;
    let ground = [200u8, 200, 200, 255].repeat((side * side) as usize);
    let (sw, sh) = (16u32, 16u32);
    let mut sprite = vec![0u8; (sw * sh * 4) as usize];
    for y in 0..sh {
        for x in 0..sw {
            let i = ((y * sw + x) * 4) as usize;
            let opaque = !(sprite_half_clear && x >= sw / 2);
            sprite[i..i + 4].copy_from_slice(&[220, 60, 60, if opaque { 255 } else { 0 }]);
        }
    }
    let billboards = if billboard {
        vec![Billboard {
            center_x_px: 192.0,
            center_z_px: 192.0,
            width_px: 16.0,
            height_px: 48.0,
            uv: [0.0, 0.0, 1.0, 1.0],
        }]
    } else {
        Vec::new()
    };
    Fixture {
        solid,
        depth,
        billboards,
        ground,
        sprite,
        sprite_size: (sw, sh),
    }
}

fn render(gpu: &GpuContext, pass: &DioramaPass, f: &Fixture) -> Vec<u8> {
    let scene = DioramaScene {
        tiles_w: COLS,
        tiles_h: COLS,
        tile_px: TILE as f32,
        solid: &f.solid,
        depth: &f.depth,
        billboards: &f.billboards,
    };
    let camera = DioramaCamera::default();
    let v = vertices_for_camera(&scene, W, H, &camera);
    pass.render_camera(
        gpu,
        &v,
        &f.ground,
        COLS * TILE,
        COLS * TILE,
        &f.sprite,
        f.sprite_size.0,
        f.sprite_size.1,
        COLS,
        COLS,
        TILE,
        W,
        H,
        &camera,
    )
    .expect("renders")
}

fn lum(px: &[u8]) -> f32 {
    0.299 * f32::from(px[0]) + 0.587 * f32::from(px[1]) + 0.114 * f32::from(px[2])
}

/// Where world `(x, 0, z)` lands in the `W x H` image under the default camera.
fn project(x: f32, z: f32) -> (f32, f32) {
    let vp = camera_matrices::view_proj_camera(
        COLS as f32,
        COLS as f32,
        TILE as f32,
        W as f32 / H as f32,
        rf_renderer::diorama::FOV_Y_DEG,
        rf_renderer::diorama::DISTANCE_FACTOR,
        &DioramaCamera::default(),
    );
    let c = |r: usize| vp[r] * x + vp[8 + r] * z + vp[12 + r];
    let (cx, cy, cw) = (c(0), c(1), c(3));
    (
        (cx / cw * 0.5 + 0.5) * W as f32,
        (0.5 - cy / cw * 0.5) * H as f32,
    )
}

fn lit_at(t: f32, shadow_map: bool) -> Lighting {
    let mut l = Lighting::new(Sun::for_time(t, Weather::None, LightView::TopDown));
    l.shadow_map = shadow_map;
    l
}

/// Pixels the shadow map darkened (same sun, map off versus on), as
/// `(count, centroid x, centroid y)`.
fn darkened(gpu: &GpuContext, f: &Fixture, t: f32) -> (usize, f32, f32) {
    let pass = DioramaPass::new(gpu);
    pass.set_lighting(Some(lit_at(t, false)));
    let without = render(gpu, &pass, f);
    pass.set_lighting(Some(lit_at(t, true)));
    let with = render(gpu, &pass, f);
    assert!(pass.shadow_map_used(), "the map was drawn");
    let (mut n, mut sx, mut sy) = (0usize, 0.0f64, 0.0f64);
    for (i, (a, b)) in without
        .chunks_exact(4)
        .zip(with.chunks_exact(4))
        .enumerate()
    {
        if a[3] == 255 && b[3] == 255 && lum(a) - lum(b) > 12.0 {
            n += 1;
            sx += (i as u32 % W) as f64;
            sy += (i as u32 / W) as f64;
        }
    }
    if n == 0 {
        return (0, 0.0, 0.0);
    }
    (n, (sx / n as f64) as f32, (sy / n as f64) as f32)
}

#[test]
fn level_ground_at_noon_is_as_bright_as_the_unlit_picture() {
    let Some(gpu) = gpu_or_skip("level_ground_at_noon") else {
        return;
    };
    let f = fixture(0, false, false);
    let pass = DioramaPass::new(&gpu);
    let unlit = render(&gpu, &pass, &f);
    pass.set_lighting(Some(lit_at(0.5, true)));
    let lit = render(&gpu, &pass, &f);
    let (x, y) = (W as usize / 2, H as usize / 2);
    let i = (y * W as usize + x) * 4;
    // This pins the shader's NORMAL_SIGN: a flipped normal reads the floor as
    // facing away from the sun and falls to the ambient 0.45.
    assert!(
        lum(&lit[i..i + 4]) > 0.93 * lum(&unlit[i..i + 4]),
        "floor lit {:?} vs unlit {:?}",
        &lit[i..i + 4],
        &unlit[i..i + 4]
    );
}

#[test]
fn removing_the_sun_returns_the_original_picture_exactly() {
    let Some(gpu) = gpu_or_skip("removing_the_sun") else {
        return;
    };
    let f = fixture(16, true, false);
    let pass = DioramaPass::new(&gpu);
    let before = render(&gpu, &pass, &f);
    pass.set_lighting(Some(lit_at(0.3, true)));
    let lit = render(&gpu, &pass, &f);
    assert_ne!(before, lit, "the sun changes the picture");
    pass.set_lighting(None);
    assert_eq!(before, render(&gpu, &pass, &f), "no sun, the old pixels");
}

#[test]
fn the_shadow_moves_with_the_sun() {
    let Some(gpu) = gpu_or_skip("the_shadow_moves_with_the_sun") else {
        return;
    };
    // A 2 tile (32 px) tall box.
    let f = fixture(16, false, false);
    let base = project(192.0, 192.0);
    let mut offsets = Vec::new();
    for t in [0.3f32, 0.5, 0.8] {
        let (n, cx, cy) = darkened(&gpu, &f, t);
        assert!(n > 50, "t={t}: a shadow of {n} px");
        let (dx, dy) = (cx - base.0, cy - base.1);
        println!("t={t}: {n} px, centroid offset ({dx:.1}, {dy:.1}) from the box foot");
        offsets.push((dx, dy));
    }
    let len = |o: (f32, f32)| (o.0 * o.0 + o.1 * o.1).sqrt();
    // Morning sun is screen-right, so its shadow falls screen-left; evening
    // is the mirror image.
    assert!(
        offsets[0].0 < -5.0,
        "morning shadow falls left: {offsets:?}"
    );
    assert!(
        offsets[2].0 > 5.0,
        "evening shadow falls right: {offsets:?}"
    );
    assert!(offsets[1].0.abs() < offsets[0].0.abs());
    // And it is shortest at noon, longest at dusk.
    assert!(len(offsets[1]) < len(offsets[0]), "{offsets:?}");
    assert!(len(offsets[0]) < len(offsets[2]), "{offsets:?}");
    // All three fall away from the sun, down the screen (the sun is behind
    // the picture's far edge in a top-down view).
    for o in &offsets {
        assert!(o.1 > 0.0, "shadows fall toward the viewer: {offsets:?}");
    }
}

#[test]
fn a_billboard_casts_by_its_silhouette() {
    let Some(gpu) = gpu_or_skip("a_billboard_casts_by_its_silhouette") else {
        return;
    };
    let solid = fixture(0, true, false);
    let half = fixture(0, true, true);
    let (n_solid, ..) = darkened(&gpu, &solid, 0.3);
    let (n_half, ..) = darkened(&gpu, &half, 0.3);
    println!("billboard shadow px: opaque {n_solid}, half cleared {n_half}");
    assert!(n_solid > 50, "the sprite casts: {n_solid}");
    assert!(
        (n_half as f32) < 0.75 * n_solid as f32,
        "clearing half the sprite shrinks its shadow: {n_half} vs {n_solid}"
    );
}

#[test]
fn the_budget_fallback_keeps_lambert_and_drops_the_map() {
    let Some(gpu) = gpu_or_skip("the_budget_fallback") else {
        return;
    };
    let f = fixture(16, false, false);
    let pass = DioramaPass::new(&gpu);
    pass.set_lighting(Some(lit_at(0.3, true)));
    let with_map = render(&gpu, &pass, &f);
    assert!(pass.shadow_map_used());
    for _ in 0..200 {
        pass.record_shadow_sample_ms(50.0);
    }
    assert!(!pass.shadow_budget_allows_map());
    let fallback = render(&gpu, &pass, &f);
    assert!(!pass.shadow_map_used(), "over budget: no map");
    assert_ne!(with_map, fallback, "the shadow is gone");
    pass.set_lighting(Some(lit_at(0.3, false)));
    pass.reset_shadow_budget();
    assert_eq!(
        fallback,
        render(&gpu, &pass, &f),
        "same picture as a sun that asks for no map"
    );
    pass.set_lighting(Some(lit_at(0.3, true)));
    assert_eq!(with_map, render(&gpu, &pass, &f), "the map comes back");
}

#[test]
fn lit_frames_are_deterministic() {
    let Some(gpu) = gpu_or_skip("lit_frames_are_deterministic") else {
        return;
    };
    let f = fixture(16, true, false);
    let pass = DioramaPass::new(&gpu);
    pass.set_lighting(Some(lit_at(0.3, true)));
    assert_eq!(render(&gpu, &pass, &f), render(&gpu, &pass, &f));
    let other = DioramaPass::new(&gpu);
    other.set_lighting(Some(lit_at(0.3, true)));
    assert_eq!(render(&gpu, &pass, &f), render(&gpu, &other, &f));
}

#[test]
fn a_steady_lit_scene_creates_nothing_after_the_first_frame() {
    let Some(gpu) = gpu_or_skip("a_steady_lit_scene") else {
        return;
    };
    let f = fixture(16, true, false);
    let pass = DioramaPass::new(&gpu);
    pass.set_lighting(Some(lit_at(0.3, true)));
    render(&gpu, &pass, &f);
    let first = pass.resource_counts();
    render(&gpu, &pass, &f);
    pass.set_lighting(Some(lit_at(0.4, true)));
    render(&gpu, &pass, &f);
    assert_eq!(pass.resource_counts(), first, "the sun moving reuses all");
}

#[test]
fn the_double_buffered_readback_lags_exactly_one_frame() {
    let Some(gpu) = gpu_or_skip("the_double_buffered_readback") else {
        return;
    };
    let (a, b, c) = (
        fixture(16, false, false),
        fixture(0, true, false),
        fixture(32, true, false),
    );
    let reference = DioramaPass::new(&gpu);
    reference.set_lighting(Some(lit_at(0.3, true)));
    let (ra, rb, rc) = (
        render(&gpu, &reference, &a),
        render(&gpu, &reference, &b),
        render(&gpu, &reference, &c),
    );
    assert_ne!(ra, rb);
    let pass = DioramaPass::new(&gpu);
    pass.set_lighting(Some(lit_at(0.3, true)));
    pass.set_async_readback(true);
    assert_eq!(render(&gpu, &pass, &a), ra, "first call: its own frame");
    assert_eq!(render(&gpu, &pass, &b), ra, "then the previous frame");
    assert_eq!(render(&gpu, &pass, &c), rb);
    assert_ne!(rc, rb, "the frames differ, so the lag is observable");
    // Back to synchronous: the frame asked for, nothing left mapped.
    pass.set_async_readback(false);
    assert_eq!(render(&gpu, &pass, &a), ra);
    assert_eq!(render(&gpu, &pass, &b), rb);
}
