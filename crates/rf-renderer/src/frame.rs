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
use rf_core_api::{CoreEvent, CoreSink, PpuPixel};

use crate::palette::palette_index_to_rgb;

/// NES visible frame width in pixels (nesdev.org/wiki/PPU_rendering: 256
/// dots of visible output per scanline).
pub const NES_WIDTH: usize = 256;
/// NES visible frame height in pixels (240 visible scanlines;
/// nesdev.org/wiki/PPU_rendering).
pub const NES_HEIGHT: usize = 240;

/// One accumulated RGBA frame, `NES_WIDTH * NES_HEIGHT * 4` bytes, row
/// major, top-to-bottom, alpha always opaque (`0xFF`) — the NES PPU has no
/// transparency concept at the frame-output boundary.
pub struct FrameBuffer {
    rgba: Vec<u8>,
}

impl FrameBuffer {
    /// A buffer pre-filled with opaque black, matching what an unbooted
    /// display shows before the first scanline ever arrives.
    #[must_use]
    pub fn new() -> Self {
        let mut rgba = vec![0u8; NES_WIDTH * NES_HEIGHT * 4];
        // Alpha channel only; RGB already zeroed by `vec!`.
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 0xFF;
        }
        FrameBuffer { rgba }
    }

    /// Row-major RGBA bytes, `width() * height() * 4` long.
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    #[must_use]
    pub fn width(&self) -> usize {
        NES_WIDTH
    }

    #[must_use]
    pub fn height(&self) -> usize {
        NES_HEIGHT
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
        if row >= NES_HEIGHT {
            // A core emitting an out-of-range scanline index is a core
            // bug, not a reason for the renderer to panic/index out of
            // bounds — drop it silently, same "degrade, never crash"
            // stance as `palette_index_to_rgb`'s masking.
            return;
        }
        let row_start = row * NES_WIDTH * 4;
        for (x, pixel) in pixels.iter().enumerate().take(NES_WIDTH) {
            let [r, g, b] = palette_index_to_rgb(pixel.palette_index);
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
    use rf_core_api::PixelLayer;

    fn solid_row(index: u8) -> Vec<PpuPixel> {
        vec![
            PpuPixel {
                palette_index: index,
                layer: PixelLayer::Background(0),
                sprite_id: None,
                priority: 0,
                dropped_by_limit: false,
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
            dropped_by_limit: false,
        });
        // Must not panic despite `row.len() == NES_WIDTH + 1`.
        fb.video_scanline(0, &row);
    }
}
