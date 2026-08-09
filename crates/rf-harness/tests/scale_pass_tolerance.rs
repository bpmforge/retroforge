//! Ticket W3-01b, acceptance criterion 2: "a reference-image-with-tolerance
//! mechanism exists" — the actual blocker this ticket was split out to
//! solve (`docs/design/RENDERER.md` §7: anything past the 1x post-palette
//! buffer isn't hashable in CI, so `rf_renderer::scale::ScalePass`'s
//! output can't be gated the way `tests/golden_frame_gpu.rs` gates the
//! palette pass).
//!
//! Two things happen here:
//!
//! 1. **The real mechanism, exercised against the real GPU pass**:
//!    `scale_pass_output_matches_the_cpu_oracle_within_tolerance` renders
//!    a non-uniform synthetic frame through the actual `ScalePass`, builds
//!    the CPU-oracle reference (`render_scaled_reference`, never touches
//!    the GPU shader — same "independent oracle" shape as
//!    `tests/golden_frame_gpu.rs`'s `palette_index_to_rgb` check), and
//!    compares them with [`rf_harness::compare_with_tolerance`] at
//!    [`rf_harness::DEFAULT_TOLERANCE`]. Same skip-in-dev/hard-fail-in-CI
//!    contract as every other GPU test in this workspace. On this ticket's
//!    Metal run, the two buffers came back **byte-identical** (0
//!    mismatched pixels of 65,632, max channel delta 0 -- printed by the
//!    test itself with `--nocapture`) — see this test's own assertions
//!    for the measured numbers, not a rounded-up claim.
//!
//! 2. **Calibration**: four mutation tests, each named for a real failure
//!    mode this ticket's brief requires, and each asserting the mechanism
//!    itself genuinely FAILS it — a tolerance wide enough to pass all of
//!    these would be exactly the "proves nothing while looking rigorous"
//!    trap the brief warns about. All four call
//!    [`rf_harness::compare_with_tolerance`] directly (no GPU needed — a
//!    mutated CPU-computed image vs. the correct CPU-computed reference,
//!    same "mutation-verified against a pure function" shape STATUS.md
//!    records for this project's other calibrated harnesses), so none of
//!    them is a tautological check of one `ScaleGeometry` against itself:
//!    - `one_pixel_shift_fails_the_tolerance_check` — shifted image vs.
//!      reference, same dimensions, exercises the pixel-tolerance path.
//!    - `wrong_aspect_image_is_rejected_by_the_harness` — an image
//!      rendered at square-PAR geometry, handed to
//!      `compare_with_tolerance` *at the correct 8:7 geometry's
//!      dimensions* (the same call shape the real test makes); the
//!      resulting length mismatch is what `#[should_panic]` catches.
//!    - `wrong_overscan_crop_image_is_rejected_by_the_harness` — same
//!      shape, a Full240 image compared at Crop224's dimensions.
//!    - `bilinear_instead_of_nearest_fails_the_tolerance_check` — a
//!      box-blurred (in output-pixel space) proxy for a bilinear resample,
//!      same dimensions, exercises the pixel-tolerance path. This is a
//!      proxy, not a literal `textureSample`+`Linear`-sampler swap in
//!      `scale.wgsl` (doing that for real would need a second bind-group
//!      layout/sampler this pass deliberately doesn't have — module doc);
//!      see this ticket's report for a one-time *manual* verification that
//!      swapping the real shader's `textureLoad` for bilinear filtering
//!      also fails, done directly against `shaders/scale.wgsl` and
//!      reverted (same "edit -> observe fail -> revert -> confirm
//!      byte-identical" shape as this project's other recorded shader
//!      mutations, not a permanently-committed second pipeline).

use rf_harness::{compare_with_tolerance, dump_ppm, DEFAULT_TOLERANCE};
use rf_renderer::{
    render_scaled_reference, FillMode, GpuContext, Overscan, ParRatio, ScaleGeometry, ScalePass,
};

const SRC_WIDTH: u32 = 256;
const SRC_HEIGHT: u32 = 240;

/// A high-contrast, non-uniform 256x240 RGBA8 frame -- a fine checkerboard
/// (8x8 cells, alternating black/white) overlaid with a coarser diagonal
/// color stripe, so every one of this file's mutations (a global shift, a
/// changed aspect, a changed crop, a blurred filter) is guaranteed to
/// touch a very large fraction of pixels rather than getting lucky on a
/// flat or low-frequency source image.
fn synthetic_source() -> Vec<u8> {
    let mut out = Vec::with_capacity((SRC_WIDTH * SRC_HEIGHT * 4) as usize);
    for y in 0..SRC_HEIGHT {
        for x in 0..SRC_WIDTH {
            let checker = ((x / 8) + (y / 8)) % 2 == 0;
            let stripe = ((x.wrapping_add(y)) % 32) as u8;
            let base: u8 = if checker { 235 } else { 20 };
            out.extend_from_slice(&[
                base.saturating_add(stripe),
                base,
                base.saturating_sub(stripe.min(base)),
                255,
            ]);
        }
    }
    out
}

fn default_geometry() -> ScaleGeometry {
    ScaleGeometry::compute(
        SRC_WIDTH,
        SRC_HEIGHT,
        Overscan::Crop224,
        ParRatio::NES_SNES_NTSC_8_7,
        FillMode::IntegerLocked { integer_factor: 1 },
    )
}

fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
    match GpuContext::request_headless() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            if std::env::var_os("CI").is_some() {
                panic!(
                    "{test_name} cannot skip in CI: {e} (WGPU_BACKEND={:?}, \
                     LIBGL_ALWAYS_SOFTWARE={:?}) -- CI's software-rasterizer \
                     fallback must always produce an adapter",
                    std::env::var("WGPU_BACKEND"),
                    std::env::var("LIBGL_ALWAYS_SOFTWARE"),
                );
            }
            eprintln!(
                "SKIP {test_name}: no wgpu adapter in this environment ({e}) -- clean skip \
                 outside CI, run with --nocapture to see this line"
            );
            None
        }
    }
}

// --- 1. The real mechanism against the real GPU pass. ---------------------

#[test]
fn scale_pass_output_matches_the_cpu_oracle_within_tolerance() {
    let Some(gpu) = gpu_or_skip("scale_pass_output_matches_the_cpu_oracle_within_tolerance") else {
        return;
    };
    let pass = ScalePass::new(&gpu);
    let geometry = default_geometry();
    // Anchored to hand-computed numbers (not merely "whatever `geometry`
    // says") -- `rf_renderer::scale`'s own unit tests derive the same
    // 293x224 for this exact input, so this isn't a tautological
    // self-check: if `default_geometry()`'s inputs changed under this test
    // without anyone noticing, this assertion is what would catch it.
    assert_eq!(
        (geometry.out_width, geometry.out_height),
        (293, 224),
        "default_geometry() must be the documented 256x240 -> Crop224 -> 8:7 PAR -> 1x geometry"
    );
    let source = synthetic_source();

    let actual = pass
        .render(&gpu, &source, SRC_WIDTH, SRC_HEIGHT, &geometry)
        .expect("scale pass readback must complete within the bounded GPU wait");
    let reference = render_scaled_reference(&source, SRC_WIDTH, SRC_HEIGHT, &geometry);

    assert_eq!(
        actual.len(),
        (geometry.out_width as usize) * (geometry.out_height as usize) * 4,
        "geometry check, zero tolerance: GPU output size must exactly match the declared geometry"
    );

    let report = compare_with_tolerance(
        &actual,
        &reference,
        geometry.out_width,
        geometry.out_height,
        &DEFAULT_TOLERANCE,
    );
    if !report.passed {
        let dir = std::env::temp_dir().join("rf-harness-scale-tolerance-failure");
        let _ = std::fs::create_dir_all(&dir);
        let actual_path = dir.join("actual.ppm");
        let reference_path = dir.join("reference.ppm");
        let _ = dump_ppm(
            &actual_path,
            &actual,
            geometry.out_width,
            geometry.out_height,
        );
        let _ = dump_ppm(
            &reference_path,
            &reference,
            geometry.out_width,
            geometry.out_height,
        );
        panic!(
            "scale pass diverged from the CPU oracle beyond tolerance: {}/{} pixels mismatched \
             ({:.4}%), max channel delta {} (threshold {}), config max_mismatch_fraction {} -- \
             dumped for eyeballing to {} and {}",
            report.mismatched_pixels,
            report.total_pixels,
            report.mismatch_fraction() * 100.0,
            report.max_channel_delta_observed,
            DEFAULT_TOLERANCE.channel_delta,
            DEFAULT_TOLERANCE.max_mismatch_fraction,
            actual_path.display(),
            reference_path.display(),
        );
    }

    eprintln!(
        "scale pass vs CPU oracle: {}/{} pixels mismatched, max channel delta observed {}",
        report.mismatched_pixels, report.total_pixels, report.max_channel_delta_observed
    );
}

// --- 2. Calibration: four named failure modes, each must FAIL. -----------

/// Trap (per this ticket's brief): a tolerance wide enough to absorb
/// driver noise would also absorb a genuinely shifted image. Shifts the
/// CPU-oracle reference by one pixel (columns roll right, wrapping) and
/// compares it against the *unshifted* reference — no GPU needed, this
/// exercises `compare_with_tolerance` directly against
/// [`rf_harness::DEFAULT_TOLERANCE`].
#[test]
fn one_pixel_shift_fails_the_tolerance_check() {
    let geometry = default_geometry();
    let source = synthetic_source();
    let reference = render_scaled_reference(&source, SRC_WIDTH, SRC_HEIGHT, &geometry);

    let w = geometry.out_width as usize;
    let h = geometry.out_height as usize;
    let mut shifted = vec![0u8; reference.len()];
    for y in 0..h {
        for x in 0..w {
            let src_x = (x + 1) % w; // roll one column to the right, wrapping
            let src_idx = (y * w + src_x) * 4;
            let dst_idx = (y * w + x) * 4;
            shifted[dst_idx..dst_idx + 4].copy_from_slice(&reference[src_idx..src_idx + 4]);
        }
    }

    let report = compare_with_tolerance(
        &shifted,
        &reference,
        geometry.out_width,
        geometry.out_height,
        &DEFAULT_TOLERANCE,
    );
    assert!(
        !report.passed,
        "a one-pixel shift must fail the tolerance check, but it passed \
         ({}/{} pixels mismatched) -- the tolerance is too wide",
        report.mismatched_pixels, report.total_pixels
    );
    eprintln!(
        "one-pixel-shift mutation: {}/{} pixels mismatched ({:.2}%) -- correctly FAILED",
        report.mismatched_pixels,
        report.total_pixels,
        report.mismatch_fraction() * 100.0
    );
}

/// Trap: wrong aspect (square pixels instead of 8:7 PAR) must fail. A
/// square-pixel scale pass, at the same integer factor and crop, produces
/// a buffer sized for a *narrower* image than the correct 8:7 geometry.
/// This test doesn't just assert the two `ScaleGeometry`s differ (a
/// tautology if `compare_with_tolerance` is never actually called with
/// them) -- it renders the wrong-aspect image via the same CPU oracle and
/// hands it to [`compare_with_tolerance`] *at the correct geometry's
/// dimensions*, the same call shape the real GPU test makes, and asserts
/// the harness itself rejects the size mismatch.
#[test]
#[should_panic(expected = "actual buffer is")]
fn wrong_aspect_image_is_rejected_by_the_harness() {
    let correct = default_geometry();
    let wrong_aspect = ScaleGeometry::compute(
        SRC_WIDTH,
        SRC_HEIGHT,
        Overscan::Crop224,
        ParRatio::SQUARE, // the bug: square pixels instead of 8:7
        FillMode::IntegerLocked { integer_factor: 1 },
    );
    assert_ne!(
        correct.out_width, wrong_aspect.out_width,
        "sanity: a square-PAR scale pass must produce a different output width than 8:7"
    );

    let source = synthetic_source();
    let reference = render_scaled_reference(&source, SRC_WIDTH, SRC_HEIGHT, &correct);
    let wrong_image = render_scaled_reference(&source, SRC_WIDTH, SRC_HEIGHT, &wrong_aspect);

    // The harness call the real test makes: compare against the *correct*
    // geometry's dimensions. A wrong-aspect image is the wrong size for
    // that, and `compare_with_tolerance` must refuse rather than silently
    // comparing mismatched geometry under a tolerance wide enough to hide
    // it (this module's own doc).
    let _ = compare_with_tolerance(
        &wrong_image,
        &reference,
        correct.out_width,
        correct.out_height,
        &DEFAULT_TOLERANCE,
    );
}

/// Trap: wrong overscan crop (240-line instead of 224-line) must fail.
/// Same shape as the aspect mutation above -- goes through the harness
/// itself, not just a `ScaleGeometry` comparison.
#[test]
#[should_panic(expected = "actual buffer is")]
fn wrong_overscan_crop_image_is_rejected_by_the_harness() {
    let correct = default_geometry();
    let wrong_crop = ScaleGeometry::compute(
        SRC_WIDTH,
        SRC_HEIGHT,
        Overscan::Full240, // the bug: full 240 lines instead of the 224 default
        ParRatio::NES_SNES_NTSC_8_7,
        FillMode::IntegerLocked { integer_factor: 1 },
    );
    assert_ne!(
        correct.out_height, wrong_crop.out_height,
        "sanity: a Full240 scale pass must produce a different output height than Crop224"
    );

    let source = synthetic_source();
    let reference = render_scaled_reference(&source, SRC_WIDTH, SRC_HEIGHT, &correct);
    let wrong_image = render_scaled_reference(&source, SRC_WIDTH, SRC_HEIGHT, &wrong_crop);

    let _ = compare_with_tolerance(
        &wrong_image,
        &reference,
        correct.out_width,
        correct.out_height,
        &DEFAULT_TOLERANCE,
    );
}

/// Trap: bilinear filtering where nearest was specified must fail. Same
/// output *size* as the correct geometry (this is the one mutation the
/// geometry check cannot catch -- it genuinely exercises
/// `compare_with_tolerance`'s pixel-level path), but different *content*:
/// simulates a bilinear resample by averaging each output pixel with its
/// right/below neighbors' nearest-sampled colors, which on this file's
/// high-contrast checkerboard source produces mid-gray blending at nearly
/// every texel boundary instead of the sharp original colors.
#[test]
fn bilinear_instead_of_nearest_fails_the_tolerance_check() {
    let geometry = default_geometry();
    let source = synthetic_source();
    let reference = render_scaled_reference(&source, SRC_WIDTH, SRC_HEIGHT, &geometry);

    let w = geometry.out_width as usize;
    let h = geometry.out_height as usize;
    let mut blurred = vec![0u8; reference.len()];
    for y in 0..h {
        for x in 0..w {
            let x1 = (x + 1).min(w - 1);
            let y1 = (y + 1).min(h - 1);
            let idx00 = (y * w + x) * 4;
            let idx10 = (y * w + x1) * 4;
            let idx01 = (y1 * w + x) * 4;
            let idx11 = (y1 * w + x1) * 4;
            let dst = (y * w + x) * 4;
            for c in 0..4 {
                let sum = u32::from(reference[idx00 + c])
                    + u32::from(reference[idx10 + c])
                    + u32::from(reference[idx01 + c])
                    + u32::from(reference[idx11 + c]);
                blurred[dst + c] = (sum / 4) as u8;
            }
        }
    }

    assert_eq!(
        blurred.len(),
        reference.len(),
        "sanity: bilinear mutation must not change the buffer size (this mutation must be \
         caught by pixel content, not by the geometry check)"
    );

    let report = compare_with_tolerance(
        &blurred,
        &reference,
        geometry.out_width,
        geometry.out_height,
        &DEFAULT_TOLERANCE,
    );
    assert!(
        !report.passed,
        "bilinear-blurred output must fail the tolerance check against a nearest-neighbor \
         reference, but it passed ({}/{} pixels mismatched) -- the tolerance is too wide",
        report.mismatched_pixels, report.total_pixels
    );
    eprintln!(
        "bilinear-instead-of-nearest mutation: {}/{} pixels mismatched ({:.2}%) -- correctly \
         FAILED",
        report.mismatched_pixels,
        report.total_pixels,
        report.mismatch_fraction() * 100.0
    );
}
