//! WGSL shader chain (ticket W3-02; FR-REND-003; `docs/design/RENDERER.md`
//! §4): the reusable pass-chain infrastructure every filter/CRT/upscaler
//! shader plugs into, plus the three **arithmetically simple** first-party
//! shaders — `nearest`, `sharp-bilinear`, `scanlines`. The three harder
//! ones (`crt-easymode`-class, `lcd-grid`, `xbr`-class) are ticket W3-02a
//! (design review G-42's clean-room licensing line — see below).
//!
//! ## Common pass interface (RENDERER.md §4)
//!
//! "WGSL passes with a common interface: `tex_in, sampler, params: UBO ->
//! tex_out`." [`ShaderChain::new`] builds exactly **one** shared
//! `wgpu::BindGroupLayout`/`wgpu::PipelineLayout` (three bindings: a
//! 32-byte `Params` UBO at binding 0, the input texture at binding 1, a
//! sampler at binding 2) and reuses it for every shader kind, present and
//! future — a `Filtering`-typed sampler binding only declares that a
//! *filtering-capable* sampler may be bound there (the same fact
//! `crate::composite`'s own sampler already relies on, `wgpu::SamplerDescriptor
//! ::default()` there being a *nearest*-filter sampler under that same
//! layout type); the actual nearest-vs-linear behavior comes from the
//! `wgpu::Sampler` object each pass binds, not the layout. W3-02a's harder
//! shaders bind the identical layout, so this ticket is where getting it
//! right actually matters.
//!
//! ## Chain description is data (RENDERER.md §4's own examples)
//!
//! [`ChainStage`]/[`ShaderKind`] are plain data — a `Vec<ChainStage>` is
//! runtime-selectable per-game/user settings, matching §4's `[crt-easymode,
//! vignette]` / `[xbrz4]` / `[]` examples exactly. [`ShaderChain::render`]
//! treats an **empty** chain as passthrough (§4's own explicit case): it
//! never touches the GPU for that case at all and returns the input bytes
//! unchanged, byte-for-byte, by construction — not merely "no panic".
//!
//! ## Licensing law applies even to these three (design review G-42)
//!
//! `nearest` and `scanlines` are arithmetically trivial ("darken every Nth
//! row", "one texel fetch") — nobody owns that math — but the WGSL text in
//! `shaders/chain_nearest.wgsl` and `shaders/chain_scanlines.wgsl` is
//! **authored fresh for this ticket, not transcribed** from any existing
//! shader, and each says so in its own header comment and in
//! [`ShaderKind::manifest`]'s `basis`/`authorship` fields (`basis: None`
//! for all three shaders here — no specific upstream to name; W3-02a's
//! `xbr`-class shader is the first to give that field real content).
//! `sharp-bilinear` is the
//! one RENDERER.md §4 itself calls out as public-domain-portable, but this
//! ticket's brief asks for authored WGSL with recorded provenance
//! regardless of licence — see `shaders/chain_sharp_bilinear.wgsl`'s header
//! for the independent derivation and why it is not a port of any specific
//! existing preset. `cargo deny check licenses` only inspects Cargo
//! dependencies and will never look inside a `.wgsl` file — this provenance
//! discipline is **review-enforced**, the same class of thing as
//! `rf-cache`'s documented `SystemTime` exemption, not a mechanically
//! gated one.
//!
//! ## Tolerance calibration (acceptance criterion 5)
//!
//! W3-01b's `rf_harness::tolerance` mechanism shipped at `channel_delta: 0`
//! for `nearest`-neighbor integer scaling because that pass, measured, was
//! byte-identical to its CPU oracle on Metal — no observed noise to size an
//! allowance against. `sharp-bilinear` is different on purpose: it samples
//! through the pipeline's real linear-filtering hardware sampler
//! (`textureSample`, not a hand-rolled `textureLoad` lerp), which is
//! exactly what RENDERER.md §7's "not hashable past 1x" warning is about.
//! [`render_sharp_bilinear_reference`] is the CPU-side oracle — its own
//! hand-rolled bilinear fetch, not a codified GPU output, same
//! "independent oracle" shape as [`crate::scale::render_scaled_reference`]
//! — and the residual gap between it and the real GPU pass is measured (not
//! assumed) in this crate's own tests (`rf-harness` is out of this ticket's
//! `write_scope`, and depending on it here would be circular — `rf-harness`
//! already dev-depends on `rf-renderer` for its GPU golden-frame tests — so
//! the tolerance comparator is reimplemented in-crate, test-only, mirroring
//! `rf_harness::tolerance::compare_with_tolerance`'s exact metric).

use std::borrow::Cow;

use crate::gpu::{align_up, read_buffer_sync, GpuContext};

const NEAREST_SHADER_SRC: &str = include_str!("shaders/chain_nearest.wgsl");
const SHARP_BILINEAR_SHADER_SRC: &str = include_str!("shaders/chain_sharp_bilinear.wgsl");
const SCANLINES_SHADER_SRC: &str = include_str!("shaders/chain_scanlines.wgsl");

/// Which first-party shader a [`ChainStage`] selects. `Copy`/`PartialEq` so
/// chain descriptions (`Vec<ChainStage>`) are cheap, comparable, ordinary
/// data — RENDERER.md §4's "chain description is data" — not a
/// GPU-resource handle in disguise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderKind {
    /// Identity/resize resample, nearest-neighbor. `shaders/chain_nearest.wgsl`.
    Nearest,
    /// Bilinear resample with a tunable sharpness knob.
    /// `shaders/chain_sharp_bilinear.wgsl`.
    SharpBilinear,
    /// Per-row darkening overlay. `shaders/chain_scanlines.wgsl`.
    Scanlines,
}

/// One tunable field a shader's manifest exposes, "name/range annotations"
/// for a UI-generated control (RENDERER.md §4). Pure data — no dependency
/// on any UI toolkit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShaderParamDescriptor {
    pub name: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

/// Per-shader provenance record (this ticket's acceptance criterion 4): the
/// licence and a statement of how the WGSL was authored, plus the UI-facing
/// parameter list RENDERER.md §4 also asks the manifest to carry. This is
/// the format ticket W3-02a's three harder shaders (`crt-easymode`-class,
/// `lcd-grid`, `xbr`-class) must also produce.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShaderManifest {
    /// Stable identifier (matches `ShaderKind`'s WGSL filename stem).
    pub id: &'static str,
    pub display_name: &'static str,
    /// SPDX-style licence expression for *this shader's WGSL text*
    /// specifically — not necessarily the whole crate's licence, though for
    /// all three shaders in this ticket the two happen to coincide (module
    /// doc: all three are original RetroForge work).
    pub license: &'static str,
    /// The upstream technique/shader this implementation is based on, if
    /// any — a **structured** field, not buried in `authorship`'s prose,
    /// because this is exactly what design review G-42 makes highest-
    /// stakes: W3-02a's `xbr`-class shader must be able to say "based on
    /// Hyllian's xBR (MIT), explicitly not xBRZ (GPL-3.0) or a libretro
    /// port" in a field a reviewer can read on its own. `None` for all
    /// three shaders in this ticket — each is independently derived, not
    /// based on any specific named upstream (see `authorship` for what
    /// "independently derived" means for each one).
    pub basis: Option<&'static str>,
    /// How the WGSL was authored — the statement design review G-42 and
    /// this ticket's brief both require, regardless of how trivial or
    /// unownable the underlying arithmetic is.
    pub authorship: &'static str,
    pub params: &'static [ShaderParamDescriptor],
}

impl ShaderKind {
    /// The WGSL source for this shader — `include_str!`'d at compile time,
    /// same "no runtime file I/O" shape as every other shader in this
    /// crate.
    fn source(self) -> &'static str {
        match self {
            ShaderKind::Nearest => NEAREST_SHADER_SRC,
            ShaderKind::SharpBilinear => SHARP_BILINEAR_SHADER_SRC,
            ShaderKind::Scanlines => SCANLINES_SHADER_SRC,
        }
    }

    /// This shader's provenance manifest (acceptance criterion 4) — a
    /// `'static` constant, not derived at runtime, so the record can never
    /// drift from what actually shipped.
    #[must_use]
    pub fn manifest(self) -> &'static ShaderManifest {
        match self {
            ShaderKind::Nearest => &ShaderManifest {
                id: "nearest",
                display_name: "Nearest",
                license: "MIT OR Apache-2.0",
                basis: None,
                authorship: "Authored fresh for RetroForge ticket W3-02 (`shaders/chain_nearest\
                    .wgsl`): one `textureSample` fetch through a nearest-filtering sampler, no \
                    blending. Nearest-neighbor resampling is arithmetically trivial and owned \
                    by nobody, but the WGSL text itself was written for this crate, not copied \
                    or ported from any other shader.",
                params: &[],
            },
            ShaderKind::SharpBilinear => &ShaderManifest {
                id: "sharp-bilinear",
                display_name: "Sharp Bilinear",
                license: "MIT OR Apache-2.0",
                basis: None,
                authorship: "Authored fresh for RetroForge ticket W3-02 (`shaders/chain_sharp_\
                    bilinear.wgsl`) from first principles: blends the sampled UV toward its \
                    nearest texel-center UV by a `sharpness` parameter, then takes one real \
                    hardware-filtered sample at the blended UV. RENDERER.md §4 records that \
                    real 'sharp-bilinear' shaders (e.g. RetroArch/libretro's) are public domain \
                    and may be ported directly, but this implementation is independently \
                    derived, not a port or transcription of RetroArch/libretro's \
                    `sharp-bilinear.glsl` or any other existing shader source -- nobody on this \
                    ticket had such a source open while writing this file.",
                params: &[ShaderParamDescriptor {
                    name: "sharpness",
                    label: "Sharpness",
                    min: 0.0,
                    max: 1.0,
                    default: 0.6,
                }],
            },
            ShaderKind::Scanlines => &ShaderManifest {
                id: "scanlines",
                display_name: "Scanlines",
                license: "MIT OR Apache-2.0",
                basis: None,
                authorship: "Authored fresh for RetroForge ticket W3-02 (`shaders/chain_\
                    scanlines.wgsl`): darkens every Nth output row by a flat multiplier. \
                    'Darken alternating rows' is trivial arithmetic nobody can own, but the \
                    WGSL text itself was written for this crate, not copied or ported from any \
                    other shader.",
                params: &[
                    ShaderParamDescriptor {
                        name: "intensity",
                        label: "Intensity",
                        min: 0.0,
                        max: 1.0,
                        default: 0.5,
                    },
                    ShaderParamDescriptor {
                        name: "period",
                        label: "Period (rows)",
                        min: 1.0,
                        max: 8.0,
                        default: 2.0,
                    },
                ],
            },
        }
    }
}

/// One stage of a shader chain: which shader, its tunable UBO fields
/// (`v0`, the "params" half of RENDERER.md §4's `tex_in, sampler, params:
/// UBO -> tex_out`), and an optional output size (`None` keeps the
/// previous stage's size — most stages in this ticket do; a future
/// upscaler like W3-02a's `xbr`-class shader would set this). Plain data,
/// cheap to build at runtime from user/game settings (module doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChainStage {
    pub kind: ShaderKind,
    v0: [f32; 4],
    out_size: Option<(u32, u32)>,
}

impl ChainStage {
    /// Identity/resize nearest-neighbor resample. No tunable parameters
    /// (see [`ShaderKind::manifest`]'s empty `params` list for `Nearest`).
    #[must_use]
    pub fn nearest() -> Self {
        ChainStage {
            kind: ShaderKind::Nearest,
            v0: [0.0; 4],
            out_size: None,
        }
    }

    /// Bilinear resample with `sharpness` (clamped to `[0, 1]` by the
    /// shader itself; out-of-range inputs degrade rather than panic, same
    /// stance as every other parameter in this crate).
    #[must_use]
    pub fn sharp_bilinear(sharpness: f32) -> Self {
        ChainStage {
            kind: ShaderKind::SharpBilinear,
            v0: [sharpness, 0.0, 0.0, 0.0],
            out_size: None,
        }
    }

    /// Per-row darkening: `intensity` in `[0, 1]`, `period` in output rows
    /// (a period of `2.0` darkens every other row — the classic look).
    #[must_use]
    pub fn scanlines(intensity: f32, period: f32) -> Self {
        ChainStage {
            kind: ShaderKind::Scanlines,
            v0: [intensity, period, 0.0, 0.0],
            out_size: None,
        }
    }

    /// Override this stage's output size (default: unchanged from
    /// whatever size fed into it). Builder-style so call sites read as
    /// `ChainStage::sharp_bilinear(0.6).with_out_size(512, 448)`.
    #[must_use]
    pub fn with_out_size(mut self, width: u32, height: u32) -> Self {
        self.out_size = Some((width, height));
        self
    }
}

/// Little-endian bytes for every chain shader's `Params { v0: vec4<f32>,
/// v1: vec4<f32> }` uniform (32 bytes) — `v0` is the stage's own tunable
/// fields, `v1.xy` is always `(1/out_width, 1/out_height)` (every shader in
/// this module needs it to turn a fragment's window-space position into a
/// UV; computed here once rather than duplicated per shader).
fn params_bytes(v0: [f32; 4], out_width: u32, out_height: u32) -> [u8; 32] {
    #[allow(clippy::cast_precision_loss)]
    let v1 = [
        1.0f32 / out_width as f32,
        1.0f32 / out_height as f32,
        0.0f32,
        0.0f32,
    ];
    let mut bytes = [0u8; 32];
    for (i, v) in v0.iter().chain(v1.iter()).enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// The WGSL shader chain: one shared bind-group/pipeline layout (module
/// doc), three pipelines (one per [`ShaderKind`]), two samplers (nearest —
/// shared by [`ShaderKind::Nearest`]/[`ShaderKind::Scanlines`] — and linear,
/// for [`ShaderKind::SharpBilinear`]). Built once in [`Self::new`] and
/// reused across [`Self::render`] calls, same "build once, reuse per
/// frame" shape as every other pass in this crate.
pub struct ShaderChain {
    bind_group_layout: wgpu::BindGroupLayout,
    nearest_pipeline: wgpu::RenderPipeline,
    sharp_bilinear_pipeline: wgpu::RenderPipeline,
    scanlines_pipeline: wgpu::RenderPipeline,
    nearest_sampler: wgpu::Sampler,
    linear_sampler: wgpu::Sampler,
}

impl ShaderChain {
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("rf-renderer::shader_chain::bind_group_layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("rf-renderer::shader_chain::pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });

        let make_pipeline = |label: &str, src: &str| -> wgpu::RenderPipeline {
            let shader = gpu
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(label),
                    source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(src)),
                });
            gpu.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
        };

        let nearest_pipeline = make_pipeline(
            "rf-renderer::shader_chain::nearest_pipeline",
            ShaderKind::Nearest.source(),
        );
        let sharp_bilinear_pipeline = make_pipeline(
            "rf-renderer::shader_chain::sharp_bilinear_pipeline",
            ShaderKind::SharpBilinear.source(),
        );
        let scanlines_pipeline = make_pipeline(
            "rf-renderer::shader_chain::scanlines_pipeline",
            ShaderKind::Scanlines.source(),
        );

        // Nearest/clamp-to-edge -- `wgpu::SamplerDescriptor::default()`,
        // same fact `crate::composite`'s own sampler already relies on
        // (see this module's own doc).
        let nearest_sampler = gpu
            .device
            .create_sampler(&wgpu::SamplerDescriptor::default());
        let linear_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rf-renderer::shader_chain::linear_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });

        ShaderChain {
            bind_group_layout,
            nearest_pipeline,
            sharp_bilinear_pipeline,
            scanlines_pipeline,
            nearest_sampler,
            linear_sampler,
        }
    }

    fn pipeline_and_sampler(&self, kind: ShaderKind) -> (&wgpu::RenderPipeline, &wgpu::Sampler) {
        match kind {
            ShaderKind::Nearest => (&self.nearest_pipeline, &self.nearest_sampler),
            ShaderKind::SharpBilinear => (&self.sharp_bilinear_pipeline, &self.linear_sampler),
            ShaderKind::Scanlines => (&self.scanlines_pipeline, &self.nearest_sampler),
        }
    }

    /// Run `source_rgba` (`source_width`x`source_height`, RGBA8) through
    /// `stages` in order, each stage's output feeding the next, and read
    /// back the final RGBA8 bytes.
    ///
    /// An **empty** `stages` slice is passthrough (RENDERER.md §4,
    /// acceptance criterion 3): returns `source_rgba` unchanged, byte for
    /// byte, without touching the GPU at all for the copy itself — never
    /// merely "doesn't panic".
    ///
    /// # Errors
    /// Returns `Err` if any stage's GPU readback does not complete within
    /// [`crate::gpu::GPU_WAIT`] — reported, never retried in a loop (see
    /// `crate::gpu`'s module doc).
    pub fn render(
        &self,
        gpu: &GpuContext,
        source_rgba: &[u8],
        source_width: u32,
        source_height: u32,
        stages: &[ChainStage],
    ) -> Result<Vec<u8>, String> {
        if stages.is_empty() {
            return Ok(source_rgba.to_vec());
        }

        let mut current = source_rgba.to_vec();
        let mut cur_size = (source_width, source_height);
        for stage in stages {
            let out_size = stage.out_size.unwrap_or(cur_size);
            current = self.render_stage(gpu, stage, &current, cur_size, out_size)?;
            cur_size = out_size;
        }
        Ok(current)
    }

    /// One stage's GPU pass: upload `source_rgba` as a texture, render
    /// through `stage`'s pipeline/sampler into an `out_size` target, read
    /// back RGBA8 bytes. Same texture-upload/render-pass/padded-readback
    /// shape as [`crate::scale::ScalePass::render`] (this module's
    /// `Params` UBO plays the role that pass's did) — duplicated rather
    /// than shared because the two passes' bind group layouts differ (this
    /// one always has a sampler binding; `ScalePass` never does, by
    /// design — its module doc).
    fn render_stage(
        &self,
        gpu: &GpuContext,
        stage: &ChainStage,
        source_rgba: &[u8],
        source_size: (u32, u32),
        out_size: (u32, u32),
    ) -> Result<Vec<u8>, String> {
        let (source_width, source_height) = source_size;
        let (out_width, out_height) = out_size;
        assert_eq!(
            source_rgba.len(),
            (source_width as usize) * (source_height as usize) * 4,
            "ShaderChain::render_stage: source buffer size does not match {source_width}x{source_height}"
        );

        let source_extent = wgpu::Extent3d {
            width: source_width,
            height: source_height,
            depth_or_array_layers: 1,
        };
        let source_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::shader_chain::source"),
            size: source_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            source_rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(source_width * 4),
                rows_per_image: Some(source_height),
            },
            source_extent,
        );
        let source_view = source_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let output_extent = wgpu::Extent3d {
            width: out_width,
            height: out_height,
            depth_or_array_layers: 1,
        };
        let output_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::shader_chain::output"),
            size: output_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let params_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::shader_chain::params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(
            &params_buffer,
            0,
            &params_bytes(stage.v0, out_width, out_height),
        );

        let (pipeline, sampler) = self.pipeline_and_sampler(stage.kind);

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rf-renderer::shader_chain::bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rf-renderer::shader_chain::encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::shader_chain::render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rpass.set_pipeline(pipeline);
            rpass.set_bind_group(0, &bind_group, &[]);
            rpass.draw(0..3, 0..1);
        }

        let bytes_per_row = out_width * 4;
        let padded_bytes_per_row = align_up(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback_size = u64::from(padded_bytes_per_row) * u64::from(out_height);
        let readback_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::shader_chain::readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(out_height),
                },
            },
            output_extent,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));

        let padded = read_buffer_sync(&gpu.device, &readback_buffer)?;
        let mut rgba = Vec::with_capacity((bytes_per_row as usize) * (out_height as usize));
        for row in padded
            .chunks(padded_bytes_per_row as usize)
            .take(out_height as usize)
        {
            rgba.extend_from_slice(&row[..bytes_per_row as usize]);
        }
        Ok(rgba)
    }
}

/// CPU oracle for `shaders/chain_sharp_bilinear.wgsl` — same UV/sharpness
/// formula, same `f32` precision, as the shader (module doc), but its own
/// hand-rolled 4-texel bilinear fetch with clamp-to-edge addressing
/// (matching [`ShaderChain::new`]'s linear sampler) rather than a codified
/// GPU output. Unlike [`crate::scale::render_scaled_reference`] (expected
/// byte-identical to its GPU pass), this oracle is **not** expected to
/// match the real GPU pass exactly — the residual gap against the GPU's
/// hardware bilinear unit is the measured noise this ticket's tolerance is
/// calibrated against (module doc).
///
/// # Panics
/// Panics if `source_rgba.len() != source_width * source_height * 4` —
/// same "malformed input is a caller bug" stance as every other oracle in
/// this crate.
#[must_use]
pub fn render_sharp_bilinear_reference(
    source_rgba: &[u8],
    source_width: u32,
    source_height: u32,
    out_width: u32,
    out_height: u32,
    sharpness: f32,
) -> Vec<u8> {
    assert_eq!(
        source_rgba.len(),
        (source_width as usize) * (source_height as usize) * 4,
        "render_sharp_bilinear_reference: source buffer size does not match \
         {source_width}x{source_height}"
    );
    let sharpness = sharpness.clamp(0.0, 1.0);
    #[allow(clippy::cast_precision_loss)]
    let src_w = source_width as f32;
    #[allow(clippy::cast_precision_loss)]
    let src_h = source_height as f32;
    #[allow(clippy::cast_precision_loss)]
    let inv_out_w = 1.0f32 / out_width as f32;
    #[allow(clippy::cast_precision_loss)]
    let inv_out_h = 1.0f32 / out_height as f32;

    let mut out = vec![0u8; (out_width as usize) * (out_height as usize) * 4];
    for oy in 0..out_height {
        for ox in 0..out_width {
            #[allow(clippy::cast_precision_loss)]
            let px = ox as f32 + 0.5;
            #[allow(clippy::cast_precision_loss)]
            let py = oy as f32 + 0.5;
            let uv = (px * inv_out_w, py * inv_out_h);
            let texel = (uv.0 * src_w, uv.1 * src_h);
            let nearest_texel = (texel.0.floor() + 0.5, texel.1.floor() + 0.5);
            let uv_nearest = (nearest_texel.0 / src_w, nearest_texel.1 / src_h);
            let uv_final = (
                uv.0 + (uv_nearest.0 - uv.0) * sharpness,
                uv.1 + (uv_nearest.1 - uv.1) * sharpness,
            );
            let color = sample_bilinear_clamped(
                source_rgba,
                source_width,
                source_height,
                uv_final.0,
                uv_final.1,
            );
            let idx = ((oy * out_width + ox) * 4) as usize;
            out[idx..idx + 4].copy_from_slice(&color);
        }
    }
    out
}

/// Manual bilinear fetch at `(u, v)` (`[0, 1]` texture-space, texel-center
/// convention), clamp-to-edge addressing — the CPU half of
/// [`render_sharp_bilinear_reference`], split out for readability.
fn sample_bilinear_clamped(rgba: &[u8], width: u32, height: u32, u: f32, v: f32) -> [u8; 4] {
    #[allow(clippy::cast_precision_loss)]
    let fx = u * width as f32 - 0.5;
    #[allow(clippy::cast_precision_loss)]
    let fy = v * height as f32 - 0.5;
    let x0f = fx.floor();
    let y0f = fy.floor();
    let tx = fx - x0f;
    let ty = fy - y0f;
    let max_x = i64::from(width) - 1;
    let max_y = i64::from(height) - 1;
    let x0 = (x0f as i64).clamp(0, max_x) as u32;
    let x1 = (x0f as i64 + 1).clamp(0, max_x) as u32;
    let y0 = (y0f as i64).clamp(0, max_y) as u32;
    let y1 = (y0f as i64 + 1).clamp(0, max_y) as u32;

    let texel = |x: u32, y: u32, c: usize| -> f32 {
        let idx = ((y * width + x) * 4) as usize + c;
        f32::from(rgba[idx])
    };

    let mut out = [0u8; 4];
    for (c, slot) in out.iter_mut().enumerate() {
        let top = texel(x0, y0, c) * (1.0 - tx) + texel(x1, y0, c) * tx;
        let bottom = texel(x0, y1, c) * (1.0 - tx) + texel(x1, y1, c) * tx;
        let v = top * (1.0 - ty) + bottom * ty;
        *slot = v.round().clamp(0.0, 255.0) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- In-crate tolerance comparator (module doc: rf-harness is out of
    // scope and dev-depends on rf-renderer already, so depending on it here
    // would be circular) -- mirrors `rf_harness::tolerance::
    // compare_with_tolerance`'s exact two-number metric. -------------------

    #[derive(Debug, Clone, Copy, PartialEq)]
    struct ToleranceConfig {
        channel_delta: u8,
        max_mismatch_fraction: f64,
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    struct ToleranceReport {
        total_pixels: usize,
        mismatched_pixels: usize,
        max_channel_delta_observed: u8,
        passed: bool,
    }

    fn compare_with_tolerance(
        actual: &[u8],
        reference: &[u8],
        cfg: &ToleranceConfig,
    ) -> ToleranceReport {
        assert_eq!(
            actual.len(),
            reference.len(),
            "compare_with_tolerance: length mismatch"
        );
        let total_pixels = actual.len() / 4;
        let mut mismatched_pixels = 0usize;
        let mut max_channel_delta_observed = 0u8;
        for (a_px, r_px) in actual.chunks_exact(4).zip(reference.chunks_exact(4)) {
            let mut pixel_max_delta = 0u8;
            for (a, r) in a_px.iter().zip(r_px.iter()) {
                let delta = a.abs_diff(*r);
                if delta > pixel_max_delta {
                    pixel_max_delta = delta;
                }
            }
            if pixel_max_delta > max_channel_delta_observed {
                max_channel_delta_observed = pixel_max_delta;
            }
            if pixel_max_delta > cfg.channel_delta {
                mismatched_pixels += 1;
            }
        }
        let mismatch_fraction = if total_pixels == 0 {
            0.0
        } else {
            mismatched_pixels as f64 / total_pixels as f64
        };
        ToleranceReport {
            total_pixels,
            mismatched_pixels,
            max_channel_delta_observed,
            passed: mismatch_fraction <= cfg.max_mismatch_fraction,
        }
    }

    /// Calibrated against this crate's real GPU sharp-bilinear pass vs. its
    /// CPU oracle across 11 configurations spanning source/output sizes
    /// from 7x13 to 256x240, non-integer scale factors on both axes, and
    /// sharpness 0.0/0.3/0.5/0.6/0.9/1.0 (measured on Metal, this ticket):
    /// at `channel_delta: 0` the mismatch fraction ranged from 0% to
    /// ~17.9% of pixels (bilinear-blended pixels differing from the CPU
    /// oracle's own bilinear by exactly 1 unit -- ordinary GPU-vs-CPU float
    /// rounding, not a bug), but **every single measured pixel across every
    /// configuration** was within 1 unit -- raising `channel_delta` to `1`
    /// alone brought every config to *zero* mismatched pixels, so
    /// `max_mismatch_fraction` stays at the tightest possible `0.0` rather
    /// than being loosened to paper over anything. Two of those 11 configs
    /// (the mild 16x16->31x31 and the worst-measured 256x240->293x224) are
    /// re-asserted directly against the real GPU pass in
    /// `sharp_bilinear_matches_cpu_oracle_within_tolerance` below, which
    /// also pins the *lower* bound mechanically: it asserts `channel_delta:
    /// 0` (nearest's own W3-01b threshold) is NOT enough here. The *upper*
    /// bound is looser -- this module's mutation tests
    /// (`wrong_filter_kernel_through_the_real_gpu_pass_fails_the_calibrated_tolerance`,
    /// `grossly_wrong_sharpness_oracle_vs_oracle_fails_the_calibrated_tolerance`)
    /// fail at a max observed delta of 69 (real GPU nearest pass vs. the
    /// sharp-bilinear oracle) and 65 (oracle-vs-oracle, wrong sharpness),
    /// meaning they'd fail equally at `channel_delta: 3` or `channel_delta:
    /// 10` -- they prove `1` isn't
    /// too *wide* to catch a wrong image, not that `1` specifically (rather
    /// than some other single-digit value) is the unique correct choice;
    /// `1` is the number this ticket reports because it is the tightest
    /// value the *measured noise* requires, not because the mutations
    /// forced it. Single-backend observation (Metal only, same posture
    /// `rf_renderer::scale`'s own tolerance and `docs/design/RENDERER.md`
    /// §7's golden hash both take) -- a red llvmpipe run in CI is a
    /// cross-backend question to investigate, not automatically a
    /// regression.
    const SHARP_BILINEAR_TOLERANCE: ToleranceConfig = ToleranceConfig {
        channel_delta: 1,
        max_mismatch_fraction: 0.0,
    };

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::request_headless() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                if std::env::var_os("CI").is_some() {
                    panic!("{test_name} cannot skip in CI: {e}");
                }
                eprintln!(
                    "SKIP {test_name}: no wgpu adapter in this environment ({e}) -- clean skip, \
                     run with --nocapture to see this line"
                );
                None
            }
        }
    }

    fn solid_rgba(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w as usize) * (h as usize) * 4);
        for _ in 0..(w * h) {
            v.extend_from_slice(&color);
        }
        v
    }

    /// High-contrast, non-uniform source (checkerboard + diagonal stripe),
    /// same shape as `crates/rf-harness/tests/scale_pass_tolerance.rs`'s
    /// `synthetic_source` -- guarantees every mutation below touches most
    /// pixels rather than getting lucky on a flat image.
    fn synthetic_source(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
        for y in 0..h {
            for x in 0..w {
                let checker = ((x / 4) + (y / 4)) % 2 == 0;
                let stripe = ((x.wrapping_add(y)) % 32) as u8;
                let base: u8 = if checker { 235 } else { 20 };
                out.extend_from_slice(&[
                    base.saturating_add(stripe),
                    base,
                    base.saturating_sub(stripe.min(base)),
                    255,
                ]);
            }
        }
        out
    }

    fn pixel_at(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]]
    }

    // --- Vacuity trap 2: empty chain must be byte-identical to the
    // unfiltered input, not merely "doesn't panic". --------------------

    #[test]
    fn empty_chain_is_byte_identical_to_the_input() {
        let Some(gpu) = gpu_or_skip("empty_chain_is_byte_identical_to_the_input") else {
            return;
        };
        let chain = ShaderChain::new(&gpu);
        let source = synthetic_source(16, 16);
        let out = chain
            .render(&gpu, &source, 16, 16, &[])
            .expect("empty chain must succeed");
        assert_eq!(
            out, source,
            "an empty chain must be byte-for-byte identical to the input (RENDERER.md §4)"
        );
    }

    // --- ShaderKind::Nearest, actually rendered: `ChainStage::nearest()`
    // is otherwise only ever constructed (never dispatched) by this
    // module's other tests, which would let a compiling-but-wrong
    // `chain_nearest.wgsl` (e.g. accidentally wired to the linear sampler)
    // through undetected. -------------------------------------------------

    #[test]
    fn nearest_pass_upscales_by_exact_block_replication_not_a_blur() {
        let Some(gpu) = gpu_or_skip("nearest_pass_upscales_by_exact_block_replication_not_a_blur")
        else {
            return;
        };
        let chain = ShaderChain::new(&gpu);
        // 2x2 checkerboard, upscaled 4x per axis via `ChainStage::nearest`
        // -- same "each source pixel must become an exact NxN block of its
        // own color" shape as `crate::scale`'s own nearest-neighbor test,
        // which is exactly what would fail if this pass were accidentally
        // bound to the linear sampler (bleeding neighbor colors into the
        // block edges) instead of the nearest one.
        let red = [255u8, 0, 0, 255];
        let green = [0u8, 255, 0, 255];
        let source = [red, green, green, red].concat();
        let out = chain
            .render(
                &gpu,
                &source,
                2,
                2,
                &[ChainStage::nearest().with_out_size(8, 8)],
            )
            .expect("nearest pass must succeed");

        assert_eq!(out.len(), 8 * 8 * 4);
        // Top-left 4x4 block: red, uniformly, no blend with its green
        // neighbors.
        for y in 0..4u32 {
            for x in 0..4u32 {
                assert_eq!(
                    pixel_at(&out, 8, x, y),
                    red,
                    "nearest upscale must replicate the top-left source pixel as a solid block, \
                     not blend with its neighbor (x={x}, y={y})"
                );
            }
        }
        // Top-right 4x4 block: green.
        for y in 0..4u32 {
            for x in 4..8u32 {
                assert_eq!(pixel_at(&out, 8, x, y), green);
            }
        }
        // Bottom-right 4x4 block: red again (checkerboard).
        for y in 4..8u32 {
            for x in 4..8u32 {
                assert_eq!(pixel_at(&out, 8, x, y), red);
            }
        }
    }

    // --- Vacuity trap 3: a param-UBO test that never varies a parameter
    // cannot tell whether the UBO is bound at all. ----------------------

    #[test]
    fn varying_scanline_intensity_changes_the_gpu_output() {
        let Some(gpu) = gpu_or_skip("varying_scanline_intensity_changes_the_gpu_output") else {
            return;
        };
        let chain = ShaderChain::new(&gpu);
        let source = solid_rgba(8, 8, [200, 150, 100, 255]);

        let off = chain
            .render(&gpu, &source, 8, 8, &[ChainStage::scanlines(0.0, 2.0)])
            .expect("scanlines pass at intensity 0 must succeed");
        let on = chain
            .render(&gpu, &source, 8, 8, &[ChainStage::scanlines(0.8, 2.0)])
            .expect("scanlines pass at intensity 0.8 must succeed");

        assert_ne!(
            off, on,
            "changing the `intensity` UBO field must change the GPU output -- if it doesn't, \
             the params buffer isn't actually bound"
        );
        // And prove it's genuinely per-row, not a flat dim across the whole
        // image: row 0 (darkened, period 2) must differ from row 1
        // (untouched) once intensity is on, but not when it's off.
        assert_eq!(
            pixel_at(&off, 8, 0, 0),
            pixel_at(&off, 8, 0, 1),
            "sanity: intensity 0 must leave every row identical"
        );
        assert_ne!(
            pixel_at(&on, 8, 0, 0),
            pixel_at(&on, 8, 0, 1),
            "intensity 0.8 must make the darkened row visibly differ from its untouched neighbor"
        );

        // `period` (v0.y) is a second declared manifest field
        // (`ShaderKind::Scanlines.manifest().params[1]`) that the assertions
        // above never exercise -- same vacuity trap 3 one level down (a
        // wrong byte offset or a `max(u32(p.v0.z), 1u)` typo reading the
        // wrong field would go unnoticed if only `intensity` were varied).
        // period 2 darkens rows 0,2,4,6; period 4 darkens rows 0,4 only --
        // row 2 is the discriminator: darkened under period 2, untouched
        // under period 4.
        let period_2 = chain
            .render(&gpu, &source, 8, 8, &[ChainStage::scanlines(0.8, 2.0)])
            .expect("scanlines pass at period 2 must succeed");
        let period_4 = chain
            .render(&gpu, &source, 8, 8, &[ChainStage::scanlines(0.8, 4.0)])
            .expect("scanlines pass at period 4 must succeed");
        assert_ne!(
            pixel_at(&period_2, 8, 0, 2),
            pixel_at(&period_4, 8, 0, 2),
            "changing the `period` UBO field must change which rows are darkened -- row 2 must \
             be darkened at period 2 but untouched at period 4"
        );
        assert_eq!(
            pixel_at(&period_4, 8, 0, 2),
            pixel_at(&period_4, 8, 0, 1),
            "sanity: at period 4, row 2 must be as untouched as row 1 (neither is a multiple of \
             4)"
        );
    }

    // --- Vacuity trap 1: a one-pass chain cannot detect ordering bugs.
    // Chain two order-sensitive passes and assert forward != reversed. --

    #[test]
    fn chain_order_is_significant_forward_differs_from_reversed() {
        let Some(gpu) = gpu_or_skip("chain_order_is_significant_forward_differs_from_reversed")
        else {
            return;
        };
        let chain = ShaderChain::new(&gpu);
        let source = synthetic_source(8, 8);

        let scan = ChainStage::scanlines(0.7, 2.0);
        let upscale = ChainStage::sharp_bilinear(0.0).with_out_size(16, 16);

        // Forward: darken alternating *source*-resolution rows, then
        // upscale -- the upscaler's bilinear blend mixes each darkened row
        // with its untouched neighbor.
        let forward = chain
            .render(&gpu, &source, 8, 8, &[scan, upscale])
            .expect("forward chain must succeed");
        // Reversed: upscale first (no darkening baked in yet, so the
        // bilinear blend has nothing darkened to mix), then darken
        // alternating rows of the *upscaled* (16-tall) image -- a
        // different set of source rows ends up darkened than in the
        // forward case.
        let reversed = chain
            .render(&gpu, &source, 8, 8, &[upscale, scan])
            .expect("reversed chain must succeed");

        assert_eq!(
            forward.len(),
            reversed.len(),
            "both orderings produce the same final size"
        );
        assert_ne!(
            forward, reversed,
            "chaining [scanlines, upscale] must produce a different result than \
             [upscale, scanlines] -- if they match, the chain isn't actually applying passes \
             in order"
        );
    }

    // --- Manifest / provenance (acceptance criterion 4): every shader
    // records a non-empty licence and authorship statement. ---------------

    #[test]
    fn every_shader_kind_has_a_nonempty_provenance_manifest() {
        for kind in [
            ShaderKind::Nearest,
            ShaderKind::SharpBilinear,
            ShaderKind::Scanlines,
        ] {
            let m = kind.manifest();
            assert!(!m.id.is_empty());
            assert!(!m.license.is_empty());
            assert!(
                m.authorship.contains("Authored fresh"),
                "{:?}'s manifest must state how the WGSL was authored, got: {}",
                kind,
                m.authorship
            );
        }
    }

    #[test]
    fn sharp_bilinear_and_scanlines_manifests_expose_their_tunable_params() {
        assert_eq!(ShaderKind::Nearest.manifest().params.len(), 0);
        let bilinear_params = ShaderKind::SharpBilinear.manifest().params;
        assert_eq!(bilinear_params.len(), 1);
        assert_eq!(bilinear_params[0].name, "sharpness");
        let scanline_params = ShaderKind::Scanlines.manifest().params;
        assert_eq!(scanline_params.len(), 2);
        assert_eq!(scanline_params[0].name, "intensity");
        assert_eq!(scanline_params[1].name, "period");
    }

    // --- Runtime selectability: chain description is plain data, built at
    // runtime, not a hardcoded compile-time pipeline. ----------------------

    #[test]
    fn chain_stages_are_runtime_constructed_data_not_compile_time_fixed() {
        // No GPU needed -- this only proves ChainStage/ShaderKind behave as
        // ordinary runtime values (RENDERER.md §4: "chain description is
        // data"), which a caller could load from user/game settings.
        let stages_a: Vec<ChainStage> = vec![ChainStage::nearest()];
        let stages_b: Vec<ChainStage> = vec![
            ChainStage::scanlines(0.5, 2.0),
            ChainStage::sharp_bilinear(0.6).with_out_size(64, 64),
        ];
        assert_eq!(stages_a.len(), 1);
        assert_eq!(stages_b.len(), 2);
        assert_eq!(stages_b[0].kind, ShaderKind::Scanlines);
        assert_eq!(stages_b[1].kind, ShaderKind::SharpBilinear);
    }

    // --- CPU oracle sanity: identity mapping (out size == in size,
    // sharpness irrelevant since every UV already lands on a texel center)
    // must reproduce the source exactly. -----------------------------------

    #[test]
    fn sharp_bilinear_reference_at_identity_size_reproduces_the_source_exactly() {
        let source = synthetic_source(6, 6);
        let out = render_sharp_bilinear_reference(&source, 6, 6, 6, 6, 0.0);
        assert_eq!(
            out, source,
            "identity-size sampling always lands exactly on a texel center, so even sharpness \
             0 (pure bilinear) must reproduce the source exactly -- no fractional blend is \
             possible when out_size == source_size"
        );
    }

    // --- Acceptance criterion 5: nonzero tolerance calibrated for a
    // filtering shader, measured against the real GPU pass. ----------------

    /// Two configurations from this constant's own calibration survey
    /// (module doc's `SHARP_BILINEAR_TOLERANCE` comment): the mild end
    /// (16x16 -> 31x31, 3.95% mismatched at `channel_delta: 0`) and the
    /// worst measured end (256x240 -> 293x224, `ScaleGeometry`'s own
    /// default 1x NES output shape, 17.90% mismatched at `channel_delta:
    /// 0`) -- both are asserted here, not just the mild one, so CI
    /// actually exercises the config the threshold's upper bound came
    /// from, not only the easiest case in the survey.
    #[test]
    fn sharp_bilinear_matches_cpu_oracle_within_tolerance() {
        let Some(gpu) = gpu_or_skip("sharp_bilinear_matches_cpu_oracle_within_tolerance") else {
            return;
        };
        let chain = ShaderChain::new(&gpu);
        let sharpness = 0.6;
        let configs: &[(u32, u32, u32, u32)] = &[
            (16, 16, 31, 31),     // mild: 3.95% mismatched at delta 0
            (256, 240, 293, 224), // worst measured: 17.90% mismatched at delta 0
        ];

        for &(src_w, src_h, out_w, out_h) in configs {
            let source = synthetic_source(src_w, src_h);
            let actual = chain
                .render(
                    &gpu,
                    &source,
                    src_w,
                    src_h,
                    &[ChainStage::sharp_bilinear(sharpness).with_out_size(out_w, out_h)],
                )
                .expect("sharp-bilinear GPU pass must succeed");
            let reference =
                render_sharp_bilinear_reference(&source, src_w, src_h, out_w, out_h, sharpness);
            assert_eq!(actual.len(), reference.len(), "geometry must match exactly");

            // Acceptance criterion 5's actual claim: `channel_delta: 0`
            // (nearest's own threshold, W3-01b) is NOT enough for a
            // filtering pass -- pin that mechanically rather than only
            // asserting the chosen nonzero value passes.
            let zero_tolerance = ToleranceConfig {
                channel_delta: 0,
                max_mismatch_fraction: 0.0,
            };
            let zero_report = compare_with_tolerance(&actual, &reference, &zero_tolerance);
            assert!(
                !zero_report.passed,
                "sanity: {src_w}x{src_h} -> {out_w}x{out_h} must NOT pass at channel_delta 0 -- \
                 if it does, this config doesn't actually demonstrate why sharp-bilinear needs a \
                 nonzero threshold"
            );

            let report = compare_with_tolerance(&actual, &reference, &SHARP_BILINEAR_TOLERANCE);
            eprintln!(
                "sharp-bilinear GPU vs CPU oracle ({src_w}x{src_h} -> {out_w}x{out_h}): \
                 {}/{} pixels mismatched at delta 0 ({:.2}%), max channel delta observed {} \
                 (tolerance: channel_delta {}, max_mismatch_fraction {})",
                zero_report.mismatched_pixels,
                zero_report.total_pixels,
                100.0 * zero_report.mismatched_pixels as f64 / zero_report.total_pixels as f64,
                report.max_channel_delta_observed,
                SHARP_BILINEAR_TOLERANCE.channel_delta,
                SHARP_BILINEAR_TOLERANCE.max_mismatch_fraction,
            );
            assert!(
                report.passed,
                "sharp-bilinear GPU output ({src_w}x{src_h} -> {out_w}x{out_h}) diverged from \
                 the CPU oracle beyond the calibrated tolerance: {}/{} pixels mismatched, max \
                 channel delta {}",
                report.mismatched_pixels, report.total_pixels, report.max_channel_delta_observed
            );
        }
    }

    /// Mutation, through the **real GPU pass** (this ticket's brief:
    /// "mutation-verify the threshold still rejects a wrong image" — an
    /// actual render, not two CPU-oracle numbers compared to each other):
    /// render the source through [`ChainStage::nearest`] (a genuinely
    /// different, blocky filter kernel) at the same size the calibration
    /// test uses, and compare it against the *sharp-bilinear* CPU oracle at
    /// [`SHARP_BILINEAR_TOLERANCE`]. "Wrong filter kernel" is exactly the
    /// failure mode this tolerance exists to catch, and this is the
    /// closest thing to it that doesn't require deliberately breaking
    /// `chain_sharp_bilinear.wgsl` itself.
    #[test]
    fn wrong_filter_kernel_through_the_real_gpu_pass_fails_the_calibrated_tolerance() {
        let Some(gpu) = gpu_or_skip(
            "wrong_filter_kernel_through_the_real_gpu_pass_fails_the_calibrated_tolerance",
        ) else {
            return;
        };
        let chain = ShaderChain::new(&gpu);
        let source = synthetic_source(16, 16);
        let out_w = 31;
        let out_h = 31;

        let wrong_kernel = chain
            .render(
                &gpu,
                &source,
                16,
                16,
                &[ChainStage::nearest().with_out_size(out_w, out_h)],
            )
            .expect("nearest pass must succeed");
        let reference = render_sharp_bilinear_reference(&source, 16, 16, out_w, out_h, 0.6);

        let report = compare_with_tolerance(&wrong_kernel, &reference, &SHARP_BILINEAR_TOLERANCE);
        assert!(
            !report.passed,
            "a nearest-filtered render compared against the sharp-bilinear reference must fail \
             the calibrated tolerance, but it passed ({}/{} pixels mismatched, max delta {}) -- \
             the tolerance is too wide",
            report.mismatched_pixels, report.total_pixels, report.max_channel_delta_observed
        );
        eprintln!(
            "wrong-filter-kernel mutation (real GPU nearest pass vs. sharp-bilinear reference): \
             {}/{} pixels mismatched, max channel delta {} -- correctly FAILED",
            report.mismatched_pixels, report.total_pixels, report.max_channel_delta_observed
        );
    }

    /// Second mutation, isolating the tolerance comparator itself (no GPU):
    /// two CPU-oracle renders at badly different sharpness values (`0.0`
    /// and `1.0`, both vs. the calibrated pass's `0.6`) must still fail
    /// [`SHARP_BILINEAR_TOLERANCE`] against each other. Kept alongside
    /// `wrong_filter_kernel_through_the_real_gpu_pass_fails_the_calibrated_tolerance`
    /// above (which is the "wrong image through the real pipeline" case)
    /// as a second, independent check that doesn't depend on any GPU being
    /// present.
    #[test]
    fn grossly_wrong_sharpness_oracle_vs_oracle_fails_the_calibrated_tolerance() {
        let source = synthetic_source(16, 16);
        let correct = render_sharp_bilinear_reference(&source, 16, 16, 31, 31, 0.6);
        for wrong_sharpness in [0.0, 1.0] {
            let wrong = render_sharp_bilinear_reference(&source, 16, 16, 31, 31, wrong_sharpness);
            let report = compare_with_tolerance(&wrong, &correct, &SHARP_BILINEAR_TOLERANCE);
            assert!(
                !report.passed,
                "a badly wrong sharpness ({wrong_sharpness} vs. the calibrated 0.6) must fail \
                 the tolerance check, but it passed ({}/{} pixels mismatched, max delta {}) -- \
                 the tolerance is too wide",
                report.mismatched_pixels, report.total_pixels, report.max_channel_delta_observed
            );
        }
    }
}
