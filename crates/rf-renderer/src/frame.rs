//! Indexed-pixels-in, RGBA-out frame accumulator (ticket W1-06).
//!
//! `rf-nes` (`NesBus::drain_video`) pushes one [`CoreSink::video_scanline`]
//! call per completed scanline, each carrying `PpuPixel`s that are
//! **indexed color + metadata, never RGB** (project law — see
//! `rf_core_api::video`'s module doc). [`FrameBuffer`] is the frontend-side
//! `CoreSink` that resolves those indices to RGBA via
//! [`crate::palette::palette_index_to_rgb`] and accumulates a whole frame
//! for the CPU-blit path acceptance criterion 1 allows ("CPU blit
//! acceptable pre-W3"). It does not touch the network, a GPU device, or
//! `egui` at all — the `retroforge` crate's app layer is the only thing
//! that turns [`FrameBuffer::rgba`] into an `egui::ColorImage`/texture.
use rf_core_api::{CoreEvent, CoreSink, OverlayPixel, PpuPixel};

use crate::palette::{resolve_index, LinePalette};

/// NES visible frame width in pixels (nesdev.org/wiki/PPU_rendering: 256
/// dots of visible output per scanline).
pub const NES_WIDTH: usize = 256;
/// NES visible frame height in pixels (240 visible scanlines;
/// nesdev.org/wiki/PPU_rendering).
pub const NES_HEIGHT: usize = 240;

/// One accumulated RGBA frame, `width * height * 4` bytes, row major,
/// top-to-bottom, alpha always opaque (`0xFF`) — the NES PPU has no
/// transparency concept at the frame-output boundary.
///
/// **The size is carried, not assumed** (ticket W11-08). Until then both
/// dimensions were the `NES_WIDTH`/`NES_HEIGHT` constants, and that one
/// fact blocked three separate features at once: widescreen needs more
/// than 256 columns, an HD pack needs a frame `HdPack::scale` times
/// larger, and SNES needs 256x224 — 512 wide in hires modes 5/6, which
/// the SNES core has emitted since W7-06 while the shell could not
/// represent it.
///
/// [`FrameBuffer::new`] still produces an NES-sized buffer, so every
/// existing caller keeps the frame it had.
pub struct FrameBuffer {
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    /// Ticket W7-20: each row's written palette, when the core sent one
    /// (`CoreSink::palette_scanline`); `None` rows use the NES table.
    palettes: Vec<Option<LinePalette>>,
}

impl FrameBuffer {
    /// A buffer pre-filled with opaque black, matching what an unbooted
    /// display shows before the first scanline ever arrives.
    #[must_use]
    pub fn new() -> Self {
        Self::with_size(NES_WIDTH, NES_HEIGHT)
    }

    /// A buffer of an arbitrary size (ticket W11-08).
    ///
    /// Zero in either axis is clamped to one rather than producing an
    /// empty buffer: every consumer indexes by `row * width * 4`, and a
    /// zero-sized frame turns that into a silent no-op that looks
    /// exactly like a core emitting nothing.
    #[must_use]
    pub fn with_size(width: usize, height: usize) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        let mut rgba = vec![0u8; width * height * 4];
        // Alpha channel only; RGB already zeroed by `vec!`.
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 0xFF;
        }
        FrameBuffer {
            rgba,
            palettes: vec![None; height],
            width,
            height,
        }
    }

    /// Row-major RGBA bytes, `width() * height() * 4` long.
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Ticket W7-20: the palette each row was resolved through (`None`
    /// for the NES table), so a consumer resolving the same indices again
    /// — the compare view's "original" — gets the same colours.
    #[must_use]
    pub fn line_palettes(&self) -> &[Option<LinePalette>] {
        &self.palettes
    }

    /// Copy of [`Self::rgba`] for callers (e.g. a cross-thread channel)
    /// that need an owned, 'static buffer rather than a borrow tied to
    /// this `FrameBuffer`'s lifetime.
    #[must_use]
    pub fn to_vec(&self) -> Vec<u8> {
        self.rgba.clone()
    }
}

impl Default for FrameBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreSink for FrameBuffer {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        let row = y as usize;
        if row >= self.height {
            // A core emitting an out-of-range scanline index is a core
            // bug, not a reason for the renderer to panic/index out of
            // bounds — drop it silently, same "degrade, never crash"
            // stance as `palette_index_to_rgb`'s masking.
            return;
        }
        let row_start = row * self.width * 4;
        let palette = self.palettes[row].as_ref();
        for (x, pixel) in pixels.iter().enumerate().take(self.width) {
            let [r, g, b] = resolve_index(pixel.palette_index, palette);
            let offset = row_start + x * 4;
            self.rgba[offset] = r;
            self.rgba[offset + 1] = g;
            self.rgba[offset + 2] = b;
            self.rgba[offset + 3] = 0xFF;
        }
    }

    /// Ticket W7-20: remember the palette row `y` is resolved through.
    /// Arrives before that row's `video_scanline` (`rf_snes::core`).
    fn palette_scanline(&mut self, y: u16, palette: &[u16], brightness: u8) {
        if let Some(slot) = self.palettes.get_mut(usize::from(y)) {
            *slot = Some(LinePalette::from_words(palette, brightness));
        }
    }

    /// Ticket W3-05a: composite the sprite-limit-bypass overlay on top of
    /// whatever [`Self::video_scanline`] already resolved for this same row
    /// — `drain()`'s call ordering (`crate::ppu`'s `Ppu::drain`) guarantees
    /// `video_scanline` for row `y` always runs first, so this always has
    /// the accuracy pixel already in place to paint over. Only `opaque`
    /// pixels write anything; a transparent one leaves the accuracy pixel
    /// (background or backdrop — never a real sprite, `Ppu::overlay_pixel`'s
    /// own priority rules already guarantee that) untouched.
    fn overlay_scanline(&mut self, y: u16, pixels: &[OverlayPixel]) {
        let row = y as usize;
        if row >= self.height {
            return; // same "degrade, never crash" stance as video_scanline.
        }
        let row_start = row * self.width * 4;
        for (x, pixel) in pixels.iter().enumerate().take(self.width) {
            if !pixel.opaque {
                continue;
            }
            let [r, g, b] = resolve_index(pixel.palette_index, self.palettes[row].as_ref());
            let offset = row_start + x * 4;
            self.rgba[offset] = r;
            self.rgba[offset + 1] = g;
            self.rgba[offset + 2] = b;
            self.rgba[offset + 3] = 0xFF;
        }
    }

    fn audio(&mut self, _samples: &[i16]) {
        // Video-only sink (ticket W1-06 scope); audio output is a later
        // ticket (rf-audio).
    }

    fn event(&mut self, _ev: CoreEvent) {
        // No subscriber (`EventMask::NONE` default, `CoreConfig::default`)
        // — `rf-nes` never constructs a `CoreEvent` for us to receive
        // unless something opts in, so this is intentionally inert.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::palette_index_to_rgb;
    use rf_core_api::PixelLayer;

    fn solid_row(index: u8) -> Vec<PpuPixel> {
        vec![
            PpuPixel {
                palette_index: index,
                layer: PixelLayer::Background(0),
                sprite_id: None,
                priority: 0,
            };
            NES_WIDTH
        ]
    }

    #[test]
    fn new_buffer_is_opaque_black() {
        let fb = FrameBuffer::new();
        assert_eq!(fb.rgba().len(), NES_WIDTH * NES_HEIGHT * 4);
        for px in fb.rgba().chunks_exact(4) {
            assert_eq!(px, [0, 0, 0, 0xFF]);
        }
    }

    #[test]
    fn video_scanline_writes_resolved_rgb_into_the_right_row() {
        let mut fb = FrameBuffer::new();
        fb.video_scanline(5, &solid_row(0x20)); // $20 = white
        let row_start = 5 * NES_WIDTH * 4;
        assert_eq!(&fb.rgba()[row_start..row_start + 4], &[255, 255, 255, 255]);
        // Row 0 (untouched) must still be black.
        assert_eq!(&fb.rgba()[0..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn out_of_range_scanline_is_dropped_not_panicking() {
        let mut fb = FrameBuffer::new();
        fb.video_scanline(NES_HEIGHT as u16, &solid_row(0x20)); // one past the last row
        fb.video_scanline(u16::MAX, &solid_row(0x20));
        // No panic reaching here is the assertion; buffer stays untouched.
        assert_eq!(fb.rgba()[3], 0xFF);
    }

    #[test]
    fn extra_pixels_past_nes_width_are_ignored() {
        let mut fb = FrameBuffer::new();
        let mut row = solid_row(0x20);
        row.push(PpuPixel {
            palette_index: 0x16,
            layer: PixelLayer::Background(0),
            sprite_id: None,
            priority: 0,
        });
        // Must not panic despite `row.len() == NES_WIDTH + 1`.
        fb.video_scanline(0, &row);
    }

    fn transparent_overlay_row() -> Vec<OverlayPixel> {
        vec![
            OverlayPixel {
                palette_index: 0,
                opaque: false
            };
            NES_WIDTH
        ]
    }

    /// Ticket W3-05a: an opaque overlay pixel must paint OVER whatever
    /// `video_scanline` already resolved for that row — the visible
    /// "de-flicker" behavior this whole ticket exists for.
    #[test]
    fn overlay_scanline_paints_opaque_pixels_over_the_accuracy_frame() {
        let mut fb = FrameBuffer::new();
        fb.video_scanline(5, &solid_row(0x20)); // accuracy frame: white
        let mut overlay = transparent_overlay_row();
        overlay[3] = OverlayPixel {
            palette_index: 0x16,
            opaque: true,
        };
        fb.overlay_scanline(5, &overlay);

        let row_start = 5 * NES_WIDTH * 4;
        let expected = palette_index_to_rgb(0x16);
        assert_eq!(
            &fb.rgba()[row_start + 3 * 4..row_start + 3 * 4 + 3],
            &expected,
            "the one opaque overlay pixel must overwrite the accuracy pixel underneath"
        );
        // Every other x on the row is untouched (still the accuracy white).
        assert_eq!(&fb.rgba()[row_start..row_start + 4], &[255, 255, 255, 255]);
        assert_eq!(
            &fb.rgba()[row_start + 4 * 4..row_start + 4 * 4 + 4],
            &[255, 255, 255, 255]
        );
    }

    /// An all-transparent overlay row (the default when the overlay is
    /// disabled — `Ppu`'s own module doc) must leave the accuracy frame
    /// completely unchanged, proving `opaque: false` never becomes a
    /// sentinel color drawn anyway.
    #[test]
    fn overlay_scanline_all_transparent_leaves_the_accuracy_frame_untouched() {
        let mut fb = FrameBuffer::new();
        fb.video_scanline(5, &solid_row(0x20));
        let before = fb.to_vec();
        fb.overlay_scanline(5, &transparent_overlay_row());
        assert_eq!(
            fb.to_vec(),
            before,
            "an all-transparent overlay row must not change a single byte"
        );
    }

    #[test]
    fn overlay_scanline_out_of_range_is_dropped_not_panicking() {
        let mut fb = FrameBuffer::new();
        let mut overlay = transparent_overlay_row();
        overlay[0] = OverlayPixel {
            palette_index: 0x16,
            opaque: true,
        };
        fb.overlay_scanline(NES_HEIGHT as u16, &overlay);
        fb.overlay_scanline(u16::MAX, &overlay);
        // No panic reaching here is the assertion; buffer stays untouched.
        assert_eq!(fb.rgba()[3], 0xFF);
    }
}

/// Ticket W11-08's own tests: the buffer carries its size.
#[cfg(test)]
mod variable_size_tests {
    use super::*;
    use crate::palette::palette_index_to_rgb;
    use rf_core_api::PixelLayer;

    /// **The default is unchanged.** Every existing caller uses `new()`,
    /// and this change must not move the NES frame by a byte — the
    /// determinism goldens and the blargg/nestest suites all flow through
    /// this buffer.
    #[test]
    fn new_is_still_exactly_an_nes_frame() {
        let fb = FrameBuffer::new();
        assert_eq!((fb.width(), fb.height()), (NES_WIDTH, NES_HEIGHT));
        assert_eq!(fb.rgba().len(), NES_WIDTH * NES_HEIGHT * 4);
        assert!(
            fb.rgba().chunks_exact(4).all(|p| p == [0, 0, 0, 0xFF]),
            "an unbooted display is opaque black, as it was before W11-08"
        );
    }

    /// A SNES frame: 256x224, the size the app could not represent.
    #[test]
    fn a_snes_sized_buffer_addresses_its_own_last_row() {
        let mut fb = FrameBuffer::with_size(256, 224);
        assert_eq!((fb.width(), fb.height()), (256, 224));
        let row: Vec<PpuPixel> = (0..256)
            .map(|_| PpuPixel {
                palette_index: 0x16,
                layer: PixelLayer::Background(0),
                sprite_id: None,
                priority: 0,
            })
            .collect();
        // Row 223 exists here and does NOT in a 240-high buffer's terms —
        // the point being that the bound is the buffer's own, not a
        // constant.
        fb.video_scanline(223, &row);
        let want = palette_index_to_rgb(0x16);
        let o = 223 * 256 * 4;
        assert_eq!([fb.rgba()[o], fb.rgba()[o + 1], fb.rgba()[o + 2]], want);
    }

    /// A hires SNES frame is 512 dots wide — the width the SNES core has
    /// emitted since W7-06 while the shell could not hold it.
    #[test]
    fn a_hires_width_buffer_accepts_a_512_dot_scanline() {
        let mut fb = FrameBuffer::with_size(512, 224);
        let row: Vec<PpuPixel> = (0..512)
            .map(|i| PpuPixel {
                palette_index: u8::try_from(i % 64).unwrap_or(0),
                layer: PixelLayer::Background(0),
                sprite_id: None,
                priority: 0,
            })
            .collect();
        fb.video_scanline(0, &row);
        // The 511th dot must have landed: a 256-wide buffer would have
        // dropped everything past 255 and this is what would have caught
        // that silently-halved frame.
        let want = palette_index_to_rgb(u8::try_from(511 % 64).unwrap());
        let o = 511 * 4;
        assert_eq!([fb.rgba()[o], fb.rgba()[o + 1], fb.rgba()[o + 2]], want);
    }

    /// An HD-pack-scale frame: 4x an NES frame.
    #[test]
    fn an_hd_pack_scale_buffer_is_the_size_it_says() {
        let fb = FrameBuffer::with_size(NES_WIDTH * 4, NES_HEIGHT * 4);
        assert_eq!(fb.rgba().len(), 1024 * 960 * 4);
    }

    /// Zero is clamped rather than producing an empty buffer, because
    /// every consumer indexes by `row * width * 4` and a zero-sized frame
    /// turns that into a silent no-op indistinguishable from a core
    /// emitting nothing.
    #[test]
    fn a_zero_sized_request_is_clamped_not_empty() {
        let fb = FrameBuffer::with_size(0, 0);
        assert_eq!((fb.width(), fb.height()), (1, 1));
        assert_eq!(fb.rgba().len(), 4);
    }
}
