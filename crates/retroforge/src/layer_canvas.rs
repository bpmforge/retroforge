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
//! * Positions come from the scroll registers, wrapped to the tilemap and
//!   accumulated with `rf_enhance::scroll_tracker::WorldAxis`. **Per line
//!   (W33-01):** when the bundle carries the registers each line was drawn
//!   with (`FrameBundle::lines`, widescreen on), every picture row of every
//!   layer has its OWN x axis, so an HDMA parallax band lands at the world x
//!   its rows were really drawn at, and no scroll-timing guess is needed
//!   (the registers are the ones that drew the line). A row's y comes from
//!   one axis per layer fed with the first line's `vofs` (vertical HDMA
//!   parallax is not modelled). Without line data (an old bundle, widescreen
//!   off) the per-frame registers are used as before, through [`ScrollLag`].
//! * **Sub-screen (W33-01).** A layer that is on the SUB screen at a dot the
//!   main screen gave to something else (Super Mario World's far hills are
//!   BG2 on the sub screen) still draws its pixel into its own canvas from
//!   `FrameBundle::sub`: the main winner is written first, a sub-screen
//!   pixel fills its own layer's canvas only where the main winner is a
//!   different layer. The margin compose ([`margin_color`]) then puts the
//!   sub-screen layer behind the main layers through the row's colour math.
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
//! * A margin cell no layer knows shows the row's backdrop colour, bsnes-hd's
//!   "auto" fill, not black. Since W33-02 that colour is RECONSTRUCTED from
//!   the row's registers ([`backdrop_color`]), not voted from the picture.
//! * A remembered cell is resolved with the palette it was seen under
//!   ([`crate::palette_table`]), not the palette of the row being drawn now.
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

use rf_core_api::{FrameBundle, LineRegisters, PixelLayer, PpuPixel};
use rf_enhance::scene_identity::SceneId;
use rf_enhance::scroll_tracker::WorldAxis;
use rf_enhance::stitcher::Canvas;

use crate::palette_table::{self, PaletteTable};

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

/// Ticket W33-01: the per-layer scroll [`LayerCanvasAccumulator::observe_frame`]
/// is given for `bundle`. A bundle that carries the registers each line was
/// drawn with needs no scroll-timing rule (the registers ARE the ones that
/// drew it), so it gets the current read, whose layer set and tilemap sizes
/// are all `observe_frame` takes from it; a bundle without them gets the read
/// that drew the frame ([`ScrollLag`]), or nothing when that is unknown.
#[must_use]
pub fn scrolls_for<'a>(
    bundle: &FrameBundle,
    drawn: &'a FrameScrolls,
    read: &'a FrameScrolls,
) -> Option<&'a [Option<LayerScroll>; LAYERS]> {
    if bundle.lines.iter().any(Option::is_some) {
        read.layers.as_ref()
    } else {
        drawn.layers.as_ref()
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerFrames {
    pub scene: u64,
    /// Where row 0 of the picture sits on each layer's canvas.
    pub origins: [Option<(i64, i64)>; LAYERS],
    /// Per picture row, how far that row's world x differs from
    /// `origins[n].0` for layer `n` (HDMA scroll bands, W33-01). Empty when
    /// no row carried line registers: every row sits at the origin.
    pub row_dx: Vec<[i32; LAYERS]>,
}

impl LayerFrames {
    /// Layer `n`'s origin for picture row `row`: the row's own world x (a
    /// per-line scroll) and the layer's y.
    #[must_use]
    pub fn origin_at(&self, n: usize, row: usize) -> Option<(i64, i64)> {
        let (ox, oy) = (*self.origins.get(n)?)?;
        let dx = self.row_dx.get(row).map_or(0, |r| r[n]);
        Some((ox + i64::from(dx), oy))
    }
}

/// A scene's per-layer canvases, shipped to the UI thread. `None` = the
/// layer has no canvas (never widened, or nothing stitched yet).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerCanvases {
    pub canvases: [Option<Canvas>; LAYERS],
    /// Layers that are HUD (see [`LayerCanvasAccumulator::hud_layers`]): they
    /// keep their canvas but give the margins nothing.
    pub hud: [bool; LAYERS],
    /// Ticket W33-02: the palettes the cells were seen under; a cell's
    /// `sprite_id` is its id here ([`crate::palette_table`]).
    pub palettes: PaletteTable,
}

impl LayerCanvases {
    /// Whether any layer has content to take a margin pixel from.
    #[must_use]
    pub fn any(&self) -> bool {
        self.canvases.iter().any(Option::is_some)
    }
}

/// One layer's world axes: the per-frame pair (registers read once per
/// frame) and, once line registers have been seen, one x axis per picture
/// row (a band's rows keep their own world x across frames, which a single
/// axis fed with varying rows would read as camera jumps).
#[derive(Debug, Clone, Default)]
struct LayerAxes {
    frame: (WorldAxis, WorldAxis),
    rows: Vec<WorldAxis>,
}

/// Stateful per-ROM accumulator; driven on the core thread for every frame,
/// right after `CanvasAccumulator::observe_frame_with_scroll` (it shares
/// that call's scene id, so the two never disagree on the scene).
#[derive(Default)]
pub struct LayerCanvasAccumulator {
    axes: [LayerAxes; LAYERS],
    scenes: HashMap<u64, [Canvas; LAYERS]>,
    last: Option<LayerFrames>,
    last_scene: Option<u64>,
    /// Per layer, the bounding box of the cells written since
    /// [`Self::take_dirty`] (world coordinates).
    dirty: [Option<Rect>; LAYERS],
    /// Per scene and layer, the scroll/rows evidence for the HUD test.
    motion: HashMap<u64, [Motion; LAYERS]>,
    /// Ticket W33-02: per scene, the palettes its cells were seen under.
    palettes: HashMap<u64, PaletteTable>,
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
        self.observe_frame_with_palettes(bundle, scene, scrolls, &[]);
    }

    /// [`Self::observe_frame`] with the palette each picture row was drawn
    /// with (ticket W33-02): the cells written are tagged with the id of
    /// their row's palette in the scene's [`PaletteTable`]. A row with no
    /// palette (NES, or `&[]`) writes cells with no id.
    pub fn observe_frame_with_palettes(
        &mut self,
        bundle: &FrameBundle,
        scene: SceneId,
        scrolls: Option<&[Option<LayerScroll>; LAYERS]>,
        line_palettes: &[Option<rf_renderer::LinePalette>],
    ) {
        self.last = None;
        self.last_scene = Some(scene.0);
        let Some(scrolls) = scrolls else { return };
        let width = usize::from(bundle.width);
        if width == 0 {
            return;
        }
        let height = usize::from(bundle.height);
        let pad = i64::from(bundle.width.saturating_sub(256) / 2);
        // W33-01: the registers each line was drawn with, when the bundle
        // has them. Then `scrolls` only says WHICH layers to stitch and at
        // what tilemap size; the positions come from the lines.
        let has_lines = bundle.lines.iter().any(Option::is_some);
        let line0 = bundle.lines.first().copied().flatten();
        let canvases = self.scenes.entry(scene.0).or_default();
        let motion = self.motion.entry(scene.0).or_default();
        let table = self.palettes.entry(scene.0).or_default();
        // One interned id per picture row (rows share a palette almost
        // always; `intern` is a hash lookup either way).
        let row_ids: Vec<Option<u8>> = (0..height)
            .map(|sy| {
                line_palettes
                    .get(sy)
                    .and_then(Option::as_ref)
                    .and_then(|p| table.intern(&p.words))
            })
            .collect();
        let mut origins = [None; LAYERS];
        let mut row_dx: Vec<[i32; LAYERS]> = if has_lines {
            vec![[0; LAYERS]; height]
        } else {
            Vec::new()
        };
        let mut row_buf: Vec<Option<PpuPixel>> = Vec::with_capacity(width);
        for (n, scroll) in scrolls.iter().enumerate() {
            let Some(scroll) = scroll else { continue };
            let (tw, th) = (scroll.tilemap_width.max(1), scroll.tilemap_height.max(1));
            let axes = &mut self.axes[n];
            // The per-frame read keeps its axis current: it is the fallback
            // for rows with no line registers.
            let wx_frame = axes.frame.0.advance(scroll.hofs, i64::from(tw));
            let vofs = line0.map_or(scroll.vofs, |l| l.vofs[n] % th);
            let wy = axes.frame.1.advance(vofs, i64::from(th));
            if has_lines && axes.rows.len() < height {
                axes.rows.resize(height, WorldAxis::new());
            }
            let wanted = PixelLayer::Background(n as u8);
            let mut rows_drawn: Option<(usize, usize)> = None;
            let mut base_x = wx_frame;
            // Bounded `for` over the frame's rows (law 8).
            for (sy, row) in bundle.video.chunks_exact(width).enumerate() {
                // Every row's own axis advances every frame, drawn or not,
                // so its deltas stay one frame apart.
                let wx = match bundle.lines.get(sy).copied().flatten() {
                    Some(l) => axes.rows[sy].advance(l.hofs[n] % tw, i64::from(tw)),
                    None => wx_frame,
                };
                if sy == 0 {
                    base_x = wx;
                }
                if has_lines {
                    row_dx[sy][n] = i32::try_from(wx - base_x).unwrap_or(0);
                }
                let sub = bundle.sub.get(sy).and_then(Option::as_ref);
                let id = row_ids.get(sy).copied().flatten();
                row_buf.clear();
                row_buf.extend(row.iter().enumerate().map(|(c, p)| {
                    if p.layer == wanted {
                        return Some(palette_table::tag(*p, id));
                    }
                    // The sub screen's pixel of THIS layer where the main
                    // screen showed something else (W33-01).
                    let sp = sub?.pixels.get(c).filter(|sp| sp.layer == wanted)?;
                    Some(PpuPixel {
                        palette_index: sp.palette_index,
                        layer: wanted,
                        sprite_id: id,
                        priority: 0,
                    })
                }));
                // A row this layer did not draw at all must not grow the
                // canvas with empty cells.
                if row_buf.iter().any(Option::is_some) {
                    let y = wy + sy as i64;
                    canvases[n].blit_row(wx, y, &row_buf);
                    let r = (wx, y, wx + width as i64, y + 1);
                    self.dirty[n] = Some(self.dirty[n].map_or(r, |d| rect_union(d, r)));
                    rows_drawn = Some(rows_drawn.map_or((sy, sy), |(lo, _)| (lo, sy)));
                }
            }
            motion[n].observe(base_x, wy, rows_drawn, height);
            origins[n] = Some((base_x + pad, wy));
        }
        self.last = Some(LayerFrames {
            scene: scene.0,
            origins,
            row_dx,
        });
    }

    /// Ticket W32-07: a fresh accumulator that CONTINUES this one's world
    /// axes (so its positions are in the same world coordinates) but holds
    /// no canvases: what a scout stitches into before its cells are merged
    /// into the live canvases with [`Self::merge_unknown_from`].
    #[must_use]
    pub fn fork_axes(&self) -> Self {
        Self {
            axes: self.axes.clone(),
            scenes: HashMap::new(),
            last: None,
            last_scene: self.last_scene,
            dirty: [None; LAYERS],
            motion: HashMap::new(),
            palettes: HashMap::new(),
        }
    }

    /// Ticket W32-07: put this accumulator's world axes back to `seed`'s
    /// (a [`Self::fork_axes`] of the live one) and forget the last frame's
    /// positions; the canvases stitched so far are kept. A scout's next pass
    /// restarts from the snapshot, so its positions restart from the seed.
    pub fn rewind_axes(&mut self, seed: &Self) {
        self.axes = seed.axes.clone();
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
        let (Some(frames), width) = (self.last.as_ref(), usize::from(bundle.width)) else {
            return Vec::new();
        };
        let Some(set) = self.scenes.get(&frames.scene) else {
            return Vec::new();
        };
        let pad = i64::from(bundle.width.saturating_sub(256) / 2);
        let hud = self.hud_layers(frames.scene);
        let known = |n: usize, c: usize, y: usize| -> bool {
            frames
                .origin_at(n, y)
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
            if let Some(rect) = rect {
                crop_into(to, from, rect);
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
        self.last.clone()
    }

    /// A clone of the current scene's non-empty layer canvases, for the UI
    /// snapshot (pull, throttled: a clone is O(canvas size)).
    #[must_use]
    pub fn current_canvases(&self) -> LayerCanvases {
        let mut out = LayerCanvases::default();
        if let Some(set) = self.last_scene.and_then(|s| self.scenes.get(&s)) {
            out.hud = self.hud_layers(self.last_scene.unwrap_or_default());
            if let Some(t) = self.last_scene.and_then(|s| self.palettes.get(&s)) {
                out.palettes = t.clone();
            }
            for (slot, canvas) in out.canvases.iter_mut().zip(set) {
                if canvas.width() > 0 && canvas.height() > 0 {
                    *slot = Some(canvas.clone());
                }
            }
        }
        out
    }
}

/// Copy the cells of `from` inside the world box `rect` into `to` (what a
/// scout ships as a delta).
pub fn crop_into(to: &mut Canvas, from: &Canvas, rect: Rect) {
    let (w, h) = (from.width(), from.height());
    if w == 0 || h == 0 {
        return;
    }
    let (ox, oy) = from.origin();
    let (x0, x1) = (rect.0.max(ox), rect.2.min(ox + w as i64));
    let (y0, y1) = (rect.1.max(oy), rect.3.min(oy + h as i64));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    // Bounded `for` over the clipped rows (law 8).
    for wy in y0..y1 {
        let start = (wy - oy) as usize * w + (x0 - ox) as usize;
        to.blit_row(x0, wy, &from.cells()[start..start + (x1 - x0) as usize]);
    }
}

/// Ticket W32-07: copy into `live` every cell `scout` knows and `live` does
/// not (live always wins). Grows `live` to cover the scout's cells. Returns
/// the number of cells filled.
pub fn merge_unknown(live: &mut Canvas, scout: &Canvas) -> usize {
    merge_unknown_in(live, scout, None)
}

/// The smallest box holding both.
pub fn rect_union(a: Rect, b: Rect) -> Rect {
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
    front_layer_pixel_in(layers, |n| origins[n], dx, dy, order, 0x0F)
}

/// [`front_layer_pixel`] with each layer's origin from `origin_of` (the
/// per-line scroll makes it depend on the row, [`LayerFrames::origin_at`])
/// and only the layers whose bit is set in `screen` (a `$212C`/`$212D`
/// mask, BG1 = bit 0).
#[must_use]
pub fn front_layer_pixel_in(
    layers: &LayerCanvases,
    origin_of: impl Fn(usize) -> Option<(i64, i64)>,
    dx: i64,
    dy: i64,
    order: Option<&[u8; LAYERS]>,
    screen: u8,
) -> Option<PpuPixel> {
    const DEFAULT: [u8; LAYERS] = [0, 1, 2, 3];
    order.unwrap_or(&DEFAULT).iter().find_map(|&n| {
        let n = usize::from(n);
        if screen & (1 << n) == 0 || *layers.hud.get(n)? {
            return None;
        }
        let canvas = layers.canvases.get(n)?.as_ref()?;
        let (ox, oy) = origin_of(n)?;
        canvas.get(ox + dx, oy + dy)
    })
}

/// Ticket W33-01: one margin pixel composed the way the picture is.
///
/// For the dot `dx` columns from the picture's left edge on picture row
/// `row`: the MAIN source is the front-most known cell of a layer on the
/// main screen on that row (`regs.tm`), else the row's backdrop colour; the
/// SUB source is the front-most known cell of a layer on the sub screen
/// (`regs.ts`) when the row's `$2130` bit 1 selects the sub screen, else the
/// fixed colour (`$2132`). Then the row's colour math runs exactly as the
/// picture's does: fullsnes "SNES PPU Color-Math": per 5-bit channel add or
/// subtract (`$2131` bit 7), clamp to 0..31, halve when `$2131` bit 6 says so
/// EXCEPT where the sub screen is selected but transparent (the fixed colour
/// stands in, and hardware does not halve it), and only where `$2131` enables
/// math for the main source (BG1-4 bits 0-3, backdrop bit 5). `$2130` bits
/// 6-7 = 3 force the main screen black first; bits 4-5 = 3 turn math off.
/// The colour WINDOW cases (modes 1 and 2 of either) cannot be evaluated off
/// the 256-dot picture, so a margin treats them as "no window effect".
/// Sprites are never known in a margin. Brightness (`$2100`) last.
///
/// Why this is what the hills need: Super Mario World keeps its far hills
/// (BG2) on the sub screen only and shows them through colour math over the
/// backdrop. A compose that only looks at main-screen layers draws sky there.
#[must_use]
pub fn margin_color(
    layers: &LayerCanvases,
    frames: &LayerFrames,
    row: usize,
    dx: i64,
    order: Option<&[u8; LAYERS]>,
    regs: &LineRegisters,
    palette: &rf_renderer::LinePalette,
) -> [u8; 3] {
    let dy = row as i64;
    let at = |screen: u8| {
        front_layer_pixel_in(layers, |n| frames.origin_at(n, row), dx, dy, order, screen)
    };
    // W33-02: a remembered cell shows under the palette it was seen with.
    let word = |p: &PpuPixel| layers.palettes.word(p, palette);
    let main = at(regs.tm & 0x0F);
    let (main_word, main_bit) = match main {
        Some(p) => (
            word(&p),
            match p.layer {
                PixelLayer::Background(n) => u32::from(n),
                _ => 5,
            },
        ),
        None => (regs.backdrop_color, 5),
    };
    let sub = regs
        .use_subscreen
        .then(|| at(regs.ts & 0x0F))
        .flatten()
        .map(|p| word(&p));
    shade(regs, main_word, main_bit, sub)
}

/// One dot's final colour: the row's colour math over `main_word` (a
/// BGR555 word whose source is `main_bit` of `$2131`: BG1-4 = 0-3, OBJ = 4,
/// backdrop = 5) and the second operand (`sub`, the sub-screen's word, or
/// `None` for a transparent sub screen = the fixed colour `$2132`), then
/// brightness. The rules are fullsnes "SNES PPU Color-Math" (see
/// [`margin_color`]); shared so the backdrop and the cells agree.
fn shade(regs: &LineRegisters, main_word: u16, main_bit: u32, sub: Option<u16>) -> [u8; 3] {
    let main_black = regs.clip_mode == 3;
    let main_word = if main_black { 0 } else { main_word };
    let op = if regs.prevent_mode == 3 || regs.math_enable & (1 << main_bit) == 0 {
        rf_core_api::ColorMathOp::None
    } else {
        let half = regs.half && !main_black && !(regs.use_subscreen && sub.is_none());
        match (regs.subtract, half) {
            (false, false) => rf_core_api::ColorMathOp::Add,
            (false, true) => rf_core_api::ColorMathOp::AddHalf,
            (true, false) => rf_core_api::ColorMathOp::Subtract,
            (true, true) => rf_core_api::ColorMathOp::SubtractHalf,
        }
    };
    let shown =
        rf_renderer::palette::color_math_bgr555(main_word, sub.unwrap_or(regs.fixed_color), op);
    rf_renderer::bgr555_to_rgb(shown, regs.brightness)
}

/// Ticket W33-02: the colour the BACKDROP shows on a row, reconstructed from
/// the registers the row was drawn with (fullsnes "SNES PPU Color-Math" and
/// "2100h - INIDISP"): CGRAM entry 0 as the main source (black when `$2130`
/// bits 6-7 force the main screen black), combined with the fixed colour
/// `$2132` when `$2131` bit 5 enables math for the backdrop (add/subtract
/// per bit 7, halved per bit 6 EXCEPT that a half is not applied when the
/// main screen is forced black), unless `$2130` bits 4-5 = 3 turn math off,
/// then scaled by the row's brightness (0 = force-blank = black).
///
/// This is what Super Mario World's HDMA sky gradient is: the fixed colour
/// changes per row, the backdrop is black, math adds them. No voting over
/// the picture, so a row nothing was drawn on still has an exact colour.
/// The sub screen is taken as transparent (the fixed colour): a layer on the
/// sub screen is a CELL, not the backdrop (see [`margin_color`]).
#[must_use]
pub fn backdrop_color(regs: &LineRegisters) -> [u8; 3] {
    shade(regs, regs.backdrop_color, 5, None)
}

/// Ticket W33-02: per picture row, the colour the backdrop shows there:
/// [`backdrop_color`] of the row's registers when it has them (SNES);
/// otherwise the palette index the picture's own `Backdrop` pixels carry
/// (the NES emits the already-resolved palette-RAM value for every dot,
/// `rf_nes::ppu` module doc, so its backdrop is exact too), resolved with the
/// row's line palette or the fixed NES table. `None` when no row has any
/// backdrop pixel and none has registers.
///
/// A row with fewer than `width / 8` backdrop pixels (a platform or a wall
/// covers it) is too thin to say anything and takes the nearest row above
/// with evidence, else the nearest below: the universal backdrop colour
/// (nesdev.org "PPU palettes", `$3F00`) changes on a split, not per row, and a
/// vote over a handful of dots streaked Mega Man 2's margins.
#[must_use]
pub fn backdrop_rows(
    bundle: &FrameBundle,
    line_palettes: &[Option<rf_renderer::LinePalette>],
) -> Vec<Option<[u8; 3]>> {
    let width = usize::from(bundle.width);
    if width == 0 {
        return Vec::new();
    }
    let enough = u32::try_from((width / 8).max(1)).unwrap_or(u32::MAX);
    // Per row: the registers' exact colour, or the winning backdrop index
    // when it has enough pixels to be believed.
    let votes: Vec<Result<[u8; 3], Option<u8>>> = bundle
        .video
        .chunks_exact(width)
        .enumerate()
        .map(|(y, row)| {
            if let Some(regs) = bundle.lines.get(y).and_then(Option::as_ref) {
                return Ok(backdrop_color(regs));
            }
            let mut counts: HashMap<u8, u32> = HashMap::new();
            for p in row.iter().filter(|p| p.layer == PixelLayer::Backdrop) {
                *counts.entry(p.palette_index).or_default() += 1;
            }
            // Ties break on the index, so the pick is deterministic.
            Err(counts
                .into_iter()
                .max_by_key(|&(i, n)| (n, i))
                .filter(|&(_, n)| n >= enough)
                .map(|(i, _)| i))
        })
        .collect();
    let rgb = |y: usize, v: &Result<[u8; 3], Option<u8>>| match v {
        Ok(c) => Some(*c),
        Err(idx) => idx
            .map(|i| rf_renderer::resolve_index(i, line_palettes.get(y).and_then(Option::as_ref))),
    };
    let mut out: Vec<Option<[u8; 3]>> = votes.iter().enumerate().map(|(y, v)| rgb(y, v)).collect();
    // Thin rows: carry the previous row's colour down, then the next row's
    // up for any still unset at the top. Each pass is one `for` over the
    // rows (law 8).
    let mut carry = None;
    for slot in &mut out {
        match slot {
            Some(c) => carry = Some(*c),
            None => *slot = carry,
        }
    }
    let mut carry = None;
    for slot in out.iter_mut().rev() {
        match slot {
            Some(c) => carry = Some(*c),
            None => *slot = carry,
        }
    }
    out
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

    fn bw(r: u16, g: u16, b: u16) -> u16 {
        r | (g << 5) | (b << 10)
    }

    /// Hand values from fullsnes "SNES PPU Color-Math" / "2100h - INIDISP":
    /// the SMW sky is a black backdrop plus a per-row fixed colour.
    #[test]
    fn the_backdrop_is_cgram_zero_through_the_rows_colour_math() {
        let sky = bw(5, 12, 20);
        let base = LineRegisters {
            math_enable: 0x20, // $2131 bit 5: math for the backdrop
            fixed_color: sky,
            backdrop_color: 0,
            brightness: 15,
            ..LineRegisters::default()
        };
        let rgb = |w: u16, b: u8| rf_renderer::bgr555_to_rgb(w, b);
        // Black + fixed colour = the fixed colour (the HDMA sky gradient).
        assert_eq!(backdrop_color(&base), rgb(sky, 15));
        // Another row, another fixed colour: no vote, no memory.
        let next = LineRegisters {
            fixed_color: bw(6, 13, 21),
            ..base
        };
        assert_eq!(backdrop_color(&next), rgb(bw(6, 13, 21), 15));
        // A lit CGRAM 0 adds per channel and clamps at 31.
        let lit = LineRegisters {
            backdrop_color: bw(30, 0, 12),
            ..base
        };
        assert_eq!(backdrop_color(&lit), rgb(bw(31, 12, 31), 15));
        // Half ($2131 bit 6) with the fixed colour as the operand.
        let half = LineRegisters { half: true, ..lit };
        assert_eq!(backdrop_color(&half), rgb(bw(17, 6, 16), 15));
        // ...but a selected, transparent sub screen is not halved.
        let sub = LineRegisters {
            use_subscreen: true,
            ..half
        };
        assert_eq!(backdrop_color(&sub), rgb(bw(31, 12, 31), 15));
        // Subtract ($2131 bit 7) clamps at 0.
        let subtract = LineRegisters {
            subtract: true,
            ..lit
        };
        assert_eq!(backdrop_color(&subtract), rgb(bw(25, 0, 0), 15));
        // Math not enabled for the backdrop, or never ($2130 bits 4-5 = 3):
        // CGRAM 0 alone.
        let off = LineRegisters {
            math_enable: 0x1F,
            ..lit
        };
        assert_eq!(backdrop_color(&off), rgb(bw(30, 0, 12), 15));
        let never = LineRegisters {
            prevent_mode: 3,
            ..lit
        };
        assert_eq!(backdrop_color(&never), rgb(bw(30, 0, 12), 15));
        // Main forced black ($2130 bits 6-7 = 3): fixed colour alone, no half.
        let black = LineRegisters {
            clip_mode: 3,
            half: true,
            ..lit
        };
        assert_eq!(backdrop_color(&black), rgb(sky, 15));
        // Brightness scales last; 0 (force blank) is black.
        let dim = LineRegisters {
            brightness: 7,
            ..base
        };
        assert_eq!(backdrop_color(&dim), rgb(sky, 7));
        let blank = LineRegisters {
            brightness: 0,
            ..base
        };
        assert_eq!(backdrop_color(&blank), [0, 0, 0]);
    }

    #[test]
    fn backdrop_rows_come_from_registers_else_from_the_pictures_backdrop_index() {
        let back = |i: u8| px(PixelLayer::Backdrop, i);
        let bg = px(PixelLayer::Background(0), 9);
        // Row 0 (registers): drawn bg only, still has an exact backdrop.
        // Row 1 (no registers, NES-like): mostly index 0x21, one 0x11.
        // Row 2: no backdrop pixel and no registers (takes row 1's).
        let video = vec![bg, bg, bg, back(0x21), back(0x21), back(0x11), bg, bg, bg];
        let mut b = FrameBundle::new(0, 3, 3, video, Vec::<CoreEvent>::new());
        let regs = LineRegisters {
            math_enable: 0x20,
            fixed_color: bw(1, 2, 3),
            brightness: 15,
            ..LineRegisters::default()
        };
        b.lines = vec![Some(regs), None, None];
        let rows = backdrop_rows(&b, &[]);
        assert_eq!(rows[0], Some(rf_renderer::bgr555_to_rgb(bw(1, 2, 3), 15)));
        assert_eq!(rows[1], Some(rf_renderer::resolve_index(0x21, None)));
        // Row 2 has nothing of its own: it takes the row above.
        assert_eq!(rows[2], rows[1]);
        // No evidence anywhere: nothing to say.
        let none = FrameBundle::new(0, 2, 1, vec![bg, bg], Vec::<CoreEvent>::new());
        assert_eq!(backdrop_rows(&none, &[]), vec![None]);
        let empty = FrameBundle::new(0, 0, 0, Vec::new(), Vec::<CoreEvent>::new());
        assert!(backdrop_rows(&empty, &[]).is_empty());
    }

    #[test]
    fn a_row_with_too_few_backdrop_pixels_takes_its_neighbours_colour() {
        // Width 16: a row needs 2 backdrop pixels. Row 0 has 1 (thin), row 1
        // has 9 of 0x21, row 2 has 1 of 0x05 (thin), row 3 none.
        let back = |i: u8| px(PixelLayer::Backdrop, i);
        let bg = px(PixelLayer::Background(0), 9);
        let mut video = Vec::new();
        for (n, i) in [(1, 0x30), (9, 0x21), (1, 0x05), (0, 0)] {
            video.extend((0..16).map(|c| if c < n { back(i) } else { bg }));
        }
        let b = FrameBundle::new(0, 16, 4, video, Vec::<CoreEvent>::new());
        let want = Some(rf_renderer::resolve_index(0x21, None));
        assert_eq!(backdrop_rows(&b, &[]), vec![want; 4]);
    }

    fn pal_words(index: u8, w: u16) -> [u16; 256] {
        let mut words = [0u16; 256];
        words[usize::from(index)] = w;
        words
    }

    /// A cell stitched under one palette keeps that palette after CGRAM
    /// changes (a per-area load, palette cycling): STITCHING_PLAN F4.
    #[test]
    fn a_remembered_cell_resolves_with_the_palette_it_was_seen_under() {
        let old = pal_words(9, bw(31, 0, 0));
        let new = pal_words(9, bw(0, 0, 31));
        let line = |w: &[u16; 256], b: u8| rf_renderer::LinePalette::from_words(w, b);
        // Frame 1: BG1 everywhere with palette index 9, CGRAM = `old`.
        let mut acc = LayerCanvasAccumulator::new();
        let frame = |hofs: u16| bundle([hofs, 0], |_, _, p| *p = px(PixelLayer::Background(0), 9));
        let b1 = frame(0);
        let scrolls = [scroll(0), None, None, None];
        let old_rows = vec![Some(line(&old, 15)); usize::from(H)];
        acc.observe_frame_with_palettes(&b1, SceneId(1), Some(&scrolls), &old_rows);
        // Frame 2: the game loaded a new palette; the camera moved on.
        let b2 = frame(0);
        let scrolls2 = [scroll(W), None, None, None];
        let new_rows = vec![Some(line(&new, 15)); usize::from(H)];
        acc.observe_frame_with_palettes(&b2, SceneId(1), Some(&scrolls2), &new_rows);
        let layers = acc.current_canvases();
        assert_eq!(layers.palettes.len(), 2, "two distinct CGRAM states");
        let frames = acc.frames().expect("placed");
        let regs = LineRegisters {
            tm: 0x01,
            brightness: 15,
            ..LineRegisters::default()
        };
        // The current row palette is `new`: a cell from frame 1 (world x 0,
        // before the origin of frame 2) must still come out RED.
        let cur = line(&new, 15);
        let (ox, _) = frames.origins[0].expect("origin");
        let dx_old = -ox + 3; // world x 3: frame 1 only
        let dx_new = 3; // frame 2's own cell
        let at = |dx: i64| margin_color(&layers, &frames, 0, dx, None, &regs, &cur);
        assert_eq!(at(dx_old), rf_renderer::bgr555_to_rgb(bw(31, 0, 0), 15));
        assert_eq!(at(dx_new), rf_renderer::bgr555_to_rgb(bw(0, 0, 31), 15));
    }

    /// A fade-in: the cell was remembered at brightness 4 and the picture is
    /// at brightness 4 now, then 12: brightness is applied to both alike,
    /// so the margin never lags the picture.
    #[test]
    fn a_fade_leaves_remembered_margin_colours_consistent_with_the_picture() {
        let words = pal_words(9, bw(20, 10, 30));
        let mut acc = LayerCanvasAccumulator::new();
        let b = bundle([0, 0], |_, _, p| *p = px(PixelLayer::Background(0), 9));
        let rows = vec![Some(rf_renderer::LinePalette::from_words(&words, 4)); usize::from(H)];
        acc.observe_frame_with_palettes(
            &b,
            SceneId(1),
            Some(&[scroll(0), None, None, None]),
            &rows,
        );
        let layers = acc.current_canvases();
        let frames = acc.frames().expect("placed");
        assert_eq!(
            layers.palettes.len(),
            1,
            "brightness is not part of the key"
        );
        for brightness in [4u8, 8, 15] {
            let regs = LineRegisters {
                tm: 0x01,
                brightness,
                ..LineRegisters::default()
            };
            let cur = rf_renderer::LinePalette::from_words(&words, brightness);
            let got = margin_color(&layers, &frames, 0, 5, None, &regs, &cur);
            // The picture's own colour for that index at that brightness.
            assert_eq!(got, cur.rgb(9), "brightness {brightness}");
        }
    }

    #[test]
    fn rows_without_a_line_palette_write_cells_with_no_id() {
        let mut acc = LayerCanvasAccumulator::new();
        let b = bundle([0, 0], |_, _, p| *p = px(PixelLayer::Background(0), 9));
        acc.observe_frame(&b, SceneId(1), Some(&[scroll(0), None, None, None]));
        let layers = acc.current_canvases();
        assert!(layers.palettes.is_empty());
        let cell = layers.canvases[0].as_ref().and_then(|c| c.get(5, 0));
        assert_eq!(cell.map(|c| c.sprite_id), Some(None));
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
            ..Default::default()
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

    // ---- W33-01: per-line scroll, sub-screen pixels, margin colour math

    /// BG1 left half, BG2 right half; each pixel's value is its own world x
    /// (`row_hofs[y][layer] + c`), with the registers each line was drawn
    /// with attached as line data.
    fn line_bundle(row_hofs: [[u16; 2]; H as usize]) -> FrameBundle {
        let mut video = Vec::new();
        for hofs in &row_hofs {
            for c in 0..usize::from(W) {
                let n = usize::from(c >= usize::from(W) / 2);
                video.push(px(
                    PixelLayer::Background(n as u8),
                    (usize::from(hofs[n]) + c) as u8,
                ));
            }
        }
        let mut b = FrameBundle::new(0, W, H, video, Vec::new());
        b.lines = row_hofs
            .iter()
            .map(|h| {
                Some(LineRegisters {
                    hofs: [h[0], h[1], 0, 0],
                    ..LineRegisters::default()
                })
            })
            .collect();
        b
    }

    /// Per canvas row, the set of `world x - value` of its known cells.
    fn row_offsets(c: &Canvas, y: i64) -> std::collections::BTreeSet<i64> {
        let (ox, _) = c.origin();
        (0..c.width() as i64)
            .filter_map(|x| {
                c.get(ox + x, y)
                    .map(|p| ox + x - i64::from(p.palette_index))
            })
            .collect()
    }

    #[test]
    fn a_layer_with_a_different_scroll_on_each_line_lands_each_row_at_its_own_world_x() {
        let mut acc = LayerCanvasAccumulator::new();
        let id = SceneId(1);
        // BG2 has an HDMA band: rows 1 and 3 are drawn 8 px further along.
        // The per-frame registers say 0 (the value at the start of the
        // frame); the lines say what really drew them.
        let f1 = line_bundle([[0, 0], [0, 8], [0, 0], [0, 8]]);
        acc.observe_frame(&f1, id, Some(&scrolls(0, 0)));
        let first = acc.frames().unwrap();
        assert_eq!(
            first.origin_at(1, 1).unwrap().0 - first.origin_at(1, 0).unwrap().0,
            8,
            "row 1 of BG2 sits 8 px right of row 0"
        );
        assert_eq!(first.origin_at(0, 1), first.origin_at(0, 0), "BG1 is flat");
        // The next frame everything moved on: BG1 by 16, BG2's rows by 4.
        let f2 = line_bundle([[16, 4], [16, 12], [16, 4], [16, 12]]);
        acc.observe_frame(&f2, id, Some(&scrolls(16, 4)));
        let set = acc.current_canvases();
        let bg2 = set.canvases[1].as_ref().unwrap();
        for y in 0..i64::from(H) {
            assert_eq!(
                row_offsets(bg2, y),
                [0].into(),
                "BG2 row {y}: every cell at the world x its content encodes"
            );
        }
        let bg1 = set.canvases[0].as_ref().unwrap();
        assert!((0..i64::from(H)).all(|y| row_offsets(bg1, y) == [0].into()));
    }

    #[test]
    fn without_line_registers_the_per_frame_scroll_is_used_as_before() {
        let mut acc = LayerCanvasAccumulator::new();
        let mut b = line_bundle([[0, 0]; H as usize]);
        b.lines.clear();
        acc.observe_frame(&b, SceneId(1), Some(&scrolls(0, 0)));
        assert!(acc.frames().unwrap().row_dx.is_empty());
        assert_eq!(acc.frames().unwrap().origin_at(1, 3).unwrap().0, 0);
    }

    fn sub_px(layer: PixelLayer, index: u8) -> rf_core_api::SubPixel {
        rf_core_api::SubPixel {
            palette_index: index,
            layer,
            op: rf_core_api::ColorMathOp::AddHalf,
            fixed: false,
            main_black: false,
        }
    }

    #[test]
    fn a_sub_screen_layer_fills_its_own_canvas_where_the_main_screen_showed_another() {
        let mut acc = LayerCanvasAccumulator::new();
        // The whole picture is BG1 (value = column); BG2 is on the sub
        // screen only. Row 1's sub line: BG2 (palette 77) on columns 10..20,
        // BG1 (palette 55) on column 30, nothing (backdrop) elsewhere.
        let mut b = bundle([0, 0], |_, _, p| {
            *p = px(PixelLayer::Background(0), 5);
        });
        for (y, c, v) in [(1usize, 30usize, 30u8)] {
            b.video[y * usize::from(W) + c].palette_index = v;
        }
        b.lines = vec![
            Some(LineRegisters {
                hofs: [0, 100, 0, 0],
                ..LineRegisters::default()
            });
            usize::from(H)
        ];
        let line: Vec<_> = (0..usize::from(W))
            .map(|c| match c {
                10..=19 => sub_px(PixelLayer::Background(1), 77),
                30 => sub_px(PixelLayer::Background(0), 55),
                _ => sub_px(PixelLayer::Backdrop, 0),
            })
            .collect();
        b.sub = vec![
            None,
            Some(rf_core_api::SubLine {
                pixels: line,
                fixed_color: 0,
            }),
            None,
            None,
        ];
        acc.observe_frame(&b, SceneId(1), Some(&scrolls(0, 100)));
        let set = acc.current_canvases();
        let bg2 = set.canvases[1].as_ref().expect("BG2 got cells");
        for c in 0..usize::from(W) {
            let got = bg2.get(100 + c as i64, 1).map(|p| p.palette_index);
            assert_eq!(got, (10..20).contains(&c).then_some(77), "column {c}");
        }
        assert_eq!(bg2.get(110, 0), None, "row 0 had no sub-screen line");
        // The main winner is written first and never overwritten by the
        // sub screen's pixel of the SAME layer, which is the same dot.
        let bg1 = set.canvases[0].as_ref().unwrap();
        assert_eq!(bg1.get(30, 1).map(|p| p.palette_index), Some(30));
        assert!(
            (0..i64::from(W)).all(|x| bg1.get(x, 1).is_none_or(|p| p.palette_index != 77)),
            "BG1 holds no BG2 pixel"
        );
    }

    fn word(r: u16, g: u16, b: u16) -> u16 {
        r | (g << 5) | (b << 10)
    }

    fn compose_fixture() -> (LayerCanvases, LayerFrames, rf_renderer::LinePalette) {
        let mut words = [0u16; 256];
        words[9] = word(20, 0, 0); // a hill colour
        words[3] = word(1, 2, 3); // a ground colour
        let layers = LayerCanvases {
            canvases: [
                Some(canvas_with(&[(7, 0, 3)])),
                Some(canvas_with(&[(5, 0, 9)])),
                None,
                None,
            ],
            ..Default::default()
        };
        let frames = LayerFrames {
            scene: 1,
            origins: [Some((0, 0)), Some((0, 0)), None, None],
            row_dx: Vec::new(),
        };
        (
            layers,
            frames,
            rf_renderer::LinePalette::from_words(&words, 15),
        )
    }

    fn regs() -> LineRegisters {
        LineRegisters {
            tm: 0x01,
            ts: 0x02,
            math_enable: 0x20,
            half: true,
            use_subscreen: true,
            fixed_color: word(4, 4, 4),
            backdrop_color: word(10, 10, 10),
            brightness: 15,
            ..LineRegisters::default()
        }
    }

    fn rgb(w: u16) -> [u8; 3] {
        rf_renderer::bgr555_to_rgb(w, 15)
    }

    #[test]
    fn the_margin_shows_a_sub_screen_layer_behind_the_backdrop_through_colour_math() {
        let (layers, frames, pal) = compose_fixture();
        let at = |dx: i64, r: &LineRegisters| margin_color(&layers, &frames, 0, dx, None, r, &pal);
        // Nothing on the main screen at x = 5, BG2's hill on the sub screen:
        // (backdrop + hill) / 2, per channel (fullsnes SNES PPU Color-Math).
        assert_eq!(at(5, &regs()), rgb(word(15, 5, 5)));
        // Hill unknown at x = 6: the sub source is the fixed colour, and the
        // half is NOT applied to a transparent sub screen: backdrop + fixed.
        assert_eq!(at(6, &regs()), rgb(word(14, 14, 14)));
        // The sub screen not selected ($2130 bit 1 clear): fixed colour, and
        // now the half applies.
        let fixed_only = LineRegisters {
            use_subscreen: false,
            ..regs()
        };
        assert_eq!(at(5, &fixed_only), rgb(word(7, 7, 7)));
        // Subtract: (backdrop - hill), clamped at 0, halved.
        let sub = LineRegisters {
            subtract: true,
            ..regs()
        };
        assert_eq!(at(5, &sub), rgb(word(0, 5, 5)));
        // Math not enabled for the backdrop: the backdrop alone.
        let off = LineRegisters {
            math_enable: 0,
            ..regs()
        };
        assert_eq!(at(5, &off), rgb(word(10, 10, 10)));
        // $2130 bits 4-5 = 3: math never.
        let never = LineRegisters {
            prevent_mode: 3,
            ..regs()
        };
        assert_eq!(at(5, &never), rgb(word(10, 10, 10)));
        // Force main black: the hill alone (0 + 20) / 1, no half on a black
        // main screen.
        let black = LineRegisters {
            clip_mode: 3,
            ..regs()
        };
        assert_eq!(at(5, &black), rgb(word(20, 0, 0)));
        // Brightness scales the result last.
        let dim = LineRegisters {
            brightness: 7,
            ..regs()
        };
        assert_eq!(at(5, &dim), rf_renderer::bgr555_to_rgb(word(15, 5, 5), 7));
    }

    #[test]
    fn math_applies_to_the_main_layer_only_when_its_own_enable_bit_is_set() {
        let (layers, frames, pal) = compose_fixture();
        // BG1 known at x = 7 on the main screen.
        let at = |r: &LineRegisters| margin_color(&layers, &frames, 0, 7, None, r, &pal);
        // Enable bit for the backdrop only: BG1 is shown as it is.
        assert_eq!(at(&regs()), rgb(word(1, 2, 3)));
        // Enable bit 0 (BG1): the ground blends with the fixed colour
        // ((1+4)/2 per channel, BG2 unknown at x = 7 so a transparent sub).
        let on = LineRegisters {
            math_enable: 0x01,
            half: false,
            ..regs()
        };
        assert_eq!(at(&on), rgb(word(5, 6, 7)));
        // A layer only on the sub screen is never the main source.
        let ts_only = LineRegisters {
            tm: 0x00,
            ts: 0x01,
            ..regs()
        };
        assert_eq!(
            at(&ts_only),
            rgb(word(5, 6, 6)),
            "backdrop + BG1 on the sub screen, halved"
        );
    }

    #[test]
    fn the_margin_reads_each_rows_own_world_x() {
        let (mut layers, mut frames, pal) = compose_fixture();
        layers.canvases[1] = Some(canvas_with(&[(5, 0, 9), (5, 1, 9)]));
        // Row 1 of BG2 was drawn 8 px further along than row 0: its hill
        // (world x 5) is 8 columns further left of the origin.
        frames.row_dx = vec![[0; LAYERS], [0, 8, 0, 0]];
        let r = regs();
        let hill = rgb(word(15, 5, 5));
        assert_eq!(margin_color(&layers, &frames, 0, 5, None, &r, &pal), hill);
        assert_ne!(margin_color(&layers, &frames, 1, 5, None, &r, &pal), hill);
        assert_eq!(margin_color(&layers, &frames, 1, -3, None, &r, &pal), hill);
    }
}
