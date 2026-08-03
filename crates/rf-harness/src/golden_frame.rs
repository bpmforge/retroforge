//! Golden-frame hash compare helper + FM-08 divergence report (ticket
//! W0-03; TESTING.md §2/§6, FAILURE_MODES.md FM-08).
//!
//! Hashes **`palette_index` only**, in scanline order — never the full
//! [`rf_core_api::PpuPixel`] (layer/sprite_id/priority/dropped_by_limit).
//! Those are core-internal metadata for the enhancement side; a golden
//! keyed on them would break the moment that metadata's shape evolves,
//! even though the actual pixels on screen (what "golden frame" means —
//! TESTING.md §2: "hash framebuffer (indexed buffer, pre-shader...)")
//! didn't change. If per-pixel metadata ever needs its own golden, that's
//! a separate hash function, not a reason to fatten this one.

use rf_core_api::{CoreEvent, CoreSink, PpuPixel};
use sha2::{Digest, Sha256};

/// A [`CoreSink`] that captures one frame's worth of `palette_index`
/// values in scanline order, ignoring audio/events. Panics if scanlines
/// arrive out of order or with an inconsistent pixel count — a core
/// emitting a partial or reordered frame must fail loudly here, not
/// produce a silently-wrong golden hash.
#[derive(Debug, Default)]
pub struct FrameCapture {
    scanlines: Vec<Vec<u8>>,
    next_expected_y: u16,
}

impl FrameCapture {
    #[must_use]
    pub fn new() -> Self {
        FrameCapture::default()
    }

    /// Palette indices in scanline order, one `Vec<u8>` per scanline in
    /// the order they were received.
    #[must_use]
    pub fn scanlines(&self) -> &[Vec<u8>] {
        &self.scanlines
    }

    /// Number of scanlines captured so far.
    #[must_use]
    pub fn scanline_count(&self) -> usize {
        self.scanlines.len()
    }
}

impl CoreSink for FrameCapture {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        assert_eq!(
            y, self.next_expected_y,
            "FrameCapture received scanline {y} out of order (expected {})",
            self.next_expected_y
        );
        self.scanlines
            .push(pixels.iter().map(|p| p.palette_index).collect());
        self.next_expected_y = self.next_expected_y.wrapping_add(1);
    }

    fn audio(&mut self, _samples: &[i16]) {}

    fn event(&mut self, _ev: CoreEvent) {}
}

/// Hash a captured frame's `palette_index` values, in scanline order, as
/// lowercase hex SHA-256. Asserts `scanlines.len() == expected_height` —
/// TESTING.md §2's golden-frame protocol assumes a complete frame; a core
/// that emits 239 of 240 lines must fail this check rather than silently
/// hash a partial frame that happens to still produce *some* hash.
///
/// # Panics
/// Panics if `scanlines.len() != expected_height`.
#[must_use]
pub fn hash_frame_palette_indices(scanlines: &[Vec<u8>], expected_height: usize) -> String {
    assert_eq!(
        scanlines.len(),
        expected_height,
        "golden-frame hash requires a complete frame: got {} scanlines, expected {}",
        scanlines.len(),
        expected_height
    );
    let mut hasher = Sha256::new();
    for line in scanlines {
        hasher.update(line);
    }
    let digest = hasher.finalize();
    use std::fmt::Write;
    digest.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// FM-08 divergence report: the first frame at which two periodic
/// (frame_number, hash) checkpoint sequences disagree, plus both hashes.
/// Generic over what was hashed (state hash, frame hash, ...) — this is
/// deliberately not coupled to how a checkpoint's hash was computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivergenceReport {
    pub frame: u64,
    pub expected_hash: String,
    pub actual_hash: String,
}

/// Compare two periodic checkpoint sequences `(frame_number, hash)` and
/// return the first frame at which they disagree (FAILURE_MODES.md FM-08:
/// "Replay halts at first divergent frame... Divergence report: frame N,
/// both hashes"). Checkpoints are matched by frame number, not position —
/// a checkpoint frame present in only one sequence is not itself a
/// divergence (bisection re-runs may sample different frames); only a
/// frame number present in both with differing hashes counts.
#[must_use]
pub fn find_first_divergence(
    expected: &[(u64, String)],
    actual: &[(u64, String)],
) -> Option<DivergenceReport> {
    let mut actual_sorted: Vec<&(u64, String)> = actual.iter().collect();
    actual_sorted.sort_by_key(|(f, _)| *f);

    let mut expected_sorted: Vec<&(u64, String)> = expected.iter().collect();
    expected_sorted.sort_by_key(|(f, _)| *f);

    for (frame, expected_hash) in &expected_sorted {
        if let Ok(idx) = actual_sorted.binary_search_by_key(frame, |(f, _)| *f) {
            let (_, actual_hash) = actual_sorted[idx];
            if actual_hash != expected_hash {
                return Some(DivergenceReport {
                    frame: *frame,
                    expected_hash: expected_hash.clone(),
                    actual_hash: actual_hash.clone(),
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::PixelLayer;

    fn px(idx: u8) -> PpuPixel {
        PpuPixel {
            palette_index: idx,
            layer: PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
            dropped_by_limit: false,
        }
    }

    #[test]
    fn hashes_only_palette_index_ignoring_other_metadata() {
        let mut a = FrameCapture::new();
        a.video_scanline(0, &[px(1), px(2)]);
        let mut b = FrameCapture::new();
        // Same palette indices, different layer/sprite_id/priority — must
        // hash identically since only palette_index is the golden.
        b.video_scanline(
            0,
            &[
                PpuPixel {
                    palette_index: 1,
                    layer: PixelLayer::Sprite,
                    sprite_id: Some(7),
                    priority: 3,
                    dropped_by_limit: true,
                },
                PpuPixel {
                    palette_index: 2,
                    layer: PixelLayer::Background(1),
                    sprite_id: None,
                    priority: 1,
                    dropped_by_limit: false,
                },
            ],
        );
        assert_eq!(
            hash_frame_palette_indices(a.scanlines(), 1),
            hash_frame_palette_indices(b.scanlines(), 1)
        );
    }

    #[test]
    fn different_palette_indices_hash_differently() {
        let mut a = FrameCapture::new();
        a.video_scanline(0, &[px(1)]);
        let mut b = FrameCapture::new();
        b.video_scanline(0, &[px(2)]);
        assert_ne!(
            hash_frame_palette_indices(a.scanlines(), 1),
            hash_frame_palette_indices(b.scanlines(), 1)
        );
    }

    #[test]
    #[should_panic(expected = "out of order")]
    fn panics_on_out_of_order_scanline() {
        let mut c = FrameCapture::new();
        c.video_scanline(0, &[px(1)]);
        c.video_scanline(2, &[px(1)]); // skipped 1
    }

    #[test]
    #[should_panic(expected = "complete frame")]
    fn panics_on_incomplete_frame_hash() {
        let mut c = FrameCapture::new();
        c.video_scanline(0, &[px(1)]);
        let _ = hash_frame_palette_indices(c.scanlines(), 2);
    }

    #[test]
    fn find_first_divergence_names_frame_and_both_hashes() {
        let expected = vec![
            (0u64, "h0".to_string()),
            (60, "h60".to_string()),
            (120, "h120-expected".to_string()),
        ];
        let actual = vec![
            (0u64, "h0".to_string()),
            (60, "h60".to_string()),
            (120, "h120-actual".to_string()),
        ];
        let report = find_first_divergence(&expected, &actual).expect("must diverge");
        assert_eq!(report.frame, 120);
        assert_eq!(report.expected_hash, "h120-expected");
        assert_eq!(report.actual_hash, "h120-actual");
    }

    #[test]
    fn find_first_divergence_returns_none_when_all_match() {
        let seq = vec![(0u64, "h0".to_string()), (60, "h60".to_string())];
        assert_eq!(find_first_divergence(&seq, &seq), None);
    }

    #[test]
    fn find_first_divergence_ignores_checkpoints_present_in_only_one_side() {
        let expected = vec![(0u64, "h0".to_string()), (60, "h60".to_string())];
        let actual = vec![(0u64, "h0".to_string())]; // bisection sampled fewer frames
        assert_eq!(find_first_divergence(&expected, &actual), None);
    }
}
