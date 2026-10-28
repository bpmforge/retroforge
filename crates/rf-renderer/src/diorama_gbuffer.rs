//! The depth + motion-vector pre-pass of the temporal upscaler (ticket
//! W39-05; `docs/design/REBUILD_LAB.md` section 9, "HD-2D on one engine").
//!
//! A temporal upscaler (MetalFX's `MTLFXTemporalScaler`, or the portable
//! fallback in [`crate::temporal`]) needs, besides the jittered colour, a
//! **depth** image and a **motion vector** per pixel. The colour comes from
//! the ordinary [`crate::diorama::DioramaPass`] (full lighting, shadow map
//! and MSAA; this crate keeps one shading path). This pass draws the *same
//! mesh* a second time, single-sampled and unlit, with the same alpha-test
//! discards, into:
//!
//! * a `Depth32Float` texture (0 near, 1 far; the standard perspective
//!   convention of [`crate::diorama::camera_matrices::perspective`], so
//!   MetalFX's `depthReversed` stays `false`), and
//! * an `Rg16Float` motion texture in **input pixels**, pointing from a
//!   pixel to where its surface point was in the previous frame (the MetalFX
//!   `motionVectorScale = 1` convention: a point that moved 10 px down and
//!   right has the vector `(-10, -10)`).
//!
//! The mesh is rasterised with the *jittered* matrix; the motion is built
//! from the *unjittered* current and previous matrices, so the sub-pixel
//! jitter never leaks into the motion vector (it would blur for ever).
//! Moving objects: pass the previous frame's vertices; they are used only
//! when they pair up with this frame's one for one (same count, kinds and
//! uvs), otherwise the frame is treated as camera motion only. Said plainly:
//! the HD-2D plan rebuilds its mesh every frame, so an entity that appears or
//! disappears reorders the list, and that frame's object motion is dropped
//! (the upscaler's history clamp absorbs it).
//!
//! Every loop is a range or an iterator (law 8).

use std::borrow::Cow;
use std::sync::Mutex;

use crate::diorama::camera_matrices::Mat4;
use crate::diorama_mesh::Vertex;
use crate::gpu::{align_up, GpuContext};

const SHADER_SRC: &str = include_str!("shaders/diorama_gbuffer.wgsl");
/// The depth image's format.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The motion image's format.
pub const MOTION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;
const VERTEX_BYTES: usize = 40;
const PREV_BYTES: usize = 12;

/// What one G-buffer frame is drawn from.
#[derive(Clone, Copy)]
pub struct GBufferInput<'a> {
    pub vertices: &'a [Vertex],
    /// Last frame's vertices, for object motion; see the module doc.
    pub prev_vertices: Option<&'a [Vertex]>,
    pub ground_rgba: &'a [u8],
    pub ground_size: (u32, u32),
    pub sprite_rgba: &'a [u8],
    pub sprite_size: (u32, u32),
    /// The matrix the mesh is rasterised with (jittered).
    pub vp_jittered: Mat4,
    /// This frame's unjittered matrix.
    pub vp_current: Mat4,
    /// Last frame's unjittered matrix.
    pub vp_previous: Mat4,
    /// The internal (input) resolution.
    pub size: (u32, u32),
}

/// The two images of a frame.
#[derive(Clone)]
pub struct GBuffer {
    /// `Depth32Float`, usage `RENDER_ATTACHMENT | TEXTURE_BINDING`.
    pub depth: wgpu::Texture,
    /// `Rg16Float`, usage `RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC`.
    pub motion: wgpu::Texture,
}

struct Sampled {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
}

struct Cache {
    size: (u32, u32),
    gbuffer: Option<GBuffer>,
    depth_view: Option<wgpu::TextureView>,
    motion_view: Option<wgpu::TextureView>,
    ground: Option<Sampled>,
    sprite: Option<Sampled>,
    vertex: Option<(wgpu::Buffer, u64)>,
    prev: Option<(wgpu::Buffer, u64)>,
}

/// The pass: one pipeline, its sampler and uniform, and the per-size images.
pub struct GBufferPass {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    cache: Mutex<Cache>,
}

impl GBufferPass {
    /// Build the pipeline.
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("rf-renderer::gbuffer::layout"),
                entries: &[
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
                    tex_entry(1),
                    tex_entry(2),
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
                label: Some("rf-renderer::gbuffer::pipeline_layout"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("rf-renderer::gbuffer::shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SRC)),
            });
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 20,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 36,
                shader_location: 3,
            },
        ];
        let prev_attributes = [wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 4,
        }];
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("rf-renderer::gbuffer::pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[
                        wgpu::VertexBufferLayout {
                            array_stride: VERTEX_BYTES as u64,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &attributes,
                        },
                        wgpu::VertexBufferLayout {
                            array_stride: PREV_BYTES as u64,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &prev_attributes,
                        },
                    ],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: MOTION_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
        // Nearest, like the Diorama pass's own sampler (pixel art).
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rf-renderer::gbuffer::sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::gbuffer::uniform"),
            size: 64 * 3 + 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            layout,
            pipeline,
            sampler,
            uniform,
            cache: Mutex::new(Cache {
                size: (0, 0),
                gbuffer: None,
                depth_view: None,
                motion_view: None,
                ground: None,
                sprite: None,
                vertex: None,
                prev: None,
            }),
        }
    }

    /// Draw the frame's depth and motion; the images stay on the GPU.
    ///
    /// # Errors
    /// A buffer whose size does not match its dimensions.
    pub fn render(&self, gpu: &GpuContext, input: &GBufferInput<'_>) -> Result<GBuffer, String> {
        for (bytes, (w, h), what) in [
            (input.ground_rgba, input.ground_size, "ground"),
            (input.sprite_rgba, input.sprite_size, "sprite"),
        ] {
            if bytes.len() != w as usize * h as usize * 4 {
                return Err(format!("gbuffer: {what} buffer does not match {w}x{h}"));
            }
        }
        let (w, h) = (input.size.0.max(1), input.size.1.max(1));
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let cache = &mut *guard;
        if cache.size != (w, h) || cache.gbuffer.is_none() {
            let make = |label, format, usage| {
                gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
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
            let depth = make(
                "rf-renderer::gbuffer::depth",
                DEPTH_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let motion = make(
                "rf-renderer::gbuffer::motion",
                MOTION_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
            );
            cache.depth_view = Some(depth.create_view(&wgpu::TextureViewDescriptor::default()));
            cache.motion_view = Some(motion.create_view(&wgpu::TextureViewDescriptor::default()));
            cache.gbuffer = Some(GBuffer { depth, motion });
            cache.size = (w, h);
        }
        for (slot, label, size, bytes) in [
            (
                &mut cache.ground,
                "rf-renderer::gbuffer::ground",
                input.ground_size,
                input.ground_rgba,
            ),
            (
                &mut cache.sprite,
                "rf-renderer::gbuffer::sprite",
                input.sprite_size,
                input.sprite_rgba,
            ),
        ] {
            let size = (size.0.max(1), size.1.max(1));
            if slot.as_ref().is_none_or(|t| t.size != size) {
                let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                *slot = Some(Sampled {
                    texture,
                    view,
                    size,
                });
            }
            if let Some(t) = slot {
                if !bytes.is_empty() {
                    gpu.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &t.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        bytes,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(size.0 * 4),
                            rows_per_image: Some(size.1),
                        },
                        wgpu::Extent3d {
                            width: size.0,
                            height: size.1,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
        }

        // Vertices: current, and the previous positions paired to them.
        let n = input.vertices.len();
        let paired = input
            .prev_vertices
            .filter(|p| p.len() == n && p.iter().zip(input.vertices).all(|(a, b)| same_slot(a, b)));
        let mut cur_bytes = Vec::with_capacity(n * VERTEX_BYTES);
        let mut prev_bytes = Vec::with_capacity(n * PREV_BYTES);
        for (i, v) in input.vertices.iter().enumerate() {
            for f in v.pos.iter().chain(&v.uv).chain(&v.color) {
                cur_bytes.extend_from_slice(&f.to_le_bytes());
            }
            cur_bytes.extend_from_slice(&v.kind.to_le_bytes());
            let p = paired.map_or(v.pos, |p| p[i].pos);
            for f in p {
                prev_bytes.extend_from_slice(&f.to_le_bytes());
            }
        }
        for (slot, label, bytes) in [
            (
                &mut cache.vertex,
                "rf-renderer::gbuffer::vertices",
                &cur_bytes,
            ),
            (&mut cache.prev, "rf-renderer::gbuffer::prev", &prev_bytes),
        ] {
            let need = (bytes.len().max(VERTEX_BYTES) as u64).next_power_of_two();
            if slot.as_ref().is_none_or(|(_, cap)| *cap < need) {
                *slot = Some((
                    gpu.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(label),
                        size: need,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    need,
                ));
            }
            if let (Some((buffer, _)), false) = (slot.as_ref(), bytes.is_empty()) {
                gpu.queue.write_buffer(buffer, 0, bytes);
            }
        }

        let mut ubo = Vec::with_capacity(64 * 3 + 16);
        for m in [input.vp_jittered, input.vp_current, input.vp_previous] {
            for f in m {
                ubo.extend_from_slice(&f.to_le_bytes());
            }
        }
        for f in [w as f32, h as f32, 0.0, 0.0] {
            ubo.extend_from_slice(&f.to_le_bytes());
        }
        gpu.queue.write_buffer(&self.uniform, 0, &ubo);

        let (
            Some(gb),
            Some(depth_view),
            Some(motion_view),
            Some(ground),
            Some(sprite),
            Some((vbuf, _)),
            Some((pbuf, _)),
        ) = (
            &cache.gbuffer,
            &cache.depth_view,
            &cache.motion_view,
            &cache.ground,
            &cache.sprite,
            &cache.vertex,
            &cache.prev,
        )
        else {
            return Err("gbuffer resources missing".to_string());
        };
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rf-renderer::gbuffer::bind_group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&ground.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&sprite.view),
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
                label: Some("rf-renderer::gbuffer::encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::gbuffer::pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: motion_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if n > 0 {
                rpass.set_pipeline(&self.pipeline);
                rpass.set_bind_group(0, &bind_group, &[]);
                rpass.set_vertex_buffer(0, vbuf.slice(..));
                rpass.set_vertex_buffer(1, pbuf.slice(..));
                rpass.draw(0..n as u32, 0..1);
            }
        }
        gpu.queue.submit(std::iter::once(encoder.finish()));
        Ok(gb.clone())
    }
}

/// Two vertices are the same slot of the mesh in two frames.
fn same_slot(a: &Vertex, b: &Vertex) -> bool {
    a.kind == b.kind && a.uv == b.uv
}

/// Decode an IEEE half to `f32` (the motion image's texel type).
#[must_use]
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f32::from(h & 0x3ff);
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        31 if frac == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

/// Read a motion image back as `(dx, dy)` pairs, row-major (tests and
/// diagnostics; one wait, not a per-frame call).
///
/// # Errors
/// A readback that does not complete within [`crate::gpu::GPU_WAIT`].
pub fn read_motion(
    gpu: &GpuContext,
    motion: &wgpu::Texture,
    size: (u32, u32),
) -> Result<Vec<[f32; 2]>, String> {
    let (w, h) = size;
    let padded = align_up(w * 4, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rf-renderer::gbuffer::motion_readback"),
        size: u64::from(padded) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: motion,
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
    let mut out = Vec::with_capacity(w as usize * h as usize);
    for row in mapped.chunks(padded as usize).take(h as usize) {
        for px in row[..w as usize * 4].chunks_exact(4) {
            out.push([
                f16_to_f32(u16::from_le_bytes([px[0], px[1]])),
                f16_to_f32(u16::from_le_bytes([px[2], px[3]])),
            ]);
        }
    }
    drop(mapped);
    buffer.unmap();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_decode() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert!(f16_to_f32(0x7c00).is_infinite());
        assert!(f16_to_f32(0x7e00).is_nan());
        // Smallest subnormal: 2^-24.
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
    }

    #[test]
    fn vertex_stride_matches_the_struct() {
        assert_eq!(std::mem::size_of::<Vertex>(), VERTEX_BYTES);
    }
}
