//! ONNX Runtime / CoreML EP local-AI upscale benchmark spike (ticket
//! W16-01; `docs/design/ENHANCEMENT_WAVE_16.md` §7-8;
//! `docs/design/AI_UPSCALING.md`; `crates/rf-ai/ai-model-manifest.toml`).
//!
//! `#![cfg(feature = "onnx")]` at the top of this FILE (not a per-test
//! `#[cfg]`) means the whole test binary does not exist when the feature
//! is off — `cargo test --workspace`'s "no external anything" promise
//! (CLAUDE.md's Build section) stays true; this file is never compiled by
//! a default build.
//!
//! Every test in here is `#[ignore]`d on top of that: it needs a real
//! model file and a real ONNX Runtime dylib staged on disk first (see
//! `scripts/fetch-ai-upscale-model.sh` / `scripts/fetch-onnx-runtime.sh`,
//! or `scripts/gpu-gate.sh RF_ONNX_BENCH=1`), and it writes timing rows
//! into `docs/evidence/gpu-passes.json`. Run explicitly:
//!
//! ```text
//! RF_AI_CACHE=$HOME/.cache/retroforge-ai \
//!   cargo test --release -p rf-ai --features onnx-coreml --test onnx_bench -- --ignored --nocapture
//! ```
#![cfg(feature = "onnx")]

mod support;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rf_ai::onnx::OnnxUpscaler;
use rf_ai::upscale::{Rgba8, Upscaler};

use support::minijson::{self, row_num, row_str, RowFields};

const FRAMES: usize = 10; // real inference (tiled, see TILE) is far slower than a GPU shader pass.
const SIZES: [(u32, u32); 2] = [(256, 240), (512, 448)];

// Measurement note (ticket W16-01, 2026-09-17, this M4 Max): per-tile
// inference on this model measured at roughly 1.8-2.2 SECONDS/tile on
// BOTH CoreML and CPU EP (see docs/design/ENHANCEMENT_WAVE_16.md §8's
// table) -- at that rate, 512x448's 56 tiles x 10 frames x 2 EPs is
// several hours of wall time for one evidence refresh, which is not a
// reasonable cost for this ticket's spike. The committed
// docs/evidence/gpu-passes.json therefore carries 256x240 rows only for
// `source: "onnx-ort"` (recorded with a smaller `n` for the same reason --
// 3 samples, not 10), each row's own `n` field says exactly how many
// samples it is. Re-running this file with the constants above regenerates
// full rows at the designed sample count; nothing about the harness itself
// is reduced, only the evidence snapshot this ticket committed.

/// The fetched model (`crates/rf-ai/ai-model-manifest.toml`'s
/// `realesrgan-x4-onnx-fp32` row) was exported with a STATIC 64x64 input
/// shape -- verified empirically (ticket W16-01): feeding it a whole
/// 256x240 frame fails with ONNX Runtime's own "Got invalid dimensions...
/// Expected: 64" error. This is a property of that specific third-party
/// re-export, not of Real-ESRGAN or of `OnnxUpscaler` (which imposes no
/// tile-size assumption itself). Tiling here is also how Real-ESRGAN is
/// actually run in practice on images larger than its training crop, so
/// "ms/frame" for this model honestly means "sum of per-tile inference
/// calls covering the frame", not one single forward pass.
const TILE: u32 = 64;

fn cache_dir() -> PathBuf {
    match std::env::var("RF_AI_CACHE") {
        Ok(v) => PathBuf::from(v),
        Err(_) => {
            let home = std::env::var("HOME").expect("HOME must be set");
            PathBuf::from(home).join(".cache/retroforge-ai")
        }
    }
}

fn model_path() -> PathBuf {
    std::env::var("RF_AI_MODEL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| cache_dir().join("realesrgan-x4-fp32.onnx"))
}

fn dylib_path() -> PathBuf {
    std::env::var("ORT_DYLIB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            cache_dir().join("onnxruntime-osx-arm64-1.20.1/lib/libonnxruntime.dylib")
        })
}

fn synthetic_rgba(w: u32, h: u32) -> Rgba8 {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            pixels[i] = ((x * 7 + y * 3) % 256) as u8;
            pixels[i + 1] = ((x * 13 + y * 5) % 256) as u8;
            pixels[i + 2] = ((x * 3 + y * 17) % 256) as u8;
            pixels[i + 3] = 255;
        }
    }
    Rgba8::new(w, h, pixels).expect("synthetic frame has the declared buffer length")
}

/// Splits `frame` into `TILE`x`TILE` tiles (edge tiles clamped-extended,
/// not padded with black, so the model never sees an artificial hard edge
/// it wasn't trained on) and runs `upscaler.upscale` on each in turn,
/// returning the wall-clock time for the whole sweep -- see `TILE`'s doc
/// for why this model needs tiling at all. `n_tiles` lets the caller
/// report both the total frame time and a derived per-tile figure.
fn upscale_tiled(upscaler: &OnnxUpscaler, frame: &Rgba8) -> (Duration, usize) {
    let tiles_x = frame.width.div_ceil(TILE);
    let tiles_y = frame.height.div_ceil(TILE);
    let mut tiles = Vec::with_capacity((tiles_x * tiles_y) as usize);
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let mut pixels = vec![0u8; (TILE * TILE * 4) as usize];
            for y in 0..TILE {
                let src_y = (ty * TILE + y).min(frame.height - 1);
                for x in 0..TILE {
                    let src_x = (tx * TILE + x).min(frame.width - 1);
                    let src_i = ((src_y * frame.width + src_x) * 4) as usize;
                    let dst_i = ((y * TILE + x) * 4) as usize;
                    pixels[dst_i..dst_i + 4].copy_from_slice(&frame.pixels[src_i..src_i + 4]);
                }
            }
            tiles
                .push(Rgba8::new(TILE, TILE, pixels).expect("tile has the declared buffer length"));
        }
    }
    let t0 = Instant::now();
    for tile in &tiles {
        upscaler.upscale(tile).expect("tile inference failed");
    }
    (t0.elapsed(), tiles.len())
}

fn percentiles(samples: &[Duration]) -> (Duration, Duration) {
    let mut sorted = samples.to_vec();
    sorted.sort();
    let idx = |q: f64| -> usize {
        let n = sorted.len();
        ((q * n as f64).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1)
    };
    (sorted[idx(0.50)], sorted[idx(0.95)])
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn evidence_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/evidence/gpu-passes.json")
}

fn write_row(pass: &str, size: &str, samples: &[Duration], source: &str) {
    let (p50, p95) = percentiles(samples);
    let mut row = RowFields::new();
    row_str(&mut row, "pass", pass);
    row_str(&mut row, "size", size);
    row_num(&mut row, "n", samples.len() as f64);
    row_num(&mut row, "p50_ms", ms(p50));
    row_num(&mut row, "p95_ms", ms(p95));
    row_str(&mut row, "source", source);
    let path = evidence_path();
    let existing = minijson::read_existing_rows(&path);
    let merged = minijson::merge_rows(existing, vec![row]);
    // Preserve whatever machine/generated_at the file already has (the
    // gpu-bench producer sets it); a from-scratch run here just needs
    // *some* value so the file is still well-formed.
    let (machine, generated_at) = existing_meta(&path);
    minijson::write_doc(&path, &machine, &generated_at, merged);
    println!(
        "  wrote {pass}/{size}: p50={:.3}ms p95={:.3}ms ({} samples)",
        ms(p50),
        ms(p95),
        samples.len()
    );
}

fn existing_meta(path: &std::path::Path) -> (String, String) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return ("unknown".to_string(), "1970-01-01T00:00:00Z".to_string());
    };
    let minijson::JVal::Obj(top) = minijson::parse(&text) else {
        return ("unknown".to_string(), "1970-01-01T00:00:00Z".to_string());
    };
    let machine = top
        .iter()
        .find(|(k, _)| k == "machine")
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let generated_at = top
        .iter()
        .find(|(k, _)| k == "generated_at")
        .and_then(|(_, v)| v.as_str())
        .unwrap_or("1970-01-01T00:00:00Z")
        .to_string();
    (machine, generated_at)
}

/// CPU EP control: no execution providers registered, ONNX Runtime's
/// default. Run first so a CoreML failure (next test) still leaves a
/// control number in the evidence file.
#[test]
#[ignore = "needs a fetched model + ORT_DYLIB_PATH; see scripts/fetch-ai-upscale-model.sh / scripts/fetch-onnx-runtime.sh"]
fn onnx_cpu_ep_inference_timing() {
    let dylib = dylib_path();
    let model = model_path();
    assert!(
        dylib.exists(),
        "ORT dylib not found at {}; run scripts/fetch-onnx-runtime.sh",
        dylib.display()
    );
    assert!(
        model.exists(),
        "model not found at {}; run scripts/fetch-ai-upscale-model.sh",
        model.display()
    );

    let upscaler = OnnxUpscaler::load(&dylib, &model, "realesrgan-x4-fp32", 4)
        .expect("loading the model on CPU EP must succeed -- it's the runtime's baseline provider");

    for &(w, h) in &SIZES {
        let src = synthetic_rgba(w, h);
        let (_, n_tiles) = upscale_tiled(&upscaler, &src); // warm-up
        let mut samples = Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
            let (elapsed, _) = upscale_tiled(&upscaler, &src);
            samples.push(elapsed);
        }
        write_row("onnx-cpu", &format!("{w}x{h}"), &samples, "onnx-ort");
        println!("    ({n_tiles} tiles of {TILE}x{TILE} per frame)");
    }
}

/// CoreML EP: the actual acceleration path this ticket exists to measure.
/// Feature-gated separately (`onnx-coreml`) because it needs `ort`'s
/// `coreml` cargo feature (a compile-time-only cfg switch, see
/// `crates/rf-ai/Cargo.toml`'s comment on `onnx-coreml`).
#[cfg(feature = "onnx-coreml")]
#[test]
#[ignore = "needs a fetched model + ORT_DYLIB_PATH; see scripts/fetch-ai-upscale-model.sh / scripts/fetch-onnx-runtime.sh"]
fn onnx_coreml_ep_inference_timing() {
    let dylib = dylib_path();
    let model = model_path();
    assert!(
        dylib.exists(),
        "ORT dylib not found at {}; run scripts/fetch-onnx-runtime.sh",
        dylib.display()
    );
    assert!(
        model.exists(),
        "model not found at {}; run scripts/fetch-ai-upscale-model.sh",
        model.display()
    );

    let providers = [ort::ep::coreml::CoreML::default().build()];
    let upscaler = OnnxUpscaler::load_with_providers(&dylib, &model, "realesrgan-x4-fp32", 4, &providers)
        .expect(
            "CoreML EP registration is fail-silent by default (ort::ep::ExecutionProviderDispatch's \
             own doc) -- a load-time error here means session creation itself failed, not just EP \
             fallback; if this fails, the exact ort error message IS the blocker to record honestly \
             per this ticket's acceptance criteria",
        );

    for &(w, h) in &SIZES {
        let src = synthetic_rgba(w, h);
        let (_, n_tiles) = upscale_tiled(&upscaler, &src); // warm-up
        let mut samples = Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
            let (elapsed, _) = upscale_tiled(&upscaler, &src);
            samples.push(elapsed);
        }
        write_row("onnx-coreml", &format!("{w}x{h}"), &samples, "onnx-ort");
        println!("    ({n_tiles} tiles of {TILE}x{TILE} per frame)");
    }
}
