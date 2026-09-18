//! Full-map and ultrawide views over a **decoded** level, with live
//! sprites placed at profile-declared positions (ticket W5-03;
//! FR-ENH-006, `docs/design/ENHANCEMENT_RUNTIME.md` §7).
//!
//! This is where the two halves of the game-aware pipeline meet. The
//! offline half ([`crate::decode`]) turns ROM bytes into a level that
//! never changes; the live half is a handful of RAM and OAM bytes the
//! profile names. Everything here is a **pure function of those two**, so
//! the whole view is testable without a renderer and without a running
//! core — the same property that made the decoder testable, one layer up.
//!
//! ## Screen space and world space, which is the bug this module exists
//! to not have
//!
//! A console's OAM holds **screen** coordinates: a sprite at OAM x=16 is
//! 16 pixels from the left of the *viewport*, wherever the viewport
//! happens to be in the level. A full-map view draws the *level*, so
//! every sprite has to be lifted into **world** space by adding the
//! camera's scroll position — which the profile also names
//! (`[camera].x`).
//!
//! Getting that wrong does not crash and does not look obviously broken:
//! sprites simply stick to the screen while the level scrolls under them,
//! which reads as "the overlay is a bit off" rather than as a coordinate
//! bug. [`sprites_in_world`] takes the camera explicitly, and never
//! defaults it, for exactly that reason.
//!
//! ## What "active" means when the profile does not say
//!
//! `profiles/nes/rf-scroller/profile.toml` deliberately declares no
//! `active` field, because that ROM has no activity flag — its own
//! comment explains why. So the only universal signal left is the
//! platform's: NES sprites are parked off-screen by writing Y >= 0xEF,
//! and a profile with `offscreen_valid = false` is saying those are not
//! real. [`sprites_in_world`] honours both — an explicit `active` mask
//! when the profile has one, the off-screen park when it does not.

use rf_profiles::schema::{FieldSpec, Profile};

use crate::camera::{Camera, CameraMode};
use crate::decode::metatile_screens::DecodedLevel;
use crate::scene_graph::{self, DrawCmd, LevelId, SceneGraph, SceneLayer, SpriteInstance};

/// NES viewport, in pixels. The outline drawn over an enhanced view
/// (acceptance criterion 3) is exactly this, positioned at the live
/// camera — it is what the player would be seeing unenhanced.
pub const ORIGINAL_VIEWPORT: (u16, u16) = (256, 240);

/// The first OAM byte value that means "parked off-screen" on the NES.
///
/// nesdev.org/wiki/PPU_OAM: sprite Y is stored minus one and the PPU
/// evaluator only considers a sprite whose Y puts it on a visible
/// scanline, so values from 0xEF up can never appear — which is why
/// every NES game parks unused sprites there.
pub const OFFSCREEN_Y: u8 = 0xEF;

/// A decoded level's size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelGeometry {
    pub width_px: u32,
    pub height_px: u32,
    /// Pixels per metatile edge (16 for a 2x2 block of 8x8 tiles).
    pub metatile_px: u32,
}

impl LevelGeometry {
    /// Derive geometry from a decoded level.
    ///
    /// `metatile_px` is computed from the metatile's own tile count
    /// rather than assumed to be 16: a `size = 4` metatile is 2x2 tiles
    /// (16 px), a `size = 1` metatile is a single tile (8 px). Assuming
    /// 16 would silently double every coordinate for a family member that
    /// used 1x1 metatiles.
    #[must_use]
    pub fn from_level(level: &DecodedLevel, tile_px: u32) -> Self {
        let tiles_per_metatile = if level.metatile_count == 0 {
            1
        } else {
            (level.metatile_tiles.len() as u32 / level.metatile_count).max(1)
        };
        // 4 tiles -> 2x2 -> edge 2; 1 tile -> edge 1. Integer sqrt of a
        // small square count.
        let edge = (1..=4)
            .rev()
            .find(|e| e * e <= tiles_per_metatile)
            .unwrap_or(1);
        let metatile_px = edge * tile_px;
        Self {
            width_px: level.width * metatile_px,
            height_px: level.height * metatile_px,
            metatile_px,
        }
    }
}

/// The live values a profile names, read out of the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LiveCamera {
    pub x: i64,
    pub y: i64,
}

/// Read `[camera].x`/`[camera].y` through `read` (a non-perturbing CPU
/// peek).
///
/// Returns `LiveCamera::default()` when the profile declares no camera —
/// a level with no scroll is at the origin, which is the correct answer
/// rather than a failure.
pub fn live_camera(profile: &Profile, read: &dyn Fn(u32) -> u8) -> LiveCamera {
    let Some(camera) = profile.camera.as_ref() else {
        return LiveCamera::default();
    };
    let axis = |spec: Option<&rf_profiles::schema::AxisSpec>| -> i64 {
        let Some(spec) = spec else { return 0 };
        let raw = match spec.ty.as_str() {
            // Little-endian, as every 6502-family game stores a 16-bit
            // value. A profile that meant big-endian would have to say so
            // with a different type; guessing from the value would be
            // guessing.
            "u16" => i64::from(read(spec.addr)) | (i64::from(read(spec.addr + 1)) << 8),
            _ => i64::from(read(spec.addr)),
        };
        raw * i64::from(spec.scale.unwrap_or(1))
    };
    LiveCamera {
        x: axis(camera.x.as_ref()),
        y: axis(camera.y.as_ref()),
    }
}

fn field<'a>(
    fields: &'a std::collections::BTreeMap<String, FieldSpec>,
    name: &str,
) -> Option<&'a FieldSpec> {
    fields.get(name)
}

fn read_field(entry: &[u8], spec: &FieldSpec) -> Option<u8> {
    match *spec {
        FieldSpec::Offset(o) => entry.get(o as usize).copied(),
        FieldSpec::Masked { offset, mask } => entry
            .get(offset as usize)
            .map(|b| b & u8::try_from(mask & 0xFF).unwrap_or(0xFF)),
    }
}

/// Live sprites, lifted from screen space into world space.
///
/// `table` is the raw entity table (for the NES: the OAM shadow the
/// profile's `[entities].table.addr` points at, already sliced by the
/// caller — this crate cannot read a bus). `camera` is what turns screen
/// coordinates into level coordinates; see the module doc for why it is
/// a required argument rather than an option.
///
/// Sprites the profile considers inactive are dropped, not returned with
/// a flag: a full-map view that drew parked sprites would show a dozen
/// enemies stacked in a corner of the level, which is worse than showing
/// none.
#[must_use]
pub fn sprites_in_world(
    profile: &Profile,
    table: &[u8],
    camera: LiveCamera,
) -> Vec<SpriteInstance> {
    let Some(entities) = profile.entities.as_ref() else {
        return Vec::new();
    };
    let stride = entities.table.stride.max(1) as usize;
    let x_field = field(&entities.fields, "x");
    let y_field = field(&entities.fields, "y");
    let kind_field = field(&entities.fields, "kind");
    let active_field = field(&entities.fields, "active");

    let mut out = Vec::new();
    for index in 0..entities.table.count as usize {
        let start = index * stride;
        let Some(entry) = table.get(start..start + stride) else {
            break;
        };
        let (Some(xf), Some(yf)) = (x_field, y_field) else {
            break; // a table with no position is not an entity table
        };
        let (Some(sx), Some(sy)) = (read_field(entry, xf), read_field(entry, yf)) else {
            continue;
        };

        let active = match active_field {
            // The profile said how to tell: believe it.
            Some(spec) => read_field(entry, spec).is_some_and(|v| v != 0),
            // It did not, so fall back to the platform's own convention.
            // Only meaningful when the profile has also said off-screen
            // sprites are not real.
            None => entities.offscreen_valid || sy < OFFSCREEN_Y,
        };
        if !active {
            continue;
        }

        out.push(SpriteInstance {
            sprite_id: u8::try_from(index).unwrap_or(u8::MAX),
            // Screen -> world. The whole point of the module doc.
            x: i32::try_from(camera.x + i64::from(sx)).unwrap_or(i32::MAX),
            y: i32::try_from(camera.y + i64::from(sy)).unwrap_or(i32::MAX),
            tile_id: kind_field
                .and_then(|f| read_field(entry, f))
                .map_or(0, u16::from),
            palette: 0,
        });
    }
    out
}

/// The outline of what the player is actually seeing, in world space
/// (acceptance criterion 3).
///
/// FR-ENH-004's principle applied to the camera rather than to fog: an
/// enhanced view shows more than the console does, and without this the
/// user cannot tell which part of it the *game* is reacting to. Drawn as
/// an overlay rather than baked into a layer so it composites over
/// everything and can be toggled without re-deriving the scene.
#[must_use]
pub fn original_viewport_outline(camera: LiveCamera, color_index: u8) -> DrawCmd {
    DrawCmd::Rect {
        x: i32::try_from(camera.x).unwrap_or(i32::MAX),
        y: i32::try_from(camera.y).unwrap_or(i32::MAX),
        width: ORIGINAL_VIEWPORT.0,
        height: ORIGINAL_VIEWPORT.1,
        color_index,
    }
}

/// A full-map scene: the whole decoded level, zoomed to fit `target`,
/// with live sprites and the original-viewport outline over it.
///
/// The zoom reuses [`crate::camera::fm13_zoom_divisor`] rather than
/// inventing a second scaling policy — FM-13 already decided that
/// fitting is done with integer divisors (float zoom raises determinism
/// questions and does not compose with pixel-grid targets), and a level
/// too big for the target is the same problem as a canvas too big for
/// the adapter.
#[must_use]
pub fn full_map_scene(
    level: LevelId,
    geometry: LevelGeometry,
    sprites: Vec<SpriteInstance>,
    camera: LiveCamera,
    target_width: u32,
    target_height: u32,
    outline_color: u8,
) -> SceneGraph {
    let divisor = crate::camera::fm13_zoom_divisor(
        geometry.width_px,
        geometry.height_px,
        target_width.max(target_height).max(1),
    );
    SceneGraph {
        camera: Camera {
            mode: CameraMode::FullMap,
            // The centre of the LEVEL, not of the viewport: a full-map
            // view that followed the player would defeat its own purpose.
            center_world: (
                i64::from(geometry.width_px / 2),
                i64::from(geometry.height_px / 2),
            ),
            zoom_divisor: divisor,
            target_width,
            target_height,
        },
        layers: vec![
            SceneLayer::DecodedLevel {
                level,
                dirty: Vec::new(),
            },
            SceneLayer::SpriteSet { sprites },
            SceneLayer::OverlayCmds {
                cmds: vec![original_viewport_outline(camera, outline_color)],
            },
        ],
    }
}

/// Build the "walls pop up" [`SceneLayer::Geometry`] for a decoded level
/// (ticket W16-06; `docs/design/ENHANCEMENT_WAVE_16.md` §5), the profile-
/// aware wrapper over `crate::scene_graph::geometry_layer`.
///
/// **Row-major transpose.** `level.metatiles` is column-major (`col *
/// height + row`, [`DecodedLevel::metatiles`]'s own doc — the source
/// encoding is per column). [`scene_graph::geometry_layer`]'s
/// `SceneLayer::Geometry` documents `solid`/`depth` as row-major (`row *
/// tiles_w + col`), matching how a diorama mesh builder walks rows
/// front-to-back — so this function transposes once here rather than
/// pushing a second indexing convention into the scene-graph contract.
///
/// **"Decoded", not "visited"**: like [`full_map_scene`] above (whose own
/// doc calls the decoded level "known in full"), this passes every tile
/// the decode produced, not a gameplay-traversal subset — the NON_GOALS
/// #11 "no guessed geometry" obligation is met by `geometry_layer`'s own
/// structural check (a `tiles` slice that does not match `tiles_w *
/// tiles_h` fails), not by filtering here; there is nothing in this
/// family's decode that is *not* known (no fog-of-war concept exists for
/// a `metatile_screens` level, same as the pixels `render_level_rgba`
/// already draws for the whole thing).
///
/// Returns `None` when the profile declared no collision table (`level.
/// collision` is `None`) or when `geometry_layer` itself refuses (see its
/// own doc).
#[must_use]
pub fn diorama_geometry_layer(
    level_id: LevelId,
    level: &DecodedLevel,
    geometry: LevelGeometry,
    collision_bits: &str,
    solid_bit_name: &str,
    origin_px: (i32, i32),
) -> Option<SceneLayer> {
    let collision = level.collision.as_ref()?;
    let (w, h) = (level.width as usize, level.height as usize);
    let mut row_major = vec![0u8; w * h];
    for row in 0..h {
        for col in 0..w {
            row_major[row * w + col] = *level.metatiles.get(col * h + row)?;
        }
    }
    scene_graph::geometry_layer(
        level_id,
        &row_major,
        level.width,
        level.height,
        geometry.metatile_px,
        origin_px,
        collision,
        collision_bits,
        solid_bit_name,
    )
}

/// An ultrawide scene over the decoded level: same layers, but the camera
/// follows the player rather than framing the level.
///
/// Distinct from [`crate::camera::ultrawide_scene_over_canvas`], which
/// composites the *stitched* canvas — pixels RetroForge observed. This
/// one composites the *decoded* level — geometry the profile knows about
/// whether or not the player has been there. The two are deliberately
/// separate functions rather than one with a flag, because they make
/// different honesty claims: the canvas shows only visited area and fogs
/// the rest (FR-ENH-004), while a decoded level is known in full and
/// needs no fog.
#[must_use]
pub fn ultrawide_scene_over_level(
    level: LevelId,
    sprites: Vec<SpriteInstance>,
    camera: LiveCamera,
    target_width: u32,
    target_height: u32,
    outline_color: u8,
) -> SceneGraph {
    SceneGraph {
        camera: Camera {
            mode: CameraMode::Ultrawide,
            center_world: (
                camera.x + i64::from(ORIGINAL_VIEWPORT.0 / 2),
                camera.y + i64::from(ORIGINAL_VIEWPORT.1 / 2),
            ),
            zoom_divisor: 1,
            target_width,
            target_height,
        },
        layers: vec![
            SceneLayer::DecodedLevel {
                level,
                dirty: Vec::new(),
            },
            SceneLayer::SpriteSet { sprites },
            SceneLayer::OverlayCmds {
                cmds: vec![original_viewport_outline(camera, outline_color)],
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_graph::WALL_HEIGHT_EIGHTHS;

    /// A 2x2 (width x height) level, one tile per metatile (`metatile_size
    /// = 1`), column-major as `DecodedLevel::metatiles` documents:
    /// column 0 is `[solid, open]` (top-to-bottom), column 1 is
    /// `[open, solid]` — chosen asymmetric on purpose so a transpose bug
    /// (row/col swapped) produces a different, checkable mask.
    fn level_2x2() -> DecodedLevel {
        DecodedLevel {
            width: 2,
            height: 2,
            metatiles: vec![0x02, 0x01, 0x01, 0x02], // col0: [2,1], col1: [1,2]
            metatile_tiles: vec![0, 0, 0, 0],
            metatile_count: 2,
            collision: Some(vec![0, 0, 0b1]), // id1 open, id2 solid (bit0)
        }
    }

    #[test]
    fn diorama_geometry_layer_transposes_column_major_into_row_major() {
        let level = level_2x2();
        let geometry = LevelGeometry::from_level(&level, 8);
        let layer = diorama_geometry_layer(
            LevelId(3),
            &level,
            geometry,
            "solid,hazard",
            "solid",
            (10, 20),
        )
        .expect("collision declared, well-shaped level");

        let SceneLayer::Geometry {
            tiles_w,
            tiles_h,
            solid,
            depth,
            origin_px,
            ..
        } = layer
        else {
            panic!("expected SceneLayer::Geometry");
        };
        assert_eq!((tiles_w, tiles_h), (2, 2));
        assert_eq!(origin_px, (10, 20));
        // Row-major (row * tiles_w + col): row0 = [col0=solid, col1=open],
        // row1 = [col0=open, col1=solid] -- the transpose of the
        // column-major source above.
        assert_eq!(
            solid,
            vec![1, 0, 0, 1],
            "row-major transpose of the column-major decode"
        );
        assert_eq!(depth, vec![WALL_HEIGHT_EIGHTHS, 0, 0, WALL_HEIGHT_EIGHTHS]);
    }

    #[test]
    fn diorama_geometry_layer_is_none_without_a_declared_collision_table() {
        let mut level = level_2x2();
        level.collision = None;
        let geometry = LevelGeometry::from_level(&level, 8);
        assert_eq!(
            diorama_geometry_layer(LevelId(1), &level, geometry, "solid", "solid", (0, 0)),
            None
        );
    }
}
