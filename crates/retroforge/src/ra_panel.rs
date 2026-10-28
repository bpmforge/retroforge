//! The Quick Menu's Achievements section (ticket W41-06;
//! `docs/design/CHEATS_ACHIEVEMENTS.md` section 4).
//!
//! Draws a snapshot the core thread sent (`CoreEvent::RaList`); it never
//! touches the RetroAchievements session (CLAUDE.md law 4). No images are
//! downloaded: a row is text, and its state is in the WORD ("Locked",
//! "Unlocked"), not carried by colour alone.

use eframe::egui;
use rf_achievements::AchievementInfo;

use crate::ra::{is_unlocked, progress_line};
use crate::theme::{type_scale, Tokens};

/// Everything the panel shows, borrowed from the app.
pub struct RaView<'a> {
    pub enabled: bool,
    pub user: Option<&'a str>,
    pub game: Option<&'a str>,
    /// The game data arrived.
    pub loaded: bool,
    /// The server did not know the game (or the load failed).
    pub load_failed: bool,
    pub hardcore: bool,
    pub offline: bool,
    pub items: &'a [AchievementInfo],
}

/// Why there is no list to show, or `None` when there is one.
#[must_use]
pub fn empty_state(v: &RaView<'_>) -> Option<&'static str> {
    if !v.enabled {
        Some("Turn on RetroAchievements in Settings")
    } else if v.user.is_none() {
        Some("Sign in to RetroAchievements in Settings")
    } else if v.load_failed {
        Some("RetroAchievements does not recognise this game, or it has no achievements")
    } else if !v.loaded {
        Some("Loading achievements...")
    } else if v.items.is_empty() {
        Some("This game has no achievements")
    } else {
        None
    }
}

/// The accessible name of one row: all of it, in reading order.
#[must_use]
pub fn row_name(a: &AchievementInfo, hardcore: bool) -> String {
    let state = if is_unlocked(a, hardcore) {
        "unlocked"
    } else {
        "locked"
    };
    let mut s = format!("{}, {} points, {state}", a.title, a.points);
    if !a.description.is_empty() {
        s.push_str(". ");
        s.push_str(&a.description);
    }
    if !a.measured_progress.is_empty() {
        s.push_str(". Progress ");
        s.push_str(&a.measured_progress);
    }
    s
}

/// Draw the section.
pub fn show(ui: &mut egui::Ui, tokens: &Tokens, v: &RaView<'_>) {
    if let Some(why) = empty_state(v) {
        ui.label(egui::RichText::new(why).color(tokens.muted));
        return;
    }
    let mode = if v.hardcore { "Hardcore" } else { "Softcore" };
    ui.label(
        egui::RichText::new(format!(
            "{}  \u{b7}  {}  \u{b7}  {mode}",
            v.game.unwrap_or("Unknown game"),
            v.user.unwrap_or_default()
        ))
        .font(crate::theme::condensed(type_scale::SUBHEAD))
        .color(tokens.ink),
    );
    ui.label(egui::RichText::new(progress_line(v.items, v.hardcore)).color(tokens.ink));
    if v.offline {
        ui.label(
            egui::RichText::new(crate::ra::OFFLINE_NOTICE)
                .font(egui::FontId::proportional(type_scale::CAPTION))
                .color(tokens.muted),
        );
    }
    ui.add_space(tokens.space_2);
    for a in v.items {
        let unlocked = is_unlocked(a, v.hardcore);
        let name = row_name(a, v.hardcore);
        let row = egui::Frame::new()
            .fill(tokens.surface)
            .stroke(egui::Stroke::new(1.0, tokens.line))
            .corner_radius(tokens.radius_sm)
            .inner_margin(tokens.space_2)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&a.title).strong().color(tokens.ink));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (word, colour) = if unlocked {
                            ("Unlocked", tokens.ok)
                        } else {
                            ("Locked", tokens.muted)
                        };
                        ui.label(egui::RichText::new(word).color(colour));
                        ui.label(
                            egui::RichText::new(format!("{} pts", a.points)).color(tokens.muted),
                        );
                    });
                });
                if !a.description.is_empty() {
                    ui.label(
                        egui::RichText::new(&a.description)
                            .font(egui::FontId::proportional(type_scale::CAPTION))
                            .color(tokens.muted),
                    );
                }
                if !a.measured_progress.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("Progress: {}", a.measured_progress))
                            .font(egui::FontId::proportional(type_scale::CAPTION))
                            .color(tokens.muted),
                    );
                }
            });
        row.response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, name.clone()));
        ui.add_space(tokens.space_1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(items: &'a [AchievementInfo]) -> RaView<'a> {
        RaView {
            enabled: true,
            user: Some("Bob"),
            game: Some("Test Game"),
            loaded: true,
            load_failed: false,
            hardcore: false,
            offline: false,
            items,
        }
    }

    fn item(measured: &str) -> AchievementInfo {
        AchievementInfo {
            id: 1,
            title: "Five Alive".into(),
            description: "Reach 5".into(),
            points: 5,
            unlocked_softcore: false,
            unlocked_hardcore: false,
            state: 1,
            measured_progress: measured.into(),
            badge_name: String::new(),
        }
    }

    #[test]
    fn empty_states_are_checked_in_the_order_the_player_can_fix_them() {
        let items = [item("")];
        let mut v = view(&items);
        assert_eq!(empty_state(&v), None);
        v.items = &[];
        assert_eq!(empty_state(&v), Some("This game has no achievements"));
        v.loaded = false;
        assert_eq!(empty_state(&v), Some("Loading achievements..."));
        v.load_failed = true;
        assert!(empty_state(&v).unwrap().contains("does not recognise"));
        v.user = None;
        assert!(empty_state(&v).unwrap().starts_with("Sign in"));
        v.enabled = false;
        assert_eq!(
            empty_state(&v),
            Some("Turn on RetroAchievements in Settings")
        );
    }

    #[test]
    fn a_row_name_carries_title_points_state_and_progress() {
        let mut a = item("3/5");
        assert_eq!(
            row_name(&a, false),
            "Five Alive, 5 points, locked. Reach 5. Progress 3/5"
        );
        a.unlocked_softcore = true;
        assert!(row_name(&a, false).contains("5 points, unlocked"));
        // Hardcore does not count a softcore unlock.
        assert!(row_name(&a, true).contains("locked"));
    }
}
