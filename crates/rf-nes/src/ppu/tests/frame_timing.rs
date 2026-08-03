//! Ticket W1-04b, acceptance criterion 1: the odd/even-frame idle-dot skip
//! (nesdev.org/wiki/PPU_rendering's "odd frame" note — quoted in full in
//! [`crate::ppu::Ppu::tick`]'s doc) and, per the W1-04a-to-W1-04b handoff
//! (`plan.json`'s W1-04b notes), the ONE thing W1-04a's own tests
//! deliberately left unproven: whether the shift registers are correctly
//! primed by a REAL pre-render line's dots 321-336 prefetch, so a fresh
//! scanline's pixel 0 shows the right tile.
//!
//! `fetch_pipeline.rs`'s tests explicitly start mid-scanline (an artificial
//! `scanline=0, dot=0` with no preceding prefetch) and skip pixels 0-15 for
//! exactly this reason (see that file's module doc). This file instead
//! starts from [`crate::ppu::Ppu::new`]'s real starting position (the
//! pre-render line, dot 0) and lets [`crate::ppu::Ppu::tick`] alone drive
//! the pipeline across the scanline boundary — nothing is poked mid-flight.
use crate::ppu::Ppu;
use rf_core_api::{CoreEvent, CoreSink, PixelLayer, PpuPixel};

use super::test_ppu;

/// 341 dots/scanline, 262 scanlines/frame (nesdev.org/wiki/PPU_rendering).
const DOTS_PER_SCANLINE: u32 = 341;
const SCANLINES_PER_FRAME: u32 = 262;
/// The pre-render scanline (see `crate::ppu`'s own `PRERENDER_SCANLINE`;
/// redefined locally rather than reaching into the parent module's private
/// const, matching `sink_emission.rs`'s existing convention of a local
/// `DOTS_PER_SCANLINE`).
const PRERENDER_SCANLINE: u16 = 261;
/// The dot the odd-frame skip fires on (see `Ppu::tick`'s doc).
const ODD_FRAME_SKIP_DOT: u16 = 339;

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

/// Tick `ppu` until it lands on `(scanline, dot) == (0, 0)`, returning how
/// many `tick()` calls that took — the frame's real dot count (341*262, or
/// one less on a skipped odd frame).
fn ticks_until_scanline_0_dot_0(ppu: &mut Ppu) -> u32 {
    let mut n = 0u32;
    loop {
        ppu.tick();
        n += 1;
        if ppu.scanline == 0 && ppu.dot == 0 {
            return n;
        }
        assert!(
            n < DOTS_PER_SCANLINE * SCANLINES_PER_FRAME * 2,
            "frame never wrapped"
        );
    }
}

#[test]
fn pre_render_lines_321_336_prefetch_correctly_primes_scanline_0s_first_two_tiles() {
    // The oracle W1-04a's own tests couldn't provide: tick from a REAL
    // power-on position (pre-render line, dot 0) through the pre-render
    // line's own dots 321-336 prefetch and into scanline 0, and check
    // pixels 0-15 land on the tiles the nametable actually names.
    let mut ppu = test_ppu();
    ppu.write_register(1, 0x0A); // PPUMASK: show background + show in leftmost 8px
                                 // v/t are already 0 from `Ppu::new` (coarse X/Y=0, fine Y=0, NT
                                 // select=0) -- already scanline 0, column 0's starting position,
                                 // no manual v/t poke needed.

    // Nametable column 0 = tile 5, column 1 = tile 6 (NT addr = 0x2000 |
    // coarse X, since coarse Y/NT-select are both 0 here) -- same layout
    // `fetch_pipeline.rs`'s own single-tile test uses.
    ppu.mem_write(0x2000, 0x05);
    ppu.mem_write(0x2001, 0x06);
    ppu.mem_write(0x23C0, 0b0000_0001); // attribute: quadrant(0,0) = 1 (covers columns 0-1)

    // Tile 5: pattern bits = 0b10 (low=0, high=1) on every row/column -- a
    // solid fill, so it doesn't matter which exact bit fine-X picks, only
    // WHICH TILE was fetched.
    ppu.chr[0x05 * 16] = 0x00; // pt_lo
    ppu.chr[0x05 * 16 + 8] = 0xFF; // pt_hi
                                   // Tile 6: pattern bits = 0b11 (low=1, high=1) -- distinct from tile 5.
    ppu.chr[0x06 * 16] = 0xFF;
    ppu.chr[0x06 * 16 + 8] = 0xFF;

    ppu.palette[(1 << 2) | 0b10] = 0x30; // attr=1, pattern=2 -> tile 5's color
    ppu.palette[(1 << 2) | 0b11] = 0x31; // attr=1, pattern=3 -> tile 6's color

    // Tick through the ENTIRE pre-render line (341 dots) and all the way
    // through scanline 0's own visible dots, using only `Ppu::tick` -- the
    // same seam `crate::system::NesBus` drives in real operation.
    for _ in 0..(DOTS_PER_SCANLINE * 2) {
        ppu.tick();
    }
    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);
    let (y, pixels) = &sink.calls[0];
    assert_eq!(*y, 0);

    for (x, p) in pixels.iter().enumerate().take(8) {
        assert_eq!(p.palette_index, 0x30, "pixel {x}: tile 5 (column 0)");
        assert_eq!(p.layer, PixelLayer::Background(0), "pixel {x}");
    }
    for (x, p) in pixels.iter().enumerate().skip(8).take(8) {
        assert_eq!(p.palette_index, 0x31, "pixel {x}: tile 6 (column 1)");
        assert_eq!(p.layer, PixelLayer::Background(0), "pixel {x}");
    }
}

#[test]
fn even_frame_pre_render_line_runs_the_full_341_dots_with_rendering_enabled() {
    let mut ppu = test_ppu();
    ppu.write_register(1, 0x08); // rendering enabled
    ppu.scanline = PRERENDER_SCANLINE;
    ppu.dot = ODD_FRAME_SKIP_DOT;
    // `frame_is_odd` defaults to `false` (even) from `Ppu::new`.

    ppu.tick(); // processes dot 339, advances to dot 340: no skip on an even frame
    assert_eq!((ppu.scanline, ppu.dot), (PRERENDER_SCANLINE, 340));

    ppu.tick(); // processes dot 340, wraps to scanline 0
    assert_eq!((ppu.scanline, ppu.dot), (0, 0));
    assert!(ppu.frame_is_odd, "frame parity toggles after the wrap");
}

#[test]
fn odd_frame_with_rendering_enabled_skips_dot_340() {
    let mut ppu = test_ppu();
    ppu.write_register(1, 0x08); // rendering enabled
    ppu.scanline = PRERENDER_SCANLINE;
    ppu.dot = ODD_FRAME_SKIP_DOT;
    ppu.frame_is_odd = true;

    ppu.tick(); // processes dot 339; the skip fires -> jumps straight to (0,0)
    assert_eq!(
        (ppu.scanline, ppu.dot),
        (0, 0),
        "dot 340 is skipped entirely on an odd frame with rendering enabled"
    );
    assert!(!ppu.frame_is_odd, "frame parity toggles once, at the wrap");
}

#[test]
fn odd_frame_with_rendering_disabled_does_not_skip() {
    let mut ppu = test_ppu();
    // PPUMASK left at 0: rendering disabled -- nesdev.org/wiki/PPU_rendering:
    // "this behavior can be bypassed by keeping rendering disabled until
    // after this scanline has passed".
    ppu.scanline = PRERENDER_SCANLINE;
    ppu.dot = ODD_FRAME_SKIP_DOT;
    ppu.frame_is_odd = true;

    ppu.tick(); // dot 340 still happens: no skip without rendering enabled
    assert_eq!((ppu.scanline, ppu.dot), (PRERENDER_SCANLINE, 340));

    ppu.tick(); // now the ordinary end-of-scanline wrap fires
    assert_eq!((ppu.scanline, ppu.dot), (0, 0));
    assert!(
        !ppu.frame_is_odd,
        "parity still toggles once, at the real (unskipped) wrap"
    );
}

#[test]
fn consecutive_frames_alternate_341_and_340_total_dots_when_rendering_is_enabled() {
    // Frame length: 262 scanlines x 341 dots, minus the one skipped dot on
    // odd frames (nesdev.org/wiki/PPU_rendering) -- checked end-to-end from
    // `Ppu::new`'s real starting position, not a hand-poked dot/scanline.
    let mut ppu = test_ppu();
    ppu.write_register(1, 0x08); // rendering enabled for the whole run

    // `Ppu::new` starts ON the pre-render line (dot 0) already, so the
    // first (0,0) reached is just that one scanline's remaining 341 dots
    // (not a full 262-scanline frame) -- it completes the "even" frame
    // `Ppu::new` was already partway through and flips parity to odd.
    let remainder_of_initial_frame = ticks_until_scanline_0_dot_0(&mut ppu);
    assert_eq!(
        remainder_of_initial_frame, DOTS_PER_SCANLINE,
        "Ppu::new starts on the pre-render line: only that line's dots remain"
    );

    // From here on, each call spans one COMPLETE 262-scanline frame.
    let odd_frame_dots = ticks_until_scanline_0_dot_0(&mut ppu);
    assert_eq!(
        odd_frame_dots,
        DOTS_PER_SCANLINE * SCANLINES_PER_FRAME - 1,
        "odd frame: one dot shorter"
    );

    let even_frame_dots = ticks_until_scanline_0_dot_0(&mut ppu);
    assert_eq!(
        even_frame_dots,
        DOTS_PER_SCANLINE * SCANLINES_PER_FRAME,
        "even frame: full 341 dots/scanline, no skip"
    );
}
