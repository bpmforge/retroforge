//! Original pipeline, end to end (ticket W3-02): palette pass -> scale pass
//! -> shader chain, one function call. `docs/STATUS.md`'s 2026-08-08 W3-01b
//! entry recorded this as an honest partial -- "ScalePass is not yet
//! chained to PalettePass (non-interference proven, 'downstream' not
//! demonstrated by a wired pipeline)" -- and this ticket's brief invited
//! closing it here, since the shader-chain work already sits immediately
//! downstream of [`crate::scale::ScalePass`]'s own output. [`run`] is
//! genuinely the three passes wired together (verified below against
//! calling each pass by hand and asserting identical bytes), not a
//! re-implementation.
//!
//! Each pass is still built and owned independently by the caller
//! ([`crate::original_pipeline::PalettePass`], [`crate::scale::ScalePass`],
//! [`crate::shader_chain::ShaderChain`]) -- this module adds no new
//! GPU-resource type of its own, only the sequencing. A caller that wants
//! just the 1x golden-hashable buffer, or just the scaled-but-unfiltered
//! buffer, still calls the individual passes directly (`tests/
//! golden_frame_gpu.rs`/`tests/scale_pass_tolerance.rs` keep doing exactly
//! that) -- `run` is a convenience for the caller that wants the whole
//! original pipeline in one call, not a replacement for the individual
//! passes.

use crate::gpu::GpuContext;
use crate::original_pipeline::{IndexedFrame, PalettePass};
use crate::scale::{ScaleGeometry, ScalePass};
use crate::shader_chain::{ChainStage, ShaderChain};

/// Render `frame` through the palette pass, then the scale pass (per
/// `geometry`), then `stages` (an empty slice is passthrough, same
/// contract as [`ShaderChain::render`]) -- the full original pipeline,
/// `docs/design/RENDERER.md` §2/§4, in one call.
///
/// # Errors
/// Returns `Err` if any stage's GPU readback does not complete within
/// [`crate::gpu::GPU_WAIT`] -- same "reported, never retried in a loop"
/// contract as every pass this function calls.
pub fn run(
    gpu: &GpuContext,
    palette_pass: &PalettePass,
    scale_pass: &ScalePass,
    shader_chain: &ShaderChain,
    frame: &IndexedFrame,
    geometry: &ScaleGeometry,
    stages: &[ChainStage],
) -> Result<Vec<u8>, String> {
    let post_palette = palette_pass.render(gpu, frame)?;
    let post_scale =
        scale_pass.render(gpu, &post_palette, frame.width(), frame.height(), geometry)?;
    shader_chain.render(
        gpu,
        &post_scale,
        geometry.out_width,
        geometry.out_height,
        stages,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::original_pipeline::IndexedFrame;
    use crate::scale::{FillMode, Overscan, ParRatio};

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::request_headless() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                if std::env::var_os("CI").is_some() {
                    panic!("{test_name} cannot skip in CI: {e}");
                }
                eprintln!(
                    "SKIP {test_name}: no wgpu adapter in this environment ({e}) -- clean skip, \
                     run with --nocapture to see this line"
                );
                None
            }
        }
    }

    /// NES-shaped (256x240) indexed frame -- `PalettePass::render` requires
    /// `width * 4` to already be 256-byte-row-aligned (its own
    /// `debug_assert`; only 256-wide NES frames satisfy that without extra
    /// row-padding, which is out of this test's scope), and 240 lines so
    /// `Overscan::Crop224` doesn't panic (it needs at least 224). Palette
    /// indices vary by position so palette/scale/chain bugs that only show
    /// on non-uniform content would be visible here too, same "don't test
    /// only a flat frame" stance as `crates/rf-harness/tests/
    /// scale_pass_tolerance.rs`'s synthetic source.
    fn indexed_test_frame() -> IndexedFrame {
        let width = 256u32;
        let height = 240u32;
        let mut indices = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                indices.push(((x + y) % 64) as u8);
            }
        }
        IndexedFrame::new(width, height, indices)
    }

    /// `run` must produce byte-identical output to calling the three passes
    /// by hand in the same order -- proves this is genuine wiring, not an
    /// independent (and possibly divergent) re-implementation.
    #[test]
    fn run_matches_calling_each_pass_by_hand() {
        let Some(gpu) = gpu_or_skip("run_matches_calling_each_pass_by_hand") else {
            return;
        };
        let palette_pass = PalettePass::new(&gpu);
        let scale_pass = ScalePass::new(&gpu);
        let shader_chain = ShaderChain::new(&gpu);
        let frame = indexed_test_frame();
        let geometry = ScaleGeometry::compute(
            frame.width(),
            frame.height(),
            Overscan::Crop224,
            ParRatio::SQUARE,
            FillMode::IntegerLocked { integer_factor: 1 },
        );
        let stages = [ChainStage::scanlines(0.5, 2.0)];

        let via_run = run(
            &gpu,
            &palette_pass,
            &scale_pass,
            &shader_chain,
            &frame,
            &geometry,
            &stages,
        )
        .expect("run must succeed");

        let post_palette = palette_pass.render(&gpu, &frame).expect("palette pass");
        let post_scale = scale_pass
            .render(
                &gpu,
                &post_palette,
                frame.width(),
                frame.height(),
                &geometry,
            )
            .expect("scale pass");
        let by_hand = shader_chain
            .render(
                &gpu,
                &post_scale,
                geometry.out_width,
                geometry.out_height,
                &stages,
            )
            .expect("shader chain");

        assert_eq!(
            via_run, by_hand,
            "run() must be byte-identical to calling PalettePass -> ScalePass -> ShaderChain by \
             hand -- this is what proves the wiring is real, not a parallel implementation"
        );
        assert_eq!(
            via_run.len(),
            (geometry.out_width as usize) * (geometry.out_height as usize) * 4,
            "final output must match the scale geometry's declared size"
        );
    }

    /// An empty chain must leave `run`'s output identical to the
    /// palette-then-scale result alone -- the pipeline-level version of
    /// `shader_chain`'s own empty-chain-is-passthrough guarantee.
    #[test]
    fn run_with_empty_chain_matches_palette_then_scale_alone() {
        let Some(gpu) = gpu_or_skip("run_with_empty_chain_matches_palette_then_scale_alone") else {
            return;
        };
        let palette_pass = PalettePass::new(&gpu);
        let scale_pass = ScalePass::new(&gpu);
        let shader_chain = ShaderChain::new(&gpu);
        let frame = indexed_test_frame();
        let geometry = ScaleGeometry::compute(
            frame.width(),
            frame.height(),
            Overscan::Crop224,
            ParRatio::SQUARE,
            FillMode::IntegerLocked { integer_factor: 2 },
        );

        let via_run = run(
            &gpu,
            &palette_pass,
            &scale_pass,
            &shader_chain,
            &frame,
            &geometry,
            &[],
        )
        .expect("run with empty chain must succeed");

        let post_palette = palette_pass.render(&gpu, &frame).expect("palette pass");
        let post_scale = scale_pass
            .render(
                &gpu,
                &post_palette,
                frame.width(),
                frame.height(),
                &geometry,
            )
            .expect("scale pass");

        assert_eq!(
            via_run, post_scale,
            "an empty shader chain must leave run()'s output identical to palette-then-scale \
             alone"
        );
    }
}
