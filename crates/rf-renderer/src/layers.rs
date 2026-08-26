//! BG/sprite layer extraction from `CoreSink` metadata (ticket W3-03).
//!
//! **Scope, read before extending this file:** this is *not* the
//! `SceneGraph`/`SceneLayer` type `docs/design/RENDERER.md` §3 describes —
//! that is `rf-enhance`'s `SceneComposer` output (ENHANCEMENT_RUNTIME.md
//! §7), arriving via W4-01's event bus, and does not exist anywhere in
//! `crates/` yet (`rf-enhance` is still a stub). This ticket's acceptance
//! is narrower and self-contained: split the `PpuPixel`s [`FrameBuffer`]
//! already resolves per scanline into two independently renderable RGBA
//! buffers, using only [`rf_core_api::PixelLayer`] — the metadata that
//! already exists (`PpuPixel::layer`, shipped W1-04a). Building a general
//! scene-graph type here would be exactly the "unvalidated scaffolding"
//! W1-02 declined to build for the `Mapper` trait: no second consumer
//! exists yet to inform its shape.
//!
//! [`LayeredFrame`] buckets every pixel into one of two RGBA buffers by
//! [`rf_core_api::PixelLayer`]:
//! - **bg**: [`PixelLayer::Backdrop`] and [`PixelLayer::Background`] (any
//!   plane index — the NES core always emits `Background(0)`, but nothing
//!   here assumes that).
//! - **sprite**: [`PixelLayer::Sprite`].
//!
//! Each buffer is transparent (`[0, 0, 0, 0]`) everywhere the *other*
//! layer drew this scanline, not merely "whatever was there before" — a
//! pixel this call routes to `sprite` is written opaque into `sprite_rgba`
//! **and** explicitly written transparent into `bg_rgba` at the same
//! offset, and vice versa. That per-pixel exclusivity is what makes a
//! debug view of either buffer alone show a genuinely isolated layer
//! (never a stale pixel left over from a previous frame's occupant at that
//! position) and is what the isolation test below actually exercises.
//!
//! Does **not** touch [`crate::frame::FrameBuffer::overlay_scanline`]
//! (ticket W3-05a's de-flicker overlay, active/in-use — plan.json W3-03's
//! forward note is explicit that migrating it is W3-05's job, not this
//! one's): [`LayeredFrame`] takes [`CoreSink::overlay_scanline`]'s default
//! no-op, so a dropped-sprite overlay never reaches either layer buffer
//! here.
//!
//! **Deliberate limitation, documented rather than papered over** (same
//! stance as `crate::stepper::EmuStepper::last_scanline`'s doc in the
//! `retroforge` crate): `CoreSink::video_scanline` carries exactly one
//! `PpuPixel` per x — the already-composited, occlusion-resolved hardware
//! output, not a per-layer plane. So `bg_rgba` is the background **minus
//! whatever a sprite occluded it this frame** (sprite-shaped holes, not
//! the full BG plane underneath), and `sprite_rgba` only ever shows
//! whichever single sprite/BG pixel won each position — never a sprite
//! drawn *behind* an opaque background pixel. This is a real information
//! loss relative to `docs/design/RENDERER.md` §3's `ExtractedBg`/
//! `SpriteSet`, which won't have this property once built (they read
//! tile/sprite atlases directly, not the composited stream) — not a bug
//! in this extraction, but a ceiling on what "from CoreSink metadata
//! alone" can ever recover.
use rf_core_api::{CoreEvent, CoreSink, PixelLayer, PpuPixel};

use crate::frame::{NES_HEIGHT, NES_WIDTH};
use crate::palette::palette_index_to_rgb;

/// A fully transparent RGBA texel — the "nothing from this layer at this
/// position" value both buffers start filled with and get explicitly
/// re-written to on every scanline (module doc).
const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

/// Two independently renderable RGBA layers extracted from the same
/// `CoreSink::video_scanline` stream [`crate::frame::FrameBuffer`]
/// consumes — `bg_rgba`/`sprite_rgba` below, each
/// `width * height * 4` bytes, row-major, top-to-bottom (module doc for
/// the exclusivity/transparency contract).
///
/// Carries its size for the same reason `FrameBuffer` does (ticket
/// W11-08): layer extraction is one of the consumers that assumed
/// 256x240, and a SNES or HD-pack frame would have had its layers
/// silently truncated to an NES-shaped rectangle.
pub struct LayeredFrame {
    bg_rgba: Vec<u8>,
    sprite_rgba: Vec<u8>,
    width: usize,
    height: usize,
}

impl LayeredFrame {
    /// Both layers start fully transparent, matching what a debug view
    /// should show before the first scanline of the first frame ever
    /// arrives (no accuracy-frame content to fall back on, unlike
    /// [`crate::frame::FrameBuffer::new`]'s opaque-black default).
    #[must_use]
    pub fn new() -> Self {
        Self::with_size(NES_WIDTH, NES_HEIGHT)
    }

    /// Layers of an arbitrary size (ticket W11-08), clamped to at least
    /// one pixel per axis for the same reason
    /// [`crate::frame::FrameBuffer::with_size`] is.
    #[must_use]
    pub fn with_size(width: usize, height: usize) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        LayeredFrame {
            bg_rgba: vec![0u8; width * height * 4],
            sprite_rgba: vec![0u8; width * height * 4],
            width,
            height,
        }
    }

    /// Row-major RGBA bytes for the background/backdrop layer only —
    /// opaque where a `Backdrop`/`Background(_)` pixel landed, transparent
    /// everywhere a `Sprite` pixel won that position instead.
    #[must_use]
    pub fn bg_rgba(&self) -> &[u8] {
        &self.bg_rgba
    }

    /// Row-major RGBA bytes for the sprite layer only — opaque where a
    /// `Sprite` pixel landed, transparent everywhere else.
    #[must_use]
    pub fn sprite_rgba(&self) -> &[u8] {
        &self.sprite_rgba
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }
}

impl Default for LayeredFrame {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreSink for LayeredFrame {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        let row = y as usize;
        if row >= self.height {
            // Same "degrade, never index out of bounds" stance as
            // `FrameBuffer::video_scanline` — a core emitting an
            // out-of-range scanline is a core bug, not a renderer panic.
            return;
        }
        let row_start = row * self.width * 4;
        for (x, pixel) in pixels.iter().enumerate().take(self.width) {
            let offset = row_start + x * 4;
            let resolved = {
                let [r, g, b] = palette_index_to_rgb(pixel.palette_index);
                [r, g, b, 0xFF]
            };
            // Route to exactly one buffer as opaque and the other as
            // explicitly transparent (module doc's exclusivity contract) —
            // never leave the non-matching buffer holding stale content.
            if matches!(pixel.layer, PixelLayer::Sprite) {
                self.sprite_rgba[offset..offset + 4].copy_from_slice(&resolved);
                self.bg_rgba[offset..offset + 4].copy_from_slice(&TRANSPARENT);
            } else {
                debug_assert!(matches!(
                    pixel.layer,
                    PixelLayer::Backdrop | PixelLayer::Background(_)
                ));
                self.bg_rgba[offset..offset + 4].copy_from_slice(&resolved);
                self.sprite_rgba[offset..offset + 4].copy_from_slice(&TRANSPARENT);
            }
        }
    }

    fn audio(&mut self, _samples: &[i16]) {
        // Video-only sink, same as `FrameBuffer` — audio is a later ticket.
    }

    fn event(&mut self, _ev: CoreEvent) {
        // No subscriber, same as `FrameBuffer` — inert until something
        // opts into `EventMask`.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(layer: PixelLayer, palette_index: u8) -> PpuPixel {
        PpuPixel {
            palette_index,
            layer,
            sprite_id: match layer {
                PixelLayer::Sprite => Some(0),
                _ => None,
            },
            priority: 0,
        }
    }

    fn solid_row(layer: PixelLayer, palette_index: u8) -> Vec<PpuPixel> {
        vec![pixel(layer, palette_index); NES_WIDTH]
    }

    #[test]
    fn new_frame_is_fully_transparent_on_both_layers() {
        let lf = LayeredFrame::new();
        assert_eq!(lf.bg_rgba().len(), NES_WIDTH * NES_HEIGHT * 4);
        assert_eq!(lf.sprite_rgba().len(), NES_WIDTH * NES_HEIGHT * 4);
        for px in lf.bg_rgba().chunks_exact(4) {
            assert_eq!(px, TRANSPARENT);
        }
        for px in lf.sprite_rgba().chunks_exact(4) {
            assert_eq!(px, TRANSPARENT);
        }
    }

    /// The core isolation property, ticket-acceptance-critical: a BG pixel
    /// must appear (opaque) in the BG layer and must NOT appear in the
    /// sprite layer at the same position.
    #[test]
    fn background_pixel_appears_in_bg_layer_and_not_in_sprite_layer() {
        let mut lf = LayeredFrame::new();
        lf.video_scanline(5, &solid_row(PixelLayer::Background(0), 0x20)); // $20 = white
        let row_start = 5 * NES_WIDTH * 4;
        assert_eq!(
            &lf.bg_rgba()[row_start..row_start + 4],
            &[255, 255, 255, 255],
            "bg layer must carry the resolved background color"
        );
        assert_eq!(
            &lf.sprite_rgba()[row_start..row_start + 4],
            &TRANSPARENT,
            "sprite layer must stay transparent where only a background pixel drew"
        );
    }

    /// The mirror-image property: a sprite pixel must appear (opaque) in
    /// the sprite layer and must NOT appear in the BG layer.
    #[test]
    fn sprite_pixel_appears_in_sprite_layer_and_not_in_bg_layer() {
        let mut lf = LayeredFrame::new();
        lf.video_scanline(5, &solid_row(PixelLayer::Sprite, 0x16)); // $16 = red-ish
        let row_start = 5 * NES_WIDTH * 4;
        let expected = palette_index_to_rgb(0x16);
        assert_eq!(
            &lf.sprite_rgba()[row_start..row_start + 3],
            &expected,
            "sprite layer must carry the resolved sprite color"
        );
        assert_eq!(lf.sprite_rgba()[row_start + 3], 0xFF);
        assert_eq!(
            &lf.bg_rgba()[row_start..row_start + 4],
            &TRANSPARENT,
            "bg layer must stay transparent where only a sprite pixel drew"
        );
    }

    /// `Backdrop` (no bg/sprite pixel opaque here) buckets with the BG
    /// layer, not the sprite layer — it is the universal backdrop plane,
    /// never sprite content.
    #[test]
    fn backdrop_pixel_buckets_into_the_bg_layer() {
        let mut lf = LayeredFrame::new();
        lf.video_scanline(0, &solid_row(PixelLayer::Backdrop, 0x0F)); // $0F = near-black
        let expected = palette_index_to_rgb(0x0F);
        assert_eq!(&lf.bg_rgba()[0..3], &expected);
        assert_eq!(lf.bg_rgba()[3], 0xFF);
        assert_eq!(&lf.sprite_rgba()[0..4], &TRANSPARENT);
    }

    /// A mixed row (half BG, half sprite) must split cleanly at the pixel
    /// boundary — proves the routing is per-pixel, not per-scanline.
    #[test]
    fn mixed_row_splits_bg_and_sprite_pixels_independently() {
        let mut lf = LayeredFrame::new();
        let mut row = solid_row(PixelLayer::Background(0), 0x20);
        row[10] = pixel(PixelLayer::Sprite, 0x16);
        lf.video_scanline(0, &row);

        let bg_offset = 9 * 4; // neighbor stays background
        assert_eq!(
            &lf.bg_rgba()[bg_offset..bg_offset + 4],
            &[255, 255, 255, 255]
        );
        assert_eq!(&lf.sprite_rgba()[bg_offset..bg_offset + 4], &TRANSPARENT);

        let sprite_offset = 10 * 4;
        let expected = palette_index_to_rgb(0x16);
        assert_eq!(
            &lf.sprite_rgba()[sprite_offset..sprite_offset + 3],
            &expected
        );
        assert_eq!(
            &lf.bg_rgba()[sprite_offset..sprite_offset + 4],
            &TRANSPARENT
        );
    }

    #[test]
    fn out_of_range_scanline_is_dropped_not_panicking() {
        let mut lf = LayeredFrame::new();
        lf.video_scanline(NES_HEIGHT as u16, &solid_row(PixelLayer::Sprite, 0x16));
        lf.video_scanline(u16::MAX, &solid_row(PixelLayer::Sprite, 0x16));
        // No panic reaching here is the assertion; buffers stay untouched.
        assert_eq!(lf.bg_rgba()[3], 0);
        assert_eq!(lf.sprite_rgba()[3], 0);
    }

    #[test]
    fn extra_pixels_past_nes_width_are_ignored() {
        let mut lf = LayeredFrame::new();
        let mut row = solid_row(PixelLayer::Background(0), 0x20);
        row.push(pixel(PixelLayer::Sprite, 0x16));
        // Must not panic despite `row.len() == NES_WIDTH + 1`.
        lf.video_scanline(0, &row);
    }

    #[test]
    fn default_matches_new() {
        let a = LayeredFrame::default();
        let b = LayeredFrame::new();
        assert_eq!(a.bg_rgba(), b.bg_rgba());
        assert_eq!(a.sprite_rgba(), b.sprite_rgba());
    }
}
