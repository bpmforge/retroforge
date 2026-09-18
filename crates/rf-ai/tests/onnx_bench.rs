//! ONNX Runtime / CoreML EP local-AI upscale benchmark spike (tickets
//! W16-01, W16-12; `docs/design/ENHANCEMENT_WAVE_16.md` §7-8;
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
//! `scripts/fetch-ai-upscale-model.sh` / `scripts/fetch-onnx-runtime.sh`),
//! and it writes timing rows into `docs/evidence/gpu-passes.json`. Run
//! explicitly:
//!
//! ```text
//! ORT_DYLIB_PATH="$(scripts/fetch-onnx-runtime.sh --print-path)" \
//! RF_AI_MODEL_PATH="$(MODEL=x4-fp32 scripts/fetch-ai-upscale-model.sh --print-path)" \
//!   cargo test --release -p rf-ai --features onnx-coreml --test onnx_bench -- --ignored --nocapture
//! ```
//!
//! ## No tiler in this file (ticket W16-12)
//!
//! W16-01's version of this file hand-rolled its own fixed-64px tiling
//! loop (`TILE`/`upscale_tiled`) because `OnnxUpscaler` couldn't tile
//! itself yet. It can now (`crate::tiling`, driven internally by
//! `OnnxUpscaler::upscale` from the model's OWN declared input size — see
//! `crates/rf-ai/src/onnx.rs`), so this file just calls
//! `upscaler.upscale(&frame)` like any other caller. Keeping a second,
//! parallel tiling implementation here — even one only used for timing —
//! would mean a tiling bug fixed in one place could silently persist in
//! the other's numbers.
#![cfg(feature = "onnx")]

mod support;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rf_ai::onnx::OnnxUpscaler;
use rf_ai::upscale::{Rgba8, Upscaler};

use support::minijson::{self, row_num, row_str, RowFields};

// Measurement note (ticket W16-01, 2026-09-17, this M4 Max): per-tile
// inference on the x4-fp32 model measured at roughly 1.8-2.2
// SECONDS/tile on BOTH CoreML and CPU EP (see
// docs/design/ENHANCEMENT_WAVE_16.md §8's table). At that rate 256x240
// (25 tiles at 64px/pad-8) is ~50s/frame and 512x448 (56 tiles) is
// several times that -- ticket W16-01 already declined to sample
// 512x448 at the full FRAMES count for exactly this reason, and W16-12
// inherits the same constraint: it is not a reasonable cost for a spike
// re-run on a developer's own machine. This file therefore samples ONE
// size (256x240) at a REDUCED sample count (3, not 10) -- `n` in each
// written row says exactly how many samples it is, so nothing is
// silently understated.
const FRAMES: usize = 3;
const SIZE: (u32, u32) = (256, 240);

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

/// Short label for the evidence row's pass name
/// (`onnx-tiled-<model>-<ep>`) — NOT the model's `model_id`, which stays a
/// full technical identifier. Defaults from the model file's own name so
/// a caller pointing `RF_AI_MODEL_PATH` at either ledgered model gets a
/// sensible label with no extra env var, but `RF_AI_MODEL_LABEL`
/// overrides it for anything else.
fn model_label() -> String {
    if let Ok(v) = std::env::var("RF_AI_MODEL_LABEL") {
        return v;
    }
    let path = model_path();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model");
    if stem.contains("compact") || stem.contains("x4v3") {
        "compact".to_string()
    } else {
        "x4-fp32".to_string()
    }
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

fn require_staged_artifacts(dylib: &std::path::Path, model: &std::path::Path) {
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
}

fn time_frame(upscaler: &OnnxUpscaler, src: &Rgba8) -> Vec<Duration> {
    upscaler.upscale(src).expect("warm-up inference failed"); // warm-up, not sampled
    (0..FRAMES)
        .map(|_| {
            let t0 = Instant::now();
            upscaler.upscale(src).expect("inference failed");
            t0.elapsed()
        })
        .collect()
}

/// CPU EP control: no execution providers registered, ONNX Runtime's
/// default. Run first so a CoreML failure (next test) still leaves a
/// control number in the evidence file.
#[test]
#[ignore = "needs a fetched model + ORT_DYLIB_PATH; see scripts/fetch-ai-upscale-model.sh / scripts/fetch-onnx-runtime.sh"]
fn onnx_cpu_ep_inference_timing() {
    let dylib = dylib_path();
    let model = model_path();
    require_staged_artifacts(&dylib, &model);

    let upscaler = OnnxUpscaler::load(&dylib, &model, "realesrgan-onnx-bench", 4)
        .expect("loading the model on CPU EP must succeed -- it's the runtime's baseline provider");

    let (w, h) = SIZE;
    let src = synthetic_rgba(w, h);
    let samples = time_frame(&upscaler, &src);
    write_row(
        &format!("onnx-tiled-{}-cpu", model_label()),
        &format!("{w}x{h}"),
        &samples,
        "onnx-ort",
    );
}

/// CoreML EP: the actual acceleration path this ticket exists to measure.
/// Feature-gated separately (`onnx-coreml`) because it needs `ort`'s
/// `coreml` cargo feature (a compile-time-only cfg switch, see
/// `crates/rf-ai/Cargo.toml`'s comment on `onnx-coreml`).
///
/// ## Whether CoreML actually ran
///
/// `ExecutionProviderDispatch`'s default `error_on_failure: false`
/// (`ort-2.0.0-rc.13/src/ep/mod.rs:99,243`) means an EP that could not
/// take the graph falls back to CPU SILENTLY — `load_with_providers`
/// succeeding is not proof CoreML executed a single node. `ort`'s Rust
/// API exposes no per-node provider-placement query, so this test uses
/// the two signals that ARE available and prints both rather than
/// asserting either:
///
/// 1. [`ort::ep::coreml::CoreML::is_available`] — confirms the BUILD
///    supports CoreML (it's linked into the official dylib per
///    `ai-model-manifest.toml`'s note), but says nothing about whether
///    THIS graph's ops were accepted.
/// 2. Timing vs. the CPU-EP control row: identical numbers across EPs is
///    the signature W16-01 already recorded for the x4-fp32 model on
///    this machine (see that ticket's CAVEAT) — genuine CoreML
///    acceleration on Apple's Neural Engine/GPU for a small conv net
///    should differ measurably from CPU, so "no difference" is treated
///    as evidence of a silent fallback, not proof of a working EP with
///    poor speedup.
#[cfg(feature = "onnx-coreml")]
#[test]
#[ignore = "needs a fetched model + ORT_DYLIB_PATH; see scripts/fetch-ai-upscale-model.sh / scripts/fetch-onnx-runtime.sh"]
fn onnx_coreml_ep_inference_timing() {
    use ort::ep::ExecutionProvider;

    let dylib = dylib_path();
    let model = model_path();
    require_staged_artifacts(&dylib, &model);

    let coreml = ort::ep::coreml::CoreML::default();
    let build_supports_coreml = coreml.is_available().unwrap_or(false);
    println!("  CoreML::is_available() (build support, not per-graph placement) = {build_supports_coreml}");

    let providers = [coreml.build()];
    let upscaler =
        OnnxUpscaler::load_with_providers(&dylib, &model, "realesrgan-onnx-bench", 4, &providers)
            .expect(
                "CoreML EP registration is fail-silent by default (ort::ep::ExecutionProviderDispatch's \
                 own doc) -- a load-time error here means session creation itself failed, not just EP \
                 fallback; if this fails, the exact ort error message IS the blocker to record honestly \
                 per this ticket's acceptance criteria",
            );

    let (w, h) = SIZE;
    let src = synthetic_rgba(w, h);
    let samples = time_frame(&upscaler, &src);
    write_row(
        &format!("onnx-tiled-{}-coreml", model_label()),
        &format!("{w}x{h}"),
        &samples,
        "onnx-ort",
    );

    println!(
        "  NOTE: compare this row's p50_ms to onnx-tiled-{}-cpu's -- W16-01 found IDENTICAL \
         numbers across EPs for the x4-fp32 model on this machine, which it recorded as the \
         signature of a silent CoreML->CPU fallback rather than proof CoreML ran.",
        model_label()
    );
}
