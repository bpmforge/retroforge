//! Renderer abstraction over wgpu: original + enhanced pipelines, shader chains
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.
//!
//! Ticket W1-06 scope: this crate holds the pieces the app shell
//! (`crates/retroforge`) needs to turn a core's indexed video output into
//! something paintable, without pulling display-color knowledge into
//! `rf-nes`/`rf-core-api` (project law — cores emit indexed pixels, never
//! RGB). [`palette`] is the NES 2C02 palette LUT; [`frame`] is the
//! `CoreSink` that resolves a whole frame's worth of indexed scanlines
//! into RGBA using it. [`gpu`] and [`original_pipeline`] are ticket
//! W3-01's headless wgpu original pipeline (indexed frame -> palette LUT ->
//! RGBA8, `docs/design/RENDERER.md` §2/§7): a direct `wgpu` dependency
//! (`docs/TECH_STACK.md`'s GPU row), never through `eframe`, since this
//! crate does not and must not depend on `eframe`. `cargo test -p
//! rf-renderer` still never *requires* a display or device -- GPU-backed
//! tests skip cleanly when no adapter exists (see [`gpu::GpuContext`]).
//! [`layers`] is ticket W3-03's BG/sprite layer extraction, built directly
//! from `CoreSink` metadata (see that module's doc for the deliberate
//! scope fence against building `RENDERER.md` §3's `SceneGraph`).

pub mod frame;
pub mod gpu;
pub mod layers;
pub mod original_pipeline;
pub mod palette;

pub use frame::FrameBuffer;
pub use gpu::{GpuContext, GpuUnavailable};
pub use layers::LayeredFrame;
pub use original_pipeline::{IndexedFrame, PalettePass};
pub use palette::{palette_index_to_rgb, NES_PALETTE};

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-renderer";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-renderer");
    }
}
