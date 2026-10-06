//! Theme tokens and typography (ticket W15-07; `docs/design/UX_WAVE_15.md`
//! §8, `docs/design/FRONTEND_UI.md` §6).
//!
//! ## Tokens are DERIVED, not invented
//!
//! §8's whole point is "every screen pulls from the same set rather than
//! literals at the call site" — so [`Tokens`] is never hand-tuned. Five of
//! its colours ([`Tokens::bg`], [`Tokens::surface`], [`Tokens::ink`],
//! [`Tokens::muted`], [`Tokens::accent`]) are copied straight from
//! [`crate::accessibility::Palette`], whose contrast ratios are already
//! measured and tested. [`Tokens::line`] and every `_soft` variant are
//! computed from those five plus three semantic base hues by the pure
//! functions below ([`mix`]) — never picked by eye.
//!
//! **The three semantic base hues (`ok`/`warn`/`error`) are the one
//! legitimate hand-chosen input.** `Palette`'s five colours have no
//! success/warning/error members to derive a hue from — inventing one by
//! rotating `accent` would produce whatever a rotation happens to land on
//! in each palette (and could land somewhere unreadable in
//! `HIGH_CONTRAST`), not a colour anyone chose to mean "warning". So
//! [`Semantics::DARK`]/[`Semantics::LIGHT`]/[`Semantics::HIGH_CONTRAST`]
//! are named constants, each independently measured against its own `bg`
//! and `surface` in `tests::semantics_clear_wcag_aa_on_both_surfaces`
//! below — at **AA** (4.5:1), not the AAA `Palette::pairs()` holds itself
//! to, per FRONTEND_UI §10's "meets WCAG 2.2 AA at minimum" for badge
//! text. Everything downstream of those three constants (the `_soft`
//! backgrounds, `line`) IS a pure function of them plus the palette, with
//! no further hand-tuning — that is what "derived, not invented" binds.
//!
//! Where a base hue happens to equal a literal this wave's code used to
//! hardcode (`DARK.ok` is exactly the `#40C060` `Check::Accepted` colour
//! elsewhere in `app.rs`), that is not a coincidence to preserve by
//! accident — it is this module picking values a reviewer has already
//! seen render correctly, rather than a fresh guess.
//!
//! ## Light is new
//!
//! [`crate::accessibility::Palette`] shipped only `DEFAULT` (dark) and
//! `HIGH_CONTRAST` — nothing in this app has ever offered a light theme.
//! §8's token table has a Light column regardless, and acceptance
//! criterion 3 asks for "light, dark and high-contrast sets all derived
//! from the same functions", so [`crate::accessibility::Palette::LIGHT`]
//! is added here to complete the set. It is not wired to a settings
//! toggle by this ticket — there is no user-facing way to select it yet,
//! same as before this ticket for `HIGH_CONTRAST`'s sibling states — but
//! it exists, is measured to the same AAA bar as `DEFAULT`/`HIGH_CONTRAST`
//! (`accessibility::tests`), and [`Tokens::light`] derives from it with
//! the exact same functions as the other two sets.
//!
//! ## Fonts
//!
//! One embedded family, two weights, both OFL: **IBM Plex Sans**
//! (Copyright 2017 IBM Corp., SIL Open Font License 1.1 — licence text at
//! `assets/fonts/IBMPlexSans-OFL.txt`, `NOTICE.md` beside it). Regular is
//! the body face; SemiBold is the display face for headings and badges.
//! IBM Plex was designed with a deliberately neutral, slightly technical
//! grotesque skeleton (IBM's own brief: "not too corporate, not too
//! playful") — a fit for a retro-hardware tool without reaching for a
//! pixel font, which would misrepresent this shell as part of the
//! emulated hardware rather than a tool observing it (law 6's honesty
//! principle, applied to typography). One family in two weights satisfies
//! "one display face... and one body face" without a second licence to
//! track; egui's bundled `Hack` stays the monospace face for code/hex
//! views, untouched.
//!
//! No network fetch: both `.ttf` files are embedded with `include_bytes!`
//! at compile time and registered into [`egui::FontDefinitions`] by
//! [`install_fonts`], called once from `RetroForgeApp::new`.

use eframe::egui;

use crate::accessibility::Palette;

/// Corner radii, theme-invariant (§8: "n/a" in the High-contrast column —
/// these do not change per palette, only colours do).
pub const RADIUS_SM: f32 = 4.0;
pub const RADIUS_MD: f32 = 8.0;

/// The 5-step spacing scale, theme-invariant. Chosen to exactly match
/// values already on screen before this ticket (`Self::apply_theme`'s
/// `item_spacing`/`button_padding`, `library_cards`' card
/// `inner_margin`/`CARD_SPACING`, `toast.rs`'s `add_space`) rather than a
/// "cleaner" scale — `tests/library_grid.rs` derives its column count from
/// `CARD_WIDTH + CARD_SPACING`, so silently moving that number would fail
/// a test with nothing to do with theming.
pub const SPACE: [f32; 5] = [2.0, 4.0, 6.0, 8.0, 10.0];

/// How long a modal's open/close fade takes (§8's motion note: "a short
/// fade on modal open and close"). `apply_theme`/the modal call sites
/// scale this to zero when `style.animation_time` is zero, per the "no
/// animation" accessibility escape hatch egui itself already offers.
pub const MODAL_FADE_SECS: f32 = 0.12;

/// The library selection ring's stroke width for a mouse/keyboard focus
/// (ticket W15-08, `docs/design/UX_WAVE_15.md` §9) — unchanged from the
/// literal `2.0` `library_rows`/`library_cards` drew before this ticket.
pub const FOCUS_RING_MOUSE: f32 = 2.0;
/// The same ring's width when the pad drove the selection there —
/// acceptance 2's "thicker... e.g. 4px vs 2px", chosen at exactly double
/// so the difference reads at a glance without needing a legend.
pub const FOCUS_RING_PAD: f32 = 4.0;
/// How far the pad ring's colour sits toward `ink` (the primary-text
/// token, the highest-contrast colour any palette defines) from `accent`
/// — acceptance 2's "stronger accent". Mixing TOWARD `ink` rather than
/// inventing a new hue keeps this derived (module doc: "derived, not
/// invented") and can only raise contrast against `bg`/`surface`, never
/// lower it below `accent`'s own — `ink` is chosen precisely because
/// every palette already measures it as the most-contrasting token there
/// is (`accessibility::tests::every_pair_in_every_palette_clears_aaa`).
const ACCENT_STRONG_MIX: f32 = 0.3;

/// The three semantic base hues a token set derives its `ok`/`warn`/
/// `error` (and their `_soft` variants) from. See the module doc for why
/// these three specifically are hand-chosen rather than derived.
struct Semantics {
    ok: [u8; 3],
    warn: [u8; 3],
    error: [u8; 3],
}

impl Semantics {
    /// Measured against `Palette::DEFAULT`'s `background`/`raised` in
    /// `tests::semantics_clear_wcag_aa_on_both_surfaces`. `ok` reuses the
    /// exact `#40C060` already on screen elsewhere in `app.rs`'s profile
    /// editor; `warn` reuses the exact `#E08030` amber used across the
    /// states modal, bindings conflict text and profile editor.
    const DARK: Semantics = Semantics {
        ok: [0x40, 0xC0, 0x60],
        warn: [0xE0, 0x80, 0x30],
        error: [0xEC, 0x70, 0x60],
    };

    /// Measured against `Palette::LIGHT`.
    const LIGHT: Semantics = Semantics {
        ok: [0x18, 0x66, 0x2A],
        warn: [0x8A, 0x5A, 0x00],
        error: [0xB3, 0x26, 0x1E],
    };

    /// Measured against `Palette::HIGH_CONTRAST`.
    const HIGH_CONTRAST: Semantics = Semantics {
        ok: [0x40, 0xE0, 0x70],
        warn: [0xFF, 0xC0, 0x30],
        error: [0xFF, 0x60, 0x50],
    };
}

fn col(c: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(c[0], c[1], c[2])
}

/// Mix `a` toward `b` by `t` (0 = `a`, 1 = `b`), in gamma space — the same
/// space every other blend in this crate already uses
/// (`Color32::lerp_to_gamma`, used by `apply_theme`'s hover/active states
/// before this ticket). A pure function of its three arguments: the same
/// inputs always produce the same output, which is what lets
/// `tests::derivation_is_pure` assert it directly.
#[must_use]
pub fn mix(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    a.lerp_to_gamma(b, t)
}

/// How far a `_soft` variant sits toward `bg` from its semantic colour.
/// Close to `bg` (soft is a background tint, not a second foreground
/// colour) but far enough to read as tinted rather than accidental.
const SOFT_MIX: f32 = 0.82;
/// How far `line` sits toward `bg` from `muted`. Higher than `SOFT_MIX`:
/// a divider must be barely there, whereas a soft badge background is
/// meant to be noticed.
const LINE_MIX: f32 = 0.72;

/// The token table from `docs/design/UX_WAVE_15.md` §8, exactly. Every
/// field here is either copied from a [`Palette`] or derived from one by
/// [`mix`] — see the module doc for the one exception (the three semantic
/// base hues).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub bg: egui::Color32,
    pub surface: egui::Color32,
    pub ink: egui::Color32,
    pub muted: egui::Color32,
    pub line: egui::Color32,
    pub accent: egui::Color32,
    pub accent_soft: egui::Color32,
    /// The pad focus ring's colour (ticket W15-08 acceptance 2): `accent`
    /// mixed toward `ink` by [`ACCENT_STRONG_MIX`] — see that constant's
    /// doc for why `ink` is the mix target.
    pub accent_strong: egui::Color32,
    pub ok: egui::Color32,
    pub ok_soft: egui::Color32,
    pub warn: egui::Color32,
    pub warn_soft: egui::Color32,
    pub error: egui::Color32,
    pub error_soft: egui::Color32,
    pub radius_sm: f32,
    pub radius_md: f32,
    pub space_1: f32,
    pub space_2: f32,
    pub space_3: f32,
    pub space_4: f32,
    pub space_5: f32,
}

/// The one place `Tokens` gets built. `palette` supplies `bg`/`surface`/
/// `ink`/`muted`/`accent` verbatim; `semantics` supplies the three
/// hand-chosen hues; everything else (`line`, every `_soft`) is `mix`
/// applied to those inputs. Called by [`Tokens::light`]/[`Tokens::dark`]/
/// [`Tokens::high_contrast`] with the three matched (palette, semantics)
/// pairs — never called with a mismatched pair outside `tests`, which
/// deliberately does that once to prove the derivation is a real function
/// of its inputs and not a memoized constant.
fn derive(palette: &Palette, semantics: &Semantics) -> Tokens {
    let bg = col(palette.background);
    let surface = col(palette.raised);
    let ink = col(palette.text);
    let muted = col(palette.text_muted);
    let accent = col(palette.accent);
    let ok = col(semantics.ok);
    let warn = col(semantics.warn);
    let error = col(semantics.error);
    Tokens {
        bg,
        surface,
        ink,
        muted,
        line: mix(muted, bg, LINE_MIX),
        accent,
        accent_soft: mix(accent, bg, SOFT_MIX),
        accent_strong: mix(accent, ink, ACCENT_STRONG_MIX),
        ok,
        ok_soft: mix(ok, bg, SOFT_MIX),
        warn,
        warn_soft: mix(warn, bg, SOFT_MIX),
        error,
        error_soft: mix(error, bg, SOFT_MIX),
        radius_sm: RADIUS_SM,
        radius_md: RADIUS_MD,
        space_1: SPACE[0],
        space_2: SPACE[1],
        space_3: SPACE[2],
        space_4: SPACE[3],
        space_5: SPACE[4],
    }
}

impl Tokens {
    /// Derived from `Palette::LIGHT`. Selected by Settings › Accessibility
    /// › Theme: Light (ticket W20-08).
    #[must_use]
    pub fn light() -> Tokens {
        derive(&Palette::LIGHT, &Semantics::LIGHT)
    }

    /// Derived from `Palette::DEFAULT` — the theme every fresh install
    /// boots into (law 6: Accuracy Mode is the reference, and a fresh
    /// install's *shell* chrome is this, unconditionally).
    #[must_use]
    pub fn dark() -> Tokens {
        derive(&Palette::DEFAULT, &Semantics::DARK)
    }

    /// Derived from `Palette::HIGH_CONTRAST`.
    #[must_use]
    pub fn high_contrast() -> Tokens {
        derive(&Palette::HIGH_CONTRAST, &Semantics::HIGH_CONTRAST)
    }

    /// The set `apply_theme` and every W15 UI surface actually draws
    /// with, chosen the same way [`crate::accessibility::AccessibilitySettings::palette`]
    /// already chooses between `DEFAULT` and `HIGH_CONTRAST` — this is
    /// that same choice, expressed as a `Tokens` set instead of a raw
    /// `Palette`.
    #[must_use]
    pub fn from_accessibility(a: &crate::accessibility::AccessibilitySettings) -> Tokens {
        let a = a.normalized();
        if a.high_contrast {
            Tokens::high_contrast()
        } else if a.theme == crate::accessibility::ThemeChoice::Light {
            Tokens::light()
        } else {
            Tokens::dark()
        }
    }

    /// The modal backdrop tint (§8's "the modal backdrop tint" in
    /// `apply_theme`'s acceptance wording): `bg`, translucent, rather than
    /// egui's own default `Color32::from_black_alpha(100)` — so a
    /// high-contrast user sees a backdrop consistent with the rest of the
    /// theme instead of a fixed grey regardless of palette.
    #[must_use]
    pub fn modal_backdrop(&self) -> egui::Color32 {
        egui::Color32::from_rgba_unmultiplied(self.bg.r(), self.bg.g(), self.bg.b(), 140)
    }
}

/// Whether `identity` is `console`, allowing for the case egui strips
/// generics oddly — kept as a tiny helper so [`console_tint`]'s match
/// arms stay one line each.
fn is_console(identity: &crate::library::EntryIdentity, console: crate::library::Console) -> bool {
    matches!(
        identity,
        crate::library::EntryIdentity::Recognized { console: c, .. } if *c == console
    )
}

/// NES/SNES card-placeholder tints (`library_cards`' "generic,
/// console-tinted placeholder", ticket W15-05 acceptance 4). Not part of
/// the §8 table — a decorative per-console hue is not a semantic
/// token — so they live here as named constants instead of literals at
/// the call site, with the mix-toward-`surface` step made a pure,
/// testable function instead of inline arithmetic.
const NES_TINT: egui::Color32 = egui::Color32::from_rgb(120, 70, 40);
const SNES_TINT: egui::Color32 = egui::Color32::from_rgb(90, 60, 130);
/// How far a console tint sits toward `surface` — unchanged from the
/// literal `0.35` this replaces.
const CONSOLE_TINT_MIX: f32 = 0.35;

/// The console-tinted placeholder colour for a library entry.
///
/// `high_contrast` flattens both consoles to plain `surface`: a
/// decorative brown/purple distinction is not a WCAG-measured pair, and
/// high-contrast mode's whole point is drawing with only the palette that
/// has been measured.
#[must_use]
pub fn console_tint(
    tokens: &Tokens,
    high_contrast: bool,
    identity: &crate::library::EntryIdentity,
) -> egui::Color32 {
    if high_contrast {
        return tokens.surface;
    }
    if is_console(identity, crate::library::Console::Nes) {
        mix(NES_TINT, tokens.surface, CONSOLE_TINT_MIX)
    } else if is_console(identity, crate::library::Console::Snes) {
        mix(SNES_TINT, tokens.surface, CONSOLE_TINT_MIX)
    } else {
        tokens.surface
    }
}

/// Ticket W20-20: a library card's console spine — the console colour at
/// full strength, as a 4-px stripe down the card's left edge. `None` in
/// high contrast (colour-coding is exactly what that palette avoids) and
/// for an unrecognized ROM.
#[must_use]
pub fn console_spine(
    high_contrast: bool,
    identity: &crate::library::EntryIdentity,
) -> Option<egui::Color32> {
    if high_contrast {
        None
    } else if is_console(identity, crate::library::Console::Nes) {
        Some(NES_TINT)
    } else if is_console(identity, crate::library::Console::Snes) {
        Some(SNES_TINT)
    } else {
        None
    }
}

/// A modal's current fade alpha, keyed by `id` (pass a stable, modal-
/// specific `egui::Id` — the three call sites in `app.rs` each use their
/// own).
///
/// Reads `target_open` every frame rather than caching it, and keeps
/// reporting a decaying alpha for [`MODAL_FADE_SECS`] after `target_open`
/// goes false — that is what lets a modal fade OUT: the caller keeps
/// rendering (with cached data — see `app.rs`'s modal functions) for as
/// long as this returns a positive alpha, not just while its own
/// `Option` is `Some`.
///
/// Respects `ctx.global_style().animation_time`: egui's own accessibility escape
/// hatch for "no animation" is setting that to zero, and honouring it
/// here means a user who disabled animation also gets an instant modal
/// rather than one that still spends 120ms fading despite the setting.
#[must_use]
pub fn modal_fade_alpha(ctx: &egui::Context, id: egui::Id, target_open: bool) -> f32 {
    let duration = if ctx.global_style().animation_time <= 0.0 {
        0.0
    } else {
        MODAL_FADE_SECS
    };
    ctx.animate_bool_with_time(id, target_open, duration)
}

/// Embed and register IBM Plex Sans (module doc) into `ctx`'s fonts.
/// Called once from `RetroForgeApp::new`. `FontFamily::Name("display")`
/// is new; `FontFamily::Proportional` is redirected to the body face
/// instead of egui's bundled `Ubuntu-Light` (whose Ubuntu Font Licence is
/// NOT OFL/Apache — NFR-011 — so it cannot be what body text actually
/// renders in, only a fallback egui keeps internally).
/// `FontFamily::Monospace` is left alone (module doc).
pub fn install_fonts(ctx: &egui::Context) {
    const BODY: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf");
    const DISPLAY: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf");

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "ibm_plex_sans_body".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(BODY)),
    );
    fonts.font_data.insert(
        "ibm_plex_sans_display".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(DISPLAY)),
    );

    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "ibm_plex_sans_body".to_owned());
    fonts.families.insert(
        egui::FontFamily::Name("display".into()),
        vec![
            "ibm_plex_sans_display".to_owned(),
            "ibm_plex_sans_body".to_owned(),
        ],
    );

    // Ticket W20-05: the Phosphor icon font, as a fallback right after the
    // body face so every symbol resolves to an icon instead of a tofu box
    // (`crate::icons`). `add_to_fonts` inserts at index 1 of Proportional
    // (egui-phosphor-0.13.0 src/lib.rs), i.e. after the Plex body face
    // inserted at 0 above. Fill gets its own named family for the icons
    // whose on-state is drawn filled; it shares Regular's codepoints, so
    // it is never a proportional fallback (it would shadow Regular).
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    fonts.font_data.insert(
        crate::icons::FILL_FAMILY.to_owned(),
        std::sync::Arc::new(egui_phosphor::Variant::Fill.font_data()),
    );
    fonts.families.insert(
        egui::FontFamily::Name(crate::icons::FILL_FAMILY.into()),
        vec![
            crate::icons::FILL_FAMILY.to_owned(),
            "ibm_plex_sans_body".to_owned(),
        ],
    );

    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::{contrast_ratio, WCAG_AA};

    /// `derive` is a pure function of its two arguments: same inputs, same
    /// output, every time — and different palettes produce different
    /// tokens, so this is not vacuously true of a function that ignores
    /// its arguments.
    #[test]
    fn derivation_is_pure() {
        let a = derive(&Palette::DEFAULT, &Semantics::DARK);
        let b = derive(&Palette::DEFAULT, &Semantics::DARK);
        assert_eq!(a, b, "same inputs must produce the same tokens");

        let light = derive(&Palette::LIGHT, &Semantics::LIGHT);
        assert_ne!(
            a, light,
            "a different base palette must produce different tokens"
        );
    }

    /// The three named sets actually differ from each other — a "light,
    /// dark and high-contrast set" that collapsed to one set would pass
    /// every other test here vacuously.
    #[test]
    fn the_three_sets_differ() {
        let light = Tokens::light();
        let dark = Tokens::dark();
        let hc = Tokens::high_contrast();
        assert_ne!(light, dark);
        assert_ne!(dark, hc);
        assert_ne!(light, hc);
    }

    /// Every `_soft` variant's luminance lies between its semantic
    /// colour's and `bg`'s — proof that `_soft` is actually a mix, not an
    /// independently chosen colour that happens to look tinted.
    #[test]
    fn soft_variants_lie_between_semantic_and_bg() {
        use crate::accessibility::relative_luminance;
        for tokens in [Tokens::light(), Tokens::dark(), Tokens::high_contrast()] {
            for (semantic, soft) in [
                (tokens.accent, tokens.accent_soft),
                (tokens.ok, tokens.ok_soft),
                (tokens.warn, tokens.warn_soft),
                (tokens.error, tokens.error_soft),
            ] {
                let l_semantic = relative_luminance([semantic.r(), semantic.g(), semantic.b()]);
                let l_bg = relative_luminance([tokens.bg.r(), tokens.bg.g(), tokens.bg.b()]);
                let l_soft = relative_luminance([soft.r(), soft.g(), soft.b()]);
                let (lo, hi) = if l_semantic <= l_bg {
                    (l_semantic, l_bg)
                } else {
                    (l_bg, l_semantic)
                };
                assert!(
                    l_soft >= lo - f32::EPSILON && l_soft <= hi + f32::EPSILON,
                    "soft luminance {l_soft} not between semantic {l_semantic} and bg {l_bg}"
                );
            }
        }
    }

    /// `line` sits strictly between `muted` and `bg` too, and is
    /// distinguishable from `bg` — an invisible divider is not a divider.
    #[test]
    fn line_is_between_muted_and_bg_and_visible_against_bg() {
        use crate::accessibility::relative_luminance;
        for tokens in [Tokens::light(), Tokens::dark(), Tokens::high_contrast()] {
            let l_muted =
                relative_luminance([tokens.muted.r(), tokens.muted.g(), tokens.muted.b()]);
            let l_bg = relative_luminance([tokens.bg.r(), tokens.bg.g(), tokens.bg.b()]);
            let l_line = relative_luminance([tokens.line.r(), tokens.line.g(), tokens.line.b()]);
            let (lo, hi) = if l_muted <= l_bg {
                (l_muted, l_bg)
            } else {
                (l_bg, l_muted)
            };
            assert!(l_line >= lo - f32::EPSILON && l_line <= hi + f32::EPSILON);
            assert_ne!(
                tokens.line, tokens.bg,
                "a line identical to bg is not a line"
            );
        }
    }

    /// FRONTEND_UI §10: "Text and badge contrast meets WCAG 2.2 AA at
    /// minimum" — checked here for the three semantic hues against both
    /// surfaces they are drawn on, in all three sets. Deliberately AA
    /// (4.5:1), not the AAA `accessibility::Palette::pairs()` holds
    /// itself to — see the module doc for why these are a separate,
    /// lower-committed bar rather than new `Palette` fields.
    #[test]
    fn semantics_clear_wcag_aa_on_both_surfaces() {
        for (name, tokens) in [
            ("LIGHT", Tokens::light()),
            ("DARK", Tokens::dark()),
            ("HIGH_CONTRAST", Tokens::high_contrast()),
        ] {
            for (semantic_name, semantic) in [
                ("ok", tokens.ok),
                ("warn", tokens.warn),
                ("error", tokens.error),
            ] {
                for (surface_name, surface) in [("bg", tokens.bg), ("surface", tokens.surface)] {
                    let ratio = contrast_ratio(
                        [semantic.r(), semantic.g(), semantic.b()],
                        [surface.r(), surface.g(), surface.b()],
                    );
                    assert!(
                        ratio >= WCAG_AA,
                        "{name}: {semantic_name} on {surface_name} is {ratio:.2}:1, below AA"
                    );
                }
            }
        }
    }

    /// WCAG 2.2 SC 1.4.11 (non-text contrast, 3:1) for `accent` against
    /// both surfaces, in all three sets — the token-level version of
    /// `accessibility::tests::the_library_focus_ring_clears_wcag22_non_text_contrast_in_both_palettes`,
    /// extended to `LIGHT` since that test only knows about `Palette`, not
    /// `Tokens`.
    #[test]
    fn accent_clears_non_text_contrast_on_both_surfaces_in_all_sets() {
        const WCAG_NON_TEXT: f32 = 3.0;
        for (name, tokens) in [
            ("LIGHT", Tokens::light()),
            ("DARK", Tokens::dark()),
            ("HIGH_CONTRAST", Tokens::high_contrast()),
        ] {
            for (surface_name, surface) in [("bg", tokens.bg), ("surface", tokens.surface)] {
                let ratio = contrast_ratio(
                    [tokens.accent.r(), tokens.accent.g(), tokens.accent.b()],
                    [surface.r(), surface.g(), surface.b()],
                );
                assert!(
                    ratio >= WCAG_NON_TEXT,
                    "{name}: accent on {surface_name} is {ratio:.2}:1, below 3:1"
                );
            }
        }
    }

    /// The pad focus ring's colour (ticket W15-08 acceptance 2) clears the
    /// same WCAG 2.2 SC 1.4.11 floor the mouse ring's `accent` does — "the
    /// same 3:1 contrast tests as the mouse ring" is the ticket's own
    /// wording, so this is that test, re-run against `accent_strong`.
    #[test]
    fn accent_strong_clears_non_text_contrast_on_both_surfaces_in_all_sets() {
        const WCAG_NON_TEXT: f32 = 3.0;
        for (name, tokens) in [
            ("LIGHT", Tokens::light()),
            ("DARK", Tokens::dark()),
            ("HIGH_CONTRAST", Tokens::high_contrast()),
        ] {
            for (surface_name, surface) in [("bg", tokens.bg), ("surface", tokens.surface)] {
                let ratio = contrast_ratio(
                    [
                        tokens.accent_strong.r(),
                        tokens.accent_strong.g(),
                        tokens.accent_strong.b(),
                    ],
                    [surface.r(), surface.g(), surface.b()],
                );
                assert!(
                    ratio >= WCAG_NON_TEXT,
                    "{name}: accent_strong on {surface_name} is {ratio:.2}:1, below 3:1"
                );
            }
        }
    }

    /// `accent_strong` is actually a mix TOWARD `ink`, not `accent` itself
    /// relabelled — a "stronger accent" that happened to equal the
    /// ordinary one would make acceptance 2's visual distinction a no-op.
    #[test]
    fn accent_strong_differs_from_accent_and_sits_toward_ink() {
        use crate::accessibility::relative_luminance;
        for tokens in [Tokens::light(), Tokens::dark(), Tokens::high_contrast()] {
            assert_ne!(
                tokens.accent, tokens.accent_strong,
                "accent_strong must be visually distinct from accent"
            );
            let l_accent =
                relative_luminance([tokens.accent.r(), tokens.accent.g(), tokens.accent.b()]);
            let l_ink = relative_luminance([tokens.ink.r(), tokens.ink.g(), tokens.ink.b()]);
            let l_strong = relative_luminance([
                tokens.accent_strong.r(),
                tokens.accent_strong.g(),
                tokens.accent_strong.b(),
            ]);
            let (lo, hi) = if l_accent <= l_ink {
                (l_accent, l_ink)
            } else {
                (l_ink, l_accent)
            };
            assert!(
                l_strong >= lo - f32::EPSILON && l_strong <= hi + f32::EPSILON,
                "accent_strong luminance {l_strong} not between accent {l_accent} and ink {l_ink}"
            );
        }
    }

    /// The high-contrast token set is measurably better than dark's, the
    /// same anti-vacuity guard `accessibility::tests` runs on `Palette`
    /// itself — re-run here because `Tokens::high_contrast` is a second,
    /// independent derivation path that could regress separately from
    /// `Palette::HIGH_CONTRAST`.
    #[test]
    fn high_contrast_tokens_are_measurably_higher_contrast() {
        use crate::accessibility::relative_luminance;
        let dark = Tokens::dark();
        let hc = Tokens::high_contrast();
        let dark_ratio = contrast_ratio(
            [dark.ink.r(), dark.ink.g(), dark.ink.b()],
            [dark.bg.r(), dark.bg.g(), dark.bg.b()],
        );
        let hc_ratio = contrast_ratio(
            [hc.ink.r(), hc.ink.g(), hc.ink.b()],
            [hc.bg.r(), hc.bg.g(), hc.bg.b()],
        );
        assert!(hc_ratio > dark_ratio);
        // Vacuity guard for the luminance helper import above.
        assert!(relative_luminance([hc.bg.r(), hc.bg.g(), hc.bg.b()]) < 0.01);
    }

    /// `console_tint` flattens to `surface` under high contrast, and gives
    /// NES/SNES distinguishable, non-`surface` colours otherwise.
    #[test]
    fn console_tint_flattens_under_high_contrast() {
        let tokens = Tokens::high_contrast();
        let nes = crate::library::EntryIdentity::Recognized {
            console: crate::library::Console::Nes,
            normalized_sha256: "a".repeat(64),
        };
        assert_eq!(console_tint(&tokens, true, &nes), tokens.surface);

        let tokens = Tokens::dark();
        let snes = crate::library::EntryIdentity::Recognized {
            console: crate::library::Console::Snes,
            normalized_sha256: "b".repeat(64),
        };
        let nes_tint = console_tint(&tokens, false, &nes);
        let snes_tint = console_tint(&tokens, false, &snes);
        assert_ne!(nes_tint, tokens.surface);
        assert_ne!(snes_tint, tokens.surface);
        assert_ne!(nes_tint, snes_tint);
    }

    /// `modal_fade_alpha` reaches (near) full opacity once open and decays
    /// toward zero once closed, and collapses to an instant step when
    /// `animation_time` is zero.
    #[test]
    fn modal_fade_alpha_opens_and_respects_zero_animation_time() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("test_modal_fade");
        let mut alpha = 0.0;
        for _ in 0..30 {
            ctx.begin_pass(egui::RawInput {
                predicted_dt: 1.0 / 60.0,
                ..Default::default()
            });
            alpha = modal_fade_alpha(&ctx, id, true);
            let _ = ctx.end_pass();
        }
        assert!(
            alpha > 0.9,
            "expected near-full alpha once open, got {alpha}"
        );

        ctx.all_styles_mut(|s| s.animation_time = 0.0);
        let id2 = egui::Id::new("test_modal_fade_instant");
        ctx.begin_pass(egui::RawInput::default());
        let instant = modal_fade_alpha(&ctx, id2, true);
        let _ = ctx.end_pass();
        assert!(
            instant > 0.99,
            "animation_time == 0 must make the fade instant, got {instant}"
        );
    }
}
