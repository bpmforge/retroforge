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
use rf_core_api::{ColorMathOp, CoreEvent, CoreSink, OverlayPixel, PixelLayer, PpuPixel, SubPixel};

use crate::palette::{bgr555_to_rgb, color_math_bgr555, resolve_index, LinePalette};

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
    /// Ticket W7-21: the sub-screen sent for each row this frame, with
    /// the row's fixed colour. Taken by that row's `video_scanline`, so a
    /// row the core sends no sub-screen for (math off) never blends with
    /// a previous frame's.
    subs: Vec<Option<(Vec<SubPixel>, u16)>>,
    /// Ticket W30-02: per pixel, "this is `PixelLayer::Background(0)` and
    /// no colour math touched it". `None` (the default) means capture is
    /// off: nothing is allocated or written.
    mode7_mask: Option<Vec<bool>>,
    /// Ticket W35-16: per pixel, the OAM index of the sprite that won it, `NO_SPRITE` for none.
    /// `None` (the default) means capture is off: nothing is allocated or written.
    sprite_ids: Option<Vec<u8>>,
}

/// [`FrameBuffer::sprite_ids`] value for a dot no sprite won.
pub const NO_SPRITE: u8 = 0xFF;

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
            subs: vec![None; height],
            mode7_mask: None,
            sprite_ids: None,
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

    /// Ticket W30-02: turn per-pixel BG1 mask capture on or off. Off (the
    /// default) costs nothing; on, every `video_scanline` also records
    /// whether the pixel is the BG1 layer with no colour math applied --
    /// the pixels an HD Mode 7 layer may replace without discarding a
    /// blend (see [`Self::mode7_mask`]). Turning it on starts all-false.
    pub fn set_mode7_mask_capture(&mut self, on: bool) {
        self.mode7_mask = on.then(|| vec![false; self.width * self.height]);
    }

    /// Ticket W30-02: `width() * height()` flags, row major, when capture
    /// is on. `true` = the pixel is `PixelLayer::Background(0)` and its
    /// colour-math operation was a no-op (none requested, or no sub-screen
    /// sent for the row); a pixel the sub-screen blended, or forced to
    /// black, is `false`. Rows not yet drawn this frame keep last frame's
    /// flags, like [`Self::rgba`].
    #[must_use]
    pub fn mode7_mask(&self) -> Option<&[bool]> {
        self.mode7_mask.as_deref()
    }

    /// Ticket W35-16: turn per-pixel sprite capture on or off. Off (the default) costs nothing; on,
    /// every `video_scanline` also records which OAM entry won each dot. Turning it on starts
    /// all-[`NO_SPRITE`].
    pub fn set_sprite_id_capture(&mut self, on: bool) {
        self.sprite_ids = on.then(|| vec![NO_SPRITE; self.width * self.height]);
    }

    /// Ticket W35-16: `width() * height()` OAM indices, row major, when capture is on
    /// ([`NO_SPRITE`] where no sprite won the dot). Rows not yet drawn this frame keep last frame's.
    #[must_use]
    pub fn sprite_ids(&self) -> Option<&[u8]> {
        self.sprite_ids.as_deref()
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
        let sub = self.subs[row].take();
        if let Some(mask) = &mut self.mode7_mask {
            // Pixels the core did not send this row are not BG1.
            mask[row * self.width..(row + 1) * self.width].fill(false);
        }
        if let Some(ids) = &mut self.sprite_ids {
            // Dots the core did not send this row are no sprite's.
            let row_ids = &mut ids[row * self.width..(row + 1) * self.width];
            row_ids.fill(NO_SPRITE);
            for (slot, pixel) in row_ids.iter_mut().zip(pixels) {
                if pixel.layer == PixelLayer::Sprite {
                    *slot = pixel.sprite_id.unwrap_or(NO_SPRITE);
                }
            }
        }
        for (x, pixel) in pixels.iter().enumerate().take(self.width) {
            if let Some(mask) = &mut self.mode7_mask {
                // Colour math only runs when a palette AND a sub-screen
                // exist for the row (the arm below); a no-op op or an
                // absent sub-screen leaves the pixel unblended.
                let blended = palette.is_some()
                    && sub
                        .as_ref()
                        .and_then(|(s, _)| s.get(x))
                        .is_some_and(|sp| sp.op != ColorMathOp::None || sp.main_black);
                mask[row * self.width + x] = pixel.layer == PixelLayer::Background(0) && !blended;
            }
            let [r, g, b] = match (
                palette,
                sub.as_ref().and_then(|(s, f)| Some((s.get(x)?, *f))),
            ) {
                // Ticket W7-21: colour math, in BGR555, before brightness.
                (Some(p), Some((sp, fixed_color))) => {
                    let main = if sp.main_black {
                        0
                    } else {
                        p.words[usize::from(pixel.palette_index)]
                    };
                    let sub_word = if sp.fixed {
                        fixed_color
                    } else {
                        p.words[usize::from(sp.palette_index)]
                    };
                    bgr555_to_rgb(color_math_bgr555(main, sub_word, sp.op), p.brightness)
                }
                _ => resolve_index(pixel.palette_index, palette),
            };
            let offset = row_start + x * 4;
            self.rgba[offset] = r;
            self.rgba[offset + 1] = g;
            self.rgba[offset + 2] = b;
            self.rgba[offset + 3] = 0xFF;
        }
    }

    /// Ticket W7-21: keep row `y`'s sub-screen for its `video_scanline`.
    fn sub_scanline(&mut self, y: u16, pixels: &[SubPixel], fixed_color: u16) {
        if let Some(slot) = self.subs.get_mut(usize::from(y)) {
            *slot = Some((pixels.to_vec(), fixed_color));
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
    /// Ticket W35-16: with sprite capture on, a dot records the OAM index of the sprite that won it
    /// (255 for a background dot), and capture never changes the picture.
    #[test]
    fn sprite_capture_records_the_winner_and_leaves_the_picture_alone() {
        let row = |sprite: Option<u8>| -> Vec<PpuPixel> {
            (0..NES_WIDTH)
                .map(|x| PpuPixel {
                    palette_index: 0x20,
                    layer: if sprite.is_some() && x % 2 == 0 {
                        PixelLayer::Sprite
                    } else {
                        PixelLayer::Background(0)
                    },
                    sprite_id: sprite.filter(|_| x % 2 == 0),
                    priority: 0,
                })
                .collect()
        };
        let mut plain = FrameBuffer::new();
        let mut capturing = FrameBuffer::new();
        assert!(capturing.sprite_ids().is_none(), "off by default");
        capturing.set_sprite_id_capture(true);
        for fb in [&mut plain, &mut capturing] {
            fb.video_scanline(3, &row(Some(9)));
            fb.video_scanline(4, &row(None));
        }
        assert_eq!(
            plain.rgba(),
            capturing.rgba(),
            "capture must not touch the picture"
        );
        let ids = capturing.sprite_ids().expect("on");
        assert_eq!(ids[3 * NES_WIDTH], 9);
        assert_eq!(ids[3 * NES_WIDTH + 1], NO_SPRITE);
        assert!(ids[4 * NES_WIDTH..5 * NES_WIDTH]
            .iter()
            .all(|&i| i == NO_SPRITE));
        // A row redrawn without the sprite forgets it.
        capturing.video_scanline(3, &row(None));
        assert_eq!(
            capturing.sprite_ids().expect("on")[3 * NES_WIDTH],
            NO_SPRITE
        );
        capturing.set_sprite_id_capture(false);
        assert!(capturing.sprite_ids().is_none());
    }

    use super::*;
    use crate::palette::palette_index_to_rgb;

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

    /// Ticket W30-02: the BG1 mask is opt-in, and a blended pixel is not in it.
    #[test]
    fn mode7_mask_is_off_by_default_and_excludes_blended_pixels() {
        let row = |layer| {
            vec![
                PpuPixel {
                    palette_index: 1,
                    layer,
                    sprite_id: None,
                    priority: 0,
                };
                NES_WIDTH
            ]
        };
        let sub = |op| {
            vec![
                SubPixel {
                    palette_index: 2,
                    layer: PixelLayer::Backdrop,
                    op,
                    fixed: false,
                    main_black: false,
                };
                NES_WIDTH
            ]
        };
        let mut fb = FrameBuffer::new();
        assert!(fb.mode7_mask().is_none(), "off by default");
        fb.video_scanline(0, &row(PixelLayer::Background(0)));
        assert!(fb.mode7_mask().is_none(), "no capture while off");

        fb.set_mode7_mask_capture(true);
        let words = [0u16; 256];
        for y in 0..4 {
            fb.palette_scanline(y, &words, 15);
        }
        // Row 0: BG1, no sub-screen. Row 1: BG2. Row 2: BG1 with a
        // blending op. Row 3: BG1 with a sub-screen whose op is a no-op.
        fb.video_scanline(0, &row(PixelLayer::Background(0)));
        fb.video_scanline(1, &row(PixelLayer::Background(1)));
        fb.sub_scanline(2, &sub(ColorMathOp::Add), 0);
        fb.video_scanline(2, &row(PixelLayer::Background(0)));
        fb.sub_scanline(3, &sub(ColorMathOp::None), 0);
        fb.video_scanline(3, &row(PixelLayer::Background(0)));
        let mask = fb.mode7_mask().expect("capture on");
        assert_eq!(mask.len(), NES_WIDTH * NES_HEIGHT);
        let at = |y: usize| mask[y * NES_WIDTH];
        assert!(at(0), "unblended BG1");
        assert!(!at(1), "not BG1");
        assert!(!at(2), "colour math applied");
        assert!(at(3), "no-op colour math counts as unblended");

        fb.set_mode7_mask_capture(false);
        assert!(fb.mode7_mask().is_none());
    }
}
