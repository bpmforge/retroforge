//! The side-view Diorama mesh (ticket W34-04; `docs/design/HD2D_PLAN.md`
//! section 2.5 and D8) -- re-exported from [`crate::diorama_mesh`], where
//! the top-down builder lives.
//!
//! A side-scroller's level is a vertical picture, not a floor. This builder
//! stands that picture upright: world X is the picture's X, world Y is
//! `plane_h - picture_y` (picture row 0 is the highest), and the camera sits
//! on the `+Z` side looking toward `-Z`. The whole picture is one textured
//! plane at `z = -back_px`; every cell the learner called solid is extruded
//! from that plane toward the camera by its depth, so ground and pipes stand
//! out as blocks and everything else (sky, scenery, backdrop) stays on the
//! plane behind them.
//!
//! ## Faces and textures
//!
//! The ground texture is the picture itself, so every UV is a picture
//! coordinate over `plane_w x plane_h`.
//!
//! - **Front** (facing the camera, at `z = depth`): the cell's own pixels,
//!   so a block looks like the tile it came from. Equal-depth cells merge
//!   into rectangles; a rectangle of cells is a contiguous rectangle of the
//!   picture, so the merge changes nothing visible.
//! - **Top**: one pixel row of the front's top edge, stretched back across
//!   the depth (a grass lip stays grass-coloured), lit full.
//! - **Left / right**: the cell's edge pixel column, darkened
//!   ([`SIDE_TINT`]).
//! - A face is emitted only where nothing equally deep touches it, and only
//!   if the eye can see it. All blocks share the plane they grow from, so
//!   no two exposed faces overlap except through perspective; shallower
//!   blocks are emitted first so a deeper one's front draws over them (the
//!   pass has no depth buffer, `crate::diorama_mesh` module doc).
//!
//! ## Ramps
//!
//! A ramp cell is a wedge: the front is a trapezoid cut by the slope, the
//! top slopes with it, and the tall end has a side wall. `slope` is the rise
//! in eighths of a tile across the cell (positive rises to the right). A
//! shallow ramp spread over several cells is drawn as one wedge per cell
//! starting from the floor; the learner does not say where in the run a cell
//! sits.
//!
//! ## Billboards
//!
//! Sprites stand upright in the XY plane at the middle of the depth of the
//! cell under their feet, so a player walking over a block stands on its top
//! face. A sprite the game draws behind the background (OAM priority) stands
//! just behind the picture plane instead, so a tall block and the plane's own
//! opaque pixels hide it through the depth buffer, as they do in the game.
//! Billboards are emitted last.
//!
//! ## Layers (ticket W34-08, D4)
//!
//! The ground texture can be an [`Atlas`]: picture-sized slots stacked one
//! above the other. Slot [`Atlas::playfield`] is the plane the blocks grow
//! from (`z = -back_px`); every [`BackPlane`] is another slot, a whole
//! picture standing further back (a slower-scrolling layer, the sky). Each
//! back plane is scaled about [`SideScene::ref_eye`] by the ratio of its
//! distance to the plane's, so from that eye it covers exactly what it would
//! at `z = 0`: the picture looks the same from the front, and the layers part
//! company when the camera moves (parallax).
//!
//! No loop here is a hand-rolled walk (law 8): every loop is a range or an
//! iterator over a fixed-size grid, and each merge extends by `take_while`
//! over a range, which stops at the grid edge.

use crate::diorama_mesh::{kind, Vertex};

/// Tint of a block's left and right faces.
pub const SIDE_TINT: f32 = 0.62;
/// Tint of the front faces: a hair under full so the top reads as lit.
pub const FRONT_TINT: f32 = 0.94;
/// How far behind its plane the overscan cells stand, picture pixels (the
/// picture's own quad wins the depth test over them).
const OVERSCAN_Z: f32 = 0.02;
/// Depth, in eighths of a tile, below which a cell is flat.
const MIN_DEPTH_EIGHTHS: u8 = 1;

/// One sprite to stand up in the side view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SideBillboard {
    /// Footprint centre X, picture pixels.
    pub center_x_px: f32,
    /// The sprite's bottom edge (its feet), picture pixels, y down.
    pub feet_y_px: f32,
    pub width_px: f32,
    pub height_px: f32,
    /// UV rect (`u0, v0, u1, v1`) into the sprite-cutout texture.
    pub uv: [f32; 4],
    /// The game draws it behind the background (OAM priority 0).
    pub behind_bg: bool,
}

/// A ground texture made of `slots` picture-sized slots stacked vertically,
/// each `pitch_px` texels tall (the picture plus a transparent gap, so linear
/// filtering never reads the slot above or below).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Atlas {
    pub slots: u32,
    pub pitch_px: f32,
    /// The slot the blocks and the main plane use.
    pub playfield: u32,
}

impl Atlas {
    /// One slot exactly as tall as the picture: the whole texture is the
    /// picture.
    #[must_use]
    pub fn single(plane_h: f32) -> Self {
        Self {
            slots: 1,
            pitch_px: plane_h,
            playfield: 0,
        }
    }

    /// The atlas's V for picture row `y` of `slot`.
    #[must_use]
    pub fn v(&self, slot: u32, y: f32) -> f32 {
        (slot as f32 * self.pitch_px + y) / (self.slots.max(1) as f32 * self.pitch_px).max(1.0)
    }
}

/// A whole picture standing further back than the main plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackPlane {
    /// Its slot in the [`Atlas`].
    pub slot: u32,
    /// How far behind the main plane it stands, picture pixels.
    pub back_px: f32,
}

/// What [`build_vertices_side`] needs.
#[derive(Debug, Clone, Copy)]
pub struct SideScene<'a> {
    /// The ground texture's size in picture pixels: the plane covers
    /// `[0, plane_w] x [0, plane_h]`.
    pub plane_w: f32,
    pub plane_h: f32,
    /// Picture position of cell (0, 0)'s top-left corner (negative under
    /// sub-cell scroll; the cell rows of a band start at its first row).
    pub origin: [f32; 2],
    pub tile_px: f32,
    pub cols: u32,
    pub rows: u32,
    /// Extrusion toward the camera in eighths of a tile; 0 is flat.
    /// `cols * rows` entries, row-major.
    pub depth: &'a [u8],
    /// Ramp rise in eighths of a tile across the cell; 0 is a plain block.
    /// Same indexing.
    pub slope: &'a [i8],
    pub billboards: &'a [SideBillboard],
    /// How far behind the blocks' origin the plane sits (set-back
    /// backdrop), picture pixels.
    pub back_px: f32,
    /// The ground texture's layout ([`Atlas::single`] for one picture).
    pub atlas: Atlas,
    /// Planes standing further back than the main one (W34-08); any order.
    pub back_planes: &'a [BackPlane],
    /// The eye the back planes are scaled to match (world space, the
    /// camera straight in front of the target): from there they look
    /// exactly like the flat picture.
    pub ref_eye: [f32; 3],
    /// Ticket W34-09: how far every plane goes on past the picture's edge on
    /// all four sides, picture pixels: sideways as the picture mirrored about
    /// its edge (W35-01; the edge column repeated smeared into streaks), up
    /// and down the edge row repeated; at most one picture's width / height
    /// each way. A pitched or
    /// swung camera looks past the picture's edge; without this it saw the
    /// clear colour there. `0.0` is the picture exactly.
    pub overscan_px: f32,
}

impl SideScene<'_> {
    fn depth_px(&self, c: u32, r: u32) -> f32 {
        let i = (r * self.cols + c) as usize;
        let d = self.depth[i];
        if d < MIN_DEPTH_EIGHTHS {
            0.0
        } else {
            f32::from(d) / 8.0 * self.tile_px
        }
    }

    fn is_block(&self, c: u32, r: u32) -> bool {
        c < self.cols
            && r < self.rows
            && self.depth_px(c, r) > 0.0
            && self.slope[(r * self.cols + c) as usize] == 0
    }

    /// Picture-space rectangle of cell `(c, r)`.
    fn cell_rect(&self, c: u32, r: u32) -> [f32; 4] {
        let x0 = self.origin[0] + c as f32 * self.tile_px;
        let y0 = self.origin[1] + r as f32 * self.tile_px;
        [x0, y0, x0 + self.tile_px, y0 + self.tile_px]
    }

    fn world_y(&self, picture_y: f32) -> f32 {
        self.plane_h - picture_y
    }

    fn u(&self, x: f32) -> f32 {
        x / self.plane_w.max(1.0)
    }

    fn v(&self, y: f32) -> f32 {
        self.atlas.v(self.atlas.playfield, y)
    }

    /// Clamp a picture rectangle to the plane; `None` when nothing is left.
    fn clip(&self, r: [f32; 4]) -> Option<[f32; 4]> {
        let c = [
            r[0].max(0.0),
            r[1].max(0.0),
            r[2].min(self.plane_w),
            r[3].min(self.plane_h),
        ];
        (c[2] > c[0] && c[3] > c[1]).then_some(c)
    }
}

/// A group of triangles drawn together, ordered by the depth they stand at.
struct Piece {
    depth: f32,
    verts: Vec<Vertex>,
}

fn quad(out: &mut Vec<Vertex>, p: [[f32; 3]; 4], uv: [[f32; 2]; 4], tint: f32, k: u32) {
    let v = |i: usize| Vertex {
        pos: p[i],
        uv: uv[i],
        color: [tint, tint, tint, 1.0],
        kind: k,
    };
    out.extend_from_slice(&[v(0), v(1), v(2), v(0), v(2), v(3)]);
}

/// Build one frame of the side-view diorama for an eye at `eye` (world
/// space, from the matching camera placement), in draw order.
///
/// # Panics
/// If `depth` or `slope` is not `cols * rows` long (a caller bug, as in
/// [`crate::diorama_mesh::build_vertices`]).
#[must_use]
pub fn build_vertices_side(scene: &SideScene<'_>, eye: [f32; 3]) -> Vec<Vertex> {
    let n = scene.cols as usize * scene.rows as usize;
    assert_eq!(scene.depth.len(), n, "build_vertices_side: depth length");
    assert_eq!(scene.slope.len(), n, "build_vertices_side: slope length");
    if scene.plane_w <= 0.0 || scene.plane_h <= 0.0 {
        return Vec::new();
    }
    let back = scene.back_px.max(0.0);
    let mut verts = Vec::new();
    // The planes standing further back, furthest first.
    let mut planes: Vec<BackPlane> = scene.back_planes.to_vec();
    planes.sort_by(|a, b| b.back_px.total_cmp(&a.back_px));
    for bp in &planes {
        push_back_plane(scene, bp, &mut verts);
    }
    // The plane: the whole picture, upright.
    push_plane(
        scene,
        scene.atlas.playfield,
        &|x, y| [x, y, -back],
        kind::PLANE,
        &mut verts,
    );

    let mut pieces: Vec<Piece> = Vec::new();
    merge_fronts(scene, eye, back, &mut pieces);
    tops_and_sides(scene, eye, back, &mut pieces);
    for r in 0..scene.rows {
        for c in 0..scene.cols {
            let i = (r * scene.cols + c) as usize;
            if scene.slope[i] != 0 && scene.depth_px(c, r) > 0.0 {
                push_wedge(scene, c, r, eye, back, &mut pieces);
            }
        }
    }
    // Shallow first; the sort is stable so equal depths keep scan order.
    pieces.sort_by(|a, b| a.depth.total_cmp(&b.depth));
    for p in pieces {
        verts.extend(p.verts);
    }
    for b in scene.billboards {
        push_billboard(scene, b, back, &mut verts);
    }
    verts
}

/// One back plane: the whole picture of its atlas slot at `z = -back_px`,
/// scaled about the reference eye so it covers what the main plane covers
/// as seen from there.
fn push_back_plane(scene: &SideScene<'_>, bp: &BackPlane, out: &mut Vec<Vertex>) {
    let z = -(scene.back_px.max(0.0) + bp.back_px.max(0.0));
    let e = scene.ref_eye;
    // The ray from the eye through a point at `z = -back_px` of the main
    // plane meets this plane at the scaled point.
    let near = e[2] + scene.back_px.max(0.0);
    let far = e[2] - z;
    let k = if near > 1.0 && far > near {
        far / near
    } else {
        1.0
    };
    let at = |x: f32, y: f32| [e[0] + (x - e[0]) * k, e[1] + (y - e[1]) * k, z];
    push_plane(scene, bp.slot, &at, kind::BACK, out);
}

/// One plane of the atlas: the whole picture of `slot`, upright, each of its
/// world points `(x, y)` (y up) placed by `place`. With
/// [`SideScene::overscan_px`] it is a 3 x 3 grid whose outer cells show the
/// picture mirrored about its left and right edges and its top and bottom
/// rows repeated (half a texel in, so filtering never reads the transparent
/// gap between two slots).
fn push_plane(
    scene: &SideScene<'_>,
    slot: u32,
    place: &dyn Fn(f32, f32) -> [f32; 3],
    k: u32,
    out: &mut Vec<Vertex>,
) {
    let (w, h) = (scene.plane_w, scene.plane_h);
    let e = scene.overscan_px.max(0.0);
    if e <= 0.0 {
        quad(
            out,
            [place(0.0, h), place(w, h), place(w, 0.0), place(0.0, 0.0)],
            [
                [0.0, scene.atlas.v(slot, 0.0)],
                [1.0, scene.atlas.v(slot, 0.0)],
                [1.0, scene.atlas.v(slot, h)],
                [0.0, scene.atlas.v(slot, h)],
            ],
            1.0,
            k,
        );
        return;
    }
    // The eight outer cells (picture coordinates, y down), each running 1 px
    // in under the picture so the picture's own edge pixels, which filtering
    // blends half-way into the transparent gap, lie over cells of their own
    // colour: they are drawn first and a hair further back, so the picture
    // wins wherever they overlap. W35-01: past the LEFT and RIGHT edges a
    // cell shows the picture mirrored about that edge (the edge column
    // stretched sideways was long horizontal streaks through the hills and
    // sky), so a swung camera sees the scenery go on. Past the TOP and BOTTOM
    // the edge row is repeated (sky above, ground below: a mirrored ground
    // would show its grass upside down). A side cell is at most one picture
    // wide, which is as far as a mirror reaches.
    let (ex, ey) = (e.min(w), e.min(h));
    let spans_x = [(-ex, 1.0), (0.0, w), (w - 1.0, w + ex)];
    let spans_y = [(-ey, 1.0), (0.0, h), (h - 1.0, h + ey)];
    // The texel a point of a cell reads, as a fraction of the picture, half
    // a texel in from the border: sideways, itself inside the picture and its
    // reflection about the nearest edge outside it.
    let mirror = |i: usize, p: f32, dim: f32| {
        let q = match i {
            0 => -p,
            2 => 2.0 * dim - p,
            _ => p,
        };
        q.clamp(0.5, dim - 0.5) / dim
    };
    // Up and down: the edge texel repeated.
    let pin = |j: usize, p: f32, dim: f32| match j {
        0 => 0.5 / dim,
        2 => (dim - 0.5) / dim,
        _ => p.clamp(0.5, dim - 0.5) / dim,
    };
    let world = |px: f32, py: f32| place(px, h - py);
    let behind = |p: [f32; 3]| [p[0], p[1], p[2] - OVERSCAN_Z];
    for (j, &(y0, y1)) in spans_y.iter().enumerate() {
        for (i, &(x0, x1)) in spans_x.iter().enumerate() {
            if (i, j) == (1, 1) {
                continue;
            }
            let c = |px: f32, py: f32| [mirror(i, px, w), scene.atlas.v(slot, pin(j, py, h) * h)];
            quad(
                out,
                [
                    behind(world(x0, y0)),
                    behind(world(x1, y0)),
                    behind(world(x1, y1)),
                    behind(world(x0, y1)),
                ],
                [c(x0, y0), c(x1, y0), c(x1, y1), c(x0, y1)],
                1.0,
                k,
            );
        }
    }
    quad(
        out,
        [place(0.0, h), place(w, h), place(w, 0.0), place(0.0, 0.0)],
        [
            [0.0, scene.atlas.v(slot, 0.0)],
            [1.0, scene.atlas.v(slot, 0.0)],
            [1.0, scene.atlas.v(slot, h)],
            [0.0, scene.atlas.v(slot, h)],
        ],
        1.0,
        k,
    );
}

/// Greedy rectangles of equal-depth plain blocks, one front quad each.
fn merge_fronts(scene: &SideScene<'_>, eye: [f32; 3], back: f32, out: &mut Vec<Piece>) {
    let _ = back;
    let mut taken = vec![false; scene.cols as usize * scene.rows as usize];
    let at = |c: u32, r: u32| (r * scene.cols + c) as usize;
    for r in 0..scene.rows {
        for c in 0..scene.cols {
            if taken[at(c, r)] || !scene.is_block(c, r) {
                continue;
            }
            let d = scene.depth_px(c, r);
            let free = |taken: &[bool], cc: u32, rr: u32| {
                !taken[at(cc, rr)] && scene.is_block(cc, rr) && scene.depth_px(cc, rr) == d
            };
            // Both counts are at least 1 (the cell itself passes `free`) and
            // stop at the grid edge, so the rectangle is never empty and
            // the scan below always moves past it.
            let run = (c..scene.cols).take_while(|&k| free(&taken, k, r)).count() as u32;
            let tall = (r..scene.rows)
                .take_while(|&rr| (c..c + run).all(|k| free(&taken, k, rr)))
                .count() as u32;
            for rr in r..r + tall {
                for k in c..c + run {
                    taken[at(k, rr)] = true;
                }
            }
            let a = scene.cell_rect(c, r);
            let b = scene.cell_rect(c + run - 1, r + tall - 1);
            let rect = [a[0], a[1], b[2], b[3]];
            let Some(cl) = scene.clip(rect) else {
                continue;
            };
            if eye[2] <= d {
                continue;
            }
            let (yt, yb) = (scene.world_y(cl[1]), scene.world_y(cl[3]));
            let (u0, u1, v0, v1) = (
                scene.u(cl[0]),
                scene.u(cl[2]),
                scene.v(cl[1]),
                scene.v(cl[3]),
            );
            let mut verts = Vec::with_capacity(6);
            quad(
                &mut verts,
                [
                    [cl[0], yt, d],
                    [cl[2], yt, d],
                    [cl[2], yb, d],
                    [cl[0], yb, d],
                ],
                [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
                FRONT_TINT,
                kind::GROUND,
            );
            out.push(Piece { depth: d, verts });
        }
    }
}

/// Top faces (per-row runs) and left/right faces (per-column runs) of the
/// plain blocks, wherever a block of at least that depth does not touch.
fn tops_and_sides(scene: &SideScene<'_>, eye: [f32; 3], back: f32, out: &mut Vec<Piece>) {
    let covered = |c: i64, r: i64, d: f32| {
        c >= 0
            && r >= 0
            && scene.is_block(c as u32, r as u32)
            && scene.depth_px(c as u32, r as u32) >= d
    };
    // Tops: runs along each row of equal-depth cells with an open top.
    for r in 0..scene.rows {
        let open = |c: u32| {
            scene.is_block(c, r) && !covered(i64::from(c), i64::from(r) - 1, scene.depth_px(c, r))
        };
        let mut used = vec![false; scene.cols as usize];
        for c in 0..scene.cols {
            if used[c as usize] || !open(c) {
                continue;
            }
            let d = scene.depth_px(c, r);
            let run = (c..scene.cols)
                .take_while(|&k| !used[k as usize] && open(k) && scene.depth_px(k, r) == d)
                .count() as u32;
            for k in c..c + run {
                used[k as usize] = true;
            }
            let (a, b) = (scene.cell_rect(c, r), scene.cell_rect(c + run - 1, r));
            let Some(cl) = scene.clip([a[0], a[1], b[2], a[1] + 1.0]) else {
                continue;
            };
            let top = scene.world_y(a[1]);
            if a[1] < 0.0 || eye[1] <= top {
                continue; // The top edge is off the picture, or seen from below.
            }
            let (u0, u1) = (scene.u(cl[0]), scene.u(cl[2]));
            let v = scene.v(cl[1] + 0.5);
            let mut verts = Vec::with_capacity(6);
            quad(
                &mut verts,
                [
                    [cl[0], top, -back],
                    [cl[2], top, -back],
                    [cl[2], top, d],
                    [cl[0], top, d],
                ],
                [[u0, v], [u1, v], [u1, v], [u0, v]],
                1.0,
                kind::GROUND,
            );
            out.push(Piece { depth: d, verts });
        }
    }
    // Sides: runs down each column, left face then right face.
    for c in 0..scene.cols {
        for side in [-1i64, 1] {
            let open = |r: u32| {
                scene.is_block(c, r)
                    && !covered(i64::from(c) + side, i64::from(r), scene.depth_px(c, r))
            };
            let mut used = vec![false; scene.rows as usize];
            for r in 0..scene.rows {
                if used[r as usize] || !open(r) {
                    continue;
                }
                let d = scene.depth_px(c, r);
                let run = (r..scene.rows)
                    .take_while(|&k| !used[k as usize] && open(k) && scene.depth_px(c, k) == d)
                    .count() as u32;
                for k in r..r + run {
                    used[k as usize] = true;
                }
                let (a, b) = (scene.cell_rect(c, r), scene.cell_rect(c, r + run - 1));
                let x = if side < 0 { a[0] } else { a[2] };
                let seen = if side < 0 { eye[0] < x } else { eye[0] > x };
                if !seen || x < 0.0 || x > scene.plane_w {
                    continue;
                }
                let (y0, y1) = (a[1].max(0.0), b[3].min(scene.plane_h));
                if y1 <= y0 {
                    continue;
                }
                // The pixel column just inside the cell.
                let inside = if side < 0 { x + 0.5 } else { x - 0.5 };
                let u = scene.u(inside);
                let (v0, v1) = (scene.v(y0), scene.v(y1));
                let (yt, yb) = (scene.world_y(y0), scene.world_y(y1));
                let mut verts = Vec::with_capacity(6);
                quad(
                    &mut verts,
                    [[x, yt, -back], [x, yt, d], [x, yb, d], [x, yb, -back]],
                    [[u, v0], [u, v0], [u, v1], [u, v1]],
                    SIDE_TINT,
                    kind::GROUND,
                );
                out.push(Piece { depth: d, verts });
            }
        }
    }
}

/// One ramp cell as a wedge (module doc, "Ramps").
fn push_wedge(
    scene: &SideScene<'_>,
    c: u32,
    r: u32,
    eye: [f32; 3],
    back: f32,
    out: &mut Vec<Piece>,
) {
    let i = (r * scene.cols + c) as usize;
    let s = scene.slope[i];
    let d = scene.depth_px(c, r);
    let rect = scene.cell_rect(c, r);
    if scene.clip(rect).is_none() {
        return;
    }
    let rise = (f32::from(s.unsigned_abs()).min(8.0) / 8.0) * scene.tile_px;
    // Heights of the solid part above the cell's bottom edge, left and right.
    let (hl, hr) = if s > 0 { (0.0, rise) } else { (rise, 0.0) };
    let (x0, x1) = (rect[0], rect[2]);
    let bottom = rect[3];
    let (yl, yr) = (bottom - hl, bottom - hr); // picture y of the slope's ends
    let clamp_x = |x: f32| x.clamp(0.0, scene.plane_w);
    let mut verts = Vec::new();
    // Front: the trapezoid under the slope.
    if eye[2] > d {
        let p = |x: f32, y: f32| [clamp_x(x), scene.world_y(y.clamp(0.0, scene.plane_h)), d];
        let t = |x: f32, y: f32| [scene.u(clamp_x(x)), scene.v(y.clamp(0.0, scene.plane_h))];
        quad(
            &mut verts,
            [p(x0, yl), p(x1, yr), p(x1, bottom), p(x0, bottom)],
            [t(x0, yl), t(x1, yr), t(x1, bottom), t(x0, bottom)],
            FRONT_TINT,
            kind::GROUND,
        );
    }
    // Top: the slope itself, one pixel row of the art stretched back.
    if eye[1] > scene.world_y(yl.min(yr)) {
        let (xa, xb) = (clamp_x(x0), clamp_x(x1));
        let (ua, ub) = (scene.u(xa), scene.u(xb));
        let v = scene.v((yl.min(yr) + 0.5).clamp(0.0, scene.plane_h));
        let w = |x: f32, y: f32, z: f32| [x, scene.world_y(y.clamp(0.0, scene.plane_h)), z];
        quad(
            &mut verts,
            [
                w(xa, yl, -back),
                w(xb, yr, -back),
                w(xb, yr, d),
                w(xa, yl, d),
            ],
            [[ua, v], [ub, v], [ub, v], [ua, v]],
            1.0,
            kind::GROUND,
        );
    }
    // The wall at the tall end.
    let (tall_x, tall_y, toward) = if s > 0 { (x1, yr, 1.0) } else { (x0, yl, -1.0) };
    if (eye[0] - tall_x) * toward > 0.0 && tall_x >= 0.0 && tall_x <= scene.plane_w {
        let u = scene.u(tall_x - 0.5 * toward);
        let (v0, v1) = (
            scene.v(tall_y.clamp(0.0, scene.plane_h)),
            scene.v(bottom.clamp(0.0, scene.plane_h)),
        );
        let (yt, yb) = (
            scene.world_y(tall_y.clamp(0.0, scene.plane_h)),
            scene.world_y(bottom.clamp(0.0, scene.plane_h)),
        );
        quad(
            &mut verts,
            [
                [tall_x, yt, -back],
                [tall_x, yt, d],
                [tall_x, yb, d],
                [tall_x, yb, -back],
            ],
            [[u, v0], [u, v0], [u, v1], [u, v1]],
            SIDE_TINT,
            kind::GROUND,
        );
    }
    if !verts.is_empty() {
        out.push(Piece { depth: d, verts });
    }
}

/// A contact shadow (when something solid is under the feet) and the
/// upright sprite quad.
fn push_billboard(scene: &SideScene<'_>, b: &SideBillboard, back: f32, out: &mut Vec<Vertex>) {
    // The cell just under the feet decides how deep the stage is.
    let col = ((b.center_x_px - scene.origin[0]) / scene.tile_px).floor();
    let row = ((b.feet_y_px + 1.0 - scene.origin[1]) / scene.tile_px).floor();
    let under = (col >= 0.0 && row >= 0.0 && col < scene.cols as f32 && row < scene.rows as f32)
        .then(|| scene.depth_px(col as u32, row as u32))
        .filter(|d| *d > 0.0);
    let stage = under.unwrap_or(scene.tile_px);
    let z = if b.behind_bg {
        // Just behind the picture plane, in front of every plane behind it.
        let gap = scene
            .back_planes
            .iter()
            .map(|p| p.back_px)
            .filter(|g| *g > 0.0)
            .fold(2.0_f32, f32::min);
        -back - gap / 2.0
    } else {
        (stage - back) / 2.0
    };
    let feet = scene.world_y(b.feet_y_px);
    if under.is_some() {
        let (hw, hd) = (b.width_px * 0.65, b.width_px * 0.35);
        quad(
            out,
            [
                [b.center_x_px - hw, feet + 0.05, z - hd],
                [b.center_x_px + hw, feet + 0.05, z - hd],
                [b.center_x_px + hw, feet + 0.05, z + hd],
                [b.center_x_px - hw, feet + 0.05, z + hd],
            ],
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            0.0,
            kind::SHADOW,
        );
        // The shadow's alpha rides in the colour.
        for v in out.iter_mut().rev().take(6) {
            v.color = [0.0, 0.0, 0.0, 0.45];
        }
    }
    let [u0, v0, u1, v1] = b.uv;
    let (x0, x1) = (
        b.center_x_px - b.width_px / 2.0,
        b.center_x_px + b.width_px / 2.0,
    );
    let top = feet + b.height_px;
    quad(
        out,
        [[x0, top, z], [x1, top, z], [x1, feet, z], [x0, feet, z]],
        [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
        1.0,
        kind::SPRITE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const EYE: [f32; 3] = [40.0, 300.0, 400.0];

    fn scene<'a>(
        cols: u32,
        rows: u32,
        depth: &'a [u8],
        slope: &'a [i8],
        billboards: &'a [SideBillboard],
    ) -> SideScene<'a> {
        SideScene {
            plane_w: cols as f32 * 16.0,
            plane_h: rows as f32 * 16.0,
            origin: [0.0, 0.0],
            tile_px: 16.0,
            cols,
            rows,
            depth,
            slope,
            billboards,
            back_px: 0.0,
            atlas: Atlas::single(rows as f32 * 16.0),
            back_planes: &[],
            ref_eye: [0.0; 3],
            overscan_px: 0.0,
        }
    }

    fn count(v: &[Vertex], k: u32) -> usize {
        v.iter().filter(|x| x.kind == k).count()
    }

    #[test]
    fn a_flat_level_is_the_picture_and_nothing_else() {
        let d = [0u8; 8];
        let s = [0i8; 8];
        let v = build_vertices_side(&scene(4, 2, &d, &s, &[]), EYE);
        assert_eq!(v.len(), 6);
        assert!(v.iter().all(|x| x.kind == kind::PLANE));
    }

    #[test]
    fn nothing_to_draw_for_an_empty_plane() {
        let sc = SideScene {
            plane_w: 0.0,
            ..scene(1, 1, &[0], &[0], &[])
        };
        assert!(build_vertices_side(&sc, EYE).is_empty());
    }

    #[test]
    fn the_picture_is_upright_with_row_zero_highest_and_blocks_come_toward_the_camera() {
        // One block in the bottom row of a 2x2 grid.
        let d = [0, 0, 8, 0];
        let s = [0i8; 4];
        let v = build_vertices_side(&scene(2, 2, &d, &s, &[]), [8.0, 300.0, 400.0]);
        // The plane's top-left corner is the highest point at z = 0.
        assert_eq!(v[0].pos, [0.0, 32.0, 0.0]);
        let front: Vec<_> = v[6..12].iter().collect();
        assert!(front.iter().all(|x| (x.pos[2] - 16.0).abs() < 1e-4));
        let ys: Vec<f32> = front.iter().map(|x| x.pos[1]).collect();
        assert!(
            ys.iter().all(|y| *y >= 0.0 && *y <= 16.0),
            "bottom row: {ys:?}"
        );
        assert!(v.iter().all(|x| x.pos[2] <= 16.0 + 1e-4));
        // The front shows the cell's own pixels: UV is its picture rect.
        assert!(front.iter().any(|x| x.uv == [0.0, 0.5]));
        assert!(front.iter().any(|x| x.uv == [0.5, 1.0]));
    }

    #[test]
    fn a_run_of_equal_blocks_is_one_front_one_top_and_two_sides() {
        let d = [8u8; 4];
        let s = [0i8; 4];
        // An eye over the middle sees the top but not the sides.
        let mid = build_vertices_side(&scene(4, 1, &d, &s, &[]), [32.0, 300.0, 400.0]);
        assert_eq!(mid.len(), 6 + 6 + 6, "plane + front + top");
        // An eye left of the run also sees the left side.
        let left = build_vertices_side(&scene(4, 1, &d, &s, &[]), [-50.0, 300.0, 400.0]);
        assert_eq!(left.len(), 6 + 6 + 6 + 6);
        // From below the top is not drawn.
        let below = build_vertices_side(&scene(4, 1, &d, &s, &[]), [32.0, -10.0, 400.0]);
        assert_eq!(below.len(), 6 + 6);
    }

    #[test]
    fn different_depths_do_not_merge_and_shallower_pieces_are_emitted_first() {
        let d = [16u8, 8, 8]; // one 2-tile-deep block, then two 1-tile-deep
        let s = [0i8; 3];
        let v = build_vertices_side(&scene(3, 1, &d, &s, &[]), [24.0, 300.0, 400.0]);
        // Each piece is whole quads; its depth is the nearest z it reaches.
        let reach: Vec<f32> = v[6..]
            .chunks(6)
            .map(|q| q.iter().map(|x| x.pos[2]).fold(f32::MIN, f32::max))
            .collect();
        assert!(reach.contains(&16.0) && reach.contains(&32.0), "{reach:?}");
        assert!(
            reach.windows(2).all(|w| w[0] <= w[1]),
            "shallow before deep: {reach:?}"
        );
    }

    #[test]
    fn a_stack_merges_into_one_rectangle() {
        let d = [8u8; 6]; // 2 wide, 3 tall
        let s = [0i8; 6];
        let v = build_vertices_side(&scene(2, 3, &d, &s, &[]), [16.0, 300.0, 400.0]);
        // plane + one front rectangle + one top; no sides seen from the middle.
        assert_eq!(v.len(), 18);
        let front = &v[6..12];
        let ys: Vec<f32> = front.iter().map(|x| x.pos[1]).collect();
        assert!(ys.contains(&48.0) && ys.contains(&0.0), "{ys:?}");
    }

    #[test]
    fn a_ramp_is_a_wedge_with_a_trapezoid_front_a_slope_and_a_wall() {
        let d = [8u8];
        let s = [4i8]; // rises half a tile to the right
        let v = build_vertices_side(&scene(1, 1, &d, &s, &[]), [60.0, 300.0, 400.0]);
        // plane + front + top + right wall (the eye is right of the cell)
        assert_eq!(v.len(), 6 + 6 + 6 + 6);
        let front = &v[6..12];
        let ys: Vec<f32> = front.iter().map(|x| x.pos[1]).collect();
        assert!(
            ys.contains(&8.0) && ys.contains(&0.0) && !ys.contains(&16.0),
            "{ys:?}"
        );
        let flipped = build_vertices_side(&scene(1, 1, &d, &[-4], &[]), [-60.0, 300.0, 400.0]);
        assert_eq!(
            flipped.len(),
            24,
            "falling to the right: the wall is on the left"
        );
    }

    #[test]
    fn a_player_on_a_block_stands_at_the_middle_of_its_depth() {
        let d = [0, 0, 8, 8];
        let s = [0i8; 4];
        let b = [SideBillboard {
            center_x_px: 24.0,
            feet_y_px: 16.0, // the top edge of the bottom row
            width_px: 8.0,
            height_px: 16.0,
            uv: [0.0, 0.0, 1.0, 1.0],
            behind_bg: false,
        }];
        let v = build_vertices_side(&scene(2, 2, &d, &s, &b), [24.0, 300.0, 400.0]);
        let sprite: Vec<_> = v.iter().filter(|x| x.kind == kind::SPRITE).collect();
        assert_eq!(sprite.len(), 6);
        assert!(sprite.iter().all(|x| (x.pos[2] - 8.0).abs() < 1e-4));
        assert!(
            sprite.iter().any(|x| (x.pos[1] - 16.0).abs() < 1e-4),
            "feet at y 16"
        );
        assert_eq!(count(&v, kind::SHADOW), 6, "a shadow on the block");
        // Sprites are emitted after every block face.
        let first_sprite = v.iter().position(|x| x.kind == kind::SPRITE).unwrap();
        assert!(v[first_sprite..].iter().all(|x| x.kind != kind::GROUND));
    }

    #[test]
    fn a_player_with_nothing_under_him_gets_no_shadow() {
        let b = [SideBillboard {
            center_x_px: 8.0,
            feet_y_px: 8.0,
            width_px: 8.0,
            height_px: 8.0,
            uv: [0.0; 4],
            behind_bg: false,
        }];
        let v = build_vertices_side(&scene(1, 1, &[0], &[0], &b), EYE);
        assert_eq!(count(&v, kind::SHADOW), 0);
        assert_eq!(count(&v, kind::SPRITE), 6);
    }

    #[test]
    fn the_set_back_plane_sits_behind_the_blocks_origin() {
        let d = [8u8];
        let s = [0i8];
        let mut sc = scene(1, 1, &d, &s, &[]);
        sc.back_px = 12.0;
        let v = build_vertices_side(&sc, EYE);
        assert!(v[..6].iter().all(|x| (x.pos[2] + 12.0).abs() < 1e-4));
        // The top face spans from the plane to the front.
        let zs: Vec<f32> = v[12..18].iter().map(|x| x.pos[2]).collect();
        assert!(zs.contains(&-12.0) && zs.contains(&16.0), "{zs:?}");
    }

    #[test]
    fn cells_off_the_picture_are_clipped_not_drawn_outside_it() {
        // Sub-cell scroll: the first column starts 8 px left of the picture.
        let d = [8u8, 8];
        let s = [0i8; 2];
        let mut sc = scene(2, 1, &d, &s, &[]);
        sc.origin = [-8.0, 0.0];
        sc.plane_w = 24.0;
        let v = build_vertices_side(&sc, [12.0, 300.0, 400.0]);
        assert!(v.iter().all(|x| x.pos[0] >= 0.0 && x.pos[0] <= 24.0));
        assert!(v.iter().all(|x| x.uv[0] >= 0.0 && x.uv[0] <= 1.0));
    }

    #[test]
    #[should_panic(expected = "depth length")]
    fn a_short_depth_array_is_a_caller_bug() {
        let _ = build_vertices_side(&scene(2, 2, &[0, 0, 0], &[0; 4], &[]), EYE);
    }

    #[test]
    fn back_planes_come_first_furthest_first_in_their_own_atlas_slot() {
        let d = [0u8; 8];
        let sl = [0i8; 8];
        let planes = [
            BackPlane {
                slot: 1,
                back_px: 20.0,
            },
            BackPlane {
                slot: 2,
                back_px: 50.0,
            },
        ];
        let sc = SideScene {
            atlas: Atlas {
                slots: 3,
                pitch_px: 34.0,
                playfield: 0,
            },
            back_planes: &planes,
            ref_eye: [32.0, 16.0, 100.0],
            ..scene(4, 2, &d, &sl, &[])
        };
        let v = build_vertices_side(&sc, EYE);
        assert_eq!(v.len(), 18, "two back planes and the main plane");
        let z = |q: usize| v[q * 6].pos[2];
        assert_eq!((z(0), z(1), z(2)), (-50.0, -20.0, 0.0));
        // Slot 2 is the third strip of the atlas, slot 0 the first.
        assert!((v[0].uv[1] - 68.0 / 102.0).abs() < 1e-5, "{:?}", v[0].uv);
        assert!((v[12].uv[1] - 0.0).abs() < 1e-6);
        // Scaled about the reference eye: 50 further back at 100 away is
        // 1.5x, so the plane covers [-16, 80] x [-8, 40] (it is 64 x 32).
        let xs: Vec<f32> = v[..6].iter().map(|x| x.pos[0]).collect();
        assert!(xs.iter().any(|x| (*x - (-16.0)).abs() < 1e-3), "{xs:?}");
        assert!(xs.iter().any(|x| (*x - 80.0).abs() < 1e-3), "{xs:?}");
    }

    #[test]
    fn a_single_slot_atlas_maps_uv_exactly_as_the_picture_did() {
        let d = [0u8; 8];
        let sl = [0i8; 8];
        let v = build_vertices_side(&scene(4, 2, &d, &sl, &[]), EYE);
        let uvs: Vec<[f32; 2]> = v.iter().map(|x| x.uv).collect();
        assert!(uvs.contains(&[0.0, 0.0]) && uvs.contains(&[1.0, 1.0]));
    }

    #[test]
    fn a_billboard_behind_the_background_stands_just_behind_the_plane() {
        let d = [0u8; 4];
        let sl = [0i8; 4];
        let b = [SideBillboard {
            center_x_px: 8.0,
            feet_y_px: 16.0,
            width_px: 8.0,
            height_px: 8.0,
            uv: [0.0, 0.0, 1.0, 1.0],
            behind_bg: true,
        }];
        let v = build_vertices_side(&scene(2, 2, &d, &sl, &b), EYE);
        let sprite: Vec<&Vertex> = v.iter().filter(|x| x.kind == kind::SPRITE).collect();
        assert_eq!(sprite.len(), 6);
        assert!(sprite.iter().all(|x| x.pos[2] < 0.0 && x.pos[2] > -2.0));
    }
}
