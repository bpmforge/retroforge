//! Temporal upscaling for the 3D (HD-2D / Diorama) renderer (ticket W39-05;
//! `docs/design/REBUILD_LAB.md` section 9): render at a lower internal
//! resolution with a sub-pixel camera jitter, per-pixel depth and per-pixel
//! motion vectors ([`crate::diorama_gbuffer`]), then accumulate the frames
//! into a 5K image.
//!
//! Two interchangeable back ends sit behind [`TemporalUpscaler`]:
//!
//! * **MetalFX** (`MTLFXTemporalScaler`, macOS, `--features metalfx`): driven
//!   through the wgpu Metal hal interop in [`crate::metalfx`]
//!   (`MetalFxTemporalScaler`), sharing wgpu's own `MTLDevice` and
//!   `MTLCommandQueue`.
//! * **Portable** (every platform wgpu runs on, and the fallback when MetalFX
//!   is not built, not on this OS, or refuses the device): a WGSL compute
//!   resolve, `shaders/temporal_upscale.wgsl` -- Gaussian-weighted 3x3 sample
//!   of the jittered frame, history reprojected with the motion vector of the
//!   nearest surface, clamped to the 3x3 colour box, blended with a 0.1 weight
//!   for the new frame (raised with speed). History is `Rgba16Float` at
//!   output resolution (an 8-bit history stalls short of convergence).
//!   Deterministic: no atomics, no data-dependent addressing beyond reads, so
//!   the same inputs give the same bytes on one device.
//!
//! ## Jitter convention
//!
//! [`jitter_for_frame`] is a Halton(2,3) sequence over [`JITTER_PHASES`]
//! frames, a pure function of the frame index, in input pixels, y down,
//! within `(-0.5, 0.5)`. A surface point whose true position is `P` is drawn
//! at `P + jitter`; [`jitter_vp`] builds that by translating clip space after
//! the projection. The motion vectors are built from the *unjittered*
//! matrices, so the jitter is never in the motion.
//!
//! ## Honest limits
//!
//! * The colour is the Diorama pass's output (4x MSAA, lit), the depth and
//!   motion come from a separate single-sample draw of the same mesh. Edge
//!   pixels can disagree by an MSAA sample; the colour box clamp absorbs it.
//! * Contact-shadow decals have no depth or motion of their own (they
//!   inherit the ground's), and a moving object's motion needs last frame's
//!   mesh to pair up (see `diorama_gbuffer`).
//! * The depth-of-field lens runs at the internal resolution, before the
//!   upscale, through its CPU path: a frame with DoF on is read back at
//!   internal size. Not a per-frame real-time claim; measured numbers are in
//!   the W39-05 tests' output.
//!
//! Every loop is a range or an iterator (law 8).

use std::borrow::Cow;

use crate::diorama::camera_matrices::{self, Mat4};
use crate::gpu::{align_up, GpuContext};

const SHADER_SRC: &str = include_str!("shaders/temporal_upscale.wgsl");

/// Length of the jitter cycle, frames.
pub const JITTER_PHASES: u32 = 16;
/// Largest internal-to-output ratio per axis the back ends are asked for
/// (MetalFX documents a supported range per device; 3x is the top of it).
pub const MAX_RATIO: f32 = 3.0;
/// The 5K output the HD-2D path targets.
pub const OUTPUT_5K: (u32, u32) = (5120, 2880);

/// The `index`-th element (1-based) of the Halton sequence in `base`.
#[must_use]
pub fn halton(index: u32, base: u32) -> f32 {
    let base = base.max(2);
    let (mut f, mut r, mut i) = (1.0f32, 0.0f32, index);
    // `i` is divided by `base >= 2` every pass, so the walk ends.
    while i > 0 {
        f /= base as f32;
        r += f * (i % base) as f32;
        i /= base;
    }
    r
}

/// The sub-pixel jitter of frame `frame`, input pixels, y down, each axis in
/// `(-0.5, 0.5)`: Halton(2, 3), a cycle of [`JITTER_PHASES`] frames.
#[must_use]
pub fn jitter_for_frame(frame: u32) -> (f32, f32) {
    let k = frame % JITTER_PHASES + 1;
    (halton(k, 2) - 0.5, halton(k, 3) - 0.5)
}

/// `vp` with the picture shifted by `jitter` input pixels (y down) on a
/// `size` internal frame: a clip-space translation applied after the
/// projection, so the shift is the same everywhere in the frame.
#[must_use]
pub fn jitter_vp(vp: Mat4, jitter: (f32, f32), size: (u32, u32)) -> Mat4 {
    let tx = 2.0 * jitter.0 / size.0.max(1) as f32;
    // NDC is y-up; pixels are y-down.
    let ty = -2.0 * jitter.1 / size.1.max(1) as f32;
    let mut t = [0.0f32; 16];
    t[0] = 1.0;
    t[5] = 1.0;
    t[10] = 1.0;
    t[15] = 1.0;
    // Column 3 (clip.w feeds x and y): x' = x + tx*w, y' = y + ty*w.
    t[12] = tx;
    t[13] = ty;
    camera_matrices::mul(t, vp)
}

/// Which back end is doing the upscale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalBackend {
    /// `MTLFXTemporalScaler`.
    MetalFx,
    /// The WGSL resolve in this module.
    Portable,
}

/// One frame's inputs, all at the internal resolution.
pub struct TemporalInputs<'a> {
    /// `Rgba8Unorm`, usage `TEXTURE_BINDING`; alpha is ignored.
    pub color: &'a wgpu::Texture,
    /// `Depth32Float`, usage `TEXTURE_BINDING`; 0 near, 1 far.
    pub depth: &'a wgpu::Texture,
    /// `Rg16Float`, usage `TEXTURE_BINDING`; see `diorama_gbuffer`.
    pub motion: &'a wgpu::Texture,
    /// What [`jitter_vp`] was given for this frame.
    pub jitter: (f32, f32),
    /// Discard history (first frame, camera cut, resize).
    pub reset: bool,
}

/// The upscaler: input size in, output size out, history inside.
pub struct TemporalUpscaler {
    inner: Inner,
    /// Why MetalFX was not used when it was asked for.
    pub fallback_reason: Option<String>,
}

enum Inner {
    Portable(Box<Portable>),
    #[cfg(all(feature = "metalfx", target_os = "macos"))]
    MetalFx(Box<crate::metalfx::MetalFxTemporalScaler>),
}

impl TemporalUpscaler {
    /// The portable back end, always available.
    #[must_use]
    pub fn portable(gpu: &GpuContext, input: (u32, u32), output: (u32, u32)) -> Self {
        Self {
            inner: Inner::Portable(Box::new(Portable::new(gpu, input, output))),
            fallback_reason: None,
        }
    }

    /// MetalFX when this build, this OS and this device allow it, else the
    /// portable back end with [`Self::fallback_reason`] saying why.
    #[must_use]
    pub fn new(
        gpu: &GpuContext,
        input: (u32, u32),
        output: (u32, u32),
        prefer_metalfx: bool,
    ) -> Self {
        if !prefer_metalfx {
            return Self::portable(gpu, input, output);
        }
        #[cfg(all(feature = "metalfx", target_os = "macos"))]
        match crate::metalfx::MetalFxTemporalScaler::new(gpu, input, output) {
            Ok(s) => Self {
                inner: Inner::MetalFx(Box::new(s)),
                fallback_reason: None,
            },
            Err(e) => {
                let mut p = Self::portable(gpu, input, output);
                p.fallback_reason = Some(e.to_string());
                p
            }
        }
        #[cfg(not(all(feature = "metalfx", target_os = "macos")))]
        {
            let mut p = Self::portable(gpu, input, output);
            p.fallback_reason = Some(
                crate::metalfx::detect()
                    .reason()
                    .unwrap_or("MetalFX temporal scaling is not built into this copy")
                    .to_string(),
            );
            p
        }
    }

    /// Which back end runs.
    #[must_use]
    pub fn backend(&self) -> TemporalBackend {
        match &self.inner {
            Inner::Portable(_) => TemporalBackend::Portable,
            #[cfg(all(feature = "metalfx", target_os = "macos"))]
            Inner::MetalFx(_) => TemporalBackend::MetalFx,
        }
    }

    /// Internal (input) size.
    #[must_use]
    pub fn input_size(&self) -> (u32, u32) {
        match &self.inner {
            Inner::Portable(p) => p.input,
            #[cfg(all(feature = "metalfx", target_os = "macos"))]
            Inner::MetalFx(m) => m.input_size(),
        }
    }

    /// Output size.
    #[must_use]
    pub fn output_size(&self) -> (u32, u32) {
        match &self.inner {
            Inner::Portable(p) => p.output,
            #[cfg(all(feature = "metalfx", target_os = "macos"))]
            Inner::MetalFx(m) => m.output_size(),
        }
    }

    /// Accumulate one frame into the history and produce the upscaled image.
    ///
    /// # Errors
    /// A back end failure (MetalFX reports nil objects as errors).
    pub fn upscale(&mut self, gpu: &GpuContext, inputs: &TemporalInputs<'_>) -> Result<(), String> {
        match &mut self.inner {
            Inner::Portable(p) => {
                p.upscale(gpu, inputs);
                Ok(())
            }
            #[cfg(all(feature = "metalfx", target_os = "macos"))]
            Inner::MetalFx(m) => m
                .scale(
                    inputs.color,
                    inputs.depth,
                    inputs.motion,
                    inputs.jitter,
                    inputs.reset,
                )
                .map_err(|e| e.to_string()),
        }
    }

    /// Override the sign the jitter is handed to MetalFX with (a no-op for
    /// the portable back end, whose convention is this module's own). For
    /// the sign-convention test; production uses `metalfx::apple::JITTER_SIGN`.
    pub fn set_metalfx_jitter_sign(&mut self, sign: (f32, f32)) {
        #[cfg(all(feature = "metalfx", target_os = "macos"))]
        if let Inner::MetalFx(m) = &mut self.inner {
            m.set_jitter_sign(sign);
        }
        let _ = sign;
    }

    /// The most recent output (`Rgba8Unorm`, `COPY_SRC | TEXTURE_BINDING`).
    #[must_use]
    pub fn output(&self) -> &wgpu::Texture {
        match &self.inner {
            Inner::Portable(p) => &p.out,
            #[cfg(all(feature = "metalfx", target_os = "macos"))]
            Inner::MetalFx(m) => m.current_output_texture(),
        }
    }

    /// Read the most recent output back as RGBA8 (alpha 255), waiting for the
    /// GPU. A 5K frame is 59 MB: call it for the frames you inspect.
    ///
    /// # Errors
    /// A readback that does not complete within [`crate::gpu::GPU_WAIT`].
    pub fn read_output(&self, gpu: &GpuContext) -> Result<Vec<u8>, String> {
        #[cfg(all(feature = "metalfx", target_os = "macos"))]
        if let Inner::MetalFx(m) = &self.inner {
            m.wait_idle();
        }
        read_rgba8(gpu, self.output(), self.output_size())
    }
}

struct Portable {
    input: (u32, u32),
    output: (u32, u32),
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    history: [wgpu::Texture; 2],
    history_views: [wgpu::TextureView; 2],
    out: wgpu::Texture,
    out_view: wgpu::TextureView,
    /// Index of the history the next frame reads.
    read: usize,
}

/// Weight of the new frame once history is valid.
const BLEND: f32 = 0.1;

impl Portable {
    fn new(gpu: &GpuContext, input: (u32, u32), output: (u32, u32)) -> Self {
        let tex = |binding, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let storage = |binding, format| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let float = wgpu::TextureSampleType::Float { filterable: true };
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("rf-renderer::temporal::layout"),
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
                    tex(1, float),
                    tex(2, wgpu::TextureSampleType::Depth),
                    tex(3, float),
                    tex(4, float),
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    storage(6, wgpu::TextureFormat::Rgba8Unorm),
                    storage(7, wgpu::TextureFormat::Rgba16Float),
                ],
            });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("rf-renderer::temporal::pipeline_layout"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("rf-renderer::temporal::shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SRC)),
            });
        let pipeline = gpu
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("rf-renderer::temporal::pipeline"),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rf-renderer::temporal::sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::temporal::uniform"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let make = |label, format, usage| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: output.0,
                    height: output.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let hist_usage =
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING;
        let history = [
            make(
                "rf-renderer::temporal::history0",
                wgpu::TextureFormat::Rgba16Float,
                hist_usage,
            ),
            make(
                "rf-renderer::temporal::history1",
                wgpu::TextureFormat::Rgba16Float,
                hist_usage,
            ),
        ];
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let history_views = [view(&history[0]), view(&history[1])];
        let out = make(
            "rf-renderer::temporal::output",
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
        let out_view = view(&out);
        Self {
            input,
            output,
            pipeline,
            layout,
            sampler,
            uniform,
            history,
            history_views,
            out,
            out_view,
            read: 0,
        }
    }

    fn upscale(&mut self, gpu: &GpuContext, inputs: &TemporalInputs<'_>) {
        let mut ubo = Vec::with_capacity(32);
        for f in [
            self.input.0 as f32,
            self.input.1 as f32,
            self.output.0 as f32,
            self.output.1 as f32,
            inputs.jitter.0,
            inputs.jitter.1,
            BLEND,
            if inputs.reset { 1.0 } else { 0.0 },
        ] {
            ubo.extend_from_slice(&f.to_le_bytes());
        }
        gpu.queue.write_buffer(&self.uniform, 0, &ubo);
        let view = |t: &wgpu::Texture, aspect| {
            t.create_view(&wgpu::TextureViewDescriptor {
                aspect,
                ..Default::default()
            })
        };
        let color = view(inputs.color, wgpu::TextureAspect::All);
        let depth = view(inputs.depth, wgpu::TextureAspect::DepthOnly);
        let motion = view(inputs.motion, wgpu::TextureAspect::All);
        let (r, w) = (self.read, 1 - self.read);
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rf-renderer::temporal::bind_group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&color),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&depth),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&motion),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&self.history_views[r]),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&self.out_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&self.history_views[w]),
                },
            ],
        });
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rf-renderer::temporal::encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("rf-renderer::temporal::pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(self.output.0.div_ceil(8), self.output.1.div_ceil(8), 1);
        }
        gpu.queue.submit(std::iter::once(encoder.finish()));
        self.read = w;
        // Keep the written history alive for the next read.
        let _ = &self.history;
    }
}

/// Read an `Rgba8Unorm` texture back, row padding stripped.
///
/// # Errors
/// A readback that does not complete within [`crate::gpu::GPU_WAIT`].
pub fn read_rgba8(
    gpu: &GpuContext,
    texture: &wgpu::Texture,
    size: (u32, u32),
) -> Result<Vec<u8>, String> {
    let (w, h) = size;
    let padded = align_up(w * 4, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rf-renderer::temporal::readback"),
        size: u64::from(padded) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(std::iter::once(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    crate::gpu::wait_for_map(&gpu.device, &rx)?;
    let mapped = buffer.slice(..).get_mapped_range();
    let row = w as usize * 4;
    let mut out = Vec::with_capacity(row * h as usize);
    for r in mapped.chunks(padded as usize).take(h as usize) {
        out.extend_from_slice(&r[..row]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halton_matches_the_textbook_values() {
        let base2: Vec<f32> = (1..=4).map(|i| halton(i, 2)).collect();
        assert_eq!(base2, vec![0.5, 0.25, 0.75, 0.125]);
        assert!((halton(1, 3) - 1.0 / 3.0).abs() < 1e-6);
        assert!((halton(2, 3) - 2.0 / 3.0).abs() < 1e-6);
        assert!((halton(3, 3) - 1.0 / 9.0).abs() < 1e-6);
        // Degenerate base is clamped, index 0 is the origin.
        assert_eq!(halton(0, 2), 0.0);
        assert!(halton(5, 0) > 0.0);
    }

    #[test]
    fn jitter_stays_inside_a_pixel_and_cycles() {
        for f in 0..(JITTER_PHASES * 3) {
            let (x, y) = jitter_for_frame(f);
            assert!(x > -0.5 && x < 0.5 && y > -0.5 && y < 0.5, "{f}: {x},{y}");
            assert_eq!((x, y), jitter_for_frame(f % JITTER_PHASES));
        }
        // All phases differ (a repeat would waste a sample).
        let all: Vec<(f32, f32)> = (0..JITTER_PHASES).map(jitter_for_frame).collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
        // Zero-mean enough that the average picture is not shifted.
        let n = JITTER_PHASES as f32;
        let (mx, my) = all
            .iter()
            .fold((0.0, 0.0), |(a, b), (x, y)| (a + x / n, b + y / n));
        assert!(mx.abs() < 0.05 && my.abs() < 0.05, "{mx},{my}");
    }

    fn project(vp: &Mat4, p: [f32; 3]) -> (f32, f32) {
        let c =
            |row: usize| vp[row] * p[0] + vp[4 + row] * p[1] + vp[8 + row] * p[2] + vp[12 + row];
        (c(0) / c(3), c(1) / c(3))
    }

    #[test]
    fn jitter_moves_the_picture_by_exactly_the_requested_pixels() {
        let size = (1707u32, 960u32);
        let vp = camera_matrices::mul(
            camera_matrices::perspective(0.8, 16.0 / 9.0, 0.1, 100.0),
            camera_matrices::look_at([0.0, 3.0, 8.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        );
        let j = (0.3f32, -0.2f32);
        let jittered = jitter_vp(vp, j, size);
        for p in [[0.0, 0.0, 0.0], [2.0, 1.0, -3.0], [-4.0, 0.5, 2.0]] {
            let (x0, y0) = project(&vp, p);
            let (x1, y1) = project(&jittered, p);
            // NDC to pixels: x right, y down.
            let dx = (x1 - x0) * 0.5 * size.0 as f32;
            let dy = -(y1 - y0) * 0.5 * size.1 as f32;
            assert!((dx - j.0).abs() < 1e-3, "dx {dx}");
            assert!((dy - j.1).abs() < 1e-3, "dy {dy}");
        }
        // Zero jitter is the identity.
        assert_eq!(jitter_vp(vp, (0.0, 0.0), size), vp);
    }
}
