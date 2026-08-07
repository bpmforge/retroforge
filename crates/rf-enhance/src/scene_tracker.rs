//! Scroll-stable scene identity (ticket W4-03d). `crate::scene_identity`'s
//! `compute_scene_id` (W4-03b) is a PURE function of one frame's content —
//! correct for room-based games, but FM-11's own detection column ("canvas
//! count per scene watermark") measures a play SESSION, and W4-03b's own
//! module doc records the resulting gap plainly: a real scrolling level
//! fragments into roughly one new scene every few block-widths of travel,
//! because the block grid is screen-anchored and every scroll step is
//! genuinely different visible content. This ticket's pre-flight refuted
//! the obvious repair (a Hamming-distance threshold on the perceptual hash:
//! a conductor probe measured 30-40% of edge buckets differing between
//! CONSECUTIVE scrolling frames, climbing to ~95% within a few seconds — a
//! threshold loose enough to survive that would also merge genuinely
//! different scenes). The signature decorrelates under scroll; it does not
//! drift.
//!
//! ## Design: identity is STATEFUL, not a pure hash
//!
//! [`SceneTracker`] holds the currently-assigned [`SceneId`] across calls
//! and only ever recomputes it at a detected DISCONTINUITY — continuity is
//! two independent signals, both required to hold for "same scene":
//!
//! 1. **Scroll continuity**: [`crate::scroll_tracker::ScrollTracker`]
//!    (W4-03a) already reconstructs a wraparound-unwrapped world position
//!    per frame (`WorldAxis::advance`'s own doc: it always resolves a raw
//!    register delta to the SHORTER interpretation around the wrap). This
//!    tracker layers one more property on top: the frame-to-frame WORLD
//!    POSITION DELTA of the primary (largest) band must stay under
//!    [`MAX_CONTINUOUS_STEP_PX`] on both axes. A real scroll advances a
//!    handful of pixels per frame (RF-Scroller's own budget: 8px every 8
//!    frames — `fixtures/nes/rf-scroller/src/main.c`'s `frame_counter &
//!    0x07u` gate); a scene cut is a different level's geometry entirely,
//!    which — per `WorldAxis`'s own documented boundary — resolves to
//!    "short interpretation" too, but that interpretation is still bounded
//!    by half the register's wrap modulus (256px for the X axis), far
//!    above any plausible single-frame scroll rate.
//! 2. **Mapper bank state**: unchanged between frames (caller-supplied raw
//!    bytes, same "pure function, caller supplies the environment-specific
//!    bit" pattern `compute_scene_id` already uses).
//!
//! While continuity holds, the currently-assigned id is returned UNCHANGED
//! — `compute_scene_id`'s per-frame perceptual hash is never even computed,
//! so its scroll-sensitivity gap simply never gets a chance to fire. Only
//! at a discontinuity (scroll jump, bank change, or the very first frame
//! ever observed) does this tracker fall back to `compute_scene_id` — the
//! job that function is genuinely good at (module doc's own "the signature
//! decorrelates" framing: a *hash comparison across a cut* is exactly a
//! same-content-same-hash question, not a track-drift-across-many-frames
//! one). Because `compute_scene_id` is a pure function of content + bank
//! state, revisiting an unchanged, previously-seen scene after a
//! discontinuity naturally reproduces the SAME id with no extra bookkeeping
//! — no history table needed.
//!
//! ## NROM constraint inherited from W2-10 — say so plainly
//!
//! RF-Scroller (this ticket's real fixture, `fixtures/nes/rf-scroller`) is
//! mapper 0 (NROM) and has **no mapper bank state at all** — the "mapper
//! bank state" half of the continuity signal above is therefore UNEXERCISED
//! by this ticket's fixture-driven validation
//! (`crates/rf-harness/tests/rf_scroller_scene_tracker.rs`); only the
//! scroll-continuity half is proven against a real running core. A
//! mapper-using fixture to exercise the other half does not exist yet.
//!
//! ## What this is NOT
//!
//! Not a room-transition detector, not a "return to a known room" cache —
//! both already fall out of `compute_scene_id` being a pure content hash
//! (module doc above). Not a fix for `compute_scene_id`'s own screen-space
//! block-grid gap (still open, still documented in `crate::scene_identity`)
//! — this tracker just avoids CALLING that function on every scrolling
//! frame, which is what actually mattered for FM-11's watermark.

use rf_core_api::{CoreEvent, PpuPixel};

use crate::scene_identity::{compute_scene_id, SceneId};
use crate::scroll_tracker::{BandWorldPos, ScanlineBand, ScrollTracker};

/// Max plausible frame-to-frame world-position delta (either axis, pixels)
/// that still counts as "smooth scrolling" rather than a scene cut.
/// RF-Scroller's own real movement budget is 8px every 8th frame (module
/// doc); this is deliberately far more generous than that — the point is
/// not to detect fast movement as a cut, only to catch genuinely different
/// geometry, which per `WorldAxis`'s own wraparound heuristic resolves to
/// at most half the register's wrap modulus (256px on the X axis). 64 sits
/// comfortably above any real single-frame NES scroll rate (even a fast
/// auto-scroller rarely exceeds ~16-24px/frame) and comfortably below that
/// 256px ceiling, so a real cut still reads as discontinuous. `pub`: the
/// real-fixture validation
/// (`crates/rf-harness/tests/rf_scroller_scene_tracker.rs`) asserts against
/// this exact bound rather than duplicating the number.
pub const MAX_CONTINUOUS_STEP_PX: i64 = 64;

/// Stateful scene identity (module doc). One instance tracks one play
/// session; construct a fresh one per ROM/run, same lifetime shape as
/// [`ScrollTracker`] itself.
pub struct SceneTracker {
    scroll: ScrollTracker,
    current_id: Option<SceneId>,
    last_world: Option<(i64, i64)>,
    last_bank_state: Vec<u8>,
}

impl SceneTracker {
    #[must_use]
    pub fn new() -> Self {
        SceneTracker {
            scroll: ScrollTracker::new(),
            current_id: None,
            last_world: None,
            last_bank_state: Vec::new(),
        }
    }

    /// Observe one frame and return its [`SceneId`] (module doc's
    /// continuity rule). `video`/`width`/`height` are `FrameBundle`'s own
    /// shape; `events` is that same frame's `FrameBundle::events` (only
    /// [`CoreEvent::Scanline`]/[`CoreEvent::ScrollWrite`] matter, the same
    /// subset `ScrollTracker` itself consumes); `mapper_bank_state` is the
    /// same caller-supplied-bytes contract `compute_scene_id` already uses
    /// — pass `&[]` for a mapper with no persistent banking state (module
    /// doc's NROM note).
    pub fn observe_frame(
        &mut self,
        video: &[PpuPixel],
        width: u16,
        height: u16,
        events: &[CoreEvent],
        mapper_bank_state: &[u8],
    ) -> SceneId {
        let bands = self.scroll.observe_frame(events);
        let world = primary_world_position(&bands);

        let bank_changed = mapper_bank_state != self.last_bank_state.as_slice();
        let world_jump = match (self.last_world, world) {
            (Some(prev), Some(cur)) => {
                (cur.0 - prev.0).abs() > MAX_CONTINUOUS_STEP_PX
                    || (cur.1 - prev.1).abs() > MAX_CONTINUOUS_STEP_PX
            }
            // No comparable prior/current world position this frame (no
            // band at all, e.g. a fully blanked frame): neither confirms
            // nor refutes continuity on its own -- `current_id.is_none()`
            // below still forces a compute on the very first call ever,
            // and every later frame's own comparison is unaffected by this
            // one gap. The safe direction to be wrong in (matching
            // `ScrollTracker::observe_frame`'s own frame-0 HUD default
            // reasoning): treat a data-free frame as non-disruptive rather
            // than forcing an expensive, likely-spurious rehash.
            _ => false,
        };

        let discontinuous = self.current_id.is_none() || bank_changed || world_jump;
        if discontinuous {
            self.current_id = Some(compute_scene_id(video, width, height, mapper_bank_state));
        }

        self.last_world = world;
        self.last_bank_state = mapper_bank_state.to_vec();
        self.current_id
            .expect("set above on this call whenever it was None")
    }

    /// The primary-band world position as of the most recent
    /// [`Self::observe_frame`] call (`None` before the first call, or if
    /// that frame had no band at all). Observability only — the tracker
    /// itself only ever needs the frame-to-frame DELTA internally, but a
    /// real-fixture caller validating continuity against a running core
    /// needs the position itself (`crates/rf-harness/tests/
    /// rf_scroller_scene_tracker.rs`'s own non-vacuity proof: per-frame
    /// delta stays bounded AND the session traveled the RAM-witnessed
    /// distance).
    #[must_use]
    pub fn last_world_position(&self) -> Option<(i64, i64)> {
        self.last_world
    }
}

impl Default for SceneTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// The band that drives world-space continuity: the LARGEST (by row span)
/// band this frame, matching `ScrollTracker::observe_frame`'s own "primary"
/// selection (that field is private; duplicated here as one `max_by_key`
/// rather than widening `ScrollTracker`'s public surface for a single
/// caller — same size/shape tradeoff `stitcher_determinism.rs`'s own
/// `run_pipeline` test helper already makes). `saturating_sub`: a band
/// whose `end < start` (this ticket's own real-fixture-found leaked-scanline
/// case, `crate::scroll_tracker`'s module-level fix) must never win by
/// wrapping to a huge width.
fn primary_world_position(bands: &[(ScanlineBand, BandWorldPos)]) -> Option<(i64, i64)> {
    bands
        .iter()
        .max_by_key(|(b, _)| b.end.saturating_sub(b.start))
        .map(|(_, pos)| (pos.world_x, pos.world_y_at_start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::PixelLayer;

    const WIDTH: u16 = 256;
    const HEIGHT: u16 = 240;
    const HUD_ROWS: u16 = 16;

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

    fn bg_pixel(v: u8) -> PpuPixel {
        PpuPixel {
            palette_index: v,
            layer: PixelLayer::Background(0),
            sprite_id: None,
            priority: 0,
            dropped_by_limit: false,
        }
    }

    /// A "world texture": the same textured-block idea
    /// `scene_identity`'s own tests use, sampled at a world X offset so
    /// scrolling frames show genuinely different visible content, the same
    /// property that fragments the raw `compute_scene_id` under scroll
    /// (module doc).
    fn frame_at_world_shift(shift: i64) -> Vec<PpuPixel> {
        let width = WIDTH as usize;
        let height = HEIGHT as usize;
        let mut video = vec![bg_pixel(0); width * height];
        for y in HUD_ROWS as usize..height {
            for x in 0..width {
                let world_x = x as i64 + shift;
                let v = ((world_x.rem_euclid(97)) ^ (y as i64).rem_euclid(61)) as u8;
                video[y * width + x] = bg_pixel(v);
            }
        }
        // HUD rows: fixed sentinel content, never moves with scroll.
        for y in 0..HUD_ROWS as usize {
            for x in 0..width {
                video[y * width + x] = bg_pixel(255);
            }
        }
        video
    }

    /// One frame's event log for a `shift`-th scroll step: HUD re-armed at
    /// (0,0) every frame (RF-Scroller's own real pattern —
    /// `fixtures/nes/rf-scroller/src/main.c`'s "Top-of-frame scroll" write),
    /// mid-frame split to `(shift mod 512, 0)`.
    fn events_at_shift(shift: i64) -> Vec<CoreEvent> {
        let mut events = vec![scroll(0, 0)];
        events.extend(scanlines(0..HUD_ROWS));
        events.push(scroll((shift.rem_euclid(512)) as u16, 0));
        events.extend(scanlines(HUD_ROWS..HEIGHT));
        events
    }

    fn different_level_frame() -> Vec<PpuPixel> {
        let width = WIDTH as usize;
        let height = HEIGHT as usize;
        let mut video = vec![bg_pixel(0); width * height];
        for y in HUD_ROWS as usize..height {
            for x in 0..width {
                // Inverted texture relative to `frame_at_world_shift` --
                // same idea `scene_identity`'s own
                // `different_scene_frame` test fixture uses.
                let v = 255 - (((x as i64).rem_euclid(97)) ^ (y as i64).rem_euclid(61)) as u8;
                video[y * width + x] = bg_pixel(v);
            }
        }
        for y in 0..HUD_ROWS as usize {
            for x in 0..width {
                video[y * width + x] = bg_pixel(255);
            }
        }
        video
    }

    // --- Stability: a sustained scrolling session is ONE scene id -------

    #[test]
    fn a_sustained_3px_per_frame_scrolling_session_stays_one_scene_id() {
        use std::collections::HashSet;
        let mut tracker = SceneTracker::new();
        let mut seen = HashSet::new();
        for frame in 0..200i64 {
            let shift = frame * 3; // typical NES scroll rate, matches scene_identity's own fixture
            let id = tracker.observe_frame(
                &frame_at_world_shift(shift),
                WIDTH,
                HEIGHT,
                &events_at_shift(shift),
                &[],
            );
            seen.insert(id);
        }
        assert_eq!(
            seen.len(),
            1,
            "a continuous 3px/frame scrolling session over 200 frames (597px total travel) must \
             stay one scene id, not fragment like the pure per-frame hash does -- got {} distinct \
             ids",
            seen.len()
        );
    }

    // --- FM-11 watermark over SCROLLING play (acceptance 3) -------------
    //
    // Deliberately separate from, and does not replace,
    // `scene_identity`'s own
    // `fifty_frames_of_sprite_and_hud_noise_over_an_unchanged_background_stay_one_scene_id`
    // -- that test's own doc explains why its scope is exactly the
    // UNCHANGED-background half of FM-11; this covers the scrolling half.

    #[test]
    fn fifty_frames_of_sprite_noise_layered_over_a_scrolling_session_stay_one_scene_id() {
        use std::collections::HashSet;
        let mut tracker = SceneTracker::new();
        let mut seen = HashSet::new();
        for frame in 0..50i64 {
            let shift = frame * 3;
            let mut video = frame_at_world_shift(shift);
            // Sprite-shaped noise: an 8x8 block of Sprite-layer pixels
            // sweeping the screen -- excluded from `compute_scene_id` by
            // `stitchable_pixel`, same discriminating perturbation
            // `scene_identity`'s own watermark test uses.
            let (sx, sy) = (
                ((frame as u32 * 5) % (WIDTH as u32 - 8)) as usize,
                (HUD_ROWS as usize)
                    + ((frame as u32 * 3) % (HEIGHT as u32 - HUD_ROWS as u32 - 8)) as usize,
            );
            for y in 0..8 {
                for x in 0..8 {
                    let (px, py) = (sx + x, sy + y);
                    video[py * WIDTH as usize + px] = PpuPixel {
                        palette_index: 255,
                        layer: PixelLayer::Sprite,
                        sprite_id: Some(0),
                        priority: 0,
                        dropped_by_limit: false,
                    };
                }
            }
            let id = tracker.observe_frame(&video, WIDTH, HEIGHT, &events_at_shift(shift), &[]);
            seen.insert(id);
        }
        assert_eq!(
            seen.len(),
            1,
            "FM-11 canvas-count-per-scene watermark over SCROLLING play (with sprite noise \
             layered on top) must stay bounded at one scene id -- got {} distinct ids",
            seen.len()
        );
    }

    // --- Difference: a genuine discontinuity DOES change the id ---------
    //
    // Proven in the SAME test as stability below is handled by the
    // fixture-driven rf-harness test (module doc's NROM note: RF-Scroller
    // has no room transition to cut to); this unit test proves the
    // DISCONTINUITY-DETECTION mechanism itself in isolation, which the
    // real-ROM test cannot do without a second real cut in the fixture.

    #[test]
    fn a_scroll_jump_far_beyond_the_continuous_step_bound_changes_the_id() {
        let mut tracker = SceneTracker::new();
        let id_before = tracker.observe_frame(
            &frame_at_world_shift(0),
            WIDTH,
            HEIGHT,
            &events_at_shift(0),
            &[],
        );
        // A raw scroll write of 300 (from a raw 0): `WorldAxis::advance`
        // resolves that to the SHORTER wrap interpretation, -212
        // (300 - 512), same as any raw delta always resolves to at most
        // half the 512 modulus. -212 is still far past
        // MAX_CONTINUOUS_STEP_PX (64), so this reads as a cut regardless
        // of the wrap heuristic -- it does not depend on hitting the
        // heuristic's own worst case (a jump landing exactly at the
        // modulus/2 boundary, `WorldAxis`'s own documented blind spot).
        let id_after = tracker.observe_frame(
            &different_level_frame(),
            WIDTH,
            HEIGHT,
            &events_at_shift(300),
            &[],
        );
        assert_ne!(
            id_before, id_after,
            "a scroll position jump far beyond any plausible single-frame scroll rate must be \
             treated as a scene cut, not smoothed away as continuity"
        );
    }

    #[test]
    fn a_mapper_bank_change_alone_changes_the_id_even_with_continuous_scroll() {
        let mut tracker = SceneTracker::new();
        let id_before = tracker.observe_frame(
            &frame_at_world_shift(0),
            WIDTH,
            HEIGHT,
            &events_at_shift(0),
            &[1, 2, 3],
        );
        // Scroll position barely moves (well within the continuity bound),
        // but the mapper bank changed -- e.g. a level warp that resets to
        // a similar-looking camera position in a different bank.
        let id_after = tracker.observe_frame(
            &frame_at_world_shift(3),
            WIDTH,
            HEIGHT,
            &events_at_shift(3),
            &[1, 2, 4],
        );
        assert_ne!(
            id_before, id_after,
            "a mapper bank change must be treated as a scene cut even when scroll position \
             alone looks continuous"
        );
    }

    // --- Mutation-shaped sanity: continuity actually short-circuits the
    // hash, not just happens to agree with it -----------------------------

    #[test]
    fn continuity_reuses_the_pinned_id_without_recomputing_from_content() {
        // Two frames with IDENTICAL video content but advancing scroll
        // events: if continuity were ignored and the id were recomputed
        // from `video` every call, these two calls would already agree
        // (same content) and this test would not discriminate anything.
        // What it actually proves: the SECOND call's content is
        // deliberately DIFFERENT (a different world shift's texture) yet
        // still returns the SAME id, because continuity pins it -- a
        // content-only implementation would fail this.
        let mut tracker = SceneTracker::new();
        let id_a = tracker.observe_frame(
            &frame_at_world_shift(0),
            WIDTH,
            HEIGHT,
            &events_at_shift(0),
            &[],
        );
        let id_b = tracker.observe_frame(
            &frame_at_world_shift(3),
            WIDTH,
            HEIGHT,
            &events_at_shift(3),
            &[],
        );
        assert_eq!(
            id_a, id_b,
            "continuous scroll must reuse the pinned scene id even though the raw content hash \
             of these two frames would differ"
        );
    }
}
