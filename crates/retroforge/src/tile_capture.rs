//! Per-frame tile grids from the PPUs (ticket W34-01; `docs/design/
//! HD2D_PLAN.md` section 2.3).
//!
//! `rf-enhance` may not name a console, so the shell reads each PPU's
//! background state and hands `rf_enhance::heights::TileGrid`s over: this is
//! that mediator. It runs on the core thread behind
//! `CoreCommand::SetTileGridCapture` (off unless Diorama / HD-2D is on) and
//! only READS the machine (law 4): a snapshot of VRAM and the registers on
//! the SNES, the tiles the PPU reports it drew on the NES.
//!
//! ## SNES
//!
//! Per background layer, one grid per run of picture lines whose scroll is
//! the same (`FrameBundle::lines` carries each line's registers, W33-01), so
//! a status bar or an HDMA parallax band is positioned by the scroll it was
//! really drawn with (HD2D_PLAN B10). Cells are the layer's own 8x8 or 16x16
//! characters; the key is a hash of the pattern bytes read out of VRAM and of
//! the palette's colours out of CGRAM, so reloading VRAM between levels does
//! not renumber anything (B7). Modes 5 and 6 (hires) and 7 (no tilemap) give
//! no grids, and modes 2 and 4 (offset-per-tile) give grids marked inexact.
//! `character_grid` in `live_heights` is the oracle: for a frame with one
//! scroll the cells here are exactly that grid's characters.
//!
//! ## NES
//!
//! The PPU reports the tiles it fetched, with the nametable address each
//! came from (`rf_nes::ppu::DrawnTile`). Tiles are grouped into rows by
//! their top edge and rows into bands by their scroll, then paired into
//! 16x16 metatiles on the 2x2 alignment the attribute table uses, so the cell
//! is the unit NES games build levels from. The pattern bytes are read from
//! the CHR window as it stands when the frame ends, so a bank switch part-way
//! down the picture hashes the later bank for the earlier rows.

use std::collections::BTreeMap;

use rf_core_api::LineRegisters;
use rf_enhance::heights::{Fnv64, TileCell, TileGrid};
use rf_snes::debug::{tilemap_entry, vram_word};

/// A layer whose scroll changes on more lines than this many runs is
/// described by one inexact band instead of a swarm of one-line grids.
pub const MAX_BANDS: usize = 16;

/// Scroll registers are 10 bits.
const SCROLL_MASK: u16 = 0x03FF;

// ---------------------------------------------------------------------
// SNES
// ---------------------------------------------------------------------

/// One background layer's registers as the PPU holds them when the frame
/// ends (the per-line values come from `SnesSource::lines`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SnesLayer {
    /// Tilemap base, in words.
    pub tilemap_base: u16,
    /// `$2107` size code, 0-3.
    pub tilemap_size: u8,
    /// Character data base, in words.
    pub char_base: u16,
    /// 16x16 characters (`$2105` bits 4-7), else 8x8.
    pub tile16: bool,
    pub hofs: u16,
    pub vofs: u16,
    /// On the main screen when the frame ended.
    pub on_main: bool,
    /// On the sub screen when the frame ended.
    pub on_sub: bool,
}

/// Everything the SNES grids are read from. Borrowed from the PPU; nothing
/// here is mutated.
pub struct SnesSource<'a> {
    /// 64 KiB.
    pub vram: &'a [u8],
    /// 256 BGR555 words.
    pub cgram: &'a [u16],
    /// `$2105` bits 0-2.
    pub mode: u8,
    pub layers: [SnesLayer; 4],
    /// Per picture line registers; empty when the bundle did not capture
    /// them (the frame-end values then stand for every line).
    pub lines: &'a [Option<LineRegisters>],
    /// Picture size in pixels: width 256, the visible line count.
    pub screen: (u32, u32),
    /// Per layer: judged a HUD by the canvas model.
    pub hud: [bool; 4],
}

/// The tile's four flips: bit 0 mirrors horizontally, bit 1 vertically.
const FLIPS: [(bool, bool); 4] = [(false, false), (true, false), (false, true), (true, true)];

#[derive(Clone, Copy)]
struct TileHash {
    /// As stored in VRAM.
    raw: u64,
    /// The smallest of the four flipped hashes.
    canon: u64,
    /// Flips (bit 0 horizontal, bit 1 vertical) that turn the stored tile
    /// into the canonical one.
    to_canon: u8,
    /// Every pixel is colour 0 (transparent): nothing to see, so nothing to
    /// hide behind or stand on.
    blank: bool,
}

/// Hash one character's pattern in each of its four orientations. The bytes
/// are the bit planes exactly as `rf_snes::debug::tile_pixel` reads them:
/// planes 0/1 of row `r` at word `+r`, planes 2/3 at `+8+r`, and so on.
fn hash_tile(vram: &[u8], char_base: u16, character: u16, depth: u8) -> TileHash {
    let words = u16::from(depth) * 4;
    let tile_word = char_base.wrapping_add(character.wrapping_mul(words));
    let depth = usize::from(depth);
    // 8 rows x `depth` plane bytes, row-major.
    let mut rows = [0u8; 64];
    for r in 0..8u16 {
        for pair in 0..depth / 2 {
            let w = vram_word(
                vram,
                tile_word.wrapping_add(pair as u16 * 8).wrapping_add(r),
            );
            rows[usize::from(r) * depth + pair * 2] = w as u8;
            rows[usize::from(r) * depth + pair * 2 + 1] = (w >> 8) as u8;
        }
    }
    let hash = |fx: bool, fy: bool| -> u64 {
        let mut h = Fnv64::new();
        h.bytes(&[depth as u8]);
        for r in 0..8 {
            let src = if fy { 7 - r } else { r };
            for &b in &rows[src * depth..src * depth + depth] {
                h.bytes(&[if fx { b.reverse_bits() } else { b }]);
            }
        }
        h.finish()
    };
    let mut best = (hash(false, false), 0u8);
    let raw = best.0;
    for (i, &(fx, fy)) in FLIPS.iter().enumerate().skip(1) {
        let v = hash(fx, fy);
        if v < best.0 {
            best = (v, i as u8);
        }
    }
    TileHash {
        raw,
        canon: best.0,
        to_canon: best.1,
        blank: rows[..8 * depth].iter().all(|&b| b == 0),
    }
}

/// Hash of the colours a palette selection shows (colour 0 is transparent
/// and never shown, so it is left out), folded to 16 bits.
fn palette_hash(cgram: &[u16], first: usize, count: usize) -> u16 {
    let mut h = Fnv64::new();
    for k in 1..count {
        let c = cgram.get((first + k) & 0xFF).copied().unwrap_or(0);
        h.bytes(&c.to_le_bytes());
    }
    h.finish16()
}

/// A run of picture lines over which one layer is shown with one scroll.
struct Run {
    y0: u32,
    y1: u32,
    hofs: u16,
    vofs: u16,
}

fn runs_for(layer: usize, src: &SnesSource<'_>) -> (Vec<Run>, bool) {
    let h = src.screen.1;
    let own = src.layers[layer];
    let mut runs: Vec<Run> = Vec::new();
    for y in 0..h {
        let (on, hofs, vofs) = match src.lines.get(y as usize).and_then(Option::as_ref) {
            Some(l) => (
                (l.tm | l.ts) & (1 << layer) != 0,
                l.hofs[layer] & SCROLL_MASK,
                l.vofs[layer] & SCROLL_MASK,
            ),
            None => (
                own.on_main || own.on_sub,
                own.hofs & SCROLL_MASK,
                own.vofs & SCROLL_MASK,
            ),
        };
        if !on {
            continue;
        }
        match runs.last_mut() {
            Some(r) if r.y1 == y && r.hofs == hofs && r.vofs == vofs => r.y1 = y + 1,
            _ => runs.push(Run {
                y0: y,
                y1: y + 1,
                hofs,
                vofs,
            }),
        }
    }
    if runs.len() > MAX_BANDS {
        let lo = runs.first().map_or(0, |r| r.y0);
        let hi = runs.last().map_or(h, |r| r.y1);
        return (
            vec![Run {
                y0: lo,
                y1: hi,
                hofs: own.hofs & SCROLL_MASK,
                vofs: own.vofs & SCROLL_MASK,
            }],
            false,
        );
    }
    (runs, true)
}

/// The tile grids of every background layer shown this frame. Empty in the
/// hires and Mode 7 modes (no tilemap to read), and for a layer the mode does
/// not have.
#[must_use]
pub fn snes_grids(src: &SnesSource<'_>) -> Vec<TileGrid> {
    if src.mode > 4 || src.vram.len() < 0x10000 || src.screen.0 == 0 || src.screen.1 == 0 {
        return Vec::new();
    }
    let depths = rf_snes::ppu::bg::bit_depths(src.mode);
    let opt_mode = rf_snes::ppu::bg::uses_offset_per_tile(src.mode);
    let mut out = Vec::new();
    for (layer, &depth) in depths.iter().enumerate() {
        if depth == 0 {
            continue;
        }
        let (runs, banded_exactly) = runs_for(layer, src);
        if runs.is_empty() {
            continue;
        }
        let regs = src.layers[layer];
        let cell_px: u32 = if regs.tile16 { 16 } else { 8 };
        let palette_first = |pal: u8| -> (usize, usize) {
            let n = if depth == 2 { 4 } else { 16 };
            let base = if src.mode == 0 { layer * 32 } else { 0 };
            (base + usize::from(pal) * n, n)
        };
        // 8 bpp ignores the tilemap's palette field: one hash of all 256.
        let all_colours = (depth == 8).then(|| palette_hash(src.cgram, 0, 256));
        let mut tiles: Vec<Option<TileHash>> = vec![None; 1024];
        let mut pals: [Option<u16>; 8] = [None; 8];
        for run in runs {
            let off = (
                u32::from(run.hofs) % cell_px,
                (u32::from(run.vofs) + run.y0) % cell_px,
            );
            let cols = (src.screen.0 + off.0).div_ceil(cell_px);
            let rows = (run.y1 - run.y0 + off.1).div_ceil(cell_px);
            let world_x0 = u32::from(run.hofs) - off.0;
            let world_y0 = u32::from(run.vofs) + run.y0 - off.1;
            let mut cells = Vec::with_capacity((cols * rows) as usize);
            for row in 0..rows {
                for col in 0..cols {
                    let e = tilemap_entry(
                        src.vram,
                        regs.tilemap_base,
                        regs.tilemap_size,
                        ((world_x0 + col * cell_px) / cell_px) as u16,
                        ((world_y0 + row * cell_px) / cell_px) as u16,
                    );
                    let mut tile = |c: u16| -> TileHash {
                        let c = c & 0x3FF;
                        *tiles[usize::from(c)]
                            .get_or_insert_with(|| hash_tile(src.vram, regs.char_base, c, depth))
                    };
                    let (pattern, flags) = if regs.tile16 {
                        // The unflipped arrangement; the flips stay the
                        // tilemap's own (module doc of tile_grid).
                        let mut h = Fnv64::new();
                        let mut blank = true;
                        for sub in [0u16, 1, 16, 17] {
                            let t = tile(e.character + sub);
                            h.u64(t.raw);
                            blank &= t.blank;
                        }
                        (h.finish(), flip_bits(e.flip_x, e.flip_y) | blank_bit(blank))
                    } else {
                        let t = tile(e.character);
                        (
                            t.canon,
                            flip_bits(
                                e.flip_x ^ (t.to_canon & 1 != 0),
                                e.flip_y ^ (t.to_canon & 2 != 0),
                            ) | blank_bit(t.blank),
                        )
                    };
                    let palette = all_colours.unwrap_or_else(|| {
                        *pals[usize::from(e.palette)].get_or_insert_with(|| {
                            let (first, n) = palette_first(e.palette);
                            palette_hash(src.cgram, first, n)
                        })
                    });
                    cells.push(TileCell {
                        pattern,
                        palette,
                        flags: flags | if e.priority { TileCell::PRIORITY } else { 0 },
                    });
                }
            }
            out.push(TileGrid {
                layer: layer as u8,
                cell_px: cell_px as u8,
                hud: src.hud[layer],
                exact: banded_exactly && !opt_mode,
                rows_px: (run.y0 as u16, run.y1 as u16),
                origin_world: (i64::from(world_x0), i64::from(world_y0)),
                offset: (off.0 as u8, off.1 as u8),
                cols: cols as u16,
                rows: rows as u16,
                cells,
            });
        }
    }
    out
}

fn blank_bit(blank: bool) -> u8 {
    if blank {
        TileCell::BLANK
    } else {
        0
    }
}

fn flip_bits(x: bool, y: bool) -> u8 {
    (if x { TileCell::FLIP_X } else { 0 }) | (if y { TileCell::FLIP_Y } else { 0 })
}

// ---------------------------------------------------------------------
// NES
// ---------------------------------------------------------------------

/// One tile row of a band: the tiles of one nametable row, by column.
struct NesRow {
    top: i32,
    x0: i16,
    tiles: Vec<Option<rf_nes::ppu::DrawnTile>>,
}

fn nt_col(t: &rf_nes::ppu::DrawnTile) -> u16 {
    t.nt_addr & 0x1F
}

fn nt_row(t: &rf_nes::ppu::DrawnTile) -> u16 {
    (t.nt_addr >> 5) & 0x1F
}

/// The nametable row below `row` (30 rows tall, then the next nametable).
fn next_nt_row(row: u16) -> u16 {
    if row == 29 {
        0
    } else {
        (row + 1) & 0x1F
    }
}

/// Group the frame's tiles into rows by top edge: the PPU reports each tile
/// once per scanline it covers, so keep the first scanline of each row.
fn nes_rows(tiles: &[rf_nes::ppu::DrawnTile]) -> Vec<NesRow> {
    let mut first_y: BTreeMap<i32, u16> = BTreeMap::new();
    let mut by_top: BTreeMap<i32, Vec<rf_nes::ppu::DrawnTile>> = BTreeMap::new();
    for t in tiles {
        let top = i32::from(t.y) - i32::from(t.fine_y);
        let first = *first_y.entry(top).or_insert(t.y);
        if t.y == first {
            by_top.entry(top).or_default().push(*t);
        }
    }
    by_top
        .into_iter()
        .filter_map(|(top, mut ts)| {
            ts.sort_by_key(|t| t.x);
            let x0 = ts.first()?.x;
            let cols = ts
                .iter()
                .map(|t| usize::try_from((i32::from(t.x) - i32::from(x0)) / 8).unwrap_or(0) + 1)
                .max()?;
            let mut tiles = vec![None; cols];
            for t in ts {
                let c = usize::try_from((i32::from(t.x) - i32::from(x0)) / 8).unwrap_or(0);
                tiles[c] = Some(t);
            }
            Some(NesRow { top, x0, tiles })
        })
        .collect()
}

/// Whether `next` continues the band `prev` is in: directly below it, at the
/// same scroll, in the next nametable row.
fn continues(prev: &NesRow, next: &NesRow) -> bool {
    let (Some(Some(a)), Some(Some(b))) = (prev.tiles.first(), next.tiles.first()) else {
        return false;
    };
    next.top == prev.top + 8
        && next.x0 == prev.x0
        && (a.nt_addr & 0x041F) == (b.nt_addr & 0x041F)
        && nt_row(b) == next_nt_row(nt_row(a))
}

/// The grids of a NES frame: one per band of rows sharing a scroll, in 16x16
/// metatile cells. `chr` is the CHR window, `palette` the 32 bytes of palette
/// RAM, `height` the picture height in lines.
#[must_use]
pub fn nes_grids(
    tiles: &[rf_nes::ppu::DrawnTile],
    palette: &[u8; 32],
    chr: &[u8],
    hud: bool,
    height: u32,
) -> Vec<TileGrid> {
    let rows = nes_rows(tiles);
    let mut chr_hash: Vec<Option<(u64, bool)>> = vec![None; 512];
    let mut chr_of = |t: &rf_nes::ppu::DrawnTile| -> (u64, bool) {
        let index = usize::from(t.base / 16) + usize::from(t.tile);
        *chr_hash[index & 0x1FF].get_or_insert_with(|| {
            let at = usize::from(t.base) + usize::from(t.tile) * 16;
            let bytes = chr.get(at..at + 16).unwrap_or(&[0; 16]);
            let mut h = Fnv64::new();
            h.bytes(bytes);
            (h.finish(), bytes.iter().all(|&b| b == 0))
        })
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        // The band is rows[i..j]; `j` always moves past `i` (law 8).
        let mut j = i + 1;
        while j < rows.len() && continues(&rows[j - 1], &rows[j]) {
            j += 1;
        }
        let band = &rows[i..j];
        i = j;
        let Some(Some(first)) = band[0].tiles.first() else {
            continue;
        };
        let par_x = usize::from(nt_col(first) & 1);
        let par_y = usize::from(nt_row(first) & 1);
        let tile_cols = band.iter().map(|r| r.tiles.len()).max().unwrap_or(0);
        let cols = (par_x + tile_cols).div_ceil(2);
        let cell_rows = (par_y + band.len()).div_ceil(2);
        let tile_at = |r: isize, c: isize| -> Option<&rf_nes::ppu::DrawnTile> {
            let row = band.get(usize::try_from(r).ok()?)?;
            row.tiles.get(usize::try_from(c).ok()?)?.as_ref()
        };
        let mut cells = Vec::with_capacity(cols * cell_rows);
        for cj in 0..cell_rows {
            for ci in 0..cols {
                let (r0, c0) = (
                    2 * cj as isize - par_y as isize,
                    2 * ci as isize - par_x as isize,
                );
                let mut h = Fnv64::new();
                let mut missing = false;
                let mut blank = true;
                let mut pal = None;
                for (dr, dc) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                    match tile_at(r0 + dr, c0 + dc) {
                        Some(t) => {
                            let (hash, tile_blank) = chr_of(t);
                            h.u64(hash);
                            blank &= tile_blank;
                            pal.get_or_insert(t.palette & 3);
                        }
                        None => {
                            h.bytes(&[0xFF]);
                            missing = true;
                        }
                    }
                }
                let p = usize::from(pal.unwrap_or(0)) * 4;
                let mut ph = Fnv64::new();
                ph.bytes(&[
                    palette[p + 1] & 0x3F,
                    palette[p + 2] & 0x3F,
                    palette[p + 3] & 0x3F,
                ]);
                cells.push(TileCell {
                    pattern: h.finish(),
                    palette: ph.finish16(),
                    flags: if missing { TileCell::PARTIAL } else { 0 } | blank_bit(blank),
                });
            }
        }
        let top0 = band[0].top;
        let y_start = top0.max(0);
        let y_end = (band[band.len() - 1].top + 8).min(i32::try_from(height).unwrap_or(i32::MAX));
        let nt = first.nt_addr;
        out.push(TileGrid {
            layer: 0,
            cell_px: 16,
            hud,
            exact: true,
            rows_px: (
                u16::try_from(y_start).unwrap_or(0),
                u16::try_from(y_end.max(y_start)).unwrap_or(0),
            ),
            origin_world: (
                (i64::from((nt >> 10) & 1) * 32 + i64::from(nt_col(first)) - par_x as i64) * 8,
                (i64::from((nt >> 11) & 1) * 30 + i64::from(nt_row(first)) - par_y as i64) * 8,
            ),
            offset: (
                (par_x as i32 * 8 - i32::from(band[0].x0)) as u8,
                (y_start - (top0 - par_y as i32 * 8)) as u8,
            ),
            cols: cols as u16,
            rows: cell_rows as u16,
            cells,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A VRAM image with a 2bpp character `c` whose rows are `rows`
    /// (plane 0 byte, plane 1 byte) at char base 0, and tilemap entries at
    /// word 0x4000.
    fn vram() -> Vec<u8> {
        vec![0u8; 0x10000]
    }

    fn put_word(v: &mut [u8], word: u16, value: u16) {
        let at = usize::from(word) * 2;
        v[at] = value as u8;
        v[at + 1] = (value >> 8) as u8;
    }

    fn put_tile(v: &mut [u8], c: u16, rows: [u16; 8]) {
        for (r, w) in rows.iter().enumerate() {
            put_word(v, c * 8 + r as u16, *w);
        }
    }

    fn layer1() -> SnesLayer {
        SnesLayer {
            tilemap_base: 0x4000,
            char_base: 0,
            on_main: true,
            ..SnesLayer::default()
        }
    }

    fn source<'a>(
        vram: &'a [u8],
        cgram: &'a [u16],
        layer: SnesLayer,
        lines: &'a [Option<LineRegisters>],
    ) -> SnesSource<'a> {
        SnesSource {
            vram,
            cgram,
            mode: 0,
            layers: [
                layer,
                SnesLayer::default(),
                SnesLayer::default(),
                SnesLayer::default(),
            ],
            lines,
            screen: (256, 224),
            hud: [false; 4],
        }
    }

    fn entry(c: u16, pal: u16, prio: bool, fx: bool, fy: bool) -> u16 {
        c | pal << 10
            | if prio { 0x2000 } else { 0 }
            | if fx { 0x4000 } else { 0 }
            | if fy { 0x8000 } else { 0 }
    }

    #[test]
    fn a_mirrored_tile_has_the_same_key_and_the_flip_that_maps_it() {
        let mut v = vram();
        // An asymmetric tile and its horizontal mirror stored as two
        // characters.
        let tile = [
            0x80FF, 0x4000, 0x2000, 0x1000, 0x0800, 0x0400, 0x0200, 0x0100,
        ];
        let mirrored = tile.map(|w| {
            let (lo, hi) = (w as u8, (w >> 8) as u8);
            u16::from(lo.reverse_bits()) | u16::from(hi.reverse_bits()) << 8
        });
        put_tile(&mut v, 1, tile);
        put_tile(&mut v, 2, mirrored);
        put_word(&mut v, 0x4000, entry(1, 0, false, false, false));
        put_word(&mut v, 0x4001, entry(2, 0, false, false, false));
        put_word(&mut v, 0x4002, entry(1, 0, false, true, false));
        let cg = vec![0u16; 256];
        let g = &snes_grids(&source(&v, &cg, layer1(), &[]))[0];
        let (a, b, c) = (
            g.obs_at(0, 0).unwrap(),
            g.obs_at(1, 0).unwrap(),
            g.obs_at(2, 0).unwrap(),
        );
        assert_eq!(a.key, b.key, "a mirror image is the same tile");
        assert_eq!(a.key, c.key, "and so is the same tile drawn flipped");
        assert_ne!(a.flip, b.flip, "but the grid says which way it is shown");
        assert_eq!(
            c.flip,
            (!a.flip.0, a.flip.1),
            "flip bits track the tilemap's"
        );
    }

    #[test]
    fn the_key_follows_the_pattern_and_colours_not_the_character_number() {
        let mut v = vram();
        let tile = [
            0x00FF, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x00AA,
        ];
        put_tile(&mut v, 3, tile);
        put_tile(&mut v, 9, tile);
        put_word(&mut v, 0x4000, entry(3, 1, false, false, false));
        put_word(&mut v, 0x4001, entry(9, 1, false, false, false));
        put_word(&mut v, 0x4002, entry(9, 2, false, false, false));
        let mut cg = vec![0u16; 256];
        // Mode 0 layer 1 reads palette 1 at 4..8 and palette 2 at 8..12.
        cg[5] = 0x1234;
        cg[9] = 0x4321;
        let g = &snes_grids(&source(&v, &cg, layer1(), &[]))[0];
        assert_eq!(g.key_at(0, 0), g.key_at(1, 0), "same bytes, new number");
        assert_ne!(g.key_at(0, 0), g.key_at(2, 0), "other colours, other key");
    }

    #[test]
    fn the_priority_bit_is_carried() {
        let mut v = vram();
        put_word(&mut v, 0x4000, entry(0, 0, true, false, false));
        let cg = vec![0u16; 256];
        let g = &snes_grids(&source(&v, &cg, layer1(), &[]))[0];
        assert!(g.obs_at(0, 0).unwrap().priority);
        assert!(!g.obs_at(1, 0).unwrap().priority);
    }

    #[test]
    fn one_scroll_gives_the_live_heights_grid_exactly() {
        let mut v = vram();
        // A tilemap of distinct characters over the whole 32x32 screen, each
        // with a different pattern.
        for i in 0..1024u16 {
            put_tile(&mut v, i % 600, [i, i ^ 0x55, i, 0, 0, i, 0, 1]);
            put_word(
                &mut v,
                0x4000 + i,
                entry(i % 600, i % 8, false, false, false),
            );
        }
        let cg: Vec<u16> = (0..256).collect();
        for (hofs, vofs) in [(0u16, 0u16), (13, 5), (251, 30), (7, 0)] {
            let mut layer = layer1();
            layer.hofs = hofs;
            layer.vofs = vofs;
            let g = snes_grids(&source(&v, &cg, layer, &[]));
            assert_eq!(g.len(), 1);
            let g = &g[0];
            let mut regs = vec![0u8; 0x40];
            regs[0x05] = 0;
            regs[0x07] = 0x40; // base 0x4000 words, 32x32
            let oracle = crate::live_heights::character_grid(
                1,
                8,
                &v,
                &regs,
                [(hofs, vofs), (0, 0), (0, 0), (0, 0)],
                (256, 224),
            )
            .unwrap();
            assert_eq!(
                (u32::from(g.cols), u32::from(g.rows)),
                (oracle.cols, oracle.rows)
            );
            assert_eq!(
                (u32::from(g.offset.0), u32::from(g.offset.1)),
                oracle.offset
            );
            for row in 0..oracle.rows {
                for col in 0..oracle.cols {
                    let c = oracle.char_at(col, row).unwrap();
                    // Same character number <=> same key within the frame,
                    // and the key is the pattern read from that character.
                    let want = hash_tile(&v, 0, c, 2);
                    let cell = g.cell(col as u16, row as u16).unwrap();
                    assert_eq!(cell.pattern, want.canon, "({col},{row}) char {c}");
                }
            }
        }
    }

    #[test]
    fn a_scroll_change_down_the_picture_makes_a_second_grid() {
        let mut v = vram();
        put_word(&mut v, 0x4000, entry(0, 0, false, false, false));
        let cg = vec![0u16; 256];
        let line = |y: u16| {
            let mut l = LineRegisters {
                tm: 1,
                ..LineRegisters::default()
            };
            l.hofs[0] = if y < 192 { 40 } else { 0 };
            Some(l)
        };
        let lines: Vec<_> = (0..224u16).map(line).collect();
        let grids = snes_grids(&source(&v, &cg, layer1(), &lines));
        assert_eq!(grids.len(), 2);
        assert_eq!(grids[0].rows_px, (0, 192));
        assert_eq!(grids[0].origin_world.0, 40);
        assert_eq!(grids[1].rows_px, (192, 224));
        assert_eq!(grids[1].offset, (0, 0));
        assert!(grids.iter().all(|g| g.exact));
    }

    #[test]
    fn a_layer_off_the_screen_gives_no_grid_and_a_swarm_is_one_inexact_band() {
        let mut v = vram();
        put_word(&mut v, 0x4000, entry(0, 0, false, false, false));
        let cg = vec![0u16; 256];
        let none: Vec<_> = (0..224).map(|_| Some(LineRegisters::default())).collect();
        assert!(snes_grids(&source(&v, &cg, layer1(), &none)).is_empty());
        let wavy: Vec<_> = (0..224u16)
            .map(|y| {
                let mut l = LineRegisters {
                    tm: 1,
                    ..LineRegisters::default()
                };
                l.hofs[0] = y;
                Some(l)
            })
            .collect();
        let g = snes_grids(&source(&v, &cg, layer1(), &wavy));
        assert_eq!(g.len(), 1);
        assert!(!g[0].exact);
    }

    #[test]
    fn hires_and_mode_seven_give_no_grids() {
        let v = vram();
        let cg = vec![0u16; 256];
        for mode in [5, 6, 7] {
            let mut s = source(&v, &cg, layer1(), &[]);
            s.mode = mode;
            assert!(snes_grids(&s).is_empty(), "mode {mode}");
        }
    }

    #[test]
    fn sixteen_pixel_characters_are_hashed_as_four_tiles() {
        let mut v = vram();
        for c in [0u16, 1, 16, 17, 2, 3, 18, 19] {
            put_tile(&mut v, c, [c, 1, 2, 3, 4, 5, 6, 7]);
        }
        put_word(&mut v, 0x4000, entry(0, 0, false, false, false));
        put_word(&mut v, 0x4001, entry(2, 0, false, true, true));
        let cg = vec![0u16; 256];
        let mut layer = layer1();
        layer.tile16 = true;
        let g = &snes_grids(&source(&v, &cg, layer, &[]))[0];
        assert_eq!(g.cell_px, 16);
        assert_eq!(g.cols, 16);
        assert_ne!(g.key_at(0, 0), g.key_at(1, 0));
        assert_eq!(
            g.obs_at(1, 0).unwrap().flip,
            (true, true),
            "tilemap flips kept"
        );
    }

    // ---- NES ----------------------------------------------------------

    fn drawn(
        x: i16,
        y: u16,
        fine_y: u8,
        tile: u8,
        nt_addr: u16,
        palette: u8,
    ) -> rf_nes::ppu::DrawnTile {
        rf_nes::ppu::DrawnTile {
            x,
            y,
            tile,
            base: 0,
            palette,
            fine_y,
            nt_addr,
        }
    }

    /// Every scanline of an unscrolled `rows` x `cols` screen, the way the
    /// PPU reports it (one record per tile per scanline).
    fn nes_screen(
        rows: u16,
        cols: u16,
        tile_of: impl Fn(u16, u16) -> u8,
    ) -> Vec<rf_nes::ppu::DrawnTile> {
        let mut v = Vec::new();
        for y in 0..rows * 8 {
            for c in 0..cols {
                let r = y / 8;
                v.push(drawn(
                    (c * 8) as i16,
                    y,
                    (y % 8) as u8,
                    tile_of(r, c),
                    0x2000 + r * 32 + c,
                    ((r / 2 + c / 2) % 4) as u8,
                ));
            }
        }
        v
    }

    fn chr() -> Vec<u8> {
        (0..0x2000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect()
    }

    #[test]
    fn an_unscrolled_nes_screen_is_one_grid_of_sixteen_pixel_metatiles() {
        let tiles = nes_screen(30, 32, |r, c| ((r * 32 + c) % 200) as u8);
        let g = nes_grids(&tiles, &[0x0F; 32], &chr(), false, 240);
        assert_eq!(g.len(), 1);
        let g = &g[0];
        assert_eq!((g.cell_px, g.cols, g.rows), (16, 16, 15));
        assert_eq!(g.rows_px, (0, 240));
        assert_eq!(g.offset, (0, 0));
        assert_eq!(g.origin_world, (0, 0));
        assert!(g.cells.iter().all(|c| !c.partial()));
        // Two metatiles with different tiles have different keys.
        assert_ne!(g.key_at(0, 0), g.key_at(1, 0));
    }

    #[test]
    fn metatiles_follow_the_attribute_alignment_not_the_screen() {
        // Scrolled one tile right: screen column 0 is nametable column 1,
        // so the first metatile cell holds only its right-hand tiles.
        let mut tiles = Vec::new();
        for y in 0..16u16 {
            for c in 0..33u16 {
                let r = y / 8;
                tiles.push(drawn(
                    (c * 8) as i16,
                    y,
                    (y % 8) as u8,
                    (c + r) as u8,
                    0x2000 + r * 32 + (c + 1) % 32 + if c + 1 >= 32 { 0x400 } else { 0 },
                    1,
                ));
            }
        }
        let g = &nes_grids(&tiles, &[0x0F; 32], &chr(), false, 16)[0];
        assert_eq!(
            g.offset.0, 8,
            "the first cell starts a tile left of the picture"
        );
        assert!(
            g.cell(0, 0).unwrap().partial(),
            "its left tiles are off the picture"
        );
        assert!(!g.cell(1, 0).unwrap().partial());
        assert_eq!(g.origin_world.0, 0);
    }

    #[test]
    fn a_fixed_status_bar_is_its_own_band() {
        // Rows 0..24 of the playfield at fine X 3, then a bar at fine X 0.
        let mut tiles = Vec::new();
        for y in 0..32u16 {
            let bar = y >= 24;
            for c in 0..33u16 {
                let r = y / 8;
                tiles.push(drawn(
                    (c * 8) as i16 - if bar { 0 } else { 3 },
                    y,
                    (y % 8) as u8,
                    (c + r) as u8,
                    0x2000 + r * 32 + c % 32,
                    0,
                ));
            }
        }
        let g = nes_grids(&tiles, &[0x0F; 32], &chr(), true, 32);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].rows_px, (0, 24));
        assert_eq!(g[1].rows_px, (24, 32));
        assert_eq!(g[0].offset.0, 3);
        assert_eq!(g[1].offset.0, 0);
        assert!(g.iter().all(|x| x.hud));
    }

    #[test]
    fn the_first_row_may_hang_above_the_picture() {
        // Fine Y 5 at the top: the first tile row's top edge is at y = -5.
        let mut tiles = Vec::new();
        for y in 0..11u16 {
            let fy = ((y + 5) % 8) as u8;
            let row = (y + 5) / 8;
            for c in 0..32u16 {
                tiles.push(drawn(
                    (c * 8) as i16,
                    y,
                    fy,
                    c as u8,
                    0x2000 + (3 + row) * 32 + c,
                    0,
                ));
            }
        }
        let g = &nes_grids(&tiles, &[0; 32], &chr(), false, 11)[0];
        assert_eq!(g.rows_px, (0, 11));
        // Row 3 is odd, so its metatile starts one tile row higher still.
        assert_eq!(g.offset.1, 5 + 8);
        assert_eq!(g.origin_world.1, (3 - 1) * 8);
    }
}
