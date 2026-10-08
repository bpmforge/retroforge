//! Enhancement runtime: event bus consumers, anti-flicker, scene graph, overlays
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.
//!
//! Ticket W4-01 gives this crate its first real content: [`bus`], the
//! event-bus/subscription mechanism over a triple-buffered `FrameBundle`
//! (`rf_core_api::triple_buffer`) — see that module's doc for what's
//! deliberately still a stub (`SpriteHistorian`, `ScrollTracker`, the
//! actual `EnhancementRuntime`) and why.

pub mod atmosphere;
pub mod bus;
pub mod camera;
pub mod camera_finder;
pub mod decode;
pub mod distribution;
pub mod experiments;
pub mod hd_render;
pub mod hdpack;
pub mod hud;
pub mod interpolation;
pub mod level_view;
pub mod loading;
pub mod mods;
pub mod overlay;
pub mod persistence;
pub mod scene_graph;
pub mod scene_identity;
pub mod scene_tracker;
pub mod scroll_tracker;
pub mod sprite_historian;
pub mod stitcher;
pub mod trust;
pub mod widescreen;

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-enhance";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-enhance");
    }
}
