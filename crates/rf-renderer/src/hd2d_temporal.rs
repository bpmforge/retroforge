//! The HD-2D frame at a low internal resolution, upscaled to 5K by a temporal
//! upscaler (ticket W39-05): jittered colour from the Diorama pass, depth and
//! motion from [`crate::diorama_gbuffer`], accumulation by
//! [`crate::temporal::TemporalUpscaler`] (MetalFX, or the portable WGSL
//! resolve when MetalFX is not available).
//!
//! The scene, camera, sun and depth of field are exactly [`crate::hd2d`]'s:
//! [`crate::hd2d::plan`] is called with the *internal* size (same aspect, so
//! the same view-projection), the sun's shadow map is fitted over the
//! unjittered matrix so the jitter cannot make shadows crawl, and the lens
//! runs at the internal size before the upscale. Presentation over `&` data,
//! like [`crate::hd2d`]: it reads no core memory, so Original mode is not
//! involved.

use crate::diorama::camera_matrices::Mat4;
use crate::diorama::DioramaPass;
use crate::diorama_gbuffer::{GBufferInput, GBufferPass};
use crate::diorama_mesh::Vertex;
use crate::gpu::GpuContext;
use crate::hd2d::Hd2dStage;
use crate::hd2d::{dof_params, fill_holes, lighting_for, plan, Hd2dLook, Hd2dMarker, Hd2dPlan};
use crate::lens::LensPass;
use crate::temporal::{
    jitter_for_frame, jitter_vp, TemporalBackend, TemporalInputs, TemporalUpscaler,
};

/// What one temporal frame reports.
#[derive(Debug, Clone)]
pub struct Hd2dTemporalFrame {
    pub plan: Hd2dPlan,
    /// The jitter this frame was drawn with, input pixels.
    pub jitter: (f32, f32),
    /// Whether history was discarded for this frame.
    pub reset: bool,
}

/// The renderer: owns the G-buffer pass, the upscaler and the history.
pub struct Hd2dTemporal {
    gbuf: GBufferPass,
    upscaler: TemporalUpscaler,
    color: wgpu::Texture,
    internal: (u32, u32),
    frame: u32,
    prev: Option<(Mat4, Vec<Vertex>)>,
    reset_next: bool,
}

impl Hd2dTemporal {
    /// An HD-2D temporal renderer drawing at `internal` and producing
    /// `output`. `prefer_metalfx` asks for MetalFX; see
    /// [`TemporalUpscaler::new`] for when that falls back.
    #[must_use]
    pub fn new(
        gpu: &GpuContext,
        internal: (u32, u32),
        output: (u32, u32),
        prefer_metalfx: bool,
    ) -> Self {
        let color = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rf-renderer::hd2d_temporal::color"),
            size: wgpu::Extent3d {
                width: internal.0,
                height: internal.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Self {
            gbuf: GBufferPass::new(gpu),
            upscaler: TemporalUpscaler::new(gpu, internal, output, prefer_metalfx),
            color,
            internal,
            frame: 0,
            prev: None,
            reset_next: true,
        }
    }

    /// The upscaler (back end, output texture, readback).
    #[must_use]
    pub fn upscaler(&self) -> &TemporalUpscaler {
        &self.upscaler
    }

    /// The upscaler, mutably (back-end knobs for tests).
    pub fn upscaler_mut(&mut self) -> &mut TemporalUpscaler {
        &mut self.upscaler
    }

    /// Which back end runs.
    #[must_use]
    pub fn backend(&self) -> TemporalBackend {
        self.upscaler.backend()
    }

    /// Drop the history on the next frame (a camera cut, a level change).
    pub fn reset(&mut self) {
        self.reset_next = true;
    }

    /// Frames drawn so far.
    #[must_use]
    pub fn frame_index(&self) -> u32 {
        self.frame
    }

    /// Draw and accumulate one frame. The upscaled image is
    /// `self.upscaler().output()`; `look.out` is ignored (the internal and
    /// output sizes were fixed at construction).
    ///
    /// # Errors
    /// A GPU readback failure, or a back end failure.
    pub fn render(
        &mut self,
        gpu: &GpuContext,
        pass: &DioramaPass,
        lens: &LensPass,
        stage: &Hd2dStage<'_>,
        markers: &[Hd2dMarker],
        look: &Hd2dLook,
    ) -> Result<Hd2dTemporalFrame, String> {
        let (iw, ih) = self.internal;
        let internal_look = Hd2dLook {
            out: self.internal,
            ..*look
        };
        let plan = plan(stage, markers, &internal_look);
        let jitter = jitter_for_frame(self.frame);
        let vp_jit = jitter_vp(plan.vp, jitter, self.internal);
        let ground = fill_holes(stage.picture_rgba, stage.size.0, stage.size.1);

        // Colour: the ordinary Diorama pass, jittered, shadow map fitted over
        // the unjittered matrix.
        pass.set_lighting(lighting_for(look));
        pass.set_shadow_fit_vp(Some(plan.vp));
        let params = dof_params(look, plan.focus_frac);
        let on_gpu = !params.any_active();
        pass.set_texture_only(on_gpu);
        let drawn = pass.render_with_vp(
            gpu,
            &plan.vertices,
            &ground,
            stage.size.0,
            stage.size.1,
            stage.sprite_rgba,
            stage.size.0,
            stage.size.1,
            vp_jit,
            iw,
            ih,
        );
        pass.set_texture_only(false);
        pass.set_shadow_fit_vp(None);
        let rgba = drawn?;
        let extent = wgpu::Extent3d {
            width: iw,
            height: ih,
            depth_or_array_layers: 1,
        };
        if on_gpu {
            let (resolved, size) = pass
                .resolved_texture()
                .ok_or_else(|| "diorama pass produced no frame".to_string())?;
            if size != self.internal {
                return Err(format!(
                    "diorama frame is {size:?}, expected {:?}",
                    self.internal
                ));
            }
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("rf-renderer::hd2d_temporal::copy"),
                });
            encoder.copy_texture_to_texture(
                resolved.as_image_copy(),
                self.color.as_image_copy(),
                extent,
            );
            gpu.queue.submit(std::iter::once(encoder.finish()));
        } else {
            // The lens works on CPU pixels; it returns straight alpha, the
            // upscaler wants colour premultiplied by coverage (which is what
            // the GPU path hands over), so premultiply.
            let mut out = lens.render(gpu, &rgba, iw, ih, None, &params)?;
            for px in out.chunks_exact_mut(4) {
                let a = u32::from(px[3]);
                for c in &mut px[..3] {
                    *c = ((u32::from(*c) * a + 127) / 255) as u8;
                }
            }
            gpu.queue.write_texture(
                self.color.as_image_copy(),
                &out,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(iw * 4),
                    rows_per_image: Some(ih),
                },
                extent,
            );
        }

        // Depth and motion.
        let (vp_prev, prev_vertices) = match &self.prev {
            Some((m, v)) => (*m, Some(v.as_slice())),
            None => (plan.vp, None),
        };
        let gb = self.gbuf.render(
            gpu,
            &GBufferInput {
                vertices: &plan.vertices,
                prev_vertices,
                ground_rgba: &ground,
                ground_size: stage.size,
                sprite_rgba: stage.sprite_rgba,
                sprite_size: stage.size,
                vp_jittered: vp_jit,
                vp_current: plan.vp,
                vp_previous: vp_prev,
                size: self.internal,
            },
        )?;

        let reset = self.reset_next || self.prev.is_none();
        self.upscaler.upscale(
            gpu,
            &TemporalInputs {
                color: &self.color,
                depth: &gb.depth,
                motion: &gb.motion,
                jitter,
                reset,
            },
        )?;
        self.reset_next = false;
        self.frame = self.frame.wrapping_add(1);
        self.prev = Some((plan.vp, plan.vertices.clone()));
        Ok(Hd2dTemporalFrame {
            plan,
            jitter,
            reset,
        })
    }

    /// The depth and motion images of the last frame (tests read the motion
    /// back).
    #[must_use]
    pub fn last_gbuffer(&self) -> Option<crate::diorama_gbuffer::GBuffer> {
        self.gbuf.last()
    }
}
