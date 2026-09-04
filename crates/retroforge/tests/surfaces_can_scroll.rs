//! **Every surface that can overflow can be scrolled** (ticket W10-04).
//!
//! ## Why this is a source lint and not a runtime check
//!
//! W10-04's third criterion asked for a check that would catch a NEW
//! unscrollable surface, *or a written statement of why none is
//! practical*. A runtime check is not practical, and the reason is
//! specific rather than a shrug:
//!
//! - The **accessibility tree cannot tell clipped from scrolled.** egui
//!   reports laid-out rects for `ScrollArea` children even when they are
//!   scrolled out of view, so a probe on the Controls window reported 31
//!   "clipped" nodes that were every one of them reachable.
//! - The **rendered frame cannot either.** `tests/renders.rs` sees that
//!   content stops; it cannot see whether the rest is reachable. That is
//!   exactly how this shipped: I reviewed a frame of the Enhance
//!   workspace whose profile inspector was cut off mid-list and saw
//!   nothing wrong, because egui's default `floating` scrollbar had not
//!   drawn itself.
//!
//! What IS checkable is the thing that actually went wrong: a window or
//! dock pane was written without a scroll area and nobody noticed. So
//! this reads the source, finds every surface function, and requires
//! each to contain a `ScrollArea` **or** be named in [`BOUNDED`] with a
//! reason. Adding a surface without either turns it red.
//!
//! It is a lint, and it is honest about being one: it cannot prove a
//! `ScrollArea` is wrapped around the *right* content. It catches the
//! absence, which is the failure that has actually occurred twice.

/// Surfaces deliberately without a scroll area, and why.
///
/// A surface belongs here only when its content **cannot** exceed its
/// container — a fixed-size image, a fixed row count — not when it
/// merely happens to fit today.
const BOUNDED: &[(&str, &str)] = &[
    (
        "debug_panels_window",
        "hosts the dock area and draws nothing itself; each pane scrolls on its own",
    ),
    (
        "enhance_window",
        "hosts the §3.3 dock area and draws nothing itself; each tab scrolls on its own \
         (crate::enhance_dock::scrolled)",
    ),
    (
        "overlay_menu",
        "a fixed list of six buttons, and it is anchored centre-centre rather than sized to \
         content",
    ),
    (
        "layers_debug_window",
        "two images drawn with shrink_to_fit, which cannot exceed the pane by construction",
    ),
    ("pattern_ui", "a fixed 128x128 pattern table image"),
    (
        "palette_ui",
        "32 palette entries in a fixed 8x4 arrangement",
    ),
    (
        "memory_rows_ui",
        "a helper called from memory_ui, which has the scroll area",
    ),
    (
        "tab_picker_ui",
        "one menu button; the menu it opens lists at most DebugTab::ALL, which is 11 entries          and is pinned by a test in rf_debugger::layout",
    ),
    (
        "annotation_form_ui",
        "a fixed grid — eight labelled fields and a button row — plus the notes editor, which          scrolls inside itself; a helper called from annotations_ui, which has the scroll area",
    ),
    (
        "snes_pattern_ui",
        "a fixed 128x128 tile page over a BG selector of at most four buttons — the same reason \
         pattern_ui is here, and the page size is a constant rather than a function of the ROM",
    ),
    (
        "snes_palette_ui",
        "CGRAM's 256 entries in a fixed 16x16 arrangement plus one colour-math line; the grid \
         cannot grow, because the hardware has exactly 256 entries",
    ),
    (
        "watchpoint_ui",
        "a collapsing header over a fixed control set plus one row per armed watch, and the core          holds at most rf_core_api::MAX_WATCHES of them; a helper called from annotations_ui,          which has the scroll area",
    ),
    (
        "annotation_import_ui",
        "a collapsing header over a fixed control set whose only growable child is a          TextEdit::multiline, which scrolls inside itself; a helper called from annotations_ui,          which has the scroll area",
    ),
    (
        "event_timeline_ui",
        "allocate_exact_size of row_height * TIMELINE_ROWS.len() — a compile-time row count",
    ),
    (
        "audio_ui",
        "one row per rf_nes::apu::CHANNEL_NAMES entry, a fixed-length array",
    ),
    (
        "map_ui",
        "images drawn with shrink_to_fit; a scroll area would make available_size unbounded \
         and grow them without limit instead of fitting them",
    ),
];

/// Extract `fn <name>(...) { .. }` bodies by brace matching.
fn function_bodies(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut idx = 0;
    while let Some(rel) = src[idx..].find("fn ") {
        let start = idx + rel;
        idx = start + 3;
        let Some(paren) = src[start..].find('(') else {
            break;
        };
        let name: String = src[start + 3..start + paren].trim().to_string();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let Some(open_rel) = src[start..].find('{') else {
            break;
        };
        let open = start + open_rel;
        let (mut depth, mut i) = (0usize, open);
        while i < bytes.len() {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        out.push((name, src[open..i.min(bytes.len())].to_string()));
    }
    out
}

/// A surface is a window body or a dock-pane body.
fn is_surface(name: &str, body: &str) -> bool {
    (name.ends_with("_window") || name.ends_with("_modal") || name.ends_with("_ui"))
        && (body.contains("egui::Window::new") || name.ends_with("_ui"))
}

#[test]
fn every_surface_either_scrolls_or_says_why_not() {
    let app = include_str!("../src/app.rs");
    let dock = include_str!("../src/debug_dock.rs");
    let enhance = include_str!("../src/enhance_dock.rs");

    let mut offenders = Vec::new();
    let mut checked = 0usize;
    for (file, src) in [
        ("app.rs", app),
        ("debug_dock.rs", dock),
        ("enhance_dock.rs", enhance),
    ] {
        for (name, body) in function_bodies(src) {
            if !is_surface(&name, &body) {
                continue;
            }
            checked += 1;
            let scrolls = body.contains("ScrollArea") || body.contains("scrolled(");
            let excused = BOUNDED.iter().any(|(n, _)| *n == name);
            if !scrolls && !excused {
                offenders.push(format!("{file}::{name}"));
            }
        }
    }

    assert!(
        checked >= 12,
        "only {checked} surfaces were found — the extractor is not matching, so this test \
         would pass on a codebase full of unscrollable panes"
    );
    assert!(
        offenders.is_empty(),
        "these surfaces can overflow with no way to scroll, and are not listed as bounded:\n  \
         {}\n\nAdd a ScrollArea, or add the surface to BOUNDED with the reason its content \
         cannot exceed its container. \"It fits today\" is not that reason.",
        offenders.join("\n  ")
    );
}

/// **The excuse list cannot rot.** A name in [`BOUNDED`] that no longer
/// exists is a stale excuse, and the next surface to take that name
/// would inherit it silently.
#[test]
fn every_bounded_exception_still_names_a_real_surface() {
    let all: String = [
        include_str!("../src/app.rs"),
        include_str!("../src/debug_dock.rs"),
        include_str!("../src/enhance_dock.rs"),
    ]
    .concat();
    let names: Vec<String> = function_bodies(&all).into_iter().map(|(n, _)| n).collect();
    for (name, why) in BOUNDED {
        assert!(
            names.iter().any(|n| n == name),
            "BOUNDED lists `{name}` ({why}) but no such function exists any more — a stale \
             excuse that the next surface to take this name would inherit"
        );
    }
}
