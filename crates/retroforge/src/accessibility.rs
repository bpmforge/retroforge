//! UI scale and the high-contrast theme (ticket W8-04, criterion 2;
//! `docs/design/FRONTEND_UI.md` §6).
//!
//! §6 asks for a "UI scale slider (egui pixels_per_point), high-contrast
//! theme, colorblind-safe palettes ... screen-reader labels". The labels
//! and focus order are navigation and live in [`crate::ui_nav`] and the
//! gamepad harness; this module is the *theme* half, which W8-04's
//! handoff recorded as not started.
//!
//! ## The contrast ratio is MEASURED, not asserted
//!
//! This is the whole reason this module has arithmetic in it rather than
//! a palette constant and a comment.
//!
//! A "high-contrast theme" nobody measured is a claim, and it is the
//! easiest possible claim to get wrong: pick two colours that look
//! strongly different on the developer's monitor, ship them, and a user
//! who needed the feature finds it does not help. So [`contrast_ratio`]
//! implements WCAG 2.1's actual formula — relative luminance with the
//! sRGB transfer curve, `(L1 + 0.05) / (L2 + 0.05)` — and the tests
//! assert the shipped palette clears **AA (4.5:1)** for body text and
//! **AAA (7:1)** where the theme claims to.
//!
//! The formula is transcribed from the published definition rather than
//! approximated: a "close enough" luminance (the common mistake is
//! averaging the channels, or skipping the gamma expansion) reports
//! numbers that are wrong in the direction of flattering the palette.
//!
//! ## Why scale is clamped
//!
//! `pixels_per_point` below ~0.5 makes the UI unreadable and above ~4
//! makes a 1080p window hold about two widgets — and both extremes are
//! reachable by a slider drag or a stray settings file. A user who
//! cannot read the UI cannot open the settings window to undo it, so the
//! bound is enforced here rather than trusted to the widget.

use serde::{Deserialize, Serialize};

/// Smallest usable `pixels_per_point`.
pub const MIN_UI_SCALE: f32 = 0.5;
/// Largest usable `pixels_per_point`.
pub const MAX_UI_SCALE: f32 = 4.0;
/// WCAG 2.1 AA minimum for body text.
pub const WCAG_AA: f32 = 4.5;
/// WCAG 2.1 AAA minimum for body text.
pub const WCAG_AAA: f32 = 7.0;

/// The accessibility half of `[video]`-adjacent settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccessibilitySettings {
    /// egui `pixels_per_point`. Always within
    /// [`MIN_UI_SCALE`]..=[`MAX_UI_SCALE`] once it has been through
    /// [`AccessibilitySettings::normalized`].
    pub ui_scale: f32,
    /// Whether the high-contrast palette is in use.
    pub high_contrast: bool,
}

impl Default for AccessibilitySettings {
    fn default() -> Self {
        Self {
            // 1.0, not the platform default: a fresh install must look
            // the same everywhere, and egui already applies the system
            // scale on top of this.
            ui_scale: 1.0,
            // OFF by default. High contrast is an accommodation, not an
            // improvement — imposing it on everyone would be the same
            // mistake as enabling an enhancement by default (law 6).
            high_contrast: false,
        }
    }
}

impl AccessibilitySettings {
    /// A copy with `ui_scale` clamped and NaN replaced.
    ///
    /// NaN is handled explicitly because it survives `clamp` — `f32::NAN.clamp(0.5, 4.0)`
    /// is NaN — and a NaN `pixels_per_point` gives a window that renders
    /// nothing at all. A settings file with `ui_scale = nan` is unusual
    /// but it is exactly the input that would brick the UI.
    #[must_use]
    pub fn normalized(&self) -> Self {
        let scale = if self.ui_scale.is_finite() {
            self.ui_scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE)
        } else {
            1.0
        };
        Self {
            ui_scale: scale,
            high_contrast: self.high_contrast,
        }
    }

    /// The palette this configuration should draw with.
    #[must_use]
    pub fn palette(&self) -> Palette {
        if self.high_contrast {
            Palette::HIGH_CONTRAST
        } else {
            Palette::DEFAULT
        }
    }
}

/// The colours a theme is judged on.
///
/// Deliberately tiny: these are the pairs whose contrast is *asserted*.
/// A palette with thirty entries and two tested pairs would be a palette
/// nobody actually checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Window background.
    pub background: [u8; 3],
    /// Body text on `background`.
    pub text: [u8; 3],
    /// Text of a focused/selected widget.
    pub accent: [u8; 3],
}

impl Palette {
    /// egui's dark theme, near enough for measurement.
    pub const DEFAULT: Palette = Palette {
        background: [0x1B, 0x1B, 0x1B],
        text: [0xC8, 0xC8, 0xC8],
        accent: [0x5A, 0xAF, 0xFF],
    };

    /// The high-contrast palette. Pure white on pure black gives 21:1 —
    /// the maximum the formula can produce — and the accent is chosen to
    /// clear AAA rather than merely AA, because a focus indicator a user
    /// cannot distinguish is the one element that makes keyboard and
    /// gamepad navigation unusable.
    pub const HIGH_CONTRAST: Palette = Palette {
        background: [0x00, 0x00, 0x00],
        text: [0xFF, 0xFF, 0xFF],
        accent: [0xFF, 0xD7, 0x00],
    };

    /// Contrast of body text against the background.
    #[must_use]
    pub fn text_contrast(&self) -> f32 {
        contrast_ratio(self.text, self.background)
    }

    /// Contrast of the focus accent against the background.
    #[must_use]
    pub fn accent_contrast(&self) -> f32 {
        contrast_ratio(self.accent, self.background)
    }
}

/// One channel's contribution to relative luminance (WCAG 2.1).
///
/// The gamma expansion is the step that is usually skipped, and skipping
/// it inflates every ratio — i.e. it reports a palette as more accessible
/// than it is, which is the wrong direction to be wrong in.
fn channel_luminance(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Relative luminance of an sRGB colour (WCAG 2.1).
#[must_use]
pub fn relative_luminance(rgb: [u8; 3]) -> f32 {
    0.2126 * channel_luminance(rgb[0])
        + 0.7152 * channel_luminance(rgb[1])
        + 0.0722 * channel_luminance(rgb[2])
}

/// WCAG 2.1 contrast ratio between two colours, `1.0`..=`21.0`.
///
/// Order-independent: the brighter colour always becomes `L1`, so a
/// caller cannot get a wrong answer by passing background first.
#[must_use]
pub fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formula's own anchors: black-on-white is 21:1 exactly, and a
    /// colour against itself is 1:1. If these are wrong every other
    /// number here is decoration.
    #[test]
    fn the_formula_matches_its_published_anchors() {
        let white = [0xFF, 0xFF, 0xFF];
        let black = [0x00, 0x00, 0x00];
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
        assert!(
            (contrast_ratio(black, white) - 21.0).abs() < 0.01,
            "order must not matter"
        );
        assert!((contrast_ratio(white, white) - 1.0).abs() < 0.001);
    }

    /// A known third-party value, so the transcription is checked against
    /// something other than its own extremes: #767676 on white is the
    /// canonical "exactly AA" grey, ~4.54:1.
    #[test]
    fn a_known_midtone_matches_the_published_value() {
        let r = contrast_ratio([0x76, 0x76, 0x76], [0xFF, 0xFF, 0xFF]);
        assert!((r - 4.54).abs() < 0.05, "expected ~4.54, got {r}");
    }

    /// **Criterion 2, measured.** The claim "high-contrast theme" is only
    /// worth making if the numbers back it.
    #[test]
    fn the_high_contrast_palette_clears_aaa() {
        let p = Palette::HIGH_CONTRAST;
        assert!(
            p.text_contrast() >= WCAG_AAA,
            "body text is {:.2}:1, below AAA {WCAG_AAA}",
            p.text_contrast()
        );
        assert!(
            p.accent_contrast() >= WCAG_AAA,
            "the FOCUS accent is {:.2}:1, below AAA — a focus indicator a user cannot \
             distinguish makes gamepad navigation unusable",
            p.accent_contrast()
        );
    }

    /// The default theme must still be legible; it is simply not the
    /// accommodation.
    #[test]
    fn the_default_palette_clears_aa() {
        let p = Palette::DEFAULT;
        assert!(p.text_contrast() >= WCAG_AA, "{:.2}:1", p.text_contrast());
        assert!(
            p.accent_contrast() >= WCAG_AA,
            "{:.2}:1",
            p.accent_contrast()
        );
    }

    /// Anti-vacuity: the high-contrast palette must actually be BETTER.
    /// Without this, shipping the same palette twice would pass every
    /// assertion above.
    #[test]
    fn high_contrast_is_measurably_better_than_the_default() {
        assert!(
            Palette::HIGH_CONTRAST.text_contrast() > Palette::DEFAULT.text_contrast(),
            "a 'high contrast' theme that is not higher contrast is a label, not a feature"
        );
    }

    #[test]
    fn the_default_settings_are_unscaled_and_not_high_contrast() {
        // Law 6's shape: an accommodation is opt-in, not imposed.
        let d = AccessibilitySettings::default();
        assert_eq!(d.ui_scale, 1.0);
        assert!(!d.high_contrast);
        assert_eq!(d.palette(), Palette::DEFAULT);
    }

    #[test]
    fn ui_scale_is_clamped_at_both_ends() {
        for (input, want) in [(0.01, MIN_UI_SCALE), (99.0, MAX_UI_SCALE), (1.5, 1.5)] {
            let s = AccessibilitySettings {
                ui_scale: input,
                high_contrast: false,
            };
            assert_eq!(s.normalized().ui_scale, want, "input {input}");
        }
    }

    #[test]
    fn a_non_finite_ui_scale_falls_back_rather_than_bricking_the_window() {
        // NaN SURVIVES clamp — f32::NAN.clamp(0.5, 4.0) is NaN — and a
        // NaN pixels_per_point renders nothing, leaving no way to open
        // settings and undo it.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let s = AccessibilitySettings {
                ui_scale: bad,
                high_contrast: false,
            };
            assert_eq!(s.normalized().ui_scale, 1.0, "input {bad}");
        }
    }

    #[test]
    fn enabling_high_contrast_switches_the_palette() {
        let s = AccessibilitySettings {
            ui_scale: 1.0,
            high_contrast: true,
        };
        assert_eq!(s.palette(), Palette::HIGH_CONTRAST);
    }
}
