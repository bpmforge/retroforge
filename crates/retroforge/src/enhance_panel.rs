//! The player's view of enhancements (ticket W20-18;
//! `docs/design/UX_WAVE_20.md` §6).
//!
//! The Enhance workspace (`crate::enhance_dock`) is the research surface:
//! scope tags, provenance, ladder chips. A player reading "(generic,
//! heuristic-gated) [shadow] — requires Enhanced mode" learns nothing they
//! can act on. This module is the plain-language layer the Quick Menu's
//! Enhancements section draws from: what each feature does, what it
//! needs, and the trust ladder as words — **Learning** (shadow: watching,
//! never acting), **Suggesting** (advisory), **On** (active).
//!
//! Same rows, same gating: every card comes from
//! `crate::enhance_ui::feature_rows`, so the panel cannot offer what the
//! honesty contract says is unavailable, and cannot disagree with the
//! badge.

use rf_enhance::trust::TrustState;

use crate::enhance_ui::Availability;

/// One sentence: what the feature does, in a player's words.
#[must_use]
pub fn describe(id: &str) -> &'static str {
    match id {
        "sprite_overlay" => "Draws sprites the console would hide when too many share one line.",
        "deflicker" => "Steadies sprites that flicker because the game takes turns drawing them.",
        "widescreen_decoded" => {
            "Widens the picture with scenery the game already keeps beside the screen."
        }
        "full_level_view" => "Shows the whole level as a map, with you on it.",
        "loading_fast_forward" => "Speeds through loading waits this game is known to have.",
        "atmosphere_fog" => "Adds drifting fog where the game draws a fog layer.",
        "diorama" => "Raises the level's walls into a 3D diorama.",
        "mode7_ground" => "Turns a Mode 7 floor into a 3D ground you look across.",
        _ => "An enhancement.",
    }
}

/// One sentence: what the feature needs before it can do anything, or
/// that it is ready.
/// Ticket W22-03: whether a feature can work without a profile — the
/// card's group. Mode 7 in 3D needs only the game to be showing Mode 7.
#[must_use]
pub fn works_on_any_game(id: &str) -> bool {
    matches!(id, "sprite_overlay" | "deflicker" | "mode7_ground")
}

/// Ticket W22-03: the mode an `Availability::NeedsMode` names, for the
/// card's "Switch to …" button.
#[must_use]
pub fn mode_named(name: &str) -> Option<crate::game_settings::Mode> {
    crate::game_settings::Mode::all()
        .into_iter()
        .find(|m| m.display_name() == name)
}

/// Ticket W22-03 (Brad, 2026-10-07: plain names): the name a player sees
/// for a feature. The research label (`FeatureRow::label`) stays in the
/// Enhance workspace.
#[must_use]
pub const fn player_name(id: &str) -> &'static str {
    match id.as_bytes() {
        b"sprite_overlay" => "No sprite dropout",
        b"deflicker" => "Steady sprites",
        b"widescreen_decoded" => "Widescreen",
        b"full_level_view" => "Full-level map",
        b"loading_fast_forward" => "Skip loading",
        b"atmosphere_fog" => "Fog and mist",
        b"diorama" => "3D diorama",
        b"mode7_ground" => "Mode 7 in 3D",
        _ => "Enhancement",
    }
}

#[must_use]
pub fn requirement(availability: &Availability) -> String {
    match availability {
        Availability::Available => "Ready.".to_string(),
        Availability::NeedsMode(mode) => format!("Needs {mode} mode."),
        Availability::NeedsProfile => "Needs a profile for this game.".to_string(),
        Availability::NeedsGameState(what) => format!("Needs {what}."),
    }
}

/// The trust ladder rung, in words.
#[must_use]
pub const fn trust_word(state: TrustState) -> &'static str {
    match state {
        TrustState::Shadow => "Learning",
        TrustState::Advisory => "Suggesting",
        TrustState::Active => "On",
    }
}

/// What each rung means, for its tooltip.
#[must_use]
pub const fn trust_meaning(state: TrustState) -> &'static str {
    match state {
        TrustState::Shadow => "Watches the game and keeps notes, but changes nothing.",
        TrustState::Advisory => "Tells you what it would do, but changes nothing.",
        TrustState::Active => "Changes what you see.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enhance_ui::{feature_rows, GameFacts};
    use crate::game_settings::GameSettings;

    /// Every row the gate produces has its own sentence, and no player
    /// sentence leaks the research vocabulary.
    #[test]
    fn every_feature_is_described_in_plain_words() {
        let rows = feature_rows(
            &GameSettings::default(),
            &GameFacts::new(false, false, false),
        );
        for row in rows {
            let text = format!("{} {}", describe(row.id), requirement(&row.availability));
            assert_ne!(
                describe(row.id),
                "An enhancement.",
                "{} has no description",
                row.id
            );
            for jargon in [
                "heuristic",
                "generic",
                "shadow",
                "advisory",
                "profile-gated",
                "W1",
                "W2",
            ] {
                assert!(
                    !text.to_lowercase().contains(jargon),
                    "{}: {text:?} contains {jargon:?}",
                    row.id
                );
            }
        }
    }

    #[test]
    fn the_ladder_reads_as_words() {
        assert_eq!(trust_word(TrustState::Shadow), "Learning");
        assert_eq!(trust_word(TrustState::Advisory), "Suggesting");
        assert_eq!(trust_word(TrustState::Active), "On");
        assert_eq!(
            requirement(&Availability::NeedsMode("Enhanced")),
            "Needs Enhanced mode."
        );
    }

    /// Ticket W22-03: every feature the panel can show has its own plain
    /// name.
    #[test]
    fn every_feature_has_a_player_name() {
        let s = crate::game_settings::GameSettings::default();
        let rows = crate::enhance_ui::feature_rows(
            &s,
            &crate::enhance_ui::GameFacts::new(true, true, true),
        );
        let names: std::collections::BTreeSet<_> = rows.iter().map(|r| player_name(r.id)).collect();
        assert_eq!(names.len(), rows.len());
        assert!(!names.contains("Enhancement"));
    }

    /// Ticket W22-03: every NeedsMode the rows use names a real mode.
    #[test]
    fn needs_mode_names_resolve() {
        use crate::game_settings::Mode;
        assert_eq!(mode_named("Enhanced"), Some(Mode::Enhanced));
        assert_eq!(mode_named("Game-Aware"), Some(Mode::GameAware));
        assert_eq!(mode_named("Nope"), None);
        assert!(works_on_any_game("sprite_overlay") && !works_on_any_game("widescreen_decoded"));
    }
}
