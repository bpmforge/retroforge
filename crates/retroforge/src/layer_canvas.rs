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
//! * Margin order, front to back: BG1 > BG2 > BG3 > BG4. The PPU's real
//!   order depends on the BG mode and per-tile priority bits; a fixed order
//!   is right for the common two-layer scrollers (ground over hills) and is
//!   a documented simplification.
//! * A margin cell no layer knows shows the row's backdrop colour (palette
//!   index 0 with the row palette), bsnes-hd's "auto" fill, not black.
//!
//! * **Which scroll a frame is stitched at (W32-07).** The scroll registers
//!   read after a frame are the ones that draw the NEXT frame (the game's NMI
//!   writes them in vblank), so callers feed `observe_frame` the values read
//!   after the PREVIOUS frame. Measured on Super Mario World: stitched at its
//!   own read, a frame lands one frame of scroll too far.
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
}

impl LayerCanvasAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one frame. `scrolls[n]` is `Some` for each layer the widescreen
    /// policy widens this frame; `None` for narrow (HUD) layers, which are
    /// not stitched. `scrolls == None` (NES, widescreen off) records no
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
        let mut origins = [None; LAYERS];
        let mut row_buf: Vec<Option<PpuPixel>> = Vec::with_capacity(width);
        for (n, scroll) in scrolls.iter().enumerate() {
            let Some(scroll) = scroll else { continue };
            let (ax, ay) = &mut self.axes[n];
            let wx = ax.advance(scroll.hofs, i64::from(scroll.tilemap_width.max(1)));
            let wy = ay.advance(scroll.vofs, i64::from(scroll.tilemap_height.max(1)));
            let wanted = PixelLayer::Background(n as u8);
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
                }
            }
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
/// top row: the front-most layer (BG1 first) with a known cell at that
/// layer's own world position, else `None`. `origins` are this frame's
/// [`LayerFrames::origins`].
#[must_use]
pub fn front_layer_pixel(
    layers: &LayerCanvases,
    origins: &[Option<(i64, i64)>; LAYERS],
    dx: i64,
    dy: i64,
) -> Option<PpuPixel> {
    layers
        .canvases
        .iter()
        .zip(origins)
        .find_map(|(canvas, origin)| {
            let (canvas, (ox, oy)) = (canvas.as_ref()?, (*origin)?);
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
    front_layer_pixel(layers, origins, dx, dy).map_or(0, |p| p.palette_index)
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
        };
        let origins = [Some((0, 0)), None, None, None];
        assert_eq!(front_layer_pixel(&layers, &origins, 40, 3), None);
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
}
