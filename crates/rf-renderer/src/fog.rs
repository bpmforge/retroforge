//! Fog/steam post pass (ticket W16-04; `docs/design/ENHANCEMENT_WAVE_16.md`
//! §4, §8; NON_GOALS #15 — WGSL + parameter UBOs only, no shader language).
//!
//! ## Why this is not another [`crate::shader_chain::ChainStage`]
//!
//! RENDERER.md §4's common chain interface is `tex_in, sampler, params:
//! UBO -> tex_out` — exactly one input texture. This pass needs **two**:
//! the already-composited scene (so the atmosphere plane's own silhouette,
//! rendered underneath by the normal pipeline, survives untouched —
//! acceptance criterion 1) and a density map built from the atmosphere
//! layer's own pixels. Bolting a second texture onto `ShaderChain`'s
//! single shared bind-group layout would either break every existing
//! shader's layout or require a second layout anyway, so [`FogPass`] is a
//! standalone pipeline, following the same conventions (32-byte two-`vec4`
//! UBO, one shared filtering sampler, [`FogManifest`] shaped like
//! [`crate::shader_chain::ShaderManifest`]) without pretending to be a
//! `ChainStage`.
//!
//! ## Architecture boundary (ARCHITECTURE.md §3; `rf_enhance::scene_graph`'s
//! own module doc)
//!
//! `rf-renderer` does not depend on `rf-enhance` in either direction. This
//! module therefore knows nothing about `SceneLayer::ExtractedBg` or
//! `BgLayerId` — it takes plain RGBA8 density bytes and a drift vector.
//! Converting an `ExtractedBg` layer's pixels (and its raw hardware scroll
//! telemetry) into that density buffer is the app shell's job
//! (`crates/retroforge/src/enhanced_view.rs`), exactly the split
//! `rf_enhance::scene_graph::solidity_mask`'s own doc explains for the
//! same reason on the other side of this boundary.
//!
//! ## Frame-budget gate (§8)
//!
//! [`BudgetGate`] is a small, GPU-free rolling-p95 tracker shared by any
//! pass that needs the same self-disabling behaviour §8 requires of the
//! fog pass, the real-time AI pass (W16-07) and MetalFX (W16-08) — none of
//! them invents its own threshold (§8's own wording). It has no notion of
//! *why* a sample was slow and no I/O of its own; recording a report-card
//! entry when it disables is the caller's job
//! (`rf_enhance::trust::TrustLadder::record_contradiction`, which this
//! crate cannot call directly for the same dependency-direction reason as
//! above).

use std::borrow::Cow;
use std::collections::VecDeque;

use crate::gpu::{align_up, read_buffer_sync, GpuContext};

const FOG_SHADER_SRC: &str = include_str!("shaders/fog.wgsl");

/// Provenance/UI manifest, same shape and fields as
/// [`crate::shader_chain::ShaderManifest`] (module doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FogManifest {
    pub id: &'static str,
    pub display_name: &'static str,
    pub license: &'static str,
    pub basis: Option<&'static str>,
    pub authorship: &'static str,
}

/// This pass's manifest (acceptance-parity with
/// [`crate::shader_chain::ShaderKind::manifest`]).
#[must_use]
pub fn manifest() -> FogManifest {
    FogManifest {
        id: "fog",
        display_name: "Atmosphere: fog",
        license: "Apache-2.0 OR MIT",
        basis: None,
        authorship: "Authored fresh for ticket W16-04: multi-octave scrolled sampling of a \
                     density map with soft (weighted-average) accumulation is a standard \
                     screen-space technique description, not a port of any specific existing \
                     shader. See shaders/fog.wgsl's own header.",
    }
}

/// Per-frame tunable inputs (module doc: the app shell builds these from
/// an `ExtractedBg` layer's scroll telemetry plus wall-clock time — this
/// crate has no clock and no scene-graph dependency of its own).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FogParams {
    /// Seconds since this fog instance started drifting. Caller-owned so
    /// this pass has no clock of its own (CLAUDE.md law 4's "cores read
    /// frames, never own time" posture, applied here to keep this crate's
    /// GPU passes equally free of hidden state).
    pub time_secs: f32,
    /// UV-space drift per second, derived from the atmosphere layer's raw
    /// scroll telemetry (`SceneLayer::ExtractedBg::scroll`) by the caller.
    pub drift_x: f32,
    pub drift_y: f32,
    /// Overall opacity/strength in `[0, 1]`; the shader clamps regardless.
    pub strength: f32,
}

impl FogParams {
    #[must_use]
    pub fn new(time_secs: f32, drift_x: f32, drift_y: f32, strength: f32) -> Self {
        FogParams {
            time_secs,
            drift_x,
            drift_y,
            strength,
        }
    }
}

fn params_bytes(params: FogParams, out_width: u32, out_height: u32) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0..4].copy_from_slice(&params.time_secs.to_le_bytes());
    bytes[4..8].copy_from_slice(&params.drift_x.to_le_bytes());
    bytes[8..12].copy_from_slice(&params.drift_y.to_le_bytes());
    bytes[12..16].copy_from_slice(&params.strength.to_le_bytes());
    // v1.xy left at 0.0 so the shader's own `select` falls back to its
    // documented default octave scales (2.3, 4.1) -- this pass has no
    // per-call reason to override them yet, but the field exists so a
    // future caller (or a tuning UI) can without a shader change.
    bytes[16..20].copy_from_slice(&0.0f32.to_le_bytes());
    bytes[20..24].copy_from_slice(&0.0f32.to_le_bytes());
    bytes[24..28].copy_from_slice(&(1.0 / out_width as f32).to_le_bytes());
    bytes[28..32].copy_from_slice(&(1.0 / out_height as f32).to_le_bytes());
    bytes
}

/// The fog pass's own pipeline/bind-group-layout/sampler (module doc: not
/// a [`crate::shader_chain::ChainStage`] — two input textures).
pub struct FogPass {
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

impl FogPass {
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("rf-renderer::fog::bind_group_layout"),
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
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("rf-renderer::fog::pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });

        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("rf-renderer::fog::shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(FOG_SHADER_SRC)),
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("rf-renderer::fog::pipeline"),
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
            });

        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rf-renderer::fog::sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        FogPass {
            bind_group_layout,
            pipeline,
            sampler,
        }
    }

    /// Render fog over `scene_rgba`, using `density_rgba` (same
    /// `width`x`height`) as the density map — same upload/render-pass/
    /// padded-readback shape as
    /// [`crate::shader_chain::ShaderChain::render_stage`] (module doc).
    ///
    /// # Errors
    /// Returns `Err` if the GPU readback does not complete within
    /// [`crate::gpu::GPU_WAIT`].
    pub fn render(
        &self,
        gpu: &GpuContext,
        scene_rgba: &[u8],
        density_rgba: &[u8],
        width: u32,
        height: u32,
        params: FogParams,
    ) -> Result<Vec<u8>, String> {
        assert_eq!(
            scene_rgba.len(),
            (width as usize) * (height as usize) * 4,
            "FogPass::render: scene buffer size does not match {width}x{height}"
        );
        assert_eq!(
            density_rgba.len(),
            scene_rgba.len(),
            "FogPass::render: density buffer must be the same size as the scene"
        );

        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let make_input_tex = |label: &str, bytes: &[u8]| -> wgpu::TextureView {
            let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                extent,
            );
            tex.create_view(&wgpu::TextureViewDescriptor::default())
        };

        let scene_view = make_input_tex("rf-renderer::fog::scene", scene_rgba);
        let density_view = make_input_tex("rf-renderer::fog::density", density_rgba);

        let output_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::fog::output"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let params_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::fog::params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue
            .write_buffer(&params_buffer, 0, &params_bytes(params, width, height));

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rf-renderer::fog::bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&density_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rf-renderer::fog::encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::fog::render_pass"),
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
            rpass.set_pipeline(&self.pipeline);
            rpass.set_bind_group(0, &bind_group, &[]);
            rpass.draw(0..3, 0..1);
        }

        let bytes_per_row = width * 4;
        let padded_bytes_per_row = align_up(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback_size = u64::from(padded_bytes_per_row) * u64::from(height);
        let readback_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::fog::readback"),
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
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));

        let padded = read_buffer_sync(&gpu.device, &readback_buffer)?;
        let mut rgba = Vec::with_capacity((bytes_per_row as usize) * (height as usize));
        for row in padded
            .chunks(padded_bytes_per_row as usize)
            .take(height as usize)
        {
            rgba.extend_from_slice(&row[..bytes_per_row as usize]);
        }
        Ok(rgba)
    }
}

// ---------------------------------------------------------------------
// Frame-budget gate (§8): a rolling p95 tracker that disables the pass it
// guards once p95 exceeds the measured threshold, with hysteresis so it
// does not flap sample-to-sample right at the boundary.
// ---------------------------------------------------------------------

/// One 60 fps frame, milliseconds (§8: "Threshold: p95 < 16.67 ms"). This
/// is the number W16-01's harness measured against, not invented here —
/// see `docs/design/ENHANCEMENT_WAVE_16.md` §8 and `docs/evidence/
/// gpu-passes.json`.
pub const DISABLE_P95_MS: f64 = 16.67;

/// Re-enable threshold, deliberately lower than [`DISABLE_P95_MS`]
/// (hysteresis) — a pass that just barely tripped the gate must not
/// immediately re-arm on the very next slightly-faster sample, which
/// would flap on/off every other frame right at the boundary and make the
/// on/off transition itself a visible artifact. 20% headroom below the
/// disable line is an arbitrary-but-documented margin, the same
/// calibration stance `rf_enhance::atmosphere`'s constants take: not a
/// measured commercial number, a plain reading of "hysteresis" for a
/// first cut.
pub const REENABLE_P95_MS: f64 = 13.0;

/// How many trailing samples the rolling p95 is computed over. Half a
/// second at 60 fps, matching `rf_enhance::atmosphere::WINDOW_FRAMES`'s
/// own window/reasoning: long enough that one slow frame (a GC pause, a
/// driver hiccup) cannot flip the gate, short enough that a genuine
/// regression is caught well inside a second of play.
pub const GATE_WINDOW: usize = 30;

/// A rolling-p95 self-disabling gate for one GPU pass (module doc). GPU-
/// free and clock-free — the caller supplies each sample's duration in
/// milliseconds (a CPU-side timestamp around submit, or a wgpu timestamp
/// query converted to ms, per this ticket's brief); this type has no
/// timer of its own so it is unit-testable without a GPU or a clock.
#[derive(Debug, Clone)]
pub struct BudgetGate {
    window: VecDeque<f64>,
    enabled: bool,
    /// How many times this gate has flipped to disabled — a report-card
    /// caller (e.g. `rf_enhance::trust::TrustLadder::record_contradiction`)
    /// can use this to record "disabled" exactly once per transition
    /// rather than once per frame it stays disabled.
    disable_transitions: u32,
}

impl BudgetGate {
    #[must_use]
    pub fn new() -> Self {
        BudgetGate {
            window: VecDeque::with_capacity(GATE_WINDOW),
            enabled: true,
            disable_transitions: 0,
        }
    }

    /// Record one frame's measured pass duration (milliseconds) and
    /// re-evaluate. Returns `true` exactly on the frame this call flips
    /// `enabled` from `true` to `false` — the caller's cue to record a
    /// report-card entry.
    pub fn record_sample_ms(&mut self, ms: f64) -> bool {
        self.window.push_back(ms);
        if self.window.len() > GATE_WINDOW {
            self.window.pop_front();
        }
        if self.window.len() < GATE_WINDOW {
            // Not enough samples for a meaningful p95 yet -- stay in
            // whatever state the gate already was (fresh gates start
            // enabled, matching "the pass is eligible until measured
            // otherwise").
            return false;
        }

        let p95 = self.p95();
        let was_enabled = self.enabled;
        if self.enabled && p95 > DISABLE_P95_MS {
            self.enabled = false;
        } else if !self.enabled && p95 < REENABLE_P95_MS {
            self.enabled = true;
        }
        let just_disabled = was_enabled && !self.enabled;
        if just_disabled {
            self.disable_transitions += 1;
        }
        just_disabled
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn disable_transitions(&self) -> u32 {
        self.disable_transitions
    }

    /// Current rolling p95 over the window (nearest-rank method, matching
    /// `bin/bench_passes/main.rs`'s own `percentiles` -- one instrument,
    /// not two independently-invented ones). `0.0` before the window
    /// fills.
    #[must_use]
    pub fn p95(&self) -> f64 {
        if self.window.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f64> = self.window.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let n = sorted.len();
        let idx = ((0.95 * n as f64).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1);
        sorted[idx]
    }
}

impl Default for BudgetGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_gate_is_enabled_and_reports_no_transitions() {
        let gate = BudgetGate::new();
        assert!(gate.is_enabled());
        assert_eq!(gate.disable_transitions(), 0);
        assert_eq!(gate.p95(), 0.0);
    }

    #[test]
    fn the_gate_does_not_judge_before_its_window_fills() {
        let mut gate = BudgetGate::new();
        for _ in 0..(GATE_WINDOW - 1) {
            let disabled = gate.record_sample_ms(1000.0); // absurdly slow
            assert!(!disabled, "must not disable before the window fills");
        }
        assert!(
            gate.is_enabled(),
            "still enabled -- not enough samples for a verdict yet"
        );
    }

    #[test]
    fn p95_over_budget_disables_the_pass_exactly_once() {
        let mut gate = BudgetGate::new();
        let mut disabled_on = Vec::new();
        for i in 0..(GATE_WINDOW + 10) {
            if gate.record_sample_ms(20.0) {
                disabled_on.push(i);
            }
        }
        assert!(!gate.is_enabled(), "20ms p95 must trip the 16.67ms gate");
        assert_eq!(
            disabled_on.len(),
            1,
            "the transition must be reported exactly once, not once per frame: {disabled_on:?}"
        );
        assert_eq!(gate.disable_transitions(), 1);
    }

    #[test]
    fn healthy_samples_never_disable_the_gate() {
        let mut gate = BudgetGate::new();
        for _ in 0..(GATE_WINDOW * 3) {
            let disabled = gate.record_sample_ms(1.6); // this crate's own measured nearest/xbr/crt range
            assert!(!disabled);
        }
        assert!(gate.is_enabled());
        assert_eq!(gate.disable_transitions(), 0);
    }

    /// Hysteresis: once disabled, the gate must not re-enable on a single
    /// sample that merely dips under the DISABLE line -- it needs the
    /// LOWER re-enable line, sustained across the rolling window.
    #[test]
    fn hysteresis_requires_the_lower_reenable_line_not_just_under_disable() {
        let mut gate = BudgetGate::new();
        for _ in 0..GATE_WINDOW {
            gate.record_sample_ms(20.0);
        }
        assert!(!gate.is_enabled());

        // Feed samples between REENABLE_P95_MS and DISABLE_P95_MS -- under
        // the disable line but not under the (lower) re-enable line.
        for _ in 0..GATE_WINDOW {
            gate.record_sample_ms(15.0);
        }
        assert!(
            !gate.is_enabled(),
            "15ms is under the disable line but still over the re-enable line -- must stay off"
        );

        // Now genuinely recover.
        for _ in 0..GATE_WINDOW {
            gate.record_sample_ms(2.0);
        }
        assert!(gate.is_enabled(), "a real recovery must re-enable the pass");
    }

    #[test]
    fn a_disable_then_recover_then_disable_cycle_counts_two_transitions() {
        let mut gate = BudgetGate::new();
        for _ in 0..GATE_WINDOW {
            gate.record_sample_ms(20.0);
        }
        for _ in 0..GATE_WINDOW {
            gate.record_sample_ms(2.0);
        }
        assert!(gate.is_enabled());
        for _ in 0..GATE_WINDOW {
            gate.record_sample_ms(20.0);
        }
        assert!(!gate.is_enabled());
        assert_eq!(gate.disable_transitions(), 2);
    }

    #[test]
    fn manifest_names_the_effect_and_states_authorship() {
        let m = manifest();
        assert_eq!(m.display_name, "Atmosphere: fog");
        assert!(!m.authorship.is_empty());
        assert_eq!(m.basis, None);
    }
}
