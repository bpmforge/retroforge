//! The Enhance workspace's pure half (ticket W4-05; FR-MODE-001/003,
//! FR-ENH-010/012, FR-PROF-005, FRONTEND_UI.md §1/§3.3, GAME_PROFILES.md
//! §4).
//!
//! Same split this shell uses everywhere the UI has judgement in it
//! (`enhanced_view`, `script_panel`, `rf_renderer::compare`): the
//! branching is what has bugs, so the branching lives here and is tested
//! headlessly, and the egui layer only paints what these functions
//! return.
//!
//! ## The honesty contract is the design constraint
//!
//! FRONTEND_UI.md §3.3: *"Feature rows are generated from capability
//! flags … the UI cannot offer what the honesty contract (ARCHITECTURE
//! §2) says is unavailable."* So [`feature_rows`] returns a row's
//! availability **and the reason** rather than a bool, and an unavailable
//! row is rendered disabled-with-explanation rather than hidden. Hiding
//! it would be the same lie by omission the badge exists to prevent: a
//! user who cannot see that full-level view *needs a profile* concludes
//! the emulator does not have the feature.

use rf_enhance::trust::{TrustLadder, TrustState};

use crate::game_settings::{GameSettings, Mode};

/// Why a feature row is or is not offerable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// Offerable in principle, but not in this mode.
    NeedsMode(&'static str),
    /// Gated behind a matched profile (`GAME_PROFILES.md` §4).
    NeedsProfile,
}

impl Availability {
    #[must_use]
    pub fn explanation(&self) -> Option<String> {
        match self {
            Availability::Available => None,
            Availability::NeedsMode(mode) => Some(format!("requires {mode} mode")),
            Availability::NeedsProfile => {
                Some("requires a matched game profile (none loaded)".to_string())
            }
        }
    }
}

/// One row of FRONTEND_UI.md §3.3's Features tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureRow {
    pub id: &'static str,
    pub label: &'static str,
    /// "(generic)" / "(requires profile)" — §3.3's scope column.
    pub scope: &'static str,
    pub enabled: bool,
    pub availability: Availability,
    /// The heuristic this feature is governed by, if any — its trust
    /// ladder chip (D-004) is shown on the row.
    pub heuristic: Option<&'static str>,
}

impl FeatureRow {
    /// A row is only *effectively* on when it is both enabled and
    /// available. Asked once here so no caller can render a checked box
    /// for something that cannot run.
    #[must_use]
    pub fn effective(&self) -> bool {
        self.enabled && self.availability == Availability::Available
    }
}

/// Build the Features tab from mode + profile presence + saved settings.
///
/// `profile_matched` is passed in rather than looked up, because it is
/// the shell's knowledge and this function must stay testable without a
/// profile loader. `diorama_available` is a THIRD, narrower fact than
/// `profile_matched`: Game-Aware plus a matching profile is necessary for
/// Diorama but not sufficient — the profile also has to have produced a
/// [`rf_enhance::scene_graph::SceneLayer::Geometry`] (i.e. its decode
/// declared a collision table, ticket W16-06/`docs/design/
/// ENHANCEMENT_WAVE_16.md` §5) — so a matched profile with no collision
/// data must still show the row as `NeedsProfile`, not `Available`.
#[must_use]
pub fn feature_rows(
    settings: &GameSettings,
    profile_matched: bool,
    diorama_available: bool,
) -> Vec<FeatureRow> {
    let mode = settings.mode;
    let enhancement = mode.enhancement_active();
    let profile_gated = mode.profile_gated_features_unlocked(profile_matched);
    let diorama_gated = mode.profile_gated_features_unlocked(diorama_available);

    // Generic features need enhancement; profile features need
    // Game-Aware AND a match. Both reasons are distinguishable, because
    // "turn on Enhanced" and "load a profile" are different actions and a
    // user told only "unavailable" cannot tell which they need.
    let generic = |enabled: bool| {
        if enhancement {
            (Availability::Available, enabled)
        } else {
            (Availability::NeedsMode("Enhanced"), enabled)
        }
    };
    let profiled = |enabled: bool| {
        if profile_gated {
            (Availability::Available, enabled)
        } else if !matches!(mode, Mode::GameAware) {
            (Availability::NeedsMode("Game-Aware"), enabled)
        } else {
            (Availability::NeedsProfile, enabled)
        }
    };

    let diorama_profiled = |enabled: bool| {
        if diorama_gated {
            (Availability::Available, enabled)
        } else if !matches!(mode, Mode::GameAware) {
            (Availability::NeedsMode("Game-Aware"), enabled)
        } else {
            (Availability::NeedsProfile, enabled)
        }
    };

    let (sprite_av, sprite_on) = generic(settings.sprite_overlay);
    let (flicker_av, flicker_on) = generic(settings.deflicker);
    let (wide_av, wide_on) = profiled(settings.widescreen_decoded);
    let (level_av, level_on) = profiled(settings.full_level_view);
    let (diorama_av, diorama_on) = diorama_profiled(settings.diorama);
    // Ticket W16-04: unlike the toggles above, this row's "on" state IS
    // the trust ladder's own rung (D-004/ENHANCEMENT_RUNTIME.md §2a) — the
    // fog pass renders exactly when the heuristic is `Active`
    // (`TrustLadder::should_act`'s own "the one question every call site
    // must ask"), not a second, independent settings bool that could
    // drift from it. `Advisory` shows as OFF here (not `Available`'s
    // effective-count) because a badge is a suggestion, never an action —
    // the same distinction `TrustLadder::should_act`'s own doc calls out
    // as "most likely to be got wrong later".
    let atmosphere_active =
        settings.trust.state(rf_enhance::atmosphere::HEURISTIC_ID) == TrustState::Active;
    let (fog_av, fog_on) = generic(atmosphere_active);

    vec![
        FeatureRow {
            id: "sprite_overlay",
            label: "Sprite-limit bypass",
            scope: "generic",
            enabled: sprite_on,
            availability: sprite_av,
            heuristic: Some("anti-flicker"),
        },
        FeatureRow {
            id: "deflicker",
            label: "De-flicker: temporal",
            scope: "generic, tuned by profile",
            enabled: flicker_on,
            availability: flicker_av,
            heuristic: Some("anti-flicker"),
        },
        FeatureRow {
            id: "widescreen_decoded",
            label: "Widescreen: decoded",
            scope: "requires profile",
            enabled: wide_on,
            availability: wide_av,
            heuristic: None,
        },
        FeatureRow {
            id: "full_level_view",
            label: "Full-level view",
            scope: "requires profile",
            enabled: level_on,
            availability: level_av,
            heuristic: None,
        },
        FeatureRow {
            id: "atmosphere_fog",
            label: "Atmosphere: fog",
            scope: "generic, heuristic-gated",
            enabled: fog_on,
            availability: fog_av,
            heuristic: Some(rf_enhance::atmosphere::HEURISTIC_ID),
        },
        FeatureRow {
            id: "diorama",
            label: "Diorama: walls",
            scope: "requires profile with collision",
            enabled: diorama_on,
            availability: diorama_av,
            heuristic: None,
        },
    ]
}

/// The status-bar badge (FRONTEND_UI.md §1/§80: `NES · Accuracy` or
/// `NES · Enhanced ⚡(3)`).
///
/// The count is of **effective** features, not enabled ones: a badge
/// claiming three active enhancements while two of them cannot run is
/// precisely the dishonesty this badge exists to prevent.
#[must_use]
pub fn badge_text(
    console: &str,
    settings: &GameSettings,
    profile_matched: bool,
    diorama_available: bool,
) -> String {
    let mode = settings.mode;
    let active = feature_rows(settings, profile_matched, diorama_available)
        .iter()
        .filter(|r| r.effective())
        .count();
    if mode.enhancement_active() && active > 0 {
        format!("{console} · {} \u{26a1}({active})", mode.display_name())
    } else {
        format!("{console} · {}", mode.display_name())
    }
}

/// The badge's hover breakdown (FRONTEND_UI.md §1: "a hover breakdown of
/// active features").
///
/// Lists what is actually on; when nothing is, says so rather than
/// returning an empty tooltip a user reads as a broken control.
#[must_use]
pub fn badge_breakdown(
    settings: &GameSettings,
    profile_matched: bool,
    diorama_available: bool,
) -> Vec<String> {
    let rows = feature_rows(settings, profile_matched, diorama_available);
    let mut out = vec![format!("Mode: {}", settings.mode.display_name())];
    let active: Vec<&FeatureRow> = rows.iter().filter(|r| r.effective()).collect();
    if active.is_empty() {
        out.push("No enhancements active — this is the unmodified emulation.".to_string());
    } else {
        for row in active {
            out.push(format!("• {} ({})", row.label, row.scope));
        }
    }
    // Hold-to-peek is advertised here because the badge is where a user
    // looks when they want to know what they are seeing.
    out.push("Hold to peek at the original.".to_string());
    out
}

/// A per-heuristic ladder chip for the Features tab (§3.3's "ladder chip
/// per heuristic: shadow/advisory/active").
#[must_use]
pub fn ladder_chip(ladder: &TrustLadder, heuristic: &str) -> String {
    let state = ladder.state(heuristic);
    let label = match state {
        TrustState::Shadow => "shadow",
        TrustState::Advisory => "advisory",
        TrustState::Active => "active",
    };
    // The suppression is part of the chip, not a separate surface: a
    // heuristic that is nominally "active" but suppressed here is not
    // acting, and a chip that said only "active" would be wrong.
    match ladder.suppression(heuristic) {
        Some(s) => format!("{label} (suppressed in {})", s.granted_in_scene),
        None => label.to_string(),
    }
}

/// The profile inspector (FR-PROF-005, GAME_PROFILES.md §4: "Match at
/// cart load by normalized hash → load base profile → apply user
/// overrides → apply per-session toggles. All layers visible in the
/// profile inspector panel.").
///
/// `layers` is the precedence chain, outermost-last, exactly as §4 orders
/// it — the panel's job is to make precedence *visible*, so the order is
/// the content.
#[must_use]
pub fn profile_inspector_lines(
    matched: Option<&str>,
    capabilities: &[(&str, bool)],
    layers: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    match matched {
        Some(title) => out.push(format!("Identity: MATCHED — {title}")),
        // "No profile" is a status, not an absence: an empty panel reads
        // as a broken inspector.
        None => out.push("Identity: no profile matched this ROM's normalized hash".to_string()),
    }
    out.push("Capabilities:".to_string());
    if capabilities.is_empty() {
        out.push("  (none declared)".to_string());
    }
    for (name, present) in capabilities {
        out.push(format!("  [{}] {name}", if *present { "x" } else { " " }));
    }
    out.push("Precedence (later overrides earlier):".to_string());
    for (i, layer) in layers.iter().enumerate() {
        out.push(format!("  {}. {layer}", i + 1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // `GameSettings::unknown` is private to `crate::game_settings`, so
    // `..Default::default()` is not reachable from this module — the
    // reassignment below is the only way to build a fixture here.
    #[allow(clippy::field_reassign_with_default)]
    fn settings(mode: Mode) -> GameSettings {
        let mut s = GameSettings::default();
        s.mode = mode;
        s.sprite_overlay = true;
        s.deflicker = true;
        s.widescreen_decoded = true;
        s.full_level_view = true;
        s
    }

    /// FR-MODE-003, the load-bearing one: a fresh install is Accuracy and
    /// NOTHING is effectively on — even if a settings file has every
    /// feature flag set, because Accuracy does not subscribe the
    /// enhancement runtime at all.
    #[test]
    fn enhancement_is_never_on_silently() {
        let fresh = GameSettings::default();
        assert_eq!(fresh.mode, Mode::Accuracy);
        assert!(feature_rows(&fresh, true, false)
            .iter()
            .all(|r| !r.effective()));

        // Even with every flag on, Accuracy keeps them off.
        let all_flags_on = settings(Mode::Accuracy);
        assert!(
            feature_rows(&all_flags_on, true, false)
                .iter()
                .all(|r| !r.effective()),
            "Accuracy must not run enhancements no matter what the file says"
        );
        assert_eq!(
            badge_text("NES", &all_flags_on, true, false),
            "NES · Accuracy"
        );
    }

    /// Research/Debug says "enhancement optional" — which means not on by
    /// itself. A debugging mode that quietly changed the picture would
    /// defeat its own purpose.
    #[test]
    fn research_debug_does_not_turn_enhancement_on_by_itself() {
        let s = settings(Mode::ResearchDebug);
        assert!(feature_rows(&s, true, false).iter().all(|r| !r.effective()));
        assert_eq!(badge_text("NES", &s, true, false), "NES · Research/Debug");
    }

    /// The badge counts EFFECTIVE features. Enhanced without a profile
    /// runs the two generic ones and cannot run the two profile ones.
    #[test]
    fn the_badge_counts_only_what_can_actually_run() {
        let s = settings(Mode::Enhanced);
        assert_eq!(
            badge_text("NES", &s, false, false),
            "NES · Enhanced \u{26a1}(2)",
            "the two generic features run; the two profile ones cannot"
        );
        // Game-Aware WITH a profile runs all four.
        let ga = settings(Mode::GameAware);
        assert_eq!(
            badge_text("NES", &ga, true, false),
            "NES · Game-Aware \u{26a1}(4)"
        );
        // Game-Aware WITHOUT a profile falls back to the generic two.
        assert_eq!(
            badge_text("NES", &ga, false, false),
            "NES · Game-Aware \u{26a1}(2)"
        );
    }

    /// The two unavailable reasons must be DISTINGUISHABLE: "turn on
    /// Enhanced" and "load a profile" are different actions, and a user
    /// told only "unavailable" cannot tell which they need.
    #[test]
    fn an_unavailable_row_says_which_thing_is_missing() {
        let accuracy = feature_rows(&settings(Mode::Accuracy), true, false);
        let sprite = accuracy.iter().find(|r| r.id == "sprite_overlay").unwrap();
        assert_eq!(sprite.availability, Availability::NeedsMode("Enhanced"));
        assert_eq!(
            sprite.availability.explanation().unwrap(),
            "requires Enhanced mode"
        );

        let game_aware_no_profile = feature_rows(&settings(Mode::GameAware), false, false);
        let level = game_aware_no_profile
            .iter()
            .find(|r| r.id == "full_level_view")
            .unwrap();
        assert_eq!(level.availability, Availability::NeedsProfile);
        assert!(level
            .availability
            .explanation()
            .unwrap()
            .contains("profile"));

        // And in Enhanced, a profile feature says to change MODE, not to
        // load a profile — the user's next action differs.
        let enhanced = feature_rows(&settings(Mode::Enhanced), true, false);
        let level = enhanced.iter().find(|r| r.id == "full_level_view").unwrap();
        assert_eq!(level.availability, Availability::NeedsMode("Game-Aware"));
    }

    /// Unavailable rows are RETURNED, not hidden — the honesty contract's
    /// "rows disabled+explained when no profile capability".
    #[test]
    fn unavailable_rows_are_still_listed() {
        let rows = feature_rows(&GameSettings::default(), false, false);
        assert_eq!(rows.len(), 6, "every feature is listed in every mode");
        assert!(rows.iter().all(|r| r.availability.explanation().is_some()));
    }

    #[test]
    fn the_breakdown_says_so_when_nothing_is_active() {
        let lines = badge_breakdown(&GameSettings::default(), false, false);
        assert!(lines.iter().any(|l| l.contains("No enhancements active")));
        assert!(lines.iter().any(|l| l.contains("Hold to peek")));

        let active = badge_breakdown(&settings(Mode::Enhanced), false, false);
        assert!(active.iter().any(|l| l.contains("Sprite-limit bypass")));
        assert!(
            !active.iter().any(|l| l.contains("Full-level view")),
            "the breakdown lists what is ON, not what exists"
        );
    }

    /// Ticket W16-04, acceptance criterion 3: the badge names the effect
    /// specifically ("Atmosphere: fog") when the heuristic is `Active`,
    /// and `Advisory`/`Shadow` must NOT count it as effective — the same
    /// "advisory does not act" law `TrustLadder::should_act`'s own tests
    /// assert, checked here at the UI layer too.
    #[test]
    fn the_badge_names_atmosphere_fog_only_when_the_ladder_is_active() {
        let mut s = settings(Mode::Enhanced);
        s.sprite_overlay = false;
        s.deflicker = false;
        s.widescreen_decoded = false;
        s.full_level_view = false;

        // Shadow (default): not effective, not named.
        assert!(!feature_rows(&s, false, false)
            .iter()
            .find(|r| r.id == "atmosphere_fog")
            .unwrap()
            .effective());
        assert!(!badge_breakdown(&s, false, false)
            .iter()
            .any(|l| l.contains("Atmosphere: fog")));

        // Advisory: still not effective, still not named -- a suggestion
        // is not an action.
        s.trust
            .set_state(rf_enhance::atmosphere::HEURISTIC_ID, TrustState::Advisory);
        assert!(!feature_rows(&s, false, false)
            .iter()
            .find(|r| r.id == "atmosphere_fog")
            .unwrap()
            .effective());
        assert!(!badge_breakdown(&s, false, false)
            .iter()
            .any(|l| l.contains("Atmosphere: fog")));

        // Active: effective, and the breakdown names it specifically.
        s.trust
            .set_state(rf_enhance::atmosphere::HEURISTIC_ID, TrustState::Active);
        assert!(feature_rows(&s, false, false)
            .iter()
            .find(|r| r.id == "atmosphere_fog")
            .unwrap()
            .effective());
        assert!(badge_breakdown(&s, false, false)
            .iter()
            .any(|l| l.contains("Atmosphere: fog")));
    }

    /// Ticket W16-06: Diorama is gated on a THIRD fact narrower than
    /// `profile_matched` — a matched Game-Aware profile with no collision
    /// data must still read `NeedsProfile`, and only `diorama_available`
    /// (Game-Aware + collision) makes it `Available` and names it on the
    /// badge.
    #[test]
    fn diorama_needs_game_aware_and_a_profile_with_collision_specifically() {
        let mut s = settings(Mode::GameAware);
        s.sprite_overlay = false;
        s.deflicker = false;
        s.widescreen_decoded = false;
        s.full_level_view = false;
        s.diorama = true;

        // Matched profile, but no collision -> NeedsProfile, not Available.
        let rows = feature_rows(&s, true, false);
        let diorama = rows.iter().find(|r| r.id == "diorama").unwrap();
        assert_eq!(diorama.availability, Availability::NeedsProfile);
        assert!(!diorama.effective());
        assert!(!badge_breakdown(&s, true, false)
            .iter()
            .any(|l| l.contains("Diorama: walls")));

        // Matched profile WITH collision -> Available, named on the badge.
        let rows = feature_rows(&s, true, true);
        let diorama = rows.iter().find(|r| r.id == "diorama").unwrap();
        assert_eq!(diorama.availability, Availability::Available);
        assert!(diorama.effective());
        assert!(badge_breakdown(&s, true, true)
            .iter()
            .any(|l| l.contains("Diorama: walls")));

        // Enhanced mode (not Game-Aware) says which thing is missing is
        // the MODE, not the profile, even with diorama_available true —
        // Enhanced simply cannot unlock a profile-gated feature.
        let enhanced = settings(Mode::Enhanced);
        let rows = feature_rows(&enhanced, true, true);
        let diorama = rows.iter().find(|r| r.id == "diorama").unwrap();
        assert_eq!(diorama.availability, Availability::NeedsMode("Game-Aware"));
    }

    /// A "suppressed" heuristic is not acting, so the chip must say so —
    /// a chip reading only "active" would be wrong.
    #[test]
    fn a_ladder_chip_shows_suppression_not_just_the_rung() {
        let mut ladder = TrustLadder::new();
        assert_eq!(ladder_chip(&ladder, "anti-flicker"), "shadow");
        ladder.set_state("anti-flicker", TrustState::Active);
        assert_eq!(ladder_chip(&ladder, "anti-flicker"), "active");
        ladder
            .suppress("anti-flicker", "deliberate strobe", "scene-a")
            .unwrap();
        assert_eq!(
            ladder_chip(&ladder, "anti-flicker"),
            "active (suppressed in scene-a)"
        );
    }

    /// GAME_PROFILES.md §4: "All layers visible in the profile inspector
    /// panel" — precedence is the content, so the ORDER is asserted.
    #[test]
    fn the_inspector_shows_identity_capabilities_and_precedence_order() {
        let lines = profile_inspector_lines(
            Some("RF-Scroller"),
            &[("full_level_view", true), ("hud_split", false)],
            &[
                "base profile".to_string(),
                "user overrides (profiles.d)".to_string(),
                "per-session toggles".to_string(),
            ],
        );
        let joined = lines.join("\n");
        assert!(joined.contains("MATCHED — RF-Scroller"));
        assert!(joined.contains("[x] full_level_view"));
        assert!(
            joined.contains("[ ] hud_split"),
            "unmet caps show unchecked"
        );
        let base = lines
            .iter()
            .position(|l| l.contains("base profile"))
            .unwrap();
        let session = lines
            .iter()
            .position(|l| l.contains("per-session"))
            .unwrap();
        assert!(base < session, "precedence order is the content");

        // No match is a STATUS, not an empty panel.
        let none = profile_inspector_lines(None, &[], &["base profile".to_string()]);
        assert!(none[0].contains("no profile matched"));
        assert!(none.iter().any(|l| l.contains("(none declared)")));
    }
}
