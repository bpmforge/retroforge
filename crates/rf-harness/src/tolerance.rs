//! Reference-image-with-tolerance compare (ticket W3-01b;
//! `docs/design/RENDERER.md` §7: "anything after the 1x buffer is not
//! hashed in CI... validated by eyeball + reference images with tolerance
//! instead" — this module is that mechanism). Generic over any two
//! same-sized RGBA8 buffers, not tied to `rf_renderer::scale` specifically
//! — mirrors [`crate::golden_frame`]'s own shape: a generic compare
//! utility lives here, the pipeline-specific test that calls it lives in
//! `tests/` (`tests/scale_pass_tolerance.rs`).
//!
//! ## The metric, stated explicitly (this ticket's brief requires it)
//!
//! Two numbers, applied together via [`ToleranceConfig`]:
//!
//! - **`channel_delta`**: the max per-channel (R/G/B/A) absolute
//!   difference a single pixel may have and still count as "matching". A
//!   *correctly*-sampled nearest-neighbor pixel (no filtering, no
//!   blending — `rf_renderer::scale`'s whole design) is byte-identical to
//!   its source texel; there is no continuum of "slightly off" colors a
//!   correct implementation can legitimately produce, only "the right
//!   texel" (delta 0) or "the wrong one" (usually a large, unrelated
//!   delta, since adjacent source pixels are not guaranteed similar
//!   colors). So this number stays small — it exists only to absorb real
//!   fixed-point/rounding noise in a *filtered* pass, not a nearest one.
//! - **`max_mismatch_fraction`**: the fraction of *pixels* (not channels)
//!   allowed to fail the `channel_delta` check before the whole compare
//!   fails. This is the number actually doing cross-backend-noise
//!   absorption: `rf_renderer::scale`'s module doc explains why GPU output
//!   is not *literally* guaranteed bit-identical to the CPU oracle (WGSL
//!   float division/floor is not guaranteed bit-for-bit portable across
//!   every driver the way Rust's is) — that residual disagreement, if it
//!   exists at all, only ever shows up at scale-ratio texel *boundaries*
//!   (a vanishing fraction of total pixels for any real frame), never as a
//!   systematic difference over the whole image. A systematic bug (wrong
//!   aspect ratio, wrong overscan crop, wrong filter kernel) changes most
//!   or all pixels, not a sliver of them.
//!
//! `crates/rf-harness/tests/scale_pass_tolerance.rs` calibrates
//! [`DEFAULT_TOLERANCE`] against four named failure modes (one-pixel
//! shift, wrong aspect, wrong overscan crop, bilinear-instead-of-nearest)
//! and asserts every one of them fails — see that file for the actual
//! numbers observed and the reasoning for each threshold.
//!
//! ## Geometry first, strictly, before any pixel comparison
//!
//! [`compare_with_tolerance`] panics immediately if `actual`/`reference`
//! don't match `width`/`height` exactly — zero tolerance on *shape*. Two of
//! this ticket's four calibration mutations (wrong aspect, wrong overscan
//! crop) change the output *size*, not just its content, so this check
//! alone already catches them; it also means a pixel-level tolerance
//! comparison is never run against mismatched geometry, which would be
//! comparing apples to oranges under a threshold wide enough to hide it.

use std::io;
use std::path::Path;

/// See module doc for what each field absorbs and why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToleranceConfig {
    pub channel_delta: u8,
    pub max_mismatch_fraction: f64,
}

/// Calibrated in `crates/rf-harness/tests/scale_pass_tolerance.rs` against
/// this crate's real GPU scale pass vs. its CPU oracle (observed on Metal,
/// Apple M5 Max, this ticket) and against all four named failure-mode
/// mutations (module doc) — every one of which must fail against this
/// exact value, not a loosened one.
pub const DEFAULT_TOLERANCE: ToleranceConfig = ToleranceConfig {
    channel_delta: 0,
    max_mismatch_fraction: 0.0,
};

/// One comparison's result — always returned, whether it passed or not, so
/// a caller can report the actual numbers (this ticket's brief: "report
/// the number, don't widen it further") rather than a bare boolean.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToleranceReport {
    pub width: u32,
    pub height: u32,
    pub total_pixels: usize,
    pub mismatched_pixels: usize,
    pub max_channel_delta_observed: u8,
    pub passed: bool,
}

impl ToleranceReport {
    #[must_use]
    pub fn mismatch_fraction(&self) -> f64 {
        if self.total_pixels == 0 {
            0.0
        } else {
            self.mismatched_pixels as f64 / self.total_pixels as f64
        }
    }
}

/// Compare `actual` against `reference`, both `width * height * 4` RGBA8
/// bytes, against `cfg`.
///
/// # Panics
/// Panics if either buffer's length doesn't match `width * height * 4` —
/// same "malformed input is a caller bug" stance the renderer crate uses
/// throughout, and specifically what makes the "wrong aspect"/"wrong
/// overscan crop" calibration mutations fail here rather than silently
/// comparing mismatched geometry under a tolerance wide enough to hide it
/// (module doc).
#[must_use]
pub fn compare_with_tolerance(
    actual: &[u8],
    reference: &[u8],
    width: u32,
    height: u32,
    cfg: &ToleranceConfig,
) -> ToleranceReport {
    let expected_len = (width as usize) * (height as usize) * 4;
    assert_eq!(
        actual.len(),
        expected_len,
        "compare_with_tolerance: actual buffer is {} bytes, expected {expected_len} for {width}x{height}",
        actual.len()
    );
    assert_eq!(
        reference.len(),
        expected_len,
        "compare_with_tolerance: reference buffer is {} bytes, expected {expected_len} for {width}x{height}",
        reference.len()
    );

    let total_pixels = (width as usize) * (height as usize);
    let mut mismatched_pixels = 0usize;
    let mut max_channel_delta_observed = 0u8;

    for (a_px, r_px) in actual.chunks_exact(4).zip(reference.chunks_exact(4)) {
        let mut pixel_max_delta = 0u8;
        for (a, r) in a_px.iter().zip(r_px.iter()) {
            let delta = a.abs_diff(*r);
            if delta > pixel_max_delta {
                pixel_max_delta = delta;
            }
        }
        if pixel_max_delta > max_channel_delta_observed {
            max_channel_delta_observed = pixel_max_delta;
        }
        if pixel_max_delta > cfg.channel_delta {
            mismatched_pixels += 1;
        }
    }

    let mismatch_fraction = if total_pixels == 0 {
        0.0
    } else {
        mismatched_pixels as f64 / total_pixels as f64
    };
    let passed = mismatch_fraction <= cfg.max_mismatch_fraction;

    ToleranceReport {
        width,
        height,
        total_pixels,
        mismatched_pixels,
        max_channel_delta_observed,
        passed,
    }
}

/// Dump `rgba` as a binary PPM (`P6`) file at `path` — the "eyeball"
/// half of RENDERER.md §7's "validated by eyeball + reference images with
/// tolerance" — so a human can open a failing test's actual/reference
/// buffers directly (any image viewer or ImageMagick reads `.ppm` with no
/// conversion) without this crate taking on an image-codec dependency
/// just to write one file. Strips alpha (`.ppm`'s `P6` is RGB-only); every
/// RGBA buffer in this workspace is alpha-opaque at the presentation
/// boundary anyway (`crate::golden_frame`/`rf_renderer::frame` module
/// docs).
///
/// # Errors
/// Returns `Err` on any I/O failure writing `path`.
///
/// # Panics
/// Panics if `rgba.len() != width * height * 4` — same stance as
/// [`compare_with_tolerance`].
pub fn dump_ppm(path: &Path, rgba: &[u8], width: u32, height: u32) -> io::Result<()> {
    assert_eq!(
        rgba.len(),
        (width as usize) * (height as usize) * 4,
        "dump_ppm: buffer size does not match {width}x{height}"
    );
    let mut out = Vec::with_capacity(rgba.len() + 32);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w as usize) * (h as usize) * 4);
        for _ in 0..(w * h) {
            v.extend_from_slice(&color);
        }
        v
    }

    #[test]
    fn identical_buffers_pass_at_zero_tolerance() {
        let a = solid(4, 4, [10, 20, 30, 255]);
        let r = a.clone();
        let report = compare_with_tolerance(&a, &r, 4, 4, &DEFAULT_TOLERANCE);
        assert!(report.passed);
        assert_eq!(report.mismatched_pixels, 0);
        assert_eq!(report.max_channel_delta_observed, 0);
    }

    #[test]
    fn a_single_differing_pixel_fails_at_zero_mismatch_fraction() {
        let mut a = solid(4, 4, [10, 20, 30, 255]);
        let r = a.clone();
        a[0] = 11; // one channel, one pixel
        let report = compare_with_tolerance(&a, &r, 4, 4, &DEFAULT_TOLERANCE);
        assert!(!report.passed);
        assert_eq!(report.mismatched_pixels, 1);
        assert_eq!(report.max_channel_delta_observed, 1);
    }

    #[test]
    fn channel_delta_within_threshold_does_not_count_as_a_mismatch() {
        let mut a = solid(4, 4, [10, 20, 30, 255]);
        let r = a.clone();
        a[0] = 12; // delta 2
        let cfg = ToleranceConfig {
            channel_delta: 2,
            max_mismatch_fraction: 0.0,
        };
        let report = compare_with_tolerance(&a, &r, 4, 4, &cfg);
        assert!(report.passed);
        assert_eq!(report.mismatched_pixels, 0);
        assert_eq!(report.max_channel_delta_observed, 2);
    }

    #[test]
    fn mismatch_fraction_above_threshold_fails_even_if_only_a_few_pixels_differ() {
        let mut a = solid(10, 10, [1, 1, 1, 255]); // 100 pixels
        let r = a.clone();
        a[0] = 200; // 1 pixel wrong = 1% mismatch
        let cfg = ToleranceConfig {
            channel_delta: 0,
            max_mismatch_fraction: 0.005, // 0.5% allowed, less than the 1% present
        };
        let report = compare_with_tolerance(&a, &r, 10, 10, &cfg);
        assert!(!report.passed);
        assert!((report.mismatch_fraction() - 0.01).abs() < 1e-9);
    }

    #[test]
    #[should_panic(expected = "reference buffer is")]
    fn mismatched_reference_length_panics_rather_than_comparing_wrong_shapes() {
        let a = solid(4, 4, [0, 0, 0, 255]);
        let r = solid(5, 4, [0, 0, 0, 255]);
        let _ = compare_with_tolerance(&a, &r, 4, 4, &DEFAULT_TOLERANCE);
    }

    #[test]
    fn dump_ppm_writes_a_readable_header_and_stripped_alpha_body() {
        let dir =
            std::env::temp_dir().join(format!("rf-harness-tolerance-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join("dump.ppm");
        let rgba = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ];
        dump_ppm(&path, &rgba, 2, 2).expect("dump_ppm must succeed");
        let bytes = std::fs::read(&path).expect("read back the dumped file");
        assert!(bytes.starts_with(b"P6\n2 2\n255\n"));
        let body = &bytes[b"P6\n2 2\n255\n".len()..];
        assert_eq!(body, &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
