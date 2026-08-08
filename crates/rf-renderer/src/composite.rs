//! Enhanced pipeline: shell-mediated compositing into arbitrarily sized
//! render targets, including ultrawide (ticket W4-03c; FR-REND-004;
//! `docs/design/RENDERER.md` §3: "Renders the `SceneGraph` back-to-front
//! into a virtual canvas whose size is decoupled from console output").
//!
//! ## Shell-mediated scope fence (plan.json W4-03c pre-flight notes)
//!
//! `ARCHITECTURE.md` §3's layer diagram draws no edge between `rf-renderer`
//! and `rf-enhance` in either direction, so this module never sees a
//! `SceneGraph`, a `TexId`/`CanvasId`/`LevelId` handle, or anything from
//! `rf-enhance` -- it has no dependency on that crate at all. The design
//! this module implements is *shell-mediated*: the `retroforge` app crate
//! (which already depends on both `rf-renderer` and `rf-enhance`) resolves
//! `SceneGraph` layers' handles into plain RGBA pixel buffers and hands
//! them here as [`CompositeLayer`]s -- textures and a target size, nothing
//! more. Building a handle registry in this crate, however tempting given
//! this is "where the compositing happens," would cross that fence for no
//! reason: nothing here needs to resolve a handle to composite bytes.
//!
//! ## FM-13 enforcement (`docs/design/FAILURE_MODES.md`)
//!
//! FM-13's *policy* (which zoom-fallback divisor to apply) is a pure
//! function in `rf-enhance::camera` (`fm13_zoom_divisor`/
//! `fm13_apply_divisor`) that this crate cannot call (same fence). This
//! module owns FM-13's other half: *enforcement* at the allocation
//! boundary, where the real adapter limit is actually known.
//! [`EnhancedCompositor::composite`] reads [`GpuContext::adapter_limits`]
//! (the real `max_texture_dimension_2d`, never a hardcoded constant --
//! CI's llvmpipe/lavapipe software adapter reports a smaller one than real
//! hardware) and clamps the requested target size *before* creating any
//! texture, reporting the reduction on [`CompositeOutcome::reduction`] so
//! the shell can surface FM-13's "view too large for GPU, reduced" toast.
//! A caller that hands this module an already-oversized size (whether
//! because it skipped the `rf-enhance` policy or computed one wrong) still
//! never causes a device loss from an allocation this crate makes --
//! that's the point of enforcing here rather than trusting the caller.

use std::borrow::Cow;

use crate::gpu::{read_buffer_sync, GpuContext};

const COMPOSITE_SHADER_SRC: &str = include_str!("shaders/composite.wgsl");

/// One layer to composite, already resolved to plain RGBA bytes by the
/// shell (module doc) -- this crate never sees the `SceneGraph`/`TexId`
/// this layer came from. `rgba` is straight (non-premultiplied) alpha,
/// row-major, top-to-bottom, `width * height * 4` bytes, matching every
/// other RGBA buffer in this crate ([`crate::FrameBuffer`],
/// [`crate::LayeredFrame`]). `dst_x`/`dst_y` place the layer's top-left
/// corner in the *target's* pixel space (signed: a layer may be partially
/// or fully off-target, e.g. a panned camera -- it is simply clipped, not
/// an error).
pub struct CompositeLayer<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub dst_x: i32,
    pub dst_y: i32,
}

/// FM-13: what the requested target size shrank to and why, so the shell
/// can surface `FAILURE_MODES.md`'s FM-13 toast ("view too large for GPU,
/// reduced") instead of silently rendering at a different size than what
/// was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetReduction {
    pub requested: (u32, u32),
    pub allocated: (u32, u32),
    pub max_dim: u32,
}

/// The composited result: `rgba` is `width * height * 4` bytes (straight
/// alpha, row-major, top-to-bottom, [`crate::composite`] module doc's
/// convention) -- `width`/`height` are what was *actually* allocated
/// (equal to the request unless [`Self::reduction`] is `Some`, FM-13).
pub struct CompositeOutcome {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub reduction: Option<TargetReduction>,
}

/// Every layer-resource handle that must stay alive from the moment its
/// bind group is created until the render pass that uses it has been
/// submitted (`EnhancedCompositor::composite`) -- named rather than a bare
/// tuple so a `Vec<LayerGpuResources>` reads as "the per-layer GPU
/// resources", not an opaque 4-tuple.
type LayerGpuResources = (
    wgpu::Buffer,
    wgpu::Texture,
    wgpu::TextureView,
    wgpu::BindGroup,
);

/// The enhanced-pipeline layer compositor: back-to-front textured-quad
/// blits into an arbitrarily sized target (module doc). Pipeline/
/// bind-group-layout/sampler are built once in [`Self::new`] and reused
/// across [`Self::composite`] calls, same "build once, reuse per frame"
/// shape as [`crate::original_pipeline::PalettePass`].
pub struct EnhancedCompositor {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl EnhancedCompositor {
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("rf-renderer::composite::shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(COMPOSITE_SHADER_SRC)),
            });

        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("rf-renderer::composite::bind_group_layout"),
                    entries: &[
                        // The per-layer NDC rect (composite.wgsl's `Rect`) --
                        // vertex stage only.
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX,
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
                label: Some("rf-renderer::composite::pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("rf-renderer::composite::pipeline"),
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
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        // Standard non-premultiplied "over" blending -- back-
                        // to-front draw order (module doc) is what makes a
                        // later, opaque layer fully replace an earlier one in
                        // an overlapping region; this is only the per-pixel
                        // blend equation, not the ordering itself.
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });

        // Nearest filtering, clamp-to-edge -- matches this project's
        // "nearest-neighbor unless a filter pass says otherwise" convention
        // (RENDERER.md §2) and keeps compositing bit-exact for the
        // golden-frame tests below (no interpolation blur at layer edges).
        let sampler = gpu
            .device
            .create_sampler(&wgpu::SamplerDescriptor::default());

        EnhancedCompositor {
            pipeline,
            bind_group_layout,
            sampler,
        }
    }

    /// FM-13 enforcement, as a pure function so it is directly unit-testable
    /// against a **mocked** limit with no GPU at all
    /// (`FAILURE_MODES.md`'s own FM-13 test column: "UT: mocked adapter
    /// limits") -- [`Self::composite`] is the only caller that supplies the
    /// *real* one. Clamps each axis independently to `max_dim` (never
    /// below 1, so a degenerate `0` request still yields an allocatable
    /// target); returns the size actually usable and, if either axis had
    /// to shrink, `Some` reduction describing what was asked for vs. what
    /// was granted -- the FM-13 "view too large for GPU, reduced" toast's
    /// payload.
    #[must_use]
    pub fn clamp_target_size(
        requested_w: u32,
        requested_h: u32,
        max_dim: u32,
    ) -> (u32, u32, Option<TargetReduction>) {
        let max_dim = max_dim.max(1);
        let w = requested_w.clamp(1, max_dim);
        let h = requested_h.clamp(1, max_dim);
        if w == requested_w && h == requested_h {
            (w, h, None)
        } else {
            (
                w,
                h,
                Some(TargetReduction {
                    requested: (requested_w, requested_h),
                    allocated: (w, h),
                    max_dim,
                }),
            )
        }
    }

    /// Composite `layers` back-to-front (`layers[0]` furthest back,
    /// `layers[last]` topmost -- draw order IS compositing order, module
    /// doc) into a target sized `requested_width`x`requested_height`,
    /// clamped first against `gpu`'s real adapter limit (FM-13 enforcement,
    /// [`Self::clamp_target_size`] against
    /// [`GpuContext::adapter_limits`] -- **before** any texture is
    /// created, so a request beyond the real limit never reaches
    /// `create_texture` and never risks a device loss from our own
    /// allocation).
    ///
    /// # Errors
    /// Returns `Err` if the GPU readback does not complete within
    /// [`crate::gpu::GPU_WAIT`] -- reported, never retried in a loop (see
    /// `crate::gpu`'s module doc).
    ///
    /// # Panics
    /// Panics if any `layer.rgba.len() != layer.width * layer.height * 4`
    /// -- a malformed layer here is a caller (shell) bug, same "caller
    /// bug, not a runtime condition to degrade through" stance as
    /// [`crate::original_pipeline::IndexedFrame::new`].
    pub fn composite(
        &self,
        gpu: &GpuContext,
        layers: &[CompositeLayer<'_>],
        requested_width: u32,
        requested_height: u32,
    ) -> Result<CompositeOutcome, String> {
        // Defensive: clamp against whichever of {the adapter's reported
        // limit, what the device was actually granted} is smaller, not
        // `adapter_limits` alone. `GpuContext::request_headless` requests
        // the device with `required_limits: adapter_limits.clone()` so
        // today these agree -- but `create_texture` validates against the
        // *device's* granted limits specifically (verified empirically
        // this ticket: requesting the conservative `DeviceDescriptor::
        // default()` grants only ~8192 regardless of a 16384-capable real
        // adapter, and enforcing against `adapter.limits()` alone in that
        // configuration reproduced the exact device-loss FM-13 exists to
        // prevent). Taking the `min()` here means "never a device loss
        // from our own allocation" holds regardless of how the device
        // happened to be requested, rather than resting on an unasserted
        // coincidence between two independently-set values.
        // Defensive: clamp against whichever of {the adapter's reported
        // limit, what the device was actually granted} is smaller, not
        // `adapter_limits` alone. `GpuContext::request_headless` requests
        // the device with `required_limits: adapter_limits.clone()` so
        // today these agree -- but `create_texture` validates against the
        // *device's* granted limits specifically (verified empirically
        // this ticket: requesting the conservative `DeviceDescriptor::
        // default()` grants only ~8192 regardless of a 16384-capable real
        // adapter, and enforcing against `adapter.limits()` alone in that
        // configuration reproduced the exact device-loss FM-13 exists to
        // prevent). Taking the `min()` here means "never a device loss
        // from our own allocation" holds regardless of how the device
        // happened to be requested, rather than resting on an unasserted
        // coincidence between two independently-set values.
        let max_dim = gpu
            .adapter_limits
            .max_texture_dimension_2d
            .min(gpu.device.limits().max_texture_dimension_2d);
        let (width, height, reduction) =
            Self::clamp_target_size(requested_width, requested_height, max_dim);

        let target_extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::composite::target"),
            size: target_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Build every layer's GPU resources (texture upload + NDC-rect
        // uniform + bind group) up front, kept alive in `resources` through
        // submission below -- keeps the render pass's mutable borrow of
        // `encoder` free of any interleaved device/queue calls.
        let mut resources: Vec<LayerGpuResources> = Vec::with_capacity(layers.len());
        for layer in layers {
            assert_eq!(
                layer.rgba.len(),
                (layer.width as usize) * (layer.height as usize) * 4,
                "CompositeLayer: {} bytes for a {}x{} layer (need {})",
                layer.rgba.len(),
                layer.width,
                layer.height,
                (layer.width as usize) * (layer.height as usize) * 4
            );

            let layer_extent = wgpu::Extent3d {
                width: layer.width.max(1),
                height: layer.height.max(1),
                depth_or_array_layers: 1,
            };
            let layer_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("rf-renderer::composite::layer"),
                size: layer_extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &layer_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                layer.rgba,
                // Queue::write_texture has no 256-byte row-alignment
                // requirement (unlike CommandEncoder::copy_*_to_*, see
                // the readback path below) -- same fact `original_pipeline`
                // already relies on for its indexed-texture upload.
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(layer.width * 4),
                    rows_per_image: Some(layer.height),
                },
                layer_extent,
            );
            let layer_view = layer_tex.create_view(&wgpu::TextureViewDescriptor::default());

            let rect = layer_rect_ndc(layer, width, height);
            let uniform_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rf-renderer::composite::rect"),
                size: 16, // one vec4<f32>, composite.wgsl's `Rect`.
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            gpu.queue
                .write_buffer(&uniform_buffer, 0, &rect_bytes(rect));

            let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rf-renderer::composite::bind_group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&layer_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });

            resources.push((uniform_buffer, layer_tex, layer_view, bind_group));
        }

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rf-renderer::composite::encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::composite::render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rpass.set_pipeline(&self.pipeline);
            // Draw in `layers`/`resources` order: index 0 first (furthest
            // back), last index last (topmost) -- this loop's iteration
            // order IS the back-to-front compositing order (module doc).
            for (_uniform_buffer, _layer_tex, _layer_view, bind_group) in &resources {
                rpass.set_bind_group(0, bind_group, &[]);
                rpass.draw(0..6, 0..1);
            }
        }

        // Readback: `CommandEncoder::copy_texture_to_buffer` (unlike
        // `Queue::write_texture` above) *does* require `bytes_per_row` to
        // be a multiple of `COPY_BYTES_PER_ROW_ALIGNMENT` (256) --
        // `original_pipeline`'s palette pass could `debug_assert` this away
        // because 256x240 RGBA8 (1024 bytes/row) already satisfies it; an
        // arbitrary/ultrawide target has no such guarantee (840 wide RGBA8
        // is 3360 bytes/row, not a multiple of 256), so this path pads
        // every row up to the alignment and strips the padding back out
        // after reading back.
        let bytes_per_row = width * 4;
        let padded_bytes_per_row = align_up(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback_size = u64::from(padded_bytes_per_row) * u64::from(height);
        let readback_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::composite::readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target_tex,
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
            target_extent,
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

        Ok(CompositeOutcome {
            rgba,
            width,
            height,
            reduction,
        })
    }
}

/// Round `value` up to the next multiple of `align` (`align` clamped to at
/// least 1 so this never divides by zero).
fn align_up(value: u32, align: u32) -> u32 {
    let align = align.max(1);
    value.div_ceil(align) * align
}

/// Pixel-space (top-left origin, +Y down, matching every RGBA buffer this
/// crate produces) -> clip-space NDC (+Y up) rect for `layer` inside a
/// `target_width`x`target_height` target -- the CPU half of
/// `composite.wgsl`'s doc, computed once per layer per [`EnhancedCompositor::composite`]
/// call rather than per fragment.
fn layer_rect_ndc(layer: &CompositeLayer<'_>, target_width: u32, target_height: u32) -> [f32; 4] {
    let tw = target_width.max(1) as f32;
    let th = target_height.max(1) as f32;
    let x0 = px_to_ndc_x(layer.dst_x as f32, tw);
    let y0 = px_to_ndc_y(layer.dst_y as f32, th);
    let x1 = px_to_ndc_x(layer.dst_x as f32 + layer.width as f32, tw);
    let y1 = px_to_ndc_y(layer.dst_y as f32 + layer.height as f32, th);
    [x0, y0, x1, y1]
}

fn px_to_ndc_x(px: f32, target_w: f32) -> f32 {
    (px / target_w).mul_add(2.0, -1.0)
}

fn px_to_ndc_y(py: f32, target_h: f32) -> f32 {
    (py / target_h).mul_add(-2.0, 1.0)
}

/// Little-endian bytes for `composite.wgsl`'s `Rect { rect: vec4<f32> }`
/// uniform -- 16 bytes, no padding (a single `vec4<f32>` is already
/// 16-byte aligned), same approach as `original_pipeline::lut_bytes`.
fn rect_bytes(rect: [f32; 4]) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    for (i, v) in rect.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{NES_HEIGHT, NES_WIDTH};

    /// Same "skip cleanly, never panic, never silently pass" contract as
    /// `crate::gpu::GpuContext::request_headless`'s doc and
    /// `crate::harness`-style GPU tests elsewhere in this workspace.
    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::request_headless() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                eprintln!(
                    "SKIP {test_name}: no wgpu adapter in this environment ({e}) -- \
                     clean skip, run with --nocapture to see this line"
                );
                None
            }
        }
    }

    fn solid_rgba(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for _ in 0..(width * height) {
            v.extend_from_slice(&color);
        }
        v
    }

    fn pixel_at(out: &CompositeOutcome, x: u32, y: u32) -> [u8; 4] {
        let idx = ((y * out.width + x) * 4) as usize;
        [
            out.rgba[idx],
            out.rgba[idx + 1],
            out.rgba[idx + 2],
            out.rgba[idx + 3],
        ]
    }

    // --- FM-13 clamp: pure function, mocked limit, no GPU needed --------

    #[test]
    fn clamp_target_size_within_limit_reports_no_reduction() {
        let (w, h, r) = EnhancedCompositor::clamp_target_size(800, 600, 8192);
        assert_eq!((w, h), (800, 600));
        assert_eq!(r, None);
    }

    #[test]
    fn clamp_target_size_over_limit_on_one_axis_reduces_and_reports() {
        let (w, h, r) = EnhancedCompositor::clamp_target_size(10_000, 600, 8192);
        assert_eq!((w, h), (8192, 600));
        assert_eq!(
            r,
            Some(TargetReduction {
                requested: (10_000, 600),
                allocated: (8192, 600),
                max_dim: 8192,
            })
        );
    }

    #[test]
    fn clamp_target_size_over_limit_on_both_axes_reduces_and_reports() {
        let (w, h, r) = EnhancedCompositor::clamp_target_size(20_000, 16_000, 4096);
        assert_eq!((w, h), (4096, 4096));
        assert_eq!(
            r,
            Some(TargetReduction {
                requested: (20_000, 16_000),
                allocated: (4096, 4096),
                max_dim: 4096,
            })
        );
    }

    // --- Back-to-front ordering: layers MUST overlap (pre-flight vacuity
    // trap (b) -- a non-overlapping ordering test cannot detect a reversed
    // draw order). ---------------------------------------------------

    #[test]
    fn back_to_front_order_makes_the_topmost_overlapping_layer_win() {
        let Some(gpu) = gpu_or_skip("back_to_front_order_makes_the_topmost_overlapping_layer_win")
        else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);

        let target_w = 200;
        let target_h = 150;
        let red = solid_rgba(100, 100, [255, 0, 0, 255]);
        let blue = solid_rgba(80, 60, [0, 0, 255, 255]);
        let layers = [
            CompositeLayer {
                rgba: &red,
                width: 100,
                height: 100,
                dst_x: 0,
                dst_y: 0,
            },
            CompositeLayer {
                rgba: &blue,
                width: 80,
                height: 60,
                dst_x: 40,
                dst_y: 40,
            },
        ];

        let out = compositor
            .composite(&gpu, &layers, target_w, target_h)
            .expect("composite must succeed");
        assert_eq!(out.width, target_w);
        assert_eq!(out.height, target_h);
        assert_eq!(out.reduction, None);

        // Inside the overlap (both layers cover [40,100)x[40,60)): the
        // later-drawn (topmost) blue layer must win.
        assert_eq!(
            pixel_at(&out, 60, 50),
            [0, 0, 255, 255],
            "the topmost (later-drawn) layer must win in the overlap region"
        );
        // Red-only region (outside blue's footprint): red must still show.
        assert_eq!(
            pixel_at(&out, 10, 10),
            [255, 0, 0, 255],
            "the back layer must still show where the front layer doesn't cover it"
        );
        // No layer covers this corner: stays the clear color.
        assert_eq!(pixel_at(&out, 150, 120), [0, 0, 0, 0]);
    }

    // --- Arbitrary/ultrawide target sizing: pre-flight vacuity trap (a) --
    // a single fixed-size golden frame proves nothing about "arbitrarily
    // sized"; the target here is neither 256x240 nor an integer multiple
    // of it, and the far corner outside the source layer's footprint must
    // stay untouched -- a naive "stretch source to fill target" upscale
    // could never produce that. --------------------------------------

    #[test]
    fn composites_into_an_ultrawide_target_not_a_multiple_of_the_source() {
        let Some(gpu) =
            gpu_or_skip("composites_into_an_ultrawide_target_not_a_multiple_of_the_source")
        else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);

        let target_w = 840u32; // 21:9-ish ultrawide (840/360 = 2.333...)
        let target_h = 360u32;
        assert_ne!(
            target_w % NES_WIDTH as u32,
            0,
            "target must not be a multiple of the source"
        );
        assert_ne!(
            target_h % NES_HEIGHT as u32,
            0,
            "target must not be a multiple of the source"
        );

        let source = solid_rgba(NES_WIDTH as u32, NES_HEIGHT as u32, [255, 255, 0, 255]);
        let layers = [CompositeLayer {
            rgba: &source,
            width: NES_WIDTH as u32,
            height: NES_HEIGHT as u32,
            dst_x: 50,
            dst_y: 30,
        }];

        let out = compositor
            .composite(&gpu, &layers, target_w, target_h)
            .expect("composite must succeed");
        assert_eq!(
            out.width, target_w,
            "must produce the exact requested arbitrary target size"
        );
        assert_eq!(out.height, target_h);
        assert_eq!(out.reduction, None);
        assert_eq!(
            out.rgba.len(),
            (target_w as usize) * (target_h as usize) * 4
        );

        // Inside the source layer's placed footprint ([50,306)x[30,270)).
        assert_eq!(pixel_at(&out, 60, 40), [255, 255, 0, 255]);
        // Outside the layer's footprint but inside the ultrawide target --
        // a naive full-target upscale of the source would have painted
        // this corner too; genuine positional compositing leaves it clear.
        assert_eq!(
            pixel_at(&out, 800, 340),
            [0, 0, 0, 0],
            "area outside every layer's footprint must stay clear, not a naive upscale"
        );
        // Exactly on the layer's top-left corner vs. one pixel outside it
        // on both axes -- pins the pixel->NDC->rasterizer mapping itself
        // (`layer_rect_ndc`/`px_to_ndc_x`/`px_to_ndc_y`) at the boundary,
        // where an off-by-one would live and every other assertion here
        // (comfortably interior to the footprint) cannot see.
        assert_eq!(
            pixel_at(&out, 50, 30),
            [255, 255, 0, 255],
            "the layer's own top-left pixel must be inside its footprint"
        );
        assert_eq!(
            pixel_at(&out, 49, 29),
            [0, 0, 0, 0],
            "one pixel outside the layer's top-left corner (either axis) must be clear"
        );
    }

    #[test]
    fn composites_into_a_second_arbitrary_size_not_a_multiple_of_the_source() {
        let Some(gpu) =
            gpu_or_skip("composites_into_a_second_arbitrary_size_not_a_multiple_of_the_source")
        else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);

        // A different arbitrary size again neither 256x240 nor a multiple
        // of it (independent of the ultrawide test above), so a special-
        // cased "handle exactly one other size" implementation is caught
        // too.
        let target_w = 333u32;
        let target_h = 777u32;
        assert_ne!(target_w % NES_WIDTH as u32, 0);
        assert_ne!(target_h % NES_HEIGHT as u32, 0);

        let green = solid_rgba(40, 40, [0, 255, 0, 255]);
        let layers = [CompositeLayer {
            rgba: &green,
            width: 40,
            height: 40,
            dst_x: 5,
            dst_y: 5,
        }];

        let out = compositor
            .composite(&gpu, &layers, target_w, target_h)
            .expect("composite must succeed");
        assert_eq!(out.width, target_w);
        assert_eq!(out.height, target_h);
        assert_eq!(pixel_at(&out, 20, 20), [0, 255, 0, 255]);
        assert_eq!(pixel_at(&out, 300, 700), [0, 0, 0, 0]);
    }

    // --- FM-13 enforcement against the REAL adapter limit: pre-flight
    // vacuity trap (c) -- must genuinely exceed the real limit, not a
    // hardcoded guess, and prove both no-oversized-allocation and the
    // reduction being reported. ----------------------------------------

    #[test]
    fn composite_clamps_to_the_real_adapter_limit_and_reports_the_reduction() {
        let Some(gpu) =
            gpu_or_skip("composite_clamps_to_the_real_adapter_limit_and_reports_the_reduction")
        else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);

        let max_dim = gpu.adapter_limits.max_texture_dimension_2d;
        // Genuinely beyond this environment's own reported limit (never a
        // hardcoded constant, per this ticket's brief) -- height stays
        // small so the request's byte footprint is trivial even though the
        // width dimension itself violates the limit.
        let requested_w = max_dim + 4096;
        let requested_h = 64;
        assert!(requested_w > max_dim);

        let layers: Vec<CompositeLayer<'_>> = Vec::new();
        let out = compositor
            .composite(&gpu, &layers, requested_w, requested_h)
            .expect(
                "must fall back to a smaller target, never fail/device-lose on our own \
                 oversized allocation",
            );

        assert!(
            out.width <= max_dim,
            "allocated width ({}) must never exceed the real adapter limit ({max_dim})",
            out.width
        );
        assert!(out.height <= max_dim);
        let reduction = out
            .reduction
            .expect("a request beyond the real limit must report a reduction (FM-13 toast)");
        assert_eq!(reduction.requested, (requested_w, requested_h));
        assert_eq!(reduction.allocated, (out.width, out.height));
        assert_eq!(reduction.max_dim, max_dim);
    }

    // --- Straight-alpha "over" blending is genuine, not indistinguishable
    // from REPLACE: every other test above uses fully opaque layers, where
    // `BlendState::ALPHA_BLENDING` and `BlendState::REPLACE` produce the
    // same output -- the realistic first caller (`crate::LayeredFrame`,
    // which emits explicitly transparent regions) hands this module
    // partial transparency, so that path needs its own coverage. -------

    #[test]
    fn partial_alpha_front_layer_genuinely_blends_not_replaces() {
        let Some(gpu) = gpu_or_skip("partial_alpha_front_layer_genuinely_blends_not_replaces")
        else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);

        let target_w = 100;
        let target_h = 100;
        let red = solid_rgba(100, 100, [255, 0, 0, 255]); // opaque back layer, fills the target
        let translucent_blue = solid_rgba(50, 50, [0, 0, 255, 128]); // ~50% alpha front layer
        let layers = [
            CompositeLayer {
                rgba: &red,
                width: 100,
                height: 100,
                dst_x: 0,
                dst_y: 0,
            },
            CompositeLayer {
                rgba: &translucent_blue,
                width: 50,
                height: 50,
                dst_x: 0,
                dst_y: 0,
            },
        ];

        let out = compositor
            .composite(&gpu, &layers, target_w, target_h)
            .expect("composite must succeed");

        let blended = pixel_at(&out, 25, 25); // inside the overlap
        assert_eq!(
            blended[1], 0,
            "green channel must stay 0 -- neither layer contributes green"
        );
        assert_eq!(
            blended[3], 255,
            "compositing anything over a fully opaque background must stay fully opaque"
        );
        assert!(
            blended[0] > 0 && blended[0] < 255,
            "red channel {} must land strictly between the two layers' values -- a \
             genuine blend, not REPLACE overriding to pure blue (0) or a no-op leaving \
             pure red (255)",
            blended[0]
        );
        assert!(
            blended[2] > 0 && blended[2] < 255,
            "blue channel {} must land strictly between the two layers' values",
            blended[2]
        );

        // Outside the translucent layer's footprint: pure opaque red,
        // untouched.
        assert_eq!(pixel_at(&out, 75, 75), [255, 0, 0, 255]);
    }
}
