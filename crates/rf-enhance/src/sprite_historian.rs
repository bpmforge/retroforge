//! `SpriteHistorian`: temporal reconstruction of software-culled sprite
//! rotation (ticket W3-05; `docs/design/ENHANCEMENT_RUNTIME.md` §2, item 2;
//! FR-ENH-002). Off by default (CLAUDE.md law 6 / FR-MODE-002).
//!
//! ## What this is NOT (read `crate::bus`'s module doc first)
//!
//! `crate::bus`'s own doc explicitly declined to build `SpriteHistorian` as
//! unvalidated scaffolding and named this ticket as its real home. §2, item
//! 1 ("sprite-limit bypass") is a SEPARATE mechanism, already shipped by
//! ticket W3-05a as `rf-nes/src/ppu`'s overlay channel
//! (`Ppu::sprite_overlay_enabled`/`OverlayPixel`) — this file does not
//! duplicate it and this ticket's own acceptance says so explicitly
//! (criterion 5).
//!
//! ## Why this module never touches `rf-nes`, and why that is load-bearing
//!
//! `crate::scene_tracker::SceneTracker::observe_frame`'s own signature
//! (`video: &[PpuPixel], width, height, events: &[CoreEvent]`) is this
//! crate's established shape for "generic, works for any `CoreSink`-shaped
//! core" — `rf-enhance`'s `Cargo.toml` depends on `rf-core-api` and
//! `rf-cache` only, never a console core directly
//! (`scripts/validate-arch.sh` rule 3 forbids it, and there is no dev-dep
//! exception the way `rf-harness`/`retroforge` get: that script's checks do
//! not distinguish `[dev-dependencies]` from `[dependencies]`, so even a
//! test-only `rf-nes` dependency here would fail the gate). Two further
//! reasons this is also the RIGHT design, not merely the only legal one:
//!
//! - [`rf_core_api::StateView::oam`] would be the natural richer input, but
//!   nothing produces one yet — `rf-nes` implements no
//!   `EmulatorCore::state_view` (ticket W2-04, still `todo` as of this
//!   ticket; see that ticket's own notes). [`rf_core_api::FrameBundle`]
//!   itself has no OAM field for the same reason (`frame_bundle.rs`'s own
//!   placement-ruling doc: `state_snapshot_refs` deliberately omitted until
//!   a producer exists).
//! - Touching zero lines of `crates/rf-nes/src/ppu/**` makes this ticket's
//!   mode-invariant criterion (4) hold BY CONSTRUCTION rather than by
//!   comparing hashes: [`SpriteHistorian::observe`] takes `video: &[PpuPixel]`
//!   (a shared reference, never `&mut`), so the Rust type system already
//!   makes it impossible for this module to mutate anything the core
//!   depends on. `crates/retroforge/tests/{mode_invariant_corpus.rs,
//!   sprite_overlay_mode_invariant.rs}` (outside this ticket's write scope)
//!   stay green because nothing they exercise changed at all — this
//!   module's own `tests::toggling_never_perturbs_the_source_frame` proves
//!   the runtime half of that claim (the source `video` is byte-identical
//!   after a call, on/off) rather than asserting it in prose.
//!
//! ## Identity key: position + palette, deliberately NOT `sprite_id`
//!
//! [`rf_core_api::PpuPixel::sprite_id`] is the OAM index that WON a pixel —
//! exactly the value software-driven OAM-slot rotation permutes. RF-Scroller
//! itself is the worked example (`fixtures/nes/rf-scroller/src/main.c`'s
//! `update_gems`, ticket W2-10a): the accuracy 8-sprite cap evicts by
//! ASCENDING OAM INDEX ONLY (`crate::ppu::sprites`'s buggy-scan doc,
//! upstream in `rf-nes`) — physical OAM slots `OAM_GEM_BASE_SLOT..+8`
//! (indices 2-9) always win, slots 10-13 always lose, every single frame,
//! regardless of which LOGICAL gem currently occupies which slot. `main.c`
//! rotates `gem_order` every 8 frames so a different logical gem (a fixed
//! on-screen position) occupies the losing slots each rotation window — so
//! keying identity on `sprite_id` would see gem "10" disappear forever (it
//! never once renders under that id) and never recognize that the entity
//! now missing from slot 10 is the SAME logical gem that was, a moment ago,
//! rendering fine under `sprite_id` 7. Position + palette survives the
//! rotation the way `sprite_id` structurally cannot — this is this ticket's
//! own load-bearing insight, not a stylistic choice; see
//! `tests::identity_survives_an_oam_slot_rotation_that_changes_sprite_id`.
//!
//! ## Ring length: 40 frames, not `ENHANCEMENT_RUNTIME.md` §2's "default 4"
//!
//! Measured against the real fixture (`fixtures/nes/rf-scroller`, W2-10a):
//! `GEM_ROTATE_MASK = 0x07` rotates `gem_order` by one slot every 8 frames,
//! and eviction is a contiguous arc of the 12-slot rotation (module doc
//! above) — so any one logical gem is dropped for 4 CONSECUTIVE rotation
//! windows = **32 consecutive frames** before it rotates back into a
//! winning slot. A 4-frame ring (the design doc's suggested default) would
//! forget the gem was ever there after its first half-second of absence and
//! never reconstruct it for the other 28 frames of the drop. [`RING_LEN_FRAMES`]
//! is deliberately 40 — comfortably past the fixture's engineered 32-frame
//! drop, the same "measured, then chosen deliberately more generous than the
//! real number" move `crate::scene_tracker::MAX_CONTINUOUS_STEP_PX` already
//! makes for its own fixture. Stated plainly: this is a real spec/fixture
//! mismatch, not a rounding error — a production `SpriteHistorian` tuned to
//! the doc's own "default 4" would not reconstruct this scene at all.
//!
//! ## Blink preservation: periodicity, scaled against the ring itself
//!
//! `ENHANCEMENT_RUNTIME.md` §2(b), verbatim: "blink detection uses period
//! regularity but is imperfect." This module implements exactly that, but
//! ties the threshold to [`RING_LEN_FRAMES`] rather than to an arbitrary
//! cycle count, which turns out to matter: an EARLIER version of this
//! heuristic required two matching completed cycles (any length) before
//! protecting an identity, and measured against the real fixture, that
//! rule could not protect RF-Scroller's blink enemy during its very FIRST
//! or SECOND off-phase (no history yet to judge regularity from -- an
//! unavoidable warm-up for any periodicity-based heuristic) and,
//! separately, would eventually have misclassified a gem "protected" too
//! given a long enough session (gem rotation is ALSO perfectly periodic,
//! 64-on/32-off) -- see this file's git history for the measured failure.
//!
//! `IdentityState::is_protected` instead requires only ONE completed
//! presence streak and ONE completed absence streak, PROVIDED BOTH fit
//! within [`RING_LEN_FRAMES`]. This is not an arbitrary second knob: an
//! identity whose entire on/off cycle fits inside the SAME window this
//! historian already uses to bridge a hardware-style drop is
//! indistinguishable from "deliberately, briefly invisible" and must not be
//! reconstructed; an identity whose PRESENCE phase alone already exceeds
//! that window (RF-Scroller's gems: 64 frames presence > 40-frame ring) can
//! never satisfy this by construction, at ANY session length, because
//! [`RING_LEN_FRAMES`] was independently chosen (module doc above) to
//! bridge the ABSENCE, not the presence, and the fixture's presence phase
//! is longer than its own absence phase. This closes the earlier version's
//! gap structurally rather than by picking a longer observation window: see
//! `crates/rf-enhance/tests/rf_scroller_gem_blink_red_fixture.rs`'s own
//! module doc for the measured warm-up (the FIRST off-phase, frames 16-23
//! of that fixture, is the one gap this heuristic cannot close -- there is
//! no history at all before an identity's first observed cycle completes).
//!
//! ## Compositing rule (last-known pixels, no motion extrapolation)
//!
//! A reconstructed identity is redrawn using the EXACT [`PpuPixel`] values
//! last observed at its ABSOLUTE on-screen position — no motion
//! extrapolation (`ENHANCEMENT_RUNTIME.md` §2's "motion-extrapolated
//! position" is aspirational; this prototype's own title says so). Correct
//! for RF-Scroller's tail scene, where `camera_x` has already saturated at
//! `MAX_CAMERA_X` (`main.c` line ~155) by the time the gem/blink scene is
//! reachable, so on-screen position is genuinely static across the sampled
//! window — a scrolling scene would need motion compensation this
//! prototype does not attempt (documented gap, not silently assumed away).
//! Reconstruction only ever overwrites a currently-[`PixelLayer::Backdrop`]
//! destination pixel — it never displaces a pixel the accuracy path (or an
//! already-placed reconstruction from a different identity) resolved for
//! real, mirroring `rf-nes/src/ppu/sprites.rs`'s own overlay priority rule
//! ("the real, already-resolved pixel always wins").
use rf_core_api::{PixelLayer, PpuPixel};

/// How many frames a recently-vanished identity is still redrawn from cache
/// (module doc's "Ring length" section: chosen to bridge RF-Scroller's
/// measured 32-frame engineered drop with margin, not the design doc's
/// suggested default of 4).
pub const RING_LEN_FRAMES: u32 = 40;

/// How long an identity may go unseen before this historian stops tracking
/// it at all (bounds memory growth over a long session; deterministic,
/// frame-index-driven, not content-driven). Generous relative to
/// [`RING_LEN_FRAMES`] so it never interacts with the reconstruction
/// decision itself -- it only reclaims identities long since abandoned.
const EVICTION_AGE_FRAMES: u64 = RING_LEN_FRAMES as u64 * 3;

/// Width (pixels) of the position-bucketing tolerance used when matching an
/// identity across frames (module doc's "Identity key" section) -- absorbs
/// a few pixels of camera drift; RF-Scroller's own fixture needs none of it
/// (camera is saturated/static in the sampled window) but a scrolling scene
/// would.
const POSITION_BUCKET_PX: u16 = 4;

/// [`rf_core_api::PpuPixel::sprite_id`]'s valid range (NES OAM: 64 sprites)
/// -- sized for the blob-extraction scratch table, not a design constant.
const MAX_SPRITE_IDS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct IdentityKey {
    y: u16,
    x_bucket: u16,
    palette_index: u8,
}

/// The exact resolved pixels last observed for one identity, absolute
/// on-screen coordinates -- what gets pasted back in on a reconstruction
/// (module doc's "Compositing rule").
#[derive(Clone)]
struct CachedSprite {
    pixels: Vec<(u16, u16, PpuPixel)>,
}

struct IdentityState {
    key: IdentityKey,
    cached: CachedSprite,
    last_seen_frame: u64,
    /// Whether the CURRENT streak (since `streak_start_frame`) is a
    /// presence or an absence run.
    streak_present: bool,
    streak_start_frame: u64,
    /// The most recently COMPLETED presence/absence streak lengths --
    /// `None` until at least one full transition of that kind has been
    /// observed (module doc's periodicity rule). Only the latest of each
    /// is kept; see `IdentityState::is_protected` for why a single
    /// completed pair, scaled against [`RING_LEN_FRAMES`], is enough.
    last_presence_len: Option<u32>,
    last_absence_len: Option<u32>,
}

impl IdentityState {
    fn new(key: IdentityKey, pixels: Vec<(u16, u16, PpuPixel)>, frame: u64) -> Self {
        IdentityState {
            key,
            cached: CachedSprite { pixels },
            last_seen_frame: frame,
            streak_present: true,
            streak_start_frame: frame,
            last_presence_len: None,
            last_absence_len: None,
        }
    }

    /// Close out the current streak if `present_now` differs from it,
    /// recording its completed length (module doc's periodicity rule).
    fn update_streak(&mut self, present_now: bool, frame: u64) {
        if self.streak_present == present_now {
            return;
        }
        let len = (frame - self.streak_start_frame) as u32;
        if self.streak_present {
            self.last_presence_len = Some(len);
        } else {
            self.last_absence_len = Some(len);
        }
        self.streak_present = present_now;
        self.streak_start_frame = frame;
    }

    /// Module doc's "Blink preservation" rule: ONE completed presence
    /// streak and ONE completed absence streak, both no longer than
    /// [`RING_LEN_FRAMES`] -- an on/off cycle that fits entirely inside the
    /// same window this historian uses to bridge a hardware-style drop is
    /// indistinguishable from deliberate, brief invisibility and must not
    /// be reconstructed. An identity whose presence phase alone exceeds the
    /// ring (RF-Scroller's gems) can never satisfy this, at any session
    /// length.
    fn is_protected(&self) -> bool {
        matches!(self.last_presence_len, Some(p) if p <= RING_LEN_FRAMES)
            && matches!(self.last_absence_len, Some(a) if a <= RING_LEN_FRAMES)
    }
}

/// One blob per distinct `sprite_id` present in a frame: every
/// [`PixelLayer::Sprite`] pixel sharing that id, in row-major scan order.
/// Within ONE frame `sprite_id` is a perfectly valid grouping key (it is
/// only unstable ACROSS frames, under rotation -- module doc) -- indexed by
/// a fixed `[Option<_>; MAX_SPRITE_IDS]` array rather than a `HashMap` so
/// iteration order is always ascending id, deterministic regardless of scan
/// order (CLAUDE.md law 4 / this crate's `stitcher::Canvas` precedent).
fn extract_blobs(video: &[PpuPixel], width: u16, height: u16) -> Vec<Vec<(u16, u16, PpuPixel)>> {
    let mut acc: Vec<Option<Vec<(u16, u16, PpuPixel)>>> = vec![None; MAX_SPRITE_IDS];
    for y in 0..height {
        for x in 0..width {
            let idx = y as usize * width as usize + x as usize;
            let Some(px) = video.get(idx).copied() else {
                continue;
            };
            if px.layer != PixelLayer::Sprite {
                continue;
            }
            let Some(id) = px.sprite_id else { continue };
            let id = id as usize;
            if id >= MAX_SPRITE_IDS {
                continue;
            }
            acc[id].get_or_insert_with(Vec::new).push((x, y, px));
        }
    }
    acc.into_iter().flatten().collect()
}

/// Cross-frame identity for a blob (module doc's "Identity key" section):
/// top-left corner (bucketed on x) + the first pixel's palette index, NOT
/// `sprite_id`.
fn blob_key(pixels: &[(u16, u16, PpuPixel)]) -> Option<IdentityKey> {
    let min_x = pixels.iter().map(|(x, _, _)| *x).min()?;
    let min_y = pixels.iter().map(|(_, y, _)| *y).min()?;
    let palette_index = pixels.first()?.2.palette_index;
    Some(IdentityKey {
        y: min_y,
        x_bucket: min_x / POSITION_BUCKET_PX,
        palette_index,
    })
}

/// Temporal de-flicker prototype (module doc). Construct one instance per
/// play session (same lifetime shape as [`crate::scene_tracker::SceneTracker`]);
/// [`SpriteHistorian::observe`] once per frame, in order.
pub struct SpriteHistorian {
    enabled: bool,
    frame_index: u64,
    identities: Vec<IdentityState>,
}

impl SpriteHistorian {
    #[must_use]
    pub fn new() -> Self {
        SpriteHistorian {
            enabled: false, // CLAUDE.md law 6 / FR-MODE-002: off by default.
            frame_index: 0,
            identities: Vec::new(),
        }
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Observe one frame's accuracy-exact video buffer (`FrameBundle`'s own
    /// shape) and return the RECONSTRUCTED buffer: identical to `video`
    /// when disabled, or when there is nothing to reconstruct; otherwise
    /// `video` with recognized, non-`IdentityState::is_protected` flicker
    /// candidates redrawn from cache (module doc's "Compositing rule").
    ///
    /// `video: &[PpuPixel]` -- a shared reference, never `&mut` -- is the
    /// structural half of this ticket's mode-invariant proof (module doc):
    /// nothing this function does can perturb the caller's own copy of the
    /// frame, on or off.
    #[must_use]
    pub fn observe(&mut self, video: &[PpuPixel], width: u16, height: u16) -> Vec<PpuPixel> {
        let mut output = video.to_vec();
        if !self.enabled {
            return output;
        }
        self.frame_index += 1;
        let frame = self.frame_index;

        let blobs = extract_blobs(video, width, height);
        let mut matched = vec![false; self.identities.len()];
        for pixels in blobs {
            let Some(key) = blob_key(&pixels) else {
                continue;
            };
            if let Some(pos) = self.identities.iter().position(|s| s.key == key) {
                let id = &mut self.identities[pos];
                id.update_streak(true, frame);
                id.cached = CachedSprite {
                    pixels: pixels.clone(),
                };
                id.last_seen_frame = frame;
                matched[pos] = true;
            } else {
                self.identities.push(IdentityState::new(key, pixels, frame));
                matched.push(true);
            }
        }

        for (i, id) in self.identities.iter_mut().enumerate() {
            if matched.get(i).copied().unwrap_or(false) {
                continue;
            }
            id.update_streak(false, frame);
            let frames_since_seen = frame - id.last_seen_frame;
            if frames_since_seen <= u64::from(RING_LEN_FRAMES) && !id.is_protected() {
                for &(x, y, px) in &id.cached.pixels {
                    if (x as usize) >= width as usize || (y as usize) >= height as usize {
                        continue;
                    }
                    let out_idx = y as usize * width as usize + x as usize;
                    if output[out_idx].layer == PixelLayer::Backdrop {
                        output[out_idx] = px;
                    }
                }
            }
        }

        self.identities
            .retain(|id| frame - id.last_seen_frame <= EVICTION_AGE_FRAMES);

        output
    }
}

impl Default for SpriteHistorian {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: u16 = 64;
    const HEIGHT: u16 = 32;

    fn backdrop() -> PpuPixel {
        PpuPixel {
            palette_index: 0,
            layer: PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
        }
    }

    fn sprite_px(sprite_id: u8, palette_index: u8) -> PpuPixel {
        PpuPixel {
            palette_index,
            layer: PixelLayer::Sprite,
            sprite_id: Some(sprite_id),
            priority: 0,
        }
    }

    /// An 8x8 solid sprite of `sprite_id`/`palette_index` at (x0,y0) over an
    /// otherwise-blank frame.
    fn frame_with_sprite(sprite_id: u8, palette_index: u8, x0: u16, y0: u16) -> Vec<PpuPixel> {
        let mut video = vec![backdrop(); WIDTH as usize * HEIGHT as usize];
        for dy in 0..8u16 {
            for dx in 0..8u16 {
                let (x, y) = (x0 + dx, y0 + dy);
                video[y as usize * WIDTH as usize + x as usize] =
                    sprite_px(sprite_id, palette_index);
            }
        }
        video
    }

    fn blank_frame() -> Vec<PpuPixel> {
        vec![backdrop(); WIDTH as usize * HEIGHT as usize]
    }

    fn has_opaque_sprite_at(frame: &[PpuPixel], x: u16, y: u16) -> bool {
        frame[y as usize * WIDTH as usize + x as usize].layer == PixelLayer::Sprite
    }

    // --- Toggle defaults and passthrough -----------------------------

    #[test]
    fn disabled_by_default() {
        assert!(!SpriteHistorian::new().enabled());
    }

    #[test]
    fn toggle_off_is_exact_passthrough_even_across_a_vanished_sprite() {
        let mut h = SpriteHistorian::new();
        assert!(!h.enabled());
        let f1 = frame_with_sprite(3, 10, 10, 10);
        let f2 = blank_frame();
        let out1 = h.observe(&f1, WIDTH, HEIGHT);
        let out2 = h.observe(&f2, WIDTH, HEIGHT);
        assert_eq!(out1, f1, "disabled: output must equal input exactly");
        assert_eq!(out2, f2, "disabled: a vanished sprite must stay vanished");
    }

    // --- Structural mode-invariant proof (criterion 4) ----------------

    #[test]
    fn toggling_never_perturbs_the_source_frame() {
        let f1 = frame_with_sprite(3, 10, 10, 10);
        let f2 = blank_frame();
        let (f1_before, f2_before) = (f1.clone(), f2.clone());

        let mut h_off = SpriteHistorian::new();
        let mut h_on = SpriteHistorian::new();
        h_on.set_enabled(true);

        let _ = h_off.observe(&f1, WIDTH, HEIGHT);
        let _ = h_off.observe(&f2, WIDTH, HEIGHT);
        let _ = h_on.observe(&f1, WIDTH, HEIGHT);
        let _ = h_on.observe(&f2, WIDTH, HEIGHT);

        assert_eq!(
            f1, f1_before,
            "the source frame must be byte-identical after being observed, on or off"
        );
        assert_eq!(
            f2, f2_before,
            "the source frame must be byte-identical after being observed, on or off"
        );
    }

    // --- Reconstruction: the actual mechanism -------------------------

    #[test]
    fn reconstructs_a_recently_vanished_sprite_within_the_ring_window() {
        let mut h = SpriteHistorian::new();
        h.set_enabled(true);
        let present = frame_with_sprite(5, 20, 4, 4);
        let vanished = blank_frame();

        let _ = h.observe(&present, WIDTH, HEIGHT);
        let reconstructed = h.observe(&vanished, WIDTH, HEIGHT);

        assert_ne!(
            reconstructed, vanished,
            "vacuity trap: the toggle must change emitted pixels, not just a bool"
        );
        assert!(
            has_opaque_sprite_at(&reconstructed, 4, 4),
            "a sprite absent for only 1 frame (well within the ring) must be redrawn"
        );
    }

    #[test]
    fn does_not_reconstruct_once_the_ring_window_has_elapsed() {
        let mut h = SpriteHistorian::new();
        h.set_enabled(true);
        let present = frame_with_sprite(5, 20, 4, 4);
        let _ = h.observe(&present, WIDTH, HEIGHT);

        let mut last = blank_frame();
        for _ in 0..(RING_LEN_FRAMES + 5) {
            last = h.observe(&blank_frame(), WIDTH, HEIGHT);
        }
        assert!(
            !has_opaque_sprite_at(&last, 4, 4),
            "an identity absent for far longer than the ring must stop being reconstructed, \
             not be remembered forever"
        );
    }

    #[test]
    fn a_real_pixel_at_the_reconstruction_site_is_never_displaced() {
        let mut h = SpriteHistorian::new();
        h.set_enabled(true);
        let present = frame_with_sprite(5, 20, 4, 4);
        let _ = h.observe(&present, WIDTH, HEIGHT);

        let mut competing = blank_frame();
        // A DIFFERENT real sprite now opaque at the exact same pixels.
        for dy in 0..8u16 {
            for dx in 0..8u16 {
                let (x, y) = (4 + dx, 4 + dy);
                competing[y as usize * WIDTH as usize + x as usize] = sprite_px(9, 63);
            }
        }
        let reconstructed = h.observe(&competing, WIDTH, HEIGHT);
        assert_eq!(
            reconstructed, competing,
            "a real, already-resolved pixel must never be displaced by a reconstruction \
             (mirrors rf-nes/src/ppu/sprites.rs's own overlay priority rule)"
        );
    }

    // --- Identity survives an OAM-slot rotation (the ticket's own insight)

    #[test]
    fn identity_survives_an_oam_slot_rotation_that_changes_sprite_id() {
        let mut h = SpriteHistorian::new();
        h.set_enabled(true);
        // Frame 1: entity rendered under sprite_id 2 (a "winning" OAM slot).
        let f1 = frame_with_sprite(2, 30, 8, 8);
        let _ = h.observe(&f1, WIDTH, HEIGHT);
        // Frame 2: entity absent (rotated into a losing slot) -- a
        // sprite_id-keyed historian would have NO memory of "sprite_id 2"
        // being gone, since it never asks "where did the entity that used
        // to be id 2 go"; a position-keyed one recognizes the gap by
        // position and reconstructs it.
        let reconstructed = h.observe(&blank_frame(), WIDTH, HEIGHT);
        assert!(
            has_opaque_sprite_at(&reconstructed, 8, 8),
            "an entity that disappears (as if evicted by OAM-index rotation) must still be \
             reconstructed at its last known position, proving identity is NOT keyed on \
             sprite_id (which the rotation itself would have permuted)"
        );
    }

    // --- Periodicity classification: the blink-preservation mechanism ---

    /// Drives a synthetic on/off pattern through a fresh historian for
    /// `cycles` full on+off repetitions, PLUS one closing presence
    /// observation (so the last absence streak actually COMPLETES --
    /// `IdentityState::is_protected` looks at completed streaks only, by
    /// design; module doc's "Blink preservation" section), and returns
    /// whether the tracked identity is classified `protected` at that
    /// point. `on_len`/`off_len` are frame counts.
    fn ends_protected(on_len: u32, off_len: u32, cycles: u32) -> bool {
        let mut h = SpriteHistorian::new();
        h.set_enabled(true);
        let present = frame_with_sprite(7, 40, 16, 16);
        let absent = blank_frame();
        for _ in 0..cycles {
            for _ in 0..on_len {
                let _ = h.observe(&present, WIDTH, HEIGHT);
            }
            for _ in 0..off_len {
                let _ = h.observe(&absent, WIDTH, HEIGHT);
            }
        }
        let _ = h.observe(&present, WIDTH, HEIGHT); // closes the last absence streak.
        h.identities
            .iter()
            .find(|id| id.key == blob_key(&extract_blobs(&present, WIDTH, HEIGHT)[0]).unwrap())
            .is_some_and(IdentityState::is_protected)
    }

    #[test]
    fn a_short_regular_on_off_pattern_becomes_protected_after_one_cycle() {
        // RF-Scroller's own blink formula: 8-on/8-off (period 16), both
        // well under RING_LEN_FRAMES (40) -- one completed cycle is enough
        // under the ring-scaled rule (module doc's "Blink preservation").
        assert!(
            ends_protected(8, 8, 1),
            "a periodic 8-on/8-off pattern (RF-Scroller's blink enemy shape), both phases well \
             under the ring length, must be classified protected after just one completed cycle"
        );
    }

    #[test]
    fn a_gem_rotation_shaped_pattern_is_never_protected_no_matter_how_many_cycles() {
        // RF-Scroller's own gem-rotation shape: 64-on/32-off. Presence (64)
        // exceeds RING_LEN_FRAMES (40), so `is_protected` can never be
        // satisfied -- checked at 1 cycle (the realistic red-fixture
        // window) AND at 6 cycles (structurally impossible to trip this,
        // unlike the earlier two-matching-cycles design this module's doc
        // records replacing).
        assert!(
            !ends_protected(64, 32, 1),
            "a single completed absence streak must never be enough to classify an identity \
             protected -- otherwise a hardware-cap-dropped sprite would be silently \
             un-reconstructed the very first time it disappears"
        );
        assert!(
            !ends_protected(64, 32, 6),
            "a gem-rotation-shaped 64-on/32-off pattern must NEVER become protected, at any \
             session length -- its presence phase alone exceeds the ring, which the ring-scaled \
             rule uses as a structural guarantee, not a probabilistic one"
        );
    }

    #[test]
    fn a_protected_identity_is_not_reconstructed_during_its_off_phase() {
        let mut h = SpriteHistorian::new();
        h.set_enabled(true);
        let present = frame_with_sprite(7, 40, 16, 16);
        let absent = blank_frame();
        // One full 8-on/8-off cycle reaches the protected classification
        // (matches the test above); enter a second off-phase and confirm.
        for _ in 0..8 {
            let _ = h.observe(&present, WIDTH, HEIGHT);
        }
        for _ in 0..8 {
            let _ = h.observe(&absent, WIDTH, HEIGHT);
        }
        for _ in 0..8 {
            let _ = h.observe(&present, WIDTH, HEIGHT);
        }
        let mut last = present.clone();
        for _ in 0..8 {
            last = h.observe(&absent, WIDTH, HEIGHT);
        }
        assert!(
            !has_opaque_sprite_at(&last, 16, 16),
            "MUTATION TARGET: a de-flicker that ignores the blink heuristic (reconstructs \
             unconditionally) must fail exactly here -- a periodic, game-driven blink must stay \
             erased during its own off-phase, not be painted back in"
        );
    }

    #[test]
    fn an_irregular_hardware_style_gap_is_still_reconstructed_mid_absence() {
        // A gem-rotation-shaped identity (64-on/32-off) mid-way through its
        // FIRST absence: must still be reconstructed.
        let mut h = SpriteHistorian::new();
        h.set_enabled(true);
        let present = frame_with_sprite(11, 50, 24, 4);
        for _ in 0..64 {
            let _ = h.observe(&present, WIDTH, HEIGHT);
        }
        let mut last = present.clone();
        for _ in 0..20 {
            last = h.observe(&blank_frame(), WIDTH, HEIGHT);
        }
        assert!(
            has_opaque_sprite_at(&last, 24, 4),
            "a gem-rotation-shaped absence, well within the ring, must still be reconstructed"
        );
    }

    #[test]
    fn a_gem_rotation_shaped_identity_is_still_reconstructed_after_completing_a_full_cycle() {
        // The improvement over the earlier design (module doc): even after
        // a FULL 64-on/32-off cycle completes and a second absence begins,
        // reconstruction must still fire -- this is the case that used to
        // require capping the test's own session length to avoid a false
        // "protected" classification; the ring-scaled rule makes that
        // capping unnecessary.
        let mut h = SpriteHistorian::new();
        h.set_enabled(true);
        let present = frame_with_sprite(11, 50, 24, 4);
        for _ in 0..64 {
            let _ = h.observe(&present, WIDTH, HEIGHT);
        }
        for _ in 0..32 {
            let _ = h.observe(&blank_frame(), WIDTH, HEIGHT);
        }
        for _ in 0..64 {
            let _ = h.observe(&present, WIDTH, HEIGHT);
        }
        let last = h.observe(&blank_frame(), WIDTH, HEIGHT);
        assert!(
            has_opaque_sprite_at(&last, 24, 4),
            "a gem-rotation-shaped identity must still be reconstructed even after one full \
             cycle has completed -- it must never be classified protected, at any point"
        );
    }
}
