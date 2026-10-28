//! W39-05: temporal upscaling of the HD-2D renderer to 5K on a synthetic scene
//! (no ROM content): jittered low-resolution frames plus depth and motion
//! vectors, accumulated by the portable WGSL resolve (everywhere) or by
//! MetalFX (`--features metalfx`, macOS, `#[ignore]`d: needs a real device).
//!
//! The artefact checks use a static camera: after the jitter cycle the output
//! must not shimmer between frames, must have no black holes where the native
//! 5K render has content, and must be closer to the native 5K render than a
//! plain bilinear upscale of the same internal frame is.
//!
//! Set `RF_W39_SHEET_DIR=<dir>` to also write the contact sheet:
//!
//! ```text
//! RF_W39_SHEET_DIR=docs/screens/w39 cargo test --release -p rf-renderer \
//!   --features metalfx --test w39_05_temporal contact_sheet -- --ignored
//! ```

use rf_renderer::diorama::DioramaPass;
use rf_renderer::diorama_gbuffer::read_motion;
use rf_renderer::gpu::GpuContext;
use rf_renderer::hd2d::{code, render, Hd2dLattice, Hd2dLook, Hd2dMarker, Hd2dStage};
use rf_renderer::hd2d_temporal::Hd2dTemporal;
use rf_renderer::lens::LensPass;
use rf_renderer::temporal::{TemporalBackend, OUTPUT_5K};

const W: u32 = 256;
const H: u32 = 224;
const T: u32 = 16;
const COLS: u32 = W / T;
const ROWS: u32 = H / T;

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

struct Scene {
    picture: Vec<u8>,
    sprites: Vec<u8>,
    depth: Vec<u8>,
    markers: Vec<Hd2dMarker>,
}

fn put(buf: &mut [u8], x: usize, y: usize, c: [u8; 4]) {
    let i = (y * W as usize + x) * 4;
    buf[i..i + 4].copy_from_slice(&c);
}

/// A made-up level: a sky, a brick floor, two floating brick platforms, and
/// five blobs (one per class) on the sprite layer.
fn scene() -> Scene {
    let (cols, rows) = (COLS as usize, ROWS as usize);
    let mut depth = vec![0u8; cols * rows];
    let mut solid = |c: usize, r: usize| {
        if c < cols && r < rows {
            depth[r * cols + c] = 8;
        }
    };
    for c in 0..cols {
        solid(c, rows - 2);
        solid(c, rows - 1);
    }
    for c in 3..7 {
        solid(c, 8);
    }
    for c in 10..13 {
        solid(c, 5);
    }
    solid(14, rows - 3);
    solid(15, rows - 3);
    solid(15, rows - 4);

    let mut picture = vec![0u8; (W * H * 4) as usize];
    for y in 0..H as usize {
        for x in 0..W as usize {
            // Sky: a vertical gradient with a few soft hills near the floor.
            let t = y as f32 / H as f32;
            let hill = ((x as f32 / 38.0).sin() * 14.0) as i32 + 170;
            let c = if (y as i32) > hill {
                [70, 140 + (x % 7) as u8, 80, 255]
            } else {
                [(90.0 + 90.0 * t) as u8, (150.0 + 70.0 * t) as u8, 235, 255]
            };
            put(&mut picture, x, y, c);
        }
    }
    for r in 0..rows {
        for c in 0..cols {
            if depth[r * cols + c] == 0 {
                continue;
            }
            for y in 0..T as usize {
                for x in 0..T as usize {
                    let edge = x == 0 || y == 0 || x == T as usize - 1 || y == T as usize - 1;
                    let v = if edge {
                        110
                    } else if (x / 4 + y / 4) % 2 == 0 {
                        190
                    } else {
                        170
                    };
                    put(
                        &mut picture,
                        c * T as usize + x,
                        r * T as usize + y,
                        [v, v / 2 + 20, 50, 255],
                    );
                }
            }
        }
    }

    let mut sprites = vec![0u8; (W * H * 4) as usize];
    let floor = ((rows - 2) as u32 * T) as i32;
    // (class code, x, feet y, w, h, colour)
    let specs = [
        (code::PLAYER, 40, floor, 16, 28, [60u8, 90, 230]),
        (code::ENEMY, 88, floor, 16, 16, [220, 50, 50]),
        (code::PROJECTILE, 150, floor - 40, 10, 10, [250, 220, 40]),
        (code::PICKUP, 190, floor - 6, 12, 12, [60, 200, 90]),
        (code::SCENERY, 226, floor, 18, 20, [140, 100, 60]),
    ];
    let mut markers = Vec::new();
    for (cc, x, feet, w, h, col) in specs {
        let (x0, y0) = (x - w / 2, feet - h);
        for yy in 0..h {
            for xx in 0..w {
                let (dx, dy) = (
                    (xx as f32 + 0.5) / w as f32 - 0.5,
                    (yy as f32 + 0.5) / h as f32 - 0.5,
                );
                if dx * dx * 4.0 + dy * dy * 4.0 <= 1.0 {
                    put(
                        &mut sprites,
                        (x0 + xx) as usize,
                        (y0 + yy) as usize,
                        [col[0], col[1], col[2], 255],
                    );
                }
            }
        }
        markers.push(Hd2dMarker::from_rect(x0, y0, w as u16, h as u16, cc));
    }
    Scene {
        picture,
        sprites,
        depth,
        markers,
    }
}

fn stage<'a>(s: &'a Scene, depth: &'a [u8]) -> Hd2dStage<'a> {
    Hd2dStage {
        picture_rgba: &s.picture,
        sprite_rgba: &s.sprites,
        size: (W, H),
        lattice: Hd2dLattice {
            origin: (0.0, 0.0),
            tile_px: T as f32,
            cols: COLS,
            rows: ROWS,
            depth,
        },
    }
}

const HALF: (u32, u32) = (2560, 1440);
const THIRD: (u32, u32) = (1707, 960);

/// Sharp, unblurred look (the lens is exercised by its own test).
fn flat_look() -> Hd2dLook {
    Hd2dLook {
        dof: 0.0,
        out: OUTPUT_5K,
        ..Hd2dLook::default()
    }
}

/// Premultiply straight-alpha RGBA in place (composite over black).
fn premultiply(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        for c in &mut px[..3] {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
    }
}

/// The reference: a full native render at `size`, over black.
fn native(gpu: &GpuContext, s: &Scene, look: &Hd2dLook, size: (u32, u32)) -> Vec<u8> {
    let (pass, lens) = (DioramaPass::new(gpu), LensPass::new(gpu));
    let st = stage(s, &s.depth);
    let (mut img, _) = render(
        gpu,
        &pass,
        &lens,
        &st,
        &s.markers,
        &Hd2dLook { out: size, ..*look },
    )
    .expect("native render");
    premultiply(&mut img);
    img
}

/// Bilinear upscale of an RGBA8 image (the no-temporal baseline).
fn bilinear_up(src: &[u8], (sw, sh): (u32, u32), (dw, dh): (u32, u32)) -> Vec<u8> {
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    let (sw_i, sh_i) = (sw as i32, sh as i32);
    for y in 0..dh as usize {
        let fy = ((y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).max(0.0);
        let y0 = (fy.floor() as i32).min(sh_i - 1);
        let y1 = (y0 + 1).min(sh_i - 1);
        let ty = fy - y0 as f32;
        for x in 0..dw as usize {
            let fx = ((x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).max(0.0);
            let x0 = (fx.floor() as i32).min(sw_i - 1);
            let x1 = (x0 + 1).min(sw_i - 1);
            let tx = fx - x0 as f32;
            let at = |xx: i32, yy: i32, c: usize| {
                f32::from(src[(yy as usize * sw as usize + xx as usize) * 4 + c])
            };
            for c in 0..4 {
                let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
                let bot = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
                out[(y * dw as usize + x) * 4 + c] = (top * (1.0 - ty) + bot * ty + 0.5) as u8;
            }
        }
    }
    out
}

/// Mean absolute RGB difference, 0..255.
fn mae(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut sum = 0u64;
    for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            sum += u64::from(p[c].abs_diff(q[c]));
        }
    }
    sum as f64 / (a.len() / 4 * 3) as f64
}

/// Fraction of pixels where any RGB channel differs by more than `thr`.
fn frac_over(a: &[u8], b: &[u8], thr: u8) -> f64 {
    let n = a.len() / 4;
    let bad = a
        .chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(p, q)| (0..3).any(|c| p[c].abs_diff(q[c]) > thr))
        .count();
    bad as f64 / n as f64
}

/// Pixels the reference has content at that the output left black.
fn holes(out: &[u8], reference: &[u8]) -> usize {
    out.chunks_exact(4)
        .zip(reference.chunks_exact(4))
        .filter(|(o, r)| r[..3].iter().any(|&c| c > 48) && o[..3].iter().all(|&c| c <= 4))
        .count()
}

/// Mean absolute RGB error over the pixels on or next to an edge of
/// `reference` (a horizontal or vertical step above 24): flat areas match
/// any upscaler, edges are where the methods differ.
fn edge_mae(out: &[u8], reference: &[u8], (w, h): (u32, u32)) -> f64 {
    let (w, h) = (w as usize, h as usize);
    let (mut sum, mut n) = (0u64, 0u64);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = (y * w + x) * 4;
            let step = |j: usize| (0..3).any(|c| reference[i + c].abs_diff(reference[j + c]) > 24);
            if step(i + 4) || step(i - 4) || step(i + w * 4) || step(i - w * 4) {
                for c in 0..3 {
                    sum += u64::from(out[i + c].abs_diff(reference[i + c]));
                }
                n += 3;
            }
        }
    }
    sum as f64 / n.max(1) as f64
}

struct Run {
    prev: Vec<u8>,
    last: Vec<u8>,
    backend: TemporalBackend,
    reason: Option<String>,
    ms_per_frame: f64,
}

/// `frames` static-camera frames; the last two are read back.
fn run_static(gpu: &GpuContext, internal: (u32, u32), prefer_metalfx: bool, frames: u32) -> Run {
    run_static_with(gpu, internal, prefer_metalfx, frames, None)
}

fn run_static_with(
    gpu: &GpuContext,
    internal: (u32, u32),
    prefer_metalfx: bool,
    frames: u32,
    jitter_sign: Option<(f32, f32)>,
) -> Run {
    let s = scene();
    let st = stage(&s, &s.depth);
    let (pass, lens) = (DioramaPass::new(gpu), LensPass::new(gpu));
    let mut hd = Hd2dTemporal::new(gpu, internal, OUTPUT_5K, prefer_metalfx);
    if let Some(sign) = jitter_sign {
        hd.upscaler_mut().set_metalfx_jitter_sign(sign);
    }
    let look = flat_look();
    let (mut prev, mut last) = (Vec::new(), Vec::new());
    let mut t0 = std::time::Instant::now();
    let mut ms_per_frame = 0.0;
    for f in 0..frames {
        hd.render(gpu, &pass, &lens, &st, &s.markers, &look)
            .expect("frame renders");
        if f == 1 {
            // Past the one-time pipeline and texture setup.
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            t0 = std::time::Instant::now();
        }
        if frames > 6 && f + 3 == frames {
            // Frames 2..=this one, drawn and synchronised, before any readback.
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            ms_per_frame = t0.elapsed().as_secs_f64() * 1000.0 / f64::from(frames - 4);
        }
        if f + 2 == frames {
            prev = hd.upscaler().read_output(gpu).expect("readback");
        }
        if f + 1 == frames {
            last = hd.upscaler().read_output(gpu).expect("readback");
        }
    }
    Run {
        prev,
        last,
        backend: hd.backend(),
        reason: hd.upscaler().fallback_reason.clone(),
        ms_per_frame,
    }
}

#[test]
fn portable_outputs_5k_from_half_and_third_scale() {
    let Some(gpu) = gpu_or_skip("portable_outputs_5k") else {
        return;
    };
    for internal in [HALF, THIRD] {
        let r = run_static(&gpu, internal, false, 4);
        assert_eq!(r.backend, TemporalBackend::Portable);
        assert_eq!(
            r.last.len(),
            OUTPUT_5K.0 as usize * OUTPUT_5K.1 as usize * 4,
            "5K output from {internal:?}"
        );
        assert!(r.last.chunks_exact(4).all(|p| p[3] == 255), "opaque");
        let lit = r
            .last
            .chunks_exact(4)
            .filter(|p| p[..3].iter().any(|&c| c > 8))
            .count();
        assert!(lit * 2 > r.last.len() / 4, "mostly lit: {lit}");
        println!(
            "portable {internal:?} -> {OUTPUT_5K:?}: {:.1} ms/frame (incl. synchronisation)",
            r.ms_per_frame
        );
    }
}

#[test]
fn portable_is_deterministic() {
    let Some(gpu) = gpu_or_skip("portable_is_deterministic") else {
        return;
    };
    let a = run_static(&gpu, THIRD, false, 6);
    let b = run_static(&gpu, THIRD, false, 6);
    assert!(a.last == b.last, "same inputs must give the same bytes");
    assert!(a.prev == b.prev);
}

/// The static-camera artefact checks both back ends must pass.
fn check_static(name: &str, internal: (u32, u32), run: &Run, reference: &[u8], baseline: &[u8]) {
    let shimmer = mae(&run.prev, &run.last);
    let shimmer_px = frac_over(&run.prev, &run.last, 12);
    let holes_n = holes(&run.last, reference);
    let err = edge_mae(&run.last, reference, OUTPUT_5K);
    let base_err = edge_mae(baseline, reference, OUTPUT_5K);
    let big = frac_over(&run.last, reference, 96);
    println!(
        "{name} {internal:?}: frame-to-frame mae {shimmer:.4}, px>12 {shimmer_px:.6}; holes {holes_n}; \
         vs native edge-mae {err:.3} (bilinear baseline {base_err:.3}), px>96 {big:.5}; {:.1} ms/frame",
        run.ms_per_frame
    );
    assert!(shimmer < 0.5, "{name}: output shimmers, mae {shimmer}");
    assert!(shimmer_px < 0.002, "{name}: edges flicker, {shimmer_px}");
    assert!(holes_n < 500, "{name}: {holes_n} black holes");
    assert!(
        err < base_err,
        "{name}: not closer to native than bilinear ({err} vs {base_err})"
    );
    assert!(big < 0.02, "{name}: {big} of pixels far from native");
}

fn baseline_for(gpu: &GpuContext, s: &Scene, internal: (u32, u32)) -> Vec<u8> {
    let low = native(gpu, s, &flat_look(), internal);
    bilinear_up(&low, internal, OUTPUT_5K)
}

#[test]
fn portable_static_camera_is_stable_and_artefact_free() {
    let Some(gpu) = gpu_or_skip("portable_static") else {
        return;
    };
    let s = scene();
    let reference = native(&gpu, &s, &flat_look(), OUTPUT_5K);
    for internal in [THIRD, HALF] {
        let run = run_static(&gpu, internal, false, 28);
        let baseline = baseline_for(&gpu, &s, internal);
        check_static("portable", internal, &run, &reference, &baseline);
    }
}

fn pan_look(yaw: f32) -> Hd2dLook {
    Hd2dLook {
        yaw_deg: yaw,
        dof: 0.0,
        ..Hd2dLook::default()
    }
}

#[test]
fn motion_vectors_are_zero_when_static_and_follow_a_pan() {
    let Some(gpu) = gpu_or_skip("motion_vectors") else {
        return;
    };
    let s = scene();
    let st = stage(&s, &s.depth);
    let (pass, lens) = (DioramaPass::new(&gpu), LensPass::new(&gpu));
    let internal = (640, 360);
    let mut hd = Hd2dTemporal::new(&gpu, internal, (1280, 720), false);
    let motion = |hd: &mut Hd2dTemporal, yaw: f32| {
        hd.render(&gpu, &pass, &lens, &st, &s.markers, &pan_look(yaw))
            .expect("frame");
        let gb = hd.last_gbuffer().expect("gbuffer");
        read_motion(&gpu, &gb.motion, internal).expect("motion readback")
    };
    let _ = motion(&mut hd, 6.0);
    let still = motion(&mut hd, 6.0);
    let mx = still
        .iter()
        .map(|m| m[0].abs().max(m[1].abs()))
        .fold(0.0f32, f32::max);
    let nz = still.iter().filter(|m| m[0] != 0.0 || m[1] != 0.0).count();
    println!(
        "static: max |motion| {mx}, nonzero {nz}, sample {:?}",
        &still[still.len() / 2..still.len() / 2 + 3]
    );
    assert!(
        still.iter().all(|m| m[0] == 0.0 && m[1] == 0.0),
        "a static camera has no motion"
    );
    let small = motion(&mut hd, 7.0);
    let large = motion(&mut hd, 9.0); // 2 degrees from 7
    let stat = |v: &[[f32; 2]]| {
        assert!(v.iter().all(|m| m[0].is_finite() && m[1].is_finite()));
        let moving = v.iter().filter(|m| m[0] != 0.0 || m[1] != 0.0).count();
        let mean = v.iter().map(|m| f64::from(m[0].hypot(m[1]))).sum::<f64>() / v.len() as f64;
        let max = v.iter().map(|m| m[0].hypot(m[1])).fold(0.0f32, f32::max);
        (moving as f64 / v.len() as f64, mean, max)
    };
    let (f1, m1, x1) = stat(&small);
    let (f2, m2, x2) = stat(&large);
    println!("pan 1 deg: moving {f1:.3} mean {m1:.3} px max {x1:.2}; pan 2 deg: moving {f2:.3} mean {m2:.3} px max {x2:.2}");
    assert!(f1 > 0.2, "most of the picture moves under a pan: {f1}");
    assert!(m2 > m1 * 1.3, "a bigger pan moves more: {m1} -> {m2}");
    assert!(x2 < 200.0, "bounded: {x2}");
}

/// The sprite layer and markers shifted right by `dx` picture pixels.
fn shifted(s: &Scene, dx: usize) -> Scene {
    let mut sprites = vec![0u8; s.sprites.len()];
    let row = W as usize * 4;
    for y in 0..H as usize {
        for x in 0..W as usize - dx {
            let (from, to) = (y * row + x * 4, y * row + (x + dx) * 4);
            sprites[to..to + 4].copy_from_slice(&s.sprites[from..from + 4]);
        }
    }
    Scene {
        picture: s.picture.clone(),
        sprites,
        depth: s.depth.clone(),
        markers: s
            .markers
            .iter()
            .map(|m| Hd2dMarker {
                x: m.x + dx as i32,
                ..*m
            })
            .collect(),
    }
}

#[test]
fn a_moving_object_has_motion_against_a_still_background() {
    let Some(gpu) = gpu_or_skip("object_motion") else {
        return;
    };
    let (s0, s1) = (scene(), shifted(&scene(), 6));
    let (pass, lens) = (DioramaPass::new(&gpu), LensPass::new(&gpu));
    let internal = (640, 360);
    let mut hd = Hd2dTemporal::new(&gpu, internal, (1280, 720), false);
    let look = pan_look(6.0);
    for sc in [&s0, &s1] {
        let st = stage(sc, &sc.depth);
        hd.render(&gpu, &pass, &lens, &st, &sc.markers, &look)
            .expect("frame");
    }
    let gb = hd.last_gbuffer().expect("gbuffer");
    let m = read_motion(&gpu, &gb.motion, internal).expect("motion");
    let moving: Vec<&[f32; 2]> = m.iter().filter(|v| v[0] != 0.0 || v[1] != 0.0).collect();
    let frac = moving.len() as f64 / m.len() as f64;
    let mean_x = moving.iter().map(|v| f64::from(v[0])).sum::<f64>() / moving.len().max(1) as f64;
    println!(
        "object shift: {} moving px ({frac:.5}), mean dx {mean_x:.2}",
        moving.len()
    );
    assert!(!moving.is_empty(), "the shifted sprites must have motion");
    assert!(frac < 0.15, "the background stays still: {frac}");
    assert!(mean_x < 0.0, "moved right -> vector points left: {mean_x}");
}

#[test]
fn the_depth_of_field_lens_runs_before_the_upscale() {
    let Some(gpu) = gpu_or_skip("dof_path") else {
        return;
    };
    let s = scene();
    let st = stage(&s, &s.depth);
    let (pass, lens) = (DioramaPass::new(&gpu), LensPass::new(&gpu));
    let mut out = Vec::new();
    for dof in [0.0, 0.9] {
        let mut hd = Hd2dTemporal::new(&gpu, (640, 360), (1280, 720), false);
        let look = Hd2dLook {
            dof,
            ..Hd2dLook::default()
        };
        for _ in 0..4 {
            hd.render(&gpu, &pass, &lens, &st, &s.markers, &look)
                .expect("frame");
        }
        out.push(hd.upscaler().read_output(&gpu).expect("readback"));
    }
    assert!(out[0].iter().any(|&c| c > 8));
    assert!(mae(&out[0], &out[1]) > 0.05, "DoF must change the picture");
}

#[test]
fn the_fallback_is_chosen_without_metalfx() {
    let Some(gpu) = gpu_or_skip("fallback_choice") else {
        return;
    };
    let hd = Hd2dTemporal::new(&gpu, (320, 180), (640, 360), true);
    // Without the `metalfx` feature (or off macOS) this must be the portable
    // path with a reason; with it, either is legal.
    if hd.backend() == TemporalBackend::Portable {
        assert!(hd.upscaler().fallback_reason.is_some());
    }
}

#[cfg(all(feature = "metalfx", target_os = "macos"))]
mod metalfx {
    use super::*;

    fn metalfx_run(
        gpu: &GpuContext,
        internal: (u32, u32),
        frames: u32,
        sign: Option<(f32, f32)>,
    ) -> Option<Run> {
        let run = run_static_with(gpu, internal, true, frames, sign);
        if run.backend != TemporalBackend::MetalFx {
            println!("SKIP metalfx: {:?}", run.reason);
            return None;
        }
        Some(run)
    }

    #[test]
    #[ignore = "needs a real Metal device with MetalFX"]
    fn metalfx_static_camera_is_stable_and_artefact_free() {
        let Some(gpu) = gpu_or_skip("metalfx_static") else {
            return;
        };
        let s = scene();
        let reference = native(&gpu, &s, &flat_look(), OUTPUT_5K);
        for internal in [THIRD, HALF] {
            let Some(run) = metalfx_run(&gpu, internal, 28, None) else {
                return;
            };
            assert_eq!(
                run.last.len(),
                OUTPUT_5K.0 as usize * OUTPUT_5K.1 as usize * 4
            );
            let baseline = baseline_for(&gpu, &s, internal);
            check_static("metalfx", internal, &run, &reference, &baseline);
        }
    }

    /// The jitter sign MetalFX wants is not in the headers: measure all four.
    #[test]
    #[ignore = "needs a real Metal device with MetalFX"]
    fn metalfx_jitter_sign_probe() {
        let Some(gpu) = gpu_or_skip("metalfx_sign") else {
            return;
        };
        let s = scene();
        let reference = native(&gpu, &s, &flat_look(), OUTPUT_5K);
        let mut best = (f64::MAX, (0.0, 0.0));
        for sign in [(1.0, 1.0), (-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0)] {
            let Some(run) = metalfx_run(&gpu, THIRD, 28, Some(sign)) else {
                return;
            };
            let shimmer = mae(&run.prev, &run.last);
            let err = mae(&run.last, &reference);
            println!(
                "jitter sign {sign:?}: frame-to-frame mae {shimmer:.4}, vs native mae {err:.3}"
            );
            if err < best.0 {
                best = (err, sign);
            }
        }
        println!("best jitter sign by error vs native: {:?}", best.1);
    }

    #[test]
    #[ignore = "needs a real Metal device with MetalFX"]
    fn metalfx_output_is_close_to_the_portable_one() {
        let Some(gpu) = gpu_or_skip("metalfx_vs_portable") else {
            return;
        };
        let Some(m) = metalfx_run(&gpu, THIRD, 28, None) else {
            return;
        };
        let p = run_static(&gpu, THIRD, false, 28);
        let d = mae(&m.last, &p.last);
        println!("metalfx vs portable mae {d:.3}");
        assert!(d < 12.0, "the two upscalers should agree broadly: {d}");
    }
}

/// The contact sheet (only when `RF_W39_SHEET_DIR` is set): two crops of the
/// 5K frame, each as native | bilinear from the internal frame | portable |
/// MetalFX (when available, else portable again).
#[test]
#[ignore = "writes docs/screens/w39 when RF_W39_SHEET_DIR is set"]
fn contact_sheet() {
    let Some(dir) = std::env::var_os("RF_W39_SHEET_DIR") else {
        return;
    };
    let Some(gpu) = gpu_or_skip("contact_sheet") else {
        return;
    };
    let s = scene();
    let reference = native(&gpu, &s, &flat_look(), OUTPUT_5K);
    let internal = THIRD;
    let baseline = baseline_for(&gpu, &s, internal);
    let portable = run_static(&gpu, internal, false, 28);
    let mfx = run_static(&gpu, internal, true, 28);
    println!(
        "sheet backends: portable={:?} second={:?} ({:?})",
        portable.backend, mfx.backend, mfx.reason
    );
    // Two 256x144 windows of the 5K frame with the most edge energy in the
    // native render (a deterministic pick), not overlapping.
    let (cw, ch) = (256usize, 144usize);
    let (ow, oh) = (OUTPUT_5K.0 as usize, OUTPUT_5K.1 as usize);
    let energy = |cx: usize, cy: usize| -> u64 {
        let mut e = 0u64;
        for y in (cy..cy + ch).step_by(2) {
            for x in (cx..cx + cw).step_by(2) {
                let i = (y * ow + x) * 4;
                for c in 0..3 {
                    e += u64::from(reference[i + c].abs_diff(reference[i + 8 + c]));
                    e += u64::from(reference[i + c].abs_diff(reference[i + ow * 8 + c]));
                }
            }
        }
        e
    };
    let mut scored: Vec<(u64, usize, usize)> = Vec::new();
    for cy in (0..oh - ch - 8).step_by(72) {
        for cx in (0..ow - cw - 8).step_by(128) {
            scored.push((energy(cx, cy), cx, cy));
        }
    }
    scored.sort_by(|a, b| b.cmp(a));
    let first = scored[0];
    let second = scored
        .iter()
        .find(|c| c.1.abs_diff(first.1) >= cw * 4 || c.2.abs_diff(first.2) >= ch * 4)
        .copied()
        .unwrap_or(scored[1]);
    let crops = [(first.1, first.2), (second.1, second.2)];
    println!("crops at {crops:?}");
    let cols = 4usize;
    let (sw, sh) = (cw * cols + (cols - 1) * 4, ch * crops.len() + 4);
    let mut sheet = vec![30u8; sw * sh * 4];
    for px in sheet.chunks_exact_mut(4) {
        px[3] = 255;
    }
    let panels: [&Vec<u8>; 4] = [&reference, &baseline, &portable.last, &mfx.last];
    for (row, &(cx, cy)) in crops.iter().enumerate() {
        for (col, img) in panels.iter().enumerate() {
            for y in 0..ch {
                let from = ((cy + y) * ow + cx) * 4;
                let to = ((row * (ch + 4) + y) * sw + col * (cw + 4)) * 4;
                sheet[to..to + cw * 4].copy_from_slice(&img[from..from + cw * 4]);
            }
        }
    }
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let png = rf_renderer::png::encode_rgba(&sheet, sw as u32, sh as u32);
    println!("sheet {sw}x{sh}, {} bytes", png.len());
    std::fs::write(dir.join("temporal-upscale-sheet.png"), png).unwrap();
}
