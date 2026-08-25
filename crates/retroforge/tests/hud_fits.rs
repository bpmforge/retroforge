//! **The HUD fits the window the application actually opens** (ticket
//! W10-01).
//!
//! ## Why this file exists
//!
//! On 2026-08-25 the bottom bar needed **1539 px** of content and
//! `main.rs` opened the window at **768 px**. Eight controls sat entirely
//! past the right edge and could not be clicked at any point in a normal
//! session — Heuristics, Compare, Layers (debug), Controls…, Library…,
//! Settings…, Debug Viewers and Camera — which meant the Library,
//! Settings and Controls windows had **no reachable opener at all**. The
//! status label was last in the row, so the one widget whose whole job is
//! reporting what happened was the most reliably invisible thing in the
//! app.
//!
//! A 1645-test suite was green throughout. `tests/ui_smoke.rs` pins its
//! harness to 1600x1000 — 2.08x the real window — behind a comment
//! calling that size "load-bearing, not cosmetic", *because smaller sizes
//! clip*. The clipping had been noticed and worked around in the test
//! instead of fixed in the app, and no test anywhere exercised the size
//! that ships.
//!
//! ## The trap this file is built to avoid
//!
//! A test that hardcodes `768.0, 720.0` would keep passing after someone
//! changed the window, which is the same class of mistake all over again.
//! [`WINDOW_SIZE`] is one constant, `main.rs` passes it to
//! `ViewportBuilder::with_inner_size`, and this file measures against it.
//! There is no second copy of the number to drift.
//!
//! ## What "clipped" means here
//!
//! egui does not report overflow; a widget laid out past the panel edge
//! is simply drawn nowhere and receives no clicks. But it still publishes
//! an AccessKit node with a real bounding box, so the overflow is
//! *measurable* even though it is invisible — which is what makes this an
//! assertion rather than a screenshot review.

use eframe::egui;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, MIN_WINDOW_SIZE, WINDOW_SIZE};

/// A labelled node whose right edge lies outside the viewport.
struct Clipped {
    label: String,
    x0: f32,
    x1: f32,
}

// Hand-written rather than derived so the coordinates are genuinely READ.
// A `#[derive(Debug)]` here leaves `x0`/`x1` dead as far as the compiler
// is concerned, and a failure message that prints "Clipped { .. }" without
// saying where the widget went is a failure message that sends the next
// reader back to the harness to find out.
impl std::fmt::Debug for Clipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} at x {:.0}..{:.0}", self.label, self.x0, self.x1)
    }
}

/// Every labelled node laid out past `width`, plus the rightmost edge any
/// node reaches (the number that says *how much* too wide the chrome is).
fn clipped_nodes(harness: &Harness<'_, RetroForgeApp>, width: f32) -> (Vec<Clipped>, f32) {
    let mut out = Vec::new();
    let mut rightmost: f32 = 0.0;
    for node in harness.root().children_recursive() {
        let accesskit = node.accesskit_node();
        let Some(bounds) = accesskit.bounding_box() else {
            continue;
        };
        rightmost = rightmost.max(bounds.x1 as f32);
        let Some(label) = accesskit.label() else {
            continue;
        };
        if label.trim().is_empty() {
            continue;
        }
        // One pixel of slack: a widget whose edge lands exactly on the
        // boundary is drawn, and rounding at that boundary is not a bug
        // worth failing a build over.
        if bounds.x1 as f32 > width - 1.0 {
            out.push(Clipped {
                label: label.to_string(),
                x0: bounds.x0 as f32,
                x1: bounds.x1 as f32,
            });
        }
    }
    (out, rightmost)
}

fn app_at(size: [f32; 2]) -> Harness<'static, RetroForgeApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(size[0], size[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    // Two passes: egui sizes many widgets from the previous frame's
    // galley, so a single run can report a layout that never appears on
    // screen.
    harness.run();
    harness.run();
    harness
}

fn assert_nothing_clipped(size: [f32; 2], what: &str) {
    let harness = app_at(size);
    let (clipped, rightmost) = clipped_nodes(&harness, size[0]);
    assert!(
        clipped.is_empty(),
        "{what}: {} control(s) are laid out past the right edge of a {}px window and \
         cannot be clicked. Widest node ends at x={rightmost:.0}. Clipped: {clipped:#?}",
        clipped.len(),
        size[0],
    );
}

/// **Nothing is clipped at the size the application ships at.**
///
/// This is the assertion whose absence let eight unreachable controls
/// ship. It measures `WINDOW_SIZE` — the same constant `main.rs` opens
/// the window with — so shrinking the window without shrinking the chrome
/// turns this red.
#[test]
fn no_control_is_clipped_at_the_shipping_window_size() {
    assert_nothing_clipped(WINDOW_SIZE, "shipping window size");
}

/// **Nothing is clipped at the smallest size a user can drag to.**
///
/// `main.rs` sets `with_min_inner_size(MIN_WINDOW_SIZE)`, so this is a
/// reachable state, not a hypothetical one — and the floor is only
/// honest if the chrome actually survives it.
#[test]
fn no_control_is_clipped_at_the_minimum_window_size() {
    assert_nothing_clipped(MIN_WINDOW_SIZE, "minimum window size");
}

/// **The measurement can fail.** Guards against the vacuity that would
/// make both tests above meaningless: if `clipped_nodes` returned an
/// empty list because the app rendered no labelled nodes at all — the
/// exact trap `ui_smoke.rs`'s module doc records — every assertion here
/// would pass on a blank window.
///
/// Squeezing the viewport to a quarter of the shipping width must produce
/// clipping. If it does not, the detector is broken, not the layout.
#[test]
fn the_clipping_detector_actually_detects_clipping() {
    let narrow = [WINDOW_SIZE[0] / 4.0, WINDOW_SIZE[1]];
    let harness = app_at(narrow);
    let (clipped, _) = clipped_nodes(&harness, narrow[0]);
    assert!(
        !clipped.is_empty(),
        "at {}px nothing was reported clipped — the detector is measuring nothing, so \
         the tests that rely on it prove nothing",
        narrow[0],
    );
}

/// **The status text keeps the first claim on the space.**
///
/// It used to be last in a left-to-right row of sixteen widgets, so it
/// was the first thing to disappear. The bar now lays it out
/// right-to-left, ahead of the FPS, A/V and profile readouts, and this
/// asserts it survives at the shipping size rather than trusting the
/// layout call.
#[test]
fn the_status_text_is_visible_at_the_shipping_window_size() {
    let harness = app_at(WINDOW_SIZE);
    let status = harness.state().status().to_string();
    assert!(
        !status.is_empty(),
        "precondition: the app boots with a status message"
    );
    let (clipped, _) = clipped_nodes(&harness, WINDOW_SIZE[0]);
    assert!(
        !clipped.iter().any(|c| c.label.contains(&status)),
        "the status text `{status}` is clipped at the shipping window size, which is \
         where it was before W10-01"
    );
    // `query_all`, not `query`: the boot status also appears as the
    // video panel's placeholder, and `query_by_label` panics on more than
    // one match — a test that panicked there would be reporting a
    // duplicate label, not a missing one.
    assert!(
        harness.query_all_by_label(&status).next().is_some(),
        "the status text is not in the accessibility tree at all"
    );
}
