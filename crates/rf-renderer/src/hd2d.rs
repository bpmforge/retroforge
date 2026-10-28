//! HD-2D render plugin support (ticket W39-04; `docs/design/REBUILD_LAB.md`
//! section 9, "HD-2D on one engine"): extruded walls from baked stage
//! geometry, billboard objects with per-class shadows, a tilted camera and a
//! depth of field, assembled from the W34 Diorama pieces.
//!
//! The render plugin (`plugins/hd2d/`, kind `render`) reads recipe facts and
//! emits draw data only: one rectangle per object it can place, whose
//! colour index is an [`ObjectClass`] code ([`class_from_code`]). The shell
//! hands those rectangles in as [`Hd2dMarker`]s (this crate depends on no
//! plugin or recipe type, see `scripts/validate-arch.sh`), together with the
//! baked wall grid as plain depth cells ([`Hd2dLattice`]) and the frame's two
//! RGBA layers. [`plan`] turns that into a mesh and a camera, [`render`]
//! draws it.
//!
//! ## What is real and what is approximated
//!
//! * Walls: a solid cell is extruded toward the camera by its baked height
//!   (`diorama_side`), so a wall has a top and sides under the tilt.
//! * Objects: opaque clusters of the sprite-only layer stand up as
//!   billboards; a marker overlapping a cluster gives it a class, and the
//!   class sets its shadow and lift ([`crate::diorama_objects`]).
//! * Depth of field: a **tilt-shift band** ([`crate::lens`]) centred on the
//!   focus object's projected feet. The picture stands on a tilted plane, so
//!   screen height is monotonic in depth for it and a horizontal sharp band
//!   is a slab of constant depth; objects far in front of or behind that
//!   slab are not individually blurred (no per-pixel depth readback). Said
//!   plainly so nobody reads more into it.
//!
//! Everything here is presentation over `&` data: it reads no core memory
//! and never touches the Accuracy buffers, so Original mode is untouched by
//! construction (and `w39_04_*` proves it with the SNES frame bytes).
//!
//! Every loop is a range, an iterator, or the flood fill in
//! [`entities_from_layer`], which marks a cell visited before it is pushed,
//! so each cell enters the stack once and the walk ends (law 8).

use crate::diorama::camera_matrices::{self, Mat4};
use crate::diorama::{DioramaCamera, DioramaPass, WINDOW_FOV_Y_DEG};
use crate::diorama_mesh::Vertex;
use crate::diorama_objects::ObjectClass;
use crate::diorama_side::{build_vertices_side_classed, Atlas, SideBillboard, SideScene};
use crate::gpu::GpuContext;
use crate::lens::{Effect, LensParams, LensPass, Weather};
use crate::lighting::{LightView, Lighting, Sun};

/// Colour-index codes of a render plugin's marker rectangles.
pub mod code {
    pub const PLAYER: u8 = 1;
    pub const ENEMY: u8 = 2;
    pub const PROJECTILE: u8 = 3;
    pub const PICKUP: u8 = 4;
    pub const SCENERY: u8 = 5;
}

/// The class a marker colour index stands for; anything else is
/// [`ObjectClass::Unknown`].
#[must_use]
pub fn class_from_code(c: u8) -> ObjectClass {
    match c {
        code::PLAYER => ObjectClass::Player,
        code::ENEMY => ObjectClass::Enemy,
        code::PROJECTILE => ObjectClass::Projectile,
        code::PICKUP => ObjectClass::Pickup,
        code::SCENERY => ObjectClass::Scenery,
        _ => ObjectClass::Unknown,
    }
}

/// One rectangle a render plugin drew, in picture pixels (y down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hd2dMarker {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub class: ObjectClass,
}

impl Hd2dMarker {
    /// From a plugin `rect(x, y, w, h, color_index)`.
    #[must_use]
    pub fn from_rect(x: i32, y: i32, w: u16, h: u16, color_index: u8) -> Self {
        Self {
            x,
            y,
            w: u32::from(w),
            h: u32::from(h),
            class: class_from_code(color_index),
        }
    }

    fn hits(&self, b: [i32; 4]) -> bool {
        let (x1, y1) = (self.x + self.w as i32, self.y + self.h as i32);
        self.x < b[2] && x1 > b[0] && self.y < b[3] && y1 > b[1]
    }
}

/// The wall grid over the picture: `cols * rows` cells of `tile_px`, the
/// first at `origin` (picture pixels; negative under sub-cell scroll).
#[derive(Debug, Clone, Copy)]
pub struct Hd2dLattice<'a> {
    pub origin: (f32, f32),
    pub tile_px: f32,
    pub cols: u32,
    pub rows: u32,
    /// Extrusion in eighths of a tile, row-major; 0 is flat.
    pub depth: &'a [u8],
}

/// What one frame is drawn from.
#[derive(Debug, Clone, Copy)]
pub struct Hd2dStage<'a> {
    /// The picture, RGBA, `size.0 * size.1 * 4` bytes.
    pub picture_rgba: &'a [u8],
    /// The sprite-only layer, same size; alpha 0 where there is no sprite.
    pub sprite_rgba: &'a [u8],
    pub size: (u32, u32),
    pub lattice: Hd2dLattice<'a>,
}

/// The look: camera, light and focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hd2dLook {
    /// Degrees; the side view applies its own gentle scale (`80` is the
    /// steepest the camera allows, about 20 degrees in a side view).
    pub pitch_deg: f32,
    pub yaw_deg: f32,
    pub zoom: f32,
    /// Time of day for the sun (`0.5` noon); `None`: unlit.
    pub sun_time: Option<f32>,
    /// Depth-of-field strength, `0.0..=1.0`; 0 is off.
    pub dof: f32,
    /// Height of the sharp band as a fraction of the frame.
    pub dof_band: f32,
    pub out: (u32, u32),
}

impl Default for Hd2dLook {
    fn default() -> Self {
        Self {
            pitch_deg: 80.0,
            yaw_deg: 6.0,
            zoom: 1.0,
            sun_time: Some(0.4),
            dof: 0.8,
            dof_band: 0.12,
            out: (1280, 720),
        }
    }
}

/// A frame ready to draw.
#[derive(Debug, Clone)]
pub struct Hd2dPlan {
    pub vertices: Vec<Vertex>,
    pub vp: Mat4,
    /// Where the sharp band's centre sits, as a fraction of the frame height
    /// from the top.
    pub focus_frac: f32,
    /// One class per billboard, in billboard order.
    pub classes: Vec<ObjectClass>,
    /// Entity boxes `[x0, y0, x1, y1)` found in the sprite layer.
    pub entities: Vec<[i32; 4]>,
    /// Lattice cells that were extruded.
    pub wall_cells: usize,
}

/// The most entities one frame stands up (a bound, not a tuning knob).
pub const MAX_ENTITIES: usize = 128;
/// Side of the coarse cells the sprite layer is clustered on, pixels.
const CLUSTER_PX: usize = 4;

/// Boxes `[x0, y0, x1, y1)` of the opaque clusters of a sprite layer: pixels
/// with alpha above 0 within a coarse cell ([`CLUSTER_PX`]) join their 8
/// neighbouring cells'. Empty for a wrong-sized buffer.
#[must_use]
pub fn entities_from_layer(rgba: &[u8], w: u32, h: u32) -> Vec<[i32; 4]> {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return Vec::new();
    }
    let (cw, ch) = (w.div_ceil(CLUSTER_PX), h.div_ceil(CLUSTER_PX));
    // Per coarse cell: the pixel box of its opaque pixels.
    let mut boxes: Vec<Option<[i32; 4]>> = vec![None; cw * ch];
    for y in 0..h {
        for x in 0..w {
            if rgba[(y * w + x) * 4 + 3] == 0 {
                continue;
            }
            let b = &mut boxes[(y / CLUSTER_PX) * cw + x / CLUSTER_PX];
            let (xi, yi) = (x as i32, y as i32);
            match b {
                Some(v) => {
                    v[0] = v[0].min(xi);
                    v[1] = v[1].min(yi);
                    v[2] = v[2].max(xi + 1);
                    v[3] = v[3].max(yi + 1);
                }
                None => *b = Some([xi, yi, xi + 1, yi + 1]),
            }
        }
    }
    let mut seen = vec![false; cw * ch];
    let mut out = Vec::new();
    for start in 0..cw * ch {
        if seen[start] || boxes[start].is_none() {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        let mut total = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        // Each cell is marked `seen` before it is pushed, so it is pushed at
        // most once and every pop shrinks the stack: this ends.
        while let Some(c) = stack.pop() {
            if let Some(b) = boxes[c] {
                total = [
                    total[0].min(b[0]),
                    total[1].min(b[1]),
                    total[2].max(b[2]),
                    total[3].max(b[3]),
                ];
            }
            let (cx, cy) = ((c % cw) as isize, (c / cw) as isize);
            for dy in -1..=1isize {
                for dx in -1..=1isize {
                    let (nx, ny) = (cx + dx, cy + dy);
                    if nx < 0 || ny < 0 || nx >= cw as isize || ny >= ch as isize {
                        continue;
                    }
                    let n = ny as usize * cw + nx as usize;
                    if !seen[n] && boxes[n].is_some() {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        if out.len() < MAX_ENTITIES {
            out.push(total);
        }
    }
    out
}

/// `rgba` with every transparent texel given the colour of the nearest
/// opaque one on its row (left first, then right): the hole a sprite leaves
/// in the background layer, filled so the picture plane has no gap under a
/// standing billboard. A row with no opaque texel stays as it was.
#[must_use]
pub fn fill_holes(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut out = rgba.to_vec();
    if w == 0 || out.len() != w * h * 4 {
        return out;
    }
    for row in out.chunks_exact_mut(w * 4) {
        let mut carry: Option<[u8; 3]> = None;
        for px in row.chunks_exact_mut(4) {
            if px[3] == 0 {
                if let Some(c) = carry {
                    px[..3].copy_from_slice(&c);
                    px[3] = 255;
                }
            } else {
                carry = Some([px[0], px[1], px[2]]);
            }
        }
        carry = None;
        for px in row.chunks_exact_mut(4).rev() {
            if px[3] == 0 {
                if let Some(c) = carry {
                    px[..3].copy_from_slice(&c);
                    px[3] = 255;
                }
            } else {
                carry = Some([px[0], px[1], px[2]]);
            }
        }
    }
    out
}

fn project_y_frac(vp: &Mat4, p: [f32; 3]) -> Option<f32> {
    let clip = |row: usize| {
        (0..4)
            .map(|k| vp[k * 4 + row] * [p[0], p[1], p[2], 1.0][k])
            .sum::<f32>()
    };
    let w = clip(3);
    (w > 1e-6).then(|| ((1.0 - clip(1) / w) / 2.0).clamp(0.0, 1.0))
}

fn camera_of(look: &Hd2dLook) -> DioramaCamera {
    DioramaCamera {
        pitch_deg: look.pitch_deg,
        yaw_deg: look.yaw_deg,
        zoom: look.zoom,
        follow: None,
    }
    .clamped()
}

/// Build the mesh, camera and focus for one frame. Pure CPU.
///
/// # Panics
/// If a buffer is not the size `stage` says, or `lattice.depth` is not
/// `cols * rows` long.
#[must_use]
pub fn plan(stage: &Hd2dStage<'_>, markers: &[Hd2dMarker], look: &Hd2dLook) -> Hd2dPlan {
    let (w, h) = stage.size;
    let n = (w as usize) * (h as usize) * 4;
    assert_eq!(stage.picture_rgba.len(), n, "hd2d: picture size");
    assert_eq!(stage.sprite_rgba.len(), n, "hd2d: sprite layer size");
    let lat = &stage.lattice;
    let cells = lat.cols as usize * lat.rows as usize;
    assert_eq!(lat.depth.len(), cells, "hd2d: lattice depth length");

    let entities = entities_from_layer(stage.sprite_rgba, w, h);
    let mut classes = Vec::with_capacity(entities.len());
    let billboards: Vec<SideBillboard> = entities
        .iter()
        .map(|b| {
            // The first marker over the box names it; none leaves the legacy look.
            classes.push(
                markers
                    .iter()
                    .find(|m| m.hits(*b))
                    .map_or(ObjectClass::Unknown, |m| m.class),
            );
            let (bw, bh) = ((b[2] - b[0]) as f32, (b[3] - b[1]) as f32);
            SideBillboard {
                center_x_px: b[0] as f32 + bw / 2.0,
                feet_y_px: b[3] as f32,
                width_px: bw,
                height_px: bh,
                uv: [
                    b[0] as f32 / w as f32,
                    b[1] as f32 / h as f32,
                    b[2] as f32 / w as f32,
                    b[3] as f32 / h as f32,
                ],
                behind_bg: false,
            }
        })
        .collect();

    let (pw, ph) = (w as f32, h as f32);
    let camera = camera_of(look);
    let aspect = look.out.0.max(1) as f32 / look.out.1.max(1) as f32;
    let target = (pw / 2.0, ph / 2.0);
    // The picture's centre in world coordinates (y up).
    let target = (target.0, ph - target.1);
    let placement =
        camera_matrices::side_placement(pw, ph, aspect, WINDOW_FOV_Y_DEG, &camera, target);
    let slope = vec![0i8; cells];
    let scene = SideScene {
        plane_w: pw,
        plane_h: ph,
        origin: [lat.origin.0, lat.origin.1],
        tile_px: lat.tile_px,
        cols: lat.cols,
        rows: lat.rows,
        depth: lat.depth,
        slope: &slope,
        billboards: &billboards,
        back_px: 0.0,
        atlas: Atlas::single(ph),
        back_planes: &[],
        ref_eye: [target.0, target.1, placement.distance],
        overscan_px: pw.max(ph),
    };
    let vertices = build_vertices_side_classed(&scene, placement.eye, &classes);
    let vp = camera_matrices::view_proj_side(pw, ph, aspect, WINDOW_FOV_Y_DEG, &camera, target);

    // Focus: the feet of the player marker's entity, else the first
    // marker's, else the picture's middle.
    let focus = markers
        .iter()
        .find(|m| m.class == ObjectClass::Player)
        .or_else(|| markers.first())
        .map(|m| {
            [
                m.x as f32 + m.w as f32 / 2.0,
                ph - (m.y + m.h as i32) as f32,
                0.0,
            ]
        })
        .unwrap_or([target.0, target.1, 0.0]);
    let focus_frac = project_y_frac(&vp, focus).unwrap_or(0.5);
    Hd2dPlan {
        vertices,
        vp,
        focus_frac,
        classes,
        entities,
        wall_cells: lat.depth.iter().filter(|&&d| d > 0).count(),
    }
}

/// The lens parameters for the depth of field of `look` and `focus_frac`.
#[must_use]
pub fn dof_params(look: &Hd2dLook, focus_frac: f32) -> LensParams {
    LensParams {
        tilt_shift: Effect::on(look.dof),
        tilt_center: focus_frac,
        tilt_band: look.dof_band.clamp(0.02, 1.0),
        ..LensParams::default()
    }
}

/// The sun `look` asks for, `None` when it is unlit.
#[must_use]
pub fn lighting_for(look: &Hd2dLook) -> Option<Lighting> {
    look.sun_time
        .map(|t| Lighting::new(Sun::for_time(t, Weather::None, LightView::Side)))
}

/// Draw one HD-2D frame: the plan's mesh through the Diorama pass (sun and
/// shadow map when `look.sun_time` is set), then the depth-of-field lens.
/// Returns RGBA at `look.out`.
///
/// # Errors
/// A GPU readback failure from either pass.
pub fn render(
    gpu: &GpuContext,
    pass: &DioramaPass,
    lens: &LensPass,
    stage: &Hd2dStage<'_>,
    markers: &[Hd2dMarker],
    look: &Hd2dLook,
) -> Result<(Vec<u8>, Hd2dPlan), String> {
    let plan = plan(stage, markers, look);
    pass.set_lighting(lighting_for(look));
    let ground = fill_holes(stage.picture_rgba, stage.size.0, stage.size.1);
    let rgba = pass.render_with_vp(
        gpu,
        &plan.vertices,
        &ground,
        stage.size.0,
        stage.size.1,
        stage.sprite_rgba,
        stage.size.0,
        stage.size.1,
        plan.vp,
        look.out.0,
        look.out.1,
    )?;
    let params = dof_params(look, plan.focus_frac);
    if params.any_active() {
        let out = lens.render(gpu, &rgba, look.out.0, look.out.1, None, &params)?;
        Ok((out, plan))
    } else {
        Ok((rgba, plan))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(w: usize, h: usize, boxes: &[[usize; 4]]) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for b in boxes {
            for y in b[1]..b[3] {
                for x in b[0]..b[2] {
                    v[(y * w + x) * 4 + 3] = 255;
                }
            }
        }
        v
    }

    #[test]
    fn clusters_become_boxes_and_touching_ones_merge() {
        let l = layer(64, 32, &[[2, 2, 10, 12], [40, 8, 52, 20], [10, 8, 14, 14]]);
        let mut e = entities_from_layer(&l, 64, 32);
        e.sort();
        // The first and third touch on the coarse grid and stand together.
        assert_eq!(e, vec![[2, 2, 14, 14], [40, 8, 52, 20]]);
        assert!(entities_from_layer(&l[..10], 64, 32).is_empty());
        assert!(entities_from_layer(&[], 0, 0).is_empty());
    }

    #[test]
    fn holes_take_the_neighbouring_colour() {
        let mut p = vec![0u8; 8 * 4];
        p[..4].copy_from_slice(&[10, 20, 30, 255]);
        p[7 * 4..].copy_from_slice(&[90, 80, 70, 255]);
        let f = fill_holes(&p, 8, 1);
        assert_eq!(&f[4..8], &[10, 20, 30, 255]);
        assert!(f.chunks_exact(4).all(|c| c[3] == 255));
        // A row with nothing opaque stays empty.
        assert!(fill_holes(&[0; 16], 4, 1).iter().all(|&b| b == 0));
    }

    #[test]
    fn marker_codes_map_to_classes() {
        assert_eq!(class_from_code(code::PLAYER), ObjectClass::Player);
        assert_eq!(class_from_code(0), ObjectClass::Unknown);
        let m = Hd2dMarker::from_rect(4, 4, 8, 8, code::PICKUP);
        assert!(m.hits([6, 6, 20, 20]) && !m.hits([12, 4, 20, 20]));
    }

    #[test]
    fn plan_extrudes_walls_stands_objects_up_and_focuses_on_the_player() {
        let (w, h) = (64u32, 48u32);
        let pic = vec![200u8; (w * h * 4) as usize];
        let spr = layer(64, 48, &[[20, 16, 28, 32], [44, 24, 52, 32]]);
        let depth: Vec<u8> = (0..16)
            .map(|i| if i / 4 == 2 || i == 0 { 8 } else { 0 })
            .collect();
        let stage = Hd2dStage {
            picture_rgba: &pic,
            sprite_rgba: &spr,
            size: (w, h),
            lattice: Hd2dLattice {
                origin: (0.0, 0.0),
                tile_px: 16.0,
                cols: 4,
                rows: 4,
                depth: &depth,
            },
        };
        let markers = [Hd2dMarker::from_rect(20, 16, 8, 16, code::PLAYER)];
        let look = Hd2dLook::default();
        let p = plan(&stage, &markers, &look);
        let flat = plan(
            &Hd2dStage {
                lattice: Hd2dLattice {
                    depth: &[0; 16],
                    ..stage.lattice
                },
                ..stage
            },
            &markers,
            &look,
        );
        assert_eq!(p.wall_cells, 5);
        assert!(p.vertices.len() > flat.vertices.len(), "walls add geometry");
        assert_eq!(p.classes, vec![ObjectClass::Player, ObjectClass::Unknown]);
        assert!((0.0..=1.0).contains(&p.focus_frac));
        // A focus lower in the picture sits lower on the frame.
        let low = [Hd2dMarker::from_rect(20, 40, 8, 8, code::PLAYER)];
        let high = [Hd2dMarker::from_rect(20, 0, 8, 8, code::PLAYER)];
        let (a, b) = (plan(&stage, &low, &look), plan(&stage, &high, &look));
        assert!(
            a.focus_frac > b.focus_frac,
            "{} {}",
            a.focus_frac,
            b.focus_frac
        );
    }

    #[test]
    fn dof_off_is_inactive_and_on_carries_the_focus() {
        let off = Hd2dLook {
            dof: 0.0,
            ..Hd2dLook::default()
        };
        assert!(!dof_params(&off, 0.4).any_active());
        let on = dof_params(&Hd2dLook::default(), 0.4);
        assert!(on.any_active() && (on.tilt_center - 0.4).abs() < 1e-6);
    }
}
