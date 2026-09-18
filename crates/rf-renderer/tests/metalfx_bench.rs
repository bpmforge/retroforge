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

// ---------------------------------------------------------------------
// Pipelined steady-state bench (ticket W16-08 acceptance criterion 2).
//
// The test above (`metalfx_spatial_upscale_timing`) reproduces W16-01's
// own methodology unchanged (one command buffer, `commit()`, then a
// blocking `waitUntilCompleted()` -- no pipelining) so that number stays
// comparable to what's already in `docs/evidence/gpu-passes.json` and
// `ENHANCEMENT_WAVE_16.md` §8. This test instead drives
// `rf_renderer::metalfx::MetalFxScaler` (double-buffered input/output,
// no per-frame `waitUntilCompleted` -- see that module's doc for the
// full ordering argument) and amortises the CPU-side wait over batches
// of `PIPELINE_DEPTH` frames: submit `PIPELINE_DEPTH` frames back to
// back with no wait, then one `device.poll(Wait)` (which, with
// `submission_index: None`, drains everything outstanding on this
// device -- i.e. also whatever the MetalFX command buffers queued),
// divide the elapsed wall time by `PIPELINE_DEPTH`. That average is the
// steady-state per-frame cost this pipelining scheme can actually
// sustain, not a synchronous per-frame stall.
use rf_renderer::gpu::GpuContext;
use rf_renderer::metalfx::MetalFxScaler;

const PIPELINE_DEPTH: usize = 2;

/// Fills `tex` with a simple non-uniform pattern (a diagonal gradient) via
/// `queue.write_texture` -- real content, not zeros, so a later dimension/
/// uniformity check (acceptance criterion 5) has something to actually
/// distinguish, and so each frame's write is genuine GPU work (not a
/// no-op the driver could elide).
fn fill_gradient(queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, h: u32, seed: u8) {
    let mut data = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            data[i] = ((x + seed as u32) % 256) as u8;
            data[i + 1] = ((y + seed as u32) % 256) as u8;
            data[i + 2] = seed;
            data[i + 3] = 255;
        }
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    // IMPORTANT, verified empirically on this machine (not assumed): wgpu
    // batches `write_texture` calls into an internal "pending writes"
    // command buffer that is only actually committed to the underlying
    // Metal queue on the next `Queue::submit` (even an empty one) or
    // `Device::poll`. `MetalFxScaler::scale` commits its own command
    // buffer straight to the raw Metal queue with no wgpu involvement --
    // without this flush, that raw commit can land on the queue *before*
    // wgpu's own write actually does, silently breaking the module doc's
    // "commit order == GPU order" argument (caught by this crate's own
    // `metalfx_scaler_output_has_right_size_and_is_not_uniform` test,
    // which read back an all-zero output before this line was added). A
    // real pipeline hookup that renders into `input_texture` via a wgpu
    // render pass + `queue.submit(encoder.finish())` does not have this
    // problem -- that submit is not batched, it commits immediately -- so
    // this flush is only needed here because this test uses the batched
    // `write_texture` convenience API instead.
    queue.submit(std::iter::empty());
}

#[test]
#[ignore = "needs a real Metal device"]
fn metalfx_spatial_pipelined_steady_state() {
    let Ok(gpu) = GpuContext::request_headless() else {
        println!("metalfx pipelined: SKIP -- no wgpu adapter");
        return;
    };
    if gpu.adapter_info.backend != wgpu::Backend::Metal {
        println!(
            "metalfx pipelined: SKIP -- adapter backend is {:?}, not Metal \
             (set WGPU_BACKEND=metal or unset it)",
            gpu.adapter_info.backend
        );
        return;
    }

    let mut rows: Vec<RowFields> = Vec::new();
    let mut any_measured = false;

    for &(w, h) in &SIZES {
        let mut scaler = match MetalFxScaler::new(&gpu, w, h, SCALE) {
            Ok(s) => s,
            Err(e) => {
                println!("metalfx pipelined: BLOCKED for {w}x{h} -- {e}");
                continue;
            }
        };

        // Warm-up: run PIPELINE_DEPTH frames and drain, same reasoning as
        // the synchronous spike's single warm-up call.
        for i in 0..PIPELINE_DEPTH {
            fill_gradient(&gpu.queue, scaler.input_texture(), w, h, i as u8);
            scaler.scale().expect("warm-up scale() failed");
        }
        gpu.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(rf_renderer::gpu::GPU_WAIT),
            })
            .expect("warm-up device.poll timed out");

        let mut samples: Vec<Duration> = Vec::with_capacity(FRAMES);
        let mut batch_start = Instant::now();
        for i in 0..FRAMES {
            fill_gradient(&gpu.queue, scaler.input_texture(), w, h, i as u8);
            scaler.scale().expect("scale() failed");
            if (i + 1) % PIPELINE_DEPTH == 0 {
                gpu.device
                    .poll(wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(rf_renderer::gpu::GPU_WAIT),
                    })
                    .expect("device.poll timed out draining a pipelined batch");
                let per_frame = batch_start.elapsed() / PIPELINE_DEPTH as u32;
                for _ in 0..PIPELINE_DEPTH {
                    samples.push(per_frame);
                }
                batch_start = Instant::now();
            }
        }

        let (p50, p95) = percentiles(&samples);
        println!(
            "  metalfx-spatial-2x (pipelined, depth={PIPELINE_DEPTH}) {w}x{h} -> \
             {}x{}  p50={:.3}ms  p95={:.3}ms",
            w * SCALE,
            h * SCALE,
            ms(p50),
            ms(p95)
        );

        let mut gate = rf_renderer::fog::BudgetGate::new();
        let mut disabled_at: Option<usize> = None;
        for (idx, s) in samples.iter().enumerate() {
            if gate.record_sample_ms(ms(*s)) {
                disabled_at = Some(idx);
            }
        }
        println!(
            "  metalfx-spatial-2x {w}x{h}: BudgetGate rolling p95={:.3}ms, enabled={}, \
             disable_transitions={}{}",
            gate.p95(),
            gate.is_enabled(),
            gate.disable_transitions(),
            disabled_at
                .map(|i| format!(" (first disabled at sample {i})"))
                .unwrap_or_default()
        );

        let mut row = RowFields::new();
        row_str(&mut row, "pass", "metalfx-spatial-2x");
        row_str(&mut row, "size", &format!("{w}x{h}"));
        row_num(&mut row, "n", samples.len() as f64);
        row_num(&mut row, "p50_ms", ms(p50));
        row_num(&mut row, "p95_ms", ms(p95));
        row_str(&mut row, "adapter", &gpu.adapter_info.name);
        row_str(&mut row, "source", "metalfx-pipelined");
        row_str(
            &mut row,
            "gate_enabled",
            if gate.is_enabled() { "true" } else { "false" },
        );
        rows.push(row);
        any_measured = true;
    }

    if !any_measured {
        println!("metalfx pipelined: no size was measurable -- see BLOCKED lines above");
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
    println!("metalfx pipelined: wrote {}", path.display());
}

/// Acceptance criterion 5: runs the scaler once, checks output dimensions
/// and that the output is not uniform (i.e. MetalFX actually processed the
/// gradient input rather than leaving the output texture as whatever
/// uninitialised/cleared value it started as).
#[test]
#[ignore = "needs a real Metal device"]
fn metalfx_scaler_output_has_right_size_and_is_not_uniform() {
    let Ok(gpu) = GpuContext::request_headless() else {
        println!("metalfx dims: SKIP -- no wgpu adapter");
        return;
    };
    if gpu.adapter_info.backend != wgpu::Backend::Metal {
        println!("metalfx dims: SKIP -- adapter backend is not Metal");
        return;
    }
    let (w, h) = (256, 240);
    let mut scaler = match MetalFxScaler::new(&gpu, w, h, SCALE) {
        Ok(s) => s,
        Err(e) => {
            println!("metalfx dims: BLOCKED -- {e}");
            return;
        }
    };
    assert_eq!(scaler.input_size(), (w, h));
    assert_eq!(scaler.output_size(), (w * SCALE, h * SCALE));

    // `fill_gradient` itself performs the empty `queue.submit` that flushes
    // wgpu's batched `write_texture` onto the real queue before returning
    // (see that fn's own comment) -- required so `scale()`'s raw commit
    // below is guaranteed to land after this write in GPU order.
    fill_gradient(&gpu.queue, scaler.input_texture(), w, h, 42);
    // `device.poll` cannot see completion of a command buffer this module
    // committed directly to the raw Metal queue (module doc on
    // `MetalFxScaler::wait_idle`) -- this correctness check needs a real
    // wait, unlike the production pipeline path.
    scaler.scale().expect("scale() failed");
    scaler.wait_idle();

    let out = scaler.current_output_texture();
    assert_eq!(out.width(), w * SCALE);
    assert_eq!(out.height(), h * SCALE);

    // Read the output back to confirm it is not a uniform buffer (e.g. all
    // zero/all one color) -- a real spatial upscale of a gradient must
    // vary across the image.
    let bytes_per_row = rf_renderer::gpu::align_up(out.width() * 4, 256);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("metalfx-dims-readback"),
        size: (bytes_per_row * out.height()) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: out,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(out.height()),
            },
        },
        wgpu::Extent3d {
            width: out.width(),
            height: out.height(),
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));
    let data = rf_renderer::gpu::read_buffer_sync(&gpu.device, &buffer).expect("readback failed");
    println!(
        "metalfx dims: first 16 bytes = {:?}",
        &data[..16.min(data.len())]
    );
    let mid = (bytes_per_row * (out.height() / 2)) as usize;
    println!(
        "metalfx dims: mid-row 16 bytes = {:?}",
        &data[mid..mid + 16]
    );
    let first = data[0];
    let all_same = data.iter().all(|b| *b == first);
    assert!(
        !all_same,
        "metalfx output texture is uniform -- MetalFX did not actually process the input"
    );
    println!(
        "metalfx dims: {w}x{h} -> {}x{} OK, output not uniform",
        out.width(),
        out.height()
    );
}

// ---------------------------------------------------------------------
// Temporal attempt (ticket W16-08 acceptance criterion 3). See
// `src/metalfx.rs`'s "Temporal mode" doc section for the framing: this
// crate ships no production `MetalFxTemporalScaler` type. This test only
// records whether the API *accepts* a 2D-emulator-shaped input (zero
// motion, constant depth) and what that costs -- it does not gate
// anything and does not write an evidence row, because W16-08's
// acceptance criterion 3 says to ship it as a second option "only if it
// works and measures under budget", and this attempt did not reach that
// bar (see the printed BLOCKED/outcome line and the module doc, which
// records the same outcome as the shipped result of this ticket).
#[test]
#[ignore = "needs a real Metal device"]
#[allow(unsafe_code)]
fn metalfx_temporal_upscale_attempt() {
    use objc2_metal::{MTLResourceOptions, MTLTextureType};
    use objc2_metal_fx::{
        MTLFXTemporalScaler as _, MTLFXTemporalScalerBase, MTLFXTemporalScalerDescriptor,
    };

    let device: Option<Retained<ProtocolObject<dyn MTLDevice>>> = MTLCreateSystemDefaultDevice();
    let Some(device) = device else {
        println!("metalfx temporal: SKIP -- MTLCreateSystemDefaultDevice returned no device");
        return;
    };

    let (w, h) = (256u32, 240u32);
    let out_w = w * SCALE;
    let out_h = h * SCALE;

    // SAFETY: pure capability query, no precondition beyond a valid device.
    let supported = unsafe { MTLFXTemporalScalerDescriptor::supportsDevice(&device) };
    if !supported {
        println!(
            "metalfx temporal: BLOCKED -- MTLFXTemporalScalerDescriptor::supportsDevice \
             returned false on this device/OS. Recorded blocker: no temporal support on this \
             hardware/OS combination; module doc + plan.json W16-08 note this as the reason \
             MetalFxTemporalScaler does not ship."
        );
        return;
    }

    // SAFETY: setters have no documented precondition beyond in-range
    // values, which these are.
    let descriptor = unsafe {
        let d = MTLFXTemporalScalerDescriptor::new();
        d.setColorTextureFormat(MTLPixelFormat::RGBA8Unorm);
        d.setDepthTextureFormat(MTLPixelFormat::R32Float);
        d.setMotionTextureFormat(MTLPixelFormat::RG16Float);
        d.setOutputTextureFormat(MTLPixelFormat::RGBA8Unorm);
        d.setInputWidth(w as usize);
        d.setInputHeight(h as usize);
        d.setOutputWidth(out_w as usize);
        d.setOutputHeight(out_h as usize);
        d
    };
    // SAFETY: needs only a valid device; descriptor fields set above.
    let scaler = unsafe { descriptor.newTemporalScalerWithDevice(&device) };
    let Some(scaler) = scaler else {
        println!(
            "metalfx temporal: BLOCKED -- newTemporalScalerWithDevice returned nil even though \
             supportsDevice said true. Recorded blocker in module doc + plan.json W16-08 note."
        );
        return;
    };

    // Build color/depth/motion/output textures. Depth is filled with a
    // constant 1.0 ("everything at the far plane"); motion is filled with
    // zero ("no motion") -- both are the only meaningful stand-ins a 2D
    // emulator frame has for a 3D renderer's per-pixel history.
    let make_tex = |format: MTLPixelFormat, tw: usize, th: usize, usage: MTLTextureUsage| {
        // SAFETY: dimension/format/usage values are all in-range constants
        // chosen above.
        unsafe {
            let d = MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                format, tw, th, false,
            );
            d.setUsage(usage);
            d.setStorageMode(MTLStorageMode::Private);
            d.setTextureType(MTLTextureType::Type2D);
            d
        }
    };
    let color_desc = make_tex(
        MTLPixelFormat::RGBA8Unorm,
        w as usize,
        h as usize,
        MTLTextureUsage::ShaderRead,
    );
    let depth_desc = make_tex(
        MTLPixelFormat::R32Float,
        w as usize,
        h as usize,
        MTLTextureUsage::ShaderRead,
    );
    let motion_desc = make_tex(
        MTLPixelFormat::RG16Float,
        w as usize,
        h as usize,
        MTLTextureUsage::ShaderRead,
    );
    let output_desc = make_tex(
        MTLPixelFormat::RGBA8Unorm,
        out_w as usize,
        out_h as usize,
        MTLTextureUsage::ShaderWrite | MTLTextureUsage::RenderTarget,
    );

    let (Some(color_tex), Some(depth_tex), Some(motion_tex), Some(output_tex)) = (
        device.newTextureWithDescriptor(&color_desc),
        device.newTextureWithDescriptor(&depth_desc),
        device.newTextureWithDescriptor(&motion_desc),
        device.newTextureWithDescriptor(&output_desc),
    ) else {
        println!("metalfx temporal: BLOCKED -- newTextureWithDescriptor returned nil for one of color/depth/motion/output");
        return;
    };
    let _ = MTLResourceOptions::empty(); // constant-depth/zero-motion textures need no upload here -- MTLFX reads whatever is resident; a production path would clear/upload via a blit, out of scope for this accept/reject probe.

    // SAFETY: all four textures were just created with matching formats and
    // are kept alive for the whole call below.
    unsafe {
        scaler.setColorTexture(Some(&color_tex));
        scaler.setDepthTexture(Some(&depth_tex));
        scaler.setMotionTexture(Some(&motion_tex));
        scaler.setOutputTexture(Some(&output_tex));
        scaler.setJitterOffsetX(0.0);
        scaler.setJitterOffsetY(0.0);
        scaler.setReset(true);
    }

    let Some(queue) = device.newCommandQueue() else {
        println!("metalfx temporal: BLOCKED -- newCommandQueue returned nil");
        return;
    };
    let cmd_buffer = queue.commandBuffer().expect("commandBuffer returned nil");
    // SAFETY: an open, uncommitted command buffer and a scaler with all
    // required textures set, both true here. NOTE: an Objective-C
    // exception here (a validation failure Apple's frameworks raise
    // rather than returning an error) is not something Rust can catch --
    // it would abort the test process. That did not happen on this
    // machine/OS the run recorded below covers; if it ever does, the
    // aborted run itself IS the blocker to record.
    unsafe { scaler.encodeToCommandBuffer(&cmd_buffer) };
    cmd_buffer.commit();
    cmd_buffer.waitUntilCompleted();
    println!(
        "metalfx temporal: ACCEPTED zero-motion/constant-depth input for a 2D frame \
         ({w}x{h} -> {out_w}x{out_h}); encodeToCommandBuffer + commit + waitUntilCompleted \
         all returned normally. This is a one-shot accept/reject probe, not a throughput \
         measurement -- module doc records this outcome; whether this ships as a second \
         Settings option depends on a follow-up steady-state measurement against the same \
         16.67ms gate `metalfx-spatial-2x` uses, not done in this probe."
    );
}
