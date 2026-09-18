//! Upscale Studio against a REAL ONNX model (ticket W16-02, criterion 4's
//! "the real model is an ignored test that runs locally"; ticket W16-12
//! adds the second test, which exercises real tiled inference).
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
//! Unlike `onnx_bench.rs` neither test here times anything — both
//! exercise exactly the path the Upscale Studio window calls
//! (`rf_ai::studio::build_studio_pack`). The first test's input is one
//! exact tile size (criterion 4's original assertion: the output
//! dimensions are the input's times the model's declared scale). The
//! second (`a_real_model_tiles_a_sheet_larger_than_its_input_size`,
//! ticket W16-12) deliberately builds a sheet BIGGER than the model's
//! tile and not an exact multiple of it, so it is the one that actually
//! proves `crate::tiling` ran against this real model rather than just
//! passing because the input happened to already be tile-sized.
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

/// The fixture above is exactly the model's tile size, so it would pass
/// even with NO tiler at all (`OnnxUpscaler` would just run it whole).
/// This test proves tiling actually happens (ticket W16-12): two
/// differently-sized assets in one animation set make `sheet_of` lay out
/// a sheet bigger than a single tile and NOT an exact multiple of it, so
/// `crate::tiling` must reflect-pad, run more than one tile, and blend
/// the seam back together — the only way the output could come back at
/// exactly `sheet * scale` with no misaligned or missing region.
#[test]
#[ignore = "needs a fetched model + ORT_DYLIB_PATH; see scripts/fetch-ai-upscale-model.sh / scripts/fetch-onnx-runtime.sh"]
fn a_real_model_tiles_a_sheet_larger_than_its_input_size() {
    use rf_ai::animation::AnimationSet;

    let side = model_input_side();
    let scale = model_scale();

    let upscaler = OnnxUpscaler::load(
        &dylib_path(),
        &model_path(),
        "studio-onnx-tiling-test",
        scale,
    )
    .expect("model + runtime must load — see this file's module doc for how to stage them");

    // Two members side by side: sheet_of lays them out left-to-right, so
    // the sheet is `side + side/2` wide by `side` tall — wider than one
    // tile and not a clean multiple of it either.
    let a = fixture_asset(side);
    let b = fixture_asset(side / 2);
    let set = AnimationSet {
        id: "tiling-check".to_string(),
        assets: [a.asset_hash.clone(), b.asset_hash.clone()]
            .into_iter()
            .collect(),
    };

    let options = StudioOptions {
        post_process: PostProcessOptions {
            requantize: false,
            intermediate_shades: 0,
            edge_mask: false,
        },
        attribution: ModelAttribution {
            model_name: "studio-onnx-tiling-test".to_string(),
            license: "see ai-model-manifest.toml".to_string(),
            version: "n/a".to_string(),
        },
    };

    let pack = studio::build_studio_pack(
        "studio-onnx-tiling-test-pack",
        "rom-fixture",
        &[a.clone(), b.clone()],
        &[set],
        &upscaler,
        &BTreeMap::new(),
        &options,
    )
    .expect("the studio pipeline must build a pack from a real tiled inference call");

    let ia = &pack.built.images[&a.asset_hash];
    let ib = &pack.built.images[&b.asset_hash];
    assert_eq!((ia.width, ia.height), (side * scale, side * scale));
    assert_eq!(
        (ib.width, ib.height),
        (side / 2 * scale, side / 2 * scale),
        "split_sheet must recover each member's own scaled size from a tiled sheet"
    );
}
