//! Whole-level heights from the stitched canvas (ticket W34-07;
//! `docs/design/HD2D_PLAN.md` W34-07).
//!
//! The learner ([`crate::live_learn`]) only ever sees the tiles on screen.
//! The canvas model has remembered much more: everything the camera showed
//! and, since the level scout, the margins nobody scrolled to. This module
//! gives those remembered pixels the learner's names, so a tile learned once
//! is known wherever it sits in the level.
//!
//! ## How a canvas cell gets a [`TileKey`]
//!
//! A learner key is a hash of a tile's VRAM bit planes and of its palette's
//! colours ([`crate::tile_capture`]). A canvas cell holds the PPU's output
//! for one dot: a CGRAM index (`palette_base + palette * colours + colour`,
//! `rf_snes::ppu::bg`) and the id of the palette snapshot it was seen under
//! (`rf_enhance::canvas::PaletteTable`). Both halves are recoverable, so the
//! key is rebuilt rather than approximated: the colour numbers of an 8x8 block
//! are written back into bit planes, hashed by the SAME function
//! (`tile_capture::hash_rows`), and the palette hash is taken of the same
//! CGRAM words. A unit test captures a synthetic frame's grid and stitches the
//! same frame into a canvas, and asserts every cell comes back with the key
//! the capture gave it.
//!
//! * Colour 0 is transparent: the PPU hands the dot to a layer behind, so a
//!   layer's canvas has no cell there. A block with such dots is keyed as if
//!   they were colour 0 (exactly what its bit planes hold), and is looked at
//!   again whenever more of it has been seen ("open" below). A sprite that
//!   stood over a dot leaves the same hole until a frame without it.
//! * The NES cannot be re-tiled: its canvas cells hold the 6-bit master
//!   colour read out of palette RAM (`palette_table` module doc), from which
//!   neither the pattern bytes nor the palette number come back.
//! * 16x16 character layers and the hires and Mode 7 modes are not handled
//!   (no grid of 8x8 keys to match).
//!
//! ## Cost
//!
//! A pass looks at every block that is not finished; a finished block is not
//! looked at again. Turning keys into heights is a table lookup per block.

use std::sync::Arc;

use rf_enhance::canvas::{CanvasModel, PaletteTable};
use rf_enhance::heights::{
    canvas_cache_key, table_cache_key, Class, HeightCanvas, HeightLearner, Learned, Stats,
    TileHeightTable, TileKey,
};
use rf_enhance::stitcher::Canvas;

use crate::tile_capture::{hash_rows, palette_hash};

/// Side of a cell in canvas pixels.
pub const CELL: i64 = 8;
/// Core frames between passes over the canvas.
pub const PASS_EVERY: u32 = 45;
/// Core frames between writes of a changed table or scene heights.
pub const PERSIST_EVERY: u32 = 300;

/// Passes over the canvas, for the cost rows (process-wide, read by the
/// proof shots).
#[derive(Debug)]
pub struct Telemetry {
    pub passes: std::sync::atomic::AtomicU64,
    pub total_us: std::sync::atomic::AtomicU64,
    pub max_us: std::sync::atomic::AtomicU64,
    /// Passes that found a changed key or table and rebuilt the heights.
    pub rebuilds: std::sync::atomic::AtomicU64,
    pub rebuild_us: std::sync::atomic::AtomicU64,
    pub max_rebuild_us: std::sync::atomic::AtomicU64,
    /// Blocks of the largest grid seen.
    pub blocks: std::sync::atomic::AtomicU64,
    /// Frames seen, scene changes, and frames with no re-tilable playfield.
    pub frames: std::sync::atomic::AtomicU64,
    pub scene_changes: std::sync::atomic::AtomicU64,
    pub no_format: std::sync::atomic::AtomicU64,
    pub restored: std::sync::atomic::AtomicU64,
    /// The audit: cells of the live playfield grid compared with the key the
    /// canvas gives the same place. Both keys present and equal, equal in
    /// pattern only, equal in palette only, different, and cells the canvas
    /// has no key for.
    pub audit_equal: std::sync::atomic::AtomicU64,
    pub audit_pattern_only: std::sync::atomic::AtomicU64,
    pub audit_palette_only: std::sync::atomic::AtomicU64,
    pub audit_different: std::sync::atomic::AtomicU64,
    pub audit_unkeyed: std::sync::atomic::AtomicU64,
    /// Time spent voting on the grid's alignment.
    pub calibration_us: std::sync::atomic::AtomicU64,
    /// The shift the alignment vote last decided on (dots, -4..4), and how
    /// many votes were taken.
    pub shift_dx: std::sync::atomic::AtomicI64,
    pub shift_dy: std::sync::atomic::AtomicI64,
    pub calibrations: std::sync::atomic::AtomicU64,
}

static TELEMETRY: Telemetry = Telemetry {
    passes: std::sync::atomic::AtomicU64::new(0),
    total_us: std::sync::atomic::AtomicU64::new(0),
    max_us: std::sync::atomic::AtomicU64::new(0),
    rebuilds: std::sync::atomic::AtomicU64::new(0),
    rebuild_us: std::sync::atomic::AtomicU64::new(0),
    max_rebuild_us: std::sync::atomic::AtomicU64::new(0),
    blocks: std::sync::atomic::AtomicU64::new(0),
    frames: std::sync::atomic::AtomicU64::new(0),
    scene_changes: std::sync::atomic::AtomicU64::new(0),
    no_format: std::sync::atomic::AtomicU64::new(0),
    restored: std::sync::atomic::AtomicU64::new(0),
    audit_equal: std::sync::atomic::AtomicU64::new(0),
    audit_pattern_only: std::sync::atomic::AtomicU64::new(0),
    audit_palette_only: std::sync::atomic::AtomicU64::new(0),
    audit_different: std::sync::atomic::AtomicU64::new(0),
    audit_unkeyed: std::sync::atomic::AtomicU64::new(0),
    calibration_us: std::sync::atomic::AtomicU64::new(0),
    shift_dx: std::sync::atomic::AtomicI64::new(0),
    shift_dy: std::sync::atomic::AtomicI64::new(0),
    calibrations: std::sync::atomic::AtomicU64::new(0),
};

/// The process-wide pass costs.
#[must_use]
pub fn telemetry() -> &'static Telemetry {
    &TELEMETRY
}

/// How a layer's canvas cells map back to tile bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerFormat {
    pub layer: u8,
    /// Bits per pixel: 2, 4 or 8.
    pub depth: u8,
    /// First CGRAM entry of the layer's palettes (`bg::palette_base`).
    pub palette_base: u8,
}

impl LayerFormat {
    /// The format of `layer` in BG `mode`, or `None` for what cannot be
    /// re-tiled (16x16 characters, hires, Mode 7, a layer the mode lacks).
    #[must_use]
    pub fn for_mode(mode: u8, layer: u8, tile16: bool) -> Option<Self> {
        if mode > 4 || tile16 || layer > 3 {
            return None;
        }
        let depth = rf_snes::ppu::bg::bit_depths(mode)[usize::from(layer)];
        (depth != 0).then(|| Self {
            layer,
            depth,
            palette_base: rf_snes::ppu::bg::palette_base(mode, usize::from(layer)),
        })
    }

    /// Colours in one of the layer's palettes.
    fn colours(self) -> usize {
        match self.depth {
            2 => 4,
            4 => 16,
            _ => 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// Nothing usable seen yet.
    Pending,
    /// Keyed, with this many dots of the block still unseen.
    Open(TileKey, u8),
    /// Every dot seen: never looked at again.
    Done(TileKey),
}

/// One block read off the canvas.
enum Block {
    Unseen,
    Seen(TileKey, u8),
}

/// The keys of every 8x8 block of one layer's canvas, on the grid the live
/// capture draws (`phase` is where its block edges fall, canvas pixels mod 8).
#[derive(Debug, Clone)]
pub struct LevelKeys {
    fmt: LayerFormat,
    phase: (i64, i64),
    /// Canvas position of block (0, 0)'s top-left corner.
    origin: (i64, i64),
    cols: u32,
    rows: u32,
    slots: Vec<Slot>,
}

fn align_down(v: i64, phase: i64) -> i64 {
    v - (v - phase).rem_euclid(CELL)
}

impl LevelKeys {
    #[must_use]
    pub fn new(fmt: LayerFormat, phase: (i64, i64)) -> Self {
        Self {
            fmt,
            phase: (phase.0.rem_euclid(CELL), phase.1.rem_euclid(CELL)),
            origin: (0, 0),
            cols: 0,
            rows: 0,
            slots: Vec::new(),
        }
    }

    #[must_use]
    pub fn matches(&self, fmt: LayerFormat, phase: (i64, i64)) -> bool {
        self.fmt == fmt && self.phase == (phase.0.rem_euclid(CELL), phase.1.rem_euclid(CELL))
    }

    /// The key of the block holding canvas pixel (`x`, `y`).
    #[must_use]
    pub fn key_at_canvas(&self, x: i64, y: i64) -> Option<TileKey> {
        let (c, r) = (
            (x - self.origin.0).div_euclid(CELL),
            (y - self.origin.1).div_euclid(CELL),
        );
        self.key(u32::try_from(c).ok()?, u32::try_from(r).ok()?)
    }

    /// The key of block (`col`, `row`), when it has one.
    #[must_use]
    pub fn key(&self, col: u32, row: u32) -> Option<TileKey> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        match self.slots[row as usize * self.cols as usize + col as usize] {
            Slot::Pending => None,
            Slot::Open(k, _) | Slot::Done(k) => Some(k),
        }
    }

    #[must_use]
    pub fn origin(&self) -> (i64, i64) {
        self.origin
    }

    #[must_use]
    pub fn dims(&self) -> (u32, u32) {
        (self.cols, self.rows)
    }

    /// Blocks with a key, and blocks still being watched (a key with dots
    /// unseen, or nothing yet).
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let keyed = self
            .slots
            .iter()
            .filter(|s| !matches!(s, Slot::Pending))
            .count();
        let finished = self
            .slots
            .iter()
            .filter(|s| matches!(s, Slot::Done(_)))
            .count();
        (keyed, self.slots.len() - finished)
    }

    /// Grow the grid to cover `canvas`, keeping every key where it was.
    fn fit(&mut self, canvas: &Canvas) {
        let (cx, cy) = canvas.origin();
        let (w, h) = (canvas.width() as i64, canvas.height() as i64);
        let ox = align_down(cx, self.phase.0);
        let oy = align_down(cy, self.phase.1);
        let cols = ((cx + w - ox + CELL - 1) / CELL).max(0) as u32;
        let rows = ((cy + h - oy + CELL - 1) / CELL).max(0) as u32;
        if (ox, oy, cols, rows) == (self.origin.0, self.origin.1, self.cols, self.rows) {
            return;
        }
        let mut slots = vec![Slot::Pending; cols as usize * rows as usize];
        if self.cols > 0 && self.rows > 0 {
            let (dc, dr) = (
                (self.origin.0 - ox).div_euclid(CELL),
                (self.origin.1 - oy).div_euclid(CELL),
            );
            for r in 0..self.rows {
                for c in 0..self.cols {
                    let (nc, nr) = (i64::from(c) + dc, i64::from(r) + dr);
                    if nc >= 0 && nr >= 0 && nc < i64::from(cols) && nr < i64::from(rows) {
                        slots[nr as usize * cols as usize + nc as usize] =
                            self.slots[r as usize * self.cols as usize + c as usize];
                    }
                }
            }
        }
        self.origin = (ox, oy);
        self.cols = cols;
        self.rows = rows;
        self.slots = slots;
    }

    /// Look at every block that is not finished. Returns whether any key
    /// changed.
    pub fn refresh(&mut self, canvas: &Canvas, palettes: &PaletteTable) -> bool {
        if canvas.width() == 0 || canvas.height() == 0 {
            return false;
        }
        self.fit(canvas);
        let mut fallback = Fallback::default();
        let mut changed = false;
        for r in 0..self.rows {
            for c in 0..self.cols {
                let at = r as usize * self.cols as usize + c as usize;
                let before = self.slots[at];
                let (old_holes, skip) = match before {
                    Slot::Done(_) => continue,
                    Slot::Open(_, h) => (Some(h), false),
                    Slot::Pending => (None, false),
                };
                let _ = skip;
                let x0 = self.origin.0 + i64::from(c) * CELL;
                let y0 = self.origin.1 + i64::from(r) * CELL;
                let holes = count_holes(canvas, x0, y0);
                if holes == 64 || old_holes == Some(holes) {
                    continue;
                }
                let next = match read_block(canvas, palettes, &mut fallback, self.fmt, x0, y0) {
                    Block::Unseen => Slot::Pending,
                    Block::Seen(k, 0) => Slot::Done(k),
                    Block::Seen(k, h) => Slot::Open(k, h),
                };
                if next != before {
                    self.slots[at] = next;
                    changed = true;
                }
            }
        }
        changed
    }

    /// The heights the table gives these keys. `scout` counts the solid
    /// blocks whose first dot came from the scout rather than from play.
    #[must_use]
    pub fn heights(
        &self,
        table: &TileHeightTable,
        canvas: &Canvas,
    ) -> Option<(HeightCanvas, LevelReport)> {
        let mut out = HeightCanvas::new(self.origin, self.cols, self.rows, CELL as u8)?;
        let mut rep = LevelReport::default();
        for r in 0..self.rows {
            for c in 0..self.cols {
                let Some(key) = self.key(c, r) else {
                    rep.unseen += 1;
                    continue;
                };
                rep.keyed += 1;
                let Some(e) = table.get(&key) else { continue };
                let (class, depth) = e.learned.drawn();
                if class == Class::Unknown {
                    continue;
                }
                rep.drawn += 1;
                if class.is_solid() {
                    rep.solid += 1;
                    let x = self.origin.0 + i64::from(c) * CELL;
                    let y = self.origin.1 + i64::from(r) * CELL;
                    if block_from_scout(canvas, x, y) {
                        rep.solid_scout += 1;
                    }
                }
                out.set(
                    c,
                    r,
                    Learned {
                        class,
                        height_eighths: if class == Class::Backdrop { 0 } else { depth },
                        confidence: e.learned.confidence,
                    },
                );
            }
        }
        Some((out, rep))
    }
}

/// Dots of the block at canvas (`x0`, `y0`) the layer's canvas has no cell for.
fn count_holes(canvas: &Canvas, x0: i64, y0: i64) -> u8 {
    let mut holes = 0u8;
    for py in 0..CELL {
        for px in 0..CELL {
            if canvas.get(x0 + px, y0 + py).is_none() {
                holes += 1;
            }
        }
    }
    holes
}

/// Whether the first known dot of the block is a scout cell.
fn block_from_scout(canvas: &Canvas, x0: i64, y0: i64) -> bool {
    for py in 0..CELL {
        for px in 0..CELL {
            if canvas.get(x0 + px, y0 + py).is_some() {
                return canvas.is_scout(x0 + px, y0 + py);
            }
        }
    }
    false
}

/// The palette snapshot most of a layer's cells were seen under, found once
/// per pass and only if some cell needs it.
#[derive(Default)]
struct Fallback(Option<Option<u8>>);

impl Fallback {
    fn get(&mut self, canvas: &Canvas, palettes: &PaletteTable) -> Option<u8> {
        *self.0.get_or_insert_with(|| {
            let mut count = [0u32; 256];
            for id in canvas.cells().iter().flatten().filter_map(|p| p.sprite_id) {
                count[usize::from(id)] += 1;
            }
            count
                .iter()
                .enumerate()
                .filter(|(id, n)| **n > 0 && palettes.get(*id as u8).is_some())
                .max_by_key(|(id, n)| (**n, std::cmp::Reverse(*id)))
                .map(|(id, _)| id as u8)
        })
    }
}

/// Rebuild the key of the block at canvas (`x0`, `y0`) from its dots.
fn read_block(
    canvas: &Canvas,
    palettes: &PaletteTable,
    fallback: &mut Fallback,
    fmt: LayerFormat,
    x0: i64,
    y0: i64,
) -> Block {
    let depth = usize::from(fmt.depth);
    let n = fmt.colours();
    let mut dots = [[0u8; 8]; 8];
    let mut holes = 0u8;
    let mut pal_no: Option<u8> = None;
    let mut snapshot: Option<u8> = None;
    for (py, row) in dots.iter_mut().enumerate() {
        for (px, dot) in row.iter_mut().enumerate() {
            let Some(p) = canvas.get(x0 + px as i64, y0 + py as i64) else {
                holes += 1;
                continue;
            };
            let v = usize::from(p.palette_index.wrapping_sub(fmt.palette_base));
            let colour = (v % n) as u8;
            if colour == 0 {
                holes += 1;
                continue;
            }
            *dot = colour;
            pal_no.get_or_insert((v / n) as u8);
            if snapshot.is_none() {
                snapshot = p.sprite_id.filter(|id| palettes.get(*id).is_some());
            }
        }
    }
    let Some(pal) = pal_no else {
        return Block::Unseen;
    };
    // A cell the scout stitched carries no palette id (it stitches without
    // per-row palettes): it is read under the palette most of the layer was
    // seen with.
    let Some(id) = snapshot.or_else(|| fallback.get(canvas, palettes)) else {
        return Block::Unseen;
    };
    let Some(cgram) = palettes.get(id) else {
        return Block::Unseen;
    };
    // The bit planes as VRAM holds them: row-major, `depth` bytes a row,
    // plane `k` of dot `x` at bit `7 - x`.
    let mut rows = [0u8; 64];
    for (py, row) in dots.iter().enumerate() {
        for plane in 0..depth {
            let mut b = 0u8;
            for (px, &colour) in row.iter().enumerate() {
                b |= ((colour >> plane) & 1) << (7 - px);
            }
            rows[py * depth + plane] = b;
        }
    }
    let tile = hash_rows(&rows, fmt.depth);
    let palette_h = if fmt.depth == 8 {
        palette_hash(cgram, 0, 256)
    } else {
        palette_hash(
            cgram,
            usize::from(fmt.palette_base) + usize::from(pal) * n,
            n,
        )
    };
    Block::Seen(
        TileKey {
            pattern: tile.canon,
            palette: palette_h,
            layer: fmt.layer,
            cell_px: CELL as u8,
        },
        holes,
    )
}

/// What a layer's canvas holds, in the terms the re-tiler cares about (the
/// proof shots print it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Census {
    /// Cells with a pixel, of which from the scout.
    pub known: usize,
    pub scout: usize,
    /// Known cells with no palette id, or one the table lacks.
    pub no_palette: usize,
    /// Known cells whose colour number is 0 (never drawn by a layer).
    pub colour0: usize,
    /// 8x8 blocks on the canvas's own grid with all 64, some, or none of
    /// their dots known.
    pub full: usize,
    pub partial: usize,
    pub empty: usize,
}

/// Count what `canvas` holds for a layer of format `fmt`.
#[must_use]
pub fn census(canvas: &Canvas, palettes: &PaletteTable, fmt: LayerFormat) -> Census {
    let mut c = Census::default();
    let n = fmt.colours();
    for (i, cell) in canvas.cells().iter().enumerate() {
        let Some(p) = cell else { continue };
        c.known += 1;
        let (x, y) = (
            canvas.origin().0 + (i % canvas.width().max(1)) as i64,
            canvas.origin().1 + (i / canvas.width().max(1)) as i64,
        );
        c.scout += usize::from(canvas.is_scout(x, y));
        if p.sprite_id.is_none_or(|id| palettes.get(id).is_none()) {
            c.no_palette += 1;
        }
        if usize::from(p.palette_index.wrapping_sub(fmt.palette_base)) % n == 0 {
            c.colour0 += 1;
        }
    }
    let (ox, oy) = (
        align_down(canvas.origin().0, 0),
        align_down(canvas.origin().1, 0),
    );
    let (w, h) = (canvas.width() as i64, canvas.height() as i64);
    // Ranges, so every pass of both loops advances by construction (law 8).
    let (end_x, end_y) = (canvas.origin().0 + w, canvas.origin().1 + h);
    for by in (oy..end_y).step_by(CELL as usize) {
        for bx in (ox..end_x).step_by(CELL as usize) {
            match count_holes(canvas, bx, by) {
                0 => c.full += 1,
                64 => c.empty += 1,
                _ => c.partial += 1,
            }
        }
    }
    c
}

/// What a pass found, for the readout and the costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LevelReport {
    /// Blocks with a key.
    pub keyed: usize,
    /// Blocks nothing is known about.
    pub unseen: usize,
    /// Blocks the table draws non-flat.
    pub drawn: usize,
    /// Blocks drawn as solid, and how many of them the scout saw first.
    pub solid: usize,
    pub solid_scout: usize,
}

/// A scene's heights as the shell draws them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelHeights {
    pub scene: u64,
    pub layer: u8,
    /// Blocks on the layer's canvas grid (canvas pixels, 8 px cells).
    pub canvas: HeightCanvas,
    /// Restored from the cache; no pass over the canvas has replaced it yet.
    pub restored: bool,
    pub report: LevelReport,
}

/// Where the live capture's block grid sits on a layer's canvas: the canvas
/// position of block (0, 0)'s corner, mod 8.
#[must_use]
pub fn grid_phase(picture_origin: (i64, i64), grid_offset: (u8, u8), grid_top: u16) -> (i64, i64) {
    (
        (picture_origin.0 - i64::from(grid_offset.0)).rem_euclid(CELL),
        (picture_origin.1 + i64::from(grid_top) - i64::from(grid_offset.1)).rem_euclid(CELL),
    )
}

const ORIENT_VERSION: u8 = 1;

fn extra_key(rom: &str, what: &str) -> rf_cache::CacheKey {
    rf_cache::CacheKey {
        rom_sha256: rom.to_string(),
        asset_hash: what.to_string(),
        producer: "retroforge-heights".to_string(),
        settings_hash: format!("{what}-v{ORIENT_VERSION}"),
    }
}

/// The remembered orientation of a ROM: a code byte under a version byte.
fn orientation_bytes(o: rf_enhance::heights::Orientation) -> Option<Vec<u8>> {
    use rf_enhance::heights::Orientation as O;
    let code = match o {
        O::SideView => 1,
        O::TopDown => 2,
        O::Unknown => return None,
    };
    Some(vec![ORIENT_VERSION, code])
}

fn orientation_from(bytes: &[u8]) -> Option<rf_enhance::heights::Orientation> {
    use rf_enhance::heights::Orientation as O;
    match bytes {
        [v, 1] if *v == ORIENT_VERSION => Some(O::SideView),
        [v, 2] if *v == ORIENT_VERSION => Some(O::TopDown),
        _ => None,
    }
}

/// What the learner session remembers between sessions and across the scenes
/// of one: the ROM's tile table and call on the camera, and each scene's
/// heights, all through the canvas model's cache (one index per root).
pub struct Persist {
    rom: String,
    table_sig: Option<Stats>,
    table_written: Option<u32>,
    orient_written: Option<rf_enhance::heights::Orientation>,
}

impl Persist {
    #[must_use]
    pub fn new(rom: String) -> Self {
        Self {
            rom,
            table_sig: None,
            table_written: None,
            orient_written: None,
        }
    }

    /// The ROM's table, when an earlier session left one.
    pub fn load_table(&self, canvases: &mut CanvasModel) -> Option<TileHeightTable> {
        let bytes = canvases.fetch_extra(&table_cache_key(&self.rom))?;
        match TileHeightTable::from_bytes(&bytes) {
            Ok(t) => Some(t),
            Err(why) => {
                log::warn!("tile-heights cache: ignoring entry: {why}");
                None
            }
        }
    }

    /// The camera call an earlier session settled on.
    pub fn load_orientation(
        &self,
        canvases: &mut CanvasModel,
    ) -> Option<rf_enhance::heights::Orientation> {
        orientation_from(&canvases.fetch_extra(&extra_key(&self.rom, "orientation"))?)
    }

    /// A scene's heights, when an earlier visit left them.
    pub fn load_scene(&self, canvases: &mut CanvasModel, scene: u64) -> Option<HeightCanvas> {
        let id = rf_enhance::scene_identity::SceneId(scene);
        let bytes = canvases.fetch_extra(&canvas_cache_key(&self.rom, id))?;
        match HeightCanvas::from_bytes(&bytes, id) {
            Ok(c) => Some(c),
            Err(why) => {
                log::warn!("height canvas cache: ignoring {}: {why}", id.to_hex());
                None
            }
        }
    }

    /// Queue the table and the call on the camera when they changed and
    /// [`PERSIST_EVERY`] frames have passed (or `force`).
    pub fn save_session(
        &mut self,
        canvases: &mut CanvasModel,
        learner: &HeightLearner,
        orientation: rf_enhance::heights::Orientation,
        frame: u32,
        force: bool,
    ) {
        let due = force
            || self
                .table_written
                .is_none_or(|w| frame.wrapping_sub(w) >= PERSIST_EVERY);
        if !due {
            return;
        }
        let sig = learner.stats();
        if self.table_sig != Some(sig) && !learner.table().is_empty() {
            canvases.queue_extra(table_cache_key(&self.rom), learner.table().to_bytes());
            self.table_sig = Some(sig);
        }
        self.table_written = Some(frame);
        if self.orient_written != Some(orientation) {
            if let Some(b) = orientation_bytes(orientation) {
                canvases.queue_extra(extra_key(&self.rom, "orientation"), b);
                self.orient_written = Some(orientation);
            }
        }
    }

    /// Queue a scene's heights.
    pub fn save_scene(&mut self, canvases: &mut CanvasModel, scene: u64, heights: &HeightCanvas) {
        let id = rf_enhance::scene_identity::SceneId(scene);
        canvases.queue_extra(canvas_cache_key(&self.rom, id), heights.to_bytes(id));
    }
}

/// Measuring where the live capture's block grid really falls on a layer's
/// canvas. The geometry gives an answer ([`grid_phase`]), but the two sides
/// count scanlines and registers their own ways (and the capture is allowed
/// to be a line off the picture), so the answer is checked against the data:
/// for cells of the live grid, every one of the 64 shifts of the canvas read
/// votes for the key the capture gave the cell.
#[derive(Debug, Clone, Copy)]
pub struct Calibration {
    votes: [u32; 64],
    audits: u32,
    /// The shift (-4..4 each way) to add to the geometry's phase, once decided.
    shift: Option<(i64, i64)>,
}

/// Most audits spent deciding before the geometry's own answer stands.
const CALIBRATION_AUDITS: u32 = 10;
/// Votes the best shift needs, and how far ahead of the runner-up.
const CALIBRATION_MIN_VOTES: u32 = 12;
/// Core frames between audits while calibrating.
const CALIBRATE_EVERY: u32 = 6;

impl Calibration {
    fn new() -> Self {
        Self {
            votes: [0; 64],
            audits: 0,
            shift: None,
        }
    }

    #[must_use]
    pub fn shift(&self) -> Option<(i64, i64)> {
        self.shift
    }

    /// Votes of one audit: every fourth cell of the grid that has real content.
    fn vote(
        &mut self,
        grid: &rf_enhance::heights::TileGrid,
        origin: (i64, i64),
        canvas: &Canvas,
        palettes: &PaletteTable,
        fmt: LayerFormat,
    ) {
        let mut fb = Fallback::default();
        let mut seen = 0u32;
        for row in 0..grid.rows {
            for col in 0..grid.cols {
                let Some(cell) = grid.cell(col, row).filter(|c| !c.partial() && !c.blank()) else {
                    continue;
                };
                let _ = cell;
                let (px, py) = grid.cell_picture_origin(col, row);
                if px < 0 || px + 8 > 256 || py < i32::from(grid.rows_px.0) {
                    continue;
                }
                seen += 1;
                if !seen.is_multiple_of(4) {
                    continue;
                }
                let want = grid.key_at(col, row).expect("cell exists");
                for dy in -4..4i64 {
                    for dx in -4..4i64 {
                        let (x, y) = (origin.0 + i64::from(px) + dx, origin.1 + i64::from(py) + dy);
                        if let Block::Seen(k, _) = read_block(canvas, palettes, &mut fb, fmt, x, y)
                        {
                            if k == want {
                                self.votes[((dx + 4) + 8 * (dy + 4)) as usize] += 1;
                            }
                        }
                    }
                }
            }
        }
        self.audits += 1;
        let (best, &most) = self
            .votes
            .iter()
            .enumerate()
            .max_by_key(|(i, v)| (**v, std::cmp::Reverse(*i)))
            .expect("64 votes");
        let runner_up = self
            .votes
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != best)
            .map(|(_, v)| *v)
            .max()
            .unwrap_or(0);
        if most >= CALIBRATION_MIN_VOTES && most >= runner_up.saturating_mul(2) {
            self.shift = Some(((best % 8) as i64 - 4, (best / 8) as i64 - 4));
        } else if self.audits >= CALIBRATION_AUDITS {
            // Nothing decisive: the geometry's answer stands.
            self.shift = Some((0, 0));
        }
        if let Some((dx, dy)) = self.shift {
            use std::sync::atomic::Ordering::Relaxed;
            let t = telemetry();
            t.shift_dx.store(dx, Relaxed);
            t.shift_dy.store(dy, Relaxed);
            t.calibrations.fetch_add(1, Relaxed);
        }
    }
}

/// The per-session machine that keeps a scene's keys and heights current.
pub struct LevelTracker {
    calibration: Calibration,
    calibrated_for: Option<(u64, LayerFormat)>,
    persist: Persist,
    scene: Option<u64>,
    keys: Option<LevelKeys>,
    last_pass: Option<u32>,
    last_merges: u64,
    last_table: Option<Stats>,
    sent: Option<Arc<LevelHeights>>,
    scene_written: Option<u32>,
    dirty_scene: bool,
}

impl LevelTracker {
    #[must_use]
    pub fn new(rom: String) -> Self {
        Self {
            calibration: Calibration::new(),
            calibrated_for: None,
            persist: Persist::new(rom),
            scene: None,
            keys: None,
            last_pass: None,
            last_merges: 0,
            last_table: None,
            sent: None,
            scene_written: None,
            dirty_scene: false,
        }
    }

    #[must_use]
    pub fn persist(&mut self) -> &mut Persist {
        &mut self.persist
    }

    /// The scene's heights as they stand: what [`Self::observe`] last built
    /// (or restored). The core thread attaches it to every frame message, so
    /// a frame the UI never receives (the pending cap drops them) cannot lose
    /// an update.
    #[must_use]
    pub fn current(&self) -> Option<Arc<LevelHeights>> {
        self.sent.clone()
    }

    /// Fold one frame in. Returns the heights when they changed (and on the
    /// first frame of a scene that has some stored).
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        frame: u32,
        scene: u64,
        fmt: Option<(LayerFormat, (i64, i64))>,
        live: Option<(&rf_enhance::heights::TileGrid, (i64, i64))>,
        canvases: &mut CanvasModel,
        learner: &HeightLearner,
    ) -> Option<Arc<LevelHeights>> {
        let mut out = None;
        telemetry()
            .frames
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.scene != Some(scene) {
            telemetry()
                .scene_changes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.leave_scene(canvases);
            self.scene = Some(scene);
            self.keys = None;
            self.last_pass = None;
            self.last_merges = 0;
            self.last_table = None;
            self.sent = None;
            if let Some(stored) = self.persist.load_scene(canvases, scene) {
                telemetry()
                    .restored
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let lh = Arc::new(LevelHeights {
                    scene,
                    layer: fmt.map_or(0, |(f, _)| f.layer),
                    report: LevelReport::default(),
                    canvas: stored,
                    restored: true,
                });
                self.sent = Some(lh.clone());
                out = Some(lh);
            }
        }
        let Some((fmt, phase)) = fmt else {
            telemetry()
                .no_format
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return out;
        };
        // Where the capture's blocks fall on this layer's canvas, measured on
        // the first frames of the scene.
        if self.calibrated_for != Some((scene, fmt)) {
            self.calibrated_for = Some((scene, fmt));
            self.calibration = Calibration::new();
            if live.is_none() {
                // Nothing to measure against: the geometry's own answer.
                self.calibration.shift = Some((0, 0));
            }
        }
        if self.calibration.shift.is_none() {
            if let (Some((grid, origin)), Some(layers), Some(pals)) = (
                live,
                canvases.scene_canvases(scene),
                canvases.scene_palettes(scene),
            ) {
                if frame.is_multiple_of(CALIBRATE_EVERY) {
                    let t0 = std::time::Instant::now();
                    self.calibration
                        .vote(grid, origin, &layers[usize::from(fmt.layer)], pals, fmt);
                    telemetry().calibration_us.fetch_add(
                        t0.elapsed().as_micros() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
            }
        }
        let Some((sx, sy)) = self.calibration.shift else {
            return out;
        };
        let phase = (phase.0 + sx, phase.1 + sy);
        // A pass is due every [`PASS_EVERY`] frames, and at once when a scout
        // delta has been merged since the last (the game may be paused while
        // the scout works, so frames alone would not come).
        let merges = canvases.scene_merges(scene);
        let due = self
            .last_pass
            .is_none_or(|p| frame.wrapping_sub(p) >= PASS_EVERY)
            || merges != self.last_merges;
        if !due {
            return out;
        }
        self.last_merges = merges;
        self.last_pass = Some(frame);
        if !self.keys.as_ref().is_some_and(|k| k.matches(fmt, phase)) {
            self.keys = Some(LevelKeys::new(fmt, phase));
            self.last_table = None;
        }
        let (Some(layers), Some(palettes)) = (
            canvases.scene_canvases(scene),
            canvases.scene_palettes(scene),
        ) else {
            return out;
        };
        let canvas = &layers[usize::from(fmt.layer)];
        let keys = self.keys.as_mut().expect("just set");
        let t0 = std::time::Instant::now();
        let keys_changed = keys.refresh(canvas, palettes);
        let refresh_us = t0.elapsed().as_micros() as u64;
        {
            use std::sync::atomic::Ordering::Relaxed;
            TELEMETRY.passes.fetch_add(1, Relaxed);
            TELEMETRY.total_us.fetch_add(refresh_us, Relaxed);
            TELEMETRY.max_us.fetch_max(refresh_us, Relaxed);
            let (c, r) = keys.dims();
            TELEMETRY
                .blocks
                .fetch_max(u64::from(c) * u64::from(r), Relaxed);
        }
        let stats = learner.stats();
        let table_changed = self.last_table != Some(stats);
        let first = self.sent.as_ref().is_none_or(|s| s.restored);
        if !(keys_changed || table_changed || first) {
            return out;
        }
        self.last_table = Some(stats);
        let t1 = std::time::Instant::now();
        let built = keys.heights(learner.table(), canvas);
        {
            use std::sync::atomic::Ordering::Relaxed;
            let us = t1.elapsed().as_micros() as u64;
            TELEMETRY.rebuilds.fetch_add(1, Relaxed);
            TELEMETRY.rebuild_us.fetch_add(us, Relaxed);
            TELEMETRY.max_rebuild_us.fetch_max(us, Relaxed);
        }
        let Some((heights, report)) = built else {
            return out;
        };
        let differs = self
            .sent
            .as_ref()
            .is_none_or(|s| s.restored || s.canvas != heights || s.report != report);
        if differs {
            let lh = Arc::new(LevelHeights {
                scene,
                layer: fmt.layer,
                canvas: heights,
                restored: false,
                report,
            });
            self.sent = Some(lh.clone());
            self.dirty_scene = true;
            out = Some(lh);
        }
        if self.dirty_scene
            && self
                .scene_written
                .is_none_or(|w| frame.wrapping_sub(w) >= PERSIST_EVERY)
        {
            self.write_scene(canvases);
            self.scene_written = Some(frame);
        }
        out
    }

    fn write_scene(&mut self, canvases: &mut CanvasModel) {
        if let (Some(scene), Some(sent)) = (self.scene, &self.sent) {
            if !sent.restored {
                self.persist.save_scene(canvases, scene, &sent.canvas);
            }
        }
        self.dirty_scene = false;
    }

    /// Queue the scene being left.
    fn leave_scene(&mut self, canvases: &mut CanvasModel) {
        if self.dirty_scene {
            self.write_scene(canvases);
        }
        self.scene_written = None;
    }

    /// Compare the live playfield grid, laid on the canvas at `origin` (the
    /// canvas position of picture pixel (0, 0)), with the keys the canvas
    /// gives the same blocks. The numbers go to [`telemetry`]: this is the
    /// check that the re-tiler and the capture name tiles alike.
    pub fn audit(&self, grid: &rf_enhance::heights::TileGrid, origin: (i64, i64)) {
        use std::sync::atomic::Ordering::Relaxed;
        let Some(keys) = &self.keys else { return };
        let t = telemetry();
        for row in 0..grid.rows {
            for col in 0..grid.cols {
                let Some(cell) = grid.cell(col, row) else {
                    continue;
                };
                if cell.partial() || cell.blank() {
                    continue;
                }
                let (px, py) = grid.cell_picture_origin(col, row);
                // Only blocks wholly inside the 256 x rows picture.
                if px < 0 || px + 8 > 256 || py < i32::from(grid.rows_px.0) {
                    continue;
                }
                let want = grid.key_at(col, row).expect("cell exists");
                let at = keys.key_at_canvas(origin.0 + i64::from(px), origin.1 + i64::from(py));
                match at {
                    None => t.audit_unkeyed.fetch_add(1, Relaxed),
                    Some(k) if k == want => t.audit_equal.fetch_add(1, Relaxed),
                    Some(k) if k.pattern == want.pattern => {
                        t.audit_pattern_only.fetch_add(1, Relaxed)
                    }
                    Some(k) if k.palette == want.palette => {
                        t.audit_palette_only.fetch_add(1, Relaxed)
                    }
                    Some(_) => t.audit_different.fetch_add(1, Relaxed),
                };
            }
        }
    }

    /// Queue everything pending (the core thread is stopping).
    pub fn finish(&mut self, canvases: &mut CanvasModel) {
        self.leave_scene(canvases);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::{PixelLayer, PpuPixel};
    use rf_enhance::canvas::tag;
    use rf_enhance::heights::Entry;
    use rf_snes::debug::{tile_pixel, tilemap_entry};

    use crate::tile_capture::{snes_grids, SnesLayer, SnesSource};

    fn put_word(v: &mut [u8], word: u16, value: u16) {
        let at = usize::from(word) * 2;
        v[at] = value as u8;
        v[at + 1] = (value >> 8) as u8;
    }

    /// A mode 1 world of `cols` x 8 tilemap entries on layer 0 (4 bpp), each
    /// tile a different pattern, drawn with palette `pal`.
    struct World {
        vram: Vec<u8>,
        cgram: Vec<u16>,
    }

    fn world() -> World {
        let mut vram = vec![0u8; 0x10000];
        // Characters 1..=40: 4 bpp, 16 words each, from char base 0.
        for c in 1..=40u16 {
            for r in 0..8u16 {
                let seed = c.wrapping_mul(0x9E37).wrapping_add(r * 0x1F3);
                put_word(&mut vram, c * 16 + r, seed | 0x0101);
                put_word(&mut vram, c * 16 + 8 + r, seed.rotate_left(5));
            }
        }
        // 32 x 32 tilemap at word 0x4000: character 1 + (i % 40), palette
        // (i / 3) % 4, one flipped.
        for i in 0..1024u16 {
            let c = 1 + (i % 40);
            let pal = (i / 3) % 4;
            let flip_x = i % 7 == 0;
            let e = c | pal << 10 | if flip_x { 0x4000 } else { 0 };
            put_word(&mut vram, 0x4000 + i, e);
        }
        let cgram: Vec<u16> = (0..256u16)
            .map(|i| i.wrapping_mul(0x0421) ^ 0x1234)
            .collect();
        World { vram, cgram }
    }

    fn source<'a>(w: &'a World, hofs: u16, vofs: u16) -> SnesSource<'a> {
        SnesSource {
            vram: &w.vram,
            cgram: &w.cgram,
            mode: 1,
            layers: [
                SnesLayer {
                    tilemap_base: 0x4000,
                    tilemap_size: 0,
                    char_base: 0,
                    tile16: false,
                    hofs,
                    vofs,
                    on_main: true,
                    on_sub: false,
                },
                SnesLayer::default(),
                SnesLayer::default(),
                SnesLayer::default(),
            ],
            lines: &[],
            screen: (256, 224),
            hud: [false; 4],
        }
    }

    /// The picture the PPU would draw for the layer: CGRAM indices.
    fn stitch(w: &World, canvas: &mut Canvas, hofs: u16, vofs: u16, world_x: i64, world_y: i64) {
        let mut buf: Vec<Option<PpuPixel>> = Vec::new();
        for sy in 0..224u32 {
            buf.clear();
            for sx in 0..256u32 {
                let tx = (u32::from(hofs) + sx) & 0xFF;
                let ty = (u32::from(vofs) + sy) & 0xFF;
                let e = tilemap_entry(&w.vram, 0x4000, 0, (tx / 8) as u16, (ty / 8) as u16);
                let px = if e.flip_x { 7 - (tx & 7) } else { tx & 7 } as u16;
                let py = if e.flip_y { 7 - (ty & 7) } else { ty & 7 } as u16;
                let colour = tile_pixel(&w.vram, 0, e.character, px, py, 4);
                buf.push((colour != 0).then(|| {
                    tag(
                        PpuPixel {
                            palette_index: e.palette * 16 + colour,
                            layer: PixelLayer::Background(0),
                            sprite_id: None,
                            priority: 0,
                        },
                        Some(0),
                    )
                }));
            }
            canvas.blit_row(world_x, world_y + i64::from(sy), &buf);
        }
    }

    fn palettes(w: &World) -> PaletteTable {
        let mut t = PaletteTable::new();
        let words: [u16; 256] = w.cgram.clone().try_into().unwrap();
        assert_eq!(t.intern(&words), Some(0));
        t
    }

    #[test]
    fn a_stitched_frame_rekeys_to_the_keys_the_capture_gave_it() {
        let w = world();
        for (hofs, vofs) in [(0u16, 0u16), (13, 5), (251, 30)] {
            let grids = snes_grids(&source(&w, hofs, vofs));
            let g = &grids[0];
            // Where the capture's blocks fall on a canvas whose pixel
            // (world_x, world_y) is the picture's top-left: `grid_phase`.
            let (world_x, world_y) = (1000 + i64::from(hofs % 3), 400 + i64::from(vofs % 5));
            let mut canvas = Canvas::new();
            stitch(&w, &mut canvas, hofs, vofs, world_x, world_y);
            let phase = grid_phase((world_x, world_y), g.offset, g.rows_px.0);
            let fmt = LayerFormat::for_mode(1, 0, false).unwrap();
            let mut keys = LevelKeys::new(fmt, phase);
            keys.refresh(&canvas, &palettes(&w));
            let (mut checked, mut equal, mut open) = (0, 0, 0);
            for row in 0..g.rows {
                for col in 0..g.cols {
                    let (px, py) = g.cell_picture_origin(col, row);
                    // Only cells wholly inside the picture have all 64 dots.
                    if px < 0 || py < 0 || px + 8 > 256 || py + 8 > 224 {
                        continue;
                    }
                    let want = g.key_at(col, row).unwrap();
                    let (cx, cy) = (world_x + i64::from(px), world_y + i64::from(py));
                    let kc = ((cx - keys.origin().0) / CELL) as u32;
                    let kr = ((cy - keys.origin().1) / CELL) as u32;
                    checked += 1;
                    match keys.key(kc, kr) {
                        Some(k) if k == want => equal += 1,
                        Some(_) => panic!("({col},{row}) at scroll ({hofs},{vofs}): wrong key"),
                        None => open += 1,
                    }
                }
            }
            assert!(checked > 600, "{checked}");
            assert_eq!(equal + open, checked);
            assert!(
                equal * 100 >= checked * 99,
                "{equal}/{checked}, {open} unseen"
            );
        }
    }

    #[test]
    fn scout_cells_with_no_palette_id_are_read_under_the_layers_usual_palette() {
        let w = world();
        let fmt = LayerFormat::for_mode(1, 0, false).unwrap();
        let pals = palettes(&w);
        let mut tagged = Canvas::new();
        stitch(&w, &mut tagged, 0, 0, 0, 0);
        // The right half as the scout leaves it: the same dots, no id.
        let mut scouted = Canvas::new();
        for sy in 0..224i64 {
            let row: Vec<Option<PpuPixel>> = (0..256i64)
                .map(|x| {
                    tagged.get(x, sy).map(|mut p| {
                        if x >= 128 {
                            p.sprite_id = None;
                        }
                        p
                    })
                })
                .collect();
            scouted.blit_row(0, sy, &row);
        }
        let mut want = LevelKeys::new(fmt, (0, 0));
        want.refresh(&tagged, &pals);
        let mut got = LevelKeys::new(fmt, (0, 0));
        got.refresh(&scouted, &pals);
        let (mut checked, mut equal) = (0, 0);
        for r in 0..want.dims().1 {
            for c in 0..want.dims().0 {
                if let Some(k) = want.key(c, r) {
                    checked += 1;
                    equal += usize::from(got.key(c, r) == Some(k));
                }
            }
        }
        assert!(checked > 600);
        assert_eq!(
            equal, checked,
            "an id-less cell reads under the usual palette"
        );
        // And with no palette to fall back on at all, nothing is keyed.
        let mut bare = Canvas::new();
        for sy in 0..224i64 {
            let row: Vec<Option<PpuPixel>> = (0..256i64)
                .map(|x| {
                    tagged.get(x, sy).map(|mut p| {
                        p.sprite_id = None;
                        p
                    })
                })
                .collect();
            bare.blit_row(0, sy, &row);
        }
        let mut none = LevelKeys::new(fmt, (0, 0));
        none.refresh(&bare, &pals);
        assert_eq!(none.counts().0, 0);
    }

    #[test]
    fn the_heights_follow_the_table_and_blocks_with_holes_are_watched() {
        let w = world();
        let grids = snes_grids(&source(&w, 0, 0));
        let g = &grids[0];
        let mut canvas = Canvas::new();
        stitch(&w, &mut canvas, 0, 0, 0, 0);
        let fmt = LayerFormat::for_mode(1, 0, false).unwrap();
        let phase = grid_phase((0, 0), g.offset, g.rows_px.0);
        let mut keys = LevelKeys::new(fmt, phase);
        keys.refresh(&canvas, &palettes(&w));
        let mut table = TileHeightTable::new();
        let key = g.key_at(3, 3).unwrap();
        {
            let e = Entry {
                learned: Learned {
                    class: Class::Wall,
                    height_eighths: 8,
                    confidence: 3,
                },
                ..Entry::default()
            };
            assert!(table.insert(key, e));
        }
        let (hc, rep) = keys.heights(&table, &canvas).unwrap();
        assert!(rep.solid >= 1 && rep.solid == rep.drawn);
        // Every block with that key stands; nothing else does.
        let mut standing = 0;
        for r in 0..hc.rows {
            for c in 0..hc.cols {
                let raised = hc.get(c, r).unwrap().class == Class::Wall;
                assert_eq!(raised, keys.key(c, r) == Some(key), "({c},{r})");
                standing += usize::from(raised);
            }
        }
        assert_eq!(standing, rep.solid);
        // A tile below the draw level is flat.
        let mut weak = *table.get(&key).unwrap();
        weak.learned.confidence = 1;
        table.insert(key, weak);
        assert_eq!(keys.heights(&table, &canvas).unwrap().1.drawn, 0);
    }

    #[test]
    fn a_dot_seen_later_updates_the_key_of_an_open_block() {
        let w = world();
        let fmt = LayerFormat::for_mode(1, 0, false).unwrap();
        let mut full = Canvas::new();
        stitch(&w, &mut full, 0, 0, 0, 0);
        // The same picture with one dot of block (2, 2) never seen.
        let mut holey = Canvas::new();
        for sy in 0..224i64 {
            let row: Vec<Option<PpuPixel>> = (0..256i64)
                .map(|x| (!(x == 17 && sy == 19)).then(|| full.get(x, sy)).flatten())
                .collect();
            holey.blit_row(0, sy, &row);
        }
        let pals = palettes(&w);
        let mut keys = LevelKeys::new(fmt, (0, 0));
        keys.refresh(&holey, &pals);
        let before = keys.key(2, 2).unwrap();
        // The dot arrives; the block's key is the true one now.
        holey.blit_row(17, 19, &[full.get(17, 19)]);
        // Only if that dot is not transparent does the key move.
        let moved = keys.refresh(&holey, &pals);
        let mut fresh = LevelKeys::new(fmt, (0, 0));
        fresh.refresh(&full, &pals);
        assert_eq!(keys.key(2, 2), fresh.key(2, 2));
        assert_eq!(moved, before != fresh.key(2, 2).unwrap());
    }

    #[test]
    fn the_grid_grows_with_the_canvas_and_keeps_its_keys() {
        let w = world();
        let fmt = LayerFormat::for_mode(1, 0, false).unwrap();
        let pals = palettes(&w);
        let mut a = Canvas::new();
        stitch(&w, &mut a, 0, 0, 64, 64);
        let mut keys = LevelKeys::new(fmt, (0, 0));
        keys.refresh(&a, &pals);
        let k = keys.key(4, 4).unwrap();
        let at = (keys.origin().0 + 4 * CELL, keys.origin().1 + 4 * CELL);
        // More of the level to the left and above.
        stitch(&w, &mut a, 0, 0, 8, 16);
        keys.refresh(&a, &pals);
        let c = ((at.0 - keys.origin().0) / CELL) as u32;
        let r = ((at.1 - keys.origin().1) / CELL) as u32;
        assert!(keys.dims().0 as i64 * CELL >= 256 + 56);
        assert!(keys.key(c, r).is_some());
        let _ = k;
    }

    #[test]
    fn formats_for_the_modes_the_retiler_handles() {
        assert_eq!(LayerFormat::for_mode(1, 0, false).unwrap().depth, 4);
        assert_eq!(LayerFormat::for_mode(1, 2, false).unwrap().depth, 2);
        assert_eq!(LayerFormat::for_mode(0, 1, false).unwrap().palette_base, 32);
        assert!(LayerFormat::for_mode(1, 3, false).is_none());
        assert!(LayerFormat::for_mode(1, 0, true).is_none());
        assert!(LayerFormat::for_mode(7, 0, false).is_none());
    }

    fn model(root: &std::path::Path) -> CanvasModel {
        let cache = rf_cache::Cache::open(root, 64 << 20).unwrap();
        CanvasModel::with_cache("rom-w34-07".to_string(), Some(cache))
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("rf_level_heights_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn the_table_the_camera_call_and_a_scenes_heights_survive_a_session() {
        use rf_enhance::heights::Orientation;
        let root = temp_root("persist");
        let w = world();
        let g = &snes_grids(&source(&w, 0, 0))[0];
        let mut table = TileHeightTable::new();
        let e = Entry {
            learned: Learned {
                class: Class::Wall,
                height_eighths: 8,
                confidence: 3,
            },
            ..Entry::default()
        };
        assert!(table.insert(g.key_at(2, 2).unwrap(), e));
        let learner = HeightLearner::with_table(table.clone());
        let mut hc = HeightCanvas::new((16, 32), 4, 3, 8).unwrap();
        hc.set(1, 1, e.learned);

        let mut first = model(&root);
        let mut p = Persist::new("rom-w34-07".to_string());
        p.save_session(&mut first, &learner, Orientation::SideView, 1, true);
        p.save_scene(&mut first, 0xABCD, &hc);
        first.flush();
        drop(first);

        // Another session over the same cache root.
        let mut second = model(&root);
        let p = Persist::new("rom-w34-07".to_string());
        let t = p.load_table(&mut second).expect("the table came back");
        assert_eq!(t, table);
        assert_eq!(p.load_orientation(&mut second), Some(Orientation::SideView));
        assert_eq!(p.load_scene(&mut second, 0xABCD), Some(hc));
        assert!(
            p.load_scene(&mut second, 0xABCE).is_none(),
            "another scene is a miss"
        );
        // Another ROM sees nothing.
        let other = Persist::new("rom-other".to_string());
        assert!(other.load_table(&mut second).is_none());
        // A model with no cache neither stores nor finds.
        let mut none = CanvasModel::with_cache(String::new(), None);
        assert!(p.load_table(&mut none).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unchanged_table_is_not_queued_twice_and_noise_is_a_miss() {
        use rf_enhance::heights::Orientation;
        let root = temp_root("noise");
        let learner = HeightLearner::new();
        let mut m = model(&root);
        let mut p = Persist::new("rom-w34-07".to_string());
        // An empty table is not worth a cache entry.
        p.save_session(&mut m, &learner, Orientation::Unknown, 1, true);
        m.flush();
        assert!(p.load_table(&mut m).is_none());
        assert!(p.load_orientation(&mut m).is_none());
        // A corrupt entry reads as a miss, not a panic.
        m.queue_extra(table_cache_key("rom-w34-07"), vec![1, 2, 3]);
        m.flush();
        assert!(p.load_table(&mut m).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tracker_turns_a_stitched_scene_into_heights_and_restores_them_next_time() {
        let root = temp_root("tracker");
        let w = world();
        let g = &snes_grids(&source(&w, 0, 0))[0];
        let mut table = TileHeightTable::new();
        let e = Entry {
            learned: Learned {
                class: Class::Wall,
                height_eighths: 8,
                confidence: 3,
            },
            ..Entry::default()
        };
        let key = g.key_at(3, 3).unwrap();
        assert!(table.insert(key, e));
        let learner = HeightLearner::with_table(table);
        let fmt = LayerFormat::for_mode(1, 0, false).unwrap();
        let phase = grid_phase((0, 0), g.offset, g.rows_px.0);

        // First visit: the scene is stitched into the model, the tracker
        // reads its heights off it.
        let mut live = model(&root);
        let scene = {
            // Put the picture on scene 77's layer 0 the way the model would.
            let mut c = Canvas::new();
            stitch(&w, &mut c, 0, 0, 0, 0);
            live.seed_scene_for_test(
                77,
                [c, Canvas::new(), Canvas::new(), Canvas::new()],
                palettes(&w),
            );
            77
        };
        let mut tracker = LevelTracker::new("rom-w34-07".to_string());
        let lh = tracker
            .observe(
                0,
                scene,
                Some((fmt, phase)),
                Some((g, (0, 0))),
                &mut live,
                &learner,
            )
            .expect("heights on the first pass");
        assert!(!lh.restored && lh.report.solid >= 1);
        assert_eq!(lh.canvas.drawn_count(), lh.report.drawn);
        // Nothing changed: nothing new to send, and no pass before PASS_EVERY.
        assert!(tracker
            .observe(
                PASS_EVERY,
                scene,
                Some((fmt, phase)),
                Some((g, (0, 0))),
                &mut live,
                &learner
            )
            .is_none());
        // Leaving the scene queues it; the model writes it.
        tracker.finish(&mut live);
        live.flush();
        drop(live);

        // Second visit: no canvas yet, no learner experience, and the heights
        // are there on the very first call.
        let mut next = model(&root);
        let mut tracker = LevelTracker::new("rom-w34-07".to_string());
        let again = tracker
            .observe(0, scene, None, None, &mut next, &HeightLearner::new())
            .expect("restored heights on the first frame");
        assert!(again.restored);
        assert_eq!(again.canvas, lh.canvas);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_grid_a_line_off_the_canvas_is_found_by_the_vote_and_corrected() {
        let w = world();
        let g = &snes_grids(&source(&w, 0, 0))[0];
        let mut table = TileHeightTable::new();
        let e = Entry {
            learned: Learned {
                class: Class::Wall,
                height_eighths: 8,
                confidence: 3,
            },
            ..Entry::default()
        };
        let key = g.key_at(3, 3).unwrap();
        assert!(table.insert(key, e));
        let learner = HeightLearner::with_table(table);
        let fmt = LayerFormat::for_mode(1, 0, false).unwrap();
        let mut c = Canvas::new();
        stitch(&w, &mut c, 0, 0, 0, 0);
        let mut live = CanvasModel::new();
        live.seed_scene_for_test(
            5,
            [c, Canvas::new(), Canvas::new(), Canvas::new()],
            palettes(&w),
        );
        // The geometry says the picture starts one line lower than the canvas
        // has it (and one dot to the right).
        let said = (1, 1);
        let phase = grid_phase(said, g.offset, g.rows_px.0);
        let mut tracker = LevelTracker::new("rom".to_string());
        let lh = tracker
            .observe(
                0,
                5,
                Some((fmt, phase)),
                Some((g, said)),
                &mut live,
                &learner,
            )
            .expect("heights once the vote has decided");
        // Reading the canvas one dot and one line before where the geometry
        // said puts the blocks back on the capture's grid.
        assert_eq!(tracker.calibration.shift(), Some((-1, -1)));
        assert!(lh.report.solid >= 1, "{:?}", lh.report);
        // Without the correction nothing would have matched.
        let mut off = LevelKeys::new(fmt, phase);
        let layers = live.scene_canvases(5).unwrap();
        off.refresh(&layers[0], live.scene_palettes(5).unwrap());
        assert_eq!(off.heights(learner.table(), &layers[0]).unwrap().1.drawn, 0);
    }

    #[test]
    fn the_remembered_orientation_round_trips_and_rejects_noise() {
        use rf_enhance::heights::Orientation as O;
        for o in [O::SideView, O::TopDown] {
            assert_eq!(orientation_from(&orientation_bytes(o).unwrap()), Some(o));
        }
        assert!(orientation_bytes(O::Unknown).is_none());
        assert!(orientation_from(&[]).is_none());
        assert!(orientation_from(&[1, 9]).is_none());
        assert!(orientation_from(&[2, 1]).is_none());
        assert!(orientation_from(&[1, 1, 0]).is_none());
    }
}
