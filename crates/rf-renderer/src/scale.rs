//! Original pipeline, scale pass (ticket W3-01b; `docs/design/RENDERER.md`
//! §2/§7): overscan crop + integer/PAR scale over the 1x post-palette
//! RGBA8 buffer, nearest-neighbor. Unlike
//! [`crate::original_pipeline::PalettePass`], this pass's GPU output is
//! **not** golden-hashable in CI (RENDERER.md §7: shader/driver output
//! differs past the 1x buffer) — [`render_scaled_reference`] below is the
//! CPU oracle `rf-harness`'s reference-image-with-tolerance test
//! (`crates/rf-harness/tests/scale_pass_tolerance.rs`) checks
//! [`ScalePass`]'s GPU output against instead of a frozen hash, the same
//! "independent oracle, not a codified GPU output" shape as
//! [`crate::palette::palette_index_to_rgb`] pairs with the palette pass's
//! golden hash.
//!
//! ## Design tension resolved (RENDERER.md §2 amendment, this ticket)
//!
//! §2 requires both "8:7 PAR" and "integer scaling default-on"; 8:7 is not
//! an integer ratio, so one combined integer scale factor cannot satisfy
//! both literally. Resolution **(b)** from the ticket brief: the
//! **vertical** axis is the integer-locked one ([`FillMode::IntegerLocked`]'s
//! `y_scale` is always a whole number — this is also the axis
//! [`Overscan`] crops, so keeping it integer keeps cropped scanlines
//! landing on exact output rows with no seam), and the **horizontal** axis
//! carries the non-integer PAR correction directly
//! (`x_scale = y_scale * par.num / par.den`), computed once, not rounded
//! away. Chosen over resolution (a) — integer-scale both axes, then a
//! second horizontal-only stretch pass — because (a) and (b) are the same
//! final geometry either way (`out_width` is
//! `source_width * y_scale * par.num / par.den` under both), so (a)'s
//! second pass would be pure overhead for an identical result; one pass,
//! one shader, one set of per-pixel rounding rules is strictly simpler to
//! reason about and to keep in lockstep with the CPU oracle below.
//!
//! ## Nearest-neighbor without a sampler
//!
//! `shaders/scale.wgsl` uses the same technique as `palette.wgsl`:
//! `@builtin(position)` (spec-guaranteed pixel-center, not driver-defined)
//! plus `textureLoad` (exact integer texel address, no filtering, no
//! `sampler` binding at all) instead of `textureSample`. This means a
//! *correct* GPU run computes the identical `floor(out_coord * inv_scale)`
//! formula, at the identical `f32` precision, as [`render_scaled_reference`]
//! — the tolerance harness exists for the residual gap (WGSL float
//! division/floor is not literally guaranteed bit-for-bit portable across
//! every driver the way Rust's is), not because the two algorithms differ.

use crate::gpu::{align_up, read_buffer_sync, GpuContext};
use std::borrow::Cow;

const SCALE_SHADER_SRC: &str = include_str!("shaders/scale.wgsl");

/// A pixel-aspect ratio as an exact fraction — a plain data type (not an
/// enum) so a caller can supply any ratio as the "user override"
/// `docs/design/RENDERER.md` §2 requires, not just the two named here.
/// Exposed as a constructor parameter to [`ScaleGeometry::compute`]; wiring
/// an actual settings-UI control is `crates/retroforge` (app shell) scope,
/// outside this ticket's `write_scope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParRatio {
    pub num: u32,
    pub den: u32,
}

impl ParRatio {
    /// NES/SNES NTSC default (`docs/design/RENDERER.md` §2).
    pub const NES_SNES_NTSC_8_7: ParRatio = ParRatio { num: 8, den: 7 };
    /// Square pixels — also this ticket's "wrong aspect" calibration
    /// mutation (`crates/rf-harness/tests/scale_pass_tolerance.rs`):
    /// swapping this in for [`Self::NES_SNES_NTSC_8_7`] changes
    /// [`ScaleGeometry::out_width`], which is what makes a square-pixel
    /// scale pass fail the harness's strict geometry check.
    pub const SQUARE: ParRatio = ParRatio { num: 1, den: 1 };
}

impl Default for ParRatio {
    /// "8:7 PAR for NES/SNES NTSC" (`docs/design/RENDERER.md` §2) as an
    /// actual code default, not only a doc sentence — a caller that
    /// doesn't override PAR gets [`Self::NES_SNES_NTSC_8_7`].
    fn default() -> Self {
        ParRatio::NES_SNES_NTSC_8_7
    }
}

/// `docs/design/RENDERER.md` §2: "overscan crop (default 224-line NTSC
/// view, full 240 optional)". Crop is vertical-only — NES/SNES have no
/// documented horizontal-overscan convention, and `docs/design/
/// RENDERER.md` §2 only ever names a *line* count — symmetric 8 lines
/// trimmed off top and bottom for the 224-line default
/// (240 - 224 = 16, split evenly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overscan {
    /// Default: the 224-line NTSC-safe view.
    Crop224,
    /// User option: the full 240-line frame, including overscan.
    Full240,
}

impl Overscan {
    /// `(crop_top, crop_height)` for a `source_height`-tall input.
    ///
    /// # Panics
    /// Panics if `self` is [`Overscan::Crop224`] and `source_height < 224`
    /// — a caller handing this pass a frame shorter than the crop it asked
    /// for is a caller bug (wrong console resolution wired in), not a
    /// runtime condition to degrade through.
    #[must_use]
    pub fn resolve(self, source_height: u32) -> (u32, u32) {
        match self {
            Overscan::Crop224 => {
                assert!(
                    source_height >= 224,
                    "Overscan::Crop224 needs a source at least 224 lines tall, got {source_height}"
                );
                let trim = source_height - 224;
                (trim / 2, 224)
            }
            Overscan::Full240 => (0, source_height),
        }
    }
}

impl Default for Overscan {
    /// "overscan crop (default 224-line NTSC view, full 240 optional)"
    /// (`docs/design/RENDERER.md` §2) as an actual code default, not only a
    /// doc sentence.
    fn default() -> Self {
        Overscan::Crop224
    }
}

/// `docs/design/RENDERER.md` §2: "integer scaling default-on with
/// fractional fill as a user option".
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillMode {
    /// Default: `y_scale` is exactly `integer_factor` (a whole number);
    /// `x_scale` carries the PAR correction (module doc's resolution (b)).
    /// Output size follows directly from the source/crop/PAR and never
    /// targets a particular viewport — it may letterbox.
    IntegerLocked { integer_factor: u32 },
    /// User option: stretch to exactly fill `viewport_w`x`viewport_h`,
    /// independently per axis. Both scale factors become non-integer in
    /// general, and — deliberately, this is the tradeoff of choosing this
    /// mode over the default — the PAR ratio passed to
    /// [`ScaleGeometry::compute`] is not separately enforced here: fitting
    /// an arbitrary viewport exactly and preserving a fixed PAR are only
    /// simultaneously possible for viewports that already have that AR.
    FractionalFill { viewport_w: u32, viewport_h: u32 },
}

impl Default for FillMode {
    /// "integer scaling default-on" (`docs/design/RENDERER.md` §2) as an
    /// actual code default: 1x is the only factor this type can pick
    /// without a caller-supplied viewport/target size to size a larger
    /// factor against — [`ScaleGeometry::nes_ntsc_default`] is the
    /// caller-facing shortcut that pairs this with a real
    /// `integer_factor`.
    fn default() -> Self {
        FillMode::IntegerLocked { integer_factor: 1 }
    }
}

/// Resolved scale geometry — pure data, no GPU handle. Computed once by
/// [`ScaleGeometry::compute`] and consumed by both [`ScalePass::render`]
/// and [`render_scaled_reference`], so the GPU pass and the CPU oracle are
/// mechanically guaranteed to target the same output size rather than each
/// independently recomputing it and risking drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleGeometry {
    pub crop_top: u32,
    pub crop_height: u32,
    pub x_scale: f32,
    pub y_scale: f32,
    pub out_width: u32,
    pub out_height: u32,
}

impl ScaleGeometry {
    /// # Panics
    /// Panics if `overscan` needs more lines than `source_height` has
    /// ([`Overscan::resolve`]'s own panic), or if `fill` is
    /// [`FillMode::IntegerLocked`] with `integer_factor == 0` — a zero
    /// scale factor is a caller bug, not a degradable runtime condition.
    #[must_use]
    pub fn compute(
        source_width: u32,
        source_height: u32,
        overscan: Overscan,
        par: ParRatio,
        fill: FillMode,
    ) -> ScaleGeometry {
        let (crop_top, crop_height) = overscan.resolve(source_height);
        match fill {
            FillMode::IntegerLocked { integer_factor } => {
                assert!(
                    integer_factor > 0,
                    "ScaleGeometry::compute: integer_factor must be nonzero"
                );
                let y_scale = integer_factor as f32;
                #[allow(clippy::cast_precision_loss)]
                let x_scale = y_scale * (par.num as f32 / par.den as f32);
                let out_width = (source_width as f32 * x_scale).round() as u32;
                let out_height = crop_height * integer_factor;
                ScaleGeometry {
                    crop_top,
                    crop_height,
                    x_scale,
                    y_scale,
                    out_width,
                    out_height,
                }
            }
            FillMode::FractionalFill {
                viewport_w,
                viewport_h,
            } => {
                assert!(
                    viewport_w > 0 && viewport_h > 0,
                    "ScaleGeometry::compute: FractionalFill viewport must be nonzero on both \
                     axes, got {viewport_w}x{viewport_h}"
                );
                #[allow(clippy::cast_precision_loss)]
                let x_scale = viewport_w as f32 / source_width as f32;
                #[allow(clippy::cast_precision_loss)]
                let y_scale = viewport_h as f32 / crop_height as f32;
                ScaleGeometry {
                    crop_top,
                    crop_height,
                    x_scale,
                    y_scale,
                    out_width: viewport_w,
                    out_height: viewport_h,
                }
            }
        }
    }

    /// `docs/design/RENDERER.md` §2's actual defaults, as one call:
    /// [`Overscan::default`] (`Crop224`), [`ParRatio::default`] (8:7 NTSC),
    /// and [`FillMode::IntegerLocked`] at the given `integer_factor` —
    /// "integer scaling default-on" needs a factor from somewhere (a
    /// viewport/zoom-level decision this crate doesn't own), so this takes
    /// it as a parameter rather than also defaulting it to a fixed `1`,
    /// unlike [`FillMode::default`] which has no other choice.
    ///
    /// # Panics
    /// Same as [`Self::compute`] with those three settings.
    #[must_use]
    pub fn nes_ntsc_default(
        source_width: u32,
        source_height: u32,
        integer_factor: u32,
    ) -> ScaleGeometry {
        ScaleGeometry::compute(
            source_width,
            source_height,
            Overscan::default(),
            ParRatio::default(),
            FillMode::IntegerLocked { integer_factor },
        )
    }
}

/// The CPU oracle (module doc): for each output pixel, `floor(out_coord *
/// inv_scale)` in `f32` — identical formula, identical precision, to
/// `shaders/scale.wgsl`'s fragment stage — clamped into the cropped source
/// region, then a direct RGBA copy (nearest-neighbor: no blending, no
/// interpolation).
///
/// # Panics
/// Panics if `source_rgba.len() != source_width * source_height * 4` —
/// same "malformed input is a caller bug" stance as
/// `original_pipeline::IndexedFrame::new`.
#[must_use]
pub fn render_scaled_reference(
    source_rgba: &[u8],
    source_width: u32,
    source_height: u32,
    geometry: &ScaleGeometry,
) -> Vec<u8> {
    assert_eq!(
        source_rgba.len(),
        (source_width as usize) * (source_height as usize) * 4,
        "render_scaled_reference: source buffer size does not match {source_width}x{source_height}"
    );
    let inv_x = 1.0f32 / geometry.x_scale;
    let inv_y = 1.0f32 / geometry.y_scale;
    let mut out = vec![0u8; (geometry.out_width as usize) * (geometry.out_height as usize) * 4];
    let max_src_x = source_width.saturating_sub(1);
    let max_src_y = (geometry.crop_top + geometry.crop_height).saturating_sub(1);
    for oy in 0..geometry.out_height {
        #[allow(clippy::cast_precision_loss)]
        let src_y_f = (oy as f32 * inv_y).floor() as i64 + i64::from(geometry.crop_top);
        let src_y = src_y_f.clamp(i64::from(geometry.crop_top), i64::from(max_src_y)) as u32;
        let src_row = src_y * source_width;
        let dst_row = oy * geometry.out_width;
        for ox in 0..geometry.out_width {
            #[allow(clippy::cast_precision_loss)]
            let src_x_f = (ox as f32 * inv_x).floor() as i64;
            let src_x = src_x_f.clamp(0, i64::from(max_src_x)) as u32;
            let src_idx = ((src_row + src_x) * 4) as usize;
            let dst_idx = ((dst_row + ox) * 4) as usize;
            out[dst_idx..dst_idx + 4].copy_from_slice(&source_rgba[src_idx..src_idx + 4]);
        }
    }
    out
}

/// Little-endian bytes for `scale.wgsl`'s `Params { v: vec4<f32> }`
/// uniform — `(1/x_scale, 1/y_scale, crop_top, 0.0)`, same "pack host-side,
/// write once" approach as `original_pipeline::lut_bytes`/
/// `composite::rect_bytes`.
fn params_bytes(geometry: &ScaleGeometry) -> [u8; 16] {
    #[allow(clippy::cast_precision_loss)]
    let values = [
        1.0f32 / geometry.x_scale,
        1.0f32 / geometry.y_scale,
        geometry.crop_top as f32,
        0.0f32,
    ];
    let mut bytes = [0u8; 16];
    for (i, v) in values.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// The GPU original-pipeline scale pass: source RGBA8 bytes in (the 1x
/// post-palette buffer — `original_pipeline::PalettePass::render`'s output
/// shape, though this type has no dependency on that one: it takes plain
/// bytes, same "shell/caller supplies plain RGBA, this crate never assumes
/// where they came from" shape as [`crate::composite::CompositeLayer`]),
/// scaled RGBA8 bytes out. Pipeline/bind-group-layout are built once in
/// [`Self::new`] and reused across [`Self::render`] calls, same "build
/// once, reuse per frame" shape as
/// [`crate::original_pipeline::PalettePass`]/[`crate::composite::EnhancedCompositor`].
pub struct ScalePass {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl ScalePass {
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("rf-renderer::scale_pass::shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SCALE_SHADER_SRC)),
            });

        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("rf-renderer::scale_pass::bind_group_layout"),
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
                                // textureLoad only, never textureSample (module
                                // doc) -- no sampler binding needed at all,
                                // same "exact texel address" shape as
                                // `original_pipeline`'s R8Uint indexed_tex.
                                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                    ],
                });

        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("rf-renderer::scale_pass::pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("rf-renderer::scale_pass::pipeline"),
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

        ScalePass {
            pipeline,
            bind_group_layout,
        }
    }

    /// Render `source_rgba` (`source_width`x`source_height`, RGBA8) through
    /// the scale pass per `geometry` and read back the resulting RGBA8
    /// bytes (`geometry.out_width * geometry.out_height * 4`).
    ///
    /// # Errors
    /// Returns `Err` if the GPU readback does not complete within
    /// [`crate::gpu::GPU_WAIT`] — reported, never retried in a loop (see
    /// `crate::gpu`'s module doc).
    ///
    /// # Panics
    /// Panics if `source_rgba.len() != source_width * source_height * 4` —
    /// same "malformed input is a caller bug" stance as
    /// [`render_scaled_reference`].
    pub fn render(
        &self,
        gpu: &GpuContext,
        source_rgba: &[u8],
        source_width: u32,
        source_height: u32,
        geometry: &ScaleGeometry,
    ) -> Result<Vec<u8>, String> {
        assert_eq!(
            source_rgba.len(),
            (source_width as usize) * (source_height as usize) * 4,
            "ScalePass::render: source buffer size does not match {source_width}x{source_height}"
        );

        let source_extent = wgpu::Extent3d {
            width: source_width,
            height: source_height,
            depth_or_array_layers: 1,
        };
        let source_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::scale_pass::source"),
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
            // Queue::write_texture has no 256-byte row-alignment
            // requirement (unlike CommandEncoder::copy_*_to_*) -- same fact
            // `original_pipeline`/`composite` already rely on for their own
            // uploads.
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(source_width * 4),
                rows_per_image: Some(source_height),
            },
            source_extent,
        );
        let source_view = source_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let output_extent = wgpu::Extent3d {
            width: geometry.out_width,
            height: geometry.out_height,
            depth_or_array_layers: 1,
        };
        let output_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::scale_pass::output"),
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
            label: Some("rf-renderer::scale_pass::params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue
            .write_buffer(&params_buffer, 0, &params_bytes(geometry));

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rf-renderer::scale_pass::bind_group"),
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
            ],
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rf-renderer::scale_pass::encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::scale_pass::render_pass"),
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

        // Readback row padding: unlike `original_pipeline` (fixed 256x240,
        // 1024 bytes/row already 256-aligned), an arbitrary PAR-scaled
        // width has no such guarantee -- same padding dance as
        // `crate::composite::EnhancedCompositor::composite`.
        let bytes_per_row = geometry.out_width * 4;
        let padded_bytes_per_row = align_up(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback_size = u64::from(padded_bytes_per_row) * u64::from(geometry.out_height);
        let readback_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::scale_pass::readback"),
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
                    rows_per_image: Some(geometry.out_height),
                },
            },
            output_extent,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));

        let padded = read_buffer_sync(&gpu.device, &readback_buffer)?;
        let mut rgba =
            Vec::with_capacity((bytes_per_row as usize) * (geometry.out_height as usize));
        for row in padded
            .chunks(padded_bytes_per_row as usize)
            .take(geometry.out_height as usize)
        {
            rgba.extend_from_slice(&row[..bytes_per_row as usize]);
        }
        Ok(rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Pure geometry: no GPU needed, and this is the "assert the chosen
    // geometry rather than whatever the code happens to produce" test the
    // ticket brief calls for -- hardcoded expected numbers, not derived
    // from the implementation under test. --------------------------------

    #[test]
    fn default_nes_geometry_at_1x_matches_hand_computed_numbers() {
        // 256x240 source, Crop224 (crop_top=8, crop_height=224), 8:7 PAR,
        // integer_factor=1: y_scale=1, x_scale=8/7, out_height=224,
        // out_width=round(256*8/7)=round(292.571...)=293.
        let g = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::IntegerLocked { integer_factor: 1 },
        );
        assert_eq!(g.crop_top, 8);
        assert_eq!(g.crop_height, 224);
        assert_eq!(g.y_scale, 1.0);
        assert!((g.x_scale - 8.0 / 7.0).abs() < 1e-6);
        assert_eq!(g.out_height, 224);
        assert_eq!(g.out_width, 293);
    }

    #[test]
    fn defaults_actually_default_to_crop224_and_8_7_par() {
        // Overscan::default()/ParRatio::default() must produce the exact
        // same geometry as spelling out Crop224 + NES_SNES_NTSC_8_7 by
        // hand -- proves the `Default` impls are wired to the right
        // variants, not merely present.
        let explicit = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::IntegerLocked { integer_factor: 1 },
        );
        let defaulted = ScaleGeometry::compute(
            256,
            240,
            Overscan::default(),
            ParRatio::default(),
            FillMode::default(),
        );
        assert_eq!(explicit, defaulted);
        let via_shortcut = ScaleGeometry::nes_ntsc_default(256, 240, 1);
        assert_eq!(explicit, via_shortcut);
    }

    #[test]
    fn default_nes_geometry_at_3x_matches_hand_computed_numbers() {
        // y_scale=3, x_scale=24/7, out_height=224*3=672,
        // out_width=round(256*24/7)=round(877.714...)=878.
        let g = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::IntegerLocked { integer_factor: 3 },
        );
        assert_eq!(g.out_height, 672);
        assert_eq!(g.out_width, 878);
    }

    #[test]
    fn full_240_overscan_keeps_all_lines() {
        let g = ScaleGeometry::compute(
            256,
            240,
            Overscan::Full240,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::IntegerLocked { integer_factor: 1 },
        );
        assert_eq!(g.crop_top, 0);
        assert_eq!(g.crop_height, 240);
        assert_eq!(g.out_height, 240);
    }

    #[test]
    fn square_par_override_produces_a_narrower_output_than_8_7() {
        let par = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::IntegerLocked { integer_factor: 1 },
        );
        let square = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::SQUARE,
            FillMode::IntegerLocked { integer_factor: 1 },
        );
        assert_eq!(square.out_width, 256);
        assert_ne!(square.out_width, par.out_width);
        assert_eq!(square.out_height, par.out_height);
    }

    #[test]
    fn fractional_fill_matches_the_requested_viewport_exactly() {
        let g = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::FractionalFill {
                viewport_w: 1000,
                viewport_h: 500,
            },
        );
        assert_eq!(g.out_width, 1000);
        assert_eq!(g.out_height, 500);
    }

    #[test]
    #[should_panic(expected = "at least 224 lines")]
    fn crop224_panics_on_a_too_short_source() {
        let _ = Overscan::Crop224.resolve(200);
    }

    #[test]
    #[should_panic(expected = "integer_factor must be nonzero")]
    fn zero_integer_factor_panics() {
        let _ = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::IntegerLocked { integer_factor: 0 },
        );
    }

    #[test]
    #[should_panic(expected = "FractionalFill viewport must be nonzero")]
    fn zero_fractional_fill_viewport_panics_rather_than_reaching_the_gpu_with_a_0x0_target() {
        let _ = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::FractionalFill {
                viewport_w: 0,
                viewport_h: 100,
            },
        );
    }

    // --- CPU oracle: correctness of the nearest-neighbor mapping itself,
    // independent of any GPU. -------------------------------------------

    fn solid_source(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w as usize) * (h as usize) * 4);
        for _ in 0..(w * h) {
            v.extend_from_slice(&color);
        }
        v
    }

    #[test]
    fn reference_oracle_output_matches_declared_geometry_size() {
        let g = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::IntegerLocked { integer_factor: 1 },
        );
        let source = solid_source(256, 240, [10, 20, 30, 255]);
        let out = render_scaled_reference(&source, 256, 240, &g);
        assert_eq!(
            out.len(),
            (g.out_width as usize) * (g.out_height as usize) * 4
        );
    }

    #[test]
    fn reference_oracle_crops_overscan_lines_out() {
        // A source whose top 8 and bottom 8 rows are a distinct "sentinel"
        // color from the rest -- Crop224 must never let that sentinel
        // color appear anywhere in the output.
        let w = 4u32;
        let h = 240u32;
        let mut source = solid_source(w, h, [1, 1, 1, 255]);
        for y in [0u32, 1, 238, 239] {
            let row_start = (y * w * 4) as usize;
            for px in 0..w as usize {
                source[row_start + px * 4..row_start + px * 4 + 4]
                    .copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let g = ScaleGeometry::compute(
            w,
            h,
            Overscan::Crop224,
            ParRatio::SQUARE, // square PAR keeps this test's math simple
            FillMode::IntegerLocked { integer_factor: 1 },
        );
        let out = render_scaled_reference(&source, w, h, &g);
        for chunk in out.chunks_exact(4) {
            assert_ne!(
                chunk,
                [255, 0, 0, 255],
                "overscan sentinel rows leaked into the Crop224 output"
            );
        }
    }

    #[test]
    fn reference_oracle_integer_scale_replicates_each_source_pixel_into_an_nxn_block() {
        // 2x2 checkerboard source, integer_factor=4, square PAR: each
        // source pixel must become an exact 4x4 block of its own color in
        // the output, proving the mapping is genuinely nearest-neighbor
        // block replication, not e.g. an averaged/blended resize.
        let w = 2u32;
        let h = 224u32; // already exactly 224 so Crop224 is a no-op here
        let mut source = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let color = if x == 0 {
                    [255, 0, 0, 255]
                } else {
                    [0, 255, 0, 255]
                };
                let idx = ((y * w + x) * 4) as usize;
                source[idx..idx + 4].copy_from_slice(&color);
            }
        }
        let g = ScaleGeometry::compute(
            w,
            h,
            Overscan::Crop224,
            ParRatio::SQUARE,
            FillMode::IntegerLocked { integer_factor: 4 },
        );
        assert_eq!(g.out_width, 8);
        let out = render_scaled_reference(&source, w, h, &g);
        // Row 0, columns 0..4 must be red; columns 4..8 must be green.
        for x in 0..4u32 {
            let idx = (x * 4) as usize;
            assert_eq!(&out[idx..idx + 4], &[255, 0, 0, 255]);
        }
        for x in 4..8u32 {
            let idx = (x * 4) as usize;
            assert_eq!(&out[idx..idx + 4], &[0, 255, 0, 255]);
        }
    }

    // --- GPU pass: skip cleanly if no adapter, hard-fail in CI, same
    // contract as every other GPU test in this crate. --------------------

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

    #[test]
    fn scale_pass_output_size_matches_declared_geometry() {
        let Some(gpu) = gpu_or_skip("scale_pass_output_size_matches_declared_geometry") else {
            return;
        };
        let pass = ScalePass::new(&gpu);
        let g = ScaleGeometry::compute(
            256,
            240,
            Overscan::Crop224,
            ParRatio::NES_SNES_NTSC_8_7,
            FillMode::IntegerLocked { integer_factor: 1 },
        );
        let source = solid_source(256, 240, [200, 100, 50, 255]);
        let out = pass
            .render(&gpu, &source, 256, 240, &g)
            .expect("scale pass readback must complete within the bounded GPU wait");
        assert_eq!(
            out.len(),
            (g.out_width as usize) * (g.out_height as usize) * 4
        );
    }
}
