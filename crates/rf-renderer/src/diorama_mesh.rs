//! The Diorama pass's CPU-side mesh builder (ticket W16-06; `docs/design/
//! ENHANCEMENT_WAVE_16.md` §5) — a **pure function** of tile data plus
//! billboard footprints, with no wgpu/GPU dependency at all, so its
//! correctness (law 8's "every walk must prove progress", and the actual
//! geometry a tile grid produces) is checkable with plain unit tests.
//! [`DioramaPass::render`] (`crate::diorama`) is the thin GPU-facing
//! wrapper that uploads whatever [`build_vertices`] returns.
//!
//! ## No depth buffer: painter's algorithm, on purpose
//!
//! Every existing pass in this crate (`fog.rs`, `composite.rs`,
//! `shader_chain.rs`, `original_pipeline.rs`, `scale.rs`) renders with
//! `depth_stencil: None` — nothing here has ever stood up a depth texture
//! or a `DepthStencilState`. Rather than introduce that novel wgpu-30
//! surface for one pass, [`build_vertices`] instead emits triangles
//! **already sorted back-to-front** (ground, then tile rows from
//! farthest-from-camera to nearest (row 0 first, W30-05b), boxes and billboards/shadows
//! interleaved per row) and [`crate::diorama::DioramaPass`] draws them
//! with ordinary alpha blending, in emitted order — the same "draw order
//! IS compositing order" contract `crate::composite`'s own module doc
//! already states for its back-to-front layers, applied here per-triangle
//! instead of per-layer.
//!
//! ## Coordinate space
//!
//! World X/Z are in the SAME pixel units as the ground texture and
//! [`crate::composite`]'s world-pixel space (`tile_px`-sized tiles laid
//! out row-major): tile `(row, col)` occupies world X in `[col*tile_px,
//! (col+1)*tile_px)` and world Z in `[row*tile_px, (row+1)*tile_px)`.
//! World Y is height, in the same pixel unit ([`wall_height_px`] converts
//! `rf_enhance::scene_graph::SceneLayer::Geometry::depth`'s eighths — this
//! crate has no dependency on `rf-enhance`, `ARCHITECTURE.md` §3, so the
//! shell passes plain `u8` eighths and this module does the conversion).
//! Row increases TOWARD the camera (§5's "only the camera pitches";
//! `crate::diorama::camera_matrices`'s fixed placement sits at positive Z,
//! looking toward -Z, ticket W30-05b), so row 0 is the far edge at the top
//! of the screen, world +X is screen-right, and the picture is the level
//! image seen from above. That is what makes "row 0 first" the correct
//! back-to-front draw order.
//!
//! ## Orbiting cameras (ticket W30-05)
//!
//! "Farthest row first" is only right while the camera sits straight
//! behind the grid. [`build_vertices_oriented`] is the general case for a
//! camera that has yawed or moved off-centre: it takes the camera's yaw and
//! its horizontal eye position, sorts boxes and billboards by Manhattan
//! distance from that eye (far first — an order that respects "a nearer
//! cell may hide a farther one" for any eye position over a tile grid),
//! draws only the box sides the eye can actually see, and turns each
//! billboard to face the yaw. The default camera never takes this path
//! ([`crate::diorama::vertices_for_camera`]), so the pinned goldens keep
//! the original [`build_vertices`] bytes.

pub use crate::diorama_side::{build_vertices_side, SideBillboard, SideScene};

/// One vertex: position (world space), texture UV, an RGBA tint/alpha,
/// and `kind` selecting which fragment behaviour `shaders/diorama.wgsl`
/// applies (ground/top-face texture, sprite-billboard texture, procedural
/// shadow, or flat-shaded box side — see the shader's own header for the
/// four values).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    pub kind: u32,
}

/// `shaders/diorama.wgsl`'s `kind` values — named here so
/// [`build_vertices`] and the shader never drift out of sync (one
/// constant set, not two independently invented ones, `BudgetGate`'s own
/// convention in `fog.rs`).
pub mod kind {
    /// Ground plane or a box's top face: sample the ground texture at
    /// `uv`, tint by `color.rgb`, opaque.
    pub const GROUND: u32 = 0;
    /// A sprite billboard: sample the sprite-cutout texture at `uv`,
    /// alpha = texture alpha * `color.a`.
    pub const SPRITE: u32 = 1;
    /// A contact-shadow quad: no texture; alpha is a radial falloff from
    /// the quad's centre (`uv` in `[0, 1]`) times `color.a`.
    pub const SHADOW: u32 = 2;
    /// A box side face: flat-shaded, no texture — `color` carries the
    /// already-darkened tint.
    pub const SIDE: u32 = 3;
}

/// World-space height (pixels) of one [`WALL_HEIGHT_EIGHTHS`]-style
/// eighth. `8` eighths (one full tile edge) is therefore `tile_px`
/// world-pixels tall — a solid tile extrudes into a cube exactly one tile
/// wide/deep/tall, matching §5's "extrude into blocks" at the plainest
/// possible scale.
#[must_use]
fn wall_height_px(depth_eighths: u8, tile_px: f32) -> f32 {
    (f32::from(depth_eighths) / 8.0) * tile_px
}

/// Darkening applied to a box's side faces relative to its top ("side
/// faces shaded" per §5) — an arbitrary-but-documented flat multiplier,
/// the same calibration stance `fog.rs`'s own constants take: not a
/// measured lighting model, a plain reading of "shaded" for a first cut.
const SIDE_SHADE: f32 = 0.55;

/// One sprite footprint to stand up as a billboard (§5: "sprites stand as
/// billboards at their OAM footprint with a contact-shadow ellipse under
/// each").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Billboard {
    /// Footprint centre, world X/Z pixels (ground-plane position, e.g. an
    /// OAM sprite's on-screen X plus half its width, and Y plus its full
    /// height for a "feet on the ground" anchor).
    pub center_x_px: f32,
    pub center_z_px: f32,
    /// Billboard quad size in world pixels (typically the sprite's own
    /// on-screen width/height).
    pub width_px: f32,
    pub height_px: f32,
    /// UV rect (`u0, v0, u1, v1`) into the sprite-cutout texture this
    /// billboard samples.
    pub uv: [f32; 4],
}

/// Everything [`build_vertices`] needs: a `tiles_w`x`tiles_h` grid
/// (row-major, matching `rf_enhance::scene_graph::SceneLayer::Geometry`'s
/// own documented layout) plus the billboards standing on it.
#[derive(Debug, Clone, Copy)]
pub struct DioramaScene<'a> {
    pub tiles_w: u32,
    pub tiles_h: u32,
    pub tile_px: f32,
    /// Row-major solidity mask, `0`/`1`, `tiles_w * tiles_h` entries.
    pub solid: &'a [u8],
    /// Row-major extrusion height in eighths, same indexing as `solid`
    /// (`rf_enhance::scene_graph::SceneLayer::Geometry::depth`'s own
    /// shape).
    pub depth: &'a [u8],
    pub billboards: &'a [Billboard],
}

/// Build every vertex for one frame of the diorama, already in
/// back-to-front draw order (module doc). Returns an empty `Vec` (not a
/// panic) for a zero-sized grid — an empty decoded level renders as an
/// empty (fully transparent, once the pass clears to that) frame, which
/// is the honest answer, not an error.
///
/// # Panics
/// Panics if `solid.len()` or `depth.len()` does not equal `tiles_w *
/// tiles_h` — a caller (shell) bug, the same "malformed input is a caller
/// bug, not a runtime condition to degrade through" stance
/// `EnhancedCompositor::composite`'s own doc takes for a mismatched
/// layer buffer.
#[must_use]
pub fn build_vertices(scene: &DioramaScene<'_>) -> Vec<Vertex> {
    let (w, h) = (scene.tiles_w as usize, scene.tiles_h as usize);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    assert_eq!(
        scene.solid.len(),
        w * h,
        "build_vertices: solid mask length does not match tiles_w*tiles_h"
    );
    assert_eq!(
        scene.depth.len(),
        w * h,
        "build_vertices: depth field length does not match tiles_w*tiles_h"
    );

    let tile_px = scene.tile_px;
    let ground_w = tile_px * scene.tiles_w as f32;
    let ground_h = tile_px * scene.tiles_h as f32;
    let mut verts = Vec::new();

    // 1. Ground plane, furthest back, full extent, full-bright, UV 0..1.
    push_quad_xz(
        &mut verts,
        [0.0, 0.0],
        [ground_w, ground_h],
        0.0,
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 1.0, 1.0, 1.0],
        kind::GROUND,
    );

    // 2. Tile rows, farthest (row 0 -- module doc) to
    // nearest, with each row's boxes and standing billboards/shadows
    // interleaved. Bounded ranges only (law 8): `row` and `col` both walk
    // a fixed, known-finite range, so this terminates regardless of
    // `solid`/`depth` content.
    for row in 0..h {
        for col in 0..w {
            let i = row * w + col;
            if scene.solid[i] == 1 {
                push_box(
                    &mut verts,
                    row,
                    col,
                    w,
                    h,
                    tile_px,
                    wall_height_px(scene.depth[i], tile_px),
                );
            }
        }
        let row_z_lo = row as f32 * tile_px;
        let row_z_hi = row_z_lo + tile_px;
        for b in scene.billboards {
            if b.center_z_px >= row_z_lo && b.center_z_px < row_z_hi {
                push_shadow(&mut verts, *b);
                push_billboard(&mut verts, *b);
            }
        }
    }
    // Billboards outside every row's Z range (e.g. exactly on the grid's
    // far edge, or a level with zero rows) still get drawn -- nearest,
    // since a boundary billboard is more likely walking toward the camera
    // than receding from it, and drawing it last (rather than dropping it)
    // is the fail-safe direction.
    for b in scene.billboards {
        let in_any_row = (0..h).any(|row| {
            let lo = row as f32 * tile_px;
            b.center_z_px >= lo && b.center_z_px < lo + tile_px
        });
        if !in_any_row {
            push_shadow(&mut verts, *b);
            push_billboard(&mut verts, *b);
        }
    }

    verts
}

/// [`build_vertices`] for a camera that has yawed or moved off-centre
/// (module doc, "Orbiting cameras"). `yaw_deg` is the camera's orbit
/// angle (billboards face it); `eye_xz` is its world-pixel `(x, z)`
/// ground position, from `crate::diorama::camera_matrices::placement`.
///
/// Billboards are **cylindrical**: each is a vertical quad (top and bottom
/// edges share the same ground x/z) whose width runs along the screen-right
/// direction for `yaw_deg`, so it turns to face the camera's yaw and never
/// leans back with the pitch.
///
/// # Panics
/// Same length checks as [`build_vertices`].
#[must_use]
pub fn build_vertices_oriented(
    scene: &DioramaScene<'_>,
    yaw_deg: f32,
    eye_xz: [f32; 2],
) -> Vec<Vertex> {
    let (w, h) = (scene.tiles_w as usize, scene.tiles_h as usize);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    assert_eq!(
        scene.solid.len(),
        w * h,
        "build_vertices_oriented: solid mask length does not match tiles_w*tiles_h"
    );
    assert_eq!(
        scene.depth.len(),
        w * h,
        "build_vertices_oriented: depth field length does not match tiles_w*tiles_h"
    );
    let tile_px = scene.tile_px;
    let ground_w = tile_px * scene.tiles_w as f32;
    let ground_h = tile_px * scene.tiles_h as f32;
    let mut verts = Vec::new();
    push_quad_xz(
        &mut verts,
        [0.0, 0.0],
        [ground_w, ground_h],
        0.0,
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 1.0, 1.0, 1.0],
        kind::GROUND,
    );

    enum Item {
        Box { row: usize, col: usize },
        Sprite(usize),
    }
    let dist = |x: f32, z: f32| (x - eye_xz[0]).abs() + (z - eye_xz[1]).abs();
    // (far-first key, boxes before sprites on a tie, index for a stable order, item)
    let mut items: Vec<(f32, u8, usize, Item)> = Vec::new();
    // One pass over a fixed-size grid and one over the billboard slice:
    // both are plain bounded iterators (law 8).
    for row in 0..h {
        for col in 0..w {
            if scene.solid[row * w + col] == 1 {
                let cx = (col as f32 + 0.5) * tile_px;
                let cz = (row as f32 + 0.5) * tile_px;
                items.push((dist(cx, cz), 0, row * w + col, Item::Box { row, col }));
            }
        }
    }
    for (i, b) in scene.billboards.iter().enumerate() {
        items.push((dist(b.center_x_px, b.center_z_px), 1, i, Item::Sprite(i)));
    }
    // Far first. `total_cmp` keeps this a total order even for a NaN.
    items.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (_, _, _, item) in items {
        match item {
            Item::Box { row, col } => {
                let i = row * w + col;
                push_box_visible(
                    &mut verts,
                    row,
                    col,
                    (w, h),
                    tile_px,
                    wall_height_px(scene.depth[i], tile_px),
                    eye_xz,
                );
            }
            Item::Sprite(i) => {
                let b = scene.billboards[i];
                push_shadow(&mut verts, b);
                push_billboard_facing(&mut verts, b, yaw_deg);
            }
        }
    }
    verts
}

/// A cylindrical billboard: the quad's two bottom corners sit `width/2`
/// either side of the footprint centre along the screen-right direction
/// `(cos yaw, -sin yaw)` (world X at yaw 0, matching [`push_billboard`]),
/// and the top edge is directly above them.
fn push_billboard_facing(out: &mut Vec<Vertex>, b: Billboard, yaw_deg: f32) {
    let yaw = yaw_deg.to_radians();
    let (rx, rz) = (yaw.cos() * b.width_px / 2.0, yaw.sin() * b.width_px / 2.0);
    push_quad_vertical_xz(
        out,
        [b.center_x_px - rx, 0.0, b.center_z_px + rz],
        [b.center_x_px + rx, 0.0, b.center_z_px - rz],
        0.0,
        b.height_px,
        b.uv,
        [1.0, 1.0, 1.0, 1.0],
        kind::SPRITE,
    );
}

/// [`push_box`] with every side the eye can see (a face is visible when the
/// eye is on its outward side) instead of the fixed front/right pair.
fn push_box_visible(
    out: &mut Vec<Vertex>,
    row: usize,
    col: usize,
    grid: (usize, usize),
    tile_px: f32,
    height_px: f32,
    eye_xz: [f32; 2],
) {
    if height_px <= 0.0 {
        return;
    }
    let (x0, z0) = (col as f32 * tile_px, row as f32 * tile_px);
    let (x1, z1) = (x0 + tile_px, z0 + tile_px);
    push_quad_xz(
        out,
        [x0, z0],
        [tile_px, tile_px],
        height_px,
        box_top_uv(row, col, grid.0, grid.1),
        [1.0, 1.0, 1.0, 1.0],
        kind::GROUND,
    );
    let side_color = [SIDE_SHADE, SIDE_SHADE, SIDE_SHADE, 1.0];
    let mut side = |p0: [f32; 3], p1: [f32; 3]| {
        push_quad_vertical_xz(
            out,
            p0,
            p1,
            0.0,
            height_px,
            [0.0, 0.0, 1.0, 1.0],
            side_color,
            kind::SIDE,
        );
    };
    if eye_xz[1] < z0 {
        side([x0, 0.0, z0], [x1, 0.0, z0]); // back, -Z
    }
    if eye_xz[1] > z1 {
        side([x0, 0.0, z1], [x1, 0.0, z1]); // front, +Z
    }
    if eye_xz[0] < x0 {
        side([x0, 0.0, z0], [x0, 0.0, z1]); // left, -X
    }
    if eye_xz[0] > x1 {
        side([x1, 0.0, z0], [x1, 0.0, z1]); // right, +X
    }
}

/// Two triangles (6 vertices, no index buffer -- this crate's other
/// passes are all single-fullscreen-triangle draws with no precedent for
/// indexed geometry, and a diorama's vertex count is small enough that
/// the duplication cost is not worth a new buffer type) covering an
/// axis-aligned XZ rectangle at height `y`.
#[allow(clippy::too_many_arguments)]
fn push_quad_xz(
    out: &mut Vec<Vertex>,
    origin_xz: [f32; 2],
    size_xz: [f32; 2],
    y: f32,
    uv_rect: [f32; 4],
    color: [f32; 4],
    kind: u32,
) {
    let [x0, z0] = origin_xz;
    let [sx, sz] = size_xz;
    let (x1, z1) = (x0 + sx, z0 + sz);
    let [u0, v0, u1, v1] = uv_rect;
    let p = |x: f32, z: f32, u: f32, v: f32| Vertex {
        pos: [x, y, z],
        uv: [u, v],
        color,
        kind,
    };
    out.extend_from_slice(&[
        p(x0, z0, u0, v0),
        p(x1, z0, u1, v0),
        p(x1, z1, u1, v1),
        p(x0, z0, u0, v0),
        p(x1, z1, u1, v1),
        p(x0, z1, u0, v1),
    ]);
}

/// A vertical rectangle (for a billboard or a box side face), facing the
/// `+/-Z` or `+/-X` axis depending on which two coordinates vary.
#[allow(clippy::too_many_arguments)]
fn push_quad_vertical_xz(
    out: &mut Vec<Vertex>,
    p0: [f32; 3],
    p1: [f32; 3],
    y0: f32,
    y1: f32,
    uv_rect: [f32; 4],
    color: [f32; 4],
    kind: u32,
) {
    let [u0, v0, u1, v1] = uv_rect;
    let a = Vertex {
        pos: [p0[0], y0, p0[2]],
        uv: [u0, v1],
        color,
        kind,
    };
    let b = Vertex {
        pos: [p1[0], y0, p1[2]],
        uv: [u1, v1],
        color,
        kind,
    };
    let c = Vertex {
        pos: [p1[0], y1, p1[2]],
        uv: [u1, v0],
        color,
        kind,
    };
    let d = Vertex {
        pos: [p0[0], y1, p0[2]],
        uv: [u0, v0],
        color,
        kind,
    };
    out.extend_from_slice(&[a, b, c, a, c, d]);
}

/// Box-top UV: the ground texture spans the whole grid `[0, 1]` (module
/// doc), so a tile's own UV rect is just its row/col fraction of the
/// grid.
fn box_top_uv(row: usize, col: usize, tiles_w: usize, tiles_h: usize) -> [f32; 4] {
    let u0 = col as f32 / tiles_w as f32;
    let v0 = row as f32 / tiles_h as f32;
    let u1 = (col + 1) as f32 / tiles_w as f32;
    let v1 = (row + 1) as f32 / tiles_h as f32;
    [u0, v0, u1, v1]
}

fn push_shadow(out: &mut Vec<Vertex>, b: Billboard) {
    // A flat quad on the ground, slightly larger than the billboard's own
    // footprint width so the shadow reads as "under" rather than
    // "exactly the same size as" the sprite -- 1.3x is an
    // arbitrary-but-documented visual choice (this file's own `SIDE_SHADE`
    // precedent), not a measured light-source calculation (no light
    // source is modelled at all; this is a contact-shadow convention, not
    // a shadow map).
    let half_w = b.width_px * 0.65;
    let half_d = b.width_px * 0.35;
    push_quad_xz(
        out,
        [b.center_x_px - half_w, b.center_z_px - half_d],
        [half_w * 2.0, half_d * 2.0],
        0.05, // a hair above the ground plane to avoid z-fighting-by-alpha-order with it
        [0.0, 0.0, 1.0, 1.0],
        [0.0, 0.0, 0.0, 0.45],
        kind::SHADOW,
    );
}

fn push_billboard(out: &mut Vec<Vertex>, b: Billboard) {
    // Upright: the quad spans X at the footprint's centre and rises in Y,
    // flat against a single Z (input rules note: "input and hit-testing
    // stay strictly 2D", so the billboard has no rotation to face the
    // camera beyond always standing in the XY plane at its own Z --
    // §5's "only the camera pitches").
    push_quad_vertical_xz(
        out,
        [b.center_x_px - b.width_px / 2.0, 0.0, b.center_z_px],
        [b.center_x_px + b.width_px / 2.0, 0.0, b.center_z_px],
        0.0,
        b.height_px,
        b.uv,
        [1.0, 1.0, 1.0, 1.0],
        kind::SPRITE,
    );
}

/// One solid tile's box: a textured top face plus two visible side faces
/// (front, facing the camera i.e. the `+Z`/nearest side; and right, `+X`)
/// — the two faces a fixed pitched camera looking from `+Z`/`+Y` can
/// actually see, so the other two (back, left) are skipped rather than
/// drawn and wasted.
#[allow(clippy::too_many_arguments)]
fn push_box(
    out: &mut Vec<Vertex>,
    row: usize,
    col: usize,
    tiles_w: usize,
    tiles_h: usize,
    tile_px: f32,
    height_px: f32,
) {
    if height_px <= 0.0 {
        return;
    }
    let (x0, z0) = (col as f32 * tile_px, row as f32 * tile_px);
    let (x1, z1) = (x0 + tile_px, z0 + tile_px);
    let top_uv = box_top_uv(row, col, tiles_w, tiles_h);

    // Top face.
    push_quad_xz(
        out,
        [x0, z0],
        [tile_px, tile_px],
        height_px,
        top_uv,
        [1.0, 1.0, 1.0, 1.0],
        kind::GROUND,
    );
    let side_color = [SIDE_SHADE, SIDE_SHADE, SIDE_SHADE, 1.0];
    // Front face (nearest the camera -- the `+Z`/higher-row edge, `z1`).
    push_quad_vertical_xz(
        out,
        [x0, 0.0, z1],
        [x1, 0.0, z1],
        0.0,
        height_px,
        [0.0, 0.0, 1.0, 1.0],
        side_color,
        kind::SIDE,
    );
    // Right face (`+X`/higher-col edge, `x1`).
    push_quad_vertical_xz(
        out,
        [x1, 0.0, z0],
        [x1, 0.0, z1],
        0.0,
        height_px,
        [0.0, 0.0, 1.0, 1.0],
        side_color,
        kind::SIDE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_solid_tile() -> (Vec<u8>, Vec<u8>) {
        (vec![1], vec![8])
    }

    #[test]
    fn empty_grid_produces_no_vertices() {
        let scene = DioramaScene {
            tiles_w: 0,
            tiles_h: 0,
            tile_px: 16.0,
            solid: &[],
            depth: &[],
            billboards: &[],
        };
        assert_eq!(build_vertices(&scene), Vec::new());
    }

    #[test]
    fn a_single_open_tile_emits_only_the_ground_plane() {
        let scene = DioramaScene {
            tiles_w: 1,
            tiles_h: 1,
            tile_px: 16.0,
            solid: &[0],
            depth: &[0],
            billboards: &[],
        };
        let verts = build_vertices(&scene);
        assert_eq!(
            verts.len(),
            6,
            "one quad = 6 vertices, no box, no billboard"
        );
        assert!(verts.iter().all(|v| v.kind == kind::GROUND));
    }

    #[test]
    #[should_panic(expected = "solid mask length")]
    fn mismatched_solid_length_panics_rather_than_guessing() {
        let scene = DioramaScene {
            tiles_w: 2,
            tiles_h: 2,
            tile_px: 16.0,
            solid: &[1, 0, 1], // 3, not 4
            depth: &[0, 0, 0, 0],
            billboards: &[],
        };
        let _ = build_vertices(&scene);
    }

    #[test]
    fn a_solid_tile_emits_ground_plus_a_box_top_and_two_sides() {
        let (solid, depth) = one_solid_tile();
        let scene = DioramaScene {
            tiles_w: 1,
            tiles_h: 1,
            tile_px: 16.0,
            solid: &solid,
            depth: &depth,
            billboards: &[],
        };
        let verts = build_vertices(&scene);
        // ground (6) + top (6) + front (6) + right (6)
        assert_eq!(verts.len(), 24);
        let box_top_y: Vec<f32> = verts[6..12].iter().map(|v| v.pos[1]).collect();
        assert!(
            box_top_y.iter().all(|&y| (y - 16.0).abs() < 1e-6),
            "a fully-solid tile (depth=8 eighths) extrudes exactly one tile edge tall: {box_top_y:?}"
        );
        let sides_darker_than_top = verts[12..24]
            .iter()
            .all(|v| v.color[0] < 1.0 && v.kind == kind::SIDE);
        assert!(
            sides_darker_than_top,
            "side faces must be shaded, not full-bright"
        );
    }

    #[test]
    fn a_billboard_produces_a_shadow_then_the_sprite_quad_in_that_order() {
        let scene = DioramaScene {
            tiles_w: 2,
            tiles_h: 2,
            tile_px: 16.0,
            solid: &[0, 0, 0, 0],
            depth: &[0, 0, 0, 0],
            billboards: &[Billboard {
                center_x_px: 8.0,
                center_z_px: 8.0, // row 0
                width_px: 8.0,
                height_px: 16.0,
                uv: [0.0, 0.0, 1.0, 1.0],
            }],
        };
        let verts = build_vertices(&scene);
        // ground(6) + row0: shadow(6) + sprite(6) + row1 has nothing
        assert_eq!(verts.len(), 18);
        assert!(verts[6..12].iter().all(|v| v.kind == kind::SHADOW));
        assert!(verts[12..18].iter().all(|v| v.kind == kind::SPRITE));
    }

    #[test]
    fn farther_rows_are_emitted_before_nearer_rows() {
        // Two solid tiles, one per row -- row 0 (farther, module doc) must
        // appear in the vertex stream before row 1 (nearer).
        let scene = DioramaScene {
            tiles_w: 1,
            tiles_h: 2,
            tile_px: 16.0,
            solid: &[1, 1], // row0=solid, row1=solid (row-major: index = row*w+col)
            depth: &[8, 8],
            billboards: &[],
        };
        let verts = build_vertices(&scene);
        // ground(6), then row0's box (top+front+right=18), then row1's box(18)
        assert_eq!(verts.len(), 6 + 18 + 18);
        let row0_z: f32 = verts[6].pos[2];
        let row1_z: f32 = verts[24].pos[2];
        assert!(
            row1_z > row0_z,
            "row 0 (farther) must be emitted before row 1 (nearer): row0_z={row0_z} row1_z={row1_z}"
        );
    }

    // --- ticket W30-05: orbiting-camera mesh ---------------------------

    fn one_sprite(x: f32, z: f32) -> Billboard {
        Billboard {
            center_x_px: x,
            center_z_px: z,
            width_px: 8.0,
            height_px: 16.0,
            uv: [0.0, 0.0, 1.0, 1.0],
        }
    }

    fn sprite_quad(yaw: f32) -> Vec<Vertex> {
        let scene = DioramaScene {
            tiles_w: 4,
            tiles_h: 4,
            tile_px: 16.0,
            solid: &[0; 16],
            depth: &[0; 16],
            billboards: &[one_sprite(32.0, 32.0)],
        };
        let verts = build_vertices_oriented(&scene, yaw, [32.0, 200.0]);
        verts
            .into_iter()
            .filter(|v| v.kind == kind::SPRITE)
            .collect()
    }

    #[test]
    fn billboards_stay_vertical_at_every_yaw() {
        for yaw in [-30.0f32, 0.0, 17.0, 30.0] {
            let q = sprite_quad(yaw);
            assert_eq!(q.len(), 6);
            // Every vertex with y == 0 has a partner at y == height with the
            // SAME ground x/z: the quad is perpendicular to the ground.
            for v in q.iter().filter(|v| v.pos[1] == 0.0) {
                assert!(
                    q.iter().any(|t| t.pos[1] == 16.0
                        && (t.pos[0] - v.pos[0]).abs() < 1e-5
                        && (t.pos[2] - v.pos[2]).abs() < 1e-5),
                    "yaw {yaw}: bottom vertex {:?} has no vertex straight above it",
                    v.pos
                );
            }
            assert!(q.iter().all(|v| v.pos[1] == 0.0 || v.pos[1] == 16.0));
        }
    }

    /// W30-05b: a billboard stands ON its footprint -- the bottom edge is at
    /// ground height (y = 0) at the footprint's own z, centred on its x, and
    /// the quad rises from there; nothing hangs below the ground.
    #[test]
    fn a_billboard_stands_with_its_feet_on_the_footprint() {
        let scene = DioramaScene {
            tiles_w: 4,
            tiles_h: 4,
            tile_px: 16.0,
            solid: &[0; 16],
            depth: &[0; 16],
            billboards: &[one_sprite(32.0, 40.0)],
        };
        for verts in [
            build_vertices(&scene),
            build_vertices_oriented(&scene, 0.0, [32.0, 200.0]),
        ] {
            let quad: Vec<&Vertex> = verts.iter().filter(|v| v.kind == kind::SPRITE).collect();
            assert_eq!(quad.len(), 6);
            let bottoms: Vec<&&Vertex> = quad.iter().filter(|v| v.pos[1] == 0.0).collect();
            assert!(!bottoms.is_empty());
            assert!(bottoms.iter().all(|v| (v.pos[2] - 40.0).abs() < 1e-5));
            let (lo, hi) = bottoms.iter().fold((f32::MAX, f32::MIN), |a, v| {
                (a.0.min(v.pos[0]), a.1.max(v.pos[0]))
            });
            assert_eq!((lo, hi), (28.0, 36.0), "8 wide, centred on the footprint x");
            assert!(quad.iter().all(|v| v.pos[1] == 0.0 || v.pos[1] == 16.0));
            assert!(quad.iter().any(|v| v.pos[1] == 16.0), "rises by height_px");
        }
    }

    #[test]
    fn a_billboard_at_yaw_zero_matches_the_original_upright_quad() {
        let scene = DioramaScene {
            tiles_w: 4,
            tiles_h: 4,
            tile_px: 16.0,
            solid: &[0; 16],
            depth: &[0; 16],
            billboards: &[one_sprite(32.0, 32.0)],
        };
        let old: Vec<Vertex> = build_vertices(&scene)
            .into_iter()
            .filter(|v| v.kind == kind::SPRITE)
            .collect();
        assert_eq!(sprite_quad(0.0), old);
    }

    #[test]
    fn a_billboard_turns_to_face_the_camera_yaw() {
        // Width runs along (cos yaw, -sin yaw): at yaw 30 the right edge is
        // further in -Z than the left, and the quad is still 8 wide.
        let q = sprite_quad(30.0);
        let bottoms: Vec<[f32; 3]> = q
            .iter()
            .filter(|v| v.pos[1] == 0.0)
            .map(|v| v.pos)
            .collect();
        let (xs, zs) = (
            bottoms
                .iter()
                .map(|p| p[0])
                .fold((f32::MAX, f32::MIN), |a, x| (a.0.min(x), a.1.max(x))),
            bottoms
                .iter()
                .map(|p| p[2])
                .fold((f32::MAX, f32::MIN), |a, z| (a.0.min(z), a.1.max(z))),
        );
        let (dx, dz) = (xs.1 - xs.0, zs.1 - zs.0);
        assert!(
            (dx - 8.0 * 30f32.to_radians().cos()).abs() < 1e-4,
            "dx {dx}"
        );
        assert!(
            (dz - 8.0 * 30f32.to_radians().sin()).abs() < 1e-4,
            "dz {dz}"
        );
        assert!(((dx * dx + dz * dz).sqrt() - 8.0).abs() < 1e-4);
    }

    #[test]
    fn a_nearer_wall_is_drawn_after_a_farther_one_when_the_eye_moves_sideways() {
        // Two walls on one row. With the eye far off to +X the col-1 wall
        // is the nearer one, so col 0 is emitted first; mirror the eye and
        // the order flips (the row-ordered builder cannot do that).
        let solid = [1u8, 1];
        let depth = [8u8, 8];
        let scene = DioramaScene {
            tiles_w: 2,
            tiles_h: 1,
            tile_px: 16.0,
            solid: &solid,
            depth: &depth,
            billboards: &[],
        };
        let tops = |eye_x: f32| -> Vec<f32> {
            build_vertices_oriented(&scene, 0.0, [eye_x, 100.0])
                .iter()
                .filter(|v| v.kind == kind::GROUND && v.pos[1] > 0.0)
                .map(|v| v.pos[0])
                .collect()
        };
        assert!(tops(200.0)[0] < 16.0, "eye at +X: col 0 first");
        assert!(tops(-200.0)[0] >= 16.0, "eye at -X: col 1 first");
    }

    #[test]
    fn only_the_sides_the_eye_can_see_are_drawn() {
        let scene = DioramaScene {
            tiles_w: 1,
            tiles_h: 1,
            tile_px: 16.0,
            solid: &[1],
            depth: &[8],
            billboards: &[],
        };
        let sides = |eye: [f32; 2]| {
            build_vertices_oriented(&scene, 0.0, eye)
                .iter()
                .filter(|v| v.kind == kind::SIDE)
                .count()
                / 6
        };
        assert_eq!(sides([8.0, 100.0]), 1, "dead ahead: front only");
        assert_eq!(sides([60.0, 100.0]), 2, "front + the +X side");
        assert_eq!(sides([-60.0, 100.0]), 2, "front + the -X side");
    }
}
