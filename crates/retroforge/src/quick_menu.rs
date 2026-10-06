//! The in-game Quick Menu's sections (ticket W20-10;
//! `docs/design/UX_WAVE_20.md` §5).
//!
//! The menu replaces the plain `egui::Window` Esc opened until W20-10 —
//! a column of buttons that did not pause the game (fixed in W20-03) and
//! sent most of its entries off to other windows. The shape follows what
//! RetroArch, DuckStation and Dolphin converged on: the game frozen and
//! dimmed behind, a rail of sections on the left, the chosen section's
//! content on the right, the honesty badge at the top, and a hint bar
//! naming the pad buttons.
//!
//! This module is the data half (which sections, in what order, with what
//! icon); `crate::app` draws them.

use egui_phosphor::regular as ph;

/// One entry in the rail. Most show content on the right; `Resume`,
/// `Reset` and `Quit` are actions that also show a short explanation
/// first, so a pad user who lands on one does not trigger it by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Resume,
    Save,
    Load,
    Rewind,
    Display,
    Enhancements,
    Controls,
    Settings,
    Reset,
    Quit,
}

impl Section {
    /// Rail order.
    pub const ALL: [Section; 10] = [
        Section::Resume,
        Section::Save,
        Section::Load,
        Section::Rewind,
        Section::Display,
        Section::Enhancements,
        Section::Controls,
        Section::Settings,
        Section::Reset,
        Section::Quit,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Section::Resume => "Resume",
            Section::Save => "Save state",
            Section::Load => "Load state",
            Section::Rewind => "Rewind",
            Section::Display => "Display",
            Section::Enhancements => "Enhancements",
            Section::Controls => "Controls",
            Section::Settings => "Settings",
            Section::Reset => "Reset game",
            Section::Quit => "Quit to library",
        }
    }

    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Section::Resume => ph::PLAY,
            Section::Save => ph::FLOPPY_DISK,
            Section::Load => ph::FOLDER_OPEN,
            Section::Rewind => ph::CLOCK_COUNTER_CLOCKWISE,
            Section::Display => ph::MONITOR,
            Section::Enhancements => ph::SPARKLE,
            Section::Controls => ph::GAME_CONTROLLER,
            Section::Settings => ph::GEAR,
            Section::Reset => ph::ARROW_COUNTER_CLOCKWISE,
            Section::Quit => ph::SIGN_OUT,
        }
    }

    /// The rail entry's text: icon, then label.
    #[must_use]
    pub fn rail_text(self) -> String {
        format!("{}  {}", self.icon(), self.label())
    }
}

/// The hint bar along the bottom: what each pad button does here.
pub const HINT: &str = "A  Select     B  Back     LB / RB  Section     Esc  Resume";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_is_listed_once_with_a_distinct_label() {
        let labels: std::collections::BTreeSet<_> =
            Section::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(labels.len(), Section::ALL.len());
        assert_eq!(
            Section::ALL[0],
            Section::Resume,
            "Resume is the default focus"
        );
    }
}
