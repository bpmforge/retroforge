//! W34-09: the side-view planes stand on past the picture's edge (no void at
//! the edges of a pitched or swung camera) and the planes behind the
//! playfield's receive no shadow (a sprite's shadow on the hills was a
//! floating grey blob).
//!
//! A 64 x 64 picture and an atlas of picture-sized slots, as `w34_08_layers`.

use rf_renderer::diorama::{camera_matrices, DioramaCamera, DioramaPass, WINDOW_FOV_Y_DEG};
use rf_renderer::diorama_mesh::{kind, Vertex};
use rf_renderer::diorama_side::{build_vertices_side, Atlas, BackPlane, SideBillboard, SideScene};
use rf_renderer::gpu::GpuContext;
use rf_renderer::lens::Weather;
use rf_renderer::lighting::{LightView, Lighting, Sun};

const PIC: u32 = 64;
const PITCH: u32 = PIC + 2;
const OUT: (u32, u32) = (256, 256);
const GREEN: [u8; 3] = [0, 200, 0];
const RED: [u8; 3] = [220, 0, 0];
const GREY: [u8; 3] = [160, 160, 160];

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

fn rect(a: &mut [u8], slot: u32, x0: u32, y0: u32, x1: u32, y1: u32, colour: [u8; 3]) {
    for y in y0..y1 {
        for x in x0..x1 {
            let o = (((slot * PITCH + y) * PIC + x) * 4) as usize;
            a[o..o + 4].copy_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
    }
}

struct Scene {
    atlas: Vec<u8>,
    slots: u32,
    back: Vec<BackPlane>,
    billboards: Vec<SideBillboard>,
}

fn scene(slots: u32) -> Scene {
    Scene {
        atlas: vec![0u8; (PIC * PITCH * slots * 4) as usize],
        slots,
        back: Vec::new(),
        billboards: Vec::new(),
    }
}

fn mesh(s: &Scene, camera: &DioramaCamera, overscan: f32) -> (Vec<Vertex>, camera_matrices::Mat4) {
    let depth = vec![0u8; 16];
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
        depth: &depth,
        slope: &slope,
        billboards: &s.billboards,
        back_px: 0.0,
        atlas: Atlas {
            slots: s.slots,
            pitch_px: PITCH as f32,
            playfield: 0,
        },
        back_planes: &s.back,
        ref_eye: [target.0, target.1, placement.distance],
        overscan_px: overscan,
    };
    let v = build_vertices_side(&scene, placement.eye);
    let vp = camera_matrices::view_proj_side(w, h, aspect, WINDOW_FOV_Y_DEG, camera, target);
    (v, vp)
}

fn draw(
    gpu: &GpuContext,
    pass: &DioramaPass,
    s: &Scene,
    v: &[Vertex],
    vp: camera_matrices::Mat4,
) -> Vec<u8> {
    let sprite = [0u8, 255, 255, 255].repeat(4);
    pass.render_with_vp(
        gpu,
        v,
        &s.atlas,
        PIC,
        PITCH * s.slots,
        &sprite,
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

/// The grey of the hills as the sun lights it (warm or cool, never the
/// green ground or the cyan sprite).
fn hills_like(p: &[u8]) -> bool {
    let (r, g, b) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
    p[3] == 255 && g - r < 40 && b - r < 100 && r > 20
}

fn near(p: &[u8], c: [u8; 3], tol: i32) -> bool {
    (0..3).all(|i| (i32::from(p[i]) - i32::from(c[i])).abs() <= tol)
}

#[test]
fn back_planes_are_their_own_kind_and_the_playfield_plane_is_not() {
    let mut s = scene(3);
    s.back = vec![
        BackPlane {
            slot: 1,
            back_px: 20.0,
        },
        BackPlane {
            slot: 2,
            back_px: 44.0,
        },
    ];
    let (v, _) = mesh(&s, &cam(0.0, 0.0), 0.0);
    let back = v.iter().filter(|x| x.kind == kind::BACK).count();
    let plane = v.iter().filter(|x| x.kind == kind::PLANE).count();
    assert_eq!(
        (back, plane),
        (12, 6),
        "two back planes, one playfield plane"
    );
}

#[test]
fn an_overscan_plane_is_nine_quads_whose_outer_ones_mirror_the_picture() {
    let mut s = scene(2);
    s.back = vec![BackPlane {
        slot: 1,
        back_px: 20.0,
    }];
    let (v, _) = mesh(&s, &cam(0.0, 0.0), 32.0);
    assert_eq!(v.len(), 2 * 9 * 6);
    // No texture coordinate of any plane leaves its own slot (the gap between
    // two slots is transparent: reading it would draw a dark seam).
    let atlas = Atlas {
        slots: 2,
        pitch_px: PITCH as f32,
        playfield: 0,
    };
    for x in &v {
        assert!((0.0..=1.0).contains(&x.uv[0]), "u {}", x.uv[0]);
        let in_slot = |slot: u32| {
            x.uv[1] >= atlas.v(slot, 0.0) - 1e-6 && x.uv[1] <= atlas.v(slot, PIC as f32) + 1e-6
        };
        assert!(in_slot(0) || in_slot(1), "v {} is in the gap", x.uv[1]);
    }
}

#[test]
fn a_swung_camera_sees_no_void_past_the_pictures_edge_with_overscan() {
    let Some(gpu) = gpu_or_skip("a_swung_camera_sees_no_void") else {
        return;
    };
    let mut s = scene(1);
    rect(&mut s.atlas, 0, 0, 0, PIC, PIC, GREEN);
    // The right-most 16 columns are red: what the plane mirrors past its edge.
    rect(&mut s.atlas, 0, PIC - 16, 0, PIC, PIC, RED);
    let camera = cam(50.0, 35.0);
    let pass = DioramaPass::new(&gpu);
    let void = |overscan: f32| {
        let (v, vp) = mesh(&s, &camera, overscan);
        let img = draw(&gpu, &pass, &s, &v, vp);
        let n = img.chunks_exact(4).filter(|p| p[3] < 255).count();
        (n, img)
    };
    let (void_flat, _) = void(0.0);
    let (void_over, img) = void(PIC as f32);
    assert!(
        void_flat > 2000,
        "the picture alone leaves clear colour at the edges of this view: {void_flat}"
    );
    assert_eq!(void_over, 0, "overscan fills the whole view");
    // The part past the right edge is the picture's own edge, mirrored: red
    // first (the last columns), then green. Streaks of one repeated column
    // would be red all the way out.
    let red = img.chunks_exact(4).filter(|p| near(p, RED, 12)).count();
    assert!(
        red > 500,
        "the edge goes on past the picture: {red} red pixels"
    );
    let green = img.chunks_exact(4).filter(|p| near(p, GREEN, 12)).count();
    assert!(green > 500, "and the green beyond it: {green} green pixels");
}

#[test]
fn a_billboards_shadow_does_not_land_on_a_plane_behind_the_playfield() {
    let Some(gpu) = gpu_or_skip("a_billboards_shadow_does_not_land") else {
        return;
    };
    // The playfield is a holey strip low in the picture, the hills one plane
    // behind it, and a sprite stands in front of both: its shadow (a sun low
    // on one side) falls somewhere on whatever is behind.
    let mut s = scene(2);
    rect(&mut s.atlas, 0, 0, 52, PIC, PIC, GREEN);
    rect(&mut s.atlas, 1, 0, 0, PIC, PIC, GREY);
    s.back = vec![BackPlane {
        slot: 1,
        back_px: 40.0,
    }];
    s.billboards = vec![SideBillboard {
        center_x_px: 32.0,
        feet_y_px: 52.0,
        width_px: 16.0,
        height_px: 28.0,
        uv: [0.0, 0.0, 1.0, 1.0],
        behind_bg: false,
    }];
    let lum =
        |p: &[u8]| 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2]);
    let darkened_by_the_map = |legacy: bool, t: f32| {
        let go = |map: bool| {
            let pass = DioramaPass::new(&gpu);
            let mut l = Lighting::new(Sun::for_time(t, Weather::None, LightView::Side));
            l.shadow_map = map;
            pass.set_lighting(Some(l));
            let (mut v, vp) = mesh(&s, &cam(0.0, 0.0), 0.0);
            if legacy {
                // W34-08's plane: every plane receives.
                for x in &mut v {
                    if x.kind == kind::BACK {
                        x.kind = kind::PLANE;
                    }
                }
            }
            let img = draw(&gpu, &pass, &s, &v, vp);
            (img, pass.shadow_map_used())
        };
        let (without, _) = go(false);
        let (with, used) = go(true);
        assert!(used, "the map was drawn at t = {t}");
        without
            .chunks_exact(4)
            .zip(with.chunks_exact(4))
            .filter(|(a, b)| hills_like(a) && b[3] == 255 && lum(a) - lum(b) > 6.0)
            .count()
    };
    let mut legacy_seen = 0;
    for t in [0.15_f32, 0.4, 0.9] {
        assert_eq!(
            darkened_by_the_map(false, t),
            0,
            "no shadow on the hills at t = {t}"
        );
        legacy_seen += darkened_by_the_map(true, t);
    }
    assert!(
        legacy_seen > 0,
        "the test sees the shadow when the hills do receive it (else it proves nothing)"
    );
}
