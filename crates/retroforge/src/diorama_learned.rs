//! The Diorama on learned heights (ticket W34-04; `docs/design/HD2D_PLAN.md`
//! sections 2.5 and D8).
//!
//! Console-free mediation between what the core thread learned
//! ([`crate::live_learn::HeightsFrame`], one `HeightGrid` per tile grid) and
//! what the renderer draws. The same code serves the NES and the SNES: it
//! reads the tile grids, the sprite boxes and the frame's pixels, never a
//! console's memory.
//!
//! ## What is drawn
//!
//! - **Side view** ([`rf_renderer::diorama_mesh::build_vertices_side`]): the
//!   picture stands upright and the cells the learner called solid grow
//!   toward the camera.
//! - **Top down**: the existing builder, on a full-screen lattice made from
//!   the learned band.
//! - **Unknown** orientation with learned heights: nothing (the flat view
//!   stays), and the readout says why. With a painted `[heights]` table the
//!   orientation is not needed to draw the old way.
//!
//! ## One lattice
//!
//! Only the playfield grid is extruded ([`Lattice::from_learned`]); HUD
//! grids stay flat on the picture. A painted table replaces the learned
//! heights wholesale ([`Lattice::from_painted`]): a painted tile number is
//! SNES-only and a different cell size from the learned keys, so the two are
//! not merged cell by cell.

use rf_enhance::heights::{Class, HeightCanvas, HeightGrid, Orientation, SpriteBox, TileGrid};
use rf_renderer::diorama::camera_matrices;
use rf_renderer::diorama_mesh::{Billboard, DioramaScene, SideBillboard, SideScene};
use rf_renderer::gpu::GpuContext;

use crate::enhanced_view::{DioramaRender, DioramaView};
use crate::live_heights::LiveGrid;

/// How a frame is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Leave the flat picture alone.
    Flat,
    Top,
    Side,
}

/// Which builder, from the orientation call and whether the heights were
/// painted (a painted table predates orientation, so Unknown stays top down).
#[must_use]
pub fn plan_for(orientation: Orientation, painted: bool) -> Plan {
    match orientation {
        Orientation::SideView => Plan::Side,
        Orientation::TopDown => Plan::Top,
        Orientation::Unknown if painted => Plan::Top,
        Orientation::Unknown => Plan::Flat,
    }
}

/// A band of cells laid on the picture, with what to raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lattice {
    pub cols: u32,
    pub rows: u32,
    pub tile_px: u32,
    /// Picture position of cell (0, 0)'s top-left corner.
    pub origin: (i32, i32),
    /// Extrusion in eighths of a tile; 0 is flat.
    pub depth: Vec<u8>,
    /// Ramp rise in eighths across the cell; 0 is a plain block.
    pub slope: Vec<i8>,
}

/// Extrusion and slope of one learned cell. A ramp is at least a full tile
/// deep so its wedge has something to stand on; floor, open air and
/// backdrop are never raised.
fn raise(class: Class, eighths: u8) -> (u8, i8) {
    match class {
        Class::Wall | Class::Tall | Class::Platform => (eighths.max(1), 0),
        Class::Ramp(s) => (eighths.max(8), s),
        Class::Floor | Class::Unknown | Class::Backdrop => (0, 0),
    }
}

impl Lattice {
    /// The playfield grid `grid` with the heights `heights` learned for it.
    #[must_use]
    pub fn from_learned(grid: &TileGrid, heights: &HeightGrid) -> Self {
        let n = usize::from(grid.cols) * usize::from(grid.rows);
        let mut depth = vec![0u8; n];
        let mut slope = vec![0i8; n];
        if !grid.hud && heights.class.len() == n && heights.depth.len() == n {
            for i in 0..n {
                let (d, s) = raise(heights.class[i], heights.depth[i]);
                depth[i] = d;
                slope[i] = s;
            }
        }
        Self {
            cols: u32::from(grid.cols),
            rows: u32::from(grid.rows),
            tile_px: u32::from(grid.cell_px),
            origin: (
                -i32::from(grid.offset.0),
                i32::from(grid.rows_px.0) - i32::from(grid.offset.1),
            ),
            depth,
            slope,
        }
    }

    /// Ticket W34-07: the cells of a whole-level height canvas that fall on a
    /// `view`-sized picture whose top-left pixel sits at `picture_origin` on
    /// the canvas (canvas pixels). The picture may be wider than the game's
    /// own screen: this is what raises the scouted margins.
    #[must_use]
    pub fn from_level(hc: &HeightCanvas, picture_origin: (i64, i64), view: (u32, u32)) -> Self {
        let p = i64::from(hc.cell_px.max(1));
        let c0 = (picture_origin.0 - hc.origin.0).div_euclid(p);
        let r0 = (picture_origin.1 - hc.origin.1).div_euclid(p);
        let left = hc.origin.0 + c0 * p - picture_origin.0;
        let top = hc.origin.1 + r0 * p - picture_origin.1;
        let cols = ((i64::from(view.0) - left + p - 1) / p).max(1) as u32;
        let rows = ((i64::from(view.1) - top + p - 1) / p).max(1) as u32;
        let n = cols as usize * rows as usize;
        let mut depth = vec![0u8; n];
        let mut slope = vec![0i8; n];
        for r in 0..rows {
            for c in 0..cols {
                let (cc, rr) = (c0 + i64::from(c), r0 + i64::from(r));
                let (Ok(cc), Ok(rr)) = (u32::try_from(cc), u32::try_from(rr)) else {
                    continue;
                };
                if let Some(l) = hc.get(cc, rr) {
                    let (class, eighths) = l.drawn();
                    let (d, s) = raise(class, eighths);
                    depth[r as usize * cols as usize + c as usize] = d;
                    slope[r as usize * cols as usize + c as usize] = s;
                }
            }
        }
        Self {
            cols,
            rows,
            tile_px: p as u32,
            origin: (left as i32, top as i32),
            depth,
            slope,
        }
    }

    /// The painted table's grid (whole screen, plain blocks).
    #[must_use]
    pub fn from_painted(g: &LiveGrid) -> Self {
        Self {
            cols: g.cols,
            rows: g.rows,
            tile_px: g.tile_px,
            origin: (-(g.offset.0 as i32), -(g.offset.1 as i32)),
            depth: g.depth.clone(),
            slope: vec![0; g.depth.len()],
        }
    }

    /// The same cells on a picture whose left edge is `dx` dots further
    /// left (a widescreen picture: the game's 256 dots sit `dx` in from its
    /// left edge).
    #[must_use]
    pub fn shifted_right(mut self, dx: i32) -> Self {
        self.origin.0 += dx;
        self
    }

    /// Cells that will be raised.
    #[must_use]
    pub fn raised(&self) -> usize {
        self.depth.iter().filter(|d| **d > 0).count()
    }

    /// This lattice as a [`LiveGrid`] covering a whole `screen`, which the
    /// top-down builder and its ground texture are made for.
    #[must_use]
    pub fn to_live_grid(&self, screen: (u32, u32)) -> LiveGrid {
        let p = self.tile_px.max(1) as i32;
        let off = (
            (-self.origin.0).rem_euclid(p),
            (-self.origin.1).rem_euclid(p),
        );
        let cols = ((screen.0 as i32 + off.0 + p - 1) / p).max(1) as u32;
        let rows = ((screen.1 as i32 + off.1 + p - 1) / p).max(1) as u32;
        let mut depth = vec![0u8; (cols * rows) as usize];
        for r in 0..self.rows {
            for c in 0..self.cols {
                let src = (r * self.cols + c) as usize;
                // `origin + off` is a multiple of the cell size, so these
                // divisions are exact.
                let fc = (self.origin.0 + c as i32 * p + off.0) / p;
                let fr = (self.origin.1 + r as i32 * p + off.1) / p;
                if (0..cols as i32).contains(&fc) && (0..rows as i32).contains(&fr) {
                    depth[(fr as u32 * cols + fc as u32) as usize] = self.depth[src];
                }
            }
        }
        LiveGrid {
            cols,
            rows,
            tile_px: p as u32,
            offset: (off.0 as u32, off.1 as u32),
            solid: depth.iter().map(|d| u8::from(*d > 0)).collect(),
            depth,
        }
    }
}

/// The picture without the sprites drawn on it: every pixel the sprite layer
/// covers is replaced by the nearest picture pixel in the same row that no
/// sprite covers, so a player is not drawn twice (once flat on the plane,
/// once as a billboard). The picture itself is kept everywhere else, colour
/// maths and all, which is why this starts from the frame and not from the
/// background layer.
#[must_use]
pub fn ground_without_sprites(sprite_rgba: &[u8], frame_rgba: &[u8], width: u32) -> Vec<u8> {
    if sprite_rgba.len() != frame_rgba.len() || width == 0 {
        return frame_rgba.to_vec();
    }
    let w = width as usize;
    let mut out = frame_rgba.to_vec();
    for (row_sp, row_out) in sprite_rgba.chunks(w * 4).zip(out.chunks_mut(w * 4)) {
        let covered = |x: usize| row_sp[x * 4 + 3] != 0;
        let n = row_sp.len() / 4;
        let mut last: Option<usize> = None;
        let mut left: Vec<Option<usize>> = vec![None; n];
        for (x, slot) in left.iter_mut().enumerate() {
            if !covered(x) {
                last = Some(x);
            }
            *slot = last;
        }
        // The source pixels are read from a copy of the row, so a filled
        // pixel is never used as a source.
        let src: Vec<u8> = row_out.to_vec();
        let mut next: Option<usize> = None;
        for x in (0..n).rev() {
            if !covered(x) {
                next = Some(x);
                continue;
            }
            let pick = match (left[x], next) {
                (Some(l), Some(r)) => Some(if x - l <= r - x { l } else { r }),
                (a, b) => a.or(b),
            };
            if let Some(s) = pick {
                row_out[x * 4..x * 4 + 4].copy_from_slice(&src[s * 4..s * 4 + 4]);
            }
        }
    }
    out
}

/// The colour most of a frame is drawn in (sampled every fourth pixel): the
/// backdrop of a level whose sky is an empty tile. Black for an empty frame.
#[must_use]
pub fn dominant_colour(rgba: &[u8]) -> [u8; 3] {
    let mut counts: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
    for px in rgba.chunks_exact(4).step_by(4) {
        *counts.entry([px[0], px[1], px[2]]).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(c, n)| (*n, std::cmp::Reverse(*c)))
        .map_or([0, 0, 0], |(c, _)| c)
}

/// `ground` with every transparent texel painted `backdrop`: a decoded
/// level image leaves what the game draws nothing on transparent, which is
/// right flat (the screen's own backdrop shows through) but leaves a hole
/// in an upright picture.
#[must_use]
pub fn fill_transparent(ground: &[u8], backdrop: [u8; 3]) -> Vec<u8> {
    let mut out = ground.to_vec();
    for px in out.chunks_exact_mut(4) {
        if px[3] == 0 {
            px[..3].copy_from_slice(&backdrop);
            px[3] = 255;
        }
    }
    out
}

/// Side-view billboards for the sprites on a `screen`-sized picture, one per
/// entity (touching sprites stand together, W34-08); those wholly off it are
/// dropped.
#[must_use]
pub fn side_billboards(sprites: &[SpriteBox], screen: (u32, u32)) -> Vec<SideBillboard> {
    crate::diorama_layers::entity_billboards(sprites, screen)
}

/// [`side_billboards`] for a picture wider than the frame the sprite layer
/// was cut from (ticket W34-07): the boxes are in the 256 hardware dots;
/// `core_pad` is where those sit in the core's own (widened) frame, which the
/// sprite texture is, and `plane_pad` where they sit on the ground plane.
#[must_use]
pub fn side_billboards_wide(
    sprites: &[SpriteBox],
    core: (u32, u32),
    core_pad: i32,
    plane_pad: i32,
) -> Vec<SideBillboard> {
    let in_core: Vec<SpriteBox> = sprites
        .iter()
        .map(|s| SpriteBox {
            x: s.x.saturating_add(core_pad as i16),
            ..*s
        })
        .collect();
    let shift = (plane_pad - core_pad) as f32;
    side_billboards(&in_core, core)
        .into_iter()
        .map(|mut b| {
            b.center_x_px += shift;
            b
        })
        .collect()
}

/// Top-down billboards on a [`LiveGrid`] lattice (feet at the bottom edge),
/// one per entity (touching sprites stand together, W34-08).
#[must_use]
pub fn top_billboards(
    sprites: &[SpriteBox],
    grid: &LiveGrid,
    screen: (u32, u32),
) -> Vec<Billboard> {
    top_billboards_wide(sprites, grid, screen, screen, (0, 0))
}

/// [`top_billboards`] for a floor wider than the frame the sprite layer was
/// cut from (ticket W34-09): `screen` is the floor's picture, `sprite_size`
/// the sprite texture's (the core's frame), and `pads` where the 256 hardware
/// dots the boxes are in sit in the core's frame and on the floor.
#[must_use]
pub fn top_billboards_wide(
    sprites: &[SpriteBox],
    grid: &LiveGrid,
    screen: (u32, u32),
    sprite_size: (u32, u32),
    (core_pad, plane_pad): (i32, i32),
) -> Vec<Billboard> {
    let (sw, sh) = (screen.0.max(1) as f32, screen.1.max(1) as f32);
    let (tw, th) = (sprite_size.0.max(1) as f32, sprite_size.1.max(1) as f32);
    crate::diorama_layers::entities_of(sprites)
        .into_iter()
        .filter_map(|(e, _)| {
            let (x, y) = ((e.x0 + plane_pad) as f32, e.y0 as f32);
            let (w, h) = (e.width() as f32, e.height() as f32);
            if w <= 0.0 || h <= 0.0 || y >= sh || y + h <= 0.0 || x >= sw || x + w <= 0.0 {
                return None;
            }
            let (gx, gy) = grid.screen_to_grid(x, y);
            let u = (e.x0 + core_pad) as f32;
            Some(Billboard {
                center_x_px: gx + w / 2.0,
                center_z_px: gy + h,
                width_px: w,
                height_px: h,
                uv: [u / tw, y / th, (u + w) / tw, (y + h) / th],
            })
        })
        .collect()
}

/// How far behind the blocks the picture plane sits for a measured backdrop
/// scroll ratio (`1 - ratio` of two tiles; a plane that scrolls at the
/// playfield's own speed is not set back at all).
#[must_use]
pub fn set_back_px(ratio: Option<f32>, tile_px: f32) -> f32 {
    ratio.map_or(0.0, |r| (1.0 - r.clamp(0.0, 1.0)) * tile_px * 2.0)
}

/// How long the last side-view mesh took to build, microseconds (a
/// measurement the cost rows read; nothing depends on it).
pub static LAST_MESH_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What a side-view frame is drawn from.
pub struct SideFrame<'a> {
    pub lattice: &'a Lattice,
    /// The picture the plane shows (`screen`-sized unless it is a level).
    pub ground_rgba: &'a [u8],
    pub ground: (u32, u32),
    pub sprites: &'a [SideBillboard],
    pub sprite_rgba: &'a [u8],
    pub sprite_size: (u32, u32),
    pub back_px: f32,
    /// Ticket W34-08: the picture pulled apart into planes. When set,
    /// `ground_rgba` is not read (the planes' atlas is the ground texture),
    /// `ground` is still the size of ONE picture (the plane) and `back_px`
    /// is ignored: the playfield is the plane the blocks grow from.
    pub planes: Option<&'a crate::diorama_layers::Planes>,
    /// The window of the picture the game shows: its size, and the centre
    /// to look at (picture pixels, y down).
    pub window: (f32, f32),
    pub centre: (f32, f32),
    pub out: (u32, u32),
    pub view: DioramaView,
}

/// The largest yaw, degrees, at which a camera `distance` from its target
/// still sees only plane when the plane goes on `reach` pixels from the target
/// on its nearer side: the ray through the far edge of the frame meets the
/// plane `distance * tan(yaw + half the horizontal field of view)` away. At
/// most the camera's own 30 degrees; never below 0.
#[must_use]
pub fn yaw_cap_deg(distance: f32, fovy_deg: f32, aspect: f32, reach: f32) -> f32 {
    let half_h = (aspect * (fovy_deg.to_radians() / 2.0).tan()).atan();
    ((reach / distance.max(1e-3)).atan() - half_h)
        .to_degrees()
        .clamp(0.0, rf_renderer::diorama::DioramaCamera::YAW_LIMIT)
}

/// Render one side-view frame.
///
/// # Errors
/// Propagates the pass's `Err` (GPU readback timeout).
pub fn render_side(
    gpu: &GpuContext,
    pass: &rf_renderer::diorama::DioramaPass,
    f: &SideFrame<'_>,
) -> Result<DioramaRender, String> {
    let (gw, gh) = (f.ground.0 as f32, f.ground.1 as f32);
    let camera = f.view.camera_at((f.centre.0, f.centre.1)).clamped();
    let aspect = f.view.projection_aspect(f.out);
    // Following with a zoom: the window narrows around the centre and may
    // not leave the picture; otherwise the whole window is framed.
    let zoom = camera.zoom;
    let (hw, hh) = (f.window.0 / 2.0 / zoom, f.window.1 / 2.0 / zoom);
    let cx = f.centre.0.clamp(hw.min(gw / 2.0), (gw - hw).max(gw / 2.0));
    let cy = f.centre.1.clamp(hh.min(gh / 2.0), (gh - hh).max(gh / 2.0));
    let target = (cx, gh - cy);
    let place = |camera: &rf_renderer::diorama::DioramaCamera| {
        camera_matrices::side_placement(
            f.window.0,
            f.window.1,
            aspect,
            rf_renderer::diorama::WINDOW_FOV_Y_DEG,
            camera,
            target,
        )
    };
    let mut camera = camera;
    let mut placement = place(&camera);
    // W35-01: the planes go on a picture's width past each side (mirrored),
    // which a view swung far enough still looks past on a very wide picture
    // (32:9): hold the yaw to what the extension covers.
    let overscan = gh.max(gw);
    let cap = yaw_cap_deg(
        placement.distance,
        rf_renderer::diorama::WINDOW_FOV_Y_DEG,
        aspect,
        cx.min(gw - cx) + overscan.min(gw),
    );
    if camera.yaw_deg.abs() > cap {
        camera.yaw_deg = cap.copysign(camera.yaw_deg);
        placement = place(&camera);
    }
    let (atlas, back_planes, back_px, ground_rgba, ground_size) = match f.planes {
        Some(p) => (
            p.atlas,
            p.back.as_slice(),
            0.0,
            p.atlas_rgba.as_slice(),
            p.atlas_size,
        ),
        None => (
            rf_renderer::diorama_side::Atlas::single(gh),
            &[][..],
            f.back_px,
            f.ground_rgba,
            f.ground,
        ),
    };
    let scene = SideScene {
        plane_w: gw,
        plane_h: gh,
        origin: [f.lattice.origin.0 as f32, f.lattice.origin.1 as f32],
        tile_px: f.lattice.tile_px as f32,
        cols: f.lattice.cols,
        rows: f.lattice.rows,
        depth: &f.lattice.depth,
        slope: &f.lattice.slope,
        billboards: f.sprites,
        back_px,
        atlas,
        back_planes,
        // The eye straight in front of the target at the camera's distance:
        // the back planes are scaled to look the same from there.
        ref_eye: [target.0, target.1, placement.distance],
        // The planes go on past the picture's edge as the picture mirrored
        // about it (W35-01; they were its edge texels stretched), so a
        // pitched or swung camera never looks off them into the clear colour
        // (W34-08's void) nor at streaks; the yaw is capped where the
        // extension stops covering the view (`yaw_cap_deg`).
        overscan_px: overscan,
    };
    let t_mesh = std::time::Instant::now();
    let vertices = rf_renderer::diorama_mesh::build_vertices_side(&scene, placement.eye);
    LAST_MESH_US.store(
        u64::try_from(t_mesh.elapsed().as_micros()).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    let vp = camera_matrices::view_proj_side(
        f.window.0,
        f.window.1,
        aspect,
        rf_renderer::diorama::WINDOW_FOV_Y_DEG,
        &camera,
        target,
    );
    let rgba = pass.render_with_vp(
        gpu,
        &vertices,
        ground_rgba,
        ground_size.0,
        ground_size.1,
        f.sprite_rgba,
        f.sprite_size.0,
        f.sprite_size.1,
        vp,
        f.out.0,
        f.out.1,
    )?;
    Ok(DioramaRender {
        rgba,
        width: f.out.0,
        height: f.out.1,
    })
}

/// What a top-down frame is drawn from.
pub struct TopFrame<'a> {
    pub lattice: &'a Lattice,
    pub frame_rgba: &'a [u8],
    /// The size of `frame_rgba`, the floor's picture (wider than the game's
    /// 256 dots on a widescreen Diorama, W34-09).
    pub screen: (u32, u32),
    pub sprites: &'a [SpriteBox],
    pub sprite_rgba: &'a [u8],
    /// The sprite texture's size (the core's frame) and where the boxes' 256
    /// dots sit in it and on the floor (`core_pad`, `plane_pad`); the floor's
    /// own size and `(0, 0)` when the picture is not widened.
    pub sprite_size: (u32, u32),
    pub pads: (i32, i32),
    /// How many rows of `screen` the game shows (an SNES frame without
    /// overscan draws 224 of the host's 240): the window the camera fits.
    pub rows: u32,
    /// The screen position to look at (screen pixels, y down): the player,
    /// or the middle of the shown rows.
    pub centre: (f32, f32),
    pub out: (u32, u32),
    pub view: DioramaView,
}

/// Where a `window`-sized view centred on `centre` may look: the centre,
/// held so the window stays inside an `extent`-sized picture. A window
/// larger than the picture (zoom 1.0 on a padded grid is smaller, never
/// larger) is centred.
#[must_use]
pub fn clamp_window_centre(
    centre: (f32, f32),
    window: (f32, f32),
    zoom: f32,
    extent: (f32, f32),
) -> (f32, f32) {
    let (hw, hh) = (window.0 / 2.0 / zoom, window.1 / 2.0 / zoom);
    (
        centre
            .0
            .clamp(hw.min(extent.0 / 2.0), (extent.0 - hw).max(extent.0 / 2.0)),
        centre
            .1
            .clamp(hh.min(extent.1 / 2.0), (extent.1 - hh).max(extent.1 / 2.0)),
    )
}

/// Everything [`render_ground_window`] needs about a floor-plan scene.
pub struct GroundWindow<'a> {
    pub scene: &'a DioramaScene<'a>,
    pub ground_rgba: &'a [u8],
    pub ground: (u32, u32),
    pub sprite_rgba: &'a [u8],
    pub sprite_size: (u32, u32),
    /// The game's own window on the floor (world pixels) and the point on
    /// the floor to centre (already smoothed and clamped).
    pub window: (f32, f32),
    pub target: (f32, f32),
    pub out: (u32, u32),
    pub view: DioramaView,
}

/// Render a floor-plan scene with the camera fitted to the game's window
/// (D1): the window fills the frame at zoom 1, `target` is at its centre.
///
/// # Errors
/// Propagates the pass's `Err` (GPU readback timeout).
pub fn render_ground_window(
    gpu: &GpuContext,
    pass: &rf_renderer::diorama::DioramaPass,
    f: &GroundWindow<'_>,
) -> Result<DioramaRender, String> {
    use camera_matrices::WindowPlane;
    let camera = f.view.camera_at(f.target).clamped();
    let aspect = f.view.projection_aspect(f.out);
    let p = camera_matrices::window_placement(
        WindowPlane::Ground,
        f.window.0,
        f.window.1,
        aspect,
        rf_renderer::diorama::WINDOW_FOV_Y_DEG,
        &camera,
        f.target,
    );
    let vertices = rf_renderer::diorama_mesh::build_vertices_oriented(
        f.scene,
        camera.yaw_deg,
        [p.eye[0], p.eye[2]],
    );
    let vp = camera_matrices::view_proj_window(
        WindowPlane::Ground,
        f.window.0,
        f.window.1,
        aspect,
        rf_renderer::diorama::WINDOW_FOV_Y_DEG,
        &camera,
        f.target,
    );
    let rgba = pass.render_with_vp(
        gpu,
        &vertices,
        f.ground_rgba,
        f.ground.0,
        f.ground.1,
        f.sprite_rgba,
        f.sprite_size.0,
        f.sprite_size.1,
        vp,
        f.out.0,
        f.out.1,
    )?;
    Ok(DioramaRender {
        rgba,
        width: f.out.0,
        height: f.out.1,
    })
}

/// Render one top-down frame on the existing builder.
///
/// # Errors
/// Propagates the pass's `Err` (GPU readback timeout).
pub fn render_top(
    gpu: &GpuContext,
    pass: &rf_renderer::diorama::DioramaPass,
    f: &TopFrame<'_>,
) -> Result<DioramaRender, String> {
    // The rows the game shows: the rest of the host's frame is not picture.
    let shown = f.rows.clamp(1, f.screen.1.max(1));
    let base = f.lattice.to_live_grid((f.screen.0, shown));
    let (base_w, base_h) = base.extent();
    // D2: the floor goes on past the room by `EDGE_MARGIN_CELLS`, in the
    // room's own edge colours, so a pitched camera never looks off the plane
    // into the clear colour. The camera itself is still held to the room.
    let grid = base.with_margin(EDGE_MARGIN_CELLS);
    let ground = crate::live_heights::padded_ground(f.frame_rgba, f.screen.0, shown, &grid);
    let (ground_w, ground_h) = grid.extent();
    let billboards = top_billboards_wide(f.sprites, &grid, f.screen, f.sprite_size, f.pads);
    let scene = DioramaScene {
        tiles_w: grid.cols,
        tiles_h: grid.rows,
        tile_px: grid.tile_px as f32,
        solid: &grid.solid,
        depth: &grid.depth,
        billboards: &billboards,
    };
    let window = (f.screen.0 as f32, shown as f32);
    let zoom = f.view.camera.clamped().zoom;
    let centre = if f.view.follow {
        f.centre
    } else {
        (window.0 / 2.0, window.1 / 2.0)
    };
    let (gx, gy) = grid.screen_to_grid(centre.0, centre.1);
    let margin = (EDGE_MARGIN_CELLS * grid.tile_px) as f32;
    let held = clamp_window_centre(
        (gx - margin, gy - margin),
        window,
        zoom,
        (base_w as f32, base_h as f32),
    );
    let target = (held.0 + margin, held.1 + margin);
    render_ground_window(
        gpu,
        pass,
        &GroundWindow {
            scene: &scene,
            ground_rgba: &ground,
            ground: (ground_w, ground_h),
            sprite_rgba: f.sprite_rgba,
            sprite_size: f.sprite_size,
            window,
            target,
            out: f.out,
            view: f.view,
        },
    )
}

/// Cells of floor the top-down Diorama lays beyond the picture on every side.
pub const EDGE_MARGIN_CELLS: u32 = 16;

#[cfg(test)]
mod tests {
    #[test]
    fn the_yaw_is_capped_only_where_the_planes_stop_covering_the_view() {
        // A 32:9 picture 1829 wide, the window fitted to its width at
        // distance ~840: the extension a picture's width each side leaves a
        // little less than the camera's 30 degrees.
        let fov = rf_renderer::diorama::WINDOW_FOV_Y_DEG;
        let wide = super::yaw_cap_deg(840.0, fov, 1829.0 / 448.0, 914.0 + 1829.0);
        assert!((20.0..30.0).contains(&wide), "{wide}");
        // A 4:3 picture with the same reach in pictures: room to spare.
        let std = super::yaw_cap_deg(300.0, fov, 256.0 / 224.0, 128.0 + 256.0);
        assert_eq!(std, 30.0);
        // More reach, more yaw; none at all is no yaw.
        let more = super::yaw_cap_deg(840.0, fov, 1829.0 / 448.0, 2.0 * 1829.0);
        assert!(more >= wide);
        assert_eq!(super::yaw_cap_deg(840.0, fov, 4.0, 0.0), 0.0);
    }

    use super::*;

    #[test]
    fn the_window_centre_is_held_so_the_window_stays_on_the_picture() {
        // A 256x224 window on a 1000x300 picture: at zoom 1 it can slide
        // between x = 128 and 872; zoomed 2x the window is half as wide.
        let w = (256.0, 224.0);
        let ext = (1000.0, 300.0);
        assert_eq!(clamp_window_centre((10.0, 150.0), w, 1.0, ext).0, 128.0);
        assert_eq!(clamp_window_centre((990.0, 150.0), w, 1.0, ext).0, 872.0);
        assert_eq!(
            clamp_window_centre((500.0, 150.0), w, 1.0, ext),
            (500.0, 150.0)
        );
        assert_eq!(clamp_window_centre((10.0, 150.0), w, 2.0, ext).0, 64.0);
        // The window is taller than the picture: centred, not pushed out.
        assert_eq!(
            clamp_window_centre((500.0, 10.0), w, 1.0, (1000.0, 100.0)).1,
            50.0
        );
    }

    fn grid(cols: u16, rows: u16, offset: (u8, u8), top: u16) -> TileGrid {
        TileGrid {
            layer: 0,
            cell_px: 8,
            hud: false,
            exact: true,
            rows_px: (top, top + rows * 8 - offset.1 as u16),
            origin_world: (0, 0),
            offset,
            cols,
            rows,
            cells: vec![Default::default(); usize::from(cols) * usize::from(rows)],
        }
    }

    fn heights(g: &TileGrid, f: impl Fn(usize) -> (Class, u8)) -> HeightGrid {
        let n = g.cells.len();
        let mut h = HeightGrid {
            cols: g.cols,
            rows: g.rows,
            cell_px: g.cell_px,
            offset: g.offset,
            class: vec![Class::Unknown; n],
            depth: vec![0; n],
            confidence: vec![3; n],
        };
        for i in 0..n {
            let (c, d) = f(i);
            h.class[i] = c;
            h.depth[i] = d;
        }
        h
    }

    #[test]
    fn orientation_picks_the_builder_and_unknown_stays_flat_unless_painted() {
        assert_eq!(plan_for(Orientation::SideView, false), Plan::Side);
        assert_eq!(plan_for(Orientation::TopDown, false), Plan::Top);
        assert_eq!(plan_for(Orientation::Unknown, false), Plan::Flat);
        assert_eq!(plan_for(Orientation::Unknown, true), Plan::Top);
        assert_eq!(plan_for(Orientation::SideView, true), Plan::Side);
    }

    #[test]
    fn only_solid_classes_are_raised_and_floor_air_and_backdrop_stay_flat() {
        let g = grid(3, 2, (0, 0), 0);
        let classes = [
            Class::Wall,
            Class::Floor,
            Class::Backdrop,
            Class::Platform,
            Class::Unknown,
            Class::Ramp(-3),
        ];
        let h = heights(&g, |i| (classes[i], 8));
        let l = Lattice::from_learned(&g, &h);
        assert_eq!(l.depth, vec![8, 0, 0, 8, 0, 8]);
        assert_eq!(l.slope, vec![0, 0, 0, 0, 0, -3]);
        assert_eq!(l.raised(), 3);
    }

    fn level_canvas() -> HeightCanvas {
        // 6 x 4 cells of 8 px at canvas (100, 52): a wall column at col 3 and
        // a ramp at (5, 3).
        let mut hc = HeightCanvas::new((100, 52), 6, 4, 8).unwrap();
        for r in 0..4 {
            hc.set(
                3,
                r,
                rf_enhance::heights::Learned {
                    class: Class::Wall,
                    height_eighths: 8,
                    confidence: 3,
                },
            );
        }
        hc.set(
            5,
            3,
            rf_enhance::heights::Learned {
                class: Class::Ramp(2),
                height_eighths: 8,
                confidence: 2,
            },
        );
        // Below the draw level: flat.
        hc.set(
            0,
            0,
            rf_enhance::heights::Learned {
                class: Class::Wall,
                height_eighths: 8,
                confidence: 1,
            },
        );
        hc
    }

    #[test]
    fn a_level_canvas_lays_its_cells_on_a_wider_picture_than_the_screen() {
        let hc = level_canvas();
        // A 40 x 24 picture whose corner is canvas (97, 50): cell (0,0) of the
        // canvas starts 3 px right and 2 px down of it.
        let l = Lattice::from_level(&hc, (97, 50), (40, 24));
        assert_eq!(
            l.origin,
            (-5, -6),
            "the first cell on the canvas grid, left of the corner"
        );
        // Picture x 0..40 over cells starting at -5: ceil(45/8) = 6 columns;
        // y: ceil(30/8) = 4 rows.
        assert_eq!((l.cols, l.rows, l.tile_px), (6, 4, 8));
        // Canvas col 3 sits at picture x = 100 + 24 - 97 = 27, i.e. lattice
        // column (27 + 5) / 8 = 4; the canvas starts at lattice column 1 (-5 + 8 = 3).
        let at = |c: usize, r: usize| l.depth[r * 6 + c];
        for r in 0..4 {
            assert_eq!(at(4, r), if r >= 1 { 8 } else { 0 }, "row {r}");
        }
        assert_eq!(
            l.raised(),
            3,
            "rows 1..4 of the wall; the weak cell and the ramp are off the view or flat"
        );
    }

    #[test]
    fn a_level_picture_that_starts_left_of_the_canvas_keeps_the_grid() {
        let hc = level_canvas();
        let l = Lattice::from_level(&hc, (60, 44), (48, 32));
        // Nothing of the canvas at columns before it; the wall is at canvas
        // x 124..132 = picture x 64..72 -> beyond a 48 px view.
        assert_eq!(l.raised(), 0);
        let l = Lattice::from_level(&hc, (100, 52), (48, 32));
        assert_eq!(l.origin, (0, 0));
        assert_eq!(l.raised(), 4 + 1, "the wall column and the ramp");
        assert_eq!(l.slope[3 * 6 + 5], 2);
    }

    #[test]
    fn billboards_on_a_wide_plane_shift_with_the_plane_not_with_the_texture() {
        let s = SpriteBox {
            x: 100,
            y: 50,
            w: 16,
            h: 16,
            behind_bg: false,
            tile: 1,
        };
        // The core frame is 352 wide (pad 48); the plane is 800 wide (pad 272).
        let b = side_billboards_wide(&[s], (352, 240), 48, 272);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].center_x_px, 100.0 + 8.0 + 272.0);
        assert_eq!(b[0].feet_y_px, 66.0);
        // The sprite texture is the core's: u from the core x (100 + 48).
        assert!((b[0].uv[0] - 148.0 / 352.0).abs() < 1e-6);
        assert!((b[0].uv[2] - 164.0 / 352.0).abs() < 1e-6);
        // Without widening it is the plain builder.
        let plain = side_billboards(&[s], (256, 240));
        let same = side_billboards_wide(&[s], (256, 240), 0, 0);
        assert_eq!(plain.len(), same.len());
        assert_eq!(plain[0].center_x_px, same[0].center_x_px);
    }

    #[test]
    fn a_hud_grid_is_never_raised() {
        let mut g = grid(2, 1, (0, 0), 0);
        g.hud = true;
        let h = heights(&g, |_| (Class::Wall, 8));
        assert_eq!(Lattice::from_learned(&g, &h).raised(), 0);
    }

    #[test]
    fn a_band_lattice_sits_where_the_picture_has_it() {
        // A band starting at picture row 64 whose first row is 3 px scrolled.
        let g = grid(4, 3, (5, 3), 64);
        let h = heights(&g, |_| (Class::Wall, 8));
        let l = Lattice::from_learned(&g, &h);
        assert_eq!(l.origin, (-5, 61));
    }

    #[test]
    fn a_band_lattice_becomes_a_full_screen_grid_cell_for_cell() {
        // 8 px cells, band at picture row 16 (cell rows start at 16), no
        // scroll: lattice row 0 is full-screen row 2.
        let g = grid(32, 4, (0, 0), 16);
        let h = heights(&g, |i| {
            if i == 33 {
                (Class::Wall, 8)
            } else {
                (Class::Unknown, 0)
            }
        });
        let l = Lattice::from_learned(&g, &h);
        let live = l.to_live_grid((256, 224));
        assert_eq!((live.cols, live.rows, live.offset), (32, 28, (0, 0)));
        // Lattice cell (1, 1) is picture (8, 24): full-screen col 1, row 3.
        assert_eq!(live.depth[3 * 32 + 1], 8);
        assert_eq!(live.depth.iter().filter(|d| **d > 0).count(), 1);
        assert_eq!(live.solid[3 * 32 + 1], 1);
    }

    #[test]
    fn sub_cell_scroll_gives_the_grid_its_offset() {
        let g = grid(33, 3, (5, 2), 0);
        let h = heights(&g, |_| (Class::Unknown, 0));
        let live = Lattice::from_learned(&g, &h).to_live_grid((256, 224));
        assert_eq!(live.offset, (5, 2));
        assert_eq!(live.cols, (256u32 + 5).div_ceil(8));
    }

    #[test]
    fn ground_without_sprites_fills_a_sprite_from_its_row_neighbours() {
        // One row of 5 pixels, the sprite covers the middle three.
        let mut sprite = vec![0u8; 20];
        for x in 1..4 {
            sprite[x * 4..x * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
        let mut frame = vec![0u8; 20];
        frame[0..4].copy_from_slice(&[10, 0, 0, 255]);
        frame[4..8].copy_from_slice(&[255, 0, 0, 255]); // the sprite itself
        frame[16..20].copy_from_slice(&[0, 0, 200, 255]);
        let out = ground_without_sprites(&sprite, &frame, 5);
        assert_eq!(&out[0..4], &[10, 0, 0, 255], "uncovered pixels are kept");
        assert_eq!(&out[4..8], &[10, 0, 0, 255], "nearer the left");
        assert_eq!(&out[12..16], &[0, 0, 200, 255], "nearer the right");
        assert_eq!(&out[16..20], &[0, 0, 200, 255]);
        // No sprite pixels at all: the frame, untouched. A size mismatch too.
        assert_eq!(ground_without_sprites(&[0; 20], &frame, 5), frame);
        assert_eq!(ground_without_sprites(&sprite[..8], &frame, 5), frame);
    }

    #[test]
    fn billboards_keep_the_feet_and_drop_what_is_off_screen() {
        let s = [
            SpriteBox {
                x: 100,
                y: 50,
                w: 8,
                h: 16,
                behind_bg: false,
                tile: 1,
            },
            SpriteBox {
                x: 100,
                y: 240,
                w: 8,
                h: 16,
                behind_bg: false,
                tile: 2,
            },
            SpriteBox {
                x: -20,
                y: 10,
                w: 8,
                h: 8,
                behind_bg: false,
                tile: 3,
            },
        ];
        let b = side_billboards(&s, (256, 224));
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].center_x_px, b[0].feet_y_px), (104.0, 66.0));
        assert_eq!(b[0].uv[0], 100.0 / 256.0);
    }

    fn live_grid(offset: (u32, u32)) -> LiveGrid {
        LiveGrid {
            cols: 33,
            rows: 29,
            tile_px: 8,
            offset,
            solid: vec![0; 33 * 29],
            depth: vec![0; 33 * 29],
        }
    }

    fn spr(x: i16, y: i16, w: u8, h: u8) -> SpriteBox {
        SpriteBox {
            x,
            y,
            w,
            h,
            behind_bg: false,
            tile: 0,
        }
    }

    #[test]
    fn a_top_down_sprite_stands_at_its_position_shifted_into_the_grid() {
        let b = top_billboards(&[spr(40, 100, 16, 16)], &live_grid((3, 5)), (256, 224));
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].width_px, b[0].height_px), (16.0, 16.0));
        assert_eq!(
            (b[0].center_x_px, b[0].center_z_px),
            (43.0 + 8.0, 105.0 + 16.0)
        );
        assert_eq!(
            b[0].uv,
            [40.0 / 256.0, 100.0 / 224.0, 56.0 / 256.0, 116.0 / 224.0]
        );
    }

    #[test]
    fn parked_and_wholly_off_screen_sprites_are_not_billboarded() {
        let g = live_grid((0, 0));
        let one = |s: SpriteBox| top_billboards(&[s], &g, (256, 224)).len();
        assert_eq!(one(spr(10, 240, 8, 8)), 0);
        assert_eq!(one(spr(-8, 10, 8, 8)), 0);
        assert_eq!(one(spr(-4, 10, 8, 8)), 1);
        assert_eq!(one(spr(256, 10, 8, 8)), 0);
    }

    #[test]
    fn a_slower_backdrop_is_set_further_back() {
        assert_eq!(set_back_px(None, 8.0), 0.0);
        assert_eq!(set_back_px(Some(1.0), 8.0), 0.0);
        assert!(set_back_px(Some(0.5), 8.0) > 0.0);
        assert!(set_back_px(Some(0.25), 8.0) > set_back_px(Some(0.5), 8.0));
    }
}
