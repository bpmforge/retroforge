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

// ---------------------------------------------------------------------
// Ticket W20-05 (`docs/design/UX_WAVE_20.md` §4 principle 10): every
// character the shell's SOURCE draws, not a hand-kept list.
//
// The two vocabularies above are only as good as whoever remembers to
// update them, and they were not updated: the grid/list toggle shipped
// `▦`/`≡` as tofu (visible in target/ui-renders/home-populated.png) while
// both tests stayed green, because neither character was on a list. This
// scans `src/**/*.rs` for every non-ASCII character inside a string or
// char literal (comments and doc comments are skipped — they are never
// drawn) and asks the fonts the APP installs (`theme::install_fonts`,
// not egui's defaults) for each one.
// ---------------------------------------------------------------------

/// Every non-ASCII char appearing inside string/char literals of `src`,
/// as written or as a `\u{..}` escape, with the first file:line it was
/// seen at.
fn literal_chars(src: &str, file: &str, out: &mut std::collections::BTreeMap<char, String>) {
    let chars: Vec<char> = src.chars().collect();
    let mut line = 1usize;
    let mut i = 0usize;
    let mut note = |c: char, line: usize, out: &mut std::collections::BTreeMap<char, String>| {
        if !c.is_ascii() {
            out.entry(c).or_insert_with(|| format!("{file}:{line}"));
        }
    };
    // Every arm below advances `i` by at least one before looping
    // (CLAUDE.md law 8): the scan cannot stall.
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
        } else if c == 'r' && (next == Some('"') || next == Some('#')) && !prev_is_ident(&chars, i)
        {
            // Raw string r"..." / r#"..."#.
            let mut j = i + 1;
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) != Some(&'"') {
                i += 1;
                continue;
            }
            j += 1;
            loop {
                match chars.get(j) {
                    None => break,
                    Some('"') if (0..hashes).all(|h| chars.get(j + 1 + h) == Some(&'#')) => {
                        j += 1 + hashes;
                        break;
                    }
                    Some(&ch) => {
                        if ch == '\n' {
                            line += 1;
                        }
                        note(ch, line, out);
                        j += 1;
                    }
                }
            }
            i = j;
        } else if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += escape(&chars, i, line, &mut note, out);
                    continue;
                }
                if chars[i] == '\n' {
                    line += 1;
                }
                note(chars[i], line, out);
                i += 1;
            }
            i += 1;
        } else if c == '\'' {
            // Char literal ('x', '\n', '\u{..}') vs lifetime ('a).
            if next == Some('\\') {
                let used = escape(&chars, i + 1, line, &mut note, out);
                i += 1 + used + 1;
            } else if chars.get(i + 2) == Some(&'\'') {
                note(next.unwrap_or(' '), line, out);
                i += 3;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
}

fn prev_is_ident(chars: &[char], i: usize) -> bool {
    i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')
}

/// Consume the escape at `chars[i] == '\\'`, recording a `\u{..}` char;
/// returns how many chars it used (always >= 2, so callers advance).
fn escape(
    chars: &[char],
    i: usize,
    line: usize,
    note: &mut impl FnMut(char, usize, &mut std::collections::BTreeMap<char, String>),
    out: &mut std::collections::BTreeMap<char, String>,
) -> usize {
    if chars.get(i + 1) == Some(&'u') && chars.get(i + 2) == Some(&'{') {
        let mut j = i + 3;
        let mut hex = String::new();
        while let Some(&h) = chars.get(j) {
            j += 1;
            if h == '}' {
                break;
            }
            hex.push(h);
        }
        if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
            note(c, line, out);
        }
        return j - i;
    }
    2
}

fn drawn_chars() -> std::collections::BTreeMap<char, String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("read source");
                // Unit-test modules are never drawn (art.rs tests a
                // Japanese title string, for instance): stop at the first
                // `#[cfg(test)] mod`, which this crate always puts last.
                let text = text
                    .find("\n#[cfg(test)]\nmod ")
                    .map_or(text.as_str(), |at| &text[..at]);
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                literal_chars(text, &rel, &mut out);
            }
        }
    }
    out
}

/// **The scan is not vacuous.** It must see characters this file knows
/// the UI draws — the `…` used in every "Settings…" label and the `·`
/// separator — and must NOT see one that appears only in a comment.
#[test]
fn the_source_scan_finds_drawn_characters_and_ignores_comments() {
    let found = drawn_chars();
    assert!(
        found.contains_key(&'\u{2026}'),
        "scan missed `…`: {found:?}"
    );
    assert!(found.contains_key(&'\u{b7}'), "scan missed `·`: {found:?}");
    let mut probe = std::collections::BTreeMap::new();
    literal_chars(
        "// ◆ in a comment\nlet s = \"x\"; /* ◇ */ let t = '\\u{2605}';",
        "probe",
        &mut probe,
    );
    assert_eq!(probe.keys().copied().collect::<Vec<_>>(), vec!['\u{2605}']);
}

/// Every non-ASCII character in a string literal anywhere in the shell
/// exists in the fonts the app installs — proportional family, 13 pt.
#[test]
fn every_character_the_source_draws_exists_in_the_app_fonts() {
    let mut harness = Harness::new_ui(|_ui| {});
    retroforge::theme::install_fonts(&harness.ctx);
    harness.run();
    let font = egui::FontId::proportional(13.0);
    let missing: Vec<String> = drawn_chars()
        .into_iter()
        .filter(|(c, _)| !harness.ctx.fonts_mut(|f| f.has_glyph(&font, *c)))
        .map(|(c, at)| format!("U+{:04X} {c:?} (first at {at})", c as u32))
        .collect();
    assert!(
        missing.is_empty(),
        "these characters render as tofu boxes — use an egui_phosphor icon instead:\n  {}",
        missing.join("\n  ")
    );
}
