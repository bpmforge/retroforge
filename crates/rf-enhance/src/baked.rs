//! Baked level geometry (ticket W39-03; `docs/design/REBUILD_LAB.md`
//! section 9, "baked level geometry").
//!
//! A *stage set* is, per scouted screen, the scout's stitched map footprint
//! joined with per-tile solidity: a height/solid grid plus extruded wall
//! quads for the HD-2D renderer. It is stored in `rf-cache` under the ROM's
//! hash beside the scout's scene entries (`persistence`), so a game's level
//! geometry is built once and reloaded.
//!
//! Solidity comes from two places, facts first:
//!
//! * **Collision facts** (W38-03 [`TileRule`] ranges, found by
//!   `rf_rebuild::collision`): when a screen has rules, every observed id is
//!   solid, one-way or open by them.
//! * **Painted heights** (the profile's `[heights.tiles]`, character number to
//!   eighths): used where there are no rules, and for the extrusion height of
//!   a solid cell when the painter set one.
//!
//! A cell the scout never saw, or whose id was not observed, stays
//! [`Kind::Unknown`] and is never extruded (NON_GOALS #11: no invented
//! geometry). Only ids, offsets and counts are stored (law 5): no pixels,
//! no ROM bytes. `rf-enhance` may not depend on `rf-rebuild`
//! (validate-arch), so the rules arrive as plain ranges; the mediator
//! converts `TileSemantics` rows.

use std::collections::BTreeMap;
use std::io::Read;

use rf_cache::{Cache, CacheError, CacheKey};

use crate::persistence::Reader;
use crate::stitcher::Canvas;

/// Cache-key version tag.
pub const SETTINGS_HASH: &str = "baked-geometry-v1";
/// Cache-key producer.
pub const PRODUCER: &str = "rf-enhance-baked";
/// Cache-key asset: one entry per ROM holds its whole stage set.
pub const ASSET: &str = "stage-set";
/// Format version inside the file.
pub const VERSION: u16 = 1;
/// Most cells a stage set may hold across all screens.
pub const MAX_CELLS: u64 = 16 * 1024 * 1024;
/// Most screens a stage set may hold.
pub const MAX_SCREENS: usize = 4096;
/// The extrusion of a solid cell nothing painted (one tile edge, eighths).
pub const DEFAULT_SOLID_EIGHTHS: u8 = 8;

const MAGIC: &[u8; 4] = b"RFBG";
const ZSTD_LEVEL: i32 = 3;
const HEADER_LEN: usize = 4 + 2 + 4;
const FOOTER_LEN: usize = 4;
const CELL_LEN: usize = 3;
const QUAD_LEN: usize = 1 + 4 + 4 + 4 + 1;
const SCREEN_FIXED_LEN: usize = 4 + 8 + 8 + 4 + 4 + 1 + 4;

/// What a rule range says its ids are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleClass {
    Solid,
    OneWay,
    /// Water, hazard: walkable space, nothing to extrude.
    Open,
}

/// One inclusive id range of collision facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRule {
    pub first: u32,
    pub last: u32,
    pub class: RuleClass,
}

/// What a cell is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// Never seen, or no id observed: drawn flat.
    Unknown = 0,
    Open = 1,
    Solid = 2,
    OneWay = 3,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Unknown,
            1 => Self::Open,
            2 => Self::Solid,
            3 => Self::OneWay,
            _ => return None,
        })
    }

    /// Solid or one-way: has a body to extrude.
    #[must_use]
    pub fn is_body(self) -> bool {
        matches!(self, Self::Solid | Self::OneWay)
    }
}

/// Where a cell's verdict came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Source {
    None = 0,
    /// Found collision rules.
    Facts = 1,
    /// The profile's painted heights.
    Painted = 2,
}

impl Source {
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::None,
            1 => Self::Facts,
            2 => Self::Painted,
            _ => return None,
        })
    }
}

/// One baked cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub kind: Kind,
    pub source: Source,
    /// Extrusion in eighths of the cell edge (0 for non-bodies).
    pub height_eighths: u8,
}

const UNKNOWN: Cell = Cell {
    kind: Kind::Unknown,
    source: Source::None,
    height_eighths: 0,
};

/// Which side of a body cell a quad stands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Face {
    /// North edge (row above is not a body).
    Top = 0,
    Bottom = 1,
    Left = 2,
    Right = 3,
}

impl Face {
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Top,
            1 => Self::Bottom,
            2 => Self::Left,
            3 => Self::Right,
            _ => return None,
        })
    }
}

/// An extruded wall: `len` cells of one face starting at `(col, row)`, going
/// right for Top/Bottom and down for Left/Right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quad {
    pub face: Face,
    pub col: u32,
    pub row: u32,
    pub len: u32,
    pub height_eighths: u8,
}

/// What the baker is given for one screen.
#[derive(Debug, Clone)]
pub struct ScreenInput<'a> {
    /// The scene or screen id the scout filed it under.
    pub id: u32,
    /// World position of cell (0, 0)'s top-left, in pixels.
    pub origin: (i64, i64),
    pub cols: u32,
    pub rows: u32,
    pub cell_px: u8,
    /// Block or character id per cell (row-major, `cols * rows`); `None`:
    /// not observed.
    pub ids: &'a [Option<u32>],
    /// Cells the scout's map has pixels for; `None`: trust `ids` alone.
    pub visited: Option<&'a [bool]>,
}

/// The solidity sources.
#[derive(Debug, Clone, Copy, Default)]
pub struct Solidity<'a> {
    pub rules: &'a [TileRule],
    /// Painted heights: id to eighths.
    pub painted: Option<&'a BTreeMap<u32, u32>>,
}

/// One baked screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BakedScreen {
    pub id: u32,
    pub origin: (i64, i64),
    pub cols: u32,
    pub rows: u32,
    pub cell_px: u8,
    pub cells: Vec<Cell>,
    pub quads: Vec<Quad>,
}

impl BakedScreen {
    #[must_use]
    pub fn cell(&self, col: u32, row: u32) -> Option<Cell> {
        (col < self.cols && row < self.rows)
            .then(|| self.cells[row as usize * self.cols as usize + col as usize])
    }

    /// Cells that are solid or one-way.
    #[must_use]
    pub fn body_count(&self) -> usize {
        self.cells.iter().filter(|c| c.kind.is_body()).count()
    }
}

/// A ROM's baked stages.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StageSet {
    pub screens: Vec<BakedScreen>,
}

/// The cell footprint of a scout canvas: which cells hold scouted pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footprint {
    /// World position (px) of cell (0, 0)'s top-left.
    pub origin: (i64, i64),
    pub cols: u32,
    pub rows: u32,
    /// Row-major, `cols * rows`: the cell has at least one scouted pixel.
    pub visited: Vec<bool>,
}

/// The cells of `canvas` (cut into `cell_px` squares on the world grid) that
/// hold at least one scouted pixel. `None` for an empty canvas or a zero
/// `cell_px`.
#[must_use]
pub fn visited_cells(canvas: &Canvas, cell_px: u8) -> Option<Footprint> {
    let cp = i64::from(cell_px);
    let (w, h) = (canvas.width() as i64, canvas.height() as i64);
    if cp == 0 || w == 0 || h == 0 {
        return None;
    }
    let (ox, oy) = canvas.origin();
    let (cx0, cy0) = (ox.div_euclid(cp), oy.div_euclid(cp));
    let cols = (ox + w - 1).div_euclid(cp) - cx0 + 1;
    let rows = (oy + h - 1).div_euclid(cp) - cy0 + 1;
    if cols * rows > MAX_CELLS as i64 {
        return None;
    }
    let mut visited = vec![false; (cols * rows) as usize];
    let cells = canvas.cells();
    for y in 0..h {
        let row = (oy + y).div_euclid(cp) - cy0;
        for x in 0..w {
            if cells[(y * w + x) as usize].is_some() {
                let col = (ox + x).div_euclid(cp) - cx0;
                visited[(row * cols + col) as usize] = true;
            }
        }
    }
    Some(Footprint {
        origin: (cx0 * cp, cy0 * cp),
        cols: cols as u32,
        rows: rows as u32,
        visited,
    })
}

/// Bake the screen a scout canvas covers: the grid is the canvas's own
/// [`visited_cells`] footprint (its origin, width and height on the world
/// grid), never an assumed screen width, and a cell the canvas holds no pixel
/// for stays [`Kind::Unknown`]. `id_at(col, row)` gives the block or character
/// id at a world cell (`col = floor(world_x / cell_px)`), `None` when the game
/// state does not say. `None` when the canvas is empty or too large.
#[must_use]
pub fn bake_canvas(
    id: u32,
    canvas: &Canvas,
    cell_px: u8,
    id_at: impl Fn(i64, i64) -> Option<u32>,
    sol: &Solidity<'_>,
) -> Option<BakedScreen> {
    let fp = visited_cells(canvas, cell_px)?;
    let cp = i64::from(cell_px);
    let (c0, r0) = (fp.origin.0.div_euclid(cp), fp.origin.1.div_euclid(cp));
    let ids: Vec<Option<u32>> = (0..i64::from(fp.rows))
        .flat_map(|r| (0..i64::from(fp.cols)).map(move |c| (c, r)))
        .map(|(c, r)| id_at(c0 + c, r0 + r))
        .collect();
    bake_screen(
        &ScreenInput {
            id,
            origin: fp.origin,
            cols: fp.cols,
            rows: fp.rows,
            cell_px,
            ids: &ids,
            visited: Some(&fp.visited),
        },
        sol,
    )
}

fn judge(id: u32, sol: &Solidity<'_>) -> Cell {
    if !sol.rules.is_empty() {
        let class = sol
            .rules
            .iter()
            .find(|r| (r.first..=r.last).contains(&id))
            .map(|r| r.class);
        let (kind, body) = match class {
            Some(RuleClass::Solid) => (Kind::Solid, true),
            Some(RuleClass::OneWay) => (Kind::OneWay, true),
            Some(RuleClass::Open) | None => (Kind::Open, false),
        };
        let painted = sol
            .painted
            .and_then(|p| p.get(&id))
            .copied()
            .filter(|&h| h > 0);
        let height_eighths = if body {
            painted.map_or(DEFAULT_SOLID_EIGHTHS, |h| h.min(255) as u8)
        } else {
            0
        };
        return Cell {
            kind,
            source: Source::Facts,
            height_eighths,
        };
    }
    match sol.painted.and_then(|p| p.get(&id)) {
        Some(&h) if h > 0 => Cell {
            kind: Kind::Solid,
            source: Source::Painted,
            height_eighths: h.min(255) as u8,
        },
        Some(_) => Cell {
            kind: Kind::Open,
            source: Source::Painted,
            height_eighths: 0,
        },
        None => UNKNOWN,
    }
}

/// Bake one screen. `None` when the input is inconsistent (`ids` or
/// `visited` not `cols * rows` long, zero `cell_px`, or too large).
#[must_use]
pub fn bake_screen(input: &ScreenInput<'_>, sol: &Solidity<'_>) -> Option<BakedScreen> {
    let n = u64::from(input.cols).checked_mul(u64::from(input.rows))?;
    if n > MAX_CELLS || input.ids.len() as u64 != n || input.cell_px == 0 {
        return None;
    }
    if input.visited.is_some_and(|v| v.len() as u64 != n) {
        return None;
    }
    let cells: Vec<Cell> = input
        .ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let seen = input.visited.is_none_or(|v| v[i]);
            match id {
                Some(id) if seen => judge(*id, sol),
                _ => UNKNOWN,
            }
        })
        .collect();
    let quads = extrude(&cells, input.cols, input.rows);
    Some(BakedScreen {
        id: input.id,
        origin: input.origin,
        cols: input.cols,
        rows: input.rows,
        cell_px: input.cell_px,
        cells,
        quads,
    })
}

/// Wall quads for every body edge whose neighbour is not a body (off the
/// grid counts as not), merged along the edge while the height is equal.
fn extrude(cells: &[Cell], cols: u32, rows: u32) -> Vec<Quad> {
    let (c, r) = (cols as usize, rows as usize);
    let body = |x: i64, y: i64| -> Option<u8> {
        if x < 0 || y < 0 || x >= c as i64 || y >= r as i64 {
            return None;
        }
        let cell = cells[y as usize * c + x as usize];
        cell.kind.is_body().then_some(cell.height_eighths)
    };
    let mut out = Vec::new();
    for face in [Face::Top, Face::Bottom, Face::Left, Face::Right] {
        let horizontal = matches!(face, Face::Top | Face::Bottom);
        let (dx, dy) = match face {
            Face::Top => (0, -1),
            Face::Bottom => (0, 1),
            Face::Left => (-1, 0),
            Face::Right => (1, 0),
        };
        // `line` runs across the faces' normal, `at` along the edge.
        let (lines, span) = if horizontal { (r, c) } else { (c, r) };
        for line in 0..lines {
            let mut at = 0usize;
            while at < span {
                let (x, y) = if horizontal { (at, line) } else { (line, at) };
                let exposed = body(x as i64, y as i64)
                    .filter(|_| body(x as i64 + dx, y as i64 + dy).is_none());
                let Some(height) = exposed else {
                    at += 1;
                    continue;
                };
                let start = at;
                at += 1;
                while at < span {
                    let (x, y) = if horizontal { (at, line) } else { (line, at) };
                    let same = body(x as i64, y as i64) == Some(height)
                        && body(x as i64 + dx, y as i64 + dy).is_none();
                    if !same {
                        break;
                    }
                    at += 1;
                }
                let (col, row) = if horizontal {
                    (start, line)
                } else {
                    (line, start)
                };
                out.push(Quad {
                    face,
                    col: col as u32,
                    row: row as u32,
                    len: (at - start) as u32,
                    height_eighths: height,
                });
            }
        }
    }
    out
}

impl StageSet {
    /// Total cells across screens.
    #[must_use]
    pub fn cell_count(&self) -> usize {
        self.screens.iter().map(|s| s.cells.len()).sum()
    }

    /// Total body cells across screens.
    #[must_use]
    pub fn body_count(&self) -> usize {
        self.screens.iter().map(BakedScreen::body_count).sum()
    }

    /// Total quads across screens.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.screens.iter().map(|s| s.quads.len()).sum()
    }

    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(self.screens.len() as u32).to_le_bytes());
        for s in &self.screens {
            body.extend_from_slice(&s.id.to_le_bytes());
            body.extend_from_slice(&s.origin.0.to_le_bytes());
            body.extend_from_slice(&s.origin.1.to_le_bytes());
            body.extend_from_slice(&s.cols.to_le_bytes());
            body.extend_from_slice(&s.rows.to_le_bytes());
            body.push(s.cell_px);
            body.extend_from_slice(&(s.quads.len() as u32).to_le_bytes());
            for c in &s.cells {
                body.extend_from_slice(&[c.kind as u8, c.source as u8, c.height_eighths]);
            }
            for q in &s.quads {
                body.push(q.face as u8);
                body.extend_from_slice(&q.col.to_le_bytes());
                body.extend_from_slice(&q.row.to_le_bytes());
                body.extend_from_slice(&q.len.to_le_bytes());
                body.push(q.height_eighths);
            }
        }
        let mut out = Vec::with_capacity(HEADER_LEN + body.len() / 4 + FOOTER_LEN);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&zstd::bulk::compress(&body, ZSTD_LEVEL).unwrap_or_default());
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// # Errors
    /// A message naming what was wrong; the caller treats it as a miss.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_LEN + FOOTER_LEN {
            return Err(format!("truncated ({} bytes)", bytes.len()));
        }
        let (signed, footer) = bytes.split_at(bytes.len() - FOOTER_LEN);
        let want = u32::from_le_bytes([footer[0], footer[1], footer[2], footer[3]]);
        if crc32fast::hash(signed) != want {
            return Err("crc mismatch (truncated or corrupt)".to_string());
        }
        let mut r = Reader::new(signed);
        if r.take(4) != Some(MAGIC.as_slice()) {
            return Err("bad magic".to_string());
        }
        if r.u16() != Some(VERSION) {
            return Err("unknown format version".to_string());
        }
        let raw_len = r.u32().ok_or("short header")? as usize;
        let max_raw =
            4 + MAX_SCREENS * SCREEN_FIXED_LEN + MAX_CELLS as usize * (CELL_LEN + 4 * QUAD_LEN);
        if raw_len > max_raw {
            return Err(format!("claims {raw_len} body bytes (cap {max_raw})"));
        }
        let compressed = r.take(r.remaining()).ok_or("short body")?;
        let decoder = zstd::Decoder::new(compressed).map_err(|e| e.to_string())?;
        let mut body = Vec::new();
        decoder
            .take(raw_len as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| e.to_string())?;
        if body.len() != raw_len {
            return Err(format!(
                "body is {} bytes, header says {raw_len}",
                body.len()
            ));
        }
        Self::parse(&body).ok_or_else(|| "body does not parse".to_string())
    }

    fn parse(body: &[u8]) -> Option<Self> {
        let mut r = Reader::new(body);
        let n = r.u32()? as usize;
        if n > MAX_SCREENS {
            return None;
        }
        let mut screens = Vec::with_capacity(n.min(64));
        let mut total_cells = 0u64;
        for _ in 0..n {
            let id = r.u32()?;
            let origin = (r.i64()?, r.i64()?);
            let (cols, rows, cell_px) = (r.u32()?, r.u32()?, r.u8()?);
            let quad_n = r.u32()? as usize;
            let cells_n = u64::from(cols).checked_mul(u64::from(rows))?;
            total_cells += cells_n;
            let need = cells_n.checked_mul(CELL_LEN as u64)?
                + (quad_n as u64).checked_mul(QUAD_LEN as u64)?;
            if total_cells > MAX_CELLS || cell_px == 0 || need > r.remaining() as u64 {
                return None;
            }
            let mut cells = Vec::with_capacity(cells_n as usize);
            for _ in 0..cells_n {
                cells.push(Cell {
                    kind: Kind::from_u8(r.u8()?)?,
                    source: Source::from_u8(r.u8()?)?,
                    height_eighths: r.u8()?,
                });
            }
            let mut quads = Vec::with_capacity(quad_n);
            for _ in 0..quad_n {
                let q = Quad {
                    face: Face::from_u8(r.u8()?)?,
                    col: r.u32()?,
                    row: r.u32()?,
                    len: r.u32()?,
                    height_eighths: r.u8()?,
                };
                let (ec, er) = if matches!(q.face, Face::Top | Face::Bottom) {
                    (q.col.checked_add(q.len)?, q.row.checked_add(1)?)
                } else {
                    (q.col.checked_add(1)?, q.row.checked_add(q.len)?)
                };
                if q.len == 0 || ec > cols || er > rows {
                    return None;
                }
                quads.push(q);
            }
            screens.push(BakedScreen {
                id,
                origin,
                cols,
                rows,
                cell_px,
                cells,
                quads,
            });
        }
        (r.remaining() == 0).then_some(Self { screens })
    }
}

/// The cache key of a ROM's stage set: the ROM hash the scout's scene
/// entries (`persistence::scene_cache_key`) are keyed by, so both live in
/// one cache root and are evicted and cleared together.
#[must_use]
pub fn stage_set_key(rom_sha256: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: ASSET.to_string(),
        producer: PRODUCER.to_string(),
        settings_hash: SETTINGS_HASH.to_string(),
    }
}

/// Persist a ROM's stage set.
///
/// # Errors
/// Propagates [`CacheError`] from `Cache::put`.
pub fn save_stage_set(
    cache: &mut Cache,
    rom_sha256: &str,
    set: &StageSet,
) -> Result<usize, CacheError> {
    let bytes = set.to_bytes();
    cache.put(&stage_set_key(rom_sha256), &bytes)?;
    Ok(bytes.len())
}

/// Restore a ROM's stage set. A missing, foreign, truncated or corrupt entry
/// is `Ok(None)` with one log line: the set is regenerable by baking again.
///
/// # Errors
/// Propagates [`CacheError`] from `Cache::get` (an I/O or containment fault).
pub fn load_stage_set(cache: &mut Cache, rom_sha256: &str) -> Result<Option<StageSet>, CacheError> {
    let Some(bytes) = cache.get(&stage_set_key(rom_sha256))? else {
        return Ok(None);
    };
    match StageSet::from_bytes(&bytes) {
        Ok(s) => Ok(Some(s)),
        Err(why) => {
            eprintln!("rf-enhance: baked-geometry cache: ignoring entry: {why}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests;
