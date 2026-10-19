//! The live-screen height grid (ticket W30-06 part A): raise the diorama out
//! of the tilemap that is **on screen right now**, using a per-tile height
//! table the profile author painted by hand (`[heights]`,
//! `rf_profiles::schema::Heights`).
//!
//! A game with no level decoder has no level to build walls from, and
//! nothing in its ROM says which tile is a wall. So the walls come from two
//! things only: the background layer's tilemap as the PPU is reading it this
//! frame, and the author's table. Nothing is inferred from game code.
//!
//! ## What is read
//!
//! A pure function of a SNES debug snapshot (`core_thread::SnesDebugFrame`:
//! VRAM, `$2100-$213F` and the four BG scroll pairs).
//!
//! - **Tilemap entry** — 16 bits per cell: `vhopppcc cccccccc` (flip y, flip
//!   x, priority, 3-bit palette, 10-bit character number). Only the
//!   character number is looked up; the table is keyed on it. Source:
//!   fullsnes "SNES PPU Background Tilemap Format"; snes.nesdev.org
//!   "Tilemaps".
//! - **Tilemap address and size** — `$2107-$210A` BGnSC: address bits 15-10
//!   of the word address in bits 2-7, size in bits 0-1 (`rf_snes::debug::
//!   tilemap_entry` owns the 32x32-screen quadrant layout). Source: fullsnes
//!   "SNES PPU BG Control"; snes.nesdev.org "PPU registers".
//! - **Tile size** — `$2105` BGMODE bits 4-7 pick 8x8 or 16x16 characters
//!   per BG. A 16x16 character is named by its top-left 8x8 tile, which is
//!   the number a tilemap entry holds.
//! - **Scroll** — `$210D-$2114` BGnHOFS/BGnVOFS, a 10-bit offset added to the
//!   screen pixel to find the tilemap pixel (modulo the map size). Source:
//!   fullsnes "SNES PPU BG Scrolling"; snes.nesdev.org "Backgrounds".
//!
//! ## Locked to the picture
//!
//! The grid's cells are the **map's** cells, not the screen's. Scrolled by
//! `s` pixels with cells `P` pixels wide, the first column starts `s mod P`
//! pixels to the left of the screen, so the grid is one cell wider and taller
//! than the screen whenever `s mod P != 0` (`32x28` at 8 px and no sub-tile
//! scroll, `33x29` with it). [`LiveGrid::offset`] is that shift; the ground
//! texture ([`padded_ground`]) and every billboard are placed by it, so a wall
//! and the tile it was raised from move together, pixel for pixel.
//!
//! ## Limits
//!
//! SNES only. Modes 5, 6 (hi-res) and 7 (no tilemap) yield no grid. The scroll
//! is the value held when the frame ended, so a mid-frame HDMA scroll change
//! (parallax bands) is walled as if its last value held for the whole screen.

use std::collections::HashMap;

use rf_snes::debug::tilemap_entry;

/// `$2105` BGMODE register offset within `ppu_regs`.
const BGMODE: usize = 0x05;
/// `$2107` BG1SC; BG2-4 follow.
const BG1SC: usize = 0x07;
/// The tilemap's scroll is 10 bits wide.
const SCROLL_MASK: u16 = 0x03FF;

/// How many BG layers each BG mode has (modes 0-4; 5-7 are refused above).
const LAYERS_IN_MODE: [u8; 5] = [4, 3, 2, 2, 2];

/// The profile's `[heights]`, parsed once at ROM open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeightTable {
    /// 1..=4.
    pub layer: u8,
    /// 8 or 16.
    pub tile_px: u8,
    heights: HashMap<u16, u8>,
}

impl HeightTable {
    /// From a loaded (and therefore validated) profile table.
    #[must_use]
    pub fn from_profile(h: &rf_profiles::Heights) -> Self {
        Self {
            layer: u8::try_from(h.layer).unwrap_or(1).clamp(1, 4),
            tile_px: if h.tile_px == 16 { 16 } else { 8 },
            heights: h.table(),
        }
    }

    /// Build directly (tests and the painter's preview).
    #[must_use]
    pub fn new(layer: u8, tile_px: u8, heights: HashMap<u16, u8>) -> Self {
        Self {
            layer,
            tile_px,
            heights,
        }
    }

    /// Height in eighths of tile number `character`; unknown tiles are flat.
    #[must_use]
    pub fn height_of(&self, character: u16) -> u8 {
        self.heights.get(&character).copied().unwrap_or(0)
    }

    /// Whether the table names no tile at all (nothing to raise).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }
}

/// One frame's wall grid, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveGrid {
    pub cols: u32,
    pub rows: u32,
    pub tile_px: u32,
    /// How far the grid's first column/row starts left of / above the
    /// screen, in pixels (`scroll mod tile_px`). `(0, 0)` with no sub-cell
    /// scroll.
    pub offset: (u32, u32),
    /// `0`/`1`, `cols * rows` entries; `1` where the height is above zero.
    pub solid: Vec<u8>,
    /// Height in eighths, same indexing.
    pub depth: Vec<u8>,
}

impl LiveGrid {
    /// The grid's pixel extent: what the ground texture must cover.
    #[must_use]
    pub fn extent(&self) -> (u32, u32) {
        (self.cols * self.tile_px, self.rows * self.tile_px)
    }

    /// Ticket W34-07 (D2): the same grid with `m` more cells on every side,
    /// all flat. The ground texture of a padded grid repeats the picture's
    /// edge pixels into the new cells ([`padded_ground`]), so a pitched camera
    /// looking past the room sees the room's own edge colours instead of the
    /// clear colour. `screen_to_grid` follows the shift.
    #[must_use]
    pub fn with_margin(&self, m: u32) -> Self {
        let (cols, rows) = (self.cols + 2 * m, self.rows + 2 * m);
        let mut solid = vec![0u8; (cols * rows) as usize];
        let mut depth = vec![0u8; (cols * rows) as usize];
        for r in 0..self.rows {
            for c in 0..self.cols {
                let (from, to) = (
                    (r * self.cols + c) as usize,
                    ((r + m) * cols + c + m) as usize,
                );
                solid[to] = self.solid[from];
                depth[to] = self.depth[from];
            }
        }
        Self {
            cols,
            rows,
            tile_px: self.tile_px,
            offset: (
                self.offset.0 + m * self.tile_px,
                self.offset.1 + m * self.tile_px,
            ),
            solid,
            depth,
        }
    }

    /// A screen pixel position in grid (world) space.
    #[must_use]
    pub fn screen_to_grid(&self, x: f32, y: f32) -> (f32, f32) {
        (x + self.offset.0 as f32, y + self.offset.1 as f32)
    }

    /// Where cell `(col, row)`'s top-left corner is on the screen (can be
    /// negative for the first column/row).
    #[must_use]
    pub fn cell_screen_origin(&self, col: u32, row: u32) -> (i32, i32) {
        (
            (col * self.tile_px) as i32 - self.offset.0 as i32,
            (row * self.tile_px) as i32 - self.offset.1 as i32,
        )
    }
}

/// One frame's grid of tile numbers: [`LiveGrid`]'s geometry with the
/// character number in each cell instead of a height. The painter hit-tests
/// and draws against this, and [`build_grid`] is this plus the table, so the
/// grid the author clicks on and the walls raised from it cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharGrid {
    pub cols: u32,
    pub rows: u32,
    pub tile_px: u32,
    /// As [`LiveGrid::offset`].
    pub offset: (u32, u32),
    /// 10-bit character numbers, `cols * rows`, row-major.
    pub chars: Vec<u16>,
}

impl CharGrid {
    /// The cell under screen pixel `(x, y)` (frame pixels, may be fractional),
    /// or `None` outside the picture.
    #[must_use]
    pub fn cell_at(&self, x: f32, y: f32) -> Option<(u32, u32)> {
        if !(x >= 0.0 && y >= 0.0) {
            return None;
        }
        let p = self.tile_px.max(1);
        // Truncation is the floor of a non-negative value.
        let col = (x as u32 + self.offset.0) / p;
        let row = (y as u32 + self.offset.1) / p;
        (col < self.cols && row < self.rows).then_some((col, row))
    }

    /// The character number in cell `(col, row)`.
    #[must_use]
    pub fn char_at(&self, col: u32, row: u32) -> Option<u16> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.chars.get((row * self.cols + col) as usize).copied()
    }

    /// Where cell `(col, row)`'s top-left corner is on the screen (negative
    /// for the first column/row under sub-cell scroll).
    #[must_use]
    pub fn cell_screen_origin(&self, col: u32, row: u32) -> (i32, i32) {
        (
            (col * self.tile_px) as i32 - self.offset.0 as i32,
            (row * self.tile_px) as i32 - self.offset.1 as i32,
        )
    }
}

/// The tile numbers on screen for `layer` (1..=4) at `tile_px` (8 or 16).
/// Same arguments and the same `None` cases as [`build_grid`].
#[must_use]
pub fn character_grid(
    layer: u8,
    tile_px: u8,
    vram: &[u8],
    ppu_regs: &[u8],
    bg_scroll: [(u16, u16); 4],
    screen: (u32, u32),
) -> Option<CharGrid> {
    let bgmode = *ppu_regs.get(BGMODE)?;
    let mode = usize::from(bgmode & 0x07);
    let layer = usize::from(layer.clamp(1, 4)) - 1;
    if usize::from(*LAYERS_IN_MODE.get(mode)?) <= layer || vram.len() < 0x10000 {
        return None;
    }
    let (screen_w, screen_h) = screen;
    if screen_w == 0 || screen_h == 0 {
        return None;
    }

    let sc = *ppu_regs.get(BG1SC + layer)?;
    let map_base = u16::from(sc & 0xFC) << 8;
    let map_size = sc & 0x03;
    let bg_cell_px: u32 = if bgmode & (0x10 << layer) != 0 { 16 } else { 8 };
    let p = u32::from(tile_px.max(1));
    let (hofs, vofs) = bg_scroll[layer];
    let (hofs, vofs) = (u32::from(hofs & SCROLL_MASK), u32::from(vofs & SCROLL_MASK));
    let offset = (hofs % p, vofs % p);

    let cols = (screen_w + offset.0).div_ceil(p);
    let rows = (screen_h + offset.1).div_ceil(p);
    let mut chars = Vec::with_capacity((cols * rows) as usize);
    // Bounded ranges only (law 8): both walk a fixed, finite count.
    for row in 0..rows {
        let world_y = vofs - offset.1 + row * p;
        for col in 0..cols {
            let world_x = hofs - offset.0 + col * p;
            let entry = tilemap_entry(
                vram,
                map_base,
                map_size,
                (world_x / bg_cell_px) as u16,
                (world_y / bg_cell_px) as u16,
            );
            chars.push(entry.character);
        }
    }
    Some(CharGrid {
        cols,
        rows,
        tile_px: p,
        offset,
        chars,
    })
}

/// Build the wall grid for the screen the PPU is showing.
///
/// `vram` is 64 KiB, `ppu_regs` is `$2100-$213F` indexed so `[n]` is
/// `$21nn`, `bg_scroll[i]` is BG`i+1`'s `(HOFS, VOFS)`, and `screen` is the
/// frame's `(width, height)` in pixels. `None` when the layer does not exist
/// in the current BG mode, the mode has no plain tilemap (5, 6, 7), or the
/// snapshot is malformed — the caller then shows the flat view.
#[must_use]
pub fn build_grid(
    table: &HeightTable,
    vram: &[u8],
    ppu_regs: &[u8],
    bg_scroll: [(u16, u16); 4],
    screen: (u32, u32),
) -> Option<LiveGrid> {
    let g = character_grid(
        table.layer,
        table.tile_px,
        vram,
        ppu_regs,
        bg_scroll,
        screen,
    )?;
    let depth: Vec<u8> = g
        .chars
        .iter()
        .map(|c| table.height_of(*c).min(64))
        .collect();
    Some(LiveGrid {
        cols: g.cols,
        rows: g.rows,
        tile_px: g.tile_px,
        offset: g.offset,
        solid: depth.iter().map(|h| u8::from(*h > 0)).collect(),
        depth,
    })
}

/// The current frame laid out as the grid's ground texture: the screen
/// shifted by [`LiveGrid::offset`] into a `cols*tile_px` by `rows*tile_px`
/// image, with the edge pixels repeated into the margins so no cell that
/// pokes past the picture is a hole. Straight RGBA, alpha forced opaque.
#[must_use]
pub fn padded_ground(frame_rgba: &[u8], width: u32, height: u32, grid: &LiveGrid) -> Vec<u8> {
    let (out_w, out_h) = grid.extent();
    let mut out = Vec::with_capacity((out_w * out_h * 4) as usize);
    if width == 0 || height == 0 || frame_rgba.len() < (width * height * 4) as usize {
        out.resize((out_w * out_h * 4) as usize, 0);
        return out;
    }
    for dy in 0..out_h {
        let sy = dy.saturating_sub(grid.offset.1).min(height - 1);
        for dx in 0..out_w {
            let sx = dx.saturating_sub(grid.offset.0).min(width - 1);
            let at = ((sy * width + sx) * 4) as usize;
            out.extend_from_slice(&frame_rgba[at..at + 3]);
            out.push(255);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP_WORDS: u16 = 0x0400;

    /// `$2105` and `$2107` set for BG1: mode `mode`, map at word 0x0400,
    /// size `size`.
    fn regs(mode: u8, size: u8, bg_tiles_16: bool) -> Vec<u8> {
        let mut r = vec![0u8; 0x40];
        r[BGMODE] = mode | if bg_tiles_16 { 0x10 } else { 0 };
        r[BG1SC] = ((MAP_WORDS >> 8) as u8 & 0xFC) | size;
        r
    }

    fn put(vram: &mut [u8], tx: u16, ty: u16, entry: u16) {
        let word = usize::from(MAP_WORDS) + usize::from(ty) * 32 + usize::from(tx);
        vram[word * 2] = entry as u8;
        vram[word * 2 + 1] = (entry >> 8) as u8;
    }

    fn table(px: u8, rows: &[(u16, u8)]) -> HeightTable {
        HeightTable::new(1, px, rows.iter().copied().collect())
    }

    fn grid(t: &HeightTable, vram: &[u8], r: &[u8], scroll: (u16, u16)) -> LiveGrid {
        build_grid(t, vram, r, [scroll, (0, 0), (0, 0), (0, 0)], (256, 224)).unwrap()
    }

    #[test]
    fn a_grid_with_a_margin_keeps_its_cells_and_repeats_the_edge_pixels_outside_them() {
        let g = LiveGrid {
            cols: 2,
            rows: 2,
            tile_px: 8,
            offset: (3, 5),
            solid: vec![1, 0, 0, 1],
            depth: vec![8, 0, 0, 4],
        };
        let m = g.with_margin(2);
        assert_eq!((m.cols, m.rows), (6, 6));
        assert_eq!(m.offset, (3 + 16, 5 + 16));
        assert_eq!(m.extent(), (48, 48));
        // The old cells sit two cells in; the margin is flat.
        let at = |c: u32, r: u32| m.depth[(r * m.cols + c) as usize];
        assert_eq!((at(2, 2), at(3, 2), at(2, 3), at(3, 3)), (8, 0, 0, 4));
        assert_eq!(m.depth.iter().filter(|d| **d > 0).count(), 2);
        assert_eq!(m.solid.iter().map(|s| usize::from(*s)).sum::<usize>(), 2);
        // A screen point maps to the same cell of the new grid, two cells over.
        assert_eq!(
            m.screen_to_grid(1.0, 1.0),
            (
                g.screen_to_grid(1.0, 1.0).0 + 16.0,
                g.screen_to_grid(1.0, 1.0).1 + 16.0
            )
        );
        // The ground texture of the padded grid repeats the picture's edge
        // pixels into the margin: nothing in it is the transparent fill.
        let frame: Vec<u8> = (0..16 * 16)
            .flat_map(|i| [(i % 251) as u8, 7, 9, 255])
            .collect();
        let tex = padded_ground(&frame, 16, 16, &m);
        let (w, h) = m.extent();
        assert_eq!(tex.len(), (w * h * 4) as usize);
        assert!(tex.chunks_exact(4).all(|p| p[3] == 255));
        // The far corner is the picture's last pixel.
        let last = &frame[(16 * 16 - 1) * 4..16 * 16 * 4 - 1];
        assert_eq!(&tex[tex.len() - 4..tex.len() - 1], last);
    }

    #[test]
    fn a_tilemap_entry_is_looked_up_by_its_ten_bit_character_ignoring_the_other_bits() {
        let mut vram = vec![0u8; 0x10000];
        // Flip y/x, priority, palette 7, character 0x0A4.
        put(&mut vram, 3, 2, 0xFC00 | 0x0A4);
        let g = grid(&table(8, &[(0x0A4, 16)]), &vram, &regs(1, 0, false), (0, 0));
        assert_eq!((g.cols, g.rows), (32, 28));
        let at = 2 * 32 + 3;
        assert_eq!((g.solid[at], g.depth[at]), (1, 16));
        assert_eq!(g.solid.iter().map(|s| u32::from(*s)).sum::<u32>(), 1);
    }

    #[test]
    fn an_unknown_tile_and_a_zero_height_are_flat() {
        let mut vram = vec![0u8; 0x10000];
        put(&mut vram, 0, 0, 0x001);
        put(&mut vram, 1, 0, 0x002);
        let g = grid(&table(8, &[(0x002, 0)]), &vram, &regs(1, 0, false), (0, 0));
        assert!(g.solid.iter().all(|s| *s == 0) && g.depth.iter().all(|d| *d == 0));
    }

    #[test]
    fn sixteen_pixel_cells_use_the_top_left_tile_only() {
        let mut vram = vec![0u8; 0x10000];
        // Cell (1,1) is tiles (2..4, 2..4). Only a non-top-left tile set.
        put(&mut vram, 3, 3, 0x0A4);
        let t = table(16, &[(0x0A4, 24)]);
        let g = grid(&t, &vram, &regs(1, 0, false), (0, 0));
        assert_eq!((g.cols, g.rows, g.tile_px), (16, 14, 16));
        assert!(g.solid.iter().all(|s| *s == 0));
        // Now the top-left tile.
        put(&mut vram, 2, 2, 0x0A4);
        let g = grid(&t, &vram, &regs(1, 0, false), (0, 0));
        assert_eq!((g.solid[16 + 1], g.depth[16 + 1]), (1, 24));
        assert_eq!(g.solid.iter().map(|s| u32::from(*s)).sum::<u32>(), 1);
    }

    #[test]
    fn a_sub_tile_scroll_widens_the_grid_and_keeps_walls_on_their_pixels() {
        let mut vram = vec![0u8; 0x10000];
        put(&mut vram, 10, 5, 0x0A4);
        let t = table(8, &[(0x0A4, 8)]);
        let r = regs(1, 0, false);
        for (sx, sy) in [(0u16, 0u16), (3, 0), (3, 5), (11, 12), (0, 7)] {
            let g = grid(&t, &vram, &r, (sx, sy));
            assert_eq!(g.offset, (u32::from(sx % 8), u32::from(sy % 8)));
            assert_eq!(g.cols, (256 + g.offset.0).div_ceil(8));
            assert_eq!(g.rows, (224 + g.offset.1).div_ceil(8));
            let hits: Vec<usize> = (0..g.solid.len()).filter(|i| g.solid[*i] == 1).collect();
            assert_eq!(hits.len(), 1, "scroll ({sx},{sy})");
            let (col, row) = (hits[0] as u32 % g.cols, hits[0] as u32 / g.cols);
            // The tile sits at map pixel (80, 40); the screen shows it at
            // map pixel minus scroll.
            assert_eq!(
                g.cell_screen_origin(col, row),
                (80 - i32::from(sx), 40 - i32::from(sy)),
                "scroll ({sx},{sy})"
            );
        }
        // No sub-tile scroll keeps the plain screen-sized grid.
        let g = grid(&t, &vram, &r, (16, 8));
        assert_eq!((g.cols, g.rows, g.offset), (32, 28, (0, 0)));
    }

    #[test]
    fn scroll_wraps_around_the_map_and_crosses_into_the_second_screen_of_a_wide_map() {
        let mut vram = vec![0u8; 0x10000];
        // 32-wide map: scrolled 8 px from the right edge, column 31 is first.
        put(&mut vram, 31, 0, 0x0A4);
        let t = table(8, &[(0x0A4, 8)]);
        let g = grid(&t, &vram, &regs(1, 0, false), (31 * 8, 0));
        assert_eq!(g.solid[0], 1);
        // 64-wide map (size 1): the right half lives 0x400 words on.
        let mut vram = vec![0u8; 0x10000];
        let word = usize::from(MAP_WORDS) + 0x400;
        vram[word * 2] = 0xA4;
        let g = grid(&t, &vram, &regs(1, 1, false), (32 * 8, 0));
        assert_eq!(g.solid[0], 1, "tile column 32 is the second screen's first");
    }

    #[test]
    fn sixteen_pixel_background_characters_are_sampled_at_sixteen_pixels() {
        let mut vram = vec![0u8; 0x10000];
        put(&mut vram, 1, 0, 0x0A4);
        let t = table(8, &[(0x0A4, 8)]);
        let g = grid(&t, &vram, &regs(1, 0, true), (0, 0));
        // BG char (1,0) covers screen x 16..32, i.e. eight-pixel cells 2 and 3.
        let row = |r: usize| g.solid[r * 32..r * 32 + 6].to_vec();
        assert_eq!(row(0), vec![0, 0, 1, 1, 0, 0]);
        assert_eq!(
            row(1),
            vec![0, 0, 1, 1, 0, 0],
            "a 16 px character is two cells tall"
        );
        assert_eq!(row(2), vec![0; 6]);
    }

    #[test]
    fn modes_without_a_plain_tilemap_and_missing_layers_give_no_grid() {
        let vram = vec![0u8; 0x10000];
        let t = table(8, &[(1, 8)]);
        let s = [(0, 0); 4];
        for mode in [5u8, 6, 7] {
            assert!(build_grid(&t, &vram, &regs(mode, 0, false), s, (256, 224)).is_none());
        }
        // BG3 does not exist in mode 3, BG4 not in mode 1.
        let mut t3 = t.clone();
        t3.layer = 3;
        assert!(build_grid(&t3, &vram, &regs(3, 0, false), s, (256, 224)).is_none());
        let mut t4 = t.clone();
        t4.layer = 4;
        assert!(build_grid(&t4, &vram, &regs(1, 0, false), s, (256, 224)).is_none());
        assert!(build_grid(&t, &vram, &[], s, (256, 224)).is_none());
        assert!(build_grid(&t, &vram[..100], &regs(1, 0, false), s, (256, 224)).is_none());
    }

    #[test]
    fn the_chosen_layer_picks_its_own_tilemap_register_and_scroll() {
        let mut vram = vec![0u8; 0x10000];
        let mut r = regs(0, 0, false);
        // BG2: map at word 0x0800.
        r[BG1SC + 1] = 0x08;
        let word = 0x0800usize + 5;
        vram[word * 2] = 0xA4;
        let mut t = table(8, &[(0x0A4, 8)]);
        t.layer = 2;
        let g = build_grid(&t, &vram, &r, [(0, 0), (8, 0), (0, 0), (0, 0)], (256, 224)).unwrap();
        assert_eq!(
            g.solid[4], 1,
            "BG2's scroll of 8 px shifts column 5 to cell 4"
        );
    }

    #[test]
    fn the_ground_is_the_frame_shifted_by_the_offset_with_edges_repeated() {
        let grid = LiveGrid {
            cols: 3,
            rows: 2,
            tile_px: 2,
            offset: (1, 1),
            solid: vec![0; 6],
            depth: vec![0; 6],
        };
        // A 4x2 frame whose pixel (x, y) has red = 10*y + x.
        let mut frame = Vec::new();
        for y in 0..2u8 {
            for x in 0..4u8 {
                frame.extend_from_slice(&[10 * y + x, 0, 0, 7]);
            }
        }
        let out = padded_ground(&frame, 4, 2, &grid);
        assert_eq!(out.len(), 6 * 4 * 4);
        let red = |x: usize, y: usize| out[(y * 6 + x) * 4];
        // Frame pixel (0,0) lands at (1,1); the margins repeat the edge.
        assert_eq!(red(1, 1), 0);
        assert_eq!(red(2, 1), 1);
        assert_eq!(red(0, 0), 0, "top-left margin repeats the corner");
        assert_eq!(red(5, 3), 13, "bottom-right margin repeats the corner");
        assert!(out.chunks(4).all(|px| px[3] == 255));
        // A short buffer degrades to a blank, not a panic.
        assert!(padded_ground(&frame[..8], 4, 2, &grid)
            .iter()
            .all(|b| *b == 0));
    }

    #[test]
    fn the_painters_character_grid_matches_the_walls_cell_for_cell() {
        let mut vram = vec![0u8; 0x10000];
        put(&mut vram, 10, 5, 0x0A4);
        put(&mut vram, 11, 5, 0x0B0);
        let t = table(8, &[(0x0A4, 8), (0x0B0, 24)]);
        let r = regs(1, 0, false);
        for scroll in [(0u16, 0u16), (3, 5), (11, 12)] {
            let s = [scroll, (0, 0), (0, 0), (0, 0)];
            let walls = build_grid(&t, &vram, &r, s, (256, 224)).unwrap();
            let chars = character_grid(1, 8, &vram, &r, s, (256, 224)).unwrap();
            assert_eq!((walls.cols, walls.rows), (chars.cols, chars.rows));
            assert_eq!(walls.offset, chars.offset);
            for (i, c) in chars.chars.iter().enumerate() {
                assert_eq!(walls.depth[i], t.height_of(*c), "cell {i} at {scroll:?}");
            }
            // Hit-testing the middle of the on-screen tile finds it.
            let (sx, sy) = (80 - i32::from(scroll.0) + 4, 40 - i32::from(scroll.1) + 4);
            let (col, row) = chars.cell_at(sx as f32, sy as f32).unwrap();
            assert_eq!(chars.char_at(col, row), Some(0x0A4), "scroll {scroll:?}");
        }
    }

    #[test]
    fn hit_testing_rejects_outside_the_picture_and_handles_sixteen_pixel_cells() {
        let mut vram = vec![0u8; 0x10000];
        put(&mut vram, 2, 2, 0x0A4);
        let r = regs(1, 0, false);
        let g = character_grid(1, 16, &vram, &r, [(0, 0); 4], (256, 224)).unwrap();
        assert_eq!((g.cols, g.rows), (16, 14));
        assert_eq!(g.cell_at(-1.0, 5.0), None);
        assert_eq!(g.cell_at(5.0, -0.5), None);
        assert_eq!(g.cell_at(256.0, 5.0), None);
        assert_eq!(g.cell_at(5.0, 224.0), None);
        // Pixel (20, 20) is in 16 px cell (1, 1) = tiles 2..4: top-left is tile (2,2).
        let (c, r_) = g.cell_at(20.0, 20.0).unwrap();
        assert_eq!((c, r_), (1, 1));
        assert_eq!(g.char_at(c, r_), Some(0x0A4));
        assert_eq!(g.char_at(99, 0), None);
    }

    #[test]
    fn the_table_comes_from_a_loaded_profile() {
        let text = include_str!("../../../profiles/snes/rf-scroller-s/profile.toml");
        let mut text = text.to_string();
        text.push_str("\n[heights]\nlayer = 2\ntile_px = 16\n[heights.tiles]\n\"0x0A4\" = 16\n");
        let profile = rf_profiles::load_str(&text)
            .expect("profile with [heights]")
            .profile;
        let t = HeightTable::from_profile(profile.heights.as_ref().unwrap());
        assert_eq!(
            (t.layer, t.tile_px, t.height_of(0xA4), t.height_of(1)),
            (2, 16, 16, 0)
        );
    }
}
