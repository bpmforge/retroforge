//! Per-layer stitched canvases for SNES widescreen margins (ticket W31-07).
//!
//! [`crate::canvas_accum::CanvasAccumulator`] stitches the COMPOSITED frame
//! at ONE layer's scroll. On a game whose backgrounds scroll at different
//! rates (hills at half the ground's speed, a status bar on its own layer)
//! that repeats terrain at the wrong offset, paints HUD pieces into the
//! world and loses the sky. Here each background layer the widescreen policy
//! widens gets its own canvas, placed by that layer's own scroll registers:
//! layer `n`'s canvas takes only the pixels whose `PpuPixel::layer` is
//! `Background(n)` (sprites and other layers are holes, never written, never
//! erasing a known cell). Layers the policy keeps narrow (the HUD) get none.
//!
//! ## Conventions
//!
//! * Same world convention as `canvas_accum::FramePosition`: bundle column
//!   `c` of row `sy` lands on world `(world_x + c, world_y + sy)` and the
//!   256-dot picture's left edge is `world_x + pad`.
//! * Positions come from the per-frame `hofs`/`vofs` registers (one sample
//!   per frame, wrapped to the tilemap, accumulated with
//!   `rf_enhance::scroll_tracker::WorldAxis`). A game that changes a layer's
//!   scroll mid-frame (HDMA parallax) is stitched at its first value:
//!   fullsnes does not let us do better from register snapshots.
//! * Margin order, front to back (W32-08): per picture row, by the 4:3
//!   picture's own evidence ([`LayerCanvasAccumulator::row_orders`]: where the
//!   picture shows layer A at a spot layer B also has a known cell, A is in
//!   front of B); BG1 > BG2 > BG3 > BG4 where a row has no evidence.
//! * Which layers get a canvas (W32-08): every background layer that is on
//!   the main screen, whatever the widescreen policy says -- the policy is
//!   about what the PPU draws, the canvas is about what we remember (SMW's
//!   far hills are BG2 and the default policy keeps them narrow). A HUD layer
//!   (its scroll never moved while another layer's did, and it only draws a
//!   band of rows) keeps its canvas but is left out of the margins
//!   ([`LayerCanvases::hud`]).
//! * A margin cell no layer knows shows the row's backdrop colour (palette
//!   index 0 with the row palette), bsnes-hd's "auto" fill, not black.
//!
//! * **Which scroll a frame is stitched at (W32-07, W32-08).** The scroll
//!   registers read after a frame are the ones that draw the NEXT frame (the
//!   game's NMI writes them in vblank), so every stitcher is fed the values
//!   read after the PREVIOUS frame. Measured on Super Mario World: stitched
//!   at its own read, a frame lands one frame of scroll too far. [`ScrollLag`]
//!   is the one place that rule lives: the composite canvas, the layer
//!   canvases and the scout all go through it.
//! * **The level scout (W32-07).** `crate::scout` stitches a second machine's
//!   frames into its own accumulator (`fork_axes`: same world axes) and its
//!   cells fill only the unknown cells of these canvases (`merge_unknown_*`).
//!
//! Presentation only: this reads finished `FrameBundle`s and never touches a
//! core.
use std::collections::HashMap;

use rf_core_api::{FrameBundle, PixelLayer, PpuPixel};
use rf_enhance::scene_identity::SceneId;
use rf_enhance::scroll_tracker::WorldAxis;
use rf_enhance::stitcher::Canvas;

/// A world-space box: `(x0, y0, x1, y1)`, the end exclusive.
pub type Rect = (i64, i64, i64, i64);

/// How many background layers the SNES has.
pub const LAYERS: usize = 4;

/// One widened layer's scroll for the frame: registers already wrapped to
/// the tilemap, plus the tilemap size they wrap at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerScroll {
    pub hofs: u16,
    pub vofs: u16,
    pub tilemap_width: u16,
    pub tilemap_height: u16,
}

/// What the scroll registers said at one read: the scroll the composite
/// (single-layer) stitcher wants and the per-layer scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameScrolls {
    /// The first widened layer's `(hofs, vofs)`, wrapped to its tilemap
    /// ([`crate::canvas_accum::CanvasAccumulator::observe_frame_with_scroll`]).
    pub composite: Option<(u16, u16)>,
    /// Every stitched layer's scroll ([`LayerCanvasAccumulator::observe_frame`]).
    pub layers: Option<[Option<LayerScroll>; LAYERS]>,
}

/// Ticket W32-08: the scroll a frame was DRAWN with.
///
/// The registers read after a frame draw the NEXT one (the game's NMI writes
/// them in vblank, after the visible lines; measured on Super Mario World in
/// W32-07), so frame `n` is stitched at the read taken after frame `n - 1`.
/// One helper for all stitchers: before this, the composite canvas used the
/// same-frame read and the layer canvases the previous one, a scroll step
/// apart.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollLag {
    prev: Option<(u64, FrameScrolls)>,
}

impl ScrollLag {
    /// Record `read` as taken after frame `frame` without asking for a drawn
    /// scroll (a scout pass starts from the restored registers).
    pub fn prime(&mut self, frame: u64, read: FrameScrolls) {
        self.prev = Some((frame, read));
    }

    /// Frame `frame` has just run and `read` is what the registers hold now.
    /// Returns the scroll that drew `frame`: the previous read, if it was
    /// taken right after frame `frame - 1` (otherwise nothing is known and
    /// the frame must not be placed).
    pub fn drawn_with(&mut self, frame: u64, read: FrameScrolls) -> FrameScrolls {
        let drawn = match self.prev.take() {
            Some((n, s)) if n + 1 == frame => s,
            _ => FrameScrolls::default(),
        };
        self.prev = Some((frame, read));
        drawn
    }

    /// Forget the last read.
    pub fn forget(&mut self) {
        self.prev = None;
    }
}

/// Per scene, what a layer's scroll and picture rows have done so far (the
/// evidence for [`LayerCanvasAccumulator::hud_layers`]).
#[derive(Debug, Clone, Copy, Default)]
struct Motion {
    seen: bool,
    x: (i64, i64),
    y: (i64, i64),
    /// First and last picture row the layer drew a pixel on.
    rows: (usize, usize),
    height: usize,
}

impl Motion {
    fn observe(&mut self, wx: i64, wy: i64, rows: Option<(usize, usize)>, height: usize) {
        if self.seen {
            self.x = (self.x.0.min(wx), self.x.1.max(wx));
            self.y = (self.y.0.min(wy), self.y.1.max(wy));
        } else {
            self.seen = true;
            self.x = (wx, wx);
            self.y = (wy, wy);
            self.rows = (usize::MAX, 0);
        }
        self.height = height;
        if let Some((lo, hi)) = rows {
            self.rows = (self.rows.0.min(lo), self.rows.1.max(hi));
        }
    }

    fn moved(&self) -> bool {
        self.seen && (self.x.1 - self.x.0 >= MOVED_PX || self.y.1 - self.y.0 >= MOVED_PX)
    }

    /// Draws only a band of the picture (less than three quarters of the
    /// rows) and has drawn something at all.
    fn fixed_band(&self) -> bool {
        self.seen
            && self.rows.0 <= self.rows.1
            && (self.rows.1 - self.rows.0 + 1) * 4 < self.height * 3
    }
}

/// A layer's world position must have spanned this many pixels to count as
/// having scrolled (a one pixel wobble is not a camera).
const MOVED_PX: i64 = 8;

/// Where each widened layer's 256-dot picture sits in its own world this
/// frame (same convention as `canvas_accum::FramePosition::origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerFrames {
    pub scene: u64,
    pub origins: [Option<(i64, i64)>; LAYERS],
}

/// A scene's per-layer canvases, shipped to the UI thread. `None` = the
/// layer has no canvas (never widened, or nothing stitched yet).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerCanvases {
    pub canvases: [Option<Canvas>; LAYERS],
    /// Layers that are HUD (see [`LayerCanvasAccumulator::hud_layers`]): they
    /// keep their canvas but give the margins nothing.
    pub hud: [bool; LAYERS],
}

impl LayerCanvases {
    /// Whether any layer has content to take a margin pixel from.
    #[must_use]
    pub fn any(&self) -> bool {
        self.canvases.iter().any(Option::is_some)
    }
}

/// Stateful per-ROM accumulator; driven on the core thread for every frame,
/// right after `CanvasAccumulator::observe_frame_with_scroll` (it shares
/// that call's scene id, so the two never disagree on the scene).
#[derive(Default)]
pub struct LayerCanvasAccumulator {
    axes: [(WorldAxis, WorldAxis); LAYERS],
    scenes: HashMap<u64, [Canvas; LAYERS]>,
    last: Option<LayerFrames>,
    last_scene: Option<u64>,
    /// Per layer, the bounding box of the cells written since
    /// [`Self::take_dirty`] (world coordinates).
    dirty: [Option<Rect>; LAYERS],
    /// Per scene and layer, the scroll/rows evidence for the HUD test.
    motion: HashMap<u64, [Motion; LAYERS]>,
}

impl LayerCanvasAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one frame. `scrolls[n]` is `Some` for each layer to stitch this
    /// frame (every layer on the main screen, W32-08); `None` for a layer
    /// that is not stitched. `scrolls == None` (NES, widescreen off) records no
    /// positions but keeps what was stitched.
    pub fn observe_frame(
        &mut self,
        bundle: &FrameBundle,
        scene: SceneId,
        scrolls: Option<&[Option<LayerScroll>; LAYERS]>,
    ) {
        self.last = None;
        self.last_scene = Some(scene.0);
        let Some(scrolls) = scrolls else { return };
        let width = usize::from(bundle.width);
        if width == 0 {
            return;
        }
        let pad = i64::from(bundle.width.saturating_sub(256) / 2);
        let canvases = self.scenes.entry(scene.0).or_default();
        let motion = self.motion.entry(scene.0).or_default();
        let mut origins = [None; LAYERS];
        let mut row_buf: Vec<Option<PpuPixel>> = Vec::with_capacity(width);
        for (n, scroll) in scrolls.iter().enumerate() {
            let Some(scroll) = scroll else { continue };
            let (ax, ay) = &mut self.axes[n];
            let wx = ax.advance(scroll.hofs, i64::from(scroll.tilemap_width.max(1)));
            let wy = ay.advance(scroll.vofs, i64::from(scroll.tilemap_height.max(1)));
            let wanted = PixelLayer::Background(n as u8);
            let mut rows_drawn: Option<(usize, usize)> = None;
            // Bounded `for` over the frame's rows (law 8).
            for (sy, row) in bundle.video.chunks_exact(width).enumerate() {
                row_buf.clear();
                row_buf.extend(row.iter().map(|p| (p.layer == wanted).then_some(*p)));
                // A row this layer did not draw at all must not grow the
                // canvas with empty cells.
                if row_buf.iter().any(Option::is_some) {
                    let y = wy + sy as i64;
                    canvases[n].blit_row(wx, y, &row_buf);
                    let r = (wx, y, wx + width as i64, y + 1);
                    self.dirty[n] = Some(self.dirty[n].map_or(r, |d| union(d, r)));
                    rows_drawn = Some(rows_drawn.map_or((sy, sy), |(lo, _)| (lo, sy)));
                }
            }
            motion[n].observe(wx, wy, rows_drawn, usize::from(bundle.height));
            origins[n] = Some((wx + pad, wy));
        }
        self.last = Some(LayerFrames {
            scene: scene.0,
            origins,
        });
    }

    /// Ticket W32-07: a fresh accumulator that CONTINUES this one's world
    /// axes (so its positions are in the same world coordinates) but holds
    /// no canvases: what a scout stitches into before its cells are merged
    /// into the live canvases with [`Self::merge_unknown_from`].
    #[must_use]
    pub fn fork_axes(&self) -> Self {
        Self {
            axes: self.axes,
            scenes: HashMap::new(),
            last: None,
            last_scene: self.last_scene,
            dirty: [None; LAYERS],
            motion: HashMap::new(),
        }
    }

    /// Ticket W32-07: put this accumulator's world axes back to `seed`'s
    /// (a [`Self::fork_axes`] of the live one) and forget the last frame's
    /// positions; the canvases stitched so far are kept. A scout's next pass
    /// restarts from the snapshot, so its positions restart from the seed.
    pub fn rewind_axes(&mut self, seed: &Self) {
        self.axes = seed.axes;
        self.last = None;
    }

    /// Ticket W32-08: which layers of `scene` are HUD: their scroll never
    /// moved while another layer's did, and they only draw a band of rows.
    /// Judged on everything seen so far, so a layer stitched before the
    /// camera first moved is left out of the margins as soon as it is known.
    #[must_use]
    pub fn hud_layers(&self, scene: u64) -> [bool; LAYERS] {
        let Some(m) = self.motion.get(&scene) else {
            return [false; LAYERS];
        };
        std::array::from_fn(|n| {
            let other_moved = m.iter().enumerate().any(|(i, o)| i != n && o.moved());
            other_moved && !m[n].moved() && m[n].fixed_band()
        })
    }

    /// Ticket W32-08: for each picture row of the frame just observed, the
    /// layers front to back, by the picture's own evidence. Where the
    /// picture shows layer A at a spot layer B also has a known cell for, A
    /// is in front of B; layers the picture says nothing about stay in layer
    /// order (BG1 first). Empty before a frame has been placed.
    #[must_use]
    pub fn row_orders(&self, bundle: &FrameBundle) -> Vec<[u8; LAYERS]> {
        let (Some(frames), width) = (self.last, usize::from(bundle.width)) else {
            return Vec::new();
        };
        let Some(set) = self.scenes.get(&frames.scene) else {
            return Vec::new();
        };
        let pad = i64::from(bundle.width.saturating_sub(256) / 2);
        let hud = self.hud_layers(frames.scene);
        let known = |n: usize, c: usize, y: usize| -> bool {
            frames.origins[n]
                .is_some_and(|(ox, oy)| set[n].get(ox - pad + c as i64, oy + y as i64).is_some())
        };
        let default: [u8; LAYERS] = std::array::from_fn(|i| i as u8);
        if width == 0 {
            return Vec::new();
        }
        // Bounded `for` over rows and columns (law 8).
        bundle
            .video
            .chunks_exact(width)
            .enumerate()
            .map(|(y, row)| {
                let mut wins = [[0i32; LAYERS]; LAYERS];
                for (c, p) in row.iter().enumerate() {
                    let PixelLayer::Background(a) = p.layer else {
                        continue;
                    };
                    let a = usize::from(a);
                    if a >= LAYERS || hud[a] {
                        continue;
                    }
                    for b in (0..LAYERS).filter(|&b| b != a && !hud[b]) {
                        if known(b, c, y) {
                            wins[a][b] += 1;
                        }
                    }
                }
                // Insertion sort by pairwise evidence: a layer moves in front
                // of the one before it only when the picture showed it
                // winning against that one. `j` drops on every iteration, so
                // the inner walk ends (law 8).
                let mut order = default;
                for i in 1..LAYERS {
                    let mut j = i;
                    while j > 0 {
                        let (front, back) = (usize::from(order[j - 1]), usize::from(order[j]));
                        if wins[back][front] <= wins[front][back] {
                            break;
                        }
                        order.swap(j - 1, j);
                        j -= 1;
                    }
                }
                order
            })
            .collect()
    }

    /// Ticket W32-08: what was stitched since the last call, cut out of the
    /// canvases of `scene` into canvases of their own (the dirty boxes'
    /// cells only), for a worker to ship to the thread that merges them
    /// ([`Self::merge_unknown_from`]).
    pub fn take_delta(&mut self, scene: u64) -> Option<[Canvas; LAYERS]> {
        let dirty = self.take_dirty();
        let set = self.scenes.get(&scene)?;
        let mut out: [Canvas; LAYERS] = std::array::from_fn(|_| Canvas::new());
        for ((from, rect), to) in set.iter().zip(dirty).zip(out.iter_mut()) {
            let Some((rx0, ry0, rx1, ry1)) = rect else {
                continue;
            };
            let (w, h) = (from.width(), from.height());
            if w == 0 || h == 0 {
                continue;
            }
            let (ox, oy) = from.origin();
            let (x0, x1) = (rx0.max(ox), rx1.min(ox + w as i64));
            let (y0, y1) = (ry0.max(oy), ry1.min(oy + h as i64));
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            for wy in y0..y1 {
                let start = (wy - oy) as usize * w + (x0 - ox) as usize;
                to.blit_row(x0, wy, &from.cells()[start..start + (x1 - x0) as usize]);
            }
        }
        Some(out)
    }

    /// The per-layer canvases stitched for `scene`, borrowed.
    #[must_use]
    pub fn scene_canvases(&self, scene: u64) -> Option<&[Canvas; LAYERS]> {
        self.scenes.get(&scene)
    }

    /// The bounding box of what was stitched per layer since the last call.
    pub fn take_dirty(&mut self) -> [Option<Rect>; LAYERS] {
        std::mem::take(&mut self.dirty)
    }

    /// As [`Self::merge_unknown_from`], looking only inside `within` per
    /// layer (a scout's [`Self::take_dirty`]): the cost of a merge follows
    /// what changed, not the size of the canvases.
    pub fn merge_unknown_within(
        &mut self,
        scene: u64,
        from: &[Canvas; LAYERS],
        within: &[Option<Rect>; LAYERS],
    ) -> usize {
        let Some(live) = self.scenes.get_mut(&scene) else {
            return 0;
        };
        live.iter_mut()
            .zip(from)
            .zip(within)
            .map(|((live, scout), rect)| rect.map_or(0, |r| merge_unknown_in(live, scout, Some(r))))
            .sum()
    }

    /// Ticket W32-07: fill the cells of `scene`'s canvases that are still
    /// UNKNOWN from `from` (a scout's canvases); a cell the live game has
    /// seen is never touched, so live always wins. A scene with no live
    /// canvas yet takes nothing (the scout belongs to a scene the live game
    /// has not been in: a stale scout). Returns how many cells were filled.
    pub fn merge_unknown_from(&mut self, scene: u64, from: &[Canvas; LAYERS]) -> usize {
        let Some(live) = self.scenes.get_mut(&scene) else {
            return 0;
        };
        live.iter_mut()
            .zip(from)
            .map(|(live, scout)| merge_unknown(live, scout))
            .sum()
    }

    /// Where the latest frame sits on each layer's canvas.
    #[must_use]
    pub fn frames(&self) -> Option<LayerFrames> {
        self.last
    }

    /// A clone of the current scene's non-empty layer canvases, for the UI
    /// snapshot (pull, throttled: a clone is O(canvas size)).
    #[must_use]
    pub fn current_canvases(&self) -> LayerCanvases {
        let mut out = LayerCanvases::default();
        if let Some(set) = self.last_scene.and_then(|s| self.scenes.get(&s)) {
            out.hud = self.hud_layers(self.last_scene.unwrap_or_default());
            for (slot, canvas) in out.canvases.iter_mut().zip(set) {
                if canvas.width() > 0 && canvas.height() > 0 {
                    *slot = Some(canvas.clone());
                }
            }
        }
        out
    }
}

/// Ticket W32-07: copy into `live` every cell `scout` knows and `live` does
/// not (live always wins). Grows `live` to cover the scout's cells. Returns
/// the number of cells filled.
pub fn merge_unknown(live: &mut Canvas, scout: &Canvas) -> usize {
    merge_unknown_in(live, scout, None)
}

fn union(a: Rect, b: Rect) -> Rect {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

/// [`merge_unknown`] restricted to the world box `within` (`None` = all).
pub fn merge_unknown_in(live: &mut Canvas, scout: &Canvas, within: Option<Rect>) -> usize {
    let (w, h) = (scout.width(), scout.height());
    if w == 0 || h == 0 {
        return 0;
    }
    let (ox, oy) = scout.origin();
    // The scout's own box, clipped to `within`, in canvas indices.
    let (x0, y0, x1, y1) = within.map_or((ox, oy, ox + w as i64, oy + h as i64), |r| {
        (
            r.0.max(ox),
            r.1.max(oy),
            r.2.min(ox + w as i64),
            r.3.min(oy + h as i64),
        )
    });
    if x0 >= x1 || y0 >= y1 {
        return 0;
    }
    let mut filled = 0;
    let mut row: Vec<Option<PpuPixel>> = Vec::with_capacity((x1 - x0) as usize);
    // Bounded `for` over the clipped rows (law 8).
    for wy in y0..y1 {
        let start = (wy - oy) as usize * w + (x0 - ox) as usize;
        let src = &scout.cells()[start..start + (x1 - x0) as usize];
        row.clear();
        row.extend(src.iter().enumerate().map(|(i, cell)| {
            let cell = (*cell)?;
            live.get(x0 + i as i64, wy).is_none().then_some(cell)
        }));
        let n = row.iter().flatten().count();
        if n > 0 {
            live.blit_row(x0, wy, &row);
            filled += n;
        }
    }
    filled
}

/// The margin pixel at `(dx, dy)` from the composite picture's left edge /
/// top row: the front-most layer with a known cell at that layer's own world
/// position, else `None`. `origins` are this frame's
/// [`LayerFrames::origins`]; HUD layers give nothing; `order` is the row's
/// front-to-back order ([`LayerCanvasAccumulator::row_orders`]), BG1 first
/// when `None`.
#[must_use]
pub fn front_layer_pixel(
    layers: &LayerCanvases,
    origins: &[Option<(i64, i64)>; LAYERS],
    dx: i64,
    dy: i64,
    order: Option<&[u8; LAYERS]>,
) -> Option<PpuPixel> {
    const DEFAULT: [u8; LAYERS] = [0, 1, 2, 3];
    order.unwrap_or(&DEFAULT).iter().find_map(|&n| {
        let n = usize::from(n);
        if *layers.hud.get(n)? {
            return None;
        }
        let canvas = layers.canvases.get(n)?.as_ref()?;
        let (ox, oy) = (*origins.get(n)?)?;
        canvas.get(ox + dx, oy + dy)
    })
}

/// Per picture row, the colour the backdrop shows on screen: the most
/// common displayed colour among the row's `Backdrop` pixels, `None` for a
/// row with none. The most common, not the first: with colour math the
/// first backdrop pixel can sit on a cloud or hill edge of the sub-screen,
/// which streaked the margins (W31-07 probe, 2026-10-16).
///
/// Why not just palette index 0: games like Super Mario World draw their sky
/// through colour math, so the row's backdrop ON SCREEN differs from
/// CGRAM entry 0 (black there). The displayed pixel is the honest answer.
/// `rgba` is the frame as displayed, `width` columns wide.
#[must_use]
pub fn row_backdrops(video: &[PpuPixel], width: u16, rgba: &[u8]) -> Vec<Option<[u8; 3]>> {
    let width = usize::from(width);
    if width == 0 {
        return Vec::new();
    }
    // Bounded `for`-style walk over the rows (law 8).
    video
        .chunks_exact(width)
        .enumerate()
        .map(|(y, row)| {
            let mut counts: std::collections::HashMap<[u8; 3], u32> =
                std::collections::HashMap::new();
            for (x, _) in row
                .iter()
                .enumerate()
                .filter(|(_, p)| p.layer == PixelLayer::Backdrop)
            {
                let at = (y * width + x) * 4;
                if let Some(p) = rgba.get(at..at + 3) {
                    *counts.entry([p[0], p[1], p[2]]).or_default() += 1;
                }
            }
            // Ties break on the colour itself, so the pick is deterministic.
            counts
                .into_iter()
                .max_by_key(|&(c, n)| (n, c))
                .map(|(c, _)| c)
        })
        .collect()
}

/// The palette index a margin cell shows: the front-most known layer's, else
/// 0 (the row's backdrop colour once resolved with the row palette).
#[must_use]
pub fn margin_palette_index(
    layers: &LayerCanvases,
    origins: &[Option<(i64, i64)>; LAYERS],
    dx: i64,
    dy: i64,
) -> u8 {
    front_layer_pixel(layers, origins, dx, dy, None).map_or(0, |p| p.palette_index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::CoreEvent;

    const W: u16 = 64;
    const H: u16 = 4;

    fn px(layer: PixelLayer, v: u8) -> PpuPixel {
        PpuPixel {
            palette_index: v,
            layer,
            sprite_id: None,
            priority: 0,
        }
    }

    /// Left half is BG1, right half BG2; each layer's pixel value encodes
    /// its OWN world column (`hofs + c`), so a misplaced layer is visible.
    fn bundle(hofs: [u16; 2], mutate: impl Fn(usize, usize, &mut PpuPixel)) -> FrameBundle {
        let mut video = Vec::new();
        for y in 0..usize::from(H) {
            for c in 0..usize::from(W) {
                let n = usize::from(c >= usize::from(W) / 2);
                let v = (hofs[n] as usize + c) as u8;
                let mut p = px(PixelLayer::Background(n as u8), v);
                mutate(y, c, &mut p);
                video.push(p);
            }
        }
        FrameBundle::new(0, W, H, video, Vec::<CoreEvent>::new())
    }

    fn scroll(hofs: u16) -> Option<LayerScroll> {
        Some(LayerScroll {
            hofs,
            vofs: 0,
            tilemap_width: 512,
            tilemap_height: 256,
        })
    }

    fn scrolls(h0: u16, h1: u16) -> [Option<LayerScroll>; LAYERS] {
        [scroll(h0), scroll(h1), None, None]
    }

    #[test]
    fn two_layers_scrolling_at_different_rates_land_at_their_own_offsets() {
        let mut acc = LayerCanvasAccumulator::new();
        let id = SceneId(7);
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
        let first = acc.frames().unwrap();
        // BG1 moves 16, BG2 only 4 (hills move slower than the ground).
        acc.observe_frame(&bundle([16, 4], |_, _, _| {}), id, Some(&scrolls(16, 4)));
        let second = acc.frames().unwrap();
        assert_eq!(second.scene, 7);
        let (o0, o1) = (first.origins, second.origins);
        assert_eq!(o0[0].unwrap().0 + 16, o1[0].unwrap().0);
        assert_eq!(o0[1].unwrap().0 + 4, o1[1].unwrap().0);
        let set = acc.current_canvases();
        let (bg1, bg2) = (set.canvases[0].as_ref().unwrap(), set.canvases[1].as_ref());
        // BG1 owns screen columns 0..32 only: world 0..48 after two frames.
        assert_eq!(bg1.get(47, 0).unwrap().palette_index, 47);
        assert_eq!(bg1.get(48, 0), None, "BG1 never drew past its half");
        // BG2 owns screen columns 32..64: world 32..68, value = its world x.
        let bg2 = bg2.unwrap();
        assert_eq!(bg2.get(67, 1).unwrap().palette_index, 67);
        assert_eq!(bg2.get(31, 0), None);
        // And no cross-talk: BG1's canvas holds no BG2 pixel.
        assert_eq!(bg1.get(50, 0), None);
    }

    #[test]
    fn a_pixel_covered_by_a_sprite_does_not_erase_a_known_cell() {
        let mut acc = LayerCanvasAccumulator::new();
        let id = SceneId(1);
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
        let covered = |y: usize, c: usize, p: &mut PpuPixel| {
            if y == 0 && c == 10 {
                *p = px(PixelLayer::Sprite, 99);
            }
            if y == 1 && c == 11 {
                *p = px(PixelLayer::Background(2), 98); // another layer
            }
        };
        acc.observe_frame(&bundle([0, 0], covered), id, Some(&scrolls(0, 0)));
        let set = acc.current_canvases();
        let bg1 = set.canvases[0].as_ref().unwrap();
        assert_eq!(bg1.get(10, 0).unwrap().palette_index, 10);
        assert_eq!(bg1.get(11, 1).unwrap().palette_index, 11);
    }

    #[test]
    fn a_hole_stays_unknown_until_a_later_frame_reveals_it() {
        let mut acc = LayerCanvasAccumulator::new();
        let id = SceneId(1);
        let hole = |_: usize, c: usize, p: &mut PpuPixel| {
            if c == 5 {
                *p = px(PixelLayer::Sprite, 1);
            }
        };
        acc.observe_frame(&bundle([0, 0], hole), id, Some(&scrolls(0, 0)));
        let before = acc.current_canvases();
        assert_eq!(before.canvases[0].as_ref().unwrap().get(5, 0), None);
        acc.observe_frame(&bundle([1, 1], |_, _, _| {}), id, Some(&scrolls(1, 1)));
        let after = acc.current_canvases();
        assert_eq!(
            after.canvases[0]
                .as_ref()
                .unwrap()
                .get(5, 0)
                .map(|p| p.palette_index),
            Some(5)
        );
    }

    #[test]
    fn a_layer_the_policy_keeps_narrow_is_not_stitched() {
        let mut acc = LayerCanvasAccumulator::new();
        // BG2 (the right half) is the HUD this time.
        let s = [scroll(0), None, None, None];
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), SceneId(1), Some(&s));
        let set = acc.current_canvases();
        assert!(set.canvases[0].is_some());
        assert!(set.canvases[1].is_none(), "narrow layer has no canvas");
        assert_eq!(acc.frames().unwrap().origins[1], None);
    }

    #[test]
    fn no_scroll_input_records_no_positions_and_keeps_the_canvases() {
        let mut acc = LayerCanvasAccumulator::new();
        let id = SceneId(1);
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), id, None);
        assert_eq!(acc.frames(), None);
        assert!(acc.current_canvases().any());
    }

    #[test]
    fn a_scroll_register_wrap_keeps_the_world_continuous() {
        let mut acc = LayerCanvasAccumulator::new();
        let id = SceneId(1);
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(508, 508)));
        let a = acc.frames().unwrap().origins[0].unwrap().0;
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(4, 4)));
        let b = acc.frames().unwrap().origins[0].unwrap().0;
        assert_eq!(b - a, 8, "511 -> 0 is a step of 8, not a jump back");
    }

    #[test]
    fn scenes_keep_separate_canvases() {
        let mut acc = LayerCanvasAccumulator::new();
        acc.observe_frame(
            &bundle([0, 0], |_, _, _| {}),
            SceneId(1),
            Some(&scrolls(0, 0)),
        );
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), SceneId(2), None);
        assert!(
            !acc.current_canvases().any(),
            "scene 2 has stitched nothing"
        );
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), SceneId(1), None);
        assert!(acc.current_canvases().any());
    }

    fn canvas_with(cells: &[(i64, i64, u8)]) -> Canvas {
        let mut c = Canvas::new();
        for &(x, y, v) in cells {
            c.blit_row(x, y, &[Some(px(PixelLayer::Background(0), v))]);
        }
        c
    }

    #[test]
    fn the_margin_takes_the_front_most_layer_with_a_known_cell() {
        let layers = LayerCanvases {
            canvases: [
                Some(canvas_with(&[(100, 0, 11)])),
                Some(canvas_with(&[(100, 0, 22), (50, 0, 33)])),
                None,
                None,
            ],
            ..Default::default()
        };
        // BG1's world is offset by 100, BG2's by 0: dx -> own coordinates.
        let origins = [Some((100, 0)), Some((0, 0)), None, None];
        assert_eq!(
            margin_palette_index(&layers, &origins, 0, 0),
            11,
            "BG1 wins"
        );
        assert_eq!(
            margin_palette_index(&layers, &origins, 100, 0),
            22,
            "BG1 unknown there, BG2 shows"
        );
        assert_eq!(margin_palette_index(&layers, &origins, 50, 0), 33);
    }

    #[test]
    fn an_unknown_margin_cell_falls_back_to_the_backdrop_colour() {
        let layers = LayerCanvases {
            canvases: [Some(canvas_with(&[(0, 0, 5)])), None, None, None],
            ..Default::default()
        };
        let origins = [Some((0, 0)), None, None, None];
        assert_eq!(front_layer_pixel(&layers, &origins, 40, 3, None), None);
        let idx = margin_palette_index(&layers, &origins, 40, 3);
        assert_eq!(idx, 0);
        let mut words = [0u16; 256];
        words[0] = 0b0_00000_11111_00000; // green backdrop
        let pal = rf_renderer::LinePalette::from_words(&words, 15);
        let rgb = rf_renderer::resolve_index(idx, Some(&pal));
        assert!(
            rgb[1] > 200 && rgb[0] < 8 && rgb[2] < 8,
            "backdrop, not black: {rgb:?}"
        );
    }

    #[test]
    fn row_backdrops_read_the_displayed_colour_of_a_backdrop_pixel() {
        // Row 0: BG, backdrop (displayed blue), BG. Row 1: no backdrop.
        let bg = px(PixelLayer::Background(0), 3);
        let back = px(PixelLayer::Backdrop, 0);
        let video = vec![bg, back, bg, bg, bg, bg];
        let mut rgba = vec![0u8; 6 * 4];
        rgba[4..7].copy_from_slice(&[0, 98, 189]);
        let rows = row_backdrops(&video, 3, &rgba);
        assert_eq!(rows, vec![Some([0, 98, 189]), None]);
        assert!(row_backdrops(&video, 0, &rgba).is_empty());
    }

    #[test]
    fn margins_compose_through_fill_margins() {
        let layers = LayerCanvases {
            canvases: [Some(canvas_with(&[(-1, 0, 7)])), None, None, None],
            ..Default::default()
        };
        let origins = [Some((0, 0)), None, None, None];
        let (h, out_w) = (1usize, 258usize);
        let native = vec![0xAAu8; 256 * h * 4];
        let world = |wx: i64, wy: i64| -> Option<[u8; 3]> {
            let i = margin_palette_index(&layers, &origins, wx, wy);
            Some(rf_renderer::resolve_index(i, None))
        };
        let out =
            rf_enhance::widescreen::fill_margins(&native, 256, h, out_w, (0, 0), &world, &|_| {
                false
            });
        let left = rf_renderer::resolve_index(7, None);
        let backdrop = rf_renderer::resolve_index(0, None);
        assert_eq!(
            &out[0..3],
            &left[..],
            "left margin cell is BG1's known cell"
        );
        let last = (out_w - 1) * 4;
        assert_eq!(&out[last..last + 3], &backdrop[..], "right is backdrop");
    }

    // ---- W32-07: merging a scout's canvases into the live ones

    fn row_of(c: &Canvas, y: i64, xs: std::ops::Range<i64>) -> Vec<Option<u8>> {
        xs.map(|x| c.get(x, y).map(|p| p.palette_index)).collect()
    }

    #[test]
    fn a_merge_fills_only_unknown_cells_and_live_wins() {
        let mut live = canvas_with(&[(0, 0, 1), (1, 0, 1), (2, 0, 1), (3, 0, 1)]);
        let scout = canvas_with(&[(2, 0, 9), (3, 0, 9), (4, 0, 9), (5, 0, 9)]);
        let filled = merge_unknown(&mut live, &scout);
        assert_eq!(filled, 2, "only x = 4 and 5 were unknown");
        assert_eq!(
            row_of(&live, 0, 0..7),
            vec![Some(1), Some(1), Some(1), Some(1), Some(9), Some(9), None]
        );
        // Idempotent: a second merge adds nothing.
        assert_eq!(merge_unknown(&mut live, &scout), 0);
    }

    #[test]
    fn a_merge_grows_the_live_canvas_to_the_scouts_cells() {
        let mut live = canvas_with(&[(0, 0, 1)]);
        let scout = canvas_with(&[(100, 3, 7)]);
        assert_eq!(merge_unknown(&mut live, &scout), 1);
        assert_eq!(live.get(100, 3).map(|p| p.palette_index), Some(7));
        assert_eq!(live.get(0, 0).map(|p| p.palette_index), Some(1));
    }

    fn two_scenes() -> (LayerCanvasAccumulator, [Canvas; LAYERS]) {
        let mut live = LayerCanvasAccumulator::new();
        live.observe_frame(
            &bundle([0, 0], |_, _, _| {}),
            SceneId(1),
            Some(&scrolls(0, 0)),
        );
        let scout = [
            canvas_with(&[(300, 0, 5)]),
            Canvas::new(),
            Canvas::new(),
            Canvas::new(),
        ];
        (live, scout)
    }

    #[test]
    fn a_scout_only_fills_the_scene_it_was_started_for() {
        let (mut live, scout) = two_scenes();
        assert_eq!(
            live.merge_unknown_from(2, &scout),
            0,
            "no canvas for scene 2"
        );
        assert_eq!(live.scene_canvases(1).unwrap()[0].get(300, 0), None);
        assert_eq!(live.merge_unknown_from(1, &scout), 1);
        let got = live.scene_canvases(1).unwrap()[0].get(300, 0);
        assert_eq!(got.map(|p| p.palette_index), Some(5));
    }

    #[test]
    fn a_dirty_box_limits_what_a_merge_looks_at() {
        let (mut live, _) = two_scenes();
        let scout = [
            canvas_with(&[(300, 0, 5), (900, 0, 6)]),
            Canvas::new(),
            Canvas::new(),
            Canvas::new(),
        ];
        let within = [Some((250, 0, 400, 1)), None, None, None];
        assert_eq!(live.merge_unknown_within(1, &scout, &within), 1);
        let bg1 = &live.scene_canvases(1).unwrap()[0];
        assert!(bg1.get(300, 0).is_some());
        assert_eq!(
            bg1.get(900, 0),
            None,
            "outside the box: left for the next merge"
        );
    }

    #[test]
    fn dirty_boxes_follow_what_was_stitched_and_clear_when_taken() {
        let mut acc = LayerCanvasAccumulator::new();
        acc.observe_frame(
            &bundle([0, 0], |_, _, _| {}),
            SceneId(1),
            Some(&scrolls(0, 0)),
        );
        acc.observe_frame(
            &bundle([16, 4], |_, _, _| {}),
            SceneId(1),
            Some(&scrolls(16, 4)),
        );
        let d = acc.take_dirty();
        // Each frame blits a full 64-wide row span (the cells the layer did
        // not draw are skipped inside it): world x 0..64 and 16..80.
        assert_eq!(d[0], Some((0, 0, 80, i64::from(H))));
        assert_eq!(d[2], None, "layer 3 was never stitched");
        assert_eq!(acc.take_dirty(), [None; LAYERS]);
    }

    #[test]
    fn a_forked_accumulator_continues_the_live_world_axes() {
        let mut live = LayerCanvasAccumulator::new();
        let id = SceneId(1);
        live.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(500, 0)));
        live.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(508, 0)));
        let live_x = live.frames().unwrap().origins[0].unwrap().0;
        let mut scout = live.fork_axes();
        // The raw register wrapped (508 -> 4): the world goes on to 516.
        scout.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(4, 0)));
        assert_eq!(scout.frames().unwrap().origins[0].unwrap().0, live_x + 8);
        assert!(
            live.current_canvases().canvases[0].is_some(),
            "live untouched"
        );
        // Rewinding puts the scout back at the live position.
        let seed = live.fork_axes();
        scout.rewind_axes(&seed);
        scout.observe_frame(
            &bundle([0, 0], |_, _, _| {}),
            id,
            Some(&scrolls(516 % 512, 0)),
        );
        assert_eq!(scout.frames().unwrap().origins[0].unwrap().0, live_x + 8);
    }

    // ---- W32-08: shared scroll timing, parallax layers, HUD, layer order

    #[test]
    fn the_lag_hands_a_frame_the_read_taken_after_the_one_before_it() {
        let read = |x: u16| FrameScrolls {
            composite: Some((x, 0)),
            layers: Some(scrolls(x, x)),
        };
        let mut lag = ScrollLag::default();
        // Nothing known before any read: the frame is not placed.
        assert_eq!(lag.drawn_with(1, read(10)), FrameScrolls::default());
        // Frame 2 was drawn with what was read after frame 1.
        assert_eq!(lag.drawn_with(2, read(20)), read(10));
        // A gap (frame 5 after frame 2): unknown again, then consecutive.
        assert_eq!(lag.drawn_with(5, read(50)), FrameScrolls::default());
        assert_eq!(lag.drawn_with(6, read(60)), read(50));
        // A primed read (a scout pass start) serves the next frame.
        lag.prime(100, read(7));
        assert_eq!(lag.drawn_with(101, read(8)), read(7));
        lag.forget();
        assert_eq!(lag.drawn_with(102, read(9)), FrameScrolls::default());
    }

    /// Picture whose BG1 pixel at screen column `c` holds `hofs + c`, i.e.
    /// its own world x, so any misplacement shows in the stitched cells.
    fn world_x_bundle(hofs: u16) -> FrameBundle {
        let mut video = Vec::new();
        for _ in 0..usize::from(H) {
            for c in 0..usize::from(W) {
                video.push(px(PixelLayer::Background(0), (usize::from(hofs) + c) as u8));
            }
        }
        let events = vec![
            rf_core_api::CoreEvent::Scanline(0),
            rf_core_api::CoreEvent::Scanline(H - 1),
        ];
        FrameBundle::new(0, W, H, video, events)
    }

    /// The set of `world x - value` over every known cell of a canvas: one
    /// element means every cell sits at the world x its content encodes.
    fn placement_offsets(c: &Canvas) -> std::collections::BTreeSet<i64> {
        let (ox, oy) = c.origin();
        let mut out = std::collections::BTreeSet::new();
        for y in 0..c.height() as i64 {
            for x in 0..c.width() as i64 {
                if let Some(p) = c.get(ox + x, oy + y) {
                    out.insert(ox + x - i64::from(p.palette_index));
                }
            }
        }
        out
    }

    #[test]
    fn the_composite_and_layer_canvases_place_the_same_content_at_the_same_world_x() {
        use crate::canvas_accum::CanvasAccumulator;
        // A camera with uneven steps, so a one-frame error cannot hide in a
        // constant offset. Frame k is drawn at DRAW[k]; the registers read
        // after frame k hold DRAW[k + 1] (they draw the next frame).
        const DRAW: [u16; 9] = [10, 13, 18, 20, 29, 31, 40, 41, 52];
        let read = |x: u16| FrameScrolls {
            composite: Some((x, 0)),
            layers: Some([scroll(x), None, None, None]),
        };
        let mut composite = CanvasAccumulator::new("w32-08".into(), None);
        let mut layers = LayerCanvasAccumulator::new();
        let mut lag = ScrollLag::default();
        lag.prime(0, read(DRAW[1]));
        for k in 1..DRAW.len() {
            let bundle = world_x_bundle(DRAW[k]);
            let drawn = lag.drawn_with(k as u64, read(DRAW.get(k + 1).copied().unwrap_or(0)));
            let scene = composite.observe_frame_with_scroll(&bundle, &[], drawn.composite);
            layers.observe_frame(&bundle, scene, drawn.layers.as_ref());
        }
        let comp = composite.current_canvas().expect("composite canvas");
        let lay = layers.current_canvases();
        let lay = lay.canvases[0].as_ref().expect("layer canvas");
        let (a, b) = (placement_offsets(&comp), placement_offsets(lay));
        assert_eq!(a.len(), 1, "composite: every cell at its world x: {a:?}");
        assert_eq!(b.len(), 1, "layer: every cell at its world x: {b:?}");
        assert_eq!(a, b, "the two canvases use the same world x");
    }

    #[test]
    fn a_layer_that_never_moves_while_another_does_is_hud_only_when_it_is_a_band() {
        let mut acc = LayerCanvasAccumulator::new();
        let id = SceneId(1);
        // BG1 scrolls; BG2 (right half) is full height at a fixed scroll;
        // BG3 is a one-row band at a fixed scroll.
        let band = |y: usize, _c: usize, p: &mut PpuPixel| {
            if y == 0 {
                *p = px(PixelLayer::Background(2), 9);
            }
        };
        let s = |h: u16| [scroll(h), scroll(4), scroll(0), None];
        acc.observe_frame(&bundle([0, 0], band), id, Some(&s(0)));
        assert_eq!(acc.hud_layers(1), [false; LAYERS], "nothing has moved yet");
        for h in [8u16, 16, 24] {
            acc.observe_frame(&bundle([h, 0], band), id, Some(&s(h)));
        }
        assert_eq!(
            acc.hud_layers(1),
            [false, false, true, false],
            "the band is HUD; the full-height static layer is not"
        );
        assert!(acc.current_canvases().hud[2]);
        assert!(
            acc.current_canvases().canvases[2].is_some(),
            "the HUD layer keeps its canvas"
        );
    }

    #[test]
    fn a_hud_layer_gives_the_margins_nothing_and_a_parallax_layer_does() {
        let layers = LayerCanvases {
            canvases: [
                None,
                Some(canvas_with(&[(5, 0, 22)])),
                Some(canvas_with(&[(5, 0, 33)])),
                None,
            ],
            hud: [false, false, true, false],
        };
        let o = Some((0, 0));
        let origins = [None, o, o, None];
        // BG2 (hills) shows although the policy never widened it; BG3 is HUD.
        assert_eq!(
            front_layer_pixel(&layers, &origins, 5, 0, None).map(|p| p.palette_index),
            Some(22)
        );
        let hud_first = [2u8, 1, 0, 3];
        assert_eq!(
            front_layer_pixel(&layers, &origins, 5, 0, Some(&hud_first)).map(|p| p.palette_index),
            Some(22),
            "even when the order puts the HUD layer in front"
        );
    }

    #[test]
    fn the_picture_decides_which_layer_is_in_front_row_by_row() {
        let mut acc = LayerCanvasAccumulator::new();
        let id = SceneId(1);
        let s = scrolls(0, 0);
        // Frame 1: BG1 left, BG2 right (each layer's cells become known).
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&s));
        // Frame 2: BG2 shows EVERYWHERE, also where BG1 is known: BG2 is in
        // front of BG1 on every row.
        let all_bg2 = |_: usize, c: usize, p: &mut PpuPixel| {
            *p = px(PixelLayer::Background(1), c as u8);
        };
        let b2 = bundle([0, 0], all_bg2);
        acc.observe_frame(&b2, id, Some(&s));
        let orders = acc.row_orders(&b2);
        assert_eq!(orders.len(), usize::from(H));
        assert!(orders.iter().all(|o| o[..2] == [1, 0]), "{orders:?}");
        // No evidence (a single known layer): the default order.
        let mut fresh = LayerCanvasAccumulator::new();
        let b1 = bundle([0, 0], |_, _, p| *p = px(PixelLayer::Background(0), 1));
        fresh.observe_frame(&b1, id, Some(&s));
        assert!(fresh.row_orders(&b1).iter().all(|o| *o == [0, 1, 2, 3]));
    }

    #[test]
    fn a_delta_carries_only_what_was_stitched_since_the_last_one() {
        let mut acc = LayerCanvasAccumulator::new();
        let id = SceneId(1);
        acc.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
        let first = acc.take_delta(1).expect("scene has canvases");
        assert!(first[0].get(0, 0).is_some());
        acc.observe_frame(&bundle([100, 0], |_, _, _| {}), id, Some(&scrolls(100, 0)));
        let second = acc.take_delta(1).unwrap();
        assert!(second[0].get(0, 0).is_none(), "already delivered");
        assert!(second[0].get(100, 0).is_some());
        // Merging the deltas into a live accumulator rebuilds the canvases.
        let mut live = LayerCanvasAccumulator::new();
        live.observe_frame(&bundle([0, 0], |_, _, _| {}), id, Some(&scrolls(0, 0)));
        assert_eq!(live.merge_unknown_from(1, &first), 0, "live already knows");
        assert!(live.merge_unknown_from(1, &second) > 0);
        assert!(live.scene_canvases(1).unwrap()[0].get(100, 0).is_some());
        assert!(acc.take_delta(2).is_none(), "no such scene");
    }
}
