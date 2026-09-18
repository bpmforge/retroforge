//! The Diorama pass ("walls pop up"; ticket W16-06; `docs/design/
//! ENHANCEMENT_WAVE_16.md` §5/§8) — a standalone GPU pass, following
//! `crate::fog::FogPass`'s own conventions exactly (that module's own doc
//! explains why a standalone pass rather than a
//! [`crate::shader_chain::ChainStage`]: this pass needs more than one
//! input texture and a bind-group layout `ChainStage` does not have).
//! [`DioramaPass`] takes two textures (a ground texture and a
//! sprite-cutout texture, both plain RGBA — this crate has no dependency
//! on `rf-enhance`, `ARCHITECTURE.md` §3, so it knows nothing about
//! `SceneLayer::Geometry` or `SceneLayer::SpriteSet`; resolving those into
//! plain bytes plus [`crate::diorama_mesh::Billboard`] footprints is the
//! app shell's job, `crates/retroforge/src/enhanced_view.rs`) and a real
//! per-tile mesh ([`crate::diorama_mesh::build_vertices`]) rather than a
//! fullscreen triangle.
//!
//! ## Camera and projection: a UBO, no depth buffer
//!
//! [`camera_matrices::view_proj`] builds a fixed-pitch ([`PITCH_DEG`]),
//! fixed-FOV ([`FOV_Y_DEG`]) perspective camera framing the whole tile
//! grid — "only the camera pitches" (§5), never the input/hit-testing
//! space, which stays 2D in the shell entirely (this crate has no
//! opinion on input at all). The resulting `view * proj` matrix is
//! uploaded as a 64-byte UBO (`shaders/diorama.wgsl`'s `Camera`) that the
//! vertex stage applies. There is **no** depth attachment —
//! `crate::diorama_mesh`'s own module doc explains why (painter's
//! algorithm, matching every other pass in this crate).
//!
//! ## Frame-budget gate reuse (§8)
//!
//! This pass does **not** define its own `BudgetGate`/threshold —
//! [`crate::fog::BudgetGate`] and [`crate::fog::DISABLE_P95_MS`] are
//! reused directly (§8: "shares the same harness ... rather than
//! inventing a second timing mechanism"); see `tests/diorama_golden.rs`'s
//! budget-gate-reuse test.

use std::borrow::Cow;

use crate::diorama_mesh::Vertex;
use crate::gpu::{align_up, read_buffer_sync, GpuContext};

const DIORAMA_SHADER_SRC: &str = include_str!("shaders/diorama.wgsl");

/// Fixed camera pitch, degrees below horizontal (§5: "the camera pitches
/// at a **fixed** angle" — the concept-shots' Link's Awakening-diorama
/// framing this ticket cites). Neither measured nor tunable at runtime;
/// a plain middle-of-the-road "looking down at a diorama" angle.
pub const PITCH_DEG: f32 = 52.0;
/// Fixed vertical field of view, degrees.
pub const FOV_Y_DEG: f32 = 50.0;

/// Small hand-rolled column-major 4x4 matrix math (this crate has no
/// `glam`/`nalgebra`/etc. dependency, `Cargo.toml`'s own dependency list —
/// adding one for two matrices is not proportionate). Column-major to
/// match WGSL's own `mat4x4<f32>` storage/multiplication convention
/// (`shaders/diorama.wgsl`'s `vs_main` does `camera.view_proj *
/// vec4(in.pos, 1.0)` directly against this layout). Plain textbook
/// perspective/look-at formulas — "arithmetic nobody owns", the same
/// provenance stance `shader_chain.rs`'s own module doc takes for
/// `nearest`/`scanlines`.
pub mod camera_matrices {
    pub type Mat4 = [f32; 16];

    #[must_use]
    pub fn perspective(fovy_rad: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fovy_rad / 2.0).tan();
        let mut m = [0.0f32; 16];
        m[0] = f / aspect;
        m[5] = f;
        m[10] = (far + near) / (near - far);
        m[11] = -1.0;
        m[14] = (2.0 * far * near) / (near - far);
        m
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn normalize(a: [f32; 3]) -> [f32; 3] {
        let len = dot(a, a).sqrt().max(1e-6);
        [a[0] / len, a[1] / len, a[2] / len]
    }

    #[must_use]
    pub fn look_at(eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> Mat4 {
        let f = normalize(sub(center, eye));
        let s = normalize(cross(f, up));
        let u = cross(s, f);
        [
            s[0],
            u[0],
            -f[0],
            0.0, //
            s[1],
            u[1],
            -f[1],
            0.0, //
            s[2],
            u[2],
            -f[2],
            0.0, //
            -dot(s, eye),
            -dot(u, eye),
            dot(f, eye),
            1.0,
        ]
    }

    /// Column-major matrix multiply, `a * b` (apply `b` first, then `a`
    /// — the same order `proj * view` names below).
    #[must_use]
    pub fn mul(a: Mat4, b: Mat4) -> Mat4 {
        let mut out = [0.0f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += a[k * 4 + row] * b[col * 4 + k];
                }
                out[col * 4 + row] = sum;
            }
        }
        out
    }

    /// Fixed-pitch, fixed-FOV camera framing a `tiles_w * tile_px` by
    /// `tiles_h * tile_px` ground plane (module doc). `distance_factor`
    /// (`>= 1.0`) trades framing tightness for headroom; [`super::
    /// DioramaPass`] always calls this with its own documented constant —
    /// exposed here only so the approximation is a named, testable
    /// number rather than a magic literal buried in a render call.
    ///
    /// **Approximate framing, stated plainly**: this is "back off until
    /// the grid probably fits", not an exact fit-to-viewport solve (which
    /// would need the grid's aspect ratio reconciled against the
    /// viewport's) — good enough for a diorama overlay, not a claim of
    /// precise composition.
    #[must_use]
    pub fn view_proj(
        tiles_w: f32,
        tiles_h: f32,
        tile_px: f32,
        aspect: f32,
        pitch_deg: f32,
        fovy_deg: f32,
        distance_factor: f32,
    ) -> Mat4 {
        let ground_w = tiles_w * tile_px;
        let ground_h = tiles_h * tile_px;
        let target = [ground_w / 2.0, 0.0, ground_h / 2.0];
        let extent = ground_w.max(ground_h).max(tile_px);
        let distance = extent * distance_factor.max(1.0);
        let pitch = pitch_deg.to_radians();
        let eye = [
            target[0],
            pitch.sin() * distance,
            target[2] - pitch.cos() * distance,
        ];
        let proj = perspective(fovy_deg.to_radians(), aspect.max(1e-3), 1.0, extent * 8.0);
        let view = look_at(eye, target, [0.0, 1.0, 0.0]);
        mul(proj, view)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_grids_own_centre_projects_inside_the_view_frustum() {
            let vp = view_proj(4.0, 4.0, 16.0, 16.0 / 9.0, 52.0, 50.0, 1.3);
            // Ground-plane centre, at y=0 (module doc's `target`).
            let center = [32.0, 0.0, 32.0, 1.0];
            let clip = mul_vec4(vp, center);
            assert!(clip[3] > 0.0, "w must be positive in front of the camera");
            let ndc = [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]];
            assert!(
                ndc[0].abs() <= 1.01 && ndc[1].abs() <= 1.01,
                "the framed grid's own centre must land inside the NDC frustum: {ndc:?}"
            );
        }

        #[test]
        fn a_farther_row_and_a_nearer_row_are_both_in_front_of_the_camera() {
            let vp = view_proj(1.0, 4.0, 16.0, 1.0, 52.0, 50.0, 1.3);
            let near_row = mul_vec4(vp, [8.0, 0.0, 8.0, 1.0]); // row 0 centre
            let far_row = mul_vec4(vp, [8.0, 0.0, 56.0, 1.0]); // row 3 centre
            assert!(near_row[3] > 0.0);
            assert!(far_row[3] > 0.0);
        }

        fn mul_vec4(m: Mat4, v: [f32; 4]) -> [f32; 4] {
            let mut out = [0.0f32; 4];
            for row in 0..4 {
                let mut sum = 0.0;
                for col in 0..4 {
                    sum += m[col * 4 + row] * v[col];
                }
                out[row] = sum;
            }
            out
        }
    }
}

fn camera_ubo_bytes(view_proj: camera_matrices::Mat4) -> [u8; 64] {
    let mut bytes = [0u8; 64];
    for (i, f) in view_proj.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    bytes
}

/// Pack `verts` into the exact byte layout `DioramaPass::new`'s
/// `VertexAttribute`s declare (offsets 0/12/20/36, module doc) — written
/// field-by-field rather than a `#[repr(C)]` transmute so this stays
/// correct regardless of host endianness (this project only ever runs on
/// little-endian hosts today, but `to_le_bytes` costs nothing and removes
/// the assumption).
fn vertex_bytes(verts: &[Vertex]) -> Vec<u8> {
    let mut out = Vec::with_capacity(std::mem::size_of_val(verts));
    for v in verts {
        for f in v.pos {
            out.extend_from_slice(&f.to_le_bytes());
        }
        for f in v.uv {
            out.extend_from_slice(&f.to_le_bytes());
        }
        for f in v.color {
            out.extend_from_slice(&f.to_le_bytes());
        }
        out.extend_from_slice(&v.kind.to_le_bytes());
    }
    out
}

/// The Diorama pass's own pipeline/bind-group-layout/sampler (module doc:
/// not a [`crate::shader_chain::ChainStage`] — two input textures, a real
/// vertex buffer, no depth attachment).
pub struct DioramaPass {
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

impl DioramaPass {
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("rf-renderer::diorama::bind_group_layout"),
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
                label: Some("rf-renderer::diorama::pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });

        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("rf-renderer::diorama::shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(DIORAMA_SHADER_SRC)),
            });

        let vertex_stride = std::mem::size_of::<Vertex>() as wgpu::BufferAddress;
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

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("rf-renderer::diorama::pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: vertex_stride,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    }],
                },
                // No depth/stencil: `crate::diorama_mesh`'s own module doc
                // explains why (painter's algorithm, matching every other
                // pass in this crate).
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });

        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rf-renderer::diorama::sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        DioramaPass {
            bind_group_layout,
            pipeline,
            sampler,
        }
    }

    /// Render one diorama frame: `vertices` (already back-to-front,
    /// `crate::diorama_mesh::build_vertices`) against `ground_rgba`
    /// (`ground_w`x`ground_h`) and `sprite_rgba` (`sprite_w`x`sprite_h`,
    /// the sprite-cutout texture billboards sample), into an
    /// `out_width`x`out_height` target with a fixed-pitch camera framing
    /// `tiles_w`x`tiles_h` tiles of `tile_px` each
    /// ([`camera_matrices::view_proj`]).
    ///
    /// Returns straight-alpha RGBA8 (transparent where nothing was
    /// drawn — the caller composites this over the rest of the scene the
    /// same way [`crate::composite::EnhancedCompositor`] composites any
    /// other layer).
    ///
    /// # Errors
    /// Returns `Err` if the GPU readback does not complete within
    /// [`crate::gpu::GPU_WAIT`].
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        gpu: &GpuContext,
        vertices: &[Vertex],
        ground_rgba: &[u8],
        ground_w: u32,
        ground_h: u32,
        sprite_rgba: &[u8],
        sprite_w: u32,
        sprite_h: u32,
        tiles_w: u32,
        tiles_h: u32,
        tile_px: u32,
        out_width: u32,
        out_height: u32,
    ) -> Result<Vec<u8>, String> {
        assert_eq!(
            ground_rgba.len(),
            (ground_w as usize) * (ground_h as usize) * 4,
            "DioramaPass::render: ground buffer size mismatch"
        );
        assert_eq!(
            sprite_rgba.len(),
            (sprite_w as usize) * (sprite_h as usize) * 4,
            "DioramaPass::render: sprite buffer size mismatch"
        );

        let make_tex = |label: &str, w: u32, h: u32, bytes: &[u8]| -> wgpu::TextureView {
            let extent = wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            };
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
            if w > 0 && h > 0 {
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
                        bytes_per_row: Some(w * 4),
                        rows_per_image: Some(h),
                    },
                    extent,
                );
            }
            tex.create_view(&wgpu::TextureViewDescriptor::default())
        };

        let ground_view = make_tex(
            "rf-renderer::diorama::ground",
            ground_w,
            ground_h,
            ground_rgba,
        );
        let sprite_view = make_tex(
            "rf-renderer::diorama::sprite",
            sprite_w,
            sprite_h,
            sprite_rgba,
        );

        let output_extent = wgpu::Extent3d {
            width: out_width.max(1),
            height: out_height.max(1),
            depth_or_array_layers: 1,
        };
        let output_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::diorama::output"),
            size: output_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let aspect = out_width.max(1) as f32 / out_height.max(1) as f32;
        let vp = camera_matrices::view_proj(
            tiles_w.max(1) as f32,
            tiles_h.max(1) as f32,
            tile_px.max(1) as f32,
            aspect,
            PITCH_DEG,
            FOV_Y_DEG,
            1.3,
        );
        let camera_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::diorama::camera"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue
            .write_buffer(&camera_buffer, 0, &camera_ubo_bytes(vp));

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rf-renderer::diorama::bind_group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&ground_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&sprite_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        // An empty vertex list (e.g. a zero-sized grid) is a valid,
        // fully-transparent frame -- use a Draw/DoDoNothing pattern (zero
        // vertices) rather than special-casing it: `create_buffer` with
        // `size: 0` is invalid on some backends, so a one-vertex
        // placeholder buffer plus a `draw(0..0, ..)` sidesteps that
        // without changing the render pass shape.
        let vb_bytes = vertex_bytes(vertices);
        let vb_size = vb_bytes.len().max(std::mem::size_of::<Vertex>()) as u64;
        let vertex_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::diorama::vertices"),
            size: vb_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !vb_bytes.is_empty() {
            gpu.queue.write_buffer(&vertex_buffer, 0, &vb_bytes);
        }

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rf-renderer::diorama::encoder"),
            });
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::diorama::render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Transparent clear: an area the mesh never
                        // covers (outside the framed grid, or a fully
                        // empty scene) composites as "nothing here",
                        // never an opaque colour standing in for one.
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
            rpass.set_bind_group(0, &bind_group, &[]);
            rpass.set_vertex_buffer(0, vertex_buffer.slice(..));
            rpass.draw(0..vertices.len() as u32, 0..1);
        }

        let bytes_per_row = out_width.max(1) * 4;
        let padded_bytes_per_row = align_up(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback_size = u64::from(padded_bytes_per_row) * u64::from(out_height.max(1));
        let readback_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::diorama::readback"),
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
                    rows_per_image: Some(out_height.max(1)),
                },
            },
            output_extent,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));

        let padded = read_buffer_sync(&gpu.device, &readback_buffer)?;
        let mut rgba = Vec::with_capacity((bytes_per_row as usize) * (out_height.max(1) as usize));
        for row in padded
            .chunks(padded_bytes_per_row as usize)
            .take(out_height.max(1) as usize)
        {
            rgba.extend_from_slice(&row[..bytes_per_row as usize]);
        }
        Ok(rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::request_headless() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                eprintln!(
                    "SKIP {test_name}: no wgpu adapter in this environment ({e}) -- clean skip"
                );
                None
            }
        }
    }

    #[test]
    fn diorama_pass_renders_a_single_solid_tile_scene() {
        let Some(gpu) = gpu_or_skip("diorama_pass_renders_a_single_solid_tile_scene") else {
            return;
        };
        let pass = DioramaPass::new(&gpu);
        let scene = crate::diorama_mesh::DioramaScene {
            tiles_w: 2,
            tiles_h: 2,
            tile_px: 16.0,
            solid: &[0, 0, 1, 0],
            depth: &[0, 0, 8, 0],
            billboards: &[],
        };
        let verts = crate::diorama_mesh::build_vertices(&scene);
        let ground = vec![128u8; 32 * 32 * 4];
        let sprite = vec![0u8; 4 * 4 * 4];
        let out = pass
            .render(
                &gpu, &verts, &ground, 32, 32, &sprite, 4, 4, 2, 2, 16, 64, 64,
            )
            .expect("diorama pass renders");
        assert_eq!(out.len(), 64 * 64 * 4);
        let any_opaque = out.chunks_exact(4).any(|px| px[3] > 0);
        assert!(
            any_opaque,
            "some pixel must be covered by the ground/box mesh"
        );
    }

    #[test]
    fn diorama_pass_is_deterministic_for_identical_inputs() {
        let Some(gpu) = gpu_or_skip("diorama_pass_is_deterministic_for_identical_inputs") else {
            return;
        };
        let pass = DioramaPass::new(&gpu);
        let scene = crate::diorama_mesh::DioramaScene {
            tiles_w: 1,
            tiles_h: 1,
            tile_px: 16.0,
            solid: &[1],
            depth: &[8],
            billboards: &[],
        };
        let verts = crate::diorama_mesh::build_vertices(&scene);
        let ground = vec![64u8; 16 * 16 * 4];
        let sprite = vec![0u8; 4];
        let a = pass
            .render(
                &gpu, &verts, &ground, 16, 16, &sprite, 1, 1, 1, 1, 16, 32, 32,
            )
            .unwrap();
        let b = pass
            .render(
                &gpu, &verts, &ground, 16, 16, &sprite, 1, 1, 1, 1, 16, 32, 32,
            )
            .unwrap();
        assert_eq!(a, b);
    }
}
