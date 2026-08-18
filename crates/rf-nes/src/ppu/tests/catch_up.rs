//! The catch-up scheduler's predicate (ticket W3-07b), tested at the
//! level where it can be tested exhaustively rather than sampled.
//!
//! `crate::system::tests::catch_up` covers the property that matters —
//! the scheduler produces a bit-identical machine — but it can only reach
//! the positions a running workload happens to visit. The rules here are
//! about *every* position, so they are enumerated over all of them.

use rf_cart::Mirroring;

use crate::ppu::{Ppu, DOTS_PER_SCANLINE};

/// Place the PPU at an exact position with rendering forced on or off.
/// `mask` bit 3 is "show background", which is all `rendering_enabled`
/// needs.
fn at(scanline: u16, dot: u16, rendering: bool) -> Ppu {
    let mut ppu = Ppu::new(vec![0u8; 0x2000], true, Mirroring::Horizontal);
    ppu.scanline = scanline;
    ppu.dot = dot;
    ppu.mask = if rendering { 0x08 } else { 0x00 };
    ppu
}

/// **The capping rule, checked rather than argued.** A run may never
/// reach dot 339: dots 339 and 340 are where `advance_counters` evaluates
/// the odd-frame skip (reading `render_enable_pipe & 0b100`) and the
/// scanline/frame wrap, and a skipped dot never calls it. That single cap
/// is what retires the frame-boundary hazards — `frame_count`,
/// `age_decay_register`, end-of-scanline video emission — all at once, so
/// it is worth an exhaustive check: 262 scanlines x 341 dots x both
/// rendering states.
#[test]
fn no_inert_run_ever_reaches_the_wrap_dots() {
    for rendering in [true, false] {
        for scanline in 0..262u16 {
            for dot in 0..DOTS_PER_SCANLINE {
                let run = at(scanline, dot, rendering).inert_run_len();
                if run > 0 {
                    assert!(
                        dot + run <= 339,
                        "run of {run} from ({scanline},{dot}) rendering={rendering} would reach \
                         dot {}, past the last skippable dot",
                        dot + run
                    );
                }
            }
        }
    }
}

/// **The predicate must never call a rendering dot inert.** Every
/// PPU-side A12 edge comes from a background or sprite pattern fetch, and
/// those only happen with rendering on; the A12 safety argument, and the
/// `render_enable_pipe` one, both rest entirely on this. A future edit to
/// the predicate's table that let a rendering dot through would break
/// MMC3 IRQ timing in a way no single ROM's output makes obvious.
#[test]
fn a_rendering_scanline_has_no_inert_dot() {
    for scanline in (0..=239u16).chain(std::iter::once(261)) {
        for dot in 0..DOTS_PER_SCANLINE {
            assert_eq!(
                at(scanline, dot, true).inert_run_len(),
                0,
                "({scanline},{dot}) was called inert with rendering ENABLED"
            );
        }
    }
}

/// The two dots that move `nmi_line` — vblank set at (241,1) and the
/// clear at (261,1) — must never be inside a run, because the scheduler
/// samples `nmi_level_latch` once per run instead of once per dot. This
/// is the specific claim `ppu_vbl_nmi` 05-08 decides at the suite level;
/// here it is pinned directly so a predicate edit fails fast rather than
/// failing as a mysterious NMI-timing regression.
#[test]
fn no_run_contains_a_dot_that_moves_the_nmi_line() {
    for rendering in [true, false] {
        for (nmi_scanline, nmi_dot) in [(241u16, 1u16), (261, 1)] {
            for start in 0..DOTS_PER_SCANLINE {
                let run = at(nmi_scanline, start, rendering).inert_run_len();
                if run > 0 {
                    assert!(
                        start > nmi_dot || start + run <= nmi_dot,
                        "a run of {run} from ({nmi_scanline},{start}) rendering={rendering} \
                         covers dot {nmi_dot}, which moves nmi_line"
                    );
                }
            }
        }
    }
}

/// `dots_until_possible_inert` is a LOWER bound, and being wrong in the
/// unsafe direction would silently swallow real inert runs — or, worse,
/// let the bus tick through a position it should have re-examined. Every
/// position where the predicate is 0 is checked against the positions
/// that follow it.
#[test]
fn the_prediction_never_overshoots_the_next_inert_dot() {
    for rendering in [true, false] {
        for scanline in 0..262u16 {
            for dot in 0..DOTS_PER_SCANLINE {
                let ppu = at(scanline, dot, rendering);
                if ppu.inert_run_len() != 0 {
                    continue;
                }
                let predicted = ppu.dots_until_possible_inert();
                assert!(predicted >= 1, "a prediction of 0 would spin the bus loop");
                // Every dot the prediction covers must genuinely be
                // non-inert. Positions past the end of this scanline are
                // not walked: `advance_counters` is what moves the
                // scanline, and the bus re-asks after every dot it ticks.
                for step in 0..predicted {
                    let d = dot + step;
                    if d >= DOTS_PER_SCANLINE {
                        break;
                    }
                    assert_eq!(
                        at(scanline, d, rendering).inert_run_len(),
                        0,
                        "prediction of {predicted} from ({scanline},{dot}) rendering={rendering} \
                         covers dot {d}, which IS inert"
                    );
                }
            }
        }
    }
}

/// Anti-vacuity for the three tests above: the predicate must actually
/// say "inert" somewhere, and in the places the design says it does. A
/// predicate stuck at 0 would satisfy every assertion above and turn the
/// whole scheduler into a no-op — which is precisely the shape
/// `mode_diff`'s stale-declaration rule refuses one level up.
#[test]
fn the_predicate_fires_where_the_design_says_it_does() {
    // Post-render is inert in both rendering states.
    assert!(at(240, 0, true).inert_run_len() > 0);
    assert!(at(240, 0, false).inert_run_len() > 0);
    // Vblank past the flag-setting dot.
    assert!(at(241, 2, true).inert_run_len() > 0);
    assert_eq!(at(241, 1, true).inert_run_len(), 0);
    assert!(at(250, 0, true).inert_run_len() > 0);
    // Visible lines: only with rendering off, and only past the last
    // output dot.
    assert_eq!(at(100, 257, true).inert_run_len(), 0);
    assert!(at(100, 257, false).inert_run_len() > 0);
    assert_eq!(at(100, 100, false).inert_run_len(), 0);
    // A run from dot 257 of a rendering-off visible line should reach the
    // cap, not stop early.
    assert_eq!(at(100, 257, false).inert_run_len(), 339 - 257);
}
