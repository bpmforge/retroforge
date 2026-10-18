//! The height learner (ticket W34-03; `docs/design/HD2D_PLAN.md` section 2).
//!
//! Heights come from PLAY, never from tile art: what the player stood on,
//! walked into, walked over and was hidden behind, all per [`TileKey`], so a
//! tile learned once is known everywhere. Clean-room: the inputs are the
//! console-free things the shell already hands this crate (tile grids,
//! sprites, the pad, the tracked player and orientation).
//!
//! ## Signals (the plan's numbering, section 2.1)
//!
//! | | rule |
//! |---|---|
//! | S1 | side view: a landing (vertical speed to 0 after >= 2 falling frames) or a frame of standing (>= 8 still frames) votes the cells just under the feet `floor_top`. Top-down: the feet's cells while moving vote `overlapped` |
//! | S2 | a direction held >= [`BLOCK_FRAMES`] frames with zero world movement on that axis votes the cell beyond the hitbox `blocked[dir]` (capped per episode, no other entity over it) |
//! | S3 | the player drawn behind a cell: the shell's hidden rectangle, else a behind-BG sprite over a cell with the priority bit |
//! | S4 | a key never walked over, next to a tile already solid (below it in side view, any side top-down), in at least an eighth of its sightings is solid mass |
//! | S5 | a layer whose scroll travel is under 90 % of the fastest is a [`Class::Backdrop`] plane (set back, never extruded) |
//! | S6 | side view: constant `dy` with `dx` on a surface (>= 4 frames, `|dy| <= |dx|`) votes the cells under the feet `slope` |
//! | S7 | a key inside 3x3 runs of itself: a prior that lowers S4's bar. It classifies nothing alone, and it cannot tell sky from ground without pixels |
//!
//! HUD grids are never read (D7). Partial cells are skipped. The player track
//! can break (W34-02: ALttP merged with the bed at f177): dynamics reset on
//! `None`, on a new track id, on a changed foreground band and on a jump
//! larger than a frame of play could make, and nothing is carried across.
//!
//! ## One collision layer
//!
//! S1, S2, S4 and S6 teach only the foreground grid (the widest band of the
//! fastest layers, the lowest layer number on a tie): at the player's feet
//! EVERY layer has a cell, most of them transparent, and a landing must not
//! make an empty tile of the status bar or the hills a floor. S3 (hidden
//! behind) reads every non-backdrop layer: what hides him may be a
//! foreground layer above his own.
//!
//! ## Classes and the ladder
//!
//! [`classify`] turns one key's [`Evidence`] into a class; the ladder is the
//! one in `canvas::registration` (up one per `GOOD_FRAMES_PER_STEP` agreeing
//! frames, down one per `BAD_FRAMES_PER_STEP` contradicting ones, 0..=3); a
//! tile below [`Learned::DRAW_LEVEL`] is drawn flat. Unknown is flat.
//!
//! ## Cost and bounds
//!
//! History is a fixed handful of counters ([`Dynamics`]); the table is
//! bounded by [`MAX_KEYS`]. No loop here is a hand-rolled walk.

use super::player::{Dirs, Entity, Orientation, Player, SpriteBox};
use super::table::{Class, Entry, Evidence, Learned, TileHeightTable, MAX_KEYS};
use super::tile_grid::{TileCell, TileGrid, TileKey};
use crate::canvas::{BAD_FRAMES_PER_STEP, GOOD_FRAMES_PER_STEP, MAX_LEVEL};

/// Frames a direction is held with no movement before a tile is blocking.
/// The plan says 3; 12 keeps a game that steps every few frames (RF-Scroller
/// moves 8 px every 8th frame) from reading each pause as a wall.
pub const BLOCK_FRAMES: u16 = 12;
/// Blocked votes one stuck episode may cast.
pub const EPISODE_VOTES: u8 = 10;
/// Votes a class needs before it is a candidate.
pub const MIN_VOTES: u32 = 3;
/// Still frames before standing counts as support.
pub const STAND_FRAMES: u16 = 8;
/// A jump bigger than this in one frame is a cut, not play.
const MAX_STEP_PX: i64 = 64;
/// Layer travel before a backdrop call is made.
const MIN_TRAVEL: u64 = 96;
/// Extrusion of a wall or platform, in eighths of a tile.
const WALL_EIGHTHS: u8 = 8;
/// Extrusion of a tile that hides the player.
const TALL_EIGHTHS: u8 = 16;

/// Which signals are on (the mutation checks switch them off one by one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signals(pub u8);

impl Signals {
    pub const S1_SUPPORT: u8 = 1;
    pub const S2_BLOCKING: u8 = 2;
    pub const S3_HIDDEN: u8 = 4;
    pub const S4_MASS: u8 = 8;
    pub const S5_PARALLAX: u8 = 16;
    pub const S6_SLOPE: u8 = 32;
    pub const S7_ART: u8 = 64;
    pub const ALL: Signals = Signals(127);

    #[must_use]
    pub const fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }

    #[must_use]
    pub const fn without(self, bit: u8) -> Self {
        Signals(self.0 & !bit)
    }
}

/// One frame's observations.
#[derive(Debug, Clone, Copy)]
pub struct FrameObs<'a> {
    /// The frame's tile grids (every layer and band).
    pub grids: &'a [TileGrid],
    /// Every sprite, for the behind-BG flag.
    pub sprites: &'a [SpriteBox],
    /// The clustered entities (`player::cluster`).
    pub entities: &'a [Entity],
    /// The tracked player, when there is one this frame.
    pub player: Option<&'a Player>,
    pub pad: Dirs,
    pub orientation: Orientation,
    /// Picture rectangle `(x0, y0, x1, y1)` where the shell found the
    /// player's sprite pixels covered by the background (S3, authoritative).
    pub hidden: Option<(i32, i32, i32, i32)>,
}

/// What [`HeightLearner::grid_for`] returns: what to draw per cell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HeightGrid {
    pub cols: u16,
    pub rows: u16,
    pub cell_px: u8,
    pub offset: (u8, u8),
    pub class: Vec<Class>,
    /// Extrusion, eighths of a tile; 0 for flat.
    pub depth: Vec<u8>,
    pub confidence: Vec<u8>,
}

/// Counts for the readout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub keys: usize,
    pub drawn: usize,
    pub solid: usize,
    /// Keys at confidence 0, 1, 2, 3.
    pub by_level: [usize; 4],
}

#[derive(Debug, Clone, Copy, Default)]
struct LayerMotion {
    band: Option<(u16, u16)>,
    last: (i64, i64),
    travel: u64,
}

/// The fixed per-player counters (no history grows).
#[derive(Debug, Clone, Copy, Default)]
struct Dynamics {
    valid: bool,
    track: u32,
    band: (u8, u16, u16),
    x0: i32,
    y1: i32,
    cam: (i64, i64),
    last_wdy: i32,
    fall: u16,
    still_v: u16,
    still_dir: [i8; 2],
    still: [u16; 2],
    episode: [u8; 2],
    slope_run: u16,
}

/// The learner.
#[derive(Debug, Clone)]
pub struct HeightLearner {
    table: TileHeightTable,
    signals: Signals,
    frames: u32,
    motion: [LayerMotion; 4],
    dynamics: Dynamics,
    dirty: Vec<TileKey>,
    hits: Vec<(TileKey, TileCell)>,
}

impl Default for HeightLearner {
    fn default() -> Self {
        Self::new()
    }
}

fn layer_ix(g: &TileGrid) -> usize {
    usize::from(g.layer).min(3)
}

fn span(g: &TileGrid) -> u16 {
    g.rows_px.1.saturating_sub(g.rows_px.0)
}

/// The picture-space cell range a rectangle covers in `g` (empty when the
/// rectangle misses the band).
fn cell_range(g: &TileGrid, x0: i32, y0: i32, x1: i32, y1: i32) -> Option<(u16, u16, u16, u16)> {
    let (top, bottom) = (i32::from(g.rows_px.0), i32::from(g.rows_px.1));
    let (y0, y1) = (y0.max(top), y1.min(bottom));
    let x0 = x0.max(0);
    if x1 <= x0 || y1 <= y0 || g.cols == 0 || g.rows == 0 {
        return None;
    }
    let p = i32::from(g.cell_px.max(1));
    let col = |x: i32| ((x + i32::from(g.offset.0)) / p).clamp(0, i32::from(g.cols) - 1) as u16;
    let row =
        |y: i32| ((y - top + i32::from(g.offset.1)) / p).clamp(0, i32::from(g.rows) - 1) as u16;
    Some((col(x0), row(y0), col(x1 - 1), row(y1 - 1)))
}

/// Turn one key's evidence into a class and an extrusion.
#[must_use]
pub fn classify(e: &Evidence, mass_ok: bool) -> (Class, u8) {
    let blocked: u32 = e.blocked.iter().sum();
    // Direct evidence only: mass is a guess made from neighbours, and a guess
    // must never outvote a body walking through the tile.
    let direct = e.floor_top.max(blocked);
    if e.hides_player >= MIN_VOTES && e.hides_player * 2 >= e.overlapped {
        return (Class::Tall, TALL_EIGHTHS);
    }
    // A slope is walked INTO (the body overlaps it) and stood on mid-tile, so
    // its own votes come before the air test.
    if e.slope_n >= MIN_VOTES && u32::from(e.slope.unsigned_abs()) * 2 >= e.slope_n {
        let mean = (e.slope_eighths / e.slope_n.max(1)).clamp(1, 8) as i8;
        let rise = if e.slope > 0 { mean } else { -mean };
        return (Class::Ramp(rise), WALL_EIGHTHS);
    }
    if e.overlapped >= MIN_VOTES && e.overlapped >= direct.saturating_mul(2) {
        return (Class::Floor, 0);
    }
    if blocked >= MIN_VOTES {
        return (Class::Wall, WALL_EIGHTHS);
    }
    if e.floor_top >= MIN_VOTES {
        return (Class::Platform, WALL_EIGHTHS);
    }
    if e.mass >= MIN_VOTES && e.overlapped <= 2 && mass_ok {
        return (Class::Wall, WALL_EIGHTHS);
    }
    if e.overlapped >= MIN_VOTES {
        return (Class::Floor, 0);
    }
    (Class::Unknown, 0)
}

/// S4's bar: the share of a key's sightings that must be below a solid tile
/// (an eighth: a fill column is solid cells stacked under the surface, so
/// only its top row is next to ground to begin with). A block-fill shape
/// (S7) lowers it to a sixteenth.
fn mass_share_ok(e: &Evidence) -> bool {
    let bar = if e.prior * 2 >= e.seen { 16 } else { 8 };
    e.mass.saturating_mul(bar) >= e.seen
}

/// One ladder step for one key.
fn step(e: &mut Entry, cand: (Class, u8)) {
    let cur = e.learned;
    if cand.0 == cur.class {
        e.contra = 0;
        e.learned.height_eighths = cand.1;
        if cur.class != Class::Unknown {
            e.agree += 1;
            if e.agree >= GOOD_FRAMES_PER_STEP as u16 {
                e.agree = 0;
                e.learned.confidence = (cur.confidence + 1).min(MAX_LEVEL);
            }
        }
    } else if cand.0 != Class::Unknown {
        e.agree = 0;
        if cur.class == Class::Unknown {
            e.learned = Learned {
                class: cand.0,
                height_eighths: cand.1,
                confidence: 0,
            };
            e.contra = 0;
        } else {
            e.contra += 1;
            if e.contra >= BAD_FRAMES_PER_STEP as u16 {
                e.contra = 0;
                e.learned.confidence = cur.confidence.saturating_sub(1);
                if e.learned.confidence == 0 {
                    e.learned = Learned {
                        class: cand.0,
                        height_eighths: cand.1,
                        confidence: 0,
                    };
                }
            }
        }
    }
}

impl HeightLearner {
    #[must_use]
    pub fn new() -> Self {
        Self::with_table(TileHeightTable::new())
    }

    /// Start from a table restored from the cache.
    #[must_use]
    pub fn with_table(table: TileHeightTable) -> Self {
        Self {
            table,
            signals: Signals::ALL,
            frames: 0,
            motion: [LayerMotion::default(); 4],
            dynamics: Dynamics::default(),
            dirty: Vec::new(),
            hits: Vec::new(),
        }
    }

    pub fn set_signals(&mut self, s: Signals) {
        self.signals = s;
    }

    #[must_use]
    pub fn table(&self) -> &TileHeightTable {
        &self.table
    }

    #[must_use]
    pub fn into_table(self) -> TileHeightTable {
        self.table
    }

    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frames
    }

    #[must_use]
    pub fn stats(&self) -> Stats {
        let mut s = Stats {
            keys: self.table.len(),
            ..Stats::default()
        };
        for (_, e) in self.table.iter() {
            s.by_level[usize::from(e.learned.confidence.min(3))] += 1;
            let (c, _) = e.learned.drawn();
            if c != Class::Unknown {
                s.drawn += 1;
            }
            if c.is_solid() {
                s.solid += 1;
            }
        }
        s
    }

    /// Read the table back for one grid: flat for HUD grids and for anything
    /// below [`Learned::DRAW_LEVEL`].
    #[must_use]
    pub fn grid_for(&self, g: &TileGrid) -> HeightGrid {
        let n = g.cells.len();
        let mut out = HeightGrid {
            cols: g.cols,
            rows: g.rows,
            cell_px: g.cell_px,
            offset: g.offset,
            class: vec![Class::Unknown; n],
            depth: vec![0; n],
            confidence: vec![0; n],
        };
        if g.hud {
            return out;
        }
        for (i, c) in g.cells.iter().enumerate() {
            if c.partial() {
                continue;
            }
            let key = TileKey {
                pattern: c.pattern,
                palette: c.palette,
                layer: g.layer,
                cell_px: g.cell_px,
            };
            if let Some(e) = self.table.get(&key) {
                let (class, depth) = e.learned.drawn();
                out.class[i] = class;
                out.depth[i] = if class == Class::Backdrop { 0 } else { depth };
                out.confidence[i] = e.learned.confidence;
            }
        }
        out
    }

    /// Fold one frame in.
    pub fn observe(&mut self, f: &FrameObs<'_>) {
        self.frames = self.frames.wrapping_add(1);
        self.dirty.clear();
        let backdrop = self.update_motion(f.grids);
        self.count_sightings(f.grids, &backdrop);
        if f.orientation != Orientation::Unknown {
            self.watch_player(f, &backdrop);
            if let (true, Some(fg)) = (
                self.signals.has(Signals::S4_MASS),
                Self::foreground(f.grids, &backdrop),
            ) {
                self.vote_mass(f.grids, fg.layer, &backdrop, f.orientation);
            }
        } else {
            self.dynamics.valid = false;
        }
        self.run_ladder(&backdrop);
    }

    /// S5: per layer travel; returns the layers that are backdrop planes.
    fn update_motion(&mut self, grids: &[TileGrid]) -> [bool; 4] {
        let mut best: [Option<&TileGrid>; 4] = [None; 4];
        for g in grids.iter().filter(|g| !g.hud) {
            let l = layer_ix(g);
            if best[l].is_none_or(|b| span(g) > span(b)) {
                best[l] = Some(g);
            }
        }
        for (l, b) in best.iter().enumerate() {
            let m = &mut self.motion[l];
            let Some(g) = b else {
                m.band = None;
                continue;
            };
            let cam = (
                g.origin_world.0 + i64::from(g.offset.0),
                g.origin_world.1 + i64::from(g.offset.1),
            );
            let band = Some(g.rows_px);
            if m.band == band {
                let (dx, dy) = (cam.0 - m.last.0, cam.1 - m.last.1);
                if dx.abs() <= MAX_STEP_PX && dy.abs() <= MAX_STEP_PX {
                    m.travel += dx.unsigned_abs() + dy.unsigned_abs();
                }
            }
            m.band = band;
            m.last = cam;
        }
        let mut out = [false; 4];
        if !self.signals.has(Signals::S5_PARALLAX) {
            return out;
        }
        let fastest = (0..4)
            .filter(|&l| self.motion[l].band.is_some())
            .map(|l| self.motion[l].travel)
            .max()
            .unwrap_or(0);
        if fastest >= MIN_TRAVEL {
            for (l, flag) in out.iter_mut().enumerate() {
                *flag = self.motion[l].band.is_some() && self.motion[l].travel * 10 < fastest * 9;
            }
        }
        out
    }

    fn foreground<'a>(grids: &'a [TileGrid], backdrop: &[bool; 4]) -> Option<&'a TileGrid> {
        // One collision layer: the widest band of the fastest layers; on a tie
        // the lowest layer number (deterministic).
        grids
            .iter()
            .filter(|g| !g.hud && !backdrop[layer_ix(g)])
            .min_by_key(|g| (std::cmp::Reverse(span(g)), g.layer))
    }

    /// `seen`, the backdrop calls, and S7 for every non-HUD cell.
    fn count_sightings(&mut self, grids: &[TileGrid], backdrop: &[bool; 4]) {
        let art = self.signals.has(Signals::S7_ART);
        let mut run_h: Vec<u8> = Vec::new();
        let mut run_v: Vec<u8> = Vec::new();
        for g in grids.iter().filter(|g| !g.hud) {
            let back = backdrop[layer_ix(g)];
            let (cols, rows) = (usize::from(g.cols), usize::from(g.rows));
            let same = |a: usize, b: usize| {
                g.cells[a].pattern == g.cells[b].pattern && g.cells[a].palette == g.cells[b].palette
            };
            let block = art && !back && cols > 0 && g.cells.len() == cols * rows;
            if block {
                run_h.clear();
                run_h.resize(cols * rows, 1);
                run_v.clear();
                run_v.resize(cols * rows, 1);
                for r in 0..rows {
                    for c in 0..cols {
                        let i = r * cols + c;
                        if c > 0 && same(i, i - 1) {
                            run_h[i] = run_h[i - 1].saturating_add(1);
                        }
                        if r > 0 && same(i, i - cols) {
                            run_v[i] = run_v[i - cols].saturating_add(1);
                        }
                    }
                }
                // Propagate each run's length back over the whole run.
                for r in (0..rows).rev() {
                    for c in (0..cols).rev() {
                        let i = r * cols + c;
                        if c + 1 < cols && same(i, i + 1) {
                            run_h[i] = run_h[i].max(run_h[i + 1]);
                        }
                        if r + 1 < rows && same(i, i + cols) {
                            run_v[i] = run_v[i].max(run_v[i + cols]);
                        }
                    }
                }
            }
            for i in 0..g.cells.len() {
                let cell = &g.cells[i];
                if cell.partial() {
                    continue;
                }
                let key = TileKey {
                    pattern: cell.pattern,
                    palette: cell.palette,
                    layer: g.layer,
                    cell_px: g.cell_px,
                };
                if let Some(e) = self.table.entry_mut(key) {
                    e.evidence.seen = e.evidence.seen.saturating_add(1);
                    if block && run_h[i] >= 3 && run_v[i] >= 3 {
                        e.evidence.prior = e.evidence.prior.saturating_add(1);
                    }
                    if back {
                        self.dirty.push(key);
                    }
                }
            }
        }
    }

    fn vote(&mut self, key: TileKey, f: impl FnOnce(&mut Evidence)) {
        if let Some(e) = self.table.entry_mut(key) {
            f(&mut e.evidence);
            self.dirty.push(key);
        }
    }

    /// Foreground cells whose picture rectangle meets the given rectangle.
    fn keys_in(&mut self, grids: &[TileGrid], backdrop: &[bool; 4], r: (i32, i32, i32, i32)) {
        self.hits.clear();
        for g in grids.iter().filter(|g| !g.hud && !backdrop[layer_ix(g)]) {
            let Some((c0, r0, c1, r1)) = cell_range(g, r.0, r.1, r.2, r.3) else {
                continue;
            };
            for row in r0..=r1 {
                for col in c0..=c1 {
                    if let (Some(k), Some(c)) = (g.key_at(col, row), g.cell(col, row)) {
                        if !c.partial() {
                            self.hits.push((k, *c));
                        }
                    }
                }
            }
        }
    }

    /// The keys of the cells of `g` the feet rest on, `feet` = `(x0, x1,
    /// inset, y1)`. `aligned`: only cells whose top edge is at the feet (a
    /// floor); a slope's feet are inside its tile.
    fn keys_under_feet(aligned: bool, g: &TileGrid, feet: (i32, i32, i32, i32)) -> Vec<TileKey> {
        let (x0, x1, ins, y1) = feet;
        let mut out = Vec::new();
        for x in [x0 + ins, (x0 + x1) / 2, x1 - ins - 1] {
            let Some((c, r)) = g.cell_at_picture(x, y1 + 1) else {
                continue;
            };
            let top = g.cell_picture_origin(c, r).1;
            let (Some(k), Some(cell)) = (g.key_at(c, r), g.cell(c, r)) else {
                continue;
            };
            if (!aligned || (top - y1).abs() <= 3) && !cell.partial() && !out.contains(&k) {
                out.push(k);
            }
        }
        out
    }

    /// S1, S2, S3, S6 and the walkable-air count, from the tracked player.
    fn watch_player(&mut self, f: &FrameObs<'_>, backdrop: &[bool; 4]) {
        let (Some(p), Some(fg)) = (f.player, Self::foreground(f.grids, backdrop)) else {
            self.dynamics.valid = false;
            return;
        };
        let e = p.entity;
        let band = (fg.layer, fg.rows_px.0, fg.rows_px.1);
        let cam = (
            fg.origin_world.0 + i64::from(fg.offset.0),
            fg.origin_world.1 + i64::from(fg.offset.1),
        );
        let prev = self.dynamics;
        let moved = if prev.valid && prev.track == p.track && prev.band == band {
            let (dcx, dcy) = (cam.0 - prev.cam.0, cam.1 - prev.cam.1);
            let (px, py) = (i64::from(e.x0 - prev.x0), i64::from(e.y1 - prev.y1));
            (dcx.abs() <= MAX_STEP_PX
                && dcy.abs() <= MAX_STEP_PX
                && px.abs() <= MAX_STEP_PX
                && py.abs() <= MAX_STEP_PX)
                .then(|| ((px + dcx) as i32, (py + dcy) as i32))
        } else {
            None
        };
        let mut d = Dynamics {
            valid: true,
            track: p.track,
            band,
            x0: e.x0,
            y1: e.y1,
            cam,
            ..prev
        };
        let Some((wdx, wdy)) = moved else {
            d.fall = 0;
            d.still_v = 0;
            d.still = [0; 2];
            d.episode = [0; 2];
            d.slope_run = 0;
            d.last_wdy = 0;
            self.dynamics = d;
            return;
        };

        let side = f.orientation == Orientation::SideView;
        let ins = (e.width() / 4).clamp(0, 3);
        let (bx0, bx1) = (e.x0 + ins, e.x1 - ins);
        let hit_rect = if side {
            (bx0, e.y0, bx1, (e.y1 - 3).max(e.y0 + 1))
        } else {
            (bx0, e.y1 - 4, bx1, e.y1)
        };

        // S3 first, so the cells that hide him do not also count as air.
        let mut hidden_keys: Vec<TileKey> = Vec::new();
        if self.signals.has(Signals::S3_HIDDEN) {
            let behind = f.sprites.iter().any(|s| {
                s.behind_bg
                    && i32::from(s.x) >= e.x0
                    && i32::from(s.y) >= e.y0
                    && i32::from(s.x) + i32::from(s.w) <= e.x1
                    && i32::from(s.y) + i32::from(s.h) <= e.y1
            });
            let rect = f
                .hidden
                .or_else(|| behind.then_some((e.x0, e.y0, e.x1, e.y1)));
            if let Some(r) = rect {
                self.keys_in(f.grids, backdrop, r);
                let need_priority = f.hidden.is_none();
                let hits = std::mem::take(&mut self.hits);
                for (k, c) in &hits {
                    if !need_priority || c.priority() {
                        hidden_keys.push(*k);
                        self.vote(*k, |ev| ev.hides_player = ev.hides_player.saturating_add(1));
                    }
                }
                self.hits = hits;
            }
        }

        // Walkable air: his body over a cell, while he moves in top-down.
        let moving = wdx != 0 || wdy != 0 || f.pad != Dirs::default();
        if self.signals.has(Signals::S1_SUPPORT) && (side || moving) {
            self.keys_in(std::slice::from_ref(fg), backdrop, hit_rect);
            let hits = std::mem::take(&mut self.hits);
            for (k, _) in &hits {
                if !hidden_keys.contains(k) {
                    self.vote(*k, |ev| ev.overlapped = ev.overlapped.saturating_add(1));
                }
            }
            self.hits = hits;
        }

        if side {
            let feet = (e.x0, e.x1, ins, e.y1);
            let under = Self::keys_under_feet(true, fg, feet);
            let inside = Self::keys_under_feet(false, fg, feet);
            self.side_dynamics(&mut d, f, (wdx, wdy), &under, &inside);
        }
        self.blocking(&mut d, f, fg, (wdx, wdy), (bx0, bx1), side);
        d.last_wdy = wdy;
        self.dynamics = d;
    }

    /// S1 (landing, standing) and S6 for a side view.
    fn side_dynamics(
        &mut self,
        d: &mut Dynamics,
        f: &FrameObs<'_>,
        (wdx, wdy): (i32, i32),
        under: &[TileKey],
        inside: &[TileKey],
    ) {
        let s1 = self.signals.has(Signals::S1_SUPPORT);
        if wdy > 0 && !f.pad.up {
            d.fall = d.fall.saturating_add(1);
        } else if wdy == 0 {
            if d.fall >= 2 && s1 {
                for k in under {
                    self.vote(*k, |ev| ev.floor_top = ev.floor_top.saturating_add(3));
                }
            }
            d.fall = 0;
        } else {
            d.fall = 0;
        }
        if wdy == 0 {
            d.still_v = d.still_v.saturating_add(1);
            if d.still_v >= STAND_FRAMES && s1 {
                for k in under {
                    self.vote(*k, |ev| ev.floor_top = ev.floor_top.saturating_add(1));
                }
            }
        } else {
            d.still_v = 0;
        }
        // S6: a constant rise or fall that follows the walk.
        let on_slope = wdx != 0
            && wdy != 0
            && wdy == d.last_wdy
            && wdy.abs() <= wdx.abs()
            && !f.pad.up
            && !f.pad.down;
        d.slope_run = if on_slope {
            d.slope_run.saturating_add(1)
        } else {
            0
        };
        if on_slope && d.slope_run >= 4 && self.signals.has(Signals::S6_SLOPE) {
            let rising_right = (wdy < 0) == (wdx > 0);
            let eighths = ((wdy.abs() * 8) / wdx.abs()).clamp(1, 8) as u32;
            for k in inside {
                self.vote(*k, |ev| {
                    ev.slope = ev.slope.saturating_add(if rising_right { 1 } else { -1 });
                    ev.slope_n = ev.slope_n.saturating_add(1);
                    ev.slope_eighths = ev.slope_eighths.saturating_add(eighths);
                });
            }
        }
    }

    /// S2: input held, no movement, nothing else over the tile.
    fn blocking(
        &mut self,
        d: &mut Dynamics,
        f: &FrameObs<'_>,
        fg: &TileGrid,
        (wdx, wdy): (i32, i32),
        (bx0, bx1): (i32, i32),
        side: bool,
    ) {
        let e = f.player.map(|p| p.entity).unwrap_or_default();
        let axes = [(f.pad.x(), wdx), (f.pad.y(), wdy)];
        for (a, (dir, moved)) in axes.into_iter().enumerate() {
            if a == 1 && side {
                d.still[1] = 0;
                continue;
            }
            if dir != 0 && dir == d.still_dir[a] && moved == 0 {
                d.still[a] = d.still[a].saturating_add(1);
            } else {
                d.still[a] = 0;
                d.episode[a] = 0;
            }
            d.still_dir[a] = dir;
            if !self.signals.has(Signals::S2_BLOCKING)
                || d.still[a] < BLOCK_FRAMES
                || d.episode[a] >= EPISODE_VOTES
            {
                continue;
            }
            d.episode[a] += 1;
            let (idx, probes): (usize, [(i32, i32); 2]) = match (a, dir > 0) {
                (0, true) => (1, [(e.x1, e.y1 - 3), (bx1, e.y1 - 3)]),
                (0, false) => (0, [(e.x0 - 1, e.y1 - 3), (bx0 - 1, e.y1 - 3)]),
                (_, true) => (3, [((bx0 + bx1) / 2, e.y1), ((bx0 + bx1) / 2, e.y1)]),
                (_, false) => (
                    2,
                    [((bx0 + bx1) / 2, e.y1 - 5), ((bx0 + bx1) / 2, e.y1 - 5)],
                ),
            };
            let mut seen: Vec<TileKey> = Vec::new();
            for (px, py) in probes {
                let covered = f
                    .entities
                    .iter()
                    .any(|o| *o != e && px >= o.x0 && px < o.x1 && py >= o.y0 && py < o.y1);
                if covered {
                    continue;
                }
                let Some((c, r)) = fg.cell_at_picture(px, py) else {
                    continue;
                };
                if let (Some(k), Some(cell)) = (fg.key_at(c, r), fg.cell(c, r)) {
                    if !cell.partial() && !seen.contains(&k) {
                        seen.push(k);
                    }
                }
            }
            for k in seen {
                self.vote(k, |ev| ev.blocked[idx] = ev.blocked[idx].saturating_add(1));
            }
        }
    }

    /// S4: a key next to solid ground that nothing walks over.
    fn vote_mass(&mut self, grids: &[TileGrid], layer: u8, backdrop: &[bool; 4], o: Orientation) {
        let solid = |t: &TileHeightTable, k: &TileKey| {
            t.get(k).is_some_and(|e| {
                e.learned.confidence >= Learned::DRAW_LEVEL && e.learned.class.is_solid()
            })
        };
        let mut votes: Vec<TileKey> = Vec::new();
        for g in grids
            .iter()
            .filter(|g| !g.hud && !backdrop[layer_ix(g)] && g.layer == layer)
        {
            let (cols, rows) = (g.cols, g.rows);
            for r in 0..rows {
                for c in 0..cols {
                    let (Some(k), Some(cell)) = (g.key_at(c, r), g.cell(c, r)) else {
                        continue;
                    };
                    if cell.partial() {
                        continue;
                    }
                    let mut near =
                        r > 0 && g.key_at(c, r - 1).is_some_and(|n| solid(&self.table, &n));
                    if o == Orientation::TopDown && !near {
                        near = g.key_at(c, r + 1).is_some_and(|n| solid(&self.table, &n))
                            || (c > 0
                                && g.key_at(c - 1, r).is_some_and(|n| solid(&self.table, &n)))
                            || g.key_at(c + 1, r).is_some_and(|n| solid(&self.table, &n));
                    }
                    if near {
                        votes.push(k);
                    }
                }
            }
        }
        for k in votes {
            self.vote(k, |ev| ev.mass = ev.mass.saturating_add(1));
        }
    }

    fn run_ladder(&mut self, backdrop: &[bool; 4]) {
        self.dirty.sort_unstable();
        self.dirty.dedup();
        let dirty = std::mem::take(&mut self.dirty);
        for k in &dirty {
            let back = backdrop[usize::from(k.layer).min(3)];
            if let Some(e) = self.table.map.get_mut(k) {
                let cand = if back {
                    (Class::Backdrop, 0)
                } else {
                    classify(&e.evidence, mass_share_ok(&e.evidence))
                };
                step(e, cand);
            }
        }
        self.dirty = dirty;
        debug_assert!(self.table.len() <= MAX_KEYS);
    }
}

#[cfg(test)]
#[path = "learner_tests.rs"]
mod tests;
