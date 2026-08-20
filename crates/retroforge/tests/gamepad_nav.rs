//! Gamepad navigation and the accessibility pass (ticket W8-04;
//! `docs/design/FRONTEND_UI.md` §4 and §6).
//!
//! ## Why this asserts NODES, not pixels
//!
//! The acceptance says so, and W4-09 recorded why it has to. An
//! AccessKit-driven test fails silently in a specific way: a query that
//! finds nothing makes the test do nothing, and "nothing went wrong"
//! reads as green. Two traps from that ticket apply directly here and
//! are guarded against below:
//!
//! 1. **At the harness's default size every panel renders zero accessible
//!    nodes**, so assertions would be written against an empty tree.
//!    Hence [`HARNESS_SIZE`], which is load-bearing rather than cosmetic.
//! 2. **Neither a bare `ui.label` nor a hover-sensed `egui::Label`
//!    contributes a node.** Only interactive widgets do. So "this surface
//!    is reachable by gamepad" can only be asserted about widgets that
//!    are actually focusable — which is the same set a screen reader can
//!    reach, and exactly the point of §6.

use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use retroforge::ui_nav::{GamepadNav, NavAction};

/// Load-bearing: see the module doc's trap 1.
const HARNESS_SIZE: (f32, f32) = (1600.0, 1000.0);

/// A small surface standing in for a menu: several focusable widgets in
/// a defined order.
fn menu_ui(ui: &mut egui::Ui, selected: &mut usize) {
    ui.heading("Library");
    for (i, name) in ["Play", "Settings", "Quit"].iter().enumerate() {
        // `selectable_label`, NOT `ui.label` — trap 2. A plain label
        // contributes no accessible node, so it can be neither focused by
        // a gamepad nor announced by a screen reader.
        if ui.selectable_label(*selected == i, *name).clicked() {
            *selected = i;
        }
    }
}

/// **Focusable nodes exist at all.** The prerequisite for every other
/// claim here — and the assertion W4-09's trap 2 says cannot be taken for
/// granted, since a plain `ui.label` contributes nothing.
#[test]
fn the_menu_surfaces_accessibility_nodes_that_can_be_focused() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(HARNESS_SIZE.0, HARNESS_SIZE.1))
        .build_ui_state(menu_ui, 0usize);
    harness.run();

    for name in ["Play", "Settings", "Quit"] {
        harness.get_by_label(name).focus();
        harness.run();
        assert!(
            harness.get_by_label(name).is_focused(),
            "{name} must be focusable, or no gamepad and no screen reader \
             can reach it"
        );
    }
}

/// **A gamepad direction moves focus.** Asserting that a DIFFERENT node
/// ends up focused — rather than that an event was sent — is the
/// difference between testing the wiring and testing the behaviour.
#[test]
fn a_gamepad_direction_moves_focus_off_the_current_node() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(HARNESS_SIZE.0, HARNESS_SIZE.1))
        .build_ui_state(menu_ui, 0usize);
    harness.run();

    harness.get_by_label("Play").focus();
    harness.run();
    assert!(harness.get_by_label("Play").is_focused(), "starting point");

    for event in GamepadNav::events_for(&[NavAction::Next]) {
        harness.input_mut().events.push(event);
    }
    harness.run();

    assert!(
        !harness.get_by_label("Play").is_focused(),
        "a gamepad Next must move focus off the item it started on"
    );
}

/// Every item is REACHABLE by repeated navigation — §4's "every UI
/// surface reachable and operable with a gamepad alone".
///
/// Walking the order and requiring each entry to be visited is stronger
/// than checking one step: a focus order that SKIPS an entry is exactly
/// the bug that makes a surface unreachable.
#[test]
fn repeated_gamepad_navigation_reaches_every_item() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(HARNESS_SIZE.0, HARNESS_SIZE.1))
        .build_ui_state(menu_ui, 0usize);
    harness.run();
    harness.get_by_label("Play").focus();
    harness.run();

    let mut reached = std::collections::BTreeSet::new();
    for _ in 0..12 {
        for name in ["Play", "Settings", "Quit"] {
            if harness.get_by_label(name).is_focused() {
                reached.insert(name);
            }
        }
        for event in GamepadNav::events_for(&[NavAction::Next]) {
            harness.input_mut().events.push(event);
        }
        harness.run();
    }

    for name in ["Play", "Settings", "Quit"] {
        assert!(
            reached.contains(name),
            "gamepad navigation never reached {name}; reached {reached:?}"
        );
    }
}

/// **Every focusable node has a text alternative** — §6's "screen-reader
/// labels on player-facing surfaces".
///
/// A focusable widget with no label is reachable and unannounceable: the
/// worst combination, because it is invisible only to the users who most
/// need the label.
#[test]
fn every_interactive_node_carries_a_label() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(HARNESS_SIZE.0, HARNESS_SIZE.1))
        .build_ui_state(menu_ui, 0usize);
    harness.run();

    let mut checked = 0;
    for node in harness.root().children_recursive() {
        let ak = node.accesskit_node();
        // Only interactive roles: a container legitimately has no label.
        if matches!(ak.role(), Role::Button | Role::CheckBox | Role::RadioButton) {
            assert!(
                ak.label().is_some_and(|l| !l.trim().is_empty()),
                "an interactive {:?} node has no text alternative",
                ak.role()
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 3,
        "the tree must actually contain interactive nodes — {checked} found, \
         which would make this test vacuous (W4-09's trap 1)"
    );
}

/// Activating with the pad's South button operates the focused widget.
#[test]
fn the_activate_button_operates_the_focused_widget() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(HARNESS_SIZE.0, HARNESS_SIZE.1))
        .build_ui_state(menu_ui, 0usize);
    harness.run();

    harness.get_by_label("Settings").focus();
    harness.run();
    for event in GamepadNav::events_for(&[NavAction::Activate]) {
        harness.input_mut().events.push(event);
    }
    harness.run();
    harness.run();

    assert_eq!(
        *harness.state(),
        1,
        "activating the focused item must operate it"
    );
}
