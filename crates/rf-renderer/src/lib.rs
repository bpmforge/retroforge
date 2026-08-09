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
//! [`composite`] is ticket W4-03c's enhanced-pipeline layer compositor:
//! shell-resolved RGBA layers, back-to-front, into an arbitrarily sized
//! (including ultrawide) render target, with FM-13 allocation-size
//! enforcement against the real adapter limit (see that module's doc for
//! the shell-mediated scope fence -- this crate still has no dependency on
//! `rf-enhance`). [`scale`] is ticket W3-01b's original-pipeline scale
//! pass (`docs/design/RENDERER.md` §2/§7): overscan crop + integer/PAR
//! scale over the 1x post-palette buffer, nearest-neighbor, plus the CPU
//! oracle ([`scale::render_scaled_reference`]) `rf-harness`'s
//! reference-image-with-tolerance test checks the GPU output against,
//! since (§7) nothing past the 1x buffer is golden-hashable in CI.

pub mod composite;
pub mod frame;
pub mod gpu;
pub mod layers;
pub mod original_pipeline;
pub mod palette;
pub mod scale;

pub use composite::{CompositeLayer, CompositeOutcome, EnhancedCompositor, TargetReduction};
pub use frame::FrameBuffer;
pub use gpu::{GpuContext, GpuUnavailable};
pub use layers::LayeredFrame;
pub use original_pipeline::{IndexedFrame, PalettePass};
pub use palette::{palette_index_to_rgb, NES_PALETTE};
pub use scale::{render_scaled_reference, FillMode, Overscan, ParRatio, ScaleGeometry, ScalePass};

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-renderer";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-renderer");
    }
}
