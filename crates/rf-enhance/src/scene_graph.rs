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

/// A per-screen solidity mask: one byte (`0` or `1`) per decoded tile, in
/// the same order as `tiles` — the field `docs/design/ENHANCEMENT_WAVE_16.md`
/// §5 names as the thing `SceneLayer::DecodedLevel` needs so a later 3D
/// compositor (W16-06) can extrude solid tiles into blocks.
///
/// **Why it is a free function here rather than a new field on
/// [`SceneLayer::DecodedLevel`] in this ticket.** That variant already has
/// two production call sites with every field spelled out explicitly —
/// `crates/retroforge/src/enhanced_view.rs`'s
/// `ultrawide_scene_over_level_full_map` and `ultrawide_scene_over_level`
/// — and `crates/retroforge/**` is outside W16-05's write_scope
/// (`crates/rf-enhance/**`, `crates/rf-profiles/**`, `profiles/**`,
/// `docs/design/GAME_PROFILES.md`). Adding a field to that struct variant
/// without also touching those two call sites does not compile, so it
/// would either break `crates/retroforge` or require editing a crate this
/// ticket has no write access to. W16-06 depends_on W16-05 and its own
/// write_scope explicitly includes `crates/retroforge/**` and
/// `crates/rf-renderer/**` — wiring this mask into the enum field (and
/// resolving it into extruded geometry) belongs there. This function is
/// the ready-to-wire producer.
///
/// **NON_GOALS #11, visited-only — an obligation on the caller.** This
/// function has no notion of "unvisited": it builds a mask for whatever
/// `tiles` it is handed. The caller (the eventual `SceneLayer::DecodedLevel`
/// producer) must pass only tiles from screens actually visited/decoded,
/// never a synthesized "whole level" guess — the same rule the stitched
/// canvas's fog already enforces (FR-ENH-004).
///
/// `collision_table` is looked up by raw tile value (as
/// `rf_enhance::decode::room_grid::DecodedRooms::collision` is shaped) or
/// by metatile id (as `rf_enhance::decode::metatile_screens::DecodedLevel::
/// collision` is shaped) — both are "one attribute byte per id", so the
/// same lookup serves either family. `bits` is the profile's
/// `[decode].collision.bits` CSV (GAME_PROFILES.md §2, e.g.
/// `"solid,platform,hazard"`); `solid_bit_name` is the bit to test —
/// ordinarily `"solid"`. Returns `None` when `solid_bit_name` is not in
/// `bits`, or when its position cannot exist in an 8-bit attribute byte.
#[must_use]
pub fn solidity_mask(
    tiles: &[u8],
    collision_table: &[u8],
    bits: &str,
    solid_bit_name: &str,
) -> Option<Vec<u8>> {
    let bit = bits.split(',').position(|b| b.trim() == solid_bit_name)?;
    if bit >= 8 {
        return None; // no attribute byte has an 8th+ bit
    }
    let mask = 1u8 << bit;
    Some(
        tiles
            .iter()
            .map(|&id| {
                collision_table
                    .get(id as usize)
                    .map_or(0, |&attr| u8::from(attr & mask != 0))
            })
            .collect(),
    )
}

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

    /// Renders a `solidity_mask` result as ASCII (`#` solid, `.` open) and
    /// checks the geometry — row/column alignment survives the round trip
    /// from a decoded `room_grid` room through the mask, which is the
    /// property that matters for a later 3D compositor walking rows and
    /// columns of tiles.
    #[test]
    fn solidity_mask_renders_a_room_with_the_right_geometry() {
        use crate::decode::room_grid::{decode, Spec};

        // A 3x3 room: a wall ring (tile 0x02) around one open floor tile
        // (tile 0x01). One room, unindexed, so `rooms[0]` is exactly this
        // 9-tile grid in row-major order.
        #[rustfmt::skip]
        let room: [u8; 9] = [
            0x02, 0x02, 0x02,
            0x02, 0x01, 0x02,
            0x02, 0x02, 0x02,
        ];
        let mut rom = room.to_vec();
        let collision_offset = rom.len() as u32;
        // bits = "solid,hazard": bit0 solid. Tile 0x01 (floor) not solid,
        // tile 0x02 (wall) solid.
        rom.extend_from_slice(&[0, 0, 0b1]); // index 0 unused, 1 open, 2 solid
        let spec = Spec {
            rooms_across: 1,
            rooms_down: 1,
            room_width: 3,
            room_height: 3,
            data_offset: 0,
            index_offset: None,
            collision_table: Some(collision_offset),
        };
        let decoded = decode(&rom, &spec).expect("synthetic room decodes");
        let tiles = decoded.room_tiles_at(0, 0).expect("one room at (0,0)");
        let collision = decoded.collision.as_ref().expect("collision declared");

        let mask =
            solidity_mask(tiles, collision, "solid,hazard", "solid").expect("`solid` is in bits");
        assert_eq!(mask.len(), tiles.len(), "one mask byte per decoded tile");

        // Render 3x3 rows of the mask exactly as the decoded room is laid
        // out, and check the ring shape survived.
        let width = spec.room_width as usize;
        let rendered: String = mask
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|&b| if b == 1 { '#' } else { '.' })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            rendered, "###\n#.#\n###",
            "wall ring around an open center:\n{rendered}"
        );
    }

    #[test]
    fn solidity_mask_is_none_when_the_bit_name_is_not_declared() {
        assert_eq!(solidity_mask(&[0], &[0], "solid,hazard", "platform"), None);
    }

    #[test]
    fn solidity_mask_is_none_past_the_eighth_bit() {
        assert_eq!(
            solidity_mask(&[0], &[0], "a,b,c,d,e,f,g,h,solid", "solid"),
            None,
            "bit index 8 cannot exist in a u8 attribute byte"
        );
    }
}
