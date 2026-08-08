//! Original pipeline, palette pass (ticket W3-01; `docs/design/RENDERER.md`
//! §2/§6/§7): `indexed frame -> R8Uint texture upload -> LUT palette pass ->
//! RGBA8 target -> readback`. This module stops at the **1x pre-shader
//! buffer** deliberately -- RENDERER.md §7: the palette pass is bit-exact by
//! construction (integer LUT lookups, no filtering, no sRGB math), so a hash
//! of *this* buffer is stable across backends/drivers; the scale pass and
//! any shader chain after it are not (§6/§7), and golden-hashing past this
//! point would produce goldens that pass on one machine and fail on another
//! for reasons that are not bugs.
//!
//! Every struct/field/type name below was checked against the vendored
//! `wgpu-29.0.4`/`wgpu-types-29.0.4` source (`~/.cargo/registry/src`), not
//! written from memory -- several differ from pre-29 idioms beyond the three
//! `docs/TECH_STACK.md` already documents: `TexelCopyTextureInfo`/
//! `TexelCopyBufferInfo`/`TexelCopyBufferLayout` replace the old
//! `ImageCopy*` names, `PipelineLayoutDescriptor::bind_group_layouts` is
//! `&[Option<&BindGroupLayout>]` (not `&[&BindGroupLayout]`) and gained an
//! `immediate_size` field replacing `push_constant_ranges`, and
//! `Device::poll` takes a `PollType` (not the old `Maintain`) returning
//! `Result<PollStatus, PollError>`.

use std::borrow::Cow;

use crate::gpu::{read_buffer_sync, GpuContext};
use crate::palette::NES_PALETTE;

const PALETTE_SHADER_SRC: &str = include_str!("shaders/palette.wgsl");

/// One frame's worth of raw NES palette indices (`$00`-`$3F`, unmasked --
/// the shader masks, same "degrade, never index out of range" stance as
/// [`crate::palette::palette_index_to_rgb`]), row-major, top-to-bottom.
/// Deliberately not [`crate::FrameBuffer`]: that type already resolved to
/// RGBA on the CPU (W1-06's blit path); this is the *pre*-resolution input
/// the GPU palette pass consumes instead.
pub struct IndexedFrame {
    width: u32,
    height: u32,
    indices: Vec<u8>,
}

impl IndexedFrame {
    /// # Panics
    /// Panics if `indices.len() != width * height` -- a malformed frame here
    /// is a caller bug (wrong dimensions), not a runtime condition to
    /// degrade through, unlike an out-of-range *palette index* within an
    /// otherwise well-formed frame.
    #[must_use]
    pub fn new(width: u32, height: u32, indices: Vec<u8>) -> Self {
        assert_eq!(
            indices.len(),
            (width as usize) * (height as usize),
            "IndexedFrame: {} indices for a {width}x{height} frame (need {})",
            indices.len(),
            (width as usize) * (height as usize)
        );
        IndexedFrame {
            width,
            height,
            indices,
        }
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }
}

/// Pack the console palette as 64 `vec4<f32>` entries (16 bytes each, RGBA,
/// alpha always opaque), little-endian, matching `palette.wgsl`'s
/// `array<vec4<f32>, 64>` storage binding layout exactly (WGSL `vec4<f32>`
/// is 16-byte aligned, so this needs no padding between entries).
fn lut_bytes(palette: &[[u8; 3]; 64]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64 * 16);
    for [r, g, b] in palette {
        for channel in [*r, *g, *b, 0xFF] {
            bytes.extend_from_slice(&(f32::from(channel) / 255.0).to_le_bytes());
        }
    }
    bytes
}

/// The GPU original-pipeline palette pass: indexed R8Uint texture in, RGBA8
/// bytes out. Pipeline/bind-group-layout/LUT are built once in [`Self::new`]
/// and reused across [`Self::render`] calls (only the per-frame indexed
/// texture and output target are recreated per call -- real per-frame reuse
/// of those is a later perf pass, not this ticket's concern).
pub struct PalettePass {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    lut_buffer: wgpu::Buffer,
}

impl PalettePass {
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("rf-renderer::palette_pass::shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(PALETTE_SHADER_SRC)),
            });

        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("rf-renderer::palette_pass::bind_group_layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Uint,
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
                label: Some("rf-renderer::palette_pass::pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("rf-renderer::palette_pass::pipeline"),
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

        let lut_bytes = lut_bytes(&NES_PALETTE);
        let lut_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::palette_pass::lut"),
            size: lut_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(&lut_buffer, 0, &lut_bytes);

        PalettePass {
            pipeline,
            bind_group_layout,
            lut_buffer,
        }
    }

    /// Render `frame` through the palette pass and read back the resulting
    /// RGBA8 bytes (`width * height * 4`, row-major, alpha always `0xFF`) --
    /// the "1x pre-shader buffer" `docs/design/RENDERER.md` §6/§7 names as
    /// the only thing CI is allowed to golden-hash.
    ///
    /// # Errors
    /// Returns `Err` if the GPU readback does not complete within
    /// [`crate::gpu::GPU_WAIT`] -- reported, never retried in a loop (see
    /// `crate::gpu`'s module doc).
    pub fn render(&self, gpu: &GpuContext, frame: &IndexedFrame) -> Result<Vec<u8>, String> {
        let width = frame.width;
        let height = frame.height;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let indexed_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::palette_pass::indexed"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &indexed_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.indices,
            // Queue::write_texture has no 256-byte row-alignment
            // requirement (unlike CommandEncoder::copy_*_to_*) -- one byte
            // per R8Uint texel, so `bytes_per_row` is just `width`.
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width),
                rows_per_image: Some(height),
            },
            extent,
        );
        let indexed_view = indexed_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let output_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::palette_pass::output"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rf-renderer::palette_pass::bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.lut_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&indexed_view),
                },
            ],
        });

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rf-renderer::palette_pass::encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::palette_pass::render_pass"),
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
        // At NES resolution (256x240) this is already 1024, a multiple of
        // 256 -- asserted rather than silently relied on, since
        // RENDERER.md §2 also names a 512-wide mode that would need
        // explicit row padding if this pass is ever reused at that width.
        debug_assert_eq!(
            bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
            0,
            "readback row stride {bytes_per_row} is not 256-aligned; \
             copy_texture_to_buffer (unlike write_texture) requires it -- \
             pad rows before widening this pass past 256px"
        );
        let readback_size = u64::from(bytes_per_row) * u64::from(height);
        let readback_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::palette_pass::readback"),
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
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));

        read_buffer_sync(&gpu.device, &readback_buffer)
    }
}
