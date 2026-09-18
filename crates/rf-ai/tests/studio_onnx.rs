//! Upscale Studio against a REAL ONNX model (ticket W16-02, criterion 4's
//! "the real model is an ignored test that runs locally").
//!
//! `#![cfg(feature = "onnx")]` at the top of this FILE, same reasoning as
//! `onnx_bench.rs`: the whole binary does not exist in a default build,
//! so `cargo test --workspace` needs no runtime, no model and no network.
//! The one test in here is additionally `#[ignore]`d because it needs a
//! fetched model and a fetched ONNX Runtime dylib staged on disk first.
//!
//! Run explicitly, after `scripts/fetch-ai-upscale-model.sh` and
//! `scripts/fetch-onnx-runtime.sh`:
//!
//! ```text
//! ORT_DYLIB_PATH="$(scripts/fetch-onnx-runtime.sh --print-path)" \
//! RF_AI_MODEL_PATH="$(scripts/fetch-ai-upscale-model.sh --print-path)" \
//!   cargo test -p rf-ai --features onnx --test studio_onnx -- --ignored --nocapture
//! ```
//!
//! Unlike `onnx_bench.rs` this does not tile a full frame or time
//! anything — it exercises exactly the path the Upscale Studio window
//! calls (`rf_ai::studio::build_studio_pack`) against one real tile-sized
//! input, and its whole assertion is the one criterion 4 names: the
//! output dimensions are the input's times the model's declared scale.
#![cfg(feature = "onnx")]

use std::collections::BTreeMap;
use std::path::PathBuf;

use rf_ai::onnx::OnnxUpscaler;
use rf_ai::pack::asset_hash;
use rf_ai::pipeline::ExtractedAsset;
use rf_ai::studio::{self, ModelAttribution, PostProcessOptions, StudioOptions};

/// The default fetched model (`ai-model-manifest.toml`'s
/// `realesrgan-x4-onnx-fp32` row) takes a STATIC 64x64 input — see
/// `onnx_bench.rs`'s `TILE` constant doc for the verified-empirically
/// note. `crates/rf-ai/ai-model-manifest.toml`'s
/// `realesrgan-general-x4v3-onnx-compact` row is 128x128 instead; either
/// can be pointed at via `RF_AI_MODEL_PATH` / `RF_AI_MODEL_INPUT`.
fn model_input_side() -> u32 {
    std::env::var("RF_AI_MODEL_INPUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

fn model_scale() -> u32 {
    std::env::var("RF_AI_MODEL_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4)
}

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

/// One opaque solid-colour tile at the model's expected input side, so no
/// tiling logic is needed here — just the studio path itself.
fn fixture_asset(side: u32) -> ExtractedAsset {
    let indexed = vec![0u8; (side * side) as usize];
    let palette = vec![200, 60, 30, 255];
    ExtractedAsset {
        asset_hash: asset_hash(&indexed, &palette),
        width: side,
        height: side,
        indexed_pixels: indexed,
        palette,
    }
}

#[test]
#[ignore = "needs a fetched model + ORT_DYLIB_PATH; see scripts/fetch-ai-upscale-model.sh / scripts/fetch-onnx-runtime.sh"]
fn a_real_model_produces_the_declared_output_dimensions_through_the_studio() {
    let side = model_input_side();
    let scale = model_scale();

    let upscaler = OnnxUpscaler::load(&dylib_path(), &model_path(), "studio-onnx-test", scale)
        .expect("model + runtime must load — see this file's module doc for how to stage them");

    let asset = fixture_asset(side);
    let options = StudioOptions {
        post_process: PostProcessOptions {
            requantize: true,
            intermediate_shades: 2,
            edge_mask: true,
        },
        attribution: ModelAttribution {
            model_name: "studio-onnx-test".to_string(),
            license: "see ai-model-manifest.toml".to_string(),
            version: "n/a".to_string(),
        },
    };

    let pack = studio::build_studio_pack(
        "studio-onnx-test-pack",
        "rom-fixture",
        std::slice::from_ref(&asset),
        &[],
        &upscaler,
        &BTreeMap::new(),
        &options,
    )
    .expect("the studio pipeline must build a pack from one real inference call");

    let image = &pack.built.images[&asset.asset_hash];
    assert_eq!(image.width, side * scale, "declared scale was not honoured");
    assert_eq!(
        image.height,
        side * scale,
        "declared scale was not honoured"
    );
}
