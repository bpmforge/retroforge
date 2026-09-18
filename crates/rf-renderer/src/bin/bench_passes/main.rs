//! GPU pass benchmark harness (ticket W16-01; `docs/design/
//! ENHANCEMENT_WAVE_16.md` §7-8; `docs/design/AI_UPSCALING.md`).
//!
//! Times the existing shader-chain passes (`nearest`, `xbr`, `crt`) plus a
//! stub "neural" compute pass (see `neural_stub.wgsl` — NOT a trained
//! model; a fixed 4x conv-like workload stand-in) at 256x240 and 512x448,
//! 300 frames each, and writes p50/p95 wall-clock ms per (pass, size) into
//! `docs/evidence/gpu-passes.json` — the one JSON file `rf-ai`'s ONNX/
//! MetalFX spikes (`crates/rf-ai/tests/onnx_bench.rs`,
//! `crates/rf-ai/tests/metalfx_bench.rs`) also write rows into, so
//! `docs/design/ENHANCEMENT_WAVE_16.md` §8's frame-budget table can be
//! read off one file.
//!
//! This binary needs a real (or software-fallback) GPU adapter — unlike
//! `cargo test --workspace`, which must never require one (CLAUDE.md
//! "Build" section). It is not part of any test suite; it is a
//! developer/benchmark tool, the same class as `gen-pal`.
//!
//! Usage: `cargo run --release -p rf-renderer --bin bench-passes`

mod minijson;

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rf_renderer::fog::{FogParams, FogPass};
use rf_renderer::gpu::GpuContext;
use rf_renderer::shader_chain::{ChainStage, ShaderChain};

use minijson::{row_num, row_str, RowFields};

const FRAMES: usize = 300;
const SIZES: [(u32, u32); 2] = [(256, 240), (512, 448)];

fn main() {
    let gpu = match GpuContext::request_headless() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("bench-passes: no GPU adapter available ({e}) — nothing to measure");
            std::process::exit(1);
        }
    };
    println!(
        "bench-passes: adapter {} ({:?}, backend {:?})",
        gpu.adapter_info.name, gpu.adapter_info.device_type, gpu.adapter_info.backend
    );

    let chain = ShaderChain::new(&gpu);
    let mut rows: Vec<RowFields> = Vec::new();

    for &(w, h) in &SIZES {
        let src = synthetic_frame(w, h);
        let size_label = format!("{w}x{h}");

        for (name, stage) in [
            ("nearest", ChainStage::nearest()),
            ("xbr", ChainStage::xbr(0.15, 1.0)),
            ("crt", ChainStage::crt(0.5, 0.3, 2.2)),
        ] {
            let samples = time_shader_pass(&gpu, &chain, &stage, &src, w, h);
            rows.push(pass_row(name, &size_label, &samples, &gpu));
            report(name, &size_label, &samples);
        }

        let samples = time_neural_stub(&gpu, &src, w, h);
        rows.push(pass_row("neural-stub", &size_label, &samples, &gpu));
        report("neural-stub", &size_label, &samples);

        let samples = time_fog_pass(&gpu, &src, w, h);
        rows.push(pass_row("fog", &size_label, &samples, &gpu));
        report("fog", &size_label, &samples);
    }

    let evidence_path = evidence_path();
    let existing = minijson::read_existing_rows(&evidence_path);
    let merged = minijson::merge_rows(existing, rows);
    minijson::write_doc(&evidence_path, &machine_label(&gpu), &now_rfc3339(), merged);
    println!("bench-passes: wrote {}", evidence_path.display());
}

fn pass_row(name: &str, size: &str, samples: &[Duration], gpu: &GpuContext) -> RowFields {
    let (p50, p95) = percentiles(samples);
    let mut row = RowFields::new();
    row_str(&mut row, "pass", name);
    row_str(&mut row, "size", size);
    row_num(&mut row, "n", samples.len() as f64);
    row_num(&mut row, "p50_ms", ms(p50));
    row_num(&mut row, "p95_ms", ms(p95));
    row_str(&mut row, "adapter", &gpu.adapter_info.name);
    row_str(
        &mut row,
        "backend",
        &format!("{:?}", gpu.adapter_info.backend),
    );
    row_str(&mut row, "source", "gpu-bench");
    row
}

fn report(name: &str, size: &str, samples: &[Duration]) {
    let (p50, p95) = percentiles(samples);
    println!(
        "  {name:<12} {size:<9} p50={:>7.3}ms  p95={:>7.3}ms",
        ms(p50),
        ms(p95)
    );
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// p50/p95 over a sorted copy of `samples` — nearest-rank method (simple,
/// matches what most local benchmark harnesses in this repo already do:
/// no interpolation, an actual observed sample, never a guessed value).
fn percentiles(samples: &[Duration]) -> (Duration, Duration) {
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort();
    let idx = |q: f64| -> usize {
        let n = sorted.len();
        ((q * n as f64).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1)
    };
    (sorted[idx(0.50)], sorted[idx(0.95)])
}

fn synthetic_frame(w: u32, h: u32) -> Vec<u8> {
    // Deterministic pseudo-pattern, not a real frame -- this harness times
    // GPU passes, not any particular console's output. A flat color would
    // let a filter's early-out paths (nothing changed) skew timings.
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            buf[i] = ((x * 7 + y * 3) % 256) as u8;
            buf[i + 1] = ((x * 13 + y * 5) % 256) as u8;
            buf[i + 2] = ((x * 3 + y * 17) % 256) as u8;
            buf[i + 3] = 255;
        }
    }
    buf
}

fn time_shader_pass(
    gpu: &GpuContext,
    chain: &ShaderChain,
    stage: &ChainStage,
    src: &[u8],
    w: u32,
    h: u32,
) -> Vec<Duration> {
    let stages = [*stage];
    // One untimed warm-up: pipeline caches / first-submission driver cost
    // must not pollute the measured distribution.
    let _ = chain.render(gpu, src, w, h, &stages);
    let mut samples = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let t0 = Instant::now();
        chain
            .render(gpu, src, w, h, &stages)
            .expect("bench-passes: shader chain render failed");
        samples.push(t0.elapsed());
    }
    samples
}

/// Times [`FogPass::render`] (ticket W16-04) end-to-end: upload scene +
/// density textures, render, blocking readback -- same wall-clock-per-call
/// contract as [`time_shader_pass`], so this row is comparable to
/// `nearest`/`xbr`/`crt` in the same table (`docs/design/
/// ENHANCEMENT_WAVE_16.md` §8's frame-budget gate applies identically).
/// The density texture reuses `src` (the same synthetic pattern) since
/// this harness only cares about wall time, not a realistic density
/// shape -- the golden test (`tests/fog_golden.rs`) is where the actual
/// visual fixture lives.
fn time_fog_pass(gpu: &GpuContext, src: &[u8], w: u32, h: u32) -> Vec<Duration> {
    let fog = FogPass::new(gpu);
    let params = FogParams::new(1.0, 0.05, 0.02, 0.8);
    // One untimed warm-up, same reasoning as `time_shader_pass`.
    let _ = fog
        .render(gpu, src, src, w, h, params)
        .expect("bench-passes: fog pass render failed");
    let mut samples = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let t0 = Instant::now();
        fog.render(gpu, src, src, w, h, params)
            .expect("bench-passes: fog pass render failed");
        samples.push(t0.elapsed());
    }
    samples
}

/// Times [`neural_stub.wgsl`]'s fixed-workload compute pass end-to-end
/// (upload -> dispatch -> blocking readback), same "wall time per frame"
/// contract as [`time_shader_pass`] so the two are comparable numbers.
fn time_neural_stub(gpu: &GpuContext, src: &[u8], w: u32, h: u32) -> Vec<Duration> {
    let scale: u32 = 4;
    let out_w = w * scale;
    let out_h = h * scale;

    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bench-passes::neural_stub"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "neural_stub.wgsl"
            ))),
        });

    let bgl = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bench-passes::neural_stub::bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bench-passes::neural_stub::layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("bench-passes::neural_stub::pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

    // src is u32-packed RGBA (one texel per u32, native-endian, matching
    // the shader's unpack_rgba); Rgba8 bytes read 4-at-a-time already have
    // this layout on a little-endian host, which is all this project ever
    // runs on (Apple Silicon, x86_64 CI).
    let src_u32: Vec<u32> = src
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    use wgpu::util::DeviceExt;
    let src_bytes: Vec<u8> = src_u32.iter().copied().flat_map(u32::to_le_bytes).collect();
    let src_buf = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bench-passes::neural_stub::src"),
            contents: &src_bytes,
            usage: wgpu::BufferUsages::STORAGE,
        });

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Params {
        width: u32,
        height: u32,
        scale: u32,
        _pad: u32,
    }
    let params = Params {
        width: w,
        height: h,
        scale,
        _pad: 0,
    };
    let params_bytes: Vec<u8> = [params.width, params.height, params.scale, params._pad]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let params_buf = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bench-passes::neural_stub::params"),
            contents: &params_bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });

    let out_len = (out_w as usize) * (out_h as usize);
    let dst_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bench-passes::neural_stub::dst"),
        size: (out_len * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bench-passes::neural_stub::readback"),
        size: (out_len * 4) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bench-passes::neural_stub::bind_group"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: src_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: dst_buf.as_entire_binding(),
            },
        ],
    });

    let wg_x = out_w.div_ceil(8);
    let wg_y = out_h.div_ceil(8);

    let run_once = || {
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("bench-passes::neural_stub::encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("bench-passes::neural_stub::pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);
        }
        encoder.copy_buffer_to_buffer(&dst_buf, 0, &readback_buf, 0, (out_len * 4) as u64);
        gpu.queue.submit(Some(encoder.finish()));
        read_buffer_sync(&gpu.device, &readback_buf)
            .expect("bench-passes: neural stub readback failed");
    };

    run_once(); // warm-up, same reasoning as time_shader_pass
    let mut samples = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        let t0 = Instant::now();
        run_once();
        samples.push(t0.elapsed());
    }
    samples
}

fn evidence_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/evidence/gpu-passes.json")
}

fn machine_label(gpu: &GpuContext) -> String {
    let hostname = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown-host".to_string());
    format!("{hostname} / {}", gpu.adapter_info.name)
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    humantime_utc(secs)
}

/// Tiny UTC "YYYY-MM-DDTHH:MM:SSZ" formatter -- avoids pulling in
/// `chrono`/`time` for one timestamp field. Civil-from-days algorithm per
/// Howard Hinnant's public-domain `chrono-compatible` date algorithms
/// (widely reproduced; no external licence to track since this is a
/// from-scratch reimplementation of well-known integer arithmetic, not a
/// derived/ported file).
fn humantime_utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86400) as i64;
    let secs_of_day = unix_secs % 86400;
    let (h, m, s) = (
        secs_of_day / 3600,
        (secs_of_day / 60) % 60,
        secs_of_day % 60,
    );

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m_num = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m_num <= 2 { y + 1 } else { y };

    format!("{y:04}-{m_num:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Blocking buffer readback, same bounded-wait shape as
/// `rf_renderer::gpu::read_buffer_sync` (which is `pub(crate)` and so not
/// reachable from this bin target — a separate compilation unit even
/// though it shares the package). Duplicated rather than widened to
/// `pub`: that function's visibility is deliberate crate-internal API
/// surface, and this bin has no write_scope reason to change it.
fn read_buffer_sync(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<Vec<u8>, String> {
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .map_err(|e| format!("device.poll timed out waiting for buffer readback: {e}"))?;
    match rx.try_recv() {
        Ok(Ok(())) => {
            let data = slice.get_mapped_range().to_vec();
            buffer.unmap();
            Ok(data)
        }
        Ok(Err(e)) => Err(format!("buffer map_async failed: {e}")),
        Err(_) => Err(
            "buffer map_async callback never fired even though device.poll returned".to_string(),
        ),
    }
}
