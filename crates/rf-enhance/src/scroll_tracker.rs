//! `ScrollTracker`: the wideNES-style per-scanline scroll-band
//! reconstruction `docs/design/ENHANCEMENT_RUNTIME.md` §3 specifies
//! (ticket W4-03a, acceptance criterion 1) — "mid-frame splits handled by
//! tracking per-scanline scroll bands; HUD bands auto-detected as
//! zero-delta regions and excluded".
//!
//! ## Inputs: the real event stream, nothing synthesized
//!
//! [`ScrollTracker::observe_frame`] consumes exactly what a real frame's
//! [`rf_core_api::FrameBundle::events`] carries — [`CoreEvent::Scanline`]
//! and [`CoreEvent::ScrollWrite`], in the single shared-FIFO emission
//! order `crate::bus`'s module doc already leans on. No caller passes raw
//! `(x, y)` tuples in; band boundaries fall out purely from where a
//! `ScrollWrite` lands relative to the `Scanline` events around it — the
//! same information a real ROM's `$2005`/`$2006` writes would produce
//! (`crates/rf-nes/src/ppu/scroll.rs`'s `write_scroll`/`write_addr`, the
//! latter conditionally as of this same ticket).
//!
//! ## Why a band's Y value is NOT constant across its own rows
//!
//! A tempting simplification: one `ScrollWrite` per band, so every row in
//! the band shares that write's `(x, y)`. That's wrong for `y`, and
//! provably so from this workspace's own PPU: `crate::ppu::background`'s
//! `process_render_dot` calls `copy_horizontal` at dot 257 of **every**
//! scanline (so `x` genuinely is constant across a band — nothing
//! refreshes it differently row to row), but `increment_y` *also* runs at
//! dot 256 of every visible scanline, advancing the live vertical
//! position by one row regardless of whether any register was rewritten.
//! `effective_scroll()` (`rf-nes`) decodes `t`, which does not itself
//! auto-increment — so the `y` a `ScrollWrite` carries is the value valid
//! for the row the write takes effect on, and every subsequent row in the
//! same band is one further row down, not a repeat of the same row. This
//! module reconstructs that per-row Y explicitly ([`ScanlineBand::y_at`])
//! rather than pretending the hardware holds Y still for an entire band.
//!
//! ## Honesty (ENHANCEMENT_RUNTIME §3's own limitation, restated here)
//!
//! A tracked band only ever reports what real `ScrollWrite`/`Scanline`
//! events revealed about *this* frame's registers. It never infers
//! unvisited geometry, and [`Stitcher`](crate::stitcher::Stitcher) (this
//! ticket's other half) only ever paints scanlines this module actually
//! attributed a scroll position to.
//!
//! Named gap inherited from `rf-nes` (`crates/rf-nes/src/ppu/scroll.rs`'s
//! `write_addr` doc): a ROM that sets its base per-frame scroll
//! exclusively through a `$2006` pair during vblank (rather than `$2005`)
//! emits no `ScrollWrite` for that update, so [`ScrollTracker`]'s carried
//! scroll simply stays at its last observed value until a `$2005` write
//! or an active-picture `$2006` split is observed.
use rf_core_api::CoreEvent;

/// One contiguous run of scanlines that shared a single `ScrollWrite`
/// origin — the "per-scanline scroll bands" acceptance criterion.
/// `[start, end)`, half-open, matching Rust range convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanlineBand {
    /// First scanline (inclusive) this band covers.
    pub start: u16,
    /// One past the last scanline this band covers.
    pub end: u16,
    /// `(x, y)` as of the `ScrollWrite` (or carried-over prior state) that
    /// took effect at `start` — **not** a constant for the whole band; use
    /// [`ScanlineBand::y_at`] for any row other than `start` itself.
    pub base_scroll: (u16, u16),
    /// Whether this band is classified as a static HUD/status-bar region
    /// (module doc: "zero-delta" vs. the previous frame's band at the
    /// same rows) — set by [`ScrollTracker::observe_frame`], never by
    /// [`compute_bands`] itself (which has no cross-frame memory).
    pub is_hud: bool,
}

impl ScanlineBand {
    /// The vertical scroll register's effective value for row `sy`
    /// (module doc's "why a band's Y value is NOT constant" section).
    /// `sy` outside `[start, end)` still computes correctly (linear
    /// extrapolation) but is meaningless for this band's own extent —
    /// callers iterate `start..end`.
    #[must_use]
    pub fn y_at(&self, sy: u16) -> i64 {
        i64::from(self.base_scroll.1) + (i64::from(sy) - i64::from(self.start))
    }

    /// The horizontal scroll register's value for any row in this band —
    /// genuinely constant (module doc: `copy_horizontal` runs identically
    /// every scanline, so nothing drifts row to row the way Y does).
    #[must_use]
    pub fn x(&self) -> u16 {
        self.base_scroll.0
    }
}

/// Pure function: walk one frame's events and produce its scanline bands,
/// given the scroll value already in effect when this frame's event slice
/// begins (a frame that never rewrites `$2005`/`$2006` still needs to know
/// what scroll was already active, carried over from the previous frame's
/// [`compute_bands`] return value). Returns the bands plus the scroll
/// value in effect when the slice ends, for the next call's `carry_in`.
///
/// No HUD classification here (`ScanlineBand::is_hud` is always `false`
/// on this function's output) — that needs cross-frame memory
/// ([`ScrollTracker`] owns it), and this function is deliberately
/// stateless so it can be tested in isolation from that.
#[must_use]
pub fn compute_bands(
    events: &[CoreEvent],
    carry_in: (u16, u16),
) -> (Vec<ScanlineBand>, (u16, u16)) {
    let mut bands = Vec::new();
    let mut base = carry_in;
    let mut band_start: Option<u16> = None;
    let mut last_seen: Option<u16> = None;

    for ev in events {
        match *ev {
            CoreEvent::ScrollWrite { x, y, .. } => {
                if let (Some(start), Some(last)) = (band_start, last_seen) {
                    bands.push(ScanlineBand {
                        start,
                        end: last + 1,
                        base_scroll: base,
                        is_hud: false,
                    });
                }
                base = (x, y);
                band_start = None;
                last_seen = None;
            }
            CoreEvent::Scanline(sy) => {
                if band_start.is_none() {
                    band_start = Some(sy);
                }
                last_seen = Some(sy);
            }
            _ => {}
        }
    }
    if let (Some(start), Some(last)) = (band_start, last_seen) {
        bands.push(ScanlineBand {
            start,
            end: last + 1,
            base_scroll: base,
            is_hud: false,
        });
    }
    (bands, base)
}

/// Horizontal/vertical scroll-register wrap moduli
/// (`crates/rf-nes/src/ppu/scroll.rs`'s `effective_scroll`): X folds a
/// nametable-select bit into an 8-31-tile coarse value plus 0-7 fine,
/// `0..=511`; Y likewise but tile rows 30/31 are the attribute-table
/// overlap nesdev.org/wiki/PPU_scrolling documents as invalid for actual
/// scroll content, so this heuristic's Y modulus is the intended
/// two-nametable span (`0..=479`), not the raw register's reachable
/// range.
const X_WRAP: i64 = 512;
const Y_WRAP: i64 = 480;

/// Wraparound-aware accumulator: converts a sequence of raw (wrapping)
/// hardware scroll values into a continuously growing world-space
/// position, per axis. This is the "wraparound heuristic" acceptance
/// criterion — the whole reason it exists is that hardware scroll wraps
/// (`x` rolls from 511 back to 0 as the player keeps walking right), and a
/// naive "new minus old" delta would read that as a huge jump backward
/// instead of a small step forward.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorldAxis {
    /// Accumulated world-space position; `None` until the first sample.
    world: Option<i64>,
    /// The last raw (wrapped) value observed, for computing the next delta.
    last_raw: Option<i64>,
}

impl WorldAxis {
    #[must_use]
    pub fn new() -> Self {
        WorldAxis::default()
    }

    /// Feed one more raw (wrapped, `0..modulus`) sample; returns the
    /// updated world-space position.
    ///
    /// The heuristic: take the delta modulo `modulus`, normalized into
    /// `(-modulus/2, modulus/2]` — the shortest signed path around the
    /// circle. A player walking steadily right produces small positive
    /// deltas that keep accumulating even as the raw value wraps; a
    /// genuinely large jump (more than half the modulus in one frame —
    /// far more than any real scroll rate) is impossible to distinguish
    /// from "wrapped the other way" by this heuristic alone, so it always
    /// resolves to the *shorter* interpretation. That is the heuristic's
    /// documented boundary (a true scene cut that happens to land near
    /// `modulus/2` away would be misread as a wrap) — acceptable here
    /// because the scope fence explicitly defers scene-identity/cut
    /// detection to W4-03b; this ticket's job is the accumulation math,
    /// not scene semantics.
    pub fn advance(&mut self, raw: u16, modulus: i64) -> i64 {
        let raw = i64::from(raw).rem_euclid(modulus);
        let world = match (self.world, self.last_raw) {
            (Some(prev_world), Some(prev_raw)) => {
                let mut delta = (raw - prev_raw).rem_euclid(modulus);
                if delta > modulus / 2 {
                    delta -= modulus;
                }
                prev_world + delta
            }
            _ => raw, // first sample: world position starts at the raw value itself
        };
        self.world = Some(world);
        self.last_raw = Some(raw);
        world
    }

    #[must_use]
    pub fn position(&self) -> Option<i64> {
        self.world
    }
}

/// Per-band world position, for the [`Stitcher`](crate::stitcher::Stitcher)
/// to place a band's content on its canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandWorldPos {
    pub world_x: i64,
    /// World Y at `band.start` — same "not constant across the band"
    /// caveat as [`ScanlineBand::y_at`]; add `(sy - band.start)` per row.
    pub world_y_at_start: i64,
}

/// Stateful wrapper around [`compute_bands`] that adds the two things a
/// single frame's events can't provide alone: HUD classification
/// (needs the previous frame's bands) and world-space accumulation
/// (needs history, via [`WorldAxis`]).
pub struct ScrollTracker {
    carry_scroll: (u16, u16),
    previous_bands: Vec<ScanlineBand>,
    world_x: WorldAxis,
    world_y: WorldAxis,
}

impl ScrollTracker {
    #[must_use]
    pub fn new() -> Self {
        ScrollTracker {
            carry_scroll: (0, 0),
            previous_bands: Vec::new(),
            world_x: WorldAxis::new(),
            world_y: WorldAxis::new(),
        }
    }

    /// Process one frame's events, returning its bands (HUD-classified)
    /// alongside each band's world position. Bands are matched to the
    /// previous frame's by scanline-range equality — a band whose rows
    /// exactly match a prior band's rows and whose `base_scroll` is
    /// unchanged is "zero-delta" (module doc; ENHANCEMENT_RUNTIME §3's
    /// literal phrase) and gets `is_hud = true`. A frame's very first
    /// observation has nothing to compare against, so nothing is ever
    /// classified HUD on the first call — conservative default (capture
    /// real content when in doubt), matching the heuristic trust ladder's
    /// "shadow" stance (`ENHANCEMENT_RUNTIME.md` §2a) of recording rather
    /// than ever silently discarding on a guess.
    ///
    /// World position is accumulated from whichever band is NOT flagged
    /// HUD (the first one found, top to bottom, if several) — this ticket
    /// deliberately does not track independent world positions for
    /// multiple simultaneously-scrolling non-HUD bands (a rarer case than
    /// HUD+world), documented here rather than silently mishandled: a
    /// frame with two genuinely independent scrolling regions will have
    /// its second region's world position aliased onto the first's
    /// accumulator.
    pub fn observe_frame(&mut self, events: &[CoreEvent]) -> Vec<(ScanlineBand, BandWorldPos)> {
        let (mut bands, carry_out) = compute_bands(events, self.carry_scroll);
        self.carry_scroll = carry_out;

        // Which band drives world-space accumulation this frame, and is
        // trusted as real content even with zero cross-frame history: the
        // LARGEST (by row span) band, full stop — not "first non-HUD in
        // scanline order" (would pick whatever starts at row 0, typically
        // the HUD itself) and not filtered by `is_hud` (computed below,
        // and would be circular here: `is_hud` on the very first frame
        // ever needs to know which band is primary, see below). A status
        // bar is, in every real case this ticket has reasoned about
        // (nesdev's documented technique, the mutation-test fixtures), a
        // small fraction of the screen; the scrolling world content is
        // the majority — the documented boundary of this heuristic (like
        // `WorldAxis::advance`'s own) is a real game whose static overlay
        // happens to outsize its viewport, out of scope here the same way
        // scene semantics generally are (module doc).
        let primary_index = bands
            .iter()
            .enumerate()
            .max_by_key(|(_, b)| b.end - b.start)
            .map(|(i, _)| i);

        let previous_bands_is_empty = self.previous_bands.is_empty();
        for (i, band) in bands.iter_mut().enumerate() {
            let matched_previous_zero_delta = self.previous_bands.iter().any(|prev| {
                prev.start == band.start
                    && prev.end == band.end
                    && prev.base_scroll == band.base_scroll
            });
            // The frame-0 fix: with no previous frame to compare against,
            // a non-primary band has ZERO evidence either way. Defaulting
            // it to "not HUD" (as an earlier version of this method did)
            // meant one frame's worth of secondary-band content could be
            // painted permanently into the canvas before classification
            // ever got a chance to run — caught by this ticket's own
            // determinism/stitcher integration test
            // (`crates/rf-enhance/tests/stitcher_determinism.rs`), which
            // found a real HUD-sentinel leak at world (0,0) from exactly
            // this path. Defaulting to HUD (excluded) instead is the safe
            // direction to be wrong in: a genuine second scrolling region
            // is rarer than a status bar, and this only affects frame 0 —
            // every later frame's classification is the plain zero-delta
            // match, unaffected by primary/size at all.
            let unproven_secondary_on_first_ever_frame =
                previous_bands_is_empty && Some(i) != primary_index;
            band.is_hud = matched_previous_zero_delta || unproven_secondary_on_first_ever_frame;
        }

        let mut result = Vec::with_capacity(bands.len());
        for (i, band) in bands.iter().enumerate() {
            let world_pos = if Some(i) == primary_index {
                BandWorldPos {
                    world_x: self.world_x.advance(band.x(), X_WRAP),
                    world_y_at_start: self.world_y.advance(band.base_scroll.1, Y_WRAP),
                }
            } else {
                // HUD bands (and any additional non-primary non-HUD bands
                // this frame, per the doc above) still get a position --
                // the last known world position, unmoved -- rather than an
                // Option the Stitcher would have to special-case.
                BandWorldPos {
                    world_x: self.world_x.position().unwrap_or(0),
                    world_y_at_start: self.world_y.position().unwrap_or(0),
                }
            };
            result.push((*band, world_pos));
        }

        self.previous_bands = bands;
        result
    }
}

impl Default for ScrollTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::PixelLayer;

    fn scroll(x: u16, y: u16) -> CoreEvent {
        CoreEvent::ScrollWrite {
            x,
            y,
            layer: PixelLayer::Background(0),
        }
    }

    fn scanlines(range: std::ops::Range<u16>) -> Vec<CoreEvent> {
        range.map(CoreEvent::Scanline).collect()
    }

    #[test]
    fn a_frame_with_no_scroll_write_is_one_band_at_the_carried_in_value() {
        let events = scanlines(0..240);
        let (bands, carry_out) = compute_bands(&events, (10, 20));
        assert_eq!(
            bands,
            vec![ScanlineBand {
                start: 0,
                end: 240,
                base_scroll: (10, 20),
                is_hud: false,
            }]
        );
        assert_eq!(carry_out, (10, 20));
    }

    #[test]
    fn a_mid_frame_scroll_write_splits_the_frame_into_two_bands() {
        // HUD rows 0-15 at (0, 0), a mid-frame $2006-driven split at
        // scanline 16 to (40, 100), covering the rest of the frame.
        let mut events = scanlines(0..16);
        events.push(scroll(40, 100));
        events.extend(scanlines(16..240));

        let (bands, carry_out) = compute_bands(&events, (0, 0));
        assert_eq!(
            bands,
            vec![
                ScanlineBand {
                    start: 0,
                    end: 16,
                    base_scroll: (0, 0),
                    is_hud: false,
                },
                ScanlineBand {
                    start: 16,
                    end: 240,
                    base_scroll: (40, 100),
                    is_hud: false,
                },
            ]
        );
        assert_eq!(carry_out, (40, 100));
    }

    #[test]
    fn y_at_extrapolates_the_per_scanline_hardware_increment() {
        let band = ScanlineBand {
            start: 16,
            end: 240,
            base_scroll: (40, 100),
            is_hud: false,
        };
        assert_eq!(band.y_at(16), 100);
        assert_eq!(band.y_at(17), 101);
        assert_eq!(band.y_at(239), 100 + (239 - 16));
        // x never drifts within a band.
        assert_eq!(band.x(), 40);
    }

    #[test]
    fn leading_scroll_writes_before_any_scanline_only_affect_the_first_bands_base() {
        // The normal per-frame vblank setup: two $2005 writes (X then Y)
        // before Scanline(0) ever fires -- only the FINAL write before the
        // first Scanline should end up as the first band's base.
        let mut events = vec![scroll(5, 0), scroll(5, 9)];
        events.extend(scanlines(0..240));
        let (bands, _) = compute_bands(&events, (0, 0));
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].base_scroll, (5, 9));
    }

    #[test]
    fn world_axis_accumulates_small_deltas_directly() {
        let mut axis = WorldAxis::new();
        assert_eq!(axis.advance(100, 512), 100);
        assert_eq!(axis.advance(108, 512), 108);
        assert_eq!(axis.advance(120, 512), 120);
    }

    #[test]
    fn world_axis_unwraps_a_forward_wrap_as_a_small_continuing_step() {
        let mut axis = WorldAxis::new();
        axis.advance(500, 512);
        // Player keeps walking right: raw wraps 500 -> 4 (delta +16, not -496).
        let world = axis.advance(4, 512);
        assert_eq!(world, 516);
    }

    #[test]
    fn world_axis_unwraps_a_backward_wrap_as_a_small_continuing_step() {
        let mut axis = WorldAxis::new();
        axis.advance(4, 512);
        // Player backs up across the boundary: raw wraps 4 -> 500 (delta -16, not +496).
        let world = axis.advance(500, 512);
        assert_eq!(world, -12);
    }

    #[test]
    fn scroll_tracker_classifies_a_static_hud_band_after_two_matching_frames() {
        let mut tracker = ScrollTracker::new();

        // Frame 1: an explicit (0,0) write establishes HUD rows 0-15
        // (real games re-arm the HUD's scroll every frame rather than
        // relying on carry-over -- see the fix note below), then a
        // mid-frame split to (0, 50) for the scrolling world, rows 16-239.
        let mut frame1 = vec![scroll(0, 0)];
        frame1.extend(scanlines(0..16));
        frame1.push(scroll(0, 50));
        frame1.extend(scanlines(16..240));
        let observed1 = tracker.observe_frame(&frame1);
        // First observation: nothing has history yet. The primary (larger)
        // band -- rows 16-239, the real world content -- is trusted and
        // not HUD; the smaller, unproven secondary band defaults to
        // excluded (`observe_frame`'s own doc: the safe direction to be
        // wrong in, and the specific bug this test's sibling integration
        // test caught when an earlier version of this method defaulted
        // the other way).
        let (small_band, _) = observed1
            .iter()
            .find(|(b, _)| b.start == 0 && b.end == 16)
            .expect("rows 0-15 present");
        assert!(
            small_band.is_hud,
            "an unproven secondary band on the very first frame ever must default to excluded"
        );
        let (large_band, _) = observed1
            .iter()
            .find(|(b, _)| b.start == 16 && b.end == 240)
            .expect("rows 16-239 present");
        assert!(
            !large_band.is_hud,
            "the primary (largest) band must be trusted even with zero history"
        );

        // Frame 2: HUD band re-armed identically (0,0) rows 0-15; world
        // band moved to (0, 58).
        let mut frame2 = vec![scroll(0, 0)];
        frame2.extend(scanlines(0..16));
        frame2.push(scroll(0, 58));
        frame2.extend(scanlines(16..240));
        let observed2 = tracker.observe_frame(&frame2);

        let hud_band = observed2
            .iter()
            .find(|(b, _)| b.start == 0 && b.end == 16)
            .expect("hud band present");
        assert!(hud_band.0.is_hud, "static rows 0-15 must be classified HUD");

        let world_band = observed2
            .iter()
            .find(|(b, _)| b.start == 16 && b.end == 240)
            .expect("world band present");
        assert!(
            !world_band.0.is_hud,
            "the band whose scroll value moved between frames must not be classified HUD"
        );
    }
}
