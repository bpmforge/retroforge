//! Placement: where each picture row of a frame lands in a layer's world, and
//! the ONE function that writes it there (ticket W33-03).
//!
//! Both consoles reduce to the same description: for every picture row of a
//! frame, the world position its column 0 sits at (a [`RowPos`], or `None` for
//! a row that must not be written). Where that position comes from is the only
//! difference:
//!
//! * **NES, bands.** The core's own `ScrollWrite`/`Scanline` events become
//!   bands ([`crate::scroll_tracker::ScrollTracker`]); [`band_rows`] turns each
//!   non-HUD band into per-row positions (nesdev "PPU scrolling": a band's x is
//!   constant, its y advances one world row per scanline).
//! * **SNES, registers.** Each background layer's per-line `BGnHOFS`/`BGnVOFS`
//!   (fullsnes "2105h-2114h - BGnHOFS/BGnVOFS") give every layer its OWN row
//!   positions, so a parallax layer lands at the world x its rows were really
//!   drawn at.
//!
//! [`place_rows`] then blits the rows ([`Pick`] says which pixels of a row are
//! worth remembering: the whole level picture on the NES, one layer's own
//! pixels on the SNES). Nothing else writes a cell of a layer canvas.
use rf_core_api::{FrameBundle, PixelLayer, PpuPixel, SubLine};

use super::merge::{rect_union, Rect};
use super::palette_table;
use super::LAYERS;
use crate::scroll_tracker::{BandWorldPos, ScanlineBand, WorldAxis};
use crate::stitcher::{stitchable_pixel, Canvas};

/// One widened layer's scroll for the frame: registers already wrapped to
/// the tilemap, plus the tilemap size they wrap at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerScroll {
    pub hofs: u16,
    pub vofs: u16,
    pub tilemap_width: u16,
    pub tilemap_height: u16,
}

/// What the scroll registers said at one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameScrolls {
    /// The first widened layer's `(hofs, vofs)`, wrapped to its tilemap: the
    /// one scroll the SCENE tracker is fed (a SNES core's own `ScrollWrite`s
    /// are not the level layer's; ticket W30-03). Not used for placement.
    pub scene: Option<(u16, u16)>,
    /// Every stitched layer's scroll ([`super::CanvasModel::observe_frame`]).
    pub layers: Option<[Option<LayerScroll>; LAYERS]>,
}

/// Ticket W32-08: the scroll a frame was DRAWN with.
///
/// The registers read after a frame draw the NEXT one (the game's NMI writes
/// them in vblank, after the visible lines; measured on Super Mario World in
/// W32-07), so frame `n` is stitched at the read taken after frame `n - 1`.
/// One helper for every stitcher.
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

/// Ticket W33-01: the per-layer scroll [`super::CanvasModel::observe_frame`]
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

/// Where each widened layer's 256-dot picture sits in its own world this
/// frame: bundle column `c` of row `sy` lands on world
/// `(origin_x - pad + c, origin_y + sy)` (`pad` = `(width - 256) / 2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerFrames {
    pub scene: u64,
    /// Where row 0 of the picture sits on each layer's canvas.
    pub origins: [Option<(i64, i64)>; LAYERS],
    /// Per picture row, how far that row's world x differs from
    /// `origins[n].0` for layer `n` (HDMA scroll bands, W33-01). Empty when
    /// no row carried line registers (and always on the NES): every row sits
    /// at the origin.
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

    /// The lowest-numbered layer's origin: layer 0 on the NES, and on the
    /// SNES just a fixed reference point the margin mapping is measured from
    /// (every layer is looked up at its own origin; this one only anchors the
    /// picture's left edge).
    #[must_use]
    pub fn reference_origin(&self) -> Option<(i64, i64)> {
        self.origins.iter().flatten().next().copied()
    }
}

/// One layer's world axes: the per-frame pair (registers read once per
/// frame) and, once line registers have been seen, one x axis per picture
/// row (a band's rows keep their own world x across frames, which a single
/// axis fed with varying rows would read as camera jumps).
#[derive(Debug, Clone, Default)]
pub(crate) struct LayerAxes {
    pub(crate) frame: (WorldAxis, WorldAxis),
    pub(crate) rows: Vec<WorldAxis>,
}

/// Where column 0 of one picture row sits in a layer's world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowPos {
    pub x: i64,
    pub y: i64,
}

/// The NES's per-row positions from a frame's classified bands: every row of
/// a non-HUD band at the band's world x, its world y advancing one row per
/// scanline from `world_y_at_start`. HUD bands are fixed UI, not level
/// geometry, so their rows get nothing (painting them into world space would
/// put the status bar into the level at whatever position happened to be under
/// it).
#[must_use]
pub fn band_rows(bands: &[(ScanlineBand, BandWorldPos)], height: usize) -> Vec<Option<RowPos>> {
    let mut rows = vec![None; height];
    for (band, pos) in bands.iter().filter(|(b, _)| !b.is_hud) {
        // Bounded `for` over the band's rows (law 8).
        for sy in band.start..band.end {
            if let Some(slot) = rows.get_mut(usize::from(sy)) {
                *slot = Some(RowPos {
                    x: pos.world_x,
                    y: pos.world_y_at_start + i64::from(sy - band.start),
                });
            }
        }
    }
    rows
}

/// The width of an unwidened NES/SNES picture, and how many of its leftmost
/// dots the NES can clip (W32-09).
const CLIP_COLUMNS_WIDTH: usize = 256;
const CLIP_COLUMNS: usize = 8;

/// Which pixels of a row [`place_rows`] remembers.
pub enum Pick<'a> {
    /// The level picture (NES, one canvas for the whole screen): every
    /// background and backdrop pixel; sprites are holes. A row is visited
    /// even where only sprites were drawn, so the canvas grows to cover it.
    Level,
    /// One SNES layer's own pixels: `Background(n)` from the main screen, and
    /// the sub screen's pixel of that layer where the main screen showed
    /// something else (W33-01). A row the layer did not draw at all is not
    /// written and does not grow the canvas. `ids[row]` is the id of the
    /// palette the row was drawn with ([`palette_table`]).
    Layer {
        wanted: PixelLayer,
        sub: &'a [Option<SubLine>],
        ids: &'a [Option<u8>],
    },
}

/// The one placement function: write the rows of `video` (`width` dots per
/// row) that have a position into `canvas`, picking pixels with `pick`.
/// `dirty` is grown by the box of every row written. Returns the first and
/// last picture row written (`None` when nothing was).
pub fn place_rows(
    canvas: &mut Canvas,
    video: &[PpuPixel],
    width: usize,
    rows: &[Option<RowPos>],
    pick: &Pick<'_>,
    dirty: &mut Option<Rect>,
) -> Option<(usize, usize)> {
    if width == 0 {
        return None;
    }
    let mut buf: Vec<Option<PpuPixel>> = Vec::with_capacity(width);
    let mut written: Option<(usize, usize)> = None;
    // Bounded `for` over the frame's rows (law 8).
    for (sy, row) in video.chunks_exact(width).enumerate() {
        let Some(Some(pos)) = rows.get(sy) else {
            continue;
        };
        buf.clear();
        match pick {
            Pick::Level => {
                buf.extend(row.iter().map(stitchable_pixel));
                // Ticket W32-09: the NES can hide its leftmost 8 dots
                // (PPUMASK bit 1, nesdev "PPU registers"), and a clipped dot
                // is drawn as the backdrop. A column that scrolls past the
                // left edge would be painted over with sky by the frame that
                // has it clipped, so a backdrop dot there only fills a cell
                // nothing is known about.
                if width == CLIP_COLUMNS_WIDTH {
                    for (c, cell) in buf.iter_mut().take(CLIP_COLUMNS).enumerate() {
                        let is_backdrop = cell.is_some_and(|p| p.layer == PixelLayer::Backdrop);
                        if is_backdrop && canvas.get(pos.x + c as i64, pos.y).is_some() {
                            *cell = None;
                        }
                    }
                }
            }
            Pick::Layer { wanted, sub, ids } => {
                let sub = sub.get(sy).and_then(Option::as_ref);
                let id = ids.get(sy).copied().flatten();
                buf.extend(row.iter().enumerate().map(|(c, p)| {
                    if p.layer == *wanted {
                        return Some(palette_table::tag(*p, id));
                    }
                    // The sub screen's pixel of THIS layer where the main
                    // screen showed something else (W33-01).
                    let sp = sub?.pixels.get(c).filter(|sp| sp.layer == *wanted)?;
                    Some(PpuPixel {
                        palette_index: sp.palette_index,
                        layer: *wanted,
                        sprite_id: id,
                        priority: 0,
                    })
                }));
                if buf.iter().all(Option::is_none) {
                    continue;
                }
            }
        }
        canvas.blit_row(pos.x, pos.y, &buf);
        let r = (pos.x, pos.y, pos.x + width as i64, pos.y + 1);
        *dirty = Some(dirty.map_or(r, |d| rect_union(d, r)));
        written = Some(written.map_or((sy, sy), |(lo, _)| (lo, sy)));
    }
    written
}
