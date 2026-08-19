//! Persisted dock-panel layout (FR-FE-004; ticket W4-06a criterion 3:
//! "`egui_dock` panel layout persisted").
//!
//! ## Why this is a hand-rolled format, not a serialized `egui_dock::DockState`
//!
//! `egui_dock` 0.20.1 documents `DockState` as directly serializable behind
//! its own `serde` feature — but that feature is **not** enabled on this
//! workspace's `egui_dock` dependency (see `crates/retroforge/Cargo.toml`'s
//! comment). Its tree embeds `egui::Rect` per node (whose own `Serialize`/
//! `Deserialize` impls need egui's `serde` feature, unverified here) and a
//! mixed unit/tuple-variant `Node<Tab>` enum nested in a `Vec` — exactly
//! the shape this workspace's one sanctioned serialization format, TOML
//! (`docs/TECH_STACK.md` §2 "Config/profiles" row), is known to strain on.
//! Rects are also meaningless to restore across a window resize —
//! `DockArea` recomputes them every frame regardless.
//!
//! So this module owns a small, versioned format instead: which panels are
//! open, in what split arrangement, at what split fractions — the part of
//! a layout actually worth remembering across a restart.
//! `crates/retroforge/src/debug_dock.rs` (egui_dock IS allowed to touch
//! that module, not this UI-free crate) walks a live
//! `egui_dock::DockState<DebugTab>`'s public `Tree`/`Node`/`SplitNode`/
//! `LeafNode` fields (all plain data with safe constructors — verified
//! against the vendored `egui_dock-0.20.1` source) to build a
//! [`PersistedLayout`] and to rebuild a `DockState` from one via `Tree`'s
//! public `split()` API.
use serde::{Deserialize, Serialize};

/// The fixed menu of debug viewers this ticket (W4-06a) and its dependents
/// (W4-06b/W4-06c, per the ticket brief) register as dockable tabs. Adding
/// a viewer later is an additive enum variant — old persisted layouts that
/// never mention it still deserialize fine (`#[serde(default)]`-free here
/// because every variant is a plain unit case with no fields to default).
///
/// `Memory` (ticket W4-06b, FR-DBG-002): the read-only, non-perturbing
/// memory-hex panel over `crate::memory_view`'s decoded rows. Adding it
/// does **not** bump [`LAYOUT_FORMAT_VERSION`] — this doc's own "additive
/// enum variant" clause covers exactly this case, an old persisted layout
/// that never mentions `Memory` still deserializes fine, it simply never
/// lists that tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebugTab {
    Pattern,
    Nametable,
    Palette,
    Oam,
    EventTimeline,
    Memory,
    /// `OamDiff` (ticket W4-06c, FR-DBG-006): which sprites changed since
    /// the last frame and which the 8-per-scanline limit dropped.
    /// Additive in the same sense as the variants above.
    OamDiff,
    /// `LuaConsole` (ticket W4-04, DEBUGGER.md §5): the plugin script
    /// REPL. Additive in exactly the sense this doc's own clause above
    /// describes — an old persisted layout that never mentions it still
    /// deserializes fine and simply never lists the tab, so
    /// [`LAYOUT_FORMAT_VERSION`] does not move.
    LuaConsole,
    /// `Trace` (ticket W4-10a, DEBUGGER.md §2-3): the per-chip trace
    /// scrollback with its filters. Additive in the same sense as every
    /// variant above — an old persisted layout that never mentions it
    /// still deserializes and simply never lists the tab, so
    /// [`LAYOUT_FORMAT_VERSION`] does not move.
    Trace,
}

/// Which axis a [`PersistedNode::Split`] divides along — matches
/// `egui_dock::Node::Vertical`/`Node::Horizontal`'s own naming (top/bottom
/// vs. left/right), not `egui_dock::Split`'s four-way
/// `Left`/`Right`/`Above`/`Below` (that finer distinction is only ever
/// used to decide which side of the split the *new* content lands on when
/// building a tree from scratch; a captured tree already has both sides,
/// so only the axis needs to survive a round trip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitAxis {
    Vertical,
    Horizontal,
}

/// One node of the persisted dock tree — either a tabbed leaf, or a split
/// dividing two child nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PersistedNode {
    Leaf {
        tabs: Vec<DebugTab>,
        /// Index into `tabs` of the currently focused tab.
        active: usize,
    },
    Split {
        axis: SplitAxis,
        /// Fraction of space given to `first` (`egui_dock::SplitNode::
        /// fraction`'s own convention: "the fraction taken by the top
        /// child" — `first` is always the node egui_dock keeps at the
        /// split's lower index, see `debug_dock`'s capture/restore).
        fraction: f32,
        first: Box<PersistedNode>,
        second: Box<PersistedNode>,
    },
}

/// Format version — bump if [`PersistedNode`]'s shape ever changes in a
/// way old files can't `serde`-deserialize into the new shape, so
/// `debug_dock`'s loader has a field to check before trusting a file from
/// an older build.
pub const LAYOUT_FORMAT_VERSION: u32 = 1;

/// Top-level persisted file contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedLayout {
    pub version: u32,
    pub root: PersistedNode,
}

/// The layout a fresh install (or a first-ever debug session) starts with:
/// Pattern+Nametable tabbed together on the left, Palette+Oam tabbed
/// together on the right, Event timeline along the bottom — a
/// Mesen-style arrangement (DEBUGGER.md §3's own framing), not an
/// arbitrary default.
#[must_use]
pub fn default_layout() -> PersistedLayout {
    PersistedLayout {
        version: LAYOUT_FORMAT_VERSION,
        root: PersistedNode::Split {
            axis: SplitAxis::Vertical,
            fraction: 0.7,
            first: Box::new(PersistedNode::Split {
                axis: SplitAxis::Horizontal,
                fraction: 0.5,
                first: Box::new(PersistedNode::Leaf {
                    tabs: vec![DebugTab::Pattern, DebugTab::Nametable],
                    active: 0,
                }),
                second: Box::new(PersistedNode::Leaf {
                    tabs: vec![DebugTab::Palette, DebugTab::Oam, DebugTab::Memory],
                    active: 0,
                }),
            }),
            second: Box::new(PersistedNode::Leaf {
                tabs: vec![DebugTab::EventTimeline, DebugTab::Trace],
                active: 0,
            }),
        },
    }
}

/// Serialize `layout` to this crate's persisted TOML form.
///
/// # Errors
/// Returns `toml::ser::Error` if `layout` somehow contains a shape TOML's
/// data model can't represent (not expected — see module doc's "why this
/// shape was chosen").
pub fn to_toml_string(layout: &PersistedLayout) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(layout)
}

/// Parse a previously-serialized layout back from TOML text.
///
/// # Errors
/// Returns `toml::de::Error` for malformed/foreign TOML — a caller should
/// fall back to [`default_layout`] rather than propagate this to the user
/// as a fatal error (a corrupted or hand-edited layout file is not a
/// reason to refuse to start the debugger).
pub fn from_toml_str(text: &str) -> Result<PersistedLayout, toml::de::Error> {
    toml::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_round_trips_through_an_actual_toml_string() {
        // Vacuity-trap requirement (ticket brief (b)): go through the real
        // serialized string form, not just a `Clone`, so a broken
        // `Serialize`/`Deserialize` derive (e.g. a field silently skipped)
        // would be caught.
        let layout = default_layout();
        let text = to_toml_string(&layout).expect("default layout must serialize to TOML");
        let restored = from_toml_str(&text).expect("that exact TOML text must parse back");
        assert_eq!(restored, layout);
    }

    #[test]
    fn round_trip_preserves_tab_order_within_a_leaf_not_just_set_membership() {
        let layout = PersistedLayout {
            version: LAYOUT_FORMAT_VERSION,
            root: PersistedNode::Leaf {
                tabs: vec![DebugTab::Oam, DebugTab::Pattern, DebugTab::Palette],
                active: 2,
            },
        };
        let text = to_toml_string(&layout).unwrap();
        let restored = from_toml_str(&text).unwrap();
        let PersistedNode::Leaf { tabs, active } = restored.root else {
            panic!("expected a Leaf node back");
        };
        assert_eq!(
            tabs,
            vec![DebugTab::Oam, DebugTab::Pattern, DebugTab::Palette]
        );
        assert_eq!(active, 2);
    }

    #[test]
    fn round_trip_preserves_nested_split_axis_and_fraction() {
        let layout = PersistedLayout {
            version: LAYOUT_FORMAT_VERSION,
            root: PersistedNode::Split {
                axis: SplitAxis::Horizontal,
                fraction: 0.3333,
                first: Box::new(PersistedNode::Leaf {
                    tabs: vec![DebugTab::EventTimeline],
                    active: 0,
                }),
                second: Box::new(PersistedNode::Split {
                    axis: SplitAxis::Vertical,
                    fraction: 0.8,
                    first: Box::new(PersistedNode::Leaf {
                        tabs: vec![DebugTab::Nametable],
                        active: 0,
                    }),
                    second: Box::new(PersistedNode::Leaf {
                        tabs: vec![DebugTab::Oam],
                        active: 0,
                    }),
                }),
            },
        };
        let text = to_toml_string(&layout).unwrap();
        let restored = from_toml_str(&text).unwrap();
        assert_eq!(restored, layout, "TOML text was:\n{text}");
    }

    #[test]
    fn from_toml_str_rejects_garbage_rather_than_panicking() {
        assert!(from_toml_str("this is not { valid TOML").is_err());
        assert!(from_toml_str("version = \"not a number\"").is_err());
    }

    #[test]
    fn default_layout_carries_the_current_format_version() {
        assert_eq!(default_layout().version, LAYOUT_FORMAT_VERSION);
    }
}
