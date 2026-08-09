//! Headless wgpu device/adapter acquisition (ticket W3-01).
//!
//! wgpu 29 breaks three pre-29 setup idioms, each as a compile error rather
//! than a loud failure -- verified against the vendored `wgpu-29.0.4` /
//! `wgpu-types-29.0.4` source, not training data (see
//! `docs/TECH_STACK.md`'s "wgpu 29 API traps" section, which this module
//! implements): `InstanceDescriptor` has no `Default` impl (use
//! [`wgpu::InstanceDescriptor::new_without_display_handle_from_env`], which
//! is also how `WGPU_BACKEND`/CI's software-rasterizer selection reaches
//! this code -- see [`GpuContext::request_headless`]'s doc); `Instance::new`
//! takes its descriptor **by value**; and `enumerate_adapters`,
//! `request_adapter`, `request_device` are all `async fn`s that return
//! `impl Future` -- awaiting one with no executor simply never returns.
//! [`pollster::block_on`] is that executor.
//!
//! Two earlier attempts at this ticket each stalled for 600s inside a GPU
//! call with no diagnostic (plan.json W3-01 notes). The most likely cause,
//! per the conductor's post-mortem, is exactly that: an unbounded wait
//! somewhere in the adapter/device/readback path. Every blocking wait this
//! crate performs therefore uses a **bounded** `wgpu::PollType::Wait { ..,
//! timeout: Some(_) }` (see [`crate::original_pipeline`]) -- never
//! `timeout: None` -- so a genuine hang surfaces as `Err(PollError::Timeout)`
//! instead of silence.

use std::time::Duration;

/// Longest this crate will ever block on a single GPU
/// request/poll step. Generous for a real driver (including a software
/// rasterizer under load), short enough that a genuine hang is reported
/// well inside any reasonable test timeout rather than eating a 600s
/// watchdog the way the first two attempts at this ticket did.
pub const GPU_WAIT: Duration = Duration::from_secs(10);

/// A minimal headless GPU handle: device + queue + the adapter info that
/// produced them (useful for CI diagnostics -- which backend/software
/// rasterizer actually answered). No surface/window; nothing here ever
/// touches a display.
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_info: wgpu::AdapterInfo,
    /// The **adapter's** real limits (ticket W4-03c), captured via
    /// `Adapter::limits()` before the adapter is dropped, and *also* what
    /// `device` was actually granted -- [`Self::request_headless`]
    /// deliberately requests the device with `required_limits:
    /// adapter_limits.clone()` rather than `DeviceDescriptor::default()`'s
    /// conservative baseline (`Limits::default()`, ~8192 on every axis
    /// regardless of hardware), precisely so `device.limits()` and this
    /// field agree and both reflect the real hardware ceiling. This was
    /// not a style choice: requesting the conservative default was tried
    /// first and produces a device that validation-panics on any texture
    /// above 8192 even on hardware (this run: Metal, `max_texture_
    /// dimension_2d` 16384) that could do far more -- FM-13 enforcement
    /// (`crate::composite`) enforcing against a ceiling that is always the
    /// same hardcoded default regardless of adapter would be exactly the
    /// vacuous "hardcode 8192" this ticket's brief warns against, and
    /// would *also* still device-lose on real hardware whenever the
    /// device's granted limit (always 8192) was below what got requested
    /// but the adapter itself could have granted more.
    pub adapter_limits: wgpu::Limits,
}

/// Why [`GpuContext::request_headless`] could not produce a context. Kept
/// distinct from a bare `None` so callers (the golden-hash test in
/// particular) can put a real diagnostic in a CI failure message rather
/// than "it didn't work" -- headless CI must not require a GPU, but if CI's
/// own software-rasterizer env (`WGPU_BACKEND=gl`,
/// `LIBGL_ALWAYS_SOFTWARE=1`) is set and still produces no adapter, that is
/// a real regression the gate must not silently skip past.
#[derive(Debug)]
pub enum GpuUnavailable {
    /// `request_adapter` returned no adapter at all for the configured
    /// backends.
    NoAdapter,
    /// An adapter was found but `request_device` failed against it.
    DeviceRequestFailed(String),
}

impl std::fmt::Display for GpuUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuUnavailable::NoAdapter => write!(f, "no wgpu adapter found"),
            GpuUnavailable::DeviceRequestFailed(e) => {
                write!(f, "adapter found but request_device failed: {e}")
            }
        }
    }
}

impl GpuContext {
    /// Acquire a headless adapter+device, honoring `WGPU_BACKEND` (and the
    /// rest of wgpu's standard env knobs) via
    /// `InstanceDescriptor::new_without_display_handle_from_env` -- this is
    /// the same env var CI's `.github/workflows/ci.yml` sets
    /// (`WGPU_BACKEND=gl`, `LIBGL_ALWAYS_SOFTWARE=1`) to force the
    /// software-rasterizer fallback (see `docs/design/RENDERER.md` §7).
    ///
    /// Returns `Err` rather than panicking when no adapter/device is
    /// available -- a sandboxed dev machine with no GPU access is a real,
    /// expected environment, not a bug (headless CI must not require a
    /// GPU). Callers are the ones who decide whether that `Err` is a clean
    /// skip (local dev) or a hard failure (CI, where the software fallback
    /// is supposed to always produce an adapter).
    pub fn request_headless() -> Result<Self, GpuUnavailable> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .map_err(|_| GpuUnavailable::NoAdapter)?;
        let adapter_info = adapter.get_info();
        // Captured while `adapter` is still alive, before `request_device`
        // below -- `Adapter::request_device` takes `&self` so this does not
        // need to precede it, but reading the adapter's own capability
        // first (rather than anything derived from the device we're about
        // to request) makes the intent unambiguous: this is the hardware
        // ceiling, not a negotiated/requested value.
        let adapter_limits = adapter.limits();
        // Request the device with the adapter's *own* limits, not
        // `DeviceDescriptor::default()`'s conservative baseline
        // (`Limits::default()`, ~8192 on every axis regardless of
        // hardware) -- verified empirically (ticket W4-03c), not assumed:
        // `Device::limits()`'s doc says the granted limits "will be equal
        // to the required_limits specified when creating the device," and
        // requesting the conservative default reproduced exactly that --
        // on this machine's real Metal adapter (`max_texture_dimension_2d`
        // 16384), the *device* still only granted 8192, and creating a
        // texture above that (even though the adapter could do it) hit a
        // real `wgpu` validation panic. FM-13 enforcement
        // (`crate::composite`) is worthless against a ceiling that is
        // always the same hardcoded default irrespective of the actual
        // adapter, so this crate must request (and therefore be granted,
        // since we're asking for no more than what `adapter.limits()` just
        // reported) the real capability up front.
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: adapter_limits.clone(),
            ..Default::default()
        }))
        .map_err(|e| GpuUnavailable::DeviceRequestFailed(e.to_string()))?;
        Ok(GpuContext {
            device,
            queue,
            adapter_info,
            adapter_limits,
        })
    }

    /// Build a [`GpuContext`] around an **existing** device/queue rather
    /// than requesting a new one (ticket W4-03e). [`Self::request_headless`]
    /// is the *only* other constructor and always creates its own private
    /// device — fine for headless tests, but a host that already has a
    /// live device (the `retroforge` app shell, via
    /// `eframe::CreationContext::wgpu_render_state` — verified this ticket
    /// that eframe 0.35's *default* feature set is `wgpu`, not `glow`:
    /// `eframe-0.35.0/Cargo.toml`'s `default` array includes `"wgpu"`, so
    /// the shell genuinely has a live `wgpu::Device`/`Queue` to hand in
    /// here) must not spin up a second one just to reach this type —
    /// textures cannot cross device boundaries, so a second device would
    /// force every enhanced-composite frame through an extra GPU→CPU→GPU
    /// round trip versus the one CPU readback ([`crate::composite`]'s own
    /// design) already pays with a shared device. `RENDERER.md` §1 already
    /// calls for exactly this: "One `Device`/`Queue` shared with egui via
    /// `egui-wgpu`".
    ///
    /// All four fields are `pub` (this struct has always allowed direct
    /// construction from another crate); this constructor exists for
    /// documentation and call-site clarity, not because the fields were
    /// ever private.
    #[must_use]
    pub fn from_shared(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_info: wgpu::AdapterInfo,
        adapter_limits: wgpu::Limits,
    ) -> Self {
        GpuContext {
            device,
            queue,
            adapter_info,
            adapter_limits,
        }
    }
}

/// Blocking buffer readback: maps `buffer` for reading, polls the device
/// with a **bounded** wait (never `timeout: None` -- see this module's own
/// doc for why), and returns the mapped bytes as an owned `Vec<u8>`.
/// `buffer` must have been created with `BufferUsages::MAP_READ`. Shared by
/// every GPU pass in this crate (`crate::original_pipeline`,
/// `crate::composite`) rather than re-derived per pass -- the
/// map_async/poll/try_recv dance is exactly the kind of thing ticket W3-01
/// already spent two stalled attempts getting right (module doc).
pub(crate) fn read_buffer_sync(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
) -> Result<Vec<u8>, String> {
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(GPU_WAIT),
        })
        .map_err(|e| format!("device.poll timed out waiting for buffer readback: {e}"))?;
    match rx.try_recv() {
        Ok(Ok(())) => {
            let data = slice.get_mapped_range().to_vec();
            buffer.unmap();
            Ok(data)
        }
        Ok(Err(e)) => Err(format!("buffer map_async failed: {e}")),
        Err(_) => Err(
            "buffer map_async callback never fired even though device.poll returned \
                 (this would be the readback hanging the way attempts 1-2 did -- reported, \
                 not retried)"
                .to_string(),
        ),
    }
}

/// Round `value` up to the next multiple of `align` (`align` clamped to at
/// least 1 so this never divides by zero). Shared by every GPU pass in this
/// crate whose readback target isn't guaranteed 256-byte-aligned by
/// construction (`crate::composite`, `crate::scale`) -- `original_pipeline`
/// can `debug_assert` the alignment away instead because 256x240 RGBA8
/// (1024 bytes/row) already satisfies it, but neither an arbitrary
/// enhanced-composite target nor a PAR-scaled output width has that
/// guarantee.
pub(crate) fn align_up(value: u32, align: u32) -> u32 {
    let align = align.max(1);
    value.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not a correctness assertion (this environment may or may not have a
    /// GPU) -- exercises the acquisition path itself and prints the result,
    /// same "skip cleanly, never panic" contract
    /// [`GpuContext::request_headless`]'s doc describes. Run with
    /// `--nocapture` to see which branch fired.
    #[test]
    fn request_headless_does_not_panic_either_way() {
        match GpuContext::request_headless() {
            Ok(ctx) => println!("headless GPU available: {:?}", ctx.adapter_info),
            Err(e) => println!("headless GPU unavailable ({e}) -- treated as a clean skip"),
        }
    }

    /// [`GpuContext::from_shared`] must reproduce an equivalent context
    /// from parts pulled back out of an existing one -- proves it actually
    /// threads `device`/`queue`/`adapter_info`/`adapter_limits` through
    /// rather than, say, silently substituting `Limits::default()`
    /// (exactly the bug `request_headless`'s own doc says was already hit
    /// once for the conservative-default reason). Skips cleanly with no
    /// GPU, same contract as every other test in this module.
    #[test]
    fn from_shared_reproduces_an_equivalent_context() {
        let Some(original) = GpuContext::request_headless().ok() else {
            println!("SKIP from_shared_reproduces_an_equivalent_context: no wgpu adapter");
            return;
        };
        let expected_max_dim = original.adapter_limits.max_texture_dimension_2d;
        let expected_backend = original.adapter_info.backend;
        let rebuilt = GpuContext::from_shared(
            original.device,
            original.queue,
            original.adapter_info,
            original.adapter_limits,
        );
        assert_eq!(
            rebuilt.adapter_limits.max_texture_dimension_2d, expected_max_dim,
            "from_shared must carry the caller's adapter_limits through unchanged, not \
             substitute a default"
        );
        assert_eq!(rebuilt.adapter_info.backend, expected_backend);
    }
}
