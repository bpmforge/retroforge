//! Pattern/CHR viewer data provider (FR-DBG-001, DEBUGGER.md §3 "Pattern/
//! CHR" row: "both pattern tables, palette selector, 8x16 mode").
//!
//! ## Where the CHR bytes come from — read this before wiring a panel
//!
//! This module decodes whatever `chr: &[u8]` it is handed; it never reads
//! one itself. `rf-debugger` may not depend on `rf-nes` at all
//! (`scripts/validate-arch.sh` rule 3), so there is no live PPU-CHR
//! accessor reachable from here even in principle — and there is currently
//! no accessor at all: `rf_nes::ppu::Ppu`'s `chr`/`chr_peek` are
//! `pub(super)`, invisible even to `rf-nes`'s own `system` module (verified
//! by reading the source, ticket W4-06a pre-flight). `crates/retroforge`'s
//! shell is the mediator (ticket brief, "shell-mediated route", same
//! resolution as SceneGraph at W4-03c/W4-03e): it already depends on
//! `rf-nes` and holds the raw ROM bytes it loaded, so it calls
//! `rf_nes::NesRom::from_ines_bytes(&rom_bytes)` — the exact parser
//! `EmuStepper`/`NesBus` use internally, already `pub` at the crate root
//! — and passes `.chr_rom()` in here as a plain slice. That function does
//! its own slicing straight from the ROM *file* bytes
//! (`rf-nes/src/system/cartridge.rs`), not from the running PPU, so this
//! is a **static** view of CHR-ROM, with two consequences a panel must not
//! paper over:
//!
//! 1. `chr_is_ram()` (on the `NesRom`) is `true` whenever the header
//!    declares `chr_rom_size == 0` — there is no static pattern data for
//!    that cartridge at all; the ROM file simply doesn't contain it (CHR
//!    RAM is written by the running game, which is exactly the live state
//!    this crate cannot reach). Callers must show "no CHR data" rather
//!    than decode a zeroed placeholder as if it were real tiles.
//! 2. For a bank-switched mapper (MMC1/UxROM/CNROM/MMC3, all in
//!    `rf_cart::nes::SUPPORTED_MAPPERS`), the file's CHR region holds
//!    **every** bank concatenated, not the 8 KiB currently windowed into
//!    `$0000-$1FFF`. This is a legitimate, Mesen-style "browse every CHR
//!    bank" view (`decode_pattern_table`'s `table`/tile-index parameters
//!    already work over however many 4 KiB halves `chr` actually holds),
//!    but it is **not** "what the PPU sees right now" — a panel must label
//!    it accordingly.
//!
//! ## 2bpp tile format (verified against
//! [nesdev.org/wiki/PPU_pattern_tables](https://www.nesdev.org/wiki/PPU_pattern_tables))
//!
//! Each tile is 16 bytes: an 8-byte "low" bitplane followed by an 8-byte
//! "high" bitplane, one byte per row. Pixel `(row, col)`'s 2-bit color
//! index is `(low_byte >> (7 - col)) & 1 | ((high_byte >> (7 - col)) & 1)
//! << 1` — bit 7 of each byte is the *leftmost* pixel.

/// One decoded 8x8 tile: each entry is a 2-bit color index (0-3), NOT yet
/// resolved to an RGB color — resolving requires a chosen 4-entry palette
/// (`crate::palette`), and a pattern viewer with no live CGRAM data (see
/// module doc) still has something to show without one.
pub type Tile = [[u8; 8]; 8];

/// Bytes per tile in the 2bpp format (module doc).
const TILE_BYTES: usize = 16;
/// Tiles per pattern-table half (`$0000-$0FFF`/`$1000-$1FFF` each hold
/// 256 tiles of 16 bytes).
const TILES_PER_HALF: usize = 256;
/// Bytes per pattern-table half.
const HALF_BYTES: usize = TILES_PER_HALF * TILE_BYTES;

/// Which 4 KiB half of an 8 KiB CHR window a tile index selects
/// (DEBUGGER.md: "both pattern tables"). For a multi-bank ROM CHR blob
/// (module doc point 2), `Left`/`Right` select the first/second 4 KiB of
/// *whichever* 8 KiB bank window `chr` was sliced to by the caller — this
/// module has no bank concept of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternTable {
    Left,
    Right,
}

/// Decode tile `tile_index` (0-255) out of `chr`, from the half selected by
/// `table`. Returns `None` if `chr` is too short to contain that tile
/// (covers both the "CHR RAM, zero bytes" and "short/malformed slice"
/// cases — a debugger viewer degrades to "no data" rather than panicking
/// on attacker- or corruption-controlled ROM bytes, matching this
/// codebase's established "never crash on ROM bytes" stance, e.g.
/// `rf_renderer`'s scanline sinks).
#[must_use]
pub fn decode_tile(chr: &[u8], table: PatternTable, tile_index: u8) -> Option<Tile> {
    let half_offset = match table {
        PatternTable::Left => 0,
        PatternTable::Right => HALF_BYTES,
    };
    let start = half_offset + tile_index as usize * TILE_BYTES;
    let bytes = chr.get(start..start + TILE_BYTES)?;
    let mut tile = [[0u8; 8]; 8];
    for (row, tile_row) in tile.iter_mut().enumerate() {
        let low = bytes[row];
        let high = bytes[8 + row];
        for (col, pixel) in tile_row.iter_mut().enumerate() {
            let shift = 7 - col;
            let lo_bit = (low >> shift) & 1;
            let hi_bit = (high >> shift) & 1;
            *pixel = lo_bit | (hi_bit << 1);
        }
    }
    Some(tile)
}

/// Decode every whole tile present in `table`'s half of `chr` (up to the
/// full 256; fewer if `chr` is shorter than a whole half — e.g. a
/// truncated/malformed CHR blob). Empty for a `chr_is_ram()` cartridge with
/// an empty slice (module doc point 1) — never a placeholder tile set.
#[must_use]
pub fn decode_pattern_table(chr: &[u8], table: PatternTable) -> Vec<Tile> {
    (0..TILES_PER_HALF)
        .map(|i| decode_tile(chr, table, i as u8))
        .take_while(Option::is_some)
        .flatten()
        .collect()
}

/// Resolve a decoded [`Tile`] to RGBA bytes (`width * height * 4`, `8*8*4`
/// here) using a caller-chosen 4-entry palette (index 0 is conventionally
/// transparent/backdrop for a sprite tile, opaque backdrop color for a BG
/// tile — this function does not decide which; it just maps index to
/// color). `alpha` is `0` for index 0 and `255` otherwise, matching
/// `rf_renderer::LayeredFrame`'s "transparent where nothing drew"
/// convention so a pattern-viewer image composites sensibly over any
/// background.
#[must_use]
pub fn tile_to_rgba(tile: &Tile, palette: [[u8; 3]; 4]) -> [u8; 8 * 8 * 4] {
    let mut out = [0u8; 8 * 8 * 4];
    for (row, pixels) in tile.iter().enumerate() {
        for (col, &index) in pixels.iter().enumerate() {
            let [r, g, b] = palette[index as usize];
            let alpha = if index == 0 { 0 } else { 255 };
            let px = (row * 8 + col) * 4;
            out[px] = r;
            out[px + 1] = g;
            out[px + 2] = b;
            out[px + 3] = alpha;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tile whose bit-plane bytes are hand-chosen so every one of the
    /// four possible 2-bit indices appears at a distinct, known column,
    /// row 0 only (rows 1-7 left zero) — the vacuity-trap-required "known
    /// input -> known output at a known coordinate" proof, not just "it
    /// constructs".
    ///
    /// Column 0 (bit 7): low=0 high=0 -> index 0
    /// Column 1 (bit 6): low=1 high=0 -> index 1
    /// Column 2 (bit 5): low=0 high=1 -> index 2
    /// Column 3 (bit 4): low=1 high=1 -> index 3
    /// low byte binary:  0 1 0 1 0 0 0 0 = 0x50
    /// high byte binary: 0 0 1 1 0 0 0 0 = 0x30
    fn one_tile_known_row0() -> Vec<u8> {
        let mut bytes = vec![0u8; TILE_BYTES];
        bytes[0] = 0x50; // low plane, row 0
        bytes[8] = 0x30; // high plane, row 0
        bytes
    }

    #[test]
    fn decode_tile_places_each_of_the_four_indices_at_the_expected_column() {
        let chr = one_tile_known_row0();
        let tile = decode_tile(&chr, PatternTable::Left, 0).expect("tile 0 must decode");
        assert_eq!(tile[0][0], 0, "column 0 must be index 0");
        assert_eq!(tile[0][1], 1, "column 1 must be index 1");
        assert_eq!(tile[0][2], 2, "column 2 must be index 2");
        assert_eq!(tile[0][3], 3, "column 3 must be index 3");
        // Untouched columns/rows must decode to 0, not garbage.
        assert_eq!(tile[0][4], 0);
        assert_eq!(tile[1], [0u8; 8]);
    }

    #[test]
    fn decode_tile_indexes_the_right_hand_pattern_table_at_the_4kib_boundary() {
        // Left half all zero, right half holds `one_tile_known_row0`'s
        // bytes at tile index 0 of the RIGHT table — proves `PatternTable`
        // actually offsets by HALF_BYTES rather than being ignored.
        let mut chr = vec![0u8; HALF_BYTES + TILE_BYTES];
        chr[HALF_BYTES] = 0x50;
        chr[HALF_BYTES + 8] = 0x30;

        let left = decode_tile(&chr, PatternTable::Left, 0).expect("left half present");
        assert_eq!(left[0][1], 0, "left half must stay all-zero index 0");

        let right = decode_tile(&chr, PatternTable::Right, 0).expect("right half present");
        assert_eq!(right[0][1], 1, "right half must see the planted tile");
    }

    #[test]
    fn decode_tile_indexes_a_second_tile_at_its_16_byte_offset() {
        let mut chr = vec![0u8; TILE_BYTES * 2];
        // Tile 1 starts at byte 16.
        chr[16] = 0x50;
        chr[16 + 8] = 0x30;

        let tile0 = decode_tile(&chr, PatternTable::Left, 0).expect("tile 0 present");
        assert_eq!(tile0[0][1], 0, "tile 0 must be untouched");
        let tile1 = decode_tile(&chr, PatternTable::Left, 1).expect("tile 1 present");
        assert_eq!(tile1[0][1], 1, "tile 1 must see the planted bytes");
    }

    #[test]
    fn decode_tile_degrades_to_none_rather_than_panicking_on_short_chr() {
        assert_eq!(decode_tile(&[], PatternTable::Left, 0), None);
        assert_eq!(decode_tile(&[0u8; 10], PatternTable::Left, 0), None);
    }

    #[test]
    fn decode_pattern_table_on_empty_chr_ram_slice_is_empty_not_a_placeholder() {
        // The `chr_is_ram()` case (module doc point 1): an empty slice
        // must decode to an empty Vec, not 256 all-zero tiles that would
        // look like real (if blank) CHR-ROM content.
        assert!(decode_pattern_table(&[], PatternTable::Left).is_empty());
    }

    #[test]
    fn decode_pattern_table_stops_at_a_truncated_final_tile_not_padding_it() {
        // One and a half tiles' worth of bytes: exactly one whole tile
        // decodes, the trailing partial tile must NOT appear.
        let chr = vec![0u8; TILE_BYTES + 4];
        let tiles = decode_pattern_table(&chr, PatternTable::Left);
        assert_eq!(tiles.len(), 1);
    }

    #[test]
    fn decode_pattern_table_decodes_a_full_256_tile_half_when_present() {
        let chr = vec![0u8; HALF_BYTES];
        assert_eq!(decode_pattern_table(&chr, PatternTable::Left).len(), 256);
    }

    #[test]
    fn tile_to_rgba_maps_each_index_to_its_palette_entry_with_index_0_transparent() {
        let mut tile: Tile = [[0u8; 8]; 8];
        tile[0][0] = 0;
        tile[0][1] = 1;
        tile[0][2] = 2;
        tile[0][3] = 3;
        let palette = [[10, 20, 30], [40, 50, 60], [70, 80, 90], [100, 110, 120]];
        let rgba = tile_to_rgba(&tile, palette);
        assert_eq!(&rgba[0..4], &[10, 20, 30, 0], "index 0 must be transparent");
        assert_eq!(&rgba[4..8], &[40, 50, 60, 255]);
        assert_eq!(&rgba[8..12], &[70, 80, 90, 255]);
        assert_eq!(&rgba[12..16], &[100, 110, 120, 255]);
    }
}
