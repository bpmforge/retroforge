//! The one-click "HD-2D" look (ticket W30-06 part B).
//!
//! Settings only; there is no rendering here. The preset turns on, together,
//! what already exists: the W30-04 lens (glow, miniature focus, soft shadows),
//! Diorama walls (live from a painted height table, or decoded), and the
//! W30-05 camera tilted down and zoomed in, following the player. Turning it
//! off puts back what the player had, which [`apply`] hands back as a
//! [`Remembered`].
//!
//! Law 6: it is a settings change the player asks for, never a default, and
//! it can only be used where Diorama is available, so Accuracy is untouched.

use rf_renderer::lens::Weather;

use crate::enhance_ui::{feature_rows, Availability, GameFacts};
use crate::game_settings::GameSettings;

/// Glow strength, percent.
pub const BLOOM_PCT: u8 = 35;
/// Miniature-focus strength, percent.
pub const TILTSHIFT_PCT: u8 = 50;
/// Soft-shadow strength, percent.
pub const SHADOWS_PCT: u8 = 50;
/// Camera tilt, degrees (the default view is 52).
pub const PITCH_DEG: u8 = 40;
/// Camera zoom, percent (1.6x).
pub const ZOOM_PCT: u16 = 160;

/// What the preset replaced, so turning it off can put it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remembered {
    lens_bloom: bool,
    lens_bloom_pct: u8,
    lens_tiltshift: bool,
    lens_tiltshift_pct: u8,
    lens_shadows: bool,
    lens_shadows_pct: u8,
    lens_grade: bool,
    lens_weather: Weather,
    diorama: bool,
    diorama_pitch: u8,
    diorama_yaw: i8,
    diorama_zoom: u16,
    diorama_follow: bool,
}

impl Remembered {
    fn of(s: &GameSettings) -> Self {
        Self {
            lens_bloom: s.lens_bloom,
            lens_bloom_pct: s.lens_bloom_pct,
            lens_tiltshift: s.lens_tiltshift,
            lens_tiltshift_pct: s.lens_tiltshift_pct,
            lens_shadows: s.lens_shadows,
            lens_shadows_pct: s.lens_shadows_pct,
            lens_grade: s.lens_grade,
            lens_weather: s.lens_weather,
            diorama: s.diorama,
            diorama_pitch: s.diorama_pitch,
            diorama_yaw: s.diorama_yaw,
            diorama_zoom: s.diorama_zoom,
            diorama_follow: s.diorama_follow,
        }
    }
}

/// Why the preset cannot be used right now, in a player's words; `None`
/// when it can. It needs Diorama, so it is exactly as available as Diorama.
#[must_use]
pub fn unavailable_reason(settings: &GameSettings, facts: &GameFacts) -> Option<String> {
    let row = feature_rows(settings, facts)
        .into_iter()
        .find(|r| r.id == "diorama")?;
    match row.availability {
        Availability::Available => None,
        Availability::NeedsMode(mode) => Some(format!("Needs {mode} mode.")),
        Availability::NeedsProfile | Availability::NeedsGameState(_) => {
            Some("Needs a height table \u{2014} paint one in the Profile Builder.".to_owned())
        }
    }
}

/// Whether every setting the preset owns is at its value: what decides the
/// button reads "Turn off".
#[must_use]
pub fn is_applied(s: &GameSettings) -> bool {
    s.lens_bloom
        && s.lens_bloom_pct == BLOOM_PCT
        && s.lens_tiltshift
        && s.lens_tiltshift_pct == TILTSHIFT_PCT
        && s.lens_shadows
        && s.lens_shadows_pct == SHADOWS_PCT
        && !s.lens_grade
        && s.lens_weather == Weather::None
        && s.diorama
        && s.diorama_pitch == PITCH_DEG
        && s.diorama_zoom == ZOOM_PCT
        && s.diorama_follow
}

/// Turn the preset on, returning what it replaced. Settings only.
pub fn apply(s: &mut GameSettings) -> Remembered {
    let before = Remembered::of(s);
    s.lens_bloom = true;
    s.lens_bloom_pct = BLOOM_PCT;
    s.lens_tiltshift = true;
    s.lens_tiltshift_pct = TILTSHIFT_PCT;
    s.lens_shadows = true;
    s.lens_shadows_pct = SHADOWS_PCT;
    s.lens_grade = false;
    s.lens_weather = Weather::None;
    s.diorama = true;
    s.diorama_pitch = PITCH_DEG;
    s.diorama_yaw = 0;
    s.diorama_zoom = ZOOM_PCT;
    s.diorama_follow = true;
    before
}

/// Turn the preset off: put back what [`apply`] replaced.
pub fn restore(s: &mut GameSettings, before: &Remembered) {
    s.lens_bloom = before.lens_bloom;
    s.lens_bloom_pct = before.lens_bloom_pct;
    s.lens_tiltshift = before.lens_tiltshift;
    s.lens_tiltshift_pct = before.lens_tiltshift_pct;
    s.lens_shadows = before.lens_shadows;
    s.lens_shadows_pct = before.lens_shadows_pct;
    s.lens_grade = before.lens_grade;
    s.lens_weather = before.lens_weather;
    s.diorama = before.diorama;
    s.diorama_pitch = before.diorama_pitch;
    s.diorama_yaw = before.diorama_yaw;
    s.diorama_zoom = before.diorama_zoom;
    s.diorama_follow = before.diorama_follow;
}

/// Turn the preset off with nothing remembered (a session restarted while it
/// was on): the lens off, Diorama off and the camera back to its defaults.
pub fn reset(s: &mut GameSettings) {
    let d = GameSettings::default();
    restore(s, &Remembered::of(&d));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_settings::Mode;

    fn settings(mode: Mode) -> GameSettings {
        let mut s = GameSettings::default();
        s.mode = mode;
        s
    }

    /// Gating: the preset is exactly as available as Diorama, and says why
    /// not in the words the Profile Builder points at.
    #[test]
    fn the_preset_is_available_only_where_diorama_is_and_explains_otherwise() {
        let table = GameFacts::new(true, false, false);
        let mut with_table = table;
        with_table.diorama_available = true;

        // Game-Aware with a table (or a decoded level): available.
        assert_eq!(
            unavailable_reason(&settings(Mode::GameAware), &with_table),
            None
        );
        // Game-Aware, matched profile, nothing to raise: points at the painter.
        let why = unavailable_reason(&settings(Mode::GameAware), &table).unwrap();
        assert!(why.contains("height table") && why.contains("Profile Builder"));
        // No profile at all: same pointer.
        let none = GameFacts::new(false, false, false);
        assert!(unavailable_reason(&settings(Mode::GameAware), &none).is_some());
        // Wrong mode: names the mode, and Accuracy can never use it (law 6).
        assert_eq!(
            unavailable_reason(&settings(Mode::Enhanced), &with_table).as_deref(),
            Some("Needs Game-Aware mode.")
        );
        assert!(unavailable_reason(&settings(Mode::Accuracy), &with_table).is_some());
    }

    /// Settings: one click sets exactly the documented values and the rows
    /// read as on; undoing it restores the player's own values, including
    /// ones the preset overwrote.
    #[test]
    fn applying_sets_the_hd2d_values_and_restoring_puts_back_the_players_own() {
        let mut s = settings(Mode::GameAware);
        s.lens_bloom = true;
        s.lens_bloom_pct = 80;
        s.lens_grade = true;
        s.lens_weather = Weather::Rain;
        s.diorama_pitch = 65;
        s.diorama_yaw = -12;
        s.diorama_zoom = 250;
        let mine = s.clone();
        assert!(!is_applied(&s));

        let mem = apply(&mut s);
        assert!(is_applied(&s));
        assert_eq!((s.lens_bloom_pct, s.lens_tiltshift_pct), (35, 50));
        assert_eq!((s.lens_shadows, s.lens_shadows_pct), (true, 50));
        assert!(!s.lens_grade);
        assert_eq!(s.lens_weather, Weather::None);
        assert_eq!((s.diorama_pitch, s.diorama_zoom), (40, 160));
        assert!(s.diorama && s.diorama_follow);
        let facts = {
            let mut f = GameFacts::new(true, false, false);
            f.diorama_available = true;
            f
        };
        let rows = feature_rows(&s, &facts);
        for id in ["diorama", "hd2d_lens"] {
            assert!(
                rows.iter().find(|r| r.id == id).unwrap().enabled,
                "{id} should read on"
            );
        }

        restore(&mut s, &mem);
        assert_eq!(s, mine, "everything the preset touched is back");
        assert!(!is_applied(&s));
    }

    #[test]
    fn resetting_without_a_memory_returns_to_the_defaults() {
        let mut s = settings(Mode::GameAware);
        apply(&mut s);
        reset(&mut s);
        assert_eq!(s, settings(Mode::GameAware));
    }
}
