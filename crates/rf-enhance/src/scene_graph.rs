//! Scene graph (ticket W4-03b; `docs/design/ENHANCEMENT_RUNTIME.md` §7):
//! "the renderer contract" — but resolving its handles (`TexId`/`CanvasId`/
//! `LevelId`) into real GPU resources is explicitly NOT this crate's job.
//! W4-03c's pre-flight note settles the architecture: `ARCHITECTURE.md`
//! §3's layer diagram draws no edge between `rf-renderer` and `rf-enhance`
//! in either direction, so `retroforge` (the app shell, which already
//! depends on both) is the handle-resolution point — "do not build a
//! handle registry inside `rf-renderer` just because that is where you
//! happen to be standing" applies symmetrically here: this crate does not
//! build one either.
//!
//! ## Scope fence (read before extending this file)
//!
//! This ticket **constructs and tests** exactly one variant:
//! [`SceneLayer::StitchedCanvas`] (`crate::camera::ultrawide_scene_over_canvas`)
//! — the ultrawide camera + fog criterion. Every other [`SceneLayer`]
//! variant is defined here so W4-03c's renderer (and any later ticket) has
//! a stable contract to target, per §7's literal shape, but is
//! **shape-only**: nothing in this crate produces an `OriginalFrame`,
//! `DecodedLevel`, `SpriteSet`, `HudPinned`, or `OverlayCmds` value,
//! because no producer for any of them exists yet (`bus.rs`'s own module
//! doc: `SpriteHistorian`/`ProfileDecoders`/`PluginHost` are all still
//! stubs). Building fake data for them here would be exactly the
//! "unvalidated scaffolding" `bus.rs` already declined to add.
//!
//! **Update, ticket W16-03**: [`SceneLayer::ExtractedBg`] gained its first
//! producer, `crate::atmosphere::AtmosphereDetector::observe` — the
//! shadow-rung atmosphere-layer heuristic
//! (`docs/design/ENHANCEMENT_WAVE_16.md` §4). It is still shadow-only:
//! the layer is produced for the per-game report card / a future Enhance
//! workspace preview, and nothing in this crate or its dependents
//! composites it into a rendered frame. `ExtractedBg` therefore moved from
//! "shape-only" to "shape plus real content" while every other variant
//! above stays shape-only.

use rf_core_api::PpuPixel;

use crate::camera::{Camera, FogStyle};

/// Opaque handle into a GPU texture the shell owns — resolution happens
/// outside this crate (module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TexId(pub u64);

/// Opaque handle identifying a stitched [`crate::stitcher::Canvas`] the
/// shell can look up (e.g. by ROM hash + [`crate::scene_identity::SceneId`]
/// — `crate::persistence::canvas_cache_key`'s own key shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanvasId(pub u64);

/// Opaque handle identifying a profile-decoded level (W5-02, not built
/// yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LevelId(pub u64);

/// Opaque handle identifying one dirty chunk within a [`LevelId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkId(pub u64);

/// Core-defined background plane index (mirrors
/// `rf_core_api::PixelLayer::Background`'s `u8` payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BgLayerId(pub u8);

/// A screen-space region pinned during enhanced-camera movement (§7's
/// `HudPinned`; FR-ENH-007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HudRegion {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Which screen corner a [`HudRegion`] stays anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// One de-flickered sprite instance (§7's `SpriteSet`) — shape-only in
/// this ticket (module doc): no `SpriteHistorian` exists yet to populate
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteInstance {
    pub sprite_id: u8,
    pub x: i32,
    pub y: i32,
    pub tile_id: u16,
    pub palette: u8,
}

/// One immediate-mode plugin/debugger draw command (§7's `OverlayCmds`) —
/// deliberately minimal (line/rect only): no plugin host or debugger
/// overlay producer exists yet either (module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawCmd {
    Line {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color_index: u8,
    },
    Rect {
        x: i32,
        y: i32,
        width: u16,
        height: u16,
        color_index: u8,
    },
}

/// One back-to-front scene layer (`docs/design/ENHANCEMENT_RUNTIME.md`
/// §7's literal shape). Module doc: only [`SceneLayer::StitchedCanvas`]
/// has a producer in this ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneLayer {
    /// Passthrough / compare-mode original frame.
    OriginalFrame { texture: TexId },
    /// The ultrawide/full-map stitched canvas, with unvisited area fogged
    /// (FR-ENH-004) — this ticket's constructed variant.
    StitchedCanvas { canvas: CanvasId, fog: FogStyle },
    /// Profile-decoded full level (W5-02).
    DecodedLevel { level: LevelId, dirty: Vec<ChunkId> },
    /// A single extracted background plane (first producer: ticket
    /// W16-03's `crate::atmosphere::AtmosphereDetector`, shadow-only —
    /// module doc's "Update, ticket W16-03" note). `pixels` carries
    /// exactly this plane's own [`PpuPixel`]s at `width`x`height` (every
    /// other on-screen pixel replaced with the backdrop default); `scroll`
    /// is the plane's raw hardware scroll value as of the frame it was
    /// captured, for a future preview/report-card display only.
    ExtractedBg {
        layer: BgLayerId,
        pixels: Vec<PpuPixel>,
        width: u16,
        height: u16,
        scroll: (i64, i64),
    },
    /// De-flickered sprite union.
    SpriteSet { sprites: Vec<SpriteInstance> },
    /// A pinned HUD region.
    HudPinned { region: HudRegion, anchor: Anchor },
    /// Plugin/debugger overlay commands.
    OverlayCmds { cmds: Vec<DrawCmd> },
}

/// The renderer contract itself (§7): a camera plus back-to-front layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneGraph {
    pub camera: Camera,
    pub layers: Vec<SceneLayer>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::CameraMode;

    /// Not a behavior test — confirms every §7 variant's shape actually
    /// type-checks and is constructible, the same "shape, not behavior"
    /// bar `rf_cache::CanvasChunk`'s own shape-only test (W4-03a) sets.
    #[test]
    fn every_scene_layer_variant_is_constructible() {
        let layers = vec![
            SceneLayer::OriginalFrame { texture: TexId(1) },
            SceneLayer::StitchedCanvas {
                canvas: CanvasId(1),
                fog: FogStyle::Themed { palette_index: 0 },
            },
            SceneLayer::DecodedLevel {
                level: LevelId(1),
                dirty: vec![ChunkId(1), ChunkId(2)],
            },
            SceneLayer::ExtractedBg {
                layer: BgLayerId(0),
                pixels: Vec::new(),
                width: 0,
                height: 0,
                scroll: (0, 0),
            },
            SceneLayer::SpriteSet {
                sprites: vec![SpriteInstance {
                    sprite_id: 0,
                    x: 10,
                    y: 20,
                    tile_id: 5,
                    palette: 1,
                }],
            },
            SceneLayer::HudPinned {
                region: HudRegion {
                    x: 0,
                    y: 0,
                    width: 32,
                    height: 16,
                },
                anchor: Anchor::TopLeft,
            },
            SceneLayer::OverlayCmds {
                cmds: vec![DrawCmd::Rect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                    color_index: 3,
                }],
            },
        ];

        let graph = SceneGraph {
            camera: Camera {
                mode: CameraMode::Original,
                center_world: (0, 0),
                zoom_divisor: 1,
                target_width: 256,
                target_height: 240,
            },
            layers,
        };
        assert_eq!(
            graph.layers.len(),
            7,
            "one constructed value per §7 variant"
        );
    }
}
