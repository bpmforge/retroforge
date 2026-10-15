//! Ticket W30-08: the controls and words under the CPU / SA-1 overclock and
//! NES "Less slowdown" rows, shared by the Enhance workspace and the
//! Enhancements cards so the two cannot drift (the `diorama_controls`
//! pattern).
//!
//! The wording follows one rule: say what the setting does to the GAME.
//! Governed (the default) only removes slowdown and keeps the game at its
//! normal speed; Fixed is the advanced option that can make a game run
//! faster than intended.

use eframe::egui;
use rf_enhance::speed_governor::{GovernorStatus, Notice, TargetSource};

use crate::game_settings::{OverclockMode, CLOCK_PCT_MIN, NES_EXTRA_LINES_MAX};

/// The card text under a chip's slider for each mode.
#[must_use]
pub const fn mode_blurb(mode: OverclockMode) -> &'static str {
    match mode {
        OverclockMode::Governed => "Removes slowdown and keeps the game at its normal speed.",
        OverclockMode::Fixed => "Some games run faster than intended.",
    }
}

/// The NES row's card text.
pub const NES_BLURB: &str = "Removes slowdown; some games may glitch.";

/// The label of each mode's choice.
#[must_use]
pub const fn mode_label(mode: OverclockMode) -> &'static str {
    match mode {
        OverclockMode::Governed => "Governed: only while the game slows down",
        OverclockMode::Fixed => "Fixed (advanced): always this speed",
    }
}

/// The live status line for a governed chip. `None` is a governor that has
/// not reported yet (the first frames after switching on).
#[must_use]
pub fn status_text(status: Option<&GovernorStatus>) -> String {
    let Some(s) = status else {
        return "Watching the game\u{2026}".to_owned();
    };
    match s.notice {
        Notice::NoSignal => {
            "No speed signal for this game; add a tick counter to its profile.".to_owned()
        }
        Notice::TurnedOff => "Turned off: the game stopped responding at higher speed.".to_owned(),
        Notice::Learning | Notice::Active => match s.target {
            Some(target) => {
                let source = match s.target_source {
                    TargetSource::Profile => "from profile",
                    _ => "learned",
                };
                format!(
                    "Normal speed: {target} ticks/s ({source}) \u{2014} now {}%",
                    s.multiplier
                )
            }
            None => format!(
                "Learning this game's normal speed\u{2026} now {}%",
                s.multiplier
            ),
        },
    }
}

/// The ceiling slider, the Governed / Fixed choice, the mode's blurb and (when
/// governed) the live status line. `cap` is the profile's highest allowed
/// ceiling. `muted` colours the small print (`None` = the default text).
/// `true` when a setting changed.
pub fn clock_controls(
    ui: &mut egui::Ui,
    pct: &mut u16,
    mode: &mut OverclockMode,
    cap: u16,
    status: Option<&GovernorStatus>,
    muted: Option<egui::Color32>,
) -> bool {
    let mut changed = false;
    let small = |text: String| {
        let t = egui::RichText::new(text).small();
        match muted {
            Some(c) => t.color(c),
            None => t,
        }
    };
    ui.horizontal(|ui| {
        ui.label(match *mode {
            OverclockMode::Governed => "Up to",
            OverclockMode::Fixed => "Speed",
        });
        changed |= ui
            .add(
                egui::Slider::new(pct, CLOCK_PCT_MIN..=cap.max(CLOCK_PCT_MIN))
                    .step_by(100.0)
                    .suffix("%"),
            )
            .changed();
    });
    ui.horizontal_wrapped(|ui| {
        for choice in [OverclockMode::Governed, OverclockMode::Fixed] {
            if ui
                .selectable_label(*mode == choice, mode_label(choice))
                .clicked()
                && *mode != choice
            {
                *mode = choice;
                changed = true;
            }
        }
    });
    ui.label(small(mode_blurb(*mode).to_owned()));
    if *mode == OverclockMode::Governed {
        ui.label(small(status_text(status)));
    }
    changed
}

/// The NES slider (0..=cap lines) and its caveat. `true` when it moved.
pub fn nes_lines_controls(
    ui: &mut egui::Ui,
    lines: &mut u16,
    cap: u16,
    muted: Option<egui::Color32>,
) -> bool {
    let changed = ui
        .add(egui::Slider::new(lines, 0..=cap.min(NES_EXTRA_LINES_MAX)).suffix(" lines"))
        .changed();
    let text = egui::RichText::new(NES_BLURB).small();
    ui.label(match muted {
        Some(c) => text.color(c),
        None => text,
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(notice: Notice, target: Option<u32>, source: TargetSource, m: u16) -> GovernorStatus {
        GovernorStatus {
            notice,
            multiplier: m,
            rate: 58,
            target,
            target_source: source,
        }
    }

    #[test]
    fn the_status_line_names_the_target_its_source_and_the_clock() {
        let learned = status_text(Some(&status(
            Notice::Active,
            Some(60),
            TargetSource::Learned,
            200,
        )));
        assert_eq!(
            learned,
            "Normal speed: 60 ticks/s (learned) \u{2014} now 200%"
        );
        let profile = status_text(Some(&status(
            Notice::Active,
            Some(30),
            TargetSource::Profile,
            100,
        )));
        assert!(profile.contains("30 ticks/s (from profile)") && profile.ends_with("now 100%"));
    }

    #[test]
    fn the_refusals_are_explained_in_plain_words() {
        let none = status_text(Some(&status(
            Notice::NoSignal,
            None,
            TargetSource::None,
            100,
        )));
        assert_eq!(
            none,
            "No speed signal for this game; add a tick counter to its profile."
        );
        let off = status_text(Some(&status(
            Notice::TurnedOff,
            Some(60),
            TargetSource::Learned,
            100,
        )));
        assert_eq!(
            off,
            "Turned off: the game stopped responding at higher speed."
        );
        assert!(status_text(Some(&status(
            Notice::Learning,
            None,
            TargetSource::None,
            100
        )))
        .starts_with("Learning"));
        assert!(status_text(None).starts_with("Watching"));
    }

    #[test]
    fn each_mode_has_the_card_text_the_design_asks_for() {
        assert_eq!(
            mode_blurb(OverclockMode::Governed),
            "Removes slowdown and keeps the game at its normal speed."
        );
        assert_eq!(
            mode_blurb(OverclockMode::Fixed),
            "Some games run faster than intended."
        );
        assert_eq!(
            mode_label(OverclockMode::Fixed),
            "Fixed (advanced): always this speed"
        );
    }
}
