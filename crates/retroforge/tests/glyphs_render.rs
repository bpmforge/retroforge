//! **Every glyph the HUD draws exists in the font it is drawn with**
//! (ticket W10-01 polish pass).
//!
//! ## Why this is a test and not a code review
//!
//! A missing glyph does not fail, warn, or log. egui draws a tofu box
//! and carries on, so the defect is invisible to the compiler, to
//! clippy, to the accessibility tree — a tofu box has the *correct*
//! label — and to every assertion in this repo. It is visible in a
//! screenshot and nowhere else.
//!
//! This project shipped `◆` that way in the profile chip. The "fix" was
//! `◇`, which is **also absent**, and that shipped too. The healthy A/V
//! dot was `●`, absent as well, and no screenshot could have caught it
//! because it only renders when an audio device is open. Three tofu
//! boxes, two of them introduced while fixing the first.
//!
//! egui exposes `Fonts::has_glyph`, so the font can simply be asked.
//!
//! ## The monospace finding
//!
//! The bundled monospace face has **no** `·`, `…` or `—`. The
//! frame/scanline readout rendered `f12 · sl34` in monospace, which is a
//! tofu box between every frame number — which is why the two families
//! are checked separately rather than against one combined set.

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{MONOSPACE_GLYPHS, PROPORTIONAL_GLYPHS};

/// The sizes `app::apply_theme` actually installs.
const PROPORTIONAL: [f32; 3] = [17.0, 13.0, 11.0];
const MONOSPACE: f32 = 11.5;

fn assert_all_present(vocabulary: &str, font: &egui::FontId, family: &str) {
    let mut harness = Harness::new_ui(|_ui| {});
    harness.run();
    for c in vocabulary.chars() {
        let present = harness.ctx.fonts_mut(|f| f.has_glyph(font, c));
        assert!(
            present,
            "U+{:04X} {c:?} is NOT in the bundled {family} font at {}pt — it renders as a \
             tofu box, silently. Pick a character the font has, or drop it; do not assume \
             a neighbour in the same Unicode block is present, because that is exactly how \
             `◆` was replaced by the equally-absent `◇`.",
            c as u32, font.size,
        );
    }
}

/// Every proportional glyph, at every size the theme installs — a glyph
/// is either in the face or it is not, but checking each size costs
/// nothing and documents which sizes exist.
#[test]
fn every_proportional_glyph_the_ui_draws_exists_in_the_font() {
    for size in PROPORTIONAL {
        assert_all_present(
            PROPORTIONAL_GLYPHS,
            &egui::FontId::proportional(size),
            "proportional",
        );
    }
}

/// The monospace vocabulary, which is currently empty — deliberately.
#[test]
fn every_monospace_glyph_the_ui_draws_exists_in_the_font() {
    assert_all_present(
        MONOSPACE_GLYPHS,
        &egui::FontId::monospace(MONOSPACE),
        "monospace",
    );
}

/// **The check can fail.** Without this the two tests above pass on an
/// empty vocabulary, and `MONOSPACE_GLYPHS` *is* empty — so one of them
/// is vacuous by construction and the guard is not optional.
///
/// `◆` is the character that shipped as a tofu box. If this ever starts
/// reporting it as present, the bundled font changed and the vocabulary
/// above should be revisited rather than trusted.
#[test]
fn the_glyph_check_actually_detects_a_missing_glyph() {
    let mut harness = Harness::new_ui(|_ui| {});
    harness.run();
    let font = egui::FontId::proportional(13.0);
    let present = harness.ctx.fonts_mut(|f| f.has_glyph(&font, '\u{25c6}'));
    assert!(
        !present,
        "U+25C6 is now present in the bundled font. That is not a failure of the app, but \
         it means this file's absent-glyph list is stale — re-probe before trusting it."
    );
}
