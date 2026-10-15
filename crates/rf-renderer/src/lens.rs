//! HD-2D lens and lighting post pass (ticket W30-04; `docs/design/
//! ENHANCEMENT_WAVE_30.md`; NON_GOALS #15 -- WGSL + parameter UBOs only).
//!
//! Five presentation effects, each opt-in with its own strength in
//! `0.0..=1.0`, composed in one pass over the picture the shell is about to
//! show: **bloom**, **tilt-shift** depth of field, soft **sprite shadows**,
//! **day/night grading** and a **weather** overlay (rain or snow).
//!
//! ## Why this is its own pass and not a `ChainStage`
//!
//! Same reason as [`crate::fog`]: the chain interface is one texture in,
//! one out. This pass reads the scene, optionally a second image (the
//! sprite-only layer the shadows need) and its own blurred intermediates.
//! It follows fog's conventions otherwise: a standalone pipeline, a
//! parameter UBO, a [`LensManifest`], headless tests, and the shared
//! [`crate::fog::BudgetGate`] (owned by the caller) so a machine that
//! cannot afford it switches it off.
//!
//! ## Shape
//!
//! Four draws in one command buffer, no readback in between:
//!
//! 1. *down*: scene -> a roughly 128x112 texture set (scene, bright-pass,
//!    sprite alpha), `d x d` pixels per texel with `d` derived from the
//!    output height, so the blur looks the same at 1x and at 4x;
//! 2. *blur H* and 3. *blur V*: one separable 5-tap Gaussian (bilinear
//!    offsets) over all three at once;
//! 4. *final*: full resolution, reads the **untouched** scene pixel with an
//!    integer `textureLoad`, so an effect at strength 0 is the identity
//!    byte for byte -- the property the tests pin.
//!
//! Passes 1-3 are skipped when no effect needs a blur (grade and weather
//! only). Everything is deterministic: the weather hashes the caller's
//! frame counter with an integer hash, so the same frame number draws the
//! same sky on every backend and nothing here owns any state.
//!
//! ## Architecture boundary
//!
//! Presentation only: the input is the RGBA the shell already composed, the
//! output is RGBA of the same size. This crate cannot name a core command,
//! so the pass cannot influence the simulation (CLAUDE.md law 6).

use std::borrow::Cow;

use crate::gpu::{align_up, read_buffer_sync, GpuContext};

const LENS_SHADER_SRC: &str = include_str!("shaders/lens.wgsl");

/// Size of the parameter block: seven `vec4<f32>`.
pub const PARAMS_BYTES: usize = 112;

/// Provenance/UI manifest, same shape as [`crate::fog::FogManifest`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LensManifest {
    pub id: &'static str,
    pub display_name: &'static str,
    pub license: &'static str,
    pub basis: Option<&'static str>,
    pub authorship: &'static str,
}

/// This pass's manifest.
#[must_use]
pub fn manifest() -> LensManifest {
    LensManifest {
        id: "lens",
        display_name: "HD-2D lens and lighting",
        license: "Apache-2.0 OR MIT",
        basis: None,
        authorship: "Authored fresh for ticket W30-04: bright-pass bloom, a banded blur, \
                     blurred-alpha shadows, a tint-and-exposure grade and a hashed rain/snow \
                     overlay are standard screen-space techniques, not a port of any specific \
                     shader. See shaders/lens.wgsl's own header.",
    }
}

/// Which sky the weather overlay draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Weather {
    #[default]
    None,
    Rain,
    Snow,
}

impl Weather {
    /// Every choice, in picker order.
    pub const ALL: [Weather; 3] = [Weather::None, Weather::Rain, Weather::Snow];

    /// The word shown in the picker.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Weather::None => "Clear",
            Weather::Rain => "Rain",
            Weather::Snow => "Snow",
        }
    }

    /// The stable word written to a settings file.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Weather::None => "none",
            Weather::Rain => "rain",
            Weather::Snow => "snow",
        }
    }

    /// Inverse of [`Self::key`]; `None` for anything else.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Weather> {
        Weather::ALL.into_iter().find(|w| w.key() == key)
    }

    /// The value the shader switches on.
    #[must_use]
    pub const fn code(self) -> f32 {
        match self {
            Weather::None => 0.0,
            Weather::Rain => 1.0,
            Weather::Snow => 2.0,
        }
    }
}

/// One effect: an opt-in flag and a strength.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Effect {
    pub enabled: bool,
    /// `0.0..=1.0`; out-of-range and NaN are clamped by [`Self::level`].
    pub strength: f32,
}

impl Effect {
    pub const OFF: Effect = Effect {
        enabled: false,
        strength: 0.0,
    };

    #[must_use]
    pub const fn on(strength: f32) -> Self {
        Effect {
            enabled: true,
            strength,
        }
    }

    /// What reaches the shader: `0.0` when off, otherwise the strength
    /// clamped to `0.0..=1.0` (NaN counts as 0).
    #[must_use]
    pub fn level(self) -> f32 {
        if self.enabled && self.strength > 0.0 {
            self.strength.min(1.0)
        } else {
            0.0
        }
    }
}

/// Everything the pass needs for one frame. `Default` is "nothing on".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LensParams {
    pub bloom: Effect,
    /// Luma above which a pixel feeds the bloom, `0.0..1.0`.
    pub bloom_threshold: f32,
    pub tilt_shift: Effect,
    /// Vertical centre of the sharp band, as a fraction of the height.
    pub tilt_center: f32,
    /// Height of the sharp band, as a fraction of the height.
    pub tilt_band: f32,
    pub shadows: Effect,
    pub grade: Effect,
    /// Time of day for the grade, `0.0..=1.0` (0 midnight, 0.5 noon).
    pub time_of_day: f32,
    pub weather: Weather,
    /// Weather density, `0.0..=1.0`; 0 draws nothing.
    pub weather_density: f32,
    /// The caller's frame counter: the weather's only source of motion.
    pub frame: u32,
}

impl Default for LensParams {
    fn default() -> Self {
        LensParams {
            bloom: Effect::OFF,
            bloom_threshold: 0.6,
            tilt_shift: Effect::OFF,
            tilt_center: 0.5,
            tilt_band: 0.3,
            shadows: Effect::OFF,
            grade: Effect::OFF,
            time_of_day: 0.5,
            weather: Weather::None,
            weather_density: 0.0,
            frame: 0,
        }
    }
}

impl LensParams {
    /// Weather density as the shader sees it: 0 unless a sky is chosen.
    #[must_use]
    pub fn weather_level(&self) -> f32 {
        if self.weather == Weather::None
            || self.weather_density.is_nan()
            || self.weather_density <= 0.0
        {
            0.0
        } else {
            self.weather_density.min(1.0)
        }
    }

    /// Whether the pass would change a single pixel (the shell skips it
    /// when not; shadows additionally need a sprite layer to do anything).
    #[must_use]
    pub fn any_active(&self) -> bool {
        self.bloom.level() > 0.0
            || self.tilt_shift.level() > 0.0
            || self.shadows.level() > 0.0
            || self.grade.level() > 0.0
            || self.weather_level() > 0.0
    }

    /// Whether the blurred intermediates are needed.
    #[must_use]
    pub fn needs_blur(&self, sprite_present: bool) -> bool {
        self.bloom.level() > 0.0
            || self.tilt_shift.level() > 0.0
            || (self.shadows.level() > 0.0 && sprite_present)
    }
}

/// A tint (per-channel multiplier) and an exposure multiplier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grade {
    pub tint: [f32; 3],
    pub exposure: f32,
}

/// The grade at the given time of day: a small fixed gradient -- night
/// blue and dim, a warm dawn, neutral noon, an orange dusk, night again.
/// Piecewise linear between keys; the argument wraps into `0.0..1.0`.
#[must_use]
pub fn grade_for(time_of_day: f32) -> Grade {
    // (time, tint, exposure). Plain artistic choices, not measured from any
    // title.
    const KEYS: [(f32, [f32; 3], f32); 9] = [
        (0.00, [0.45, 0.55, 1.00], 0.55),
        (0.20, [0.45, 0.55, 1.00], 0.55),
        (0.27, [1.10, 0.85, 0.75], 0.90),
        (0.36, [1.02, 0.98, 0.94], 1.00),
        (0.50, [1.00, 1.00, 1.00], 1.00),
        (0.64, [1.02, 0.98, 0.94], 1.00),
        (0.73, [1.15, 0.80, 0.60], 0.90),
        (0.82, [0.45, 0.55, 1.00], 0.55),
        (1.00, [0.45, 0.55, 1.00], 0.55),
    ];
    let t = if time_of_day.is_finite() {
        time_of_day.rem_euclid(1.0)
    } else {
        0.5
    };
    // KEYS[0].0 is 0.0 and t >= 0, so a window always exists; the last
    // window also covers t == 1.0 should rounding ever produce it.
    for pair in KEYS.windows(2) {
        let (t0, c0, e0) = pair[0];
        let (t1, c1, e1) = pair[1];
        if t <= t1 {
            let f = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
            let mix = |a: f32, b: f32| a + (b - a) * f;
            return Grade {
                tint: [mix(c0[0], c1[0]), mix(c0[1], c1[1]), mix(c0[2], c1[2])],
                exposure: mix(e0, e1),
            };
        }
    }
    Grade {
        tint: KEYS[KEYS.len() - 1].1,
        exposure: KEYS[KEYS.len() - 1].2,
    }
}

/// Down-sample factor and size of the small blur textures for a given
/// output size: `d` pixels per small texel, `d = 2 * round(height / 224)`
/// (at least 2), so the small set is about 128x112 whether the frame is 1x
/// or 4x and the blur reaches the same fraction of the picture.
#[must_use]
pub fn small_size(width: u32, height: u32) -> (u32, u32, u32) {
    let scale = ((height as f32 / 224.0).round() as u32).max(1);
    let d = scale * 2;
    (width.div_ceil(d).max(1), height.div_ceil(d).max(1), d)
}

/// Shadow offset in output pixels (down-right): a few pixels at 1x,
/// growing with the output.
#[must_use]
pub fn shadow_offset_px(height: u32) -> f32 {
    3.0 * (height as f32 / 224.0).max(1.0)
}

/// The sprite-only image the shadows are cast from.
#[derive(Debug, Clone, Copy)]
pub struct SpriteLayer<'a> {
    /// RGBA, only the alpha channel is read.
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// Inputs that depend on the sizes involved rather than on the settings.
struct Geometry {
    width: u32,
    height: u32,
    small: (u32, u32),
    footprint: u32,
    sprite_present: bool,
}

fn pack(v: [[f32; 4]; 7]) -> [u8; PARAMS_BYTES] {
    let mut bytes = [0u8; PARAMS_BYTES];
    for (i, f) in v.iter().flatten().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    bytes
}

/// The UBO contents for one draw. `blur_dir` is the axis of the blur
/// taps (`(1, 0)` or `(0, 1)`), ignored by the other draws.
fn params_bytes(params: &LensParams, g: &Geometry, blur_dir: (f32, f32)) -> [u8; PARAMS_BYTES] {
    let grade = grade_for(params.time_of_day);
    let offset = shadow_offset_px(g.height);
    let (sw, sh) = g.small;
    let small_inv = (1.0 / sw as f32, 1.0 / sh as f32);
    let shadow = if g.sprite_present {
        params.shadows.level()
    } else {
        0.0
    };
    pack([
        [
            params.bloom.level(),
            params.bloom_threshold.clamp(0.0, 0.99),
            params.tilt_shift.level(),
            params.tilt_center,
        ],
        [
            params.tilt_band.clamp(0.0, 1.0),
            shadow,
            offset / g.width as f32,
            offset / g.height as f32,
        ],
        [
            grade.tint[0],
            grade.tint[1],
            grade.tint[2],
            params.grade.level(),
        ],
        [
            grade.exposure,
            params.weather.code(),
            params.weather_level(),
            // Wrapped so the f32 stays exact and the sky repeats rather
            // than losing precision in a long session.
            (params.frame % 65_536) as f32,
        ],
        [
            1.0 / g.width as f32,
            1.0 / g.height as f32,
            g.width as f32 / g.height as f32,
            if shadow > 0.0 { 1.0 } else { 0.0 },
        ],
        [
            small_inv.0,
            small_inv.1,
            g.footprint as f32 / g.width as f32,
            g.footprint as f32 / g.height as f32,
        ],
        [blur_dir.0 * small_inv.0, blur_dir.1 * small_inv.1, 0.0, 0.0],
    ])
}

const TEX_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The lens pass's pipelines, bind-group layout, sampler and a 1x1
/// stand-in texture for unused inputs.
pub struct LensPass {
    bind_group_layout: wgpu::BindGroupLayout,
    down: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    fin: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
}

impl LensPass {
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("rf-renderer::lens::bind_group_layout"),
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
                        texture_entry(1),
                        texture_entry(2),
                        texture_entry(3),
                        texture_entry(4),
                        texture_entry(5),
                        wgpu::BindGroupLayoutEntry {
                            binding: 6,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("rf-renderer::lens::pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("rf-renderer::lens::shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(LENS_SHADER_SRC)),
            });
        let pipeline = |label: &str, entry: &str, targets: usize| {
            let target: Option<wgpu::ColorTargetState> = Some(TEX_FORMAT.into());
            let all = [target.clone(), target.clone(), target];
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
                        entry_point: Some(entry),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &all[..targets],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
        };
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rf-renderer::lens::sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy = upload_texture(gpu, "rf-renderer::lens::dummy", &[0, 0, 0, 0], 1, 1);
        LensPass {
            down: pipeline("rf-renderer::lens::down", "fs_down", 3),
            blur: pipeline("rf-renderer::lens::blur", "fs_blur", 3),
            fin: pipeline("rf-renderer::lens::final", "fs_final", 1),
            bind_group_layout,
            sampler,
            dummy,
        }
    }

    /// Run the pass over `scene_rgba` (`width` x `height`) and return the
    /// result, same size. `sprite` is the shadow source; without it (or
    /// when its byte length does not match its stated size) shadows are a
    /// no-op. Runs on the GPU even when nothing is active -- the caller
    /// decides whether to call ([`LensParams::any_active`]).
    ///
    /// # Errors
    /// Returns `Err` if the GPU readback does not complete within
    /// [`crate::gpu::GPU_WAIT`].
    ///
    /// # Panics
    /// If `scene_rgba` is not `width * height * 4` bytes.
    pub fn render(
        &self,
        gpu: &GpuContext,
        scene_rgba: &[u8],
        width: u32,
        height: u32,
        sprite: Option<SpriteLayer<'_>>,
        params: &LensParams,
    ) -> Result<Vec<u8>, String> {
        assert_eq!(
            scene_rgba.len(),
            (width as usize) * (height as usize) * 4,
            "LensPass::render: scene buffer size does not match {width}x{height}"
        );
        let sprite = sprite.filter(|s| {
            s.width > 0
                && s.height > 0
                && s.rgba.len() == (s.width as usize) * (s.height as usize) * 4
        });
        let (sw, sh, footprint) = small_size(width, height);
        let geometry = Geometry {
            width,
            height,
            small: (sw, sh),
            footprint,
            sprite_present: sprite.is_some(),
        };
        let blur = params.needs_blur(sprite.is_some());

        let scene_view = upload_texture(gpu, "rf-renderer::lens::scene", scene_rgba, width, height);
        let sprite_view = sprite
            .map(|s| upload_texture(gpu, "rf-renderer::lens::sprite", s.rgba, s.width, s.height));
        let sprite_in = sprite_view.as_ref().unwrap_or(&self.dummy);

        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let output_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::lens::output"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TEX_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Two sets of three small targets, ping-ponged by the blur; only
        // allocated when a blur is needed.
        let small_set = |label: &str| -> Option<[wgpu::TextureView; 3]> {
            blur.then(|| {
                std::array::from_fn(|_| {
                    gpu.device
                        .create_texture(&wgpu::TextureDescriptor {
                            label: Some(label),
                            size: wgpu::Extent3d {
                                width: sw,
                                height: sh,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: TEX_FORMAT,
                            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                                | wgpu::TextureUsages::TEXTURE_BINDING,
                            view_formats: &[],
                        })
                        .create_view(&wgpu::TextureViewDescriptor::default())
                })
            })
        };
        let set0 = small_set("rf-renderer::lens::small0");
        let set1 = small_set("rf-renderer::lens::small1");

        let ubo = |dir: (f32, f32)| {
            let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rf-renderer::lens::params"),
                size: PARAMS_BYTES as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            gpu.queue
                .write_buffer(&buffer, 0, &params_bytes(params, &geometry, dir));
            buffer
        };
        let bind = |buffer: &wgpu::Buffer, tex: [&wgpu::TextureView; 5]| {
            let mut entries = vec![wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }];
            for (i, view) in tex.into_iter().enumerate() {
                entries.push(wgpu::BindGroupEntry {
                    binding: i as u32 + 1,
                    resource: wgpu::BindingResource::TextureView(view),
                });
            }
            entries.push(wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            });
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rf-renderer::lens::bind_group"),
                layout: &self.bind_group_layout,
                entries: &entries,
            })
        };
        let draw = |encoder: &mut wgpu::CommandEncoder,
                    pipeline: &wgpu::RenderPipeline,
                    group: &wgpu::BindGroup,
                    targets: &[&wgpu::TextureView]| {
            let attachments: Vec<Option<wgpu::RenderPassColorAttachment<'_>>> = targets
                .iter()
                .map(|view| {
                    Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })
                })
                .collect();
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::lens::render_pass"),
                color_attachments: &attachments,
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rpass.set_pipeline(pipeline);
            rpass.set_bind_group(0, group, &[]);
            rpass.draw(0..3, 0..1);
        };

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rf-renderer::lens::encoder"),
            });
        // Keep the buffers alive until submit.
        let mut buffers = Vec::new();
        let final_inputs: [&wgpu::TextureView; 3] = if let (Some(s0), Some(s1)) = (&set0, &set1) {
            let b_down = ubo((0.0, 0.0));
            let g = bind(
                &b_down,
                [
                    &scene_view,
                    sprite_in,
                    &self.dummy,
                    &self.dummy,
                    &self.dummy,
                ],
            );
            draw(&mut encoder, &self.down, &g, &[&s0[0], &s0[1], &s0[2]]);
            let b_h = ubo((1.0, 0.0));
            let g = bind(&b_h, [&s0[0], &s0[1], &s0[2], &self.dummy, &self.dummy]);
            draw(&mut encoder, &self.blur, &g, &[&s1[0], &s1[1], &s1[2]]);
            let b_v = ubo((0.0, 1.0));
            let g = bind(&b_v, [&s1[0], &s1[1], &s1[2], &self.dummy, &self.dummy]);
            draw(&mut encoder, &self.blur, &g, &[&s0[0], &s0[1], &s0[2]]);
            buffers.extend([b_down, b_h, b_v]);
            [&s0[0], &s0[1], &s0[2]]
        } else {
            [&self.dummy, &self.dummy, &self.dummy]
        };
        let b_final = ubo((0.0, 0.0));
        let g = bind(
            &b_final,
            [
                &scene_view,
                sprite_in,
                final_inputs[0],
                final_inputs[1],
                final_inputs[2],
            ],
        );
        draw(&mut encoder, &self.fin, &g, &[&output_view]);
        buffers.push(b_final);

        let bytes_per_row = width * 4;
        let padded_bytes_per_row = align_up(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::lens::readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(height),
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
        drop(buffers);

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

/// Upload RGBA8 bytes as a sampled texture and return its view.
fn upload_texture(
    gpu: &GpuContext,
    label: &str,
    bytes: &[u8],
    width: u32,
    height: u32,
) -> wgpu::TextureView {
    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TEX_FORMAT,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> Geometry {
        Geometry {
            width: 256,
            height: 224,
            small: (128, 112),
            footprint: 2,
            sprite_present: true,
        }
    }

    fn floats(bytes: &[u8; PARAMS_BYTES]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    #[test]
    fn an_effect_is_off_until_enabled_and_clamped_when_on() {
        assert_eq!(Effect::OFF.level(), 0.0);
        assert_eq!(
            Effect {
                enabled: false,
                strength: 0.9
            }
            .level(),
            0.0,
            "a strength without the flag does nothing"
        );
        assert_eq!(Effect::on(0.4).level(), 0.4);
        assert_eq!(Effect::on(7.0).level(), 1.0);
        assert_eq!(Effect::on(-1.0).level(), 0.0);
        assert_eq!(Effect::on(f32::NAN).level(), 0.0);
    }

    #[test]
    fn nothing_is_active_by_default_and_each_effect_alone_activates_the_pass() {
        let base = LensParams::default();
        assert!(!base.any_active());
        let cases = [
            LensParams {
                bloom: Effect::on(0.5),
                ..base
            },
            LensParams {
                tilt_shift: Effect::on(0.5),
                ..base
            },
            LensParams {
                shadows: Effect::on(0.5),
                ..base
            },
            LensParams {
                grade: Effect::on(0.5),
                ..base
            },
            LensParams {
                weather: Weather::Rain,
                weather_density: 0.5,
                ..base
            },
        ];
        for c in cases {
            assert!(c.any_active(), "{c:?}");
        }
        // A sky with no density, or a density with no sky, draws nothing.
        assert!(!LensParams {
            weather: Weather::Snow,
            weather_density: 0.0,
            ..base
        }
        .any_active());
        assert!(!LensParams {
            weather: Weather::None,
            weather_density: 1.0,
            ..base
        }
        .any_active());
    }

    #[test]
    fn only_the_blurring_effects_ask_for_the_blur_passes() {
        let base = LensParams::default();
        let grade_weather = LensParams {
            grade: Effect::on(1.0),
            weather: Weather::Rain,
            weather_density: 1.0,
            ..base
        };
        assert!(!grade_weather.needs_blur(true));
        let shadows = LensParams {
            shadows: Effect::on(1.0),
            ..base
        };
        assert!(shadows.needs_blur(true));
        assert!(!shadows.needs_blur(false), "no sprite layer, no blur");
        assert!(LensParams {
            bloom: Effect::on(0.1),
            ..base
        }
        .needs_blur(false));
    }

    #[test]
    fn the_grade_is_neutral_at_noon_dim_and_blue_at_night_and_warm_at_dusk() {
        let noon = grade_for(0.5);
        assert_eq!(noon.tint, [1.0, 1.0, 1.0]);
        assert_eq!(noon.exposure, 1.0);
        let night = grade_for(0.0);
        assert!(night.exposure < 0.7, "night is dimmer: {night:?}");
        assert!(night.tint[2] > night.tint[0], "night is blue: {night:?}");
        let dusk = grade_for(0.73);
        assert!(dusk.tint[0] > dusk.tint[2], "dusk is orange: {dusk:?}");
        let dawn = grade_for(0.27);
        assert!(dawn.tint[0] > dawn.tint[2], "dawn is warm: {dawn:?}");
        // 0 and 1 are the same midnight; out-of-range wraps; NaN is noon.
        assert_eq!(grade_for(1.0), grade_for(0.0));
        assert_eq!(grade_for(1.5), grade_for(0.5));
        assert_eq!(grade_for(f32::NAN), noon);
    }

    #[test]
    fn the_grade_is_continuous_between_its_keys() {
        let mut prev = grade_for(0.0);
        for i in 1..=1000 {
            let g = grade_for(i as f32 / 1000.0);
            assert!(
                (g.exposure - prev.exposure).abs() < 0.02,
                "exposure jumps near {}",
                i
            );
            for c in 0..3 {
                assert!(
                    (g.tint[c] - prev.tint[c]).abs() < 0.03,
                    "tint jumps near {i}"
                );
            }
            prev = g;
        }
    }

    #[test]
    fn the_small_texture_set_is_about_the_same_size_at_every_scale() {
        assert_eq!(small_size(256, 224), (128, 112, 2));
        assert_eq!(small_size(1024, 896), (128, 112, 8));
        assert_eq!(small_size(64, 32), (32, 16, 2));
        let (w, h, d) = small_size(1, 1);
        assert!(w >= 1 && h >= 1 && d >= 2);
    }

    #[test]
    fn the_shadow_offset_grows_with_the_output() {
        assert_eq!(shadow_offset_px(224), 3.0);
        assert_eq!(shadow_offset_px(100), 3.0, "never less than 3 px");
        assert_eq!(shadow_offset_px(896), 12.0);
    }

    #[test]
    fn the_parameter_block_packs_every_field_at_its_documented_offset() {
        let params = LensParams {
            bloom: Effect::on(0.25),
            bloom_threshold: 0.7,
            tilt_shift: Effect::on(0.5),
            tilt_center: 0.4,
            tilt_band: 0.2,
            shadows: Effect::on(0.75),
            grade: Effect::on(0.6),
            time_of_day: 0.5,
            weather: Weather::Snow,
            weather_density: 0.3,
            frame: 65_536 + 12,
        };
        let f = floats(&params_bytes(&params, &geometry(), (1.0, 0.0)));
        assert_eq!(f.len(), 28);
        assert_eq!(&f[0..4], &[0.25, 0.7, 0.5, 0.4]);
        assert_eq!(f[4], 0.2, "band");
        assert_eq!(f[5], 0.75, "shadow strength");
        assert_eq!(f[6], 3.0 / 256.0, "shadow offset x in UV");
        assert_eq!(f[7], 3.0 / 224.0, "shadow offset y in UV");
        assert_eq!(&f[8..11], &[1.0, 1.0, 1.0], "noon tint");
        assert_eq!(f[11], 0.6, "grade strength");
        assert_eq!(f[12], 1.0, "noon exposure");
        assert_eq!(f[13], 2.0, "snow");
        assert_eq!(f[14], 0.3, "density");
        assert_eq!(f[15], 12.0, "frame wraps at 65536");
        assert_eq!(f[16], 1.0 / 256.0);
        assert_eq!(f[17], 1.0 / 224.0);
        assert_eq!(f[18], 256.0 / 224.0, "aspect");
        assert_eq!(f[19], 1.0, "sprite present");
        assert_eq!(f[20], 1.0 / 128.0);
        assert_eq!(f[21], 1.0 / 112.0);
        assert_eq!(f[22], 2.0 / 256.0);
        assert_eq!(f[23], 2.0 / 224.0);
        assert_eq!(f[24], 1.0 / 128.0, "horizontal blur step");
        assert_eq!(f[25], 0.0);
        let v = floats(&params_bytes(&params, &geometry(), (0.0, 1.0)));
        assert_eq!((v[24], v[25]), (0.0, 1.0 / 112.0), "vertical blur step");
    }

    #[test]
    fn disabled_effects_pack_as_zero_whatever_their_stored_strength() {
        let params = LensParams {
            bloom: Effect {
                enabled: false,
                strength: 1.0,
            },
            tilt_shift: Effect {
                enabled: false,
                strength: 1.0,
            },
            shadows: Effect {
                enabled: false,
                strength: 1.0,
            },
            grade: Effect {
                enabled: false,
                strength: 1.0,
            },
            weather: Weather::Rain,
            weather_density: 0.0,
            ..LensParams::default()
        };
        let f = floats(&params_bytes(&params, &geometry(), (0.0, 0.0)));
        assert_eq!((f[0], f[2], f[5], f[11], f[14]), (0.0, 0.0, 0.0, 0.0, 0.0));
        assert_eq!(f[19], 0.0, "no shadows, no sprite sampling");
    }

    #[test]
    fn without_a_sprite_layer_the_shadow_strength_packs_as_zero() {
        let mut g = geometry();
        g.sprite_present = false;
        let params = LensParams {
            shadows: Effect::on(1.0),
            ..LensParams::default()
        };
        let f = floats(&params_bytes(&params, &g, (0.0, 0.0)));
        assert_eq!((f[5], f[19]), (0.0, 0.0));
    }

    #[test]
    fn weather_keys_round_trip_and_unknown_keys_are_refused() {
        for w in Weather::ALL {
            assert_eq!(Weather::from_key(w.key()), Some(w));
            assert!(!w.label().is_empty());
        }
        assert_eq!(Weather::from_key("hail"), None);
        assert_eq!(Weather::default(), Weather::None);
    }

    #[test]
    fn manifest_names_the_effect_and_states_authorship() {
        let m = manifest();
        assert_eq!(m.display_name, "HD-2D lens and lighting");
        assert!(!m.authorship.is_empty());
        assert_eq!(m.basis, None);
    }
}
