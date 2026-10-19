//! Ticket W34-06b: what the synchronous readback in each GPU pass costs. The
//! shared `read_buffer_sync` used to block in `device.poll(Wait)`, which on
//! Metal returns on a ~1.5 ms timer (see W34-06 in HD2D_PLAN). Timing, so
//! `#[ignore]`d:
//!
//! ```text
//! cargo test --release -p rf-renderer --test w34_06b_readback_cost -- --ignored --nocapture
//! ```

use std::time::Instant;

use rf_core_api::gsu_poly::{PolyJob, PolyOp};
use rf_renderer::fog::{FogParams, FogPass};
use rf_renderer::gpu::GpuContext;
use rf_renderer::lens::{Effect, LensParams, LensPass};
use rf_renderer::shader_chain::{ChainStage, ShaderChain};
use rf_renderer::superfx_gpu::SuperFxGpu;

const FRAMES: usize = 200;

fn frame(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut buf = vec![255u8; (w * h * 4) as usize];
    for (i, px) in buf.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        px[0] = ((x * 7 + y * 3 + seed) % 256) as u8;
        px[1] = ((x * 13 + y * 5) % 256) as u8;
        px[2] = ((x * 3 + y * 17) % 256) as u8;
    }
    buf
}

fn stats(label: &str, mut v: Vec<f64>) {
    v.sort_by(f64::total_cmp);
    let at = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
    println!("{label:<34} p50 {:.2} ms   p95 {:.2} ms", at(0.5), at(0.95));
}

fn time(mut f: impl FnMut(u32)) -> Vec<f64> {
    for i in 0..10 {
        f(i);
    }
    (0..FRAMES)
        .map(|i| {
            let t = Instant::now();
            f(i as u32);
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect()
}

#[test]
#[ignore = "timing; needs a GPU; run deliberately in release"]
fn readback_cost_per_pass() {
    let Ok(gpu) = GpuContext::request_headless() else {
        println!("SKIP: no wgpu adapter");
        return;
    };
    for (w, h) in [(256u32, 224u32), (1024, 896)] {
        let scene = frame(w, h, 0);
        let lens = LensPass::new(&gpu);
        let all = LensParams {
            bloom: Effect::on(0.6),
            tilt_shift: Effect::on(0.6),
            grade: Effect::on(0.6),
            ..LensParams::default()
        };
        stats(
            &format!("lens bloom+tilt+grade {w}x{h}"),
            time(|_| {
                lens.render(&gpu, &scene, w, h, None, &all).unwrap();
            }),
        );
        let grade = LensParams {
            grade: Effect::on(0.6),
            ..LensParams::default()
        };
        stats(
            &format!("lens grade only {w}x{h}"),
            time(|_| {
                lens.render(&gpu, &scene, w, h, None, &grade).unwrap();
            }),
        );
        let fog = FogPass::new(&gpu);
        let density = frame(w, h, 3);
        stats(
            &format!("fog {w}x{h}"),
            time(|i| {
                let p = FogParams::new(i as f32 * 0.016, 1.0, 0.0, 0.5);
                fog.render(&gpu, &scene, &density, w, h, p).unwrap();
            }),
        );
        let chain = ShaderChain::new(&gpu);
        stats(
            &format!("shader chain crt {w}x{h}"),
            time(|_| {
                chain
                    .render(&gpu, &scene, w, h, &[ChainStage::crt(0.5, 0.4, 2.2)])
                    .unwrap();
            }),
        );
    }

    // Super Fx polygons: about 80 triangles-worth of quads in a 224x192
    // viewport, 4x with 4x MSAA (the HD path's heaviest setting).
    let sfx = SuperFxGpu::new(&gpu);
    let ops: Vec<PolyOp> = (0..80u32)
        .map(|i| {
            let (x, y) = ((i * 37 % 200) as u8, (i * 23 % 170) as u8);
            PolyOp::Polygon {
                corners: vec![(x, y), (x + 24, y + 2), (x + 20, y + 20), (x + 2, y + 16)],
                colr: (i % 15) as u8 + 1,
                por: 0,
            }
        })
        .collect();
    let job = PolyJob {
        ops,
        bpp: 4,
        viewport: (224, 192),
        plotted: Vec::new(),
        bitmap: None,
    };
    for (scale, msaa) in [(4u32, 4u32), (3, 1)] {
        stats(
            &format!("superfx gpu {scale}x msaa{msaa} 224x192"),
            time(|_| {
                sfx.render(&gpu, &job, (224, 192), scale, msaa, &|i| {
                    Some([i * 16, 255 - i * 8, 90])
                })
                .unwrap();
            }),
        );
    }
}
