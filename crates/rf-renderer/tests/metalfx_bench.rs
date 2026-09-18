//! MetalFX spatial-upscale timing spike (ticket W16-01; `docs/design/
//! ENHANCEMENT_WAVE_16.md` §7-8, acceptance criterion 3).
//!
//! `#![cfg(...)]` at the top of this FILE means the whole test binary
//! does not exist unless the `metalfx` feature AND macOS are both true —
//! `cargo test --workspace` never touches this file by default.
//!
//! Every test is additionally `#[ignore]`d — it needs a real Metal
//! device, which is not guaranteed even on macOS (a sandboxed CI runner
//! might have none). Run explicitly:
//!
//! ```text
//! cargo test --release -p rf-renderer --features metalfx --test metalfx_bench -- --ignored --nocapture
//! ```
//!
//! `objc2`/`objc2-metal`/`objc2-metal-fx` are MIT (madsmtm/objc2 project;
//! verified LICENSE.txt 2026-09-17) — this is FFI to a first-party Apple
//! framework, the lowest-risk of the three real-time runtimes
//! `docs/design/ENHANCEMENT_WAVE_16.md` §7 ranks (MetalFX, burn-wgpu,
//! ort/CoreML).
#![cfg(all(feature = "metalfx", target_os = "macos"))]

// `#[path]` rather than a bare `mod bench_support;` (the file's own name
// is `minijson`, not `bench_support`) -- reuses `bench-passes`'s minijson
// helper in-crate rather than duplicating its ~300 lines a second time;
// `include!` was tried first and rejected: it splices the file's `//!`
// inner doc comments into a position rustc refuses to parse them at
// (E0753), whereas `#[path]` loads the file as a real module, the same
// mechanism `bench_passes/main.rs`'s own `mod minijson;` uses.
#[path = "../src/bin/bench_passes/minijson.rs"]
mod bench_support;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLPixelFormat,
    MTLStorageMode, MTLTextureDescriptor, MTLTextureUsage,
};
use objc2_metal_fx::{MTLFXSpatialScaler, MTLFXSpatialScalerBase, MTLFXSpatialScalerDescriptor};

use bench_support::{row_num, row_str, RowFields};

const FRAMES: usize = 300; // same protocol as bench-passes's GPU shader-chain rows.
const SIZES: [(u32, u32); 2] = [(256, 240), (512, 448)];
const SCALE: u32 = 2; // MetalFX spatial is typically run at 1.3x-2x, unlike ESRGAN's fixed 4x.

fn evidence_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/evidence/gpu-passes.json")
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

#[test]
#[ignore = "needs a real Metal device"]
// This whole function is Objective-C FFI (objc2/objc2-metal/objc2-metal-fx):
// every `unsafe` block below carries its own `SAFETY:` comment satisfying
// `undocumented_unsafe_blocks`; this one `#[allow(unsafe_code)]` covers the
// workspace's `unsafe_code = "warn"` lint for the whole function rather
// than repeating the same allow a dozen times.
#[allow(unsafe_code)]
fn metalfx_spatial_upscale_timing() {
    // SAFETY: `MTLCreateSystemDefaultDevice` is a pure query of the
    // system's default GPU, documented to return either a valid retained
    // device or NULL (mapped to `None` here) -- no preconditions on the
    // caller.
    let device: Option<Retained<ProtocolObject<dyn MTLDevice>>> = MTLCreateSystemDefaultDevice();
    let Some(device) = device else {
        println!("metalfx: SKIP -- MTLCreateSystemDefaultDevice returned no device");
        return;
    };
    println!("metalfx: device = {}", device.name());

    let mut rows: Vec<RowFields> = Vec::new();
    let mut any_measured = false;

    for &(w, h) in &SIZES {
        let out_w = w * SCALE;
        let out_h = h * SCALE;

        // SAFETY: `supportsDevice` is a capability query documented to
        // accept any valid `MTLDevice`; no other precondition.
        let supported = unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(&device) };
        if !supported {
            println!(
                "metalfx: BLOCKED for {w}x{h} -- MTLFXSpatialScalerDescriptor::supportsDevice \
                 returned false on this device/OS"
            );
            continue;
        }

        // SAFETY: `new` / the property setters below have no documented
        // preconditions beyond "a live descriptor object"; values set are
        // in-range for the declared NSUInteger/enum types.
        let descriptor = unsafe {
            let d = MTLFXSpatialScalerDescriptor::new();
            d.setColorTextureFormat(MTLPixelFormat::RGBA8Unorm);
            d.setOutputTextureFormat(MTLPixelFormat::RGBA8Unorm);
            d.setInputWidth(w as usize);
            d.setInputHeight(h as usize);
            d.setOutputWidth(out_w as usize);
            d.setOutputHeight(out_h as usize);
            d
        };

        // SAFETY: `newSpatialScalerWithDevice` has no precondition beyond
        // a valid device; the descriptor's fields were just set above to
        // in-range values.
        let scaler = unsafe { descriptor.newSpatialScalerWithDevice(&device) };
        let Some(scaler) = scaler else {
            println!(
                "metalfx: BLOCKED for {w}x{h} -- newSpatialScalerWithDevice returned nil \
                 (supportsDevice said true, but scaler creation still failed)"
            );
            continue;
        };

        // SAFETY: `texture2DDescriptorWithPixelFormat_width_height_mipmapped`
        // and the setters below have no documented precondition beyond
        // in-range dimension/enum values, which these are.
        let color_tex_desc = unsafe {
            let d = MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                w as usize,
                h as usize,
                false,
            );
            d.setUsage(MTLTextureUsage::ShaderRead);
            d.setStorageMode(MTLStorageMode::Private);
            d
        };
        // SAFETY: same reasoning as `color_tex_desc` above.
        let output_tex_desc = unsafe {
            let d = MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                out_w as usize,
                out_h as usize,
                false,
            );
            // MTLFXSpatialScalerBase's own doc: "You are responsible for
            // providing a texture with a private storageMode" for the
            // output texture.
            d.setUsage(MTLTextureUsage::ShaderWrite | MTLTextureUsage::RenderTarget);
            d.setStorageMode(MTLStorageMode::Private);
            d
        };

        // `newTextureWithDescriptor` is a safe fn on this binding (not
        // `unsafe fn`) -- no `unsafe` block needed here.
        let color_tex = device.newTextureWithDescriptor(&color_tex_desc);
        let output_tex = device.newTextureWithDescriptor(&output_tex_desc);
        let (Some(color_tex), Some(output_tex)) = (color_tex, output_tex) else {
            println!("metalfx: BLOCKED for {w}x{h} -- newTextureWithDescriptor returned nil");
            continue;
        };

        // SAFETY: both textures were just created with matching
        // dimensions/format/usage and are kept alive for the whole loop
        // below (bound to `color_tex`/`output_tex`), satisfying the
        // setter's "must be kept alive while in use" contract.
        unsafe {
            scaler.setColorTexture(Some(&color_tex));
            scaler.setOutputTexture(Some(&output_tex));
        }

        let Some(queue) = device.newCommandQueue() else {
            println!("metalfx: BLOCKED for {w}x{h} -- newCommandQueue returned nil");
            continue;
        };

        let run_once = || {
            let cmd_buffer = queue.commandBuffer().expect("commandBuffer returned nil");
            // SAFETY: `encodeToCommandBuffer` requires an open (just
            // created, uncommitted) command buffer and a scaler with its
            // color/output textures already set -- both true here.
            unsafe { scaler.encodeToCommandBuffer(&cmd_buffer) };
            cmd_buffer.commit();
            cmd_buffer.waitUntilCompleted();
        };

        run_once(); // warm-up, same reasoning as bench-passes
        let mut samples = Vec::with_capacity(FRAMES);
        for _ in 0..FRAMES {
            let t0 = Instant::now();
            run_once();
            samples.push(t0.elapsed());
        }

        let (p50, p95) = percentiles(&samples);
        println!(
            "  metalfx-spatial {w}x{h} -> {out_w}x{out_h}  p50={:.3}ms  p95={:.3}ms",
            ms(p50),
            ms(p95)
        );
        let mut row = RowFields::new();
        row_str(&mut row, "pass", "metalfx-spatial");
        row_str(&mut row, "size", &format!("{w}x{h}"));
        row_num(&mut row, "n", samples.len() as f64);
        row_num(&mut row, "p50_ms", ms(p50));
        row_num(&mut row, "p95_ms", ms(p95));
        row_str(&mut row, "adapter", &device.name().to_string());
        row_str(&mut row, "source", "metalfx");
        rows.push(row);
        any_measured = true;
    }

    if !any_measured {
        println!("metalfx: no size was measurable on this device -- see BLOCKED lines above");
        return;
    }

    let path = evidence_path();
    let existing = bench_support::read_existing_rows(&path);
    let merged = bench_support::merge_rows(existing, rows);
    let (machine, generated_at) = {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        match bench_support::parse(&text) {
            bench_support::JVal::Obj(top) => {
                let m = top
                    .iter()
                    .find(|(k, _)| k == "machine")
                    .and_then(|(_, v)| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let g = top
                    .iter()
                    .find(|(k, _)| k == "generated_at")
                    .and_then(|(_, v)| v.as_str())
                    .unwrap_or("1970-01-01T00:00:00Z")
                    .to_string();
                (m, g)
            }
            _ => ("unknown".to_string(), "1970-01-01T00:00:00Z".to_string()),
        }
    };
    bench_support::write_doc(&path, &machine, &generated_at, merged);
    println!("metalfx: wrote {}", path.display());
}
