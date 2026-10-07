//! Side-by-side original/enhanced compare view (ticket W3-04;
//! FR-REND-005, `docs/design/RENDERER.md` §5).
//!
//! §5 describes two presentations — "split-screen (draggable divider) or
//! A/B blink" — and both are implemented here as **pure functions over
//! two RGBA buffers**, with no `egui` and no GPU. That is deliberate and
//! copies `crate::compare`'s sibling in the shell,
//! `retroforge::enhanced_view`: the branching is what has bugs, so the
//! branching is what gets tested headlessly, and the UI's only job is to
//! match on the result and paint it.
//!
//! ## "Same frame" is the requirement, and it is a sequencing claim
//!
//! FR-REND-005 says "synced frame". Both inputs to every function here
//! come from one `FrameMsg`, so synchronisation is structural rather than
//! something to keep in step: there is no path by which this module can
//! be handed an original from frame N and an enhanced from frame N-1,
//! because it never fetches either itself.
//!
//! ## Why the split composes a new buffer instead of clipping two draws
//!
//! Clipping two textures at a divider is what a UI toolkit would do, and
//! it would leave the compare view unable to answer the other half of
//! this ticket — FR-FE-005's screenshot. Composing the buffer means the
//! thing on screen and the thing captured are produced by one function,
//! so they cannot disagree. It also makes the divider testable by reading
//! pixels rather than by driving a UI.

/// Resolve a core's accuracy-exact indexed frame into RGBA — the
/// "original" half of the compare view.
///
/// The pairing this enables is what makes FR-REND-005's "synced frame"
/// structural rather than something to maintain: `FrameBundle::video` is
/// documented as "always the accuracy-exact stream: assembled from
/// `CoreSink::video_scanline` only, never `overlay_scanline`", while the
/// shell's displayed buffer is the same frame with the overlay painted
/// over it (`crate::frame::FrameBuffer::overlay_scanline`). One frame,
/// two renderings, the same 256x240 geometry — no scaling policy to
/// invent and no second fetch that could land a frame apart.
///
/// # Panics
/// Panics if `pixels` is not exactly `width * height` long.
#[must_use]
pub fn original_rgba_from_indexed(
    pixels: &[rf_core_api::PpuPixel],
    width: u32,
    height: u32,
) -> Vec<u8> {
    original_rgba_from_indexed_with(pixels, width, height, &[])
}

/// [`original_rgba_from_indexed`] with each row's written palette
/// (ticket W7-20): row `y` resolves through `palettes[y]` when it is
/// `Some`, else the NES table — so an SNES frame's "original" is in its
/// own colours. An empty slice is the NES behaviour.
///
/// # Panics
/// As [`original_rgba_from_indexed`].
#[must_use]
pub fn original_rgba_from_indexed_with(
    pixels: &[rf_core_api::PpuPixel],
    width: u32,
    height: u32,
    palettes: &[Option<crate::palette::LinePalette>],
) -> Vec<u8> {
    assert_eq!(
        pixels.len(),
        (width as usize) * (height as usize),
        "original_rgba_from_indexed: not {width}x{height} indexed pixels"
    );
    let mut out = Vec::with_capacity(pixels.len() * 4);
    for (row, line) in pixels.chunks(width.max(1) as usize).enumerate() {
        let palette = palettes.get(row).and_then(Option::as_ref);
        for px in line {
            let [r, g, b] = crate::palette::resolve_index(px.palette_index, palette);
            out.extend_from_slice(&[r, g, b, 0xFF]);
        }
    }
    out
}

/// How the compare view is presented (`RENDERER.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum CompareMode {
    /// Not comparing — the shell paints whatever it would normally.
    #[default]
    Off,
    /// Split screen: original on the left of `divider`, enhanced on the
    /// right. `divider` is a fraction of the width in `[0, 1]`.
    Split { divider: f32 },
    /// A/B blink: show one whole image, then the other, swapping every
    /// `period_frames` frames.
    Blink { period_frames: u32 },
}

/// Which single image an A/B blink should be showing at `frame_count`.
///
/// Separate from [`compose_split`] because blink does not compose
/// anything — it picks. Returning a bool rather than copying a buffer
/// keeps the "no allocation for the cheap mode" property obvious.
///
/// `period_frames` of 0 is treated as 1 rather than dividing by zero: a
/// degenerate setting should blink as fast as it can, not panic.
#[must_use]
pub fn blink_shows_original(frame_count: u64, period_frames: u32) -> bool {
    let period = u64::from(period_frames.max(1));
    (frame_count / period).is_multiple_of(2)
}

/// Compose a split-screen compare image: `original` left of the divider,
/// `enhanced` right of it.
///
/// Both buffers must be `width * height * 4` RGBA bytes — the compare
/// view is only meaningful for two renderings of the same frame at the
/// same geometry, and a caller that has one of each at different sizes
/// has a scaling decision to make first, which is not this function's to
/// guess at.
///
/// `divider` is clamped to `[0, 1]`, so a UI dragging past either edge
/// degrades to "all enhanced" / "all original" rather than panicking or
/// producing a torn image.
///
/// # Panics
/// Panics if either buffer's length does not match `width`x`height` —
/// a caller bug (mismatched geometry), not a runtime condition.
#[must_use]
pub fn compose_split(
    original: &[u8],
    enhanced: &[u8],
    width: u32,
    height: u32,
    divider: f32,
) -> Vec<u8> {
    let expected = (width as usize) * (height as usize) * 4;
    assert_eq!(
        original.len(),
        expected,
        "compose_split: original buffer is not {width}x{height} RGBA"
    );
    assert_eq!(
        enhanced.len(),
        expected,
        "compose_split: enhanced buffer is not {width}x{height} RGBA"
    );

    #[allow(clippy::cast_precision_loss)]
    let split_x = (divider.clamp(0.0, 1.0) * width as f32).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let split_x = (split_x as u32).min(width);

    let mut out = Vec::with_capacity(expected);
    for y in 0..height {
        let row = (y as usize) * (width as usize) * 4;
        let left_bytes = (split_x as usize) * 4;
        out.extend_from_slice(&original[row..row + left_bytes]);
        out.extend_from_slice(&enhanced[row + left_bytes..row + (width as usize) * 4]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter()
            .copied()
            .cycle()
            .take((width as usize) * (height as usize) * 4)
            .collect()
    }

    fn pixel(buf: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y as usize) * (width as usize) + (x as usize)) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    /// The accuracy stream really does resolve to the palette, and
    /// opaquely — a compare pane full of transparent pixels would look
    /// like a working split over a black original.
    #[test]
    fn indexed_pixels_resolve_through_the_palette_opaquely() {
        let px = |index: u8| rf_core_api::PpuPixel {
            palette_index: index,
            layer: rf_core_api::PixelLayer::Background(0),
            sprite_id: None,
            priority: 0,
        };
        let pixels = vec![px(0x21), px(0x0F)];
        let rgba = original_rgba_from_indexed(&pixels, 2, 1);
        assert_eq!(rgba.len(), 8);
        assert_eq!(&rgba[0..3], &crate::palette::palette_index_to_rgb(0x21));
        assert_eq!(rgba[3], 0xFF, "must be opaque");
        assert_eq!(&rgba[4..7], &crate::palette::palette_index_to_rgb(0x0F));
        assert_ne!(&rgba[0..3], &rgba[4..7], "two palette entries must differ");
    }

    #[test]
    fn split_puts_original_left_and_enhanced_right() {
        let (w, h) = (8, 4);
        let out = compose_split(&solid(w, h, RED), &solid(w, h, BLUE), w, h, 0.5);
        assert_eq!(out.len(), (w as usize) * (h as usize) * 4);
        for y in 0..h {
            assert_eq!(pixel(&out, w, 0, y), RED, "left edge must be the original");
            assert_eq!(pixel(&out, w, 3, y), RED, "just left of the divider");
            assert_eq!(pixel(&out, w, 4, y), BLUE, "just right of the divider");
            assert_eq!(pixel(&out, w, 7, y), BLUE, "right edge must be enhanced");
        }
    }

    /// The divider actually moves — otherwise "draggable" would be a
    /// label on a fixed 50/50 split.
    #[test]
    fn moving_the_divider_moves_the_boundary() {
        let (w, h) = (8, 2);
        let (orig, enh) = (solid(w, h, RED), solid(w, h, BLUE));
        let quarter = compose_split(&orig, &enh, w, h, 0.25);
        let three_quarter = compose_split(&orig, &enh, w, h, 0.75);
        // 0.25 * 8 = 2, so x=0..1 are original and x=2 is the first
        // enhanced column.
        assert_eq!(pixel(&quarter, w, 1, 0), RED);
        assert_eq!(pixel(&quarter, w, 2, 0), BLUE, "0.25 boundary is at x=2");
        assert_eq!(
            pixel(&three_quarter, w, 5, 0),
            RED,
            "0.75 boundary is at x=6"
        );
        assert_eq!(pixel(&three_quarter, w, 6, 0), BLUE);
        assert_ne!(quarter, three_quarter, "the divider must change the image");
    }

    /// Dragging past an edge degrades rather than tearing or panicking.
    #[test]
    fn a_divider_outside_the_frame_clamps_to_all_of_one_side() {
        let (w, h) = (4, 2);
        let (orig, enh) = (solid(w, h, RED), solid(w, h, BLUE));
        let all_enhanced = compose_split(&orig, &enh, w, h, -5.0);
        let all_original = compose_split(&orig, &enh, w, h, 5.0);
        assert_eq!(
            all_enhanced, enh,
            "divider <= 0 shows only the enhanced view"
        );
        assert_eq!(all_original, orig, "divider >= 1 shows only the original");
    }

    #[test]
    fn blink_alternates_on_its_period_and_never_divides_by_zero() {
        assert!(blink_shows_original(0, 30));
        assert!(blink_shows_original(29, 30));
        assert!(!blink_shows_original(30, 30), "swaps after one period");
        assert!(!blink_shows_original(59, 30));
        assert!(blink_shows_original(60, 30), "and swaps back");

        // Degenerate period: blink every frame rather than panic.
        assert!(blink_shows_original(0, 0));
        assert!(!blink_shows_original(1, 0));
    }
}
