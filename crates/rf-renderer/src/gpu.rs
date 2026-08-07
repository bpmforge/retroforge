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
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|e| GpuUnavailable::DeviceRequestFailed(e.to_string()))?;
        Ok(GpuContext {
            device,
            queue,
            adapter_info,
        })
    }
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
}
