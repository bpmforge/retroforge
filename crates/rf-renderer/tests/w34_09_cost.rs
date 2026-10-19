//! Ticket W34-09: where the layered side-view Diorama's time goes at the 4x
//! window (1170 x 896, the image a 1700 x 760 window gives SMW at 16:9 is
//! wider; this is the 4:3 4x row the W34-08 note timed), and what each cut
//! buys. Timing, so `#[ignore]`d.
//!
//! ```text
//! cargo test --release -p rf-renderer --test w34_09_cost -- --ignored --nocapture
//! ```
//!
//! The scene is Super Mario World's shape: a 256 x 224 picture, nine rows of
//! ground blocks (8 eighths), 12 billboards, and an atlas of one, three or
//! four planes (playfield, hills, sky), all opaque and noisy so no texture
//! compresses away. Rows alternate chunk by chunk (30 frames per chunk) so
//! drift and machine load hit every row alike. The time is the mesh build
//! plus the pass plus the readback, the app's own gate figure minus the CPU
//! split and the HUD composite.

use std::time::Instant;

use rf_renderer::diorama::{camera_matrices, DioramaCamera, DioramaPass, WINDOW_FOV_Y_DEG};
use rf_renderer::diorama_side::{build_vertices_side, Atlas, BackPlane, SideBillboard, SideScene};
use rf_renderer::gpu::GpuContext;
use rf_renderer::lens::Weather;
use rf_renderer::lighting::{LightView, Lighting, Sun};

const PIC: (u32, u32) = (256, 224);
const GAP: u32 = 2;
const TILE: f32 = 16.0;

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p) as usize]
}

fn atlas(slots: u32) -> (Vec<u8>, u32) {
    let pitch = PIC.1 + GAP;
    let mut a = vec![0u8; (PIC.0 * pitch * slots * 4) as usize];
    for slot in 0..slots {
        for y in 0..PIC.1 {
            for x in 0..PIC.0 {
                let o = (((slot * pitch + y) * PIC.0 + x) * 4) as usize;
                a[o] = ((x * 7 + y * 3 + slot * 40) % 256) as u8;
                a[o + 1] = ((x * 13 + y * 5) % 256) as u8;
                a[o + 2] = ((x * 3 + y * 17 + slot * 90) % 256) as u8;
                a[o + 3] = 255;
            }
        }
    }
    (a, pitch)
}

struct Row {
    name: &'static str,
    slots: u32,
    lighting: Option<Lighting>,
    /// W34-08's plane: back planes receive the shadow (`kind::PLANE`).
    legacy: bool,
    overscan: f32,
    /// The image size.
    out: (u32, u32),
}

#[test]
#[ignore = "timing; needs a GPU; run deliberately in release"]
fn layered_side_pass_cost_at_the_4x_window() {
    let gpu = match GpuContext::request_headless() {
        Ok(g) => g,
        Err(e) => {
            println!("SKIP: no wgpu adapter ({e})");
            return;
        }
    };
    println!(
        "adapter: {} ({:?})",
        gpu.adapter_info.name, gpu.adapter_info.backend
    );
    let sun = Sun::for_time(0.4, Weather::None, LightView::Side);
    let lit = Some(Lighting::new(sun));
    let row = |name, slots, lighting, legacy, overscan| Row {
        name,
        slots,
        lighting,
        legacy,
        overscan,
        out: (1170, 896),
    };
    let small = |name, slots, lighting, legacy, overscan| Row {
        out: (585, 448),
        ..row(name, slots, lighting, legacy, overscan)
    };
    let tiny = |name, slots, lighting, legacy, overscan| Row {
        out: (64, 64),
        ..row(name, slots, lighting, legacy, overscan)
    };
    let rows = [
        row("1 plane, sun+map", 1, lit, false, 0.0),
        row("3 planes W34-08", 3, lit, true, 0.0),
        row("3 planes W34-09", 3, lit, false, 224.0),
        row("3 planes W34-09 no os", 3, lit, false, 0.0),
        row(
            "3 planes, no map",
            3,
            Some(Lighting {
                sun,
                shadow_map: false,
            }),
            true,
            0.0,
        ),
        row("3 planes, unlit", 3, None, true, 0.0),
        row("1 plane, unlit", 1, None, false, 0.0),
        small("585x448 3 planes W34-09", 3, lit, false, 224.0),
        tiny("64x64 3 planes W34-09", 3, lit, false, 224.0),
    ];
    let sprite = vec![200u8; 64 * 64 * 4];
    let cols = PIC.0 / TILE as u32 + 2;
    let nrows = PIC.1 / TILE as u32 + 2;
    let mut depth = vec![0u8; (cols * nrows) as usize];
    for r in nrows - 6..nrows {
        for c in 0..cols {
            if (c * 7 + r * 3) % 9 != 0 {
                depth[(r * cols + c) as usize] = 8;
            }
        }
    }
    let slope = vec![0i8; depth.len()];
    let bills: Vec<SideBillboard> = (0..12)
        .map(|i| SideBillboard {
            center_x_px: 20.0 + i as f32 * 18.0,
            feet_y_px: 150.0,
            width_px: 16.0,
            height_px: 24.0,
            uv: [0.0, 0.0, 0.25, 0.375],
            behind_bg: i % 5 == 0,
        })
        .collect();
    let camera = DioramaCamera {
        pitch_deg: 52.0,
        ..DioramaCamera::default()
    };
    let aspect = 1170.0 / 896.0;
    let window = (PIC.0 as f32, PIC.1 as f32);
    let target = (window.0 / 2.0, window.1 / 2.0);
    let placement = camera_matrices::side_placement(
        window.0,
        window.1,
        aspect,
        WINDOW_FOV_Y_DEG,
        &camera,
        target,
    );
    let vp = camera_matrices::view_proj_side(
        window.0,
        window.1,
        aspect,
        WINDOW_FOV_Y_DEG,
        &camera,
        target,
    );
    let pass = DioramaPass::new(&gpu);
    let mut total: Vec<Vec<f64>> = vec![Vec::new(); rows.len()];
    for chunk in 0..14 {
        for (m, row) in rows.iter().enumerate() {
            pass.set_lighting(row.lighting);
            let (tex, pitch) = atlas(row.slots.max(4));
            let back: Vec<BackPlane> = (1..row.slots)
                .map(|s| BackPlane {
                    slot: s,
                    back_px: 24.0 * s as f32,
                })
                .collect();
            for i in 0..30 {
                let t = Instant::now();
                let scene = SideScene {
                    plane_w: window.0,
                    plane_h: window.1,
                    origin: [-(i as f32 % 8.0), 0.0],
                    tile_px: TILE,
                    cols,
                    rows: nrows,
                    depth: &depth,
                    slope: &slope,
                    billboards: &bills,
                    back_px: 0.0,
                    atlas: Atlas {
                        slots: row.slots.max(4),
                        pitch_px: pitch as f32,
                        playfield: 0,
                    },
                    back_planes: &back,
                    ref_eye: [target.0, window.1 - target.1, placement.distance],
                    overscan_px: row.overscan,
                };
                let mut v = build_vertices_side(&scene, placement.eye);
                if row.legacy {
                    for vert in &mut v {
                        if vert.kind == rf_renderer::diorama_mesh::kind::BACK {
                            vert.kind = rf_renderer::diorama_mesh::kind::PLANE;
                        }
                    }
                }
                let out = pass
                    .render_with_vp(
                        &gpu,
                        &v,
                        &tex,
                        PIC.0,
                        pitch * row.slots.max(4),
                        &sprite,
                        64,
                        64,
                        vp,
                        row.out.0,
                        row.out.1,
                    )
                    .expect("render");
                std::hint::black_box(&out);
                if chunk > 0 || i >= 5 {
                    total[m].push(t.elapsed().as_secs_f64() * 1e3);
                }
            }
        }
    }
    for (m, row) in rows.iter().enumerate() {
        println!(
            "W34-09 COST {:>24}: mesh+pass+readback p50 {:.2} p95 {:.2} ms",
            row.name,
            pct(&mut total[m], 0.5),
            pct(&mut total[m], 0.95),
        );
    }
}
