//! Criterion 3: "indexed pixels + metadata emitted via CoreSink" — proves
//! [`crate::ppu::Ppu::drain`] actually calls
//! [`rf_core_api::CoreSink::video_scanline`] with the expected per-scanline
//! data, using a recording test-only sink (no `EmulatorCore` wrapper exists
//! in this crate yet — see the `crate::ppu` module doc's scope fence).
use super::test_ppu;
use rf_core_api::{CoreEvent, CoreSink, PixelLayer, PpuPixel};

/// 341 dots/scanline (nesdev.org/wiki/PPU_rendering) — ticking this many
/// times completes exactly one scanline (`Ppu::tick`'s call k processes dot
/// k-1, so the 341st call processes dot 340, the last of the line, before
/// wrapping to the next scanline's dot 0).
const DOTS_PER_SCANLINE: u32 = 341;

#[derive(Default)]
struct RecordingSink {
    calls: Vec<(u16, Vec<PpuPixel>)>,
}

impl CoreSink for RecordingSink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.calls.push((y, pixels.to_vec()));
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

#[test]
fn one_completed_visible_scanline_emits_exactly_one_video_scanline_call() {
    let mut ppu = test_ppu();
    ppu.palette[0] = 0x0F; // universal backdrop color
                           // `Ppu::new` starts on the pre-render line (261); jump straight to the
                           // start of visible scanline 0 so one lap of `DOTS_PER_SCANLINE` ticks
                           // completes exactly the line under test.
    ppu.scanline = 0;
    ppu.dot = 0;
    // Rendering disabled (mask left at 0): every pixel is the backdrop.
    for _ in 0..DOTS_PER_SCANLINE {
        ppu.tick();
    }
    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);

    assert_eq!(sink.calls.len(), 1);
    let (y, pixels) = &sink.calls[0];
    assert_eq!(*y, 0);
    assert_eq!(pixels.len(), 256);
    for p in pixels {
        assert_eq!(p.palette_index, 0x0F);
        assert_eq!(p.layer, PixelLayer::Backdrop);
        assert_eq!(p.sprite_id, None);
        assert!(!p.dropped_by_limit);
    }
}

#[test]
fn drain_clears_the_queue_so_a_second_drain_is_empty() {
    let mut ppu = test_ppu();
    ppu.scanline = 0;
    ppu.dot = 0;
    for _ in 0..DOTS_PER_SCANLINE {
        ppu.tick();
    }
    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);
    assert_eq!(sink.calls.len(), 1);
    ppu.drain(&mut sink);
    assert_eq!(
        sink.calls.len(),
        1,
        "second drain must not re-emit anything"
    );
}

#[test]
fn a_full_frame_emits_exactly_240_scanlines_in_order() {
    let mut ppu = test_ppu();
    let dots_per_frame = 262u32 * DOTS_PER_SCANLINE;
    for _ in 0..dots_per_frame {
        ppu.tick();
    }
    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);

    assert_eq!(
        sink.calls.len(),
        240,
        "only the 240 visible lines are emitted"
    );
    for (i, (y, _)) in sink.calls.iter().enumerate() {
        assert_eq!(*y, i as u16, "scanlines drain in ascending order");
    }
}
