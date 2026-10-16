//! Ticket W31-05: the Super FX polygon stream redrawn as **GPU triangles** at
//! a higher internal resolution, with hardware multisample antialiasing.
//!
//! One WGSL shader (`shaders/superfx_poly.wgsl`) runs through the renderer's
//! wgpu backend: Metal on macOS, Vulkan/DX12 elsewhere. Presentation only
//! (project laws 4 and 6): it reads a [`PolyJob`] a core already observed and
//! never writes anything back. Clean-room: a flat-colour triangle pass, no
//! code from any emulator.
//!
//! ## Geometry
//!
//! The game fills the rows `[ymin, ymax)` of a polygon and, per row, the
//! columns `floor(xl)..=floor(xr)` ([`crate::gsu_poly`]): the right edge is
//! one native pixel further right than the geometric edge. The polygon is cut
//! into horizontal bands at its corner rows; in each band the leftmost and
//! rightmost edges are the left and right edge (the same min/max rule the
//! scanline fill uses, so any polygon works), and the band is the quad
//! between them ([`polygon_triangles`]). The right edge is drawn `+1` native
//! pixel further right, so a tip shared by a left and a right edge is one
//! pixel wide, as in the game. Two more corrections make a continuous edge
//! stand where the game's staircase stands: the game reads an edge's x at the
//! **top** of a row while a pixel sample reads it at the row's centre (move
//! the edge by `-slope / 2`), and it `floor`s it (move the edge by the mean
//! fraction that drops, [`edge_slope_and_bias`]).
//!
//! A degenerate polygon (every corner on one line, e.g. the one-pixel panel
//! lines) has no area, so it is drawn as a thin quad one native pixel wide
//! (steep) or tall (shallow) along its segment: continuous, where the
//! scanline rule leaves one lone pixel per row. A `PLOT` is a 1x1 native
//! quad.
//!
//! **Corners stay on the whole-pixel grid**: the game stores integer screen
//! corners, so this sharpens edges (true antialiasing and finer slopes) but
//! cannot recover sub-pixel corner positions. Those would need the
//! camera-space records the GSU keeps at RAM 0x0600-0x07A1 (follow-on
//! research ticket W31-06).
//!
//! ## Colour
//!
//! Each vertex carries the two colours its op can produce (even / odd native
//! pixel parity, from [`plot_colour`] -- the one definition of `PLOT`'s
//! colour rule), already resolved through the caller's index -> RGB mapping;
//! alpha 0 means "draws nothing". The fragment shader picks by
//! `(native_x ^ native_y) & 1`, so dither keeps its native-pixel scale.
//!
//! ## Output
//!
//! The job is drawn in op order (painter's algorithm, no depth buffer) into a
//! cleared `Rgba8Unorm` target of `viewport * scale`, multisampled and
//! resolved, then read back. The resolve averages the stored samples, so the
//! raw result is premultiplied by coverage; [`GpuPolyImage::rgba`] is
//! converted back to **straight** alpha, where alpha is the pixel's coverage.

use std::borrow::Cow;

use rf_core_api::gsu_poly::{plot_colour, PolyJob, PolyOp};

use crate::gpu::{align_up, read_buffer_sync, GpuContext};

const SHADER_SRC: &str = include_str!("shaders/superfx_poly.wgsl");
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Smallest and largest supported integer scale.
pub const SCALE_MIN: u32 = 2;
pub const SCALE_MAX: u32 = 8;
/// Sample counts the setting offers; each is used only if the device supports
/// it for [`FORMAT`].
const SAMPLE_CHOICES: [u32; 3] = [1, 4, 8];

/// A GPU-drawn picture: `w * h` RGBA, straight alpha = coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuPolyImage {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

impl GpuPolyImage {
    /// `(rgb, alpha)` at `(x, y)`.
    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> ([u8; 3], u8) {
        let i = (y * self.w + x) * 4;
        (
            [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]],
            self.rgba[i + 3],
        )
    }
}

/// One vertex: native-pixel position and the two parity colours (packed
/// RGBA8, red in the low byte, alpha 0 = transparent).
#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    pos: [f32; 2],
    even: u32,
    odd: u32,
}

impl Vertex {
    fn bytes(vs: &[Vertex]) -> Vec<u8> {
        let mut out = Vec::with_capacity(vs.len() * 16);
        for v in vs {
            out.extend_from_slice(&v.pos[0].to_le_bytes());
            out.extend_from_slice(&v.pos[1].to_le_bytes());
            out.extend_from_slice(&v.even.to_le_bytes());
            out.extend_from_slice(&v.odd.to_le_bytes());
        }
        out
    }
}

/// An edge's `(slope, bias)`: `dx / dy` in native pixels per row, and the mean
/// of what the game's `floor` drops from the edge's x over its rows. Over
/// the `dy` rows the fractional part of `x` takes each multiple of
/// `gcd(|dx|, dy) / dy` once, so the mean is `(1 - g / dy) / 2` (0 for a
/// vertical edge). The game's staircase sits that far left of the true edge
/// on average, so the GPU edge is moved by it.
fn edge_slope_and_bias(dx: i32, dy: i32) -> (f32, f32) {
    let (mut p, mut q) = (dx.abs(), dy);
    // Euclid: `q` shrinks every pass, so this terminates (law 8).
    while q != 0 {
        (p, q) = (q, p % q);
    }
    (dx as f32 / dy as f32, (1.0 - p as f32 / dy as f32) / 2.0)
}

/// An edge of the polygon, oriented downward (`ya < yb`).
struct Edge {
    xa: i32,
    ya: i32,
    yb: i32,
    slope: f32,
    bias: f32,
}

impl Edge {
    /// The true (unshifted) x at row coordinate `y`.
    fn x_at(&self, y: f32) -> f32 {
        self.xa as f32 + self.slope * (y - self.ya as f32)
    }
    /// How far the GPU edge sits from the true one: `-slope / 2` (the game
    /// reads x at a row's top, a pixel sample at its centre) minus the mean
    /// `floor` loss, plus the one pixel the right edge is drawn further.
    fn shift(&self, right: bool) -> f32 {
        f32::from(u8::from(right)) - self.slope / 2.0 - self.bias
    }
}

/// The triangle list (native pixels) the GPU should fill for `corners`, in
/// the way the game's scanline rule fills them: a polygon is cut into
/// horizontal bands at its distinct corner rows (rows `[ymin, ymax)`); in a
/// band the leftmost active edge is the left edge and the rightmost the right
/// edge (the game fills from its leftmost to its rightmost crossing), and the
/// band is the quad between them. The right edge is drawn one native pixel
/// further and every edge is moved by [`Edge::shift`] (see the module doc).
/// Orientation-free, and exact for any polygon whose edges do not cross
/// inside a band.
///
/// A polygon with no area (all corners on one line) becomes a thin quad one
/// native pixel wide (steep) or tall (shallow); fewer than two distinct
/// corners, or no height, give nothing. Pure, so the rule is unit-tested
/// without a GPU.
#[must_use]
pub fn polygon_triangles(corners: &[(u8, u8)]) -> Vec<[f32; 2]> {
    // Drop consecutive duplicates (including the wrap-around one).
    let mut pts: Vec<(i32, i32)> = Vec::with_capacity(corners.len());
    for &(x, y) in corners {
        let p = (i32::from(x), i32::from(y));
        if pts.last() != Some(&p) {
            pts.push(p);
        }
    }
    while pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    if pts.len() < 2 {
        return Vec::new();
    }
    // Twice the signed area: 0 means every corner is on one line.
    let area2: i64 = pts
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let b = pts[(i + 1) % pts.len()];
            i64::from(a.0) * i64::from(b.1) - i64::from(b.0) * i64::from(a.1)
        })
        .sum();
    if area2 == 0 {
        // The two extreme points, lexicographically.
        let (Some(&a), Some(&b)) = (pts.iter().min(), pts.iter().max()) else {
            return Vec::new();
        };
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        // Shallow -> one native pixel tall, steep -> one native pixel wide.
        let (ox, oy) = if dx.abs() > dy.abs() { (0, 1) } else { (1, 0) };
        let f = |p: (i32, i32), ex: i32, ey: i32| [(p.0 + ex) as f32, (p.1 + ey) as f32];
        let q = [f(a, 0, 0), f(b, 0, 0), f(b, ox, oy), f(a, ox, oy)];
        return vec![q[0], q[1], q[2], q[0], q[2], q[3]];
    }
    let edges: Vec<Edge> = pts
        .iter()
        .enumerate()
        .filter_map(|(i, &a)| {
            let b = pts[(i + 1) % pts.len()];
            let (a, b) = if a.1 <= b.1 { (a, b) } else { (b, a) };
            (a.1 < b.1).then(|| {
                let (slope, bias) = edge_slope_and_bias(b.0 - a.0, b.1 - a.1);
                Edge {
                    xa: a.0,
                    ya: a.1,
                    yb: b.1,
                    slope,
                    bias,
                }
            })
        })
        .collect();
    let mut rows: Vec<i32> = pts.iter().map(|p| p.1).collect();
    rows.sort_unstable();
    rows.dedup();
    let mut out = Vec::new();
    // Adjacent pairs of the sorted distinct rows: always advances.
    for band in rows.windows(2) {
        let (y0, y1) = (band[0], band[1]);
        let mid = (y0 + y1) as f32 / 2.0;
        let mut active = edges.iter().filter(|e| e.ya <= y0 && e.yb >= y1);
        let Some(first) = active.next() else { continue };
        let (mut left, mut right) = (first, first);
        for e in active {
            if e.x_at(mid) < left.x_at(mid) {
                left = e;
            }
            if e.x_at(mid) > right.x_at(mid) {
                right = e;
            }
        }
        let (sl, sr) = (left.shift(false), right.shift(true));
        let (f0, f1) = (y0 as f32, y1 as f32);
        let (tl, tr) = ([left.x_at(f0) + sl, f0], [right.x_at(f0) + sr, f0]);
        let (bl, br) = ([left.x_at(f1) + sl, f1], [right.x_at(f1) + sr, f1]);
        out.extend([tl, tr, br, tl, br, bl]);
    }
    out
}

/// The triangle-list vertices of `job`, plus the colour per op.
fn build_vertices(job: &PolyJob, colours: &dyn Fn(u8) -> Option<[u8; 3]>) -> Vec<Vertex> {
    let keep: u8 = if job.bpp >= 8 {
        0xFF
    } else {
        (1u8 << job.bpp) - 1
    };
    // The colour a native pixel of the given parity takes, as packed RGBA8.
    let pack = |colr: u8, por: u8, x: u8, y: u8| -> u32 {
        plot_colour(colr, por, job.bpp, x, y)
            .and_then(|c| colours(c & keep))
            .map_or(0, |[r, g, b]| {
                u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16 | 0xFF << 24
            })
    };
    let mut out = Vec::new();
    for op in &job.ops {
        let (tris, colr, por) = match op {
            PolyOp::Plot { x, y, colr, por } => {
                let (x, y) = (f32::from(*x), f32::from(*y));
                let (tl, tr, br, bl) = ([x, y], [x + 1.0, y], [x + 1.0, y + 1.0], [x, y + 1.0]);
                (vec![tl, tr, br, tl, br, bl], *colr, *por)
            }
            PolyOp::Polygon { corners, colr, por } => (polygon_triangles(corners), *colr, *por),
        };
        // Parity 0 is the pixel (0, 0), parity 1 is (1, 0): `plot_colour`
        // reads only `(x ^ y) & 1`.
        let (even, odd) = (pack(colr, por, 0, 0), pack(colr, por, 1, 0));
        if even == 0 && odd == 0 {
            continue;
        }
        out.extend(tris.into_iter().map(|pos| Vertex { pos, even, odd }));
    }
    out
}

/// The Super FX GPU drawing pass: built once, then [`Self::render`] per job.
pub struct SuperFxGpu {
    bind_group_layout: wgpu::BindGroupLayout,
    /// `(sample count, pipeline)` for each supported choice.
    pipelines: Vec<(u32, wgpu::RenderPipeline)>,
}

/// Whether a `Rgba8Unorm` render texture of `count` samples is valid on this
/// device. Probed with a validation error scope: the adapter is not kept in
/// [`GpuContext`], and on wgpu 29 a count the format features do not report
/// is a validation error, not a panic, under a scope.
fn sample_count_ok(device: &wgpu::Device, count: u32) -> bool {
    if count == 1 {
        return true;
    }
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _probe = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rf-renderer::superfx_gpu::sample_probe"),
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: count,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    pollster::block_on(scope.pop()).is_none()
}

impl SuperFxGpu {
    /// Build the shader module, layout and one pipeline per supported
    /// sample count.
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        let device = &gpu.device;
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rf-renderer::superfx_gpu::bind_group_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rf-renderer::superfx_gpu::pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rf-renderer::superfx_gpu::shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER_SRC)),
        });
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 8,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 12,
                shader_location: 2,
            },
        ];
        let pipelines = SAMPLE_CHOICES
            .into_iter()
            .filter(|&n| sample_count_ok(device, n))
            .map(|count| {
                let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("rf-renderer::superfx_gpu::pipeline"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &attributes,
                        }],
                    },
                    // No culling (either winding), no depth: painter's order.
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState {
                        count,
                        mask: !0,
                        alpha_to_coverage_enabled: false,
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: FORMAT,
                            // Replace: later ops overwrite the samples they cover.
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                });
                (count, pipeline)
            })
            .collect();
        Self {
            bind_group_layout,
            pipelines,
        }
    }

    /// The sample counts this device can draw with (always contains 1).
    #[must_use]
    pub fn supported_samples(&self) -> Vec<u32> {
        self.pipelines.iter().map(|p| p.0).collect()
    }

    /// The sample count [`Self::render`] will really use for a request: the
    /// largest supported choice not above it.
    #[must_use]
    pub fn effective_samples(&self, requested: u32) -> u32 {
        self.pipelines
            .iter()
            .map(|p| p.0)
            .filter(|&n| n <= requested.max(1))
            .max()
            .unwrap_or(1)
    }

    /// Draw `job` at `scale`x (clamped to `2..=8`) with `msaa` samples
    /// (`1`, `4` or `8`, lowered to what the device supports). `colours`
    /// maps a palette index to its screen RGB (`None` = not drawn).
    /// Returns the `viewport * scale` picture with straight alpha = coverage.
    ///
    /// # Errors
    /// An empty viewport, a target larger than the device allows, or a
    /// readback failure.
    pub fn render(
        &self,
        gpu: &GpuContext,
        job: &PolyJob,
        viewport: (u16, u16),
        scale: u32,
        msaa: u32,
        colours: &dyn Fn(u8) -> Option<[u8; 3]>,
    ) -> Result<GpuPolyImage, String> {
        let scale = scale.clamp(SCALE_MIN, SCALE_MAX);
        let (vw, vh) = (u32::from(viewport.0), u32::from(viewport.1));
        if vw == 0 || vh == 0 {
            return Err("empty viewport".to_string());
        }
        let (w, h) = (vw * scale, vh * scale);
        let max = gpu.adapter_limits.max_texture_dimension_2d;
        if w > max || h > max {
            return Err(format!("{w}x{h} exceeds the device's {max} texture limit"));
        }
        let samples = self.effective_samples(msaa);
        let pipeline = self
            .pipelines
            .iter()
            .find(|p| p.0 == samples)
            .map(|p| &p.1)
            .ok_or("no pipeline for the sample count")?;
        let device = &gpu.device;

        let vertices = build_vertices(job, colours);
        let vb_bytes = Vertex::bytes(&vertices);
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::superfx_gpu::vertices"),
            size: vb_bytes.len().max(16) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !vb_bytes.is_empty() {
            gpu.queue.write_buffer(&vertex_buffer, 0, &vb_bytes);
        }
        let mut params = Vec::with_capacity(16);
        for v in [vw as f32, vh as f32, scale as f32, 0.0f32] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::superfx_gpu::params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(&params_buffer, 0, &params);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rf-renderer::superfx_gpu::bind_group"),
            layout: &self.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buffer.as_entire_binding(),
            }],
        });

        let extent = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let make = |label, sample_count, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage,
                view_formats: &[],
            })
        };
        let resolved = make(
            "rf-renderer::superfx_gpu::resolved",
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let resolved_view = resolved.create_view(&wgpu::TextureViewDescriptor::default());
        let msaa_tex = (samples > 1).then(|| {
            make(
                "rf-renderer::superfx_gpu::msaa",
                samples,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
        });
        let msaa_view = msaa_tex
            .as_ref()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rf-renderer::superfx_gpu::encoder"),
        });
        {
            let (view, resolve_target, store) = match &msaa_view {
                Some(mv) => (mv, Some(&resolved_view), wgpu::StoreOp::Discard),
                None => (&resolved_view, None, wgpu::StoreOp::Store),
            };
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::superfx_gpu::pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !vertices.is_empty() {
                rpass.set_pipeline(pipeline);
                rpass.set_bind_group(0, &bind_group, &[]);
                rpass.set_vertex_buffer(0, vertex_buffer.slice(..));
                rpass.draw(0..vertices.len() as u32, 0..1);
            }
        }
        let bytes_per_row = w * 4;
        let padded = align_up(bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::superfx_gpu::readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &resolved,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            extent,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));
        let raw = read_buffer_sync(device, &readback)?;

        let mut rgba = Vec::with_capacity(bytes_per_row as usize * h as usize);
        for row in raw.chunks(padded as usize).take(h as usize) {
            for px in row[..bytes_per_row as usize].chunks_exact(4) {
                let a = u32::from(px[3]);
                // The resolve averaged coverage-weighted colours (the clear
                // is transparent black): divide the coverage back out.
                let un = |c: u8| -> u8 {
                    if a == 0 || a == 255 {
                        c
                    } else {
                        ((u32::from(c) * 255 + a / 2) / a).min(255) as u8
                    }
                };
                rgba.extend_from_slice(&[un(px[0]), un(px[1]), un(px[2]), px[3]]);
            }
        }
        Ok(GpuPolyImage {
            w: w as usize,
            h: h as usize,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gsu_poly::{rasterise_job_scaled, NONE};

    fn job(ops: Vec<PolyOp>, bpp: u8) -> PolyJob {
        PolyJob {
            ops,
            bpp,
            viewport: (16, 16),
            plotted: Vec::new(),
            bitmap: None,
        }
    }

    fn poly(corners: &[(u8, u8)], colr: u8, por: u8) -> PolyOp {
        PolyOp::Polygon {
            corners: corners.to_vec(),
            colr,
            por,
        }
    }

    /// Index -> a distinct colour (index 0 included), never `None`.
    fn colour(i: u8) -> Option<[u8; 3]> {
        Some([i * 16 + 8, 200 - i * 9, i * 5 + 30])
    }

    fn gpu() -> Option<(GpuContext, SuperFxGpu)> {
        match GpuContext::request_headless() {
            Ok(g) => {
                let s = SuperFxGpu::new(&g);
                Some((g, s))
            }
            Err(e) => {
                println!("SKIP superfx_gpu: {e}");
                None
            }
        }
    }

    fn extent(t: &[[f32; 2]]) -> (f32, f32, f32, f32) {
        let f = |g: fn(&[f32; 2]) -> f32, m: fn(f32, f32) -> f32, init: f32| {
            t.iter().map(g).fold(init, m)
        };
        (
            f(|p| p[0], f32::min, f32::MAX),
            f(|p| p[0], f32::max, f32::MIN),
            f(|p| p[1], f32::min, f32::MAX),
            f(|p| p[1], f32::max, f32::MIN),
        )
    }

    #[test]
    fn a_rectangle_is_the_pixel_inclusive_rectangle_whatever_the_winding() {
        // Rows 1..5, columns 2..=6 -> [2, 7) x [1, 5).
        let t = polygon_triangles(&[(2, 1), (6, 1), (6, 5), (2, 5)]);
        assert_eq!(t.len(), 6);
        assert_eq!(extent(&t), (2.0, 7.0, 1.0, 5.0));
        let t = polygon_triangles(&[(2, 5), (6, 5), (6, 1), (2, 1)]);
        assert_eq!(extent(&t), (2.0, 7.0, 1.0, 5.0));
    }

    #[test]
    fn a_triangle_tip_is_one_pixel_wide_at_the_row_centre() {
        // Apex (4,0): the game's row 0 is the one pixel [4, 5). The left
        // edge (slope -1) is moved +0.5, the right edge (slope +1) is
        // shifted +1 and moved -0.5, so both start at x = 4.5 on the row's
        // top and are 1 apart (4 .. 5) at its centre.
        let t = polygon_triangles(&[(4, 0), (8, 4), (0, 4)]);
        assert_eq!(t.len(), 6);
        assert!(t.contains(&[4.5, 0.0]) && t.contains(&[8.5, 4.0]) && t.contains(&[0.5, 4.0]));
        let at_centre =
            |a: [f32; 2], b: [f32; 2]| a[0] + (b[0] - a[0]) * (0.5 - a[1]) / (b[1] - a[1]);
        assert!((at_centre([4.5, 0.0], [0.5, 4.0]) - 4.0).abs() < 1e-6);
        assert!((at_centre([4.5, 0.0], [8.5, 4.0]) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn the_floor_bias_is_the_mean_fraction_over_the_edge() {
        // dx 10 over dy 9 (gcd 1): fractions k/9 for k = 0..9 -> mean 4/9.
        let (slope, bias) = edge_slope_and_bias(10, 9);
        assert!((slope - 10.0 / 9.0).abs() < 1e-6 && (bias - 4.0 / 9.0).abs() < 1e-6);
        assert_eq!(
            edge_slope_and_bias(0, 7).1,
            0.0,
            "vertical: nothing dropped"
        );
        assert_eq!(edge_slope_and_bias(-15, 3).1, 0.0, "whole pixels per row");
        // Brute force against the game's own floor for a few edges.
        for (dx, dy) in [(5i32, 12i32), (-5, 12), (7, 3), (-3, 8), (1, 1)] {
            let mean: f32 = (0..dy)
                .map(|k| {
                    let n = dx * k;
                    (n.rem_euclid(dy)) as f32 / dy as f32
                })
                .sum::<f32>()
                / dy as f32;
            assert!(
                (edge_slope_and_bias(dx, dy).1 - mean).abs() < 1e-5,
                "{dx}/{dy}"
            );
        }
    }

    #[test]
    fn a_fan_cannot_overshoot_a_steep_polygon() {
        // The old fan-from-a-vertex outline spilled outside steep shapes:
        // the area covered must stay near the 1x rule's (here 96 pixels).
        let t = polygon_triangles(&[(5, 0), (15, 9), (0, 12)]);
        let area: f32 = t
            .chunks_exact(3)
            .map(|v| {
                ((v[1][0] - v[0][0]) * (v[2][1] - v[0][1])
                    - (v[2][0] - v[0][0]) * (v[1][1] - v[0][1]))
                    .abs()
                    / 2.0
            })
            .sum();
        assert!((area - 96.0).abs() < 3.0, "area {area}");
    }

    #[test]
    fn degenerate_polygons_become_thin_quads() {
        // Steep: one native pixel wide.
        let t = polygon_triangles(&[(5, 2), (5, 12), (5, 2)]);
        assert_eq!(t.len(), 6);
        assert_eq!(extent(&t), (5.0, 6.0, 2.0, 12.0));
        // Shallow: one native pixel tall.
        let t = polygon_triangles(&[(2, 3), (14, 5)]);
        assert_eq!(extent(&t), (2.0, 14.0, 3.0, 6.0));
        // Collinear three points.
        assert_eq!(polygon_triangles(&[(0, 0), (4, 4), (8, 8)]).len(), 6);
        // A single point, and nothing, draw nothing.
        assert!(polygon_triangles(&[(3, 3), (3, 3), (3, 3)]).is_empty());
        assert!(polygon_triangles(&[]).is_empty());
    }

    #[test]
    fn a_square_at_msaa_1_covers_exactly_the_cpu_raster() {
        let Some((g, s)) = gpu() else { return };
        let j = job(vec![poly(&[(2, 1), (6, 1), (6, 5), (2, 5)], 3, 0)], 4);
        let img = s.render(&g, &j, (16, 16), 3, 1, &colour).expect("render");
        assert_eq!((img.w, img.h), (48, 48));
        let cpu = rasterise_job_scaled(&j, (16, 16), 3);
        for y in 0..48 {
            for x in 0..48 {
                let (rgb, a) = img.pixel(x, y);
                let c = cpu[y * 48 + x];
                if c == NONE {
                    assert_eq!(a, 0, "({x},{y}) should be empty");
                } else {
                    assert_eq!(a, 255, "({x},{y}) should be covered");
                    assert_eq!(Some(rgb), colour(c), "({x},{y}) colour");
                }
            }
        }
    }

    /// The game's own rule is the 1x raster. Drawn at 3x with no MSAA, a
    /// native pixel is "covered" when most of its 3x3 block is; that must
    /// agree with the 1x raster except on pixels the edge only grazes. Run
    /// on shapes with steep and shallow edges, with and without the
    /// centre-sampling and floor corrections the edges apply.
    fn mismatch_with_1x(s: &SuperFxGpu, g: &GpuContext, corners: &[(u8, u8)]) -> (usize, usize) {
        use crate::gsu_poly::rasterise_job_1x;
        let j = job(vec![poly(corners, 5, 0)], 4);
        let img = s.render(g, &j, (16, 16), 3, 1, &colour).expect("render");
        let one = rasterise_job_1x(&j, (16, 16));
        let (mut l1, mut covered, mut area) = (0usize, 0usize, 0usize);
        for ny in 0..16 {
            for nx in 0..16 {
                let n = (0..9)
                    .filter(|k| img.pixel(nx * 3 + k % 3, ny * 3 + k / 3).1 == 255)
                    .count();
                let want = one[ny * 16 + nx] != NONE;
                covered += usize::from(want);
                area += n;
                l1 += if want { 9 - n } else { n };
            }
        }
        println!(
            "  area {:.1} vs {covered}; L1 {:.1}",
            area as f32 / 9.0,
            l1 as f32 / 9.0
        );
        (l1 / 9, covered)
    }

    #[test]
    fn slanted_polygons_agree_with_the_1x_rule_per_native_pixel() {
        let Some((g, s)) = gpu() else { return };
        for (name, corners) in [
            ("shallow", vec![(5u8, 0u8), (15, 9), (0, 12)]),
            ("steep", vec![(8, 0), (11, 15), (3, 14)]),
            ("quad", vec![(2, 1), (13, 3), (11, 14), (1, 10)]),
            ("anticlockwise", vec![(5, 0), (0, 12), (15, 9)]),
        ] {
            let (diff, covered) = mismatch_with_1x(&s, &g, &corners);
            println!("{name}: {diff} of {covered} (L1, in native pixels) differ from the 1x rule");
            assert!(diff * 6 < covered, "{name}: {diff} of {covered}");
        }
    }

    #[test]
    fn msaa_4_gives_partial_coverage_on_a_slanted_edge() {
        let Some((g, s)) = gpu() else { return };
        println!("supported sample counts: {:?}", s.supported_samples());
        if !s.supported_samples().contains(&4) {
            println!("SKIP: 4x MSAA unsupported");
            return;
        }
        let j = job(vec![poly(&[(4, 0), (12, 8), (0, 8)], 5, 0)], 4);
        let one = s.render(&g, &j, (16, 16), 3, 1, &colour).expect("1x");
        let four = s.render(&g, &j, (16, 16), 3, 4, &colour).expect("4x");
        let partial = |i: &GpuPolyImage| {
            (0..i.h)
                .flat_map(|y| (0..i.w).map(move |x| (x, y)))
                .filter(|&(x, y)| matches!(i.pixel(x, y).1, 1..=254))
                .count()
        };
        assert_eq!(partial(&one), 0, "no MSAA: coverage is all or nothing");
        assert!(partial(&four) > 10, "partial pixels: {}", partial(&four));
        // Partial pixels carry the polygon's own colour (straight alpha).
        for y in 0..four.h {
            for x in 0..four.w {
                let (rgb, a) = four.pixel(x, y);
                if (1..=254).contains(&a) {
                    let want = colour(5).unwrap();
                    for k in 0..3 {
                        assert!(
                            (i32::from(rgb[k]) - i32::from(want[k])).abs() <= 2,
                            "({x},{y}) a={a} {rgb:?} vs {want:?}"
                        );
                    }
                }
            }
        }
        // Unsupported counts fall back to the largest supported one.
        let eff = s.effective_samples(8);
        assert!(eff == 4 || eff == 8);
    }

    #[test]
    fn one_pixel_lines_are_continuous() {
        let Some((g, s)) = gpu() else { return };
        // A shallow and a steep line, as the game's degenerate panel edges.
        let j = job(
            vec![
                poly(&[(2, 3), (14, 5), (2, 3)], 4, 0),
                poly(&[(5, 8), (5, 15)], 6, 0),
            ],
            4,
        );
        let img = s.render(&g, &j, (16, 16), 3, 1, &colour).expect("render");
        // Shallow: every column from x = 6 to 42 has covered pixels, and
        // adjacent columns' covered rows touch or overlap (no gap).
        let col_rows = |x: usize, y0: usize, y1: usize| -> Vec<usize> {
            (y0..y1).filter(|&y| img.pixel(x, y).1 == 255).collect()
        };
        let mut prev: Option<(usize, usize)> = None;
        for x in 6..42 {
            let rows = col_rows(x, 0, 24);
            assert!(!rows.is_empty(), "gap in the shallow line at x={x}");
            let (lo, hi) = (rows[0], rows[rows.len() - 1]);
            if let Some((plo, phi)) = prev {
                assert!(lo <= phi + 1 && plo <= hi + 1, "jump at x={x}");
            }
            prev = Some((lo, hi));
        }
        // Steep: native column 5 -> scaled 15..18, rows 24..45 solid.
        for y in 24..45 {
            for x in 15..18 {
                assert_eq!(img.pixel(x, y).1, 255, "steep line hole at ({x},{y})");
            }
        }
    }

    #[test]
    fn dither_parity_matches_plot_colour_at_native_pixel_granularity() {
        let Some((g, s)) = gpu() else { return };
        let j = job(
            vec![
                // 0x12 dithered: even parity -> 2, odd parity -> 1.
                poly(&[(0, 0), (6, 0), (6, 4), (0, 4)], 0x12, 2),
                // High nibble 0 on odd parity: transparent there.
                poly(&[(8, 0), (12, 0), (12, 3), (8, 3)], 0x04, 2),
                PolyOp::Plot {
                    x: 3,
                    y: 7,
                    colr: 0x93,
                    por: 2,
                },
            ],
            4,
        );
        let img = s.render(&g, &j, (16, 16), 3, 1, &colour).expect("render");
        let cpu = rasterise_job_scaled(&j, (16, 16), 3);
        let mut checked = 0;
        for y in 0..48 {
            for x in 0..48 {
                let (rgb, a) = img.pixel(x, y);
                let c = cpu[y * 48 + x];
                if c == NONE {
                    assert_eq!(a, 0, "({x},{y})");
                } else {
                    assert_eq!(a, 255, "({x},{y})");
                    assert_eq!(Some(rgb), colour(c), "({x},{y}) index {c}");
                    // And the index is what the rule gives for the native pixel.
                    let want = plot_colour(
                        match (x / 3, y / 3) {
                            (8..=12, 0..=2) => 0x04,
                            (3, 7) => 0x93,
                            _ => 0x12,
                        },
                        2,
                        4,
                        (x / 3) as u8,
                        (y / 3) as u8,
                    );
                    assert_eq!(want.map(|w| w & 0x0F), Some(c));
                    checked += 1;
                }
            }
        }
        assert!(checked > 300, "{checked}");
    }

    #[test]
    fn painter_order_and_an_unmapped_index() {
        let Some((g, s)) = gpu() else { return };
        let j = job(
            vec![
                poly(&[(0, 0), (8, 0), (8, 8), (0, 8)], 3, 0),
                poly(&[(2, 2), (4, 2), (4, 4), (2, 4)], 7, 0),
                // index 9 has no colour: draws nothing.
                poly(&[(10, 10), (14, 10), (14, 14), (10, 14)], 9, 0),
            ],
            4,
        );
        let map = |i: u8| (i != 9).then(|| colour(i).unwrap());
        let img = s.render(&g, &j, (16, 16), 2, 1, &map).expect("render");
        assert_eq!(img.pixel(5, 5), (colour(7).unwrap(), 255));
        assert_eq!(img.pixel(1, 1), (colour(3).unwrap(), 255));
        assert_eq!(img.pixel(22, 22).1, 0);
        // An empty job is an empty (transparent) image, not an error.
        let empty = s
            .render(&g, &job(Vec::new(), 4), (16, 16), 2, 1, &map)
            .expect("empty");
        assert!(empty.rgba.iter().all(|&b| b == 0));
        assert!(s.render(&g, &j, (0, 0), 2, 1, &map).is_err());
    }
}
