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

#[test]
fn undrained_frames_accumulate_rather_than_silently_dropping_scanlines() {
    // The module doc's `completed` queue is preallocated for one frame but
    // NOT capped — two frames without an intervening `drain` must yield
    // 480 rows, not 240 (which would mean the second frame silently
    // overwrote or dropped the first).
    let mut ppu = test_ppu();
    let dots_per_frame = 262u32 * DOTS_PER_SCANLINE;
    for _ in 0..(dots_per_frame * 2) {
        ppu.tick();
    }
    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);

    assert_eq!(
        sink.calls.len(),
        480,
        "two undrained frames' worth of scanlines must both survive"
    );
    for (i, (y, _)) in sink.calls.iter().enumerate() {
        assert_eq!(
            *y,
            (i % 240) as u16,
            "each frame's 240 lines still run 0..239"
        );
    }
}

#[test]
fn visible_scanline_pixels_resolve_through_the_full_bg_pipeline_once_warmed_up() {
    // Criterion 3's non-backdrop path: pattern bits + attribute bits must
    // actually resolve to a real palette-RAM value with `Background(0)`
    // layer, not just the all-zero/backdrop case the other tests in this
    // file exercise.
    let mut ppu = test_ppu();
    ppu.write_register(1, 0x08); // PPUMASK bit 3: show background
    ppu.scanline = 0;
    ppu.dot = 0;
    ppu.v = 0x0000;

    // Uniform nametable (every tile = 5) and uniform attribute (every
    // 2-bit quadrant = 1) so tile/attribute-block boundaries don't matter.
    for addr in 0x2000u16..=0x23BF {
        ppu.mem_write(addr, 0x05);
    }
    for addr in 0x23C0u16..=0x23FF {
        ppu.mem_write(addr, 0b0101_0101);
    }
    // Uniform pattern bits too (every bit position yields pattern = 1),
    // so this test doesn't depend on exactly which shift-register bit
    // fine-X selects.
    ppu.chr[0x50] = 0xFF;
    ppu.chr[0x58] = 0x00;
    ppu.palette[0x05] = 0x2A; // (attr=1 << 2) | pattern=1 = address 5

    for _ in 0..DOTS_PER_SCANLINE {
        ppu.tick();
    }
    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);
    let (_, pixels) = &sink.calls[0];

    // Pixels 0-15 are this artificially-started scanline's own pipeline
    // warm-up (no prefetch ran on a preceding line — see
    // `fetch_pipeline.rs`'s dot-9-reload timing doc): the shift registers
    // only finish loading real data by dot 17 (pixel 16). Check the
    // steady state from there on, where every tile is identical anyway.
    for (x, p) in pixels.iter().enumerate().skip(16) {
        assert_eq!(p.palette_index, 0x2A, "pixel {x}");
        assert_eq!(p.layer, PixelLayer::Background(0), "pixel {x}");
    }
}

#[test]
fn transparent_pixels_use_the_universal_backdrop_never_the_tiles_own_attribute() {
    // The trap `background.rs::output_pixel` explicitly guards against:
    // pattern bits = 00 must resolve to palette address 0 (the universal
    // backdrop), never `attribute << 2 | 0`, even when the tile's own
    // attribute is non-zero.
    let mut ppu = test_ppu();
    ppu.write_register(1, 0x08);
    ppu.scanline = 0;
    ppu.dot = 0;
    ppu.v = 0x0000;

    for addr in 0x2000u16..=0x23BF {
        ppu.mem_write(addr, 0x05);
    }
    for addr in 0x23C0u16..=0x23FF {
        ppu.mem_write(addr, 0xFF); // every quadrant = 3 (non-zero)
    }
    ppu.chr[0x50] = 0x00; // pattern bits always 00: transparent
    ppu.chr[0x58] = 0x00;
    ppu.palette[0x00] = 0x11; // universal backdrop
    ppu.palette[0x0C] = 0x22; // (attr=3 << 2) | 0 — must NEVER be read

    for _ in 0..DOTS_PER_SCANLINE {
        ppu.tick();
    }
    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);
    let (_, pixels) = &sink.calls[0];

    for (x, p) in pixels.iter().enumerate().skip(16) {
        assert_eq!(
            p.palette_index, 0x11,
            "pixel {x} must read the backdrop, never attr<<2|0"
        );
        assert_eq!(p.layer, PixelLayer::Backdrop, "pixel {x}");
    }
}
