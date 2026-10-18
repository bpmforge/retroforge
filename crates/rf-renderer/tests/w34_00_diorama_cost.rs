//! Ticket W34-00: what the Diorama pass costs, alone, at the sizes that
//! matter. Timing, so `#[ignore]`d.
//!
//! ```text
//! cargo test --release -p rf-renderer --test w34_00_diorama_cost -- --ignored --nocapture
//! ```
//!
//! The scene is the live-screen path's: a 33 x 29 grid of 8 px cells (the
//! padded SNES screen), the lowest nine rows walls 8 eighths tall with a few
//! gaps, 24 billboards. Each size is timed with the HD-2D preset camera (pitch
//! 40, zoom 1.6, follow) and the default camera, and once with NO geometry,
//! which isolates the cost the geometry does not change: creating the
//! textures, the encoder, and the synchronous readback (HD2D_PLAN B5).
//!
//! `256x224` is what the app renders today (the frame's own size, then the UI
//! scales the texture NEAREST); `1024x896` is the 4x frame, the size the plan
//! renders at once D5 lands. This harness's own `bench-passes` rows (W16-01)
//! time the same pass on a 16 px checkerboard at 256x240 and 512x448.

use std::time::Instant;

use rf_renderer::diorama::{vertices_for_camera, DioramaCamera, DioramaPass};
use rf_renderer::diorama_mesh::{Billboard, DioramaScene};
use rf_renderer::gpu::GpuContext;

const COLS: u32 = 33;
const ROWS: u32 = 29;
const TILE_PX: u32 = 8;
const FRAMES: usize = 200;

fn frame(w: u32, h: u32) -> Vec<u8> {
    let mut buf = vec![255u8; (w * h * 4) as usize];
    for (i, px) in buf.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        px[0] = ((x * 7 + y * 3) % 256) as u8;
        px[1] = ((x * 13 + y * 5) % 256) as u8;
        px[2] = ((x * 3 + y * 17) % 256) as u8;
    }
    buf
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p) as usize]
}

#[test]
#[ignore = "timing; needs a GPU; run deliberately in release"]
fn diorama_pass_cost_at_1x_2x_4x() {
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
    let pass = DioramaPass::new(&gpu);
    let (gw, gh) = (COLS * TILE_PX, ROWS * TILE_PX);
    let ground = frame(gw, gh);
    let sprite = frame(16, 16);

    let n = (COLS * ROWS) as usize;
    let mut solid = vec![0u8; n];
    let mut depth = vec![0u8; n];
    for row in ROWS - 9..ROWS {
        for col in 0..COLS {
            if (col * 7 + row * 3) % 5 != 0 {
                solid[(row * COLS + col) as usize] = 1;
                depth[(row * COLS + col) as usize] = 8;
            }
        }
    }
    let billboards: Vec<Billboard> = (0..24)
        .map(|i| Billboard {
            center_x_px: 20.0 + i as f32 * 10.0,
            center_z_px: 90.0 + (i % 5) as f32 * 14.0,
            width_px: 16.0,
            height_px: 16.0,
            uv: [0.0, 0.0, 1.0, 1.0],
        })
        .collect();
    let scene = DioramaScene {
        tiles_w: COLS,
        tiles_h: ROWS,
        tile_px: TILE_PX as f32,
        solid: &solid,
        depth: &depth,
        billboards: &billboards,
    };
    let empty = DioramaScene {
        solid: &vec![0u8; n],
        depth: &vec![0u8; n],
        billboards: &[],
        ..DioramaScene {
            tiles_w: COLS,
            tiles_h: ROWS,
            tile_px: TILE_PX as f32,
            solid: &[],
            depth: &[],
            billboards: &[],
        }
    };
    let preset = DioramaCamera {
        pitch_deg: 40.0,
        yaw_deg: 0.0,
        zoom: 1.6,
        follow: Some((gw as f32 / 2.0, gh as f32 * 0.6)),
    };

    for (label, camera) in [
        ("default camera", DioramaCamera::default()),
        ("HD-2D preset camera", preset),
    ] {
        for &(w, h) in &[(256u32, 224u32), (512, 448), (1024, 896)] {
            let mut row = Vec::new();
            for (what, sc) in [("walls+billboards", &scene), ("no geometry", &empty)] {
                let mut mesh_us = Vec::new();
                let mut pass_ms = Vec::new();
                let mut verts = 0usize;
                for i in 0..FRAMES + 5 {
                    let t = Instant::now();
                    let v = vertices_for_camera(sc, w, h, &camera);
                    let mesh = t.elapsed().as_secs_f64() * 1e6;
                    verts = v.len();
                    let t = Instant::now();
                    let out = pass
                        .render_camera(
                            &gpu, &v, &ground, gw, gh, &sprite, 16, 16, COLS, ROWS, TILE_PX, w, h,
                            &camera,
                        )
                        .expect("render");
                    let ms = t.elapsed().as_secs_f64() * 1e3;
                    std::hint::black_box(&out);
                    if i >= 5 {
                        mesh_us.push(mesh);
                        pass_ms.push(ms);
                    }
                }
                row.push(format!(
                    "{what}: {verts} verts, mesh p50 {:.0} us, pass p50 {:.2} ms p95 {:.2} ms",
                    pct(&mut mesh_us, 0.5),
                    pct(&mut pass_ms, 0.5),
                    pct(&mut pass_ms, 0.95)
                ));
            }
            println!(
                "DIORAMA {label} {w}x{h} (readback {} KB): {}",
                w * h * 4 / 1024,
                row.join(" | ")
            );
        }
    }
}
