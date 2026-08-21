//! Frame-interpolation candidacy (ticket W8-09;
//! `docs/design/FRAME_INTERPOLATION.md`).
//!
//! **This module does not interpolate anything.** W8-09 is a design-first
//! ticket, and the design's §7 declines to implement blending: a full
//! frame of added input latency is inherent to interpolating toward a
//! frame you must first possess, and it is a poor trade in an emulator.
//!
//! What it delivers instead is the question any future implementation has
//! to answer before it blends a single pixel: **given two consecutive
//! frames, may this pair be interpolated at all?** Settling that here,
//! with tests, means the answer exists before a blender does — rather
//! than being discovered later from a bug report about cross-fading
//! cutscenes.
//!
//! The refusals come straight from the design's §3, and two of them are
//! answerable only because other tickets already built the detectors:
//! scene identity (§3.3) from `crate::scene_identity`, and HUD bands
//! (§3.5) from `crate::hud`.

use crate::scene_identity::SceneId;

/// Why a frame pair must not be interpolated.
///
/// Each variant is one of `FRAME_INTERPOLATION.md` §3's cases, kept as
/// distinct variants rather than a bool so a UI or a log can say which
/// hazard fired — "not interpolated" and "not interpolated because the
/// scene cut" are different facts to anyone debugging judder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// §3.3 — a scene change sits between the two frames. Blending across
    /// it produces a cross-fade where the game had an instant cut.
    SceneCut,
    /// §3.4 — the palette changed, so the difference between the frames is
    /// colour rather than motion, and a midpoint colour existed nowhere.
    PaletteChanged,
    /// The pair is not consecutive, so there is nothing to interpolate
    /// *between* — a dropped or rewound frame.
    NotConsecutive,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Refusal::SceneCut => {
                "a scene change lies between these frames; blending would cross-fade a cut"
            }
            Refusal::PaletteChanged => {
                "the palette changed, so the difference is colour rather than motion"
            }
            Refusal::NotConsecutive => "the frames are not consecutive",
        };
        f.write_str(s)
    }
}

/// One frame's properties, as far as candidacy is concerned.
///
/// Deliberately not the frame itself: candidacy is a question about
/// *context*, and a function that took pixels would invite someone to
/// answer it by looking at them — which §3 is precisely a list of reasons
/// not to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameContext {
    pub index: u64,
    pub scene: SceneId,
    /// A digest of the frame's palette. Any change at all disqualifies the
    /// pair, so the value only needs to differ when the palette does.
    pub palette_digest: u64,
}

/// The verdict, with the region a blender would be allowed to touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidacy {
    /// Interpolation is permissible, but **only** over these scanline
    /// ranges. HUD bands are excluded (§3.5), so this is not simply the
    /// whole frame.
    Allowed { scanlines: Vec<(u16, u16)> },
    /// Refused, with the reason.
    Refused(Refusal),
}

impl Candidacy {
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Candidacy::Allowed { .. })
    }

    #[must_use]
    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            Candidacy::Refused(r) => Some(*r),
            Candidacy::Allowed { .. } => None,
        }
    }
}

/// May this frame pair be interpolated, and over what?
///
/// `hud` is the HUD bands detected for the frame (`crate::hud`), excluded
/// from the permitted region per §3.5: a static status bar over a moving
/// playfield shimmers if the whole frame is blended.
///
/// `height` is the visible scanline count — 224 or 239 on the SNES, and
/// passed in rather than assumed because W7-06 made that a runtime
/// property of `$2133`.
#[must_use]
pub fn candidacy(
    prev: FrameContext,
    next: FrameContext,
    hud: &[crate::hud::HudRegion],
    height: u16,
) -> Candidacy {
    if next.index != prev.index + 1 {
        return Candidacy::Refused(Refusal::NotConsecutive);
    }
    if prev.scene != next.scene {
        return Candidacy::Refused(Refusal::SceneCut);
    }
    if prev.palette_digest != next.palette_digest {
        return Candidacy::Refused(Refusal::PaletteChanged);
    }

    // Whatever is left after removing the HUD bands. Built by walking the
    // frame rather than subtracting rectangles, because HUD bands may be
    // at either edge and there may be more than one.
    //
    // Each pass skips a run of HUD lines and then takes a run of permitted
    // ones. The skip is what guarantees progress: without it, a `y` that
    // sits inside a band advances in neither loop, and the walk spins
    // forever pushing empty ranges (see docs/LESSONS.md RF-L-09).
    let is_hud = |y: u16| hud.iter().any(|h| h.contains(y));
    let mut scanlines = Vec::new();
    let mut y = 0u16;
    while y < height {
        while y < height && is_hud(y) {
            y += 1;
        }
        let start = y;
        while y < height && !is_hud(y) {
            y += 1;
        }
        if y > start {
            scanlines.push((start, y));
        }
    }
    Candidacy::Allowed { scanlines }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud::{HudRegion, HudSource};

    fn ctx(index: u64, scene: u64, palette: u64) -> FrameContext {
        FrameContext {
            index,
            scene: SceneId(scene),
            palette_digest: palette,
        }
    }

    fn hud_top(end: u16) -> HudRegion {
        HudRegion {
            start: 0,
            end,
            source: HudSource::Profile,
        }
    }

    /// The ordinary case: consecutive frames, same scene, same palette.
    /// The permitted region is the frame minus the HUD.
    #[test]
    fn a_steady_frame_pair_is_allowed_below_the_hud() {
        let c = candidacy(ctx(10, 1, 7), ctx(11, 1, 7), &[hud_top(32)], 224);
        assert_eq!(
            c,
            Candidacy::Allowed {
                scanlines: vec![(32, 224)]
            },
            "the HUD's own scanlines must not be in the permitted region"
        );
    }

    /// **§3.3.** A scene cut is refused, and says so.
    #[test]
    fn a_scene_cut_is_refused() {
        let c = candidacy(ctx(10, 1, 7), ctx(11, 2, 7), &[], 224);
        assert_eq!(c.refusal(), Some(Refusal::SceneCut));
        assert!(c.to_string_check());
    }

    /// **§3.4.** A palette change means the difference is colour, not
    /// motion.
    #[test]
    fn a_palette_change_is_refused() {
        let c = candidacy(ctx(10, 1, 7), ctx(11, 1, 8), &[], 224);
        assert_eq!(c.refusal(), Some(Refusal::PaletteChanged));
    }

    /// A gap in frame indices means there is nothing to interpolate
    /// between.
    #[test]
    fn a_non_consecutive_pair_is_refused() {
        assert_eq!(
            candidacy(ctx(10, 1, 7), ctx(12, 1, 7), &[], 224).refusal(),
            Some(Refusal::NotConsecutive)
        );
        // And the same frame twice is not a pair either.
        assert_eq!(
            candidacy(ctx(10, 1, 7), ctx(10, 1, 7), &[], 224).refusal(),
            Some(Refusal::NotConsecutive)
        );
    }

    /// Two HUD bands (top status bar and bottom message box) split the
    /// permitted region into two, which is why this walks the frame rather
    /// than subtracting one rectangle.
    #[test]
    fn hud_bands_at_both_edges_split_the_permitted_region() {
        let hud = [
            hud_top(16),
            HudRegion {
                start: 200,
                end: 224,
                source: HudSource::Heuristic,
            },
        ];
        let c = candidacy(ctx(1, 1, 1), ctx(2, 1, 1), &hud, 224);
        assert_eq!(
            c,
            Candidacy::Allowed {
                scanlines: vec![(16, 200)]
            }
        );
    }

    /// A frame that is entirely HUD leaves nothing to interpolate. This is
    /// the case that hung the walk before RF-L-09: `y` starts inside a
    /// band, so the version that did not skip HUD lines first never
    /// advanced and pushed `(0, 0)` until the machine ran out of memory.
    #[test]
    fn an_all_hud_frame_permits_nothing_and_terminates() {
        let c = candidacy(ctx(1, 1, 1), ctx(2, 1, 1), &[hud_top(224)], 224);
        assert_eq!(
            c,
            Candidacy::Allowed { scanlines: vec![] },
            "no scanline is permitted, and no empty range is emitted"
        );
    }

    /// Overscan is a runtime property (`$2133`), so the height is a
    /// parameter — a hard-coded 224 would silently drop 15 lines of an
    /// overscan game.
    #[test]
    fn the_visible_height_is_respected() {
        let c = candidacy(ctx(1, 1, 1), ctx(2, 1, 1), &[], 239);
        assert_eq!(
            c,
            Candidacy::Allowed {
                scanlines: vec![(0, 239)]
            }
        );
    }

    impl Candidacy {
        /// Every refusal must have a non-empty explanation; a reason a UI
        /// cannot show is not a reason.
        fn to_string_check(&self) -> bool {
            self.refusal().is_some_and(|r| !r.to_string().is_empty())
        }
    }
}
