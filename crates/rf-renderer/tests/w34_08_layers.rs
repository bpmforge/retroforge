//! W34-08: planes at their own depths (D4) and sprites that stand behind the
//! background (D3), in the side-view Diorama pass.
//!
//! A 64 x 64 picture with an atlas of picture-sized slots: slot 0 is the
//! plane the blocks grow from, the others are planes standing further back.
//! Colours are chosen so each plane and each sprite can be found in the
//! image by colour alone.

use rf_renderer::diorama::{camera_matrices, DioramaCamera, DioramaPass, WINDOW_FOV_Y_DEG};
use rf_renderer::diorama_side::{build_vertices_side, Atlas, BackPlane, SideBillboard, SideScene};
use rf_renderer::gpu::GpuContext;
use rf_renderer::lens::Weather;
use rf_renderer::lighting::{LightView, Lighting, Sun};

const PIC: u32 = 64;
const GAP: u32 = 2;
const PITCH: u32 = PIC + GAP;
const OUT: (u32, u32) = (256, 256);

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

/// An atlas of `slots` pictures, all transparent.
fn atlas(slots: u32) -> Vec<u8> {
    vec![0u8; (PIC * PITCH * slots * 4) as usize]
}

/// Fill the rectangle `[x0, x1) x [y0, y1)` of `slot` with `colour`.
fn rect(a: &mut [u8], slot: u32, x0: u32, y0: u32, x1: u32, y1: u32, colour: [u8; 3]) {
    for y in y0..y1 {
        for x in x0..x1 {
            let o = (((slot * PITCH + y) * PIC + x) * 4) as usize;
            a[o..o + 4].copy_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
    }
}

struct Fixture {
    atlas: Vec<u8>,
    slots: u32,
    back: Vec<BackPlane>,
    depth: Vec<u8>,
    billboards: Vec<SideBillboard>,
    sprite: Vec<u8>,
}

fn fixture(slots: u32) -> Fixture {
    Fixture {
        atlas: atlas(slots),
        slots,
        back: Vec::new(),
        depth: vec![0; 16],
        billboards: Vec::new(),
        sprite: [0u8, 255, 255, 255].repeat(4),
    }
}

fn render(gpu: &GpuContext, f: &Fixture, camera: &DioramaCamera) -> Vec<u8> {
    let slope = vec![0i8; 16];
    let (w, h) = (PIC as f32, PIC as f32);
    let target = (w / 2.0, h / 2.0);
    let aspect = OUT.0 as f32 / OUT.1 as f32;
    let placement = camera_matrices::side_placement(w, h, aspect, WINDOW_FOV_Y_DEG, camera, target);
    let scene = SideScene {
        plane_w: w,
        plane_h: h,
        origin: [0.0, 0.0],
        tile_px: 16.0,
        cols: 4,
        rows: 4,
        depth: &f.depth,
        slope: &slope,
        billboards: &f.billboards,
        back_px: 0.0,
        atlas: Atlas {
            slots: f.slots,
            pitch_px: PITCH as f32,
            playfield: 0,
        },
        back_planes: &f.back,
        ref_eye: [target.0, target.1, placement.distance],
    };
    let vertices = build_vertices_side(&scene, placement.eye);
    let vp = camera_matrices::view_proj_side(w, h, aspect, WINDOW_FOV_Y_DEG, camera, target);
    DioramaPass::new(gpu)
        .render_with_vp(
            gpu,
            &vertices,
            &f.atlas,
            PIC,
            PITCH * f.slots,
            &f.sprite,
            2,
            2,
            vp,
            OUT.0,
            OUT.1,
        )
        .expect("renders")
}

fn cam(pitch: f32, yaw: f32) -> DioramaCamera {
    DioramaCamera {
        pitch_deg: pitch,
        yaw_deg: yaw,
        zoom: 1.0,
        follow: None,
    }
}

/// `(count, centroid x, centroid y)` of the opaque pixels within `tol` of
/// `colour`.
fn find(img: &[u8], colour: [u8; 3], tol: i32) -> (usize, f32, f32) {
    let (mut n, mut sx, mut sy) = (0usize, 0.0f64, 0.0f64);
    for (i, p) in img.chunks_exact(4).enumerate() {
        let near = (0..3).all(|c| (i32::from(p[c]) - i32::from(colour[c])).abs() <= tol);
        if near && p[3] == 255 {
            n += 1;
            sx += (i as u32 % OUT.0) as f64;
            sy += (i as u32 / OUT.0) as f64;
        }
    }
    if n == 0 {
        return (0, 0.0, 0.0);
    }
    (n, (sx / n as f64) as f32, (sy / n as f64) as f32)
}

const GREEN: [u8; 3] = [0, 200, 0];
const RED: [u8; 3] = [220, 0, 0];
const CYAN: [u8; 3] = [0, 255, 255];
const GREY: [u8; 3] = [120, 120, 120];

/// D4: a plane set back by its scroll ratio looks like the flat picture from
/// the front, and parts company with the playfield when the camera swings.
#[test]
fn a_back_plane_matches_the_picture_from_the_front_and_shows_parallax_when_the_camera_moves() {
    let Some(gpu) = gpu_or_skip("a_back_plane_parallax") else {
        return;
    };
    let mut f = fixture(3);
    // Playfield (slot 0): a green square low in the picture. Hills (slot 1)
    // and sky (slot 2): a red square high up, same columns, and grey sky.
    rect(&mut f.atlas, 0, 24, 40, 40, 56, GREEN);
    rect(&mut f.atlas, 1, 24, 8, 40, 24, RED);
    rect(&mut f.atlas, 2, 0, 0, 64, 64, GREY);
    f.back = vec![
        BackPlane {
            slot: 1,
            back_px: 40.0,
        },
        BackPlane {
            slot: 2,
            back_px: 64.0,
        },
    ];
    let front = render(&gpu, &f, &cam(0.0, 0.0));
    let (ng, gx0, _) = find(&front, GREEN, 8);
    let (nr, rx0, _) = find(&front, RED, 8);
    assert!(ng > 400 && nr > 400, "both squares show: {ng} {nr}");
    // Pitch is held at the side view's own minimum, so a few pixels of
    // parallax remain; the columns still line up to within that.
    assert!(
        (gx0 - rx0).abs() < 2.0,
        "front view: green {gx0}, red {rx0}"
    );
    // The sky fills what the planes in front leave open.
    assert!(find(&front, GREY, 4).0 > 5000, "the sky plane shows");

    let swung = render(&gpu, &f, &cam(0.0, 25.0));
    let (_, gx1, _) = find(&swung, GREEN, 8);
    let (_, rx1, _) = find(&swung, RED, 8);
    assert!(
        ((gx1 - rx1) - (gx0 - rx0)).abs() > 4.0,
        "yawed 25 degrees the hills move against the playfield: \
         front {gx0} / {rx0}, swung {gx1} / {rx1}"
    );

    // The same picture on ONE plane has no parallax at all.
    let mut one = fixture(1);
    rect(&mut one.atlas, 0, 24, 40, 40, 56, GREEN);
    rect(&mut one.atlas, 0, 24, 8, 40, 24, RED);
    let a = render(&gpu, &one, &cam(0.0, 0.0));
    let b = render(&gpu, &one, &cam(0.0, 25.0));
    let d0 = find(&a, GREEN, 8).1 - find(&a, RED, 8).1;
    let d1 = find(&b, GREEN, 8).1 - find(&b, RED, 8).1;
    assert!((d1 - d0).abs() < 2.0, "one plane: {d0} then {d1}");
}

fn behind_sprite(behind: bool) -> SideBillboard {
    SideBillboard {
        center_x_px: 32.0,
        feet_y_px: 40.0,
        width_px: 16.0,
        height_px: 24.0,
        uv: [0.0, 0.0, 1.0, 1.0],
        behind_bg: behind,
    }
}

/// D3: a sprite the game draws behind the background is hidden by the
/// plane's opaque pixels (the sprite in front of it is not), and shows only
/// through the plane's own holes.
#[test]
fn a_sprite_behind_the_background_is_hidden_by_the_plane_and_shows_through_its_holes() {
    let Some(gpu) = gpu_or_skip("a_sprite_behind_the_background") else {
        return;
    };
    let mut f = fixture(1);
    rect(&mut f.atlas, 0, 0, 0, 64, 64, GREY);
    f.billboards = vec![behind_sprite(false)];
    let front = render(&gpu, &f, &cam(0.0, 0.0));
    assert!(
        find(&front, CYAN, 4).0 > 300,
        "in front of the plane it shows"
    );
    f.billboards = vec![behind_sprite(true)];
    let behind = render(&gpu, &f, &cam(0.0, 0.0));
    assert_eq!(
        find(&behind, CYAN, 4).0,
        0,
        "behind an opaque plane: hidden"
    );

    // A hole in the plane over the sprite's lower half: only that shows.
    let mut g = fixture(1);
    rect(&mut g.atlas, 0, 0, 0, 64, 64, GREY);
    for y in 28..40 {
        for x in 0..64 {
            let o = ((y * PIC + x) * 4 + 3) as usize;
            g.atlas[o] = 0;
        }
    }
    g.billboards = vec![behind_sprite(true)];
    let through = render(&gpu, &g, &cam(0.0, 0.0));
    let (n, _, cy) = find(&through, CYAN, 4);
    let (nf, _, cyf) = find(&front, CYAN, 4);
    assert!(n > 40 && n < nf / 2, "through the hole only: {n} of {nf}");
    assert!(
        cy > cyf,
        "the visible part is the lower one ({cy} vs {cyf})"
    );
}

/// D3: a Tall box hides a behind-the-background sprite where it stands in
/// front of it, even through a hole in the plane.
#[test]
fn a_tall_box_hides_a_sprite_behind_the_background() {
    let Some(gpu) = gpu_or_skip("a_tall_box_hides") else {
        return;
    };
    let mut f = fixture(1);
    // The plane is grey except a hole across the sprite's rows.
    rect(&mut f.atlas, 0, 0, 0, 64, 64, GREY);
    for y in 16..44 {
        for x in 0..64 {
            let o = ((y * PIC + x) * 4 + 3) as usize;
            f.atlas[o] = 0;
        }
    }
    f.billboards = vec![behind_sprite(true)];
    let open = render(&gpu, &f, &cam(0.0, 0.0));
    let n_open = find(&open, CYAN, 4).0;
    assert!(n_open > 300, "the hole shows the sprite: {n_open}");
    // A box two tiles deep over the right half of the sprite (cells at
    // picture x 32..48, rows y 16..48): its front texture is the plane's
    // (grey, opaque there, so give that part of the plane pixels).
    rect(&mut f.atlas, 0, 32, 16, 48, 48, GREY);
    for r in 1..3 {
        f.depth[r * 4 + 2] = 16; // two tiles of depth
    }
    let boxed = render(&gpu, &f, &cam(0.0, 0.0));
    let n_boxed = find(&boxed, CYAN, 4).0;
    assert!(
        n_boxed < n_open * 3 / 4 && n_boxed > n_open / 4,
        "the box hides its half only: {n_boxed} of {n_open}"
    );
}

/// A plane must not shadow the plane behind it: the planes are receivers
/// only (`kind::PLANE`), or each would darken the next in the shape of its
/// own outline. Boxes and billboards still cast onto them.
#[test]
fn a_back_plane_is_not_shadowed_by_the_plane_in_front_of_it() {
    let Some(gpu) = gpu_or_skip("a_back_plane_is_not_shadowed") else {
        return;
    };
    let mut f = fixture(2);
    rect(&mut f.atlas, 0, 0, 0, 32, 64, GREEN);
    rect(&mut f.atlas, 1, 0, 0, 64, 64, GREY);
    f.back = vec![BackPlane {
        slot: 1,
        back_px: 40.0,
    }];
    let lum =
        |p: &[u8]| 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2]);
    for t in [0.3_f32, 0.8] {
        let render_with = |map: bool| {
            let pass = DioramaPass::new(&gpu);
            let mut l = Lighting::new(Sun::for_time(t, Weather::None, LightView::Side));
            l.shadow_map = map;
            pass.set_lighting(Some(l));
            let slope = vec![0i8; 16];
            let target = (32.0, 32.0);
            let camera = cam(0.0, 0.0);
            let aspect = OUT.0 as f32 / OUT.1 as f32;
            let placement = camera_matrices::side_placement(
                64.0,
                64.0,
                aspect,
                WINDOW_FOV_Y_DEG,
                &camera,
                target,
            );
            let scene = SideScene {
                plane_w: 64.0,
                plane_h: 64.0,
                origin: [0.0, 0.0],
                tile_px: 16.0,
                cols: 4,
                rows: 4,
                depth: &f.depth,
                slope: &slope,
                billboards: &[],
                back_px: 0.0,
                atlas: Atlas {
                    slots: 2,
                    pitch_px: PITCH as f32,
                    playfield: 0,
                },
                back_planes: &f.back,
                ref_eye: [32.0, 32.0, placement.distance],
            };
            let v = build_vertices_side(&scene, placement.eye);
            let vp = camera_matrices::view_proj_side(
                64.0,
                64.0,
                aspect,
                WINDOW_FOV_Y_DEG,
                &camera,
                target,
            );
            let img = pass
                .render_with_vp(
                    &gpu,
                    &v,
                    &f.atlas,
                    PIC,
                    PITCH * 2,
                    &f.sprite,
                    2,
                    2,
                    vp,
                    OUT.0,
                    OUT.1,
                )
                .expect("renders");
            (img, pass.shadow_map_used())
        };
        let (without, _) = render_with(false);
        let (with, used) = render_with(true);
        assert!(used, "the map was drawn at t = {t}");
        let darkened = without
            .chunks_exact(4)
            .zip(with.chunks_exact(4))
            .filter(|(a, b)| a[3] == 255 && b[3] == 255 && lum(a) - lum(b) > 6.0)
            .count();
        assert_eq!(darkened, 0, "no shadow on the back plane at t = {t}");
    }
}

/// What a bigger ground texture costs the pass each frame: the same mesh at
/// 1170 x 896 with a one-picture ground and with a four-slot atlas (the
/// layered Diorama's). Timing, run by hand:
/// `cargo test --release -p rf-renderer --test w34_08_layers atlas_upload -- --ignored --nocapture`.
#[test]
#[ignore = "timing report, run by hand"]
fn atlas_upload_cost() {
    let Some(gpu) = gpu_or_skip("atlas_upload_cost") else {
        return;
    };
    let (w, h) = (256u32, 224u32);
    let depth = vec![0u8; 32 * 28];
    let slope = vec![0i8; 32 * 28];
    let sprite = [0u8, 255, 255, 255].repeat(4);
    let camera = cam(13.0, 0.0);
    let aspect = 1170.0 / 896.0;
    let target = (w as f32 / 2.0, h as f32 / 2.0);
    let placement = camera_matrices::side_placement(
        w as f32,
        h as f32,
        aspect,
        WINDOW_FOV_Y_DEG,
        &camera,
        target,
    );
    let vp = camera_matrices::view_proj_side(
        w as f32,
        h as f32,
        aspect,
        WINDOW_FOV_Y_DEG,
        &camera,
        target,
    );
    let pass = DioramaPass::new(&gpu);
    for slots in [1u32, 4] {
        let pitch = if slots == 1 { h } else { h + 2 };
        let ground = vec![180u8; (w * pitch * slots * 4) as usize];
        let scene = SideScene {
            plane_w: w as f32,
            plane_h: h as f32,
            origin: [0.0, 0.0],
            tile_px: 8.0,
            cols: 32,
            rows: 28,
            depth: &depth,
            slope: &slope,
            billboards: &[],
            back_px: 0.0,
            atlas: Atlas {
                slots,
                pitch_px: pitch as f32,
                playfield: 0,
            },
            back_planes: &[],
            ref_eye: [target.0, target.1, placement.distance],
        };
        let v = build_vertices_side(&scene, placement.eye);
        let mut ms = Vec::new();
        for i in 0..260 {
            // A fresh buffer each frame, as the app's per-frame split makes.
            let g = ground.clone();
            let t = std::time::Instant::now();
            pass.render_with_vp(&gpu, &v, &g, w, pitch * slots, &sprite, 2, 2, vp, 1170, 896)
                .expect("renders");
            if i >= 20 {
                ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        ms.sort_by(f64::total_cmp);
        println!(
            "{slots} slot(s), ground {w}x{} : p50 {:.2} ms p95 {:.2} ms",
            pitch * slots,
            ms[120],
            ms[228]
        );
    }
}
