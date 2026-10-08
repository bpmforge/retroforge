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
    GameInfo,
    Controls,
    Settings,
    Reset,
    Quit,
}

impl Section {
    /// Rail order.
    pub const ALL: [Section; 11] = [
        Section::Resume,
        Section::Save,
        Section::Load,
        Section::Rewind,
        Section::Display,
        Section::Enhancements,
        Section::GameInfo,
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
            Section::GameInfo => "Game info",
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
            Section::GameInfo => ph::LIST_CHECKS,
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

    /// Ticket W21-02: the key shown beside this rail entry — the binding
    /// that does the same thing outside the menu, so the menu teaches the
    /// shortcut ("learn by seeing"). `None` where no key maps to it.
    #[must_use]
    pub fn key_hint(self, bindings: &crate::app_bindings::AppBindings) -> Option<String> {
        use crate::app_bindings::AppAction;
        let action = match self {
            Section::Resume => return Some("Esc".to_owned()),
            Section::Save => AppAction::SaveState,
            Section::Load => AppAction::LoadState,
            Section::Rewind => AppAction::Rewind,
            _ => return None,
        };
        bindings.key_for(action).map(|k| k.name().to_owned())
    }
}

/// Ticket W21-02: the pad hint bar — button, its face colour (the
/// review's A red, B yellow, X blue, Y green), and what it does here.
pub const PAD_HINTS: [(&str, [u8; 3], &str); 3] = [
    ("A", [0xE2, 0x6D, 0x5A], "Select"),
    ("B", [0xE8, 0xC3, 0x4A], "Back"),
    ("LB/RB", [0x5A, 0xA2, 0xE2], "Section"),
];

/// Ticket W23-03: the hint bar after keyboard or mouse use.
pub const KEY_HINTS: [(&[&str], &str); 3] = [
    (
        &[
            egui_phosphor::regular::ARROW_UP,
            egui_phosphor::regular::ARROW_DOWN,
        ],
        "Choose",
    ),
    (&[egui_phosphor::regular::ARROW_RIGHT], "Into section"),
    (&["Enter"], "Select"),
];

/// Ticket W21-02: how much smaller the Quick Menu backdrop is than the
/// frame. Stretched back with linear filtering, 8x gives a soft blur of a
/// 256x240 frame (32x30 texels) without a shader.
pub const BACKDROP_DOWNSCALE: usize = 8;

/// Box-average `rgba` (`w`x`h`, 4 bytes per pixel) down by `factor` in
/// each direction; edge blocks that do not divide evenly are dropped.
/// `None` for a malformed buffer, a zero factor, or a frame smaller than
/// one block. Ranges only — no hand-indexed walk (law 8).
#[must_use]
pub fn downscale_box(
    rgba: &[u8],
    w: usize,
    h: usize,
    factor: usize,
) -> Option<(Vec<u8>, usize, usize)> {
    if factor == 0 || rgba.len() != w * h * 4 {
        return None;
    }
    let (sw, sh) = (w / factor, h / factor);
    if sw == 0 || sh == 0 {
        return None;
    }
    let n = (factor * factor) as u32;
    let mut out = Vec::with_capacity(sw * sh * 4);
    for by in 0..sh {
        for bx in 0..sw {
            let mut sum = [0u32; 4];
            for y in by * factor..(by + 1) * factor {
                let row = (y * w + bx * factor) * 4;
                for px in rgba[row..row + factor * 4].chunks_exact(4) {
                    for (s, &c) in sum.iter_mut().zip(px) {
                        *s += u32::from(c);
                    }
                }
            }
            #[allow(clippy::cast_possible_truncation)]
            out.extend(sum.iter().map(|s| (s / n) as u8));
        }
    }
    Some((out, sw, sh))
}

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

    #[test]
    fn downscale_averages_blocks_and_rejects_bad_input() {
        // 4x2, factor 2: two blocks, left all 10s and 30s, right all 200.
        let px = |v: u8| [v, v, v, 255];
        let mut rgba = Vec::new();
        for row in [[10, 30, 200, 200], [30, 10, 200, 200]] {
            for v in row {
                rgba.extend(px(v));
            }
        }
        let (out, w, h) = downscale_box(&rgba, 4, 2, 2).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, vec![20, 20, 20, 255, 200, 200, 200, 255]);
        assert!(downscale_box(&rgba, 4, 2, 0).is_none());
        assert!(downscale_box(&rgba, 4, 2, 4).is_none());
        assert!(downscale_box(&rgba[..8], 4, 2, 2).is_none());
    }

    #[test]
    fn rail_shows_the_keys_that_do_the_same_thing() {
        let b = crate::app_bindings::AppBindings::default();
        assert_eq!(Section::Resume.key_hint(&b).as_deref(), Some("Esc"));
        assert_eq!(
            Section::Save.key_hint(&b),
            b.key_for(crate::app_bindings::AppAction::SaveState)
                .map(|k| k.name().to_owned())
        );
        assert!(Section::Save.key_hint(&b).is_some());
        assert!(Section::Settings.key_hint(&b).is_none());
    }
}
