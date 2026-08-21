//! HUD separation (ticket W8-06; FR-ENH-007, FR-ENH-011, FR-ENH-012).
//!
//! ## What a HUD is, in terms this crate can actually observe
//!
//! A split-screen HUD is not a picture, it is a **scroll discontinuity**.
//! The game writes one scroll value for the status bar and a different
//! one for the playfield, part-way down the frame — classically on a
//! sprite-0 hit, on the SNES usually from HDMA. So the HUD is the band of
//! scanlines whose scroll stays put while the rest of the frame moves.
//!
//! That is why criterion 2 forbids pixel guessing and names
//! `ScrollWrite` + HDMA: those events *are* the phenomenon, where pixels
//! are only a consequence of it. [`crate::scroll_tracker::compute_bands`]
//! already turns those events into bands and leaves
//! [`ScanlineBand::is_hud`] permanently `false`; this module is what fills
//! that flag in.
//!
//! ## Two sources, and the profile always wins
//!
//! FR-ENH-007: "HUD separation shall activate only from profile rules or a
//! verified split heuristic". A profile's `[camera.hud]` is a human
//! statement about a specific game and beats any amount of inference, so
//! when one is present the heuristic still *runs* — and is compared
//! against it. A disagreement is a [`Contradiction`], because a heuristic
//! that quietly disagrees with the profile on this game will quietly be
//! wrong on the next one where nobody wrote a profile at all.
//!
//! ## Criterion 3 is the trust ladder, not a new mechanism
//!
//! "A false positive is visible in the ledger rather than silently
//! altering the picture" is exactly what D-004's ladder already gives:
//! [`TrustState::Shadow`] is the default everywhere, and it means detect
//! and record, never act. So a fresh install cannot have a HUD false
//! positive alter anything — the verdict is written to the report card and
//! the picture is untouched. [`Verdict::acted`] reports which happened,
//! rather than a caller having to infer it.

use rf_core_api::CoreEvent;

use crate::scroll_tracker::ScanlineBand;
use crate::trust::TrustLadder;

/// The heuristic's name in the trust ladder and the report card.
pub const HEURISTIC: &str = "hud_separation";

/// Where a HUD region came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudSource {
    /// A profile's `[camera.hud]` said so.
    Profile,
    /// Inferred from scroll discontinuity.
    Heuristic,
}

/// A band of scanlines identified as HUD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HudRegion {
    /// First scanline, inclusive.
    pub start: u16,
    /// Last scanline, exclusive.
    pub end: u16,
    pub source: HudSource,
}

impl HudRegion {
    #[must_use]
    pub fn contains(&self, y: u16) -> bool {
        y >= self.start && y < self.end
    }
}

/// A profile's declaration could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HudError {
    /// `scanlines` was not the `[start, end]` pair the schema documents.
    MalformedScanlines(usize),
    /// `region` was not one this build knows.
    UnknownRegion(String),
}

impl std::fmt::Display for HudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HudError::MalformedScanlines(n) => write!(
                f,
                "[camera.hud].scanlines needs exactly two entries (start, end), found {n}"
            ),
            HudError::UnknownRegion(r) => {
                write!(f, "`{r}` is not a HUD region (top, bottom)")
            }
        }
    }
}

impl std::error::Error for HudError {}

/// Read a profile's `[camera.hud]` declaration.
///
/// # Errors
/// [`HudError`] when the declaration is malformed — refused rather than
/// ignored, because a profile that meant to pin a HUD and silently did not
/// is worse than one that never tried.
pub fn from_profile(spec: &rf_profiles::schema::HudSpec) -> Result<HudRegion, HudError> {
    if spec.scanlines.len() != 2 {
        return Err(HudError::MalformedScanlines(spec.scanlines.len()));
    }
    match spec.region.as_str() {
        "top" | "bottom" => {}
        other => return Err(HudError::UnknownRegion(other.to_string())),
    }
    Ok(HudRegion {
        start: spec.scanlines[0] as u16,
        end: spec.scanlines[1] as u16,
        source: HudSource::Profile,
    })
}

/// What one frame's observation concluded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Verdict {
    /// The regions believed to be HUD this frame.
    pub regions: Vec<HudRegion>,
    /// **Whether the picture may actually be changed.** False under
    /// `Shadow` and `Advisory`, which is what keeps a false positive in
    /// the ledger instead of on the screen.
    pub acted: bool,
}

/// Cross-frame HUD detection.
///
/// Stateful because the heuristic is inherently comparative: a band that
/// does not scroll is only interesting when another band does, and a band
/// that never moves across several frames is only a HUD if the playfield
/// was moving during them. One frame cannot tell a HUD from a game that is
/// simply standing still.
#[derive(Debug, Clone, Default)]
pub struct HudSeparator {
    declared: Option<HudRegion>,
    /// Consecutive frames the same candidate has held.
    stable: u32,
    candidate: Option<HudRegion>,
}

/// Frames a candidate must survive before the heuristic will name it.
///
/// Three rather than one because a single frame of "this band did not
/// scroll" is the ordinary case for any game that paused, and naming a HUD
/// on it would mean every pause produced one.
pub const STABLE_FRAMES: u32 = 3;

impl HudSeparator {
    #[must_use]
    pub fn new(declared: Option<HudRegion>) -> Self {
        Self {
            declared,
            stable: 0,
            candidate: None,
        }
    }

    /// True when a profile declared the HUD, so the heuristic is only ever
    /// a cross-check here.
    #[must_use]
    pub fn is_declared(&self) -> bool {
        self.declared.is_some()
    }

    /// Observe one frame.
    ///
    /// `scene` is the trust ladder's suppression context. `ladder` is taken
    /// mutably because a contradiction is *recorded*, not returned and
    /// forgotten — FR-ENH-012's report card is the point.
    pub fn observe(
        &mut self,
        bands: &[ScanlineBand],
        events: &[CoreEvent],
        ladder: &mut TrustLadder,
        scene: &str,
    ) -> Verdict {
        let found = detect(bands, events);

        // Stability: the same candidate must hold for several frames.
        match (found, self.candidate) {
            (Some(f), Some(c)) if f.start == c.start && f.end == c.end => self.stable += 1,
            (Some(f), _) => {
                self.candidate = Some(f);
                self.stable = 1;
            }
            (None, _) => {
                self.candidate = None;
                self.stable = 0;
            }
        }
        let inferred = (self.stable >= STABLE_FRAMES)
            .then_some(self.candidate)
            .flatten();

        // A profile declaration wins outright, and a disagreement with it
        // is recorded: the heuristic being wrong HERE, where someone
        // checked, predicts it being wrong where nobody did.
        if let Some(declared) = self.declared {
            if let Some(inf) = inferred {
                if inf.start != declared.start || inf.end != declared.end {
                    ladder.record_contradiction(
                        HEURISTIC,
                        &format!(
                            "heuristic found a HUD at {}..{} but the profile declares {}..{}; \
                             the profile is being used",
                            inf.start, inf.end, declared.start, declared.end
                        ),
                        scene,
                    );
                }
            }
            return Verdict {
                regions: vec![declared],
                // A declaration is a human statement, not a heuristic, so
                // FR-ENH-007 lets it act without the ladder's permission.
                acted: true,
            };
        }

        let Some(region) = inferred else {
            return Verdict::default();
        };
        // No profile: now the ladder decides. Shadow records and does not
        // act, which is criterion 3.
        let acted = ladder.should_act(HEURISTIC, scene);
        if !acted {
            ladder.record_contradiction(
                HEURISTIC,
                &format!(
                    "detected a HUD at {}..{} but did not act on it (trust state is {})",
                    region.start,
                    region.end,
                    ladder.state(HEURISTIC).name()
                ),
                scene,
            );
        }
        Verdict {
            regions: vec![region],
            acted,
        }
    }
}

/// The single-frame heuristic: find a band at the top or bottom of the
/// frame whose scroll differs from the frame's dominant scroll.
///
/// **Edges only, deliberately.** A scroll split in the middle of the
/// screen is a parallax layer or a water effect, not a status bar, and
/// treating one as a HUD would pin the wrong half of the picture. HUDs sit
/// at an edge because that is where a status bar goes.
///
/// Returns `None` when there is no split at all — one band means one
/// scroll value for the whole frame, which is a game without a HUD or a
/// game whose HUD is drawn some other way.
#[must_use]
pub fn detect(bands: &[ScanlineBand], events: &[CoreEvent]) -> Option<HudRegion> {
    if bands.len() < 2 {
        return None;
    }
    // Criterion 2: the inputs are the events, not the pixels. A frame with
    // no scroll write and no DMA cannot have a split, whatever its bands
    // look like.
    let split_evidence = events.iter().any(|e| {
        matches!(
            e,
            CoreEvent::ScrollWrite { .. } | CoreEvent::DmaStart { .. }
        )
    });
    if !split_evidence {
        return None;
    }

    let first = bands.first()?;
    let last = bands.last()?;
    // The playfield is the widest band; the HUD is the edge one that
    // disagrees with it.
    let playfield = bands.iter().max_by_key(|b| b.end.saturating_sub(b.start))?;

    // Only the two EDGE bands are ever candidates; that restriction is
    // what rejects a mid-screen split, and it is load-bearing rather than
    // tidy. (No guard against `candidate == playfield` is needed: a band
    // cannot have a scroll different from its own.)
    for candidate in [first, last] {
        if candidate.base_scroll != playfield.base_scroll {
            return Some(HudRegion {
                start: candidate.start,
                end: candidate.end,
                source: HudSource::Heuristic,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::TrustState;
    use rf_core_api::PixelLayer;

    fn band(start: u16, end: u16, scroll: (u16, u16)) -> ScanlineBand {
        ScanlineBand {
            start,
            end,
            base_scroll: scroll,
            is_hud: false,
        }
    }

    /// A classic split: a static 32-line status bar at the top, a
    /// scrolling playfield below.
    fn split_frame() -> (Vec<ScanlineBand>, Vec<CoreEvent>) {
        (
            vec![band(0, 32, (0, 0)), band(32, 224, (120, 0))],
            vec![CoreEvent::ScrollWrite {
                x: 120,
                y: 0,
                layer: PixelLayer::Background(0),
            }],
        )
    }

    /// **Criterion 2**: the events are the input. Identical bands with no
    /// scroll or DMA event produce no verdict, because without one there
    /// was no split to see.
    #[test]
    fn without_a_scroll_or_dma_event_there_is_no_hud() {
        let (bands, _) = split_frame();
        assert!(
            detect(&bands, &[]).is_none(),
            "bands alone must not be enough - that is pixel-shaped reasoning \
             wearing a different hat"
        );
        assert!(detect(&bands, &[CoreEvent::FrameStart]).is_none());
        // The same bands WITH the event do produce one.
        let (bands, events) = split_frame();
        assert!(detect(&bands, &events).is_some());
    }

    /// A frame with one scroll value has no split, however many events it
    /// fired.
    #[test]
    fn a_frame_without_a_split_has_no_hud() {
        let bands = vec![band(0, 224, (40, 0))];
        let events = vec![CoreEvent::ScrollWrite {
            x: 40,
            y: 0,
            layer: PixelLayer::Background(0),
        }];
        assert!(detect(&bands, &events).is_none());
    }

    /// A split in the MIDDLE is parallax or a water effect, not a status
    /// bar — pinning it would freeze the wrong half of the picture.
    #[test]
    fn a_mid_screen_split_is_not_treated_as_a_hud() {
        let bands = vec![
            band(0, 100, (60, 0)),
            band(100, 130, (10, 0)),
            band(130, 224, (60, 0)),
        ];
        let events = vec![CoreEvent::ScrollWrite {
            x: 10,
            y: 0,
            layer: PixelLayer::Background(0),
        }];
        assert!(
            detect(&bands, &events).is_none(),
            "only an edge band can be a HUD"
        );
    }

    /// **Criterion 3.** A fresh install is all-shadow, so a detection is
    /// recorded and the picture is left alone.
    #[test]
    fn under_shadow_a_detection_is_ledgered_and_never_acted_on() {
        let mut ladder = TrustLadder::new();
        let mut sep = HudSeparator::new(None);
        let (bands, events) = split_frame();

        let mut verdict = Verdict::default();
        for _ in 0..STABLE_FRAMES {
            verdict = sep.observe(&bands, &events, &mut ladder, "scene-1");
        }
        assert_eq!(verdict.regions.len(), 1, "the HUD was detected");
        assert!(
            !verdict.acted,
            "shadow is the default everywhere: detect and record, never act"
        );
        let card = ladder.report_card();
        assert!(
            card.iter()
                .any(|c| c.heuristic == HEURISTIC && c.detail.contains("did not act")),
            "the false-positive-safe path must leave a trace: {card:?}"
        );
    }

    /// Promoted to Active, the same detection acts.
    #[test]
    fn an_active_heuristic_acts_on_the_same_detection() {
        let mut ladder = TrustLadder::new();
        ladder.set_state(HEURISTIC, TrustState::Active);
        let mut sep = HudSeparator::new(None);
        let (bands, events) = split_frame();

        let mut verdict = Verdict::default();
        for _ in 0..STABLE_FRAMES {
            verdict = sep.observe(&bands, &events, &mut ladder, "scene-1");
        }
        assert!(verdict.acted);
        assert_eq!(verdict.regions[0].source, HudSource::Heuristic);
    }

    /// One frame is never enough: a game that paused would otherwise grow
    /// a HUD.
    #[test]
    fn a_single_frame_does_not_name_a_hud() {
        let mut ladder = TrustLadder::new();
        ladder.set_state(HEURISTIC, TrustState::Active);
        let mut sep = HudSeparator::new(None);
        let (bands, events) = split_frame();

        let first = sep.observe(&bands, &events, &mut ladder, "s");
        assert!(
            first.regions.is_empty(),
            "one frame of 'this band did not scroll' is just a paused game"
        );
    }

    /// **The profile wins, and the disagreement is recorded.**
    #[test]
    fn a_profile_declaration_overrides_the_heuristic_and_logs_the_difference() {
        let declared = HudRegion {
            start: 0,
            end: 16, // deliberately NOT the 0..32 the heuristic will find
            source: HudSource::Profile,
        };
        let mut ladder = TrustLadder::new();
        let mut sep = HudSeparator::new(Some(declared));
        let (bands, events) = split_frame();

        let mut verdict = Verdict::default();
        for _ in 0..STABLE_FRAMES {
            verdict = sep.observe(&bands, &events, &mut ladder, "scene-1");
        }
        assert_eq!(
            verdict.regions,
            vec![declared],
            "the profile's region is used"
        );
        assert!(
            verdict.acted,
            "a declaration is a human statement, not a heuristic, so it does not \
             wait on the trust ladder"
        );
        let card = ladder.report_card();
        assert!(
            card.iter()
                .any(|c| c.detail.contains("but the profile declares")),
            "a heuristic disagreeing with a profile predicts it being wrong where \
             nobody wrote one: {card:?}"
        );
    }

    /// A malformed declaration is refused, not ignored.
    #[test]
    fn a_malformed_profile_declaration_is_refused() {
        use rf_profiles::schema::HudSpec;
        let bad = HudSpec {
            region: "top".to_string(),
            scanlines: vec![0],
            detect: "sprite0_split".to_string(),
        };
        assert_eq!(from_profile(&bad), Err(HudError::MalformedScanlines(1)));

        let sideways = HudSpec {
            region: "sideways".to_string(),
            scanlines: vec![0, 32],
            detect: "sprite0_split".to_string(),
        };
        assert!(matches!(
            from_profile(&sideways),
            Err(HudError::UnknownRegion(_))
        ));

        let good = HudSpec {
            region: "top".to_string(),
            scanlines: vec![0, 32],
            detect: "sprite0_split".to_string(),
        };
        let region = from_profile(&good).expect("a well-formed declaration");
        assert_eq!((region.start, region.end), (0, 32));
        assert_eq!(region.source, HudSource::Profile);
        assert!(region.contains(0) && region.contains(31) && !region.contains(32));
    }
}
