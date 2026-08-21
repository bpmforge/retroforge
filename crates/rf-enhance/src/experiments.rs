//! Smooth camera and room stitching (ticket W8-08; ROADMAP P8).
//!
//! Two enhancements that share one problem: **both show the player
//! something the hardware never produced.** Smooth camera invents
//! positions between the game's own updates; room stitching shows rooms
//! the player is not currently in. Neither is a decoding of observed
//! output the way de-flicker or the sprite-limit bypass are, so P8's exit
//! criteria — "golden/A-B tests + per-game override knobs +
//! honesty-contract UI labels" — are not decoration here, they are what
//! makes the features admissible under law 6 at all.
//!
//! ## The three requirements are met by things that already exist
//!
//! * **Per-game override knobs**: [`crate::trust::TrustLadder::pin`],
//!   which FR-ENH-011 already defines as "profiles may pin states". A
//!   second override mechanism would be a second thing to disagree with
//!   the first.
//! * **Honesty labels**: [`HonestyLabel`], carried by the feature rather
//!   than written into a UI, so a surface that forgets to show one is a
//!   visible omission instead of an invisible one.
//! * **A-B tests**: each feature's test asserts the *accuracy-exact*
//!   output is byte-identical with the feature on and off, and that the
//!   enhanced output differs. Both halves matter: the first is law 6, the
//!   second is anti-vacuity.
//!
//! ## No floats
//!
//! Interpolation is fixed-point (`alpha` is 0..=255) rather than `f32`.
//! ARCHITECTURE's determinism rule is about core state, and this is not
//! core state — but a replay that reproduces only on the same FPU is a
//! replay that does not reproduce, and there is no reason to accept that
//! for a feature whose whole output is positions.

use std::collections::BTreeMap;

use rf_profiles::schema::Profile;

use crate::scene_identity::SceneId;
use crate::stitcher::Canvas;

/// What a feature is showing that the hardware did not.
///
/// Carried by the feature so a UI cannot show the picture without having
/// been handed the claim that goes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HonestyLabel {
    /// Stable identifier, also the trust-ladder heuristic name.
    pub feature: &'static str,
    /// One sentence, shown verbatim. Written in terms of what the user is
    /// looking at, not what the code does.
    pub claim: &'static str,
    /// **True when the feature INVENTS data** rather than revealing data
    /// the machine produced. The distinction a badge must not blur:
    /// stitching shows real pixels from earlier, interpolation shows
    /// positions that never existed.
    pub invents: bool,
}

/// Trust-ladder name and label for smooth camera.
pub const SMOOTH_CAMERA: &str = "smooth_camera";
/// Trust-ladder name and label for room stitching.
pub const ROOM_STITCH: &str = "room_stitch";

// ---------------------------------------------------------------------
// Smooth camera
// ---------------------------------------------------------------------

/// Profile-gated entity interpolation.
///
/// A game moves its camera in whole pixels once per frame. At a higher
/// display rate, or with a fractional scroll, the honest options are to
/// repeat a position or to invent one between the two the game actually
/// used. This does the latter, which is why it is off unless a profile
/// says the game tolerates it.
#[derive(Debug, Clone, Default)]
pub struct SmoothCamera {
    enabled: bool,
    prev: Option<(i64, i64)>,
    cur: Option<(i64, i64)>,
}

impl SmoothCamera {
    /// Read the gate from `[capabilities].smooth_camera`.
    ///
    /// **The profile enables the CAPABILITY, not the feature.** As with
    /// `[mods]` and `[widescreen]`, a profile states that this game can
    /// take interpolation; whether it is switched on is the trust
    /// ladder's business and ultimately the user's.
    #[must_use]
    pub fn from_profile(profile: &Profile) -> Self {
        Self {
            enabled: profile.capabilities.smooth_camera,
            prev: None,
            cur: None,
        }
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        self.enabled
    }

    /// Record this frame's camera position.
    pub fn observe(&mut self, pos: (i64, i64)) {
        self.prev = self.cur;
        self.cur = Some(pos);
    }

    /// The position `alpha`/255 of the way from the previous frame to the
    /// current one.
    ///
    /// Returns `None` — meaning "use the real position" — when the feature
    /// is unavailable or there is no previous frame to interpolate from.
    /// A caller that treats `None` as (0, 0) would slam the camera to the
    /// origin on the first frame, so the type makes that hard to write.
    #[must_use]
    pub fn interpolated(&self, alpha: u8) -> Option<(i64, i64)> {
        if !self.enabled {
            return None;
        }
        let (prev, cur) = (self.prev?, self.cur?);
        let lerp = |a: i64, b: i64| a + ((b - a) * i64::from(alpha)) / 255;
        Some((lerp(prev.0, cur.0), lerp(prev.1, cur.1)))
    }

    #[must_use]
    pub fn label() -> HonestyLabel {
        HonestyLabel {
            feature: SMOOTH_CAMERA,
            claim: "Camera motion is interpolated between the game's own updates; \
                    in-between positions are generated, not from the game.",
            invents: true,
        }
    }
}

// ---------------------------------------------------------------------
// Room stitching
// ---------------------------------------------------------------------

/// Room stitching for top-down profiles.
///
/// One [`Canvas`] per room, keyed by [`SceneId`]. Rooms are kept apart
/// rather than merged into one plane because a top-down game's rooms do
/// not share a coordinate space — two rooms both start at (0, 0), and
/// stitching them into one canvas would overlay them.
#[derive(Debug, Clone, Default)]
pub struct RoomStitcher {
    enabled: bool,
    rooms: BTreeMap<SceneId, Canvas>,
}

impl RoomStitcher {
    /// Gate on `[camera].mode == "top_down_rooms"`.
    ///
    /// A side-scroller's canvas is one continuous plane and
    /// [`crate::stitcher`] already handles it; rooms are the case that
    /// needs its own keying, so the mode is the honest gate rather than a
    /// separate capability bit somebody would have to remember to set.
    #[must_use]
    pub fn from_profile(profile: &Profile) -> Self {
        let enabled = profile
            .camera
            .as_ref()
            .is_some_and(|c| c.mode == "top_down_rooms");
        Self {
            enabled,
            rooms: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// The canvas for one room, created empty on first visit.
    ///
    /// Returns `None` when the feature is unavailable, so a caller cannot
    /// accumulate rooms for a game whose profile never asked for it.
    pub fn room_mut(&mut self, id: SceneId) -> Option<&mut Canvas> {
        if !self.enabled {
            return None;
        }
        Some(self.rooms.entry(id).or_default())
    }

    #[must_use]
    pub fn room(&self, id: SceneId) -> Option<&Canvas> {
        self.rooms.get(&id)
    }

    /// Every room seen so far, in a stable order.
    ///
    /// `BTreeMap` rather than `HashMap` so two runs that visited the same
    /// rooms produce the same order — a map view that reshuffled itself
    /// between runs would make its own golden untestable.
    pub fn rooms(&self) -> impl Iterator<Item = (&SceneId, &Canvas)> {
        self.rooms.iter()
    }

    #[must_use]
    pub fn label() -> HonestyLabel {
        HonestyLabel {
            feature: ROOM_STITCH,
            claim: "Rooms you have visited are shown together; unvisited area is \
                    fogged and no room is drawn from anything but pixels the game \
                    produced.",
            // Stitching REVEALS observed pixels rather than generating
            // new ones — the opposite of interpolation, and the reason
            // these two carry different labels rather than one shared
            // "enhanced" badge.
            invents: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::{TrustLadder, TrustState};

    fn profile_with(smooth: bool, camera_mode: Option<&str>) -> Profile {
        let mut toml = format!(
            "[meta]\nprofile_version = \"0.1\"\ntitle = \"T\"\nconsole = \"snes\"\n\
             region = \"ntsc\"\n\n[capabilities]\nsmooth_camera = {smooth}\n"
        );
        if let Some(mode) = camera_mode {
            toml.push_str(&format!("\n[camera]\nmode = \"{mode}\"\n"));
        }
        rf_profiles::load_str(&toml).expect("valid profile").profile
    }

    /// **A-B, smooth camera.** With the capability off the interpolator is
    /// inert; with it on the in-between position differs from both
    /// endpoints. Both halves are the test: the first is law 6, the second
    /// is what stops "inert" passing for "working".
    #[test]
    fn smooth_camera_is_inert_until_a_profile_allows_it_and_then_interpolates() {
        let mut off = SmoothCamera::from_profile(&profile_with(false, None));
        off.observe((0, 0));
        off.observe((100, 40));
        assert!(!off.is_available());
        assert_eq!(
            off.interpolated(128),
            None,
            "with the capability off there is no interpolation to be had - a caller \
             gets None and uses the game's own position"
        );

        let mut on = SmoothCamera::from_profile(&profile_with(true, None));
        on.observe((0, 0));
        on.observe((100, 40));
        assert!(on.is_available());
        let mid = on.interpolated(128).expect("a midpoint exists");
        assert_eq!(mid, (50, 20), "half of the way from (0,0) to (100,40)");
        assert_ne!(mid, (0, 0));
        assert_ne!(mid, (100, 40));

        // The endpoints are exact, so a caller stepping alpha 0..=255
        // starts and ends on positions the game really used.
        assert_eq!(on.interpolated(0), Some((0, 0)));
        assert_eq!(on.interpolated(255), Some((100, 40)));
    }

    /// The first frame has nothing to interpolate from, and says so rather
    /// than inventing an origin.
    #[test]
    fn the_first_frame_has_no_interpolated_position() {
        let mut c = SmoothCamera::from_profile(&profile_with(true, None));
        c.observe((30, 30));
        assert_eq!(
            c.interpolated(128),
            None,
            "treating a missing previous frame as (0,0) would slam the camera to \
             the origin on the first frame"
        );
    }

    /// **A-B, room stitching.** Gated on the camera mode, and inert for a
    /// side-scroller whose canvas the plain stitcher already handles.
    #[test]
    fn room_stitching_is_gated_on_the_top_down_camera_mode() {
        let mut side = RoomStitcher::from_profile(&profile_with(false, Some("side_scroller")));
        assert!(!side.is_available());
        assert!(
            side.room_mut(SceneId(1)).is_none(),
            "a side-scroller must not accumulate rooms - its canvas is one plane"
        );
        assert_eq!(side.room_count(), 0);

        let mut rooms = RoomStitcher::from_profile(&profile_with(false, Some("top_down_rooms")));
        assert!(rooms.is_available());
        assert!(rooms.room_mut(SceneId(1)).is_some());
        assert!(rooms.room_mut(SceneId(2)).is_some());
        assert!(
            rooms.room_mut(SceneId(1)).is_some(),
            "revisiting is not a new room"
        );
        assert_eq!(rooms.room_count(), 2);
    }

    /// Rooms are kept apart, not merged: two rooms both start at (0, 0),
    /// so one plane would overlay them.
    #[test]
    fn two_rooms_get_two_canvases() {
        let mut rooms = RoomStitcher::from_profile(&profile_with(false, Some("top_down_rooms")));
        rooms.room_mut(SceneId(7));
        rooms.room_mut(SceneId(9));
        let ids: Vec<u64> = rooms.rooms().map(|(id, _)| id.0).collect();
        assert_eq!(
            ids,
            vec![7, 9],
            "and in a stable order, or a map view's own golden could not exist"
        );
        assert!(rooms.room(SceneId(8)).is_none());
    }

    /// **The honesty labels distinguish what the two features do**, which
    /// is the whole reason they are not one shared "enhanced" badge.
    #[test]
    fn the_labels_say_which_feature_invents_and_which_reveals() {
        let smooth = SmoothCamera::label();
        let stitch = RoomStitcher::label();
        assert!(
            smooth.invents,
            "interpolation generates positions the game never used"
        );
        assert!(
            !stitch.invents,
            "stitching shows pixels the game really produced, just earlier"
        );
        assert_ne!(smooth.claim, stitch.claim);
        for l in [smooth, stitch] {
            assert!(!l.claim.is_empty());
            assert!(
                l.claim.ends_with('.'),
                "a claim is shown verbatim to a user, so it is a sentence"
            );
        }
    }

    /// **Per-game override knobs** are the trust ladder's, not a second
    /// mechanism: a profile may pin either feature, and a pin outranks the
    /// live state.
    #[test]
    fn a_profile_can_pin_either_feature_through_the_trust_ladder() {
        let mut ladder = TrustLadder::new();
        assert!(
            !ladder.should_act(SMOOTH_CAMERA, "scene"),
            "fresh install is all-shadow, so neither feature acts"
        );
        ladder.pin(SMOOTH_CAMERA, TrustState::Active);
        assert!(ladder.should_act(SMOOTH_CAMERA, "scene"));
        assert!(
            !ladder.should_act(ROOM_STITCH, "scene"),
            "pinning one feature must not enable the other"
        );
    }
}
