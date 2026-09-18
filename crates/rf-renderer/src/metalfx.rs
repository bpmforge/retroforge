//! MetalFX scaler path (ticket W16-08; `docs/design/ENHANCEMENT_WAVE_16.md`
//! §7 Path B, §8; builds on W16-01's synchronous timing spike in
//! `tests/metalfx_bench.rs`).
//!
//! This is a **scaler**, not a content enhancement: it upscales the pixels
//! the core already produced, after the palette pass, the same way `nearest`
//! /`xbr`/`crt` in [`crate::shader_chain`] do (CLAUDE.md law 6 — Accuracy
//! Mode may use it because it never touches simulation state). It therefore
//! lives in Settings > Video next to the other scalers, not on the
//! enhancement ladder (`rf-enhance`'s trust ladder is for things that alter
//! or invent picture content).
//!
//! ## Availability logic
//!
//! [`MetalFxAvailability`]/[`availability_from`] are plain data + a pure
//! function, deliberately kept independent of `cfg(feature = "metalfx")` /
//! `cfg(target_os = "macos")` / any real Metal call so the four cases
//! (feature off, non-macOS, unsupported device, supported) are unit-testable
//! on every platform this workspace builds on, not just a Mac with the
//! `metalfx` feature turned on. [`detect`] is the real, environment-reading
//! entry point Settings > Video calls; it is a thin wrapper that plugs the
//! real `cfg!`/device-query answers into [`availability_from`].
//!
//! ## Interop and synchronisation design (acceptance criterion 1)
//!
//! [`MetalFxScaler`] (only compiled `cfg(all(feature = "metalfx",
//! target_os = "macos"))`) owns two double-buffered pairs of **wgpu**
//! textures (input, output) rather than raw Metal ones — the rest of the
//! renderer stays in wgpu terms; only this module reaches into
//! `wgpu::hal::api::Metal` to pull out the raw `MTLTexture`/`MTLDevice`/
//! `MTLCommandQueue` objects MetalFX itself needs:
//!
//! - `wgpu::Device::as_hal::<wgpu::hal::api::Metal>()` → `wgpu_hal::metal::
//!   Device::raw_device()` for the `MTLDevice` the scaler is created against.
//! - `wgpu::Queue::as_hal::<wgpu::hal::api::Metal>()` → `wgpu_hal::metal::
//!   Queue::as_raw()` for the **same** `MTLCommandQueue` wgpu itself submits
//!   work to — critical: this module creates no command queue of its own.
//! - `wgpu::Texture::as_hal::<wgpu::hal::api::Metal>()` → `wgpu_hal::metal::
//!   Texture::raw_handle()` for the `MTLTexture` backing each wgpu texture,
//!   handed straight to `MTLFXSpatialScaler::setColorTexture`/
//!   `setOutputTexture` — no CPU round trip, no second texture allocation.
//!
//! Texture usages were checked against `wgpu-hal-29.0.4/src/metal/conv.rs`'s
//! `map_texture_usage` (law 2 — verified against source, not guessed): the
//! input texture is created with `TEXTURE_BINDING` (`wgt::TextureUses::
//! RESOURCE` → `MTLTextureUsage::ShaderRead`, what MetalFX documents it
//! needs for the color texture); the output texture is created with
//! `STORAGE_BINDING | RENDER_ATTACHMENT | TEXTURE_BINDING` (→ `ShaderWrite |
//! RenderTarget | ShaderRead`, what MetalFX documents for its output plus
//! `ShaderRead` so the present/blit stage downstream can sample it as an
//! ordinary wgpu texture with no extra copy).
//!
//! **Ordering, without a CPU stall.** A Metal command queue executes the
//! command buffers committed to it in commit order (Apple's documented
//! queue contract). Because [`MetalFxScaler`] shares wgpu's own
//! `MTLCommandQueue` rather than opening a second one, the rule is simply:
//! commit the scaler's command buffer only *after* the `wgpu::Queue::submit`
//! call that renders into [`MetalFxScaler::input_texture`] has returned, and
//! only submit the present/blit pass that reads
//! [`MetalFxScaler::current_output_texture`] *after* [`MetalFxScaler::
//! scale`] has committed. Each of those three command buffers — wgpu's
//! render, MetalFX's scale, wgpu's present — lands on the same queue in
//! that order, so the GPU serialises the read-after-write hazard for us;
//! nothing here calls `waitUntilCompleted`, unlike the W16-01 spike. Double-
//! buffering both the input and output textures (`current`/`1 - current`
//! flipped every [`MetalFxScaler::scale`] call) is what makes that safe to
//! do *without* a CPU wait: frame N's scale command buffer can still be
//! in-flight on the GPU when frame N+1 starts writing its input texture,
//! because N+1 writes into the *other* slot. `MTLCommandQueue` itself caps
//! how many uncompleted command buffers it will accept
//! (`maxCommandBufferCount`), which backpressures the CPU if the GPU ever
//! falls more than a couple of frames behind — this module relies on that
//! built-in limit rather than reimplementing a semaphore, since two textures
//! at emulator-frame resolution is a fixed, small memory cost either way.
//!
//! ## Temporal mode (acceptance criterion 3)
//!
//! Attempted in `tests/metalfx_bench.rs`'s `metalfx_temporal_upscale_attempt`
//! (macOS + `metalfx`, `#[ignore]`d, needs a real device): `MTLFXTemporalScaler`
//! requires a motion-vector texture and a depth texture per frame. RetroForge
//! is a 2D emulator core with no motion vectors or depth buffer, so the
//! attempt supplies a zero-filled motion texture (`RG16Float`, "no motion")
//! and a constant depth texture (`R32Float`, all `1.0`, "everything at the
//! far plane") and records whether `MTLFXTemporalScalerDescriptor::
//! supportsDevice` / `newTemporalScalerWithDevice` accept that and what the
//! output looks like. **This module does not ship a production
//! `MetalFxTemporalScaler` type** — see that test's doc comment for the
//! measured outcome and the blocker (if any); `retroforge::settings::
//! MetalFxSetting::Temporal` exists as a settings-file-compatible variant
//! but the Settings > Video UI never offers it, for the same reason.

/// Why MetalFX spatial upscaling is or is not offered right now. Kept as
/// plain data (no GPU/OS query inside this type) so [`availability_from`]
/// is unit-testable on any platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetalFxAvailability {
    /// Feature-gated at compile time — this build has no MetalFX code at
    /// all (`cargo build` without `--features metalfx`).
    FeatureDisabled,
    /// This build has the `metalfx` code, but the running OS is not macOS
    /// (MetalFX is an Apple-only framework).
    NotMacOs,
    /// macOS, `metalfx` feature compiled in, but this specific device
    /// answered `false` to `MTLFXSpatialScalerDescriptor::supportsDevice`
    /// (or no device query could be performed at all — treated the same
    /// way: conservatively unsupported rather than assumed working).
    UnsupportedDevice,
    /// macOS, `metalfx` feature compiled in, device supports it.
    Available,
}

impl MetalFxAvailability {
    /// `true` only for [`MetalFxAvailability::Available`] — the single
    /// condition under which Settings > Video should let the user pick
    /// "MetalFX spatial".
    #[must_use]
    pub fn is_available(self) -> bool {
        matches!(self, MetalFxAvailability::Available)
    }

    /// User-facing reason string for the disabled cases (acceptance
    /// criterion 1: "shown disabled with the reason"); `None` when
    /// available (nothing to disclose).
    #[must_use]
    pub fn reason(self) -> Option<&'static str> {
        match self {
            MetalFxAvailability::FeatureDisabled => {
                Some("MetalFX support was not built into this copy of RetroForge")
            }
            MetalFxAvailability::NotMacOs => Some("MetalFX is only available on macOS"),
            MetalFxAvailability::UnsupportedDevice => {
                Some("This GPU does not support MetalFX (supportsDevice)")
            }
            MetalFxAvailability::Available => None,
        }
    }
}

/// Pure decision function behind [`detect`] — `feature_enabled`/`is_macos`
/// are the compile-time facts, `device_supported` is the runtime
/// `supportsDevice` answer (`None` when it could not even be queried, e.g.
/// no default Metal device). Exhaustively covers the four cases the
/// acceptance criteria ask to be unit-tested.
#[must_use]
pub fn availability_from(
    feature_enabled: bool,
    is_macos: bool,
    device_supported: Option<bool>,
) -> MetalFxAvailability {
    if !feature_enabled {
        return MetalFxAvailability::FeatureDisabled;
    }
    if !is_macos {
        return MetalFxAvailability::NotMacOs;
    }
    match device_supported {
        Some(true) => MetalFxAvailability::Available,
        Some(false) | None => MetalFxAvailability::UnsupportedDevice,
    }
}

/// Real entry point: plugs the actual `cfg!`/device-query facts into
/// [`availability_from`]. Callable on every platform — it simply cannot
/// ever return [`MetalFxAvailability::Available`] unless both the feature
/// and macOS are true and a real device says yes.
#[must_use]
pub fn detect() -> MetalFxAvailability {
    let feature_enabled = cfg!(feature = "metalfx");
    let is_macos = cfg!(target_os = "macos");
    #[cfg(all(feature = "metalfx", target_os = "macos"))]
    let device_supported = apple::query_spatial_device_support();
    #[cfg(not(all(feature = "metalfx", target_os = "macos")))]
    let device_supported: Option<bool> = None;
    availability_from(feature_enabled, is_macos, device_supported)
}

#[cfg(all(feature = "metalfx", target_os = "macos"))]
pub use apple::{MetalFxError, MetalFxScaler};

#[cfg(all(feature = "metalfx", target_os = "macos"))]
mod apple {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::Message;
    use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLDevice, MTLPixelFormat};
    use objc2_metal_fx::{
        MTLFXSpatialScaler, MTLFXSpatialScalerBase, MTLFXSpatialScalerDescriptor,
    };

    use crate::gpu::GpuContext;

    /// Why [`MetalFxScaler::new`] or [`MetalFxScaler::scale`] failed. A
    /// `String` payload rather than a richer enum -- every failure here is
    /// "some Metal/MetalFX call returned nil/false", reported once at
    /// construction so the caller can fall back to a plain shader-chain
    /// scaler, not a condition callers branch on case-by-case.
    #[derive(Debug, Clone)]
    pub struct MetalFxError(pub String);

    impl std::fmt::Display for MetalFxError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "MetalFX: {}", self.0)
        }
    }

    impl std::error::Error for MetalFxError {}

    /// `true` if a default Metal device exists and reports
    /// `MTLFXSpatialScalerDescriptor::supportsDevice`. `None`-mapped to
    /// `false` by [`super::detect`] -- see that fn's doc.
    ///
    /// # Safety note
    /// `MTLCreateSystemDefaultDevice` and `supportsDevice` are both pure
    /// queries with no preconditions (same reasoning as
    /// `tests/metalfx_bench.rs`'s own use of them).
    #[allow(unsafe_code)]
    pub(super) fn query_spatial_device_support() -> Option<bool> {
        // SAFETY: pure system query, documented to return `None`/NULL or a
        // valid retained device; no preconditions.
        let device = objc2_metal::MTLCreateSystemDefaultDevice()?;
        // SAFETY: `supportsDevice` accepts any valid `MTLDevice` reference.
        Some(unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(&device) })
    }

    /// Writes a full-extent zero buffer into `tex` through wgpu's own
    /// `Queue::write_texture` so wgpu-core marks every subresource
    /// "initialized" -- see the call site's doc in [`MetalFxScaler::new`]
    /// for why a texture whose real writes all happen via the raw Metal
    /// handle needs this once, up front.
    fn mark_initialized(queue: &wgpu::Queue, tex: &wgpu::Texture, w: u32, h: u32) {
        let zeros = vec![0u8; (w * h * 4) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &zeros,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Double-buffered MetalFX spatial upscaler sharing wgpu's own Metal
    /// device/queue (module doc: interop and synchronisation design).
    pub struct MetalFxScaler {
        queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
        scaler: Retained<ProtocolObject<dyn MTLFXSpatialScaler>>,
        input: [wgpu::Texture; 2],
        output: [wgpu::Texture; 2],
        /// Slot [`Self::input_texture`] currently points to -- the caller
        /// writes here, then [`Self::scale`] reads it. Flipped at the end
        /// of [`Self::scale`], so the *next* frame's write lands in the
        /// other slot while this frame's output (see `produced`) may still
        /// be in flight on the GPU.
        current: usize,
        /// Slot [`Self::current_output_texture`] points to -- the pairing
        /// [`Self::scale`] most recently produced. Deliberately a
        /// *separate* field from `current`: once `current` flips (above),
        /// `current` and `produced` refer to different slots until the
        /// next `scale()` call brings them back in sync. Reading output
        /// through `current` instead of a dedicated `produced` field was
        /// this module's first (wrong) attempt -- it returned the *other*
        /// slot's stale contents (still whatever `mark_initialized` wrote)
        /// once `current` had already flipped, caught by this crate's own
        /// `metalfx_scaler_output_has_right_size_and_is_not_uniform` test.
        produced: usize,
        input_size: (u32, u32),
        output_size: (u32, u32),
        /// The command buffer [`Self::scale`] most recently committed.
        /// `None` before the first call. Not used by the steady-state
        /// pipeline itself (which never waits, module doc) -- exists so
        /// [`Self::wait_idle`] can give a *correctness* caller (a test, or
        /// a one-shot preview) a real synchronisation point, since a
        /// generic `wgpu::Device::poll` cannot see completion of a command
        /// buffer this module committed directly to the raw Metal queue
        /// (wgpu-core's poll only tracks submissions made through wgpu's
        /// own `Queue::submit`).
        last_command_buffer: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
    }

    #[allow(unsafe_code)]
    impl MetalFxScaler {
        /// Build a scaler for `input_w x input_h -> input_w*scale x
        /// input_h*scale`. `gpu.device`/`gpu.queue` must be a Metal-backed
        /// wgpu device (i.e. this process actually selected the Metal
        /// backend) -- callers check [`super::detect`]`().is_available()`
        /// first, which already implies macOS, but a multi-backend build
        /// (e.g. `WGPU_BACKEND=gl` forced for testing) could still fail the
        /// `as_hal` downcast below, reported as [`MetalFxError`] rather than
        /// panicking.
        pub fn new(
            gpu: &GpuContext,
            input_w: u32,
            input_h: u32,
            scale: u32,
        ) -> Result<Self, MetalFxError> {
            let output_w = input_w * scale;
            let output_h = input_h * scale;

            // SAFETY: `as_hal`'s own contract (wgpu 29 doc) -- the returned
            // guard is dropped immediately after we copy out an owned
            // `Retained` handle (`.clone()`/`raw_device().clone()` below),
            // so nothing here outlives the guard or is used after the
            // device/queue are destroyed while still in use by the GPU.
            let device: Retained<ProtocolObject<dyn MTLDevice>> = unsafe {
                gpu.device
                    .as_hal::<wgpu::hal::api::Metal>()
                    .ok_or_else(|| {
                        MetalFxError("wgpu device is not backed by the Metal API".to_string())
                    })?
                    .raw_device()
                    .clone()
            };
            // SAFETY: same `as_hal` contract as the device above; `retain`
            // (objc2's `Message::retain`) on a live `ProtocolObject`
            // reference is always sound -- it only bumps the Objective-C
            // retain count so this module can hold its own owned handle
            // past the `as_hal` guard's lifetime.
            let queue: Retained<ProtocolObject<dyn MTLCommandQueue>> = unsafe {
                gpu.queue
                    .as_hal::<wgpu::hal::api::Metal>()
                    .ok_or_else(|| {
                        MetalFxError("wgpu queue is not backed by the Metal API".to_string())
                    })?
                    .as_raw()
                    .retain()
            };

            // SAFETY: `supportsDevice` has no precondition beyond a valid
            // device.
            if !unsafe { MTLFXSpatialScalerDescriptor::supportsDevice(&device) } {
                return Err(MetalFxError(
                    "MTLFXSpatialScalerDescriptor::supportsDevice returned false".to_string(),
                ));
            }

            // SAFETY: descriptor setters have no precondition beyond
            // in-range values, which these are (positive dimensions from a
            // caller-supplied frame size).
            let descriptor = unsafe {
                let d = MTLFXSpatialScalerDescriptor::new();
                d.setColorTextureFormat(MTLPixelFormat::RGBA8Unorm);
                d.setOutputTextureFormat(MTLPixelFormat::RGBA8Unorm);
                d.setInputWidth(input_w as usize);
                d.setInputHeight(input_h as usize);
                d.setOutputWidth(output_w as usize);
                d.setOutputHeight(output_h as usize);
                d
            };
            // SAFETY: `newSpatialScalerWithDevice` needs only a valid
            // device; the descriptor's fields were just set to in-range
            // values above.
            let scaler =
                unsafe { descriptor.newSpatialScalerWithDevice(&device) }.ok_or_else(|| {
                    MetalFxError("newSpatialScalerWithDevice returned nil".to_string())
                })?;

            let make_input = || {
                gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("metalfx-input"),
                    size: wgpu::Extent3d {
                        width: input_w,
                        height: input_h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    // TEXTURE_BINDING (-> ShaderRead, module doc: verified
                    // against wgpu-hal's map_texture_usage) is what MetalFX
                    // itself requires of the color texture. RENDER_ATTACHMENT
                    // lets the upstream pipeline stage render the
                    // post-palette RGBA directly into this texture via a
                    // wgpu render pass; COPY_DST lets it instead be filled
                    // by `queue.write_texture`/`copy_texture_to_texture`
                    // (what this crate's own bench/tests use) -- both are
                    // ordinary write paths a caller may prefer, neither
                    // changes the Metal-side usage bits MetalFX cares about
                    // (COPY_DST has no Metal-usage-bit equivalent in
                    // `map_texture_usage`, so it costs nothing there; wgpu
                    // still places this in Private storage mode since no
                    // MAP_READ/MAP_WRITE usage is requested, matching the
                    // W16-01 spike's hand-rolled descriptor).
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            };
            let make_output = || {
                gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("metalfx-output"),
                    size: wgpu::Extent3d {
                        width: output_w,
                        height: output_h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    // ShaderWrite | RenderTarget (what MetalFX's own doc
                    // requires of the output texture) | ShaderRead (so the
                    // present/blit stage can sample this directly with no
                    // extra copy) -- module doc's conv.rs mapping.
                    // COPY_SRC lets a caller/test read the result back;
                    // COPY_DST is used exactly once per texture, right
                    // after creation (see the `mark_initialized` call in
                    // `Self::new` below) -- not for per-frame writes.
                    usage: wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            };

            let output = [make_output(), make_output()];
            // wgpu-core lazily zero-clears any texture subresource it has
            // never itself written before the first wgpu-side access to it
            // (its "uninitialized" tracking, independent per subresource) --
            // this scaler's per-frame writes into `output` happen entirely
            // through the raw Metal handle (`Self::scale`, via `as_hal`),
            // which wgpu-core cannot see. Left alone, the *first* wgpu-side
            // read of an output texture (e.g. a caller's present/blit pass,
            // or this crate's own tests reading it back) would silently
            // stomp MetalFX's real result with wgpu's own zero-clear right
            // before the read. A one-time, wgpu-tracked write over the full
            // extent right here marks each subresource "initialized" from
            // wgpu-core's point of view, which is sticky for the texture's
            // lifetime -- no further extra writes needed on later frames.
            for tex in &output {
                mark_initialized(&gpu.queue, tex, output_w, output_h);
            }

            Ok(MetalFxScaler {
                queue,
                scaler,
                input: [make_input(), make_input()],
                output,
                current: 0,
                produced: 0,
                input_size: (input_w, input_h),
                output_size: (output_w, output_h),
                last_command_buffer: None,
            })
        }

        /// The wgpu texture the caller should render/copy this frame's
        /// post-palette RGBA into, *before* calling [`Self::scale`].
        #[must_use]
        pub fn input_texture(&self) -> &wgpu::Texture {
            &self.input[self.current]
        }

        #[must_use]
        pub fn input_size(&self) -> (u32, u32) {
            self.input_size
        }

        #[must_use]
        pub fn output_size(&self) -> (u32, u32) {
            self.output_size
        }

        /// The most recently produced output texture (valid to sample only
        /// after the command buffer [`Self::scale`] committed has actually
        /// run -- guaranteed by queue ordering per the module doc once the
        /// caller's present/blit pass is itself submitted after this call
        /// returns, not by any wait performed here).
        #[must_use]
        pub fn current_output_texture(&self) -> &wgpu::Texture {
            &self.output[self.produced]
        }

        /// Encode and commit one MetalFX spatial-scale pass reading
        /// [`Self::input_texture`] and writing the texture
        /// [`Self::current_output_texture`] will return afterwards, then
        /// flip the double-buffer index. Does **not** wait for completion
        /// -- see the module doc's ordering argument for why that is safe.
        ///
        /// # Errors
        /// Returns [`MetalFxError`] if pulling the raw `MTLTexture` out of
        /// either wgpu texture fails (would mean the texture came from a
        /// non-Metal backend, or was already destroyed) or if
        /// `commandBuffer()` returns nil (queue exhausted/invalid).
        pub fn scale(&mut self) -> Result<(), MetalFxError> {
            let input_tex = &self.input[self.current];
            let output_tex = &self.output[self.current];

            // SAFETY: `as_hal`'s contract -- the guards are held only for
            // the duration of this block, during which the textures are
            // alive (owned by `self`) and not concurrently destroyed.
            let input_hal =
                unsafe { input_tex.as_hal::<wgpu::hal::api::Metal>() }.ok_or_else(|| {
                    MetalFxError("input wgpu texture is not backed by the Metal API".to_string())
                })?;
            // SAFETY: same reasoning as `input_hal` above.
            let output_hal =
                unsafe { output_tex.as_hal::<wgpu::hal::api::Metal>() }.ok_or_else(|| {
                    MetalFxError("output wgpu texture is not backed by the Metal API".to_string())
                })?;
            let input_raw = input_hal.raw_handle();
            let output_raw = output_hal.raw_handle();

            // SAFETY: both textures are kept alive by `self` for the whole
            // call (the setters' "must be kept alive while in use"
            // contract); matching color/output formats and dimensions were
            // fixed at construction time in `Self::new`.
            unsafe {
                self.scaler.setColorTexture(Some(input_raw));
                self.scaler.setOutputTexture(Some(output_raw));
            }
            drop(input_hal);
            drop(output_hal);

            let cmd_buffer = self
                .queue
                .commandBuffer()
                .ok_or_else(|| MetalFxError("commandBuffer() returned nil".to_string()))?;
            // SAFETY: `encodeToCommandBuffer` requires an open, uncommitted
            // command buffer (just created above) and a scaler with its
            // color/output textures already set (just done above).
            unsafe { self.scaler.encodeToCommandBuffer(&cmd_buffer) };
            cmd_buffer.commit();
            // Deliberately no `waitUntilCompleted()` here -- module doc's
            // "ordering, without a CPU stall" explains why committing to
            // wgpu's own shared queue is sufficient for production frame
            // pacing. The handle is kept so a correctness caller can still
            // opt into a real wait via `Self::wait_idle`.
            self.last_command_buffer = Some(cmd_buffer);

            self.produced = self.current;
            self.current = 1 - self.current;
            Ok(())
        }

        /// Blocks until the command buffer from the most recent
        /// [`Self::scale`] call has finished on the GPU. Not used by the
        /// production pipeline (see module doc) -- for tests and one-shot
        /// previews that need a real synchronisation point, since
        /// `wgpu::Device::poll` cannot observe a command buffer this
        /// module committed directly to the raw Metal queue.
        pub fn wait_idle(&self) {
            if let Some(buf) = &self.last_command_buffer {
                buf.waitUntilCompleted();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_disabled_wins_over_everything_else() {
        assert_eq!(
            availability_from(false, true, Some(true)),
            MetalFxAvailability::FeatureDisabled
        );
    }

    #[test]
    fn non_macos_is_reported_even_with_feature_on() {
        assert_eq!(
            availability_from(true, false, Some(true)),
            MetalFxAvailability::NotMacOs
        );
    }

    #[test]
    fn unsupported_device_when_query_says_false() {
        assert_eq!(
            availability_from(true, true, Some(false)),
            MetalFxAvailability::UnsupportedDevice
        );
    }

    #[test]
    fn unsupported_device_when_query_could_not_run() {
        assert_eq!(
            availability_from(true, true, None),
            MetalFxAvailability::UnsupportedDevice
        );
    }

    #[test]
    fn available_only_when_all_three_line_up() {
        assert_eq!(
            availability_from(true, true, Some(true)),
            MetalFxAvailability::Available
        );
    }

    #[test]
    fn only_available_reports_no_reason() {
        assert!(MetalFxAvailability::Available.reason().is_none());
        assert!(MetalFxAvailability::FeatureDisabled.reason().is_some());
        assert!(MetalFxAvailability::NotMacOs.reason().is_some());
        assert!(MetalFxAvailability::UnsupportedDevice.reason().is_some());
    }

    #[test]
    fn detect_never_panics() {
        // Just exercises the real cfg!/device-query path on whatever
        // platform this test happens to run on -- must never panic, may
        // return any variant depending on the build/hardware.
        let _ = detect();
    }
}
