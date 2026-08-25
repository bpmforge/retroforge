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
/// nobody actually checked. Every field added here must come with its
/// assertions — [`Palette::worst_contrast`] exists so that adding a
/// colour without checking it is not possible by accident.
///
/// Two surfaces, not one (ticket W10-01). Until W10-01 there was a single
/// `background`, which is why every control in the status bar rendered as
/// the same flat rectangle: with nothing to sit *on*, a button, a
/// disabled button and a non-clickable status badge are indistinguishable.
/// `raised` is what a control is drawn on, and every text colour is
/// verified against **both** surfaces — a palette checked only against
/// the page background says nothing about the text actually inside the
/// widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The page behind everything.
    pub background: [u8; 3],
    /// The surface a control is drawn on: buttons, menus, the status
    /// chips. One step up from `background`, which is the whole point.
    pub raised: [u8; 3],
    /// Body text, on either surface.
    pub text: [u8; 3],
    /// Secondary text — units, placeholders, the "no profile" chip.
    /// Dimmer than `text` on purpose, but still held to the same
    /// standard: unreadable secondary text is unreadable text.
    pub text_muted: [u8; 3],
    /// Focus rings, selection, and the one colour the eye is meant to
    /// find first.
    pub accent: [u8; 3],
}

impl Palette {
    /// The dark theme.
    ///
    /// Every one of the six text/surface pairs clears **AAA**, computed
    /// before these values were written rather than checked afterwards;
    /// the tests below re-derive them so a future edit cannot quietly
    /// drop one below the line.
    pub const DEFAULT: Palette = Palette {
        background: [0x14, 0x16, 0x1A],
        raised: [0x1E, 0x22, 0x2A],
        text: [0xD6, 0xDA, 0xE2],
        text_muted: [0xA6, 0xAF, 0xBE],
        // Lighter than the #5AAFFF this replaced, for one measured
        // reason: #5AAFFF is AAA on `background` but only 6.84:1 on
        // `raised`, and a focus ring is drawn on controls, which sit on
        // `raised`. The value that matters is the one on the surface the
        // ring is actually drawn on.
        accent: [0x6F, 0xBA, 0xFF],
    };

    /// The high-contrast palette. Pure white on pure black gives 21:1 —
    /// the maximum the formula can produce — and the accent is chosen to
    /// clear AAA rather than merely AA, because a focus indicator a user
    /// cannot distinguish is the one element that makes keyboard and
    /// gamepad navigation unusable.
    pub const HIGH_CONTRAST: Palette = Palette {
        background: [0x00, 0x00, 0x00],
        raised: [0x1A, 0x1A, 0x1A],
        text: [0xFF, 0xFF, 0xFF],
        text_muted: [0xD0, 0xD0, 0xD0],
        accent: [0xFF, 0xD7, 0x00],
    };

    /// Every (foreground, surface) pair this palette will ever render,
    /// named, so a test can assert over the whole set instead of over a
    /// hand-copied list that silently stops covering new fields.
    #[must_use]
    pub fn pairs(&self) -> [(&'static str, f32); 6] {
        [
            (
                "text on background",
                contrast_ratio(self.text, self.background),
            ),
            ("text on raised", contrast_ratio(self.text, self.raised)),
            (
                "text_muted on background",
                contrast_ratio(self.text_muted, self.background),
            ),
            (
                "text_muted on raised",
                contrast_ratio(self.text_muted, self.raised),
            ),
            (
                "accent on background",
                contrast_ratio(self.accent, self.background),
            ),
            ("accent on raised", contrast_ratio(self.accent, self.raised)),
        ]
    }

    /// The weakest pair, and its name. This is the number a palette is
    /// actually worth: an average would let one unreadable pair hide
    /// behind five good ones.
    #[must_use]
    pub fn worst_contrast(&self) -> (&'static str, f32) {
        self.pairs().into_iter().fold(
            ("", f32::INFINITY),
            |acc, p| if p.1 < acc.1 { p } else { acc },
        )
    }

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

    /// **Every pair, on both surfaces, clears AAA — in both palettes.**
    ///
    /// Asserted over `Palette::pairs()` rather than over a hand-written
    /// list, so a colour added to the struct without a matching
    /// assertion is impossible: the new pair joins this loop or it does
    /// not exist. The old version of this test checked two pairs against
    /// a three-colour palette and was the entire evidence base for a
    /// module nothing imported.
    #[test]
    fn every_pair_in_every_palette_clears_aaa() {
        for (name, palette) in [
            ("DEFAULT", Palette::DEFAULT),
            ("HIGH_CONTRAST", Palette::HIGH_CONTRAST),
        ] {
            for (pair, ratio) in palette.pairs() {
                assert!(
                    ratio >= WCAG_AAA,
                    "{name}: {pair} is {ratio:.2}:1, below AAA ({WCAG_AAA}:1). \
                     Text a user cannot read is not a theme."
                );
            }
        }
    }

    /// **The accent clears AAA on `raised`, not merely on `background`.**
    ///
    /// Called out separately because it is the pair that is easiest to
    /// get wrong and worst to get wrong. The focus ring is drawn on
    /// *controls*, and controls sit on `raised` — so a palette verified
    /// only against the page background can ship a focus indicator that
    /// is genuinely hard to see, which is the one defect that makes
    /// keyboard and gamepad navigation unusable. #5AAFFF, the accent
    /// this replaced in W10-01, was AAA on `background` and 6.84:1 on
    /// `raised`.
    #[test]
    fn the_accent_is_verified_against_the_surface_it_is_drawn_on() {
        for (name, palette) in [
            ("DEFAULT", Palette::DEFAULT),
            ("HIGH_CONTRAST", Palette::HIGH_CONTRAST),
        ] {
            let ratio = contrast_ratio(palette.accent, palette.raised);
            assert!(
                ratio >= WCAG_AAA,
                "{name}: the focus accent is {ratio:.2}:1 against `raised`"
            );
        }
    }

    /// **`raised` is actually distinguishable from `background`.**
    ///
    /// The reason the two surfaces exist. If they were equal every pair
    /// above would still pass — the palette would be perfectly readable
    /// and perfectly flat, which is the exact complaint W10-01 started
    /// from: a primary action, a disabled button and a status badge all
    /// rendering as the same rectangle. A vacuity guard, not a colour
    /// preference.
    #[test]
    fn the_two_surfaces_are_actually_different() {
        for (name, palette) in [
            ("DEFAULT", Palette::DEFAULT),
            ("HIGH_CONTRAST", Palette::HIGH_CONTRAST),
        ] {
            assert_ne!(
                palette.raised, palette.background,
                "{name}: a raised surface identical to the background is not a surface"
            );
            let separation = contrast_ratio(palette.raised, palette.background);
            assert!(
                separation > 1.05,
                "{name}: `raised` is {separation:.3}:1 against `background` — \
                 indistinguishable in practice"
            );
        }
    }

    /// `worst_contrast` reports the weakest pair, not an average — an
    /// average would let one unreadable pair hide behind five good ones.
    #[test]
    fn worst_contrast_finds_the_weakest_pair() {
        let p = Palette::DEFAULT;
        let (name, worst) = p.worst_contrast();
        assert!(!name.is_empty(), "the weakest pair must be named");
        for (_, ratio) in p.pairs() {
            assert!(worst <= ratio + f32::EPSILON, "{worst} was not the minimum");
        }
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
