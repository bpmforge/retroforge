//! Nametable/tilemap viewer data provider (FR-DBG-001, DEBUGGER.md §3
//! "Nametable/Tilemap" row: "4 nametables w/ scroll + mirroring overlay,
//! attribute grid").
//!
//! ## Data source — same gap as `crate::palette`, read that module's doc
//!
//! One NES nametable is 1024 bytes of PPU-internal VRAM
//! (`rf_core_api::StateView::vram`'s field), which lives in
//! `rf_nes::ppu::Ppu`'s `pub(super) vram: [u8; 0x1000]` — invisible even to
//! `rf-nes`'s own `system` module, and there is no `NesBus`/`Ppu` getter
//! for it at all (verified the same way `crate::palette`'s doc verifies the
//! missing `palette()` getter). [`decode_nametable`] below is therefore
//! decode logic only, tested against synthetic bytes; wiring it to real
//! VRAM needs a future `NesBus::vram()` accessor mirroring the existing
//! `NesBus::oam()` — out of this ticket's write scope (`crates/rf-nes` is
//! not in it).
//!
//! ## Layout ([nesdev.org/wiki/PPU_nametables](https://www.nesdev.org/wiki/PPU_nametables),
//! [nesdev.org/wiki/PPU_attribute_tables](https://www.nesdev.org/wiki/PPU_attribute_tables))
//!
//! A nametable is a 32x30 grid of tile-index bytes (960 bytes, row-major,
//! `nametable[row * 32 + col]`), followed at offset `0x3C0` by a 64-byte
//! attribute table: an 8x8 grid of bytes, each covering a 32x32-pixel (4x4
//! tile) block, split into four 16x16-pixel (2x2 tile) quadrants. Bits
//! `0-1` of the byte select the top-left quadrant's palette, `2-3`
//! top-right, `4-5` bottom-left, `6-7` bottom-right.

/// Tiles per nametable row/column.
pub const NAMETABLE_COLS: usize = 32;
pub const NAMETABLE_ROWS: usize = 30;
/// Tile-index bytes precede the attribute table at this offset (module
/// doc).
const ATTRIBUTE_TABLE_OFFSET: usize = 0x3C0;
/// Total bytes one nametable occupies.
pub const NAMETABLE_LEN: usize = 0x400;
/// Attribute-table columns/rows (8x8 bytes, module doc).
const ATTR_COLS: usize = 8;

/// One decoded nametable cell: which tile it names, and which of the 4
/// background palettes (`crate::palette::decode_palette`'s groups 0-3)
/// applies to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NametableCell {
    pub tile_index: u8,
    pub palette: u8,
}

/// Decode one 1024-byte nametable into its 32x30 grid, resolving each
/// cell's palette from the attribute table (module doc). Returns `None` if
/// `nametable` is shorter than [`NAMETABLE_LEN`] — no live producer exists
/// yet (module doc), so an empty slice degrades to "no data" rather than a
/// bogus all-tile-0 grid.
#[must_use]
pub fn decode_nametable(
    nametable: &[u8],
) -> Option<[[NametableCell; NAMETABLE_COLS]; NAMETABLE_ROWS]> {
    let nametable: &[u8; NAMETABLE_LEN] = nametable.get(..NAMETABLE_LEN)?.try_into().ok()?;
    let mut grid = [[NametableCell {
        tile_index: 0,
        palette: 0,
    }; NAMETABLE_COLS]; NAMETABLE_ROWS];
    for row in 0..NAMETABLE_ROWS {
        for col in 0..NAMETABLE_COLS {
            let tile_index = nametable[row * NAMETABLE_COLS + col];
            let attr_byte = nametable[ATTRIBUTE_TABLE_OFFSET + (row / 4) * ATTR_COLS + (col / 4)];
            let quadrant = ((row % 4) / 2) * 2 + (col % 4) / 2;
            let palette = (attr_byte >> (quadrant * 2)) & 0x03;
            grid[row][col] = NametableCell {
                tile_index,
                palette,
            };
        }
    }
    Some(grid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_nametable_places_a_known_tile_at_the_expected_cell() {
        let mut buf = [0u8; NAMETABLE_LEN];
        // Row 5, col 10 -> offset 5*32+10 = 170.
        buf[170] = 0xAB;
        let grid = decode_nametable(&buf).expect("full buffer must decode");
        assert_eq!(grid[5][10].tile_index, 0xAB);
        // Every other cell stays tile 0, not garbage.
        assert_eq!(grid[0][0].tile_index, 0);
        assert_eq!(grid[29][31].tile_index, 0);
    }

    #[test]
    fn decode_nametable_resolves_each_of_the_four_attribute_quadrants() {
        let mut buf = [0u8; NAMETABLE_LEN];
        // Attribute byte for tile-block (row 0-3, col 0-3) is at attribute
        // offset 0 (row/4=0, col/4=0). Quadrants: bits 0-1 top-left
        // (tiles row0-1,col0-1), 2-3 top-right (row0-1,col2-3), 4-5
        // bottom-left (row2-3,col0-1), 6-7 bottom-right (row2-3,col2-3).
        // Palette 1 top-left, 2 top-right, 3 bottom-left, 0 bottom-right:
        // 0b00_11_10_01 = 0x39.
        buf[ATTRIBUTE_TABLE_OFFSET] = 0b00_11_10_01;
        let grid = decode_nametable(&buf).unwrap();
        assert_eq!(grid[0][0].palette, 1, "top-left quadrant");
        assert_eq!(grid[1][1].palette, 1, "top-left quadrant, other corner");
        assert_eq!(grid[0][2].palette, 2, "top-right quadrant");
        assert_eq!(grid[2][0].palette, 3, "bottom-left quadrant");
        assert_eq!(grid[3][3].palette, 0, "bottom-right quadrant");
    }

    #[test]
    fn decode_nametable_selects_the_right_attribute_byte_for_a_distant_block() {
        let mut buf = [0u8; NAMETABLE_LEN];
        // Tile block at row 8-11, col 12-15 -> attribute index
        // (8/4)*8 + (12/4) = 2*8+3 = 19.
        buf[ATTRIBUTE_TABLE_OFFSET + 19] = 0b00_00_00_01; // top-left = palette 1
        let grid = decode_nametable(&buf).unwrap();
        assert_eq!(grid[8][12].palette, 1);
        // An unrelated cell in a different block must not see it.
        assert_eq!(grid[0][0].palette, 0);
    }

    #[test]
    fn decode_nametable_returns_none_for_a_short_slice() {
        assert_eq!(decode_nametable(&[]), None);
        assert_eq!(decode_nametable(&[0u8; NAMETABLE_LEN - 1]), None);
    }
}
