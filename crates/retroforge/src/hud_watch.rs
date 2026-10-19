//! Which part of the picture is HUD (ticket W34-08, D4 and D7;
//! `docs/design/HD2D_PLAN.md` section 4).
//!
//! The Diorama draws the HUD flat on top in screen space, so it must know
//! which pixels are HUD. The canvas model's own verdict
//! (`CanvasModel::hud_layers`) only exists while the scene is being
//! stitched for widescreen, so this is a second, cheaper judge that reads the
//! tile grids the learner already gets. It never touches a console: the same
//! code serves the NES and the SNES.
//!
//! ## Two kinds of HUD
//!
//! - **A layer of its own** (SNES: Super Mario World's status bar on BG3,
//!   A Link to the Past's hearts and items on BG3). The layer is not the
//!   playfield, its visible pixels sit in one band at the top or bottom edge
//!   of the picture, its scroll stays put over the watch window, and either
//!   the playfield moved meanwhile or the two layers are not scrolled
//!   together (a room that never scrolls cannot show the first).
//! - **A band of the playfield's layer** (NES: Super Mario Bros. 3's status
//!   bar, a scroll split under the level). The playfield's widest band stops
//!   short of one edge and the band that reaches that edge keeps its scroll.
//!   The little bands between (a split writes the scroll over several lines)
//!   are part of it.
//!
//! The judge looks at two windows of [`WINDOW_FRAMES`] frames, the current
//! and the one before, so a scene change forgets within about twice that.
//! It is a hint like `TileGrid::hud`, never a verdict: a wrong call only
//! flattens a band of the picture (it is drawn on top instead of in the
//! scene), and nothing is learned from it.

use rf_enhance::heights::TileGrid;

/// The id a sprite pixel has in a frame's layer map.
pub const LAYER_SPRITE: u8 = 0xFF;
/// The id a pixel nothing drew has (the backdrop colour).
pub const LAYER_BACKDROP: u8 = 0xFE;
/// Frames in one watch window.
pub const WINDOW_FRAMES: u32 = 240;
/// A scroll that spans fewer pixels than this has not moved.
pub const MOVED_PX: i64 = 8;
/// How many rows a HUD layer's pixels may span, as a fraction of the picture
/// (numerator over denominator).
const MAX_BAND: (u32, u32) = (3, 4);
/// A band this close to the top or bottom of the picture touches that edge.
const EDGE_SLACK_PX: u32 = 16;

/// The HUD of one frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HudInfo {
    /// Background layers that are HUD (index = layer).
    pub layers: [bool; 4],
    /// Picture rows `[start, end)` that are HUD whatever drew them.
    pub rows: Vec<(u16, u16)>,
}

impl HudInfo {
    /// Whether any pixel is HUD.
    #[must_use]
    pub fn any(&self) -> bool {
        self.layers.iter().any(|l| *l) || !self.rows.is_empty()
    }

    /// Whether picture row `y` is in a HUD band.
    #[must_use]
    pub fn row(&self, y: u16) -> bool {
        self.rows.iter().any(|(a, b)| y >= *a && y < *b)
    }
}

/// What the tile-grid capture records about a layer's scroll over a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    min: (i64, i64),
    max: (i64, i64),
}

impl Span {
    fn at(p: (i64, i64)) -> Self {
        Self { min: p, max: p }
    }

    fn join(self, o: Self) -> Self {
        Self {
            min: (self.min.0.min(o.min.0), self.min.1.min(o.min.1)),
            max: (self.max.0.max(o.max.0), self.max.1.max(o.max.1)),
        }
    }

    fn moved(&self) -> bool {
        self.max.0 - self.min.0 >= MOVED_PX || self.max.1 - self.min.1 >= MOVED_PX
    }
}

/// One band's identity: its layer and the picture row it starts at.
type Key = (u8, u16);

#[derive(Debug, Clone, Default)]
struct Window {
    frames: u32,
    spans: Vec<(Key, Span)>,
}

impl Window {
    fn note(&mut self, k: Key, p: (i64, i64)) {
        match self.spans.iter_mut().find(|(key, _)| *key == k) {
            Some((_, s)) => *s = s.join(Span::at(p)),
            None => self.spans.push((k, Span::at(p))),
        }
    }
}

/// Where the picture's layer `l` was scrolled to this frame, from its widest
/// band: the world position of picture pixel `(0, band start)`.
fn scroll_of(g: &TileGrid) -> (i64, i64) {
    (
        g.origin_world.0 + i64::from(g.offset.0),
        g.origin_world.1 + i64::from(g.offset.1),
    )
}

fn band_rows(g: &TileGrid) -> u32 {
    u32::from(g.rows_px.1.saturating_sub(g.rows_px.0))
}

/// The rows each background layer's visible pixels span this frame:
/// `ids[i]` is the layer of pixel `i` (`LAYER_SPRITE` / `LAYER_BACKDROP` for
/// the others), `width` pixels per row.
#[must_use]
pub fn visible_rows(ids: &[u8], width: usize) -> [Option<(u16, u16)>; 4] {
    let mut out: [Option<(u16, u16)>; 4] = [None; 4];
    if width == 0 {
        return out;
    }
    for (y, row) in ids.chunks_exact(width).enumerate() {
        let y = u16::try_from(y).unwrap_or(u16::MAX);
        let mut seen = [false; 4];
        for id in row {
            if let Some(s) = seen.get_mut(usize::from(*id)) {
                *s = true;
            }
        }
        for (l, s) in seen.iter().enumerate() {
            if *s {
                out[l] = Some(out[l].map_or((y, y), |(a, _)| (a, y)));
            }
        }
    }
    out
}

/// The judge. One per session.
#[derive(Debug, Clone, Default)]
pub struct HudWatch {
    cur: Window,
    prev: Window,
}

impl HudWatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one frame in and judge it. `fg` is the playfield grid's index in
    /// `grids`, `visible` the rows each layer's pixels span
    /// ([`visible_rows`]), `height` the rows of the picture.
    pub fn observe(
        &mut self,
        grids: &[TileGrid],
        fg: Option<usize>,
        visible: &[Option<(u16, u16)>; 4],
        height: u32,
    ) -> HudInfo {
        if self.cur.frames >= WINDOW_FRAMES {
            self.prev = std::mem::take(&mut self.cur);
        }
        self.cur.frames += 1;
        for g in grids {
            self.cur.note((g.layer, g.rows_px.0), scroll_of(g));
        }
        let mut info = HudInfo::default();
        let Some(fg) = fg.and_then(|i| grids.get(i)) else {
            return info;
        };
        let fg_layer = fg.layer;
        let keyed = self.keyed();
        let fg_moved = layer_moved(&keyed, fg_layer) == Some(true);
        // A layer of its own.
        for l in (0..4u8).filter(|l| *l != fg_layer) {
            let Some((a, b)) = visible[usize::from(l)] else {
                continue;
            };
            let Some(moved) = layer_moved(&keyed, l) else {
                continue;
            };
            let rows = u32::from(b) - u32::from(a) + 1;
            let at_edge = u32::from(a) < EDGE_SLACK_PX || u32::from(b) + EDGE_SLACK_PX >= height;
            let narrow = rows * MAX_BAND.1 < height * MAX_BAND.0;
            let this = grids.iter().find(|g| g.layer == l).map(scroll_of);
            let apart = this.is_some_and(|t| {
                let f = scroll_of(fg);
                (t.0 - f.0).abs() >= MOVED_PX || (t.1 - f.1).abs() >= MOVED_PX
            });
            if at_edge && narrow && !moved && (fg_moved || apart) {
                info.layers[usize::from(l)] = true;
            }
        }
        // A band of the playfield's own layer.
        let tall = u32::from(fg.rows_px.1);
        let h = height.max(tall);
        if fg.rows_px.0 as u32 <= EDGE_SLACK_PX && band_rows(fg) < h * 9 / 10 && band_rows(fg) > 0 {
            // Stops short of the bottom: the bands from its end on, if the
            // one reaching the bottom keeps its scroll.
            if let Some(last) = grids
                .iter()
                .filter(|g| g.layer == fg_layer && g.rows_px.0 >= fg.rows_px.1)
                .max_by_key(|g| g.rows_px.1)
            {
                let still = band_moved(&keyed, fg_layer, last.rows_px.0) == Some(false);
                if still && u32::from(last.rows_px.1) + EDGE_SLACK_PX >= h {
                    info.rows.push((fg.rows_px.1, last.rows_px.1));
                }
            }
        } else if u32::from(fg.rows_px.1) + EDGE_SLACK_PX >= h
            && fg.rows_px.0 > 0
            && u32::from(fg.rows_px.0) * 3 < h
        {
            // Starts below the top: the bands above it.
            if let Some(first) = grids
                .iter()
                .filter(|g| g.layer == fg_layer && g.rows_px.1 <= fg.rows_px.0)
                .min_by_key(|g| g.rows_px.0)
            {
                let still = band_moved(&keyed, fg_layer, first.rows_px.0) == Some(false);
                if still && u32::from(first.rows_px.0) <= EDGE_SLACK_PX {
                    info.rows.push((first.rows_px.0, fg.rows_px.0));
                }
            }
        }
        info
    }

    /// Every band's scroll span over both windows.
    fn keyed(&self) -> Vec<(Key, Span)> {
        let mut out: Vec<(Key, Span)> = Vec::new();
        for w in [&self.prev, &self.cur] {
            for (k, span) in &w.spans {
                match out.iter_mut().find(|(key, _)| key == k) {
                    Some((_, s)) => *s = s.join(*span),
                    None => out.push((*k, *span)),
                }
            }
        }
        out
    }
}

/// Whether any band of `layer` scrolled (`None` when it has no band).
fn layer_moved(keyed: &[(Key, Span)], layer: u8) -> Option<bool> {
    let mut seen = false;
    for ((l, _), span) in keyed {
        if *l == layer {
            if span.moved() {
                return Some(true);
            }
            seen = true;
        }
    }
    seen.then_some(false)
}

/// Whether the band of `layer` starting at row `start` scrolled.
fn band_moved(keyed: &[(Key, Span)], layer: u8, start: u16) -> Option<bool> {
    keyed
        .iter()
        .find(|(k, _)| *k == (layer, start))
        .map(|(_, s)| s.moved())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_enhance::heights::TileCell;

    fn grid(layer: u8, rows_px: (u16, u16), world: (i64, i64)) -> TileGrid {
        TileGrid {
            layer,
            cell_px: 8,
            hud: false,
            exact: true,
            rows_px,
            origin_world: world,
            offset: (0, 0),
            cols: 4,
            rows: 4,
            cells: vec![TileCell::default(); 16],
        }
    }

    fn ids(rows: &[(u8, u16)], width: usize) -> Vec<u8> {
        // (layer, how many rows), top to bottom.
        rows.iter()
            .flat_map(|(l, n)| std::iter::repeat_n(*l, width * usize::from(*n)))
            .collect()
    }

    #[test]
    fn visible_rows_span_each_layers_first_to_last_row() {
        let v = visible_rows(&ids(&[(2, 3), (0, 4), (2, 1)], 4), 4);
        assert_eq!(v[2], Some((0, 7)));
        assert_eq!(v[0], Some((3, 6)));
        assert_eq!(v[1], None);
    }

    /// SMW: BG3 draws a status bar at the top and never scrolls; BG1 does.
    #[test]
    fn a_layer_that_draws_only_a_top_band_and_never_scrolls_is_hud() {
        let mut w = HudWatch::new();
        let vis = visible_rows(&ids(&[(2, 36), (0, 188)], 8), 8);
        let mut info = HudInfo::default();
        for x in 0..40 {
            let gs = [grid(0, (0, 224), (x * 3, 0)), grid(2, (0, 224), (0, 0))];
            info = w.observe(&gs, Some(0), &vis, 224);
        }
        assert_eq!(info.layers, [false, false, true, false]);
        assert!(info.rows.is_empty());
    }

    /// The same layer while nothing has moved is judged by being apart from
    /// the playfield (a room that never scrolls: ALttP).
    #[test]
    fn in_a_room_that_never_scrolls_a_band_layer_scrolled_apart_is_hud() {
        let mut w = HudWatch::new();
        let vis = visible_rows(&ids(&[(2, 28), (1, 196)], 8), 8);
        let gs = [grid(1, (0, 224), (256, 272)), grid(2, (0, 224), (0, 0))];
        let info = w.observe(&gs, Some(0), &vis, 224);
        assert_eq!(info.layers, [false, false, true, false]);
    }

    /// Hills (BG2) fill most of the picture and move with the camera: not HUD.
    #[test]
    fn a_scrolling_background_is_not_hud() {
        let mut w = HudWatch::new();
        let vis = visible_rows(&ids(&[(1, 180), (0, 44)], 8), 8);
        let mut info = HudInfo::default();
        for x in 0..40 {
            let gs = [grid(0, (0, 224), (x * 4, 0)), grid(1, (0, 224), (x * 2, 0))];
            info = w.observe(&gs, Some(0), &vis, 224);
        }
        assert!(!info.any(), "{info:?}");
    }

    /// A layer in the middle of the picture is not a status bar.
    #[test]
    fn a_band_that_does_not_touch_an_edge_is_not_hud() {
        let mut w = HudWatch::new();
        let vis = visible_rows(&ids(&[(0, 60), (2, 40), (0, 124)], 8), 8);
        let mut info = HudInfo::default();
        for x in 0..40 {
            let gs = [grid(0, (0, 224), (x * 3, 0)), grid(2, (0, 224), (0, 0))];
            info = w.observe(&gs, Some(0), &vis, 224);
        }
        assert!(!info.any(), "{info:?}");
    }

    /// SMB3: the playfield's band stops at row 193; split bands below it,
    /// the last one fixed.
    #[test]
    fn a_fixed_scroll_split_under_the_playfield_is_a_hud_band() {
        let mut w = HudWatch::new();
        let vis = [Some((0, 239)), None, None, None];
        let mut info = HudInfo::default();
        for x in 0..40 {
            let gs = [
                grid(0, (0, 193), (x * 2, 224)),
                grid(0, (193, 201), (0, 0)),
                grid(0, (194, 202), (x % 5, 432)),
                grid(0, (202, 240), (0, 432)),
            ];
            info = w.observe(&gs, Some(0), &vis, 240);
        }
        assert_eq!(info.rows, vec![(193, 240)]);
        assert!(info.row(200) && !info.row(100));
        assert_eq!(info.layers, [false; 4]);
    }

    /// A status bar on top: the playfield starts lower.
    #[test]
    fn a_fixed_band_above_the_playfield_is_a_hud_band() {
        let mut w = HudWatch::new();
        let vis = [Some((0, 239)), None, None, None];
        let gs = [grid(0, (0, 32), (0, 0)), grid(0, (32, 240), (50, 0))];
        let info = w.observe(&gs, Some(1), &vis, 240);
        assert_eq!(info.rows, vec![(0, 32)]);
    }

    /// One full-height band: nothing to call.
    #[test]
    fn a_single_full_height_band_has_no_hud_band() {
        let mut w = HudWatch::new();
        let vis = [Some((0, 239)), None, None, None];
        let info = w.observe(&[grid(0, (0, 240), (5, 0))], Some(0), &vis, 240);
        assert!(!info.any());
    }

    #[test]
    fn a_scene_change_is_forgotten_after_two_windows() {
        let mut w = HudWatch::new();
        let vis = visible_rows(&ids(&[(2, 36), (0, 188)], 8), 8);
        for x in 0..40 {
            let gs = [grid(0, (0, 224), (x * 3, 0)), grid(2, (0, 224), (0, 0))];
            w.observe(&gs, Some(0), &vis, 224);
        }
        // BG3 now scrolls along with BG1 for two windows.
        let mut info = HudInfo::default();
        for x in 0..(2 * WINDOW_FRAMES as i64 + 2) {
            let gs = [grid(0, (0, 224), (x * 3, 0)), grid(2, (0, 224), (x * 3, 0))];
            info = w.observe(&gs, Some(0), &vis, 224);
        }
        assert!(!info.any(), "{info:?}");
    }
}
