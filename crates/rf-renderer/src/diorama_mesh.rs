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
//! farthest-from-camera to nearest, boxes and billboards/shadows
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
//! Row increases AWAY from the camera (§5's "only the camera pitches";
//! `crate::diorama::camera_matrices`'s fixed placement sits at negative Z,
//! looking toward +Z), which is what makes "farthest row first" the
//! correct back-to-front draw order.

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

    // 2. Tile rows, farthest (highest row index -- module doc) to
    // nearest, with each row's boxes and standing billboards/shadows
    // interleaved. Bounded ranges only (law 8): `row` and `col` both walk
    // a fixed, known-finite range, so this terminates regardless of
    // `solid`/`depth` content.
    for row in (0..h).rev() {
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
/// (front, facing the camera i.e. the `-Z`/nearest side; and right, `+X`)
/// — the two faces a fixed pitched camera looking from `-Z`/`+Y` can
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
    // Front face (nearest the camera -- the `-Z`/lower-row edge, `z0`).
    push_quad_vertical_xz(
        out,
        [x0, 0.0, z0],
        [x1, 0.0, z0],
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
        // ground(6) + row1 has nothing + row0: shadow(6) + sprite(6)
        assert_eq!(verts.len(), 18);
        assert!(verts[6..12].iter().all(|v| v.kind == kind::SHADOW));
        assert!(verts[12..18].iter().all(|v| v.kind == kind::SPRITE));
    }

    #[test]
    fn farther_rows_are_emitted_before_nearer_rows() {
        // Two solid tiles, one per row -- row 1 (farther, module doc) must
        // appear in the vertex stream before row 0 (nearer).
        let scene = DioramaScene {
            tiles_w: 1,
            tiles_h: 2,
            tile_px: 16.0,
            solid: &[1, 1], // row0=solid, row1=solid (row-major: index = row*w+col)
            depth: &[8, 8],
            billboards: &[],
        };
        let verts = build_vertices(&scene);
        // ground(6), then row1's box (top+front+right=18), then row0's box(18)
        assert_eq!(verts.len(), 6 + 18 + 18);
        let row1_z: f32 = verts[6].pos[2];
        let row0_z: f32 = verts[24].pos[2];
        assert!(
            row1_z > row0_z,
            "row 1 (farther) must be emitted before row 0 (nearer): row1_z={row1_z} row0_z={row0_z}"
        );
    }
}
