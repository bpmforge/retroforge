//! Background tile rendering for modes 0 and 1 (ticket W6-03a).
//!
//! ## What differs between the two modes
//!
//! Only the bit depths and the layer count:
//!
//! | mode | BG1  | BG2  | BG3  | BG4  |
//! |------|------|------|------|------|
//! | 0    | 2bpp | 2bpp | 2bpp | 2bpp |
//! | 1    | 4bpp | 4bpp | 2bpp | —    |
//!
//! Mode 0's four layers each get their own block of 8 palettes — BG1
//! takes palettes 0-7, BG2 8-15, BG3 16-23, BG4 24-31 — which is why
//! four 2bpp layers can coexist without fighting over CGRAM. Mode 1's
//! layers all index from palette 0. Getting that offset wrong produces a
//! picture with correct shapes in entirely wrong colours, which reads as
//! a palette bug rather than a mode bug.
//!
//! ## Tilemap geometry
//!
//! A tilemap is 32×32 entries of 16 bits. `$2107`-`$210A` bits 0-1 select
//! whether the layer is one such map, two side by side, two stacked, or
//! four — and the extra screens are consecutive 1 KiB blocks after the
//! base, in the order left/right then top/bottom. With 16×16 tiles the
//! same map covers twice the pixels in each direction.

use super::{BgLayer, Ppu, WIDTH};

/// One background's contribution to a scanline.
#[derive(Debug, Clone)]
pub struct BgScanline {
    /// Palette index per x, or `None` where the tile pixel is
    /// transparent (colour 0).
    pub pixels: [Option<u8>; WIDTH],
    /// The tilemap priority bit of whatever produced that pixel.
    pub priority: [u8; WIDTH],
}

impl Default for BgScanline {
    fn default() -> Self {
        Self {
            pixels: [None; WIDTH],
            priority: [0; WIDTH],
        }
    }
}

/// All four layers for one scanline.
#[derive(Debug, Clone, Default)]
pub struct BgScanlines {
    pub layers: [BgScanline; 4],
}

/// Bits per pixel for each layer in the active mode. `0` means the mode
/// has no such layer.
///
/// | mode | BG1  | BG2  | BG3  | BG4  | notes                    |
/// |------|------|------|------|------|--------------------------|
/// | 0    | 2bpp | 2bpp | 2bpp | 2bpp | four layers, four palette blocks |
/// | 1    | 4bpp | 4bpp | 2bpp | —    | BG3 priority bit         |
/// | 2    | 4bpp | 4bpp | —    | —    | offset-per-tile          |
/// | 3    | 8bpp | 4bpp | —    | —    | direct colour available  |
/// | 4    | 8bpp | 2bpp | —    | —    | offset-per-tile + direct colour |
/// | 5    | 4bpp | 2bpp | —    | —    | hires 512                |
/// | 6    | 4bpp | —    | —    | —    | offset-per-tile + hires  |
///
/// In modes 2, 4 and 6, BG3 still HAS a tilemap — it just is not drawn.
/// It carries the per-column scroll offsets instead, which is why those
/// rows read "—" here and yet [`offset_per_tile`] reads BG3's map.
#[must_use]
pub fn bit_depths(mode: u8) -> [u8; 4] {
    match mode {
        1 => [4, 4, 2, 0],
        2 => [4, 4, 0, 0],
        3 => [8, 4, 0, 0],
        4 => [8, 2, 0, 0],
        5 => [4, 2, 0, 0],
        6 => [4, 0, 0, 0],
        // Mode 0, and mode 7 until W7-04 implements it. Rendering an
        // unimplemented mode as mode 0 gives a visibly wrong picture
        // rather than a dead PPU — and the golden runner refuses to hash
        // a mode the PPU does not claim, so this can never be mistaken
        // for correct output.
        _ => [2, 2, 2, 2],
    }
}

/// Does this mode use BG3's tilemap as per-column scroll offsets?
#[must_use]
pub fn uses_offset_per_tile(mode: u8) -> bool {
    matches!(mode, 2 | 4 | 6)
}

/// The palette block each layer's colours are offset into.
///
/// Mode 0 alone separates the layers; every other mode indexes from 0.
#[must_use]
pub fn palette_base(mode: u8, bg: usize) -> u8 {
    if mode == 0 {
        (bg as u8) * 32
    } else {
        0
    }
}

/// Render every enabled background for scanline `y`.
#[must_use]
pub fn render_backgrounds(ppu: &Ppu, y: u16) -> BgScanlines {
    let mut out = BgScanlines::default();
    let depths = bit_depths(ppu.bg_mode);
    for (bg, &depth) in depths.iter().enumerate() {
        if depth == 0 || !ppu.bgs[bg].enabled {
            continue;
        }
        out.layers[bg] = render_layer(ppu, bg, y, depth, palette_base(ppu.bg_mode, bg));
    }
    out
}

fn render_layer(ppu: &Ppu, bg_index: usize, y: u16, depth: u8, palette_base: u8) -> BgScanline {
    let bg = &ppu.bgs[bg_index];
    let mut out = BgScanline::default();
    let tile_px = if bg.tile_size_16 { 16u16 } else { 8 };

    for x in 0..WIDTH {
        // Offset-per-tile replaces this column's scroll wholesale rather
        // than adding to it (modes 2/4/6 only; a no-op elsewhere).
        let column = (x as u16) / 8;
        let offsets = offset_per_tile(ppu, bg_index, column);
        let hofs = offsets.h.unwrap_or(bg.hofs);
        let vofs = offsets.v.unwrap_or(bg.vofs);

        // Mosaic snaps the SOURCE coordinate, so a block of pixels all
        // fetch the same texel. Snapping the output instead would blur
        // rather than blockify.
        let mx = ppu.mosaic.snap(bg_index, x as u16);
        let my = ppu.mosaic.snap(bg_index, y);
        let world_x = mx.wrapping_add(hofs);
        let world_y = my.wrapping_add(vofs);

        let entry = tilemap_entry(ppu, bg, world_x, world_y, tile_px);
        let character = entry & 0x03FF;
        let palette = ((entry >> 10) & 0x07) as u8;
        let priority = ((entry >> 13) & 0x01) as u8;
        let flip_x = entry & 0x4000 != 0;
        let flip_y = entry & 0x8000 != 0;

        // Position within the tile, honouring flips.
        let mut px = world_x % tile_px;
        let mut py = world_y % tile_px;
        if flip_x {
            px = tile_px - 1 - px;
        }
        if flip_y {
            py = tile_px - 1 - py;
        }

        // A 16×16 tile is four 8×8 characters laid out 2×2, with the
        // second row a full 16 characters further on — not 2, because the
        // character map is 16 wide.
        let sub = (px / 8) + (py / 8) * 16;
        let character = (character + sub) & 0x03FF;
        let colour = fetch_pixel(ppu, bg.char_base, character, px % 8, py % 8, depth);

        if colour != 0 {
            // 8bpp addresses all 256 CGRAM entries directly, so the
            // tilemap's palette field is IGNORED — applying it would fold
            // a 256-colour image into a 32-colour block.
            let index = match depth {
                8 => colour,
                4 => palette_base.wrapping_add(palette.wrapping_mul(16).wrapping_add(colour)),
                _ => palette_base.wrapping_add(palette.wrapping_mul(4).wrapping_add(colour)),
            };
            out.pixels[x] = Some(index);
            out.priority[x] = priority;
        }
    }
    out
}

/// Fetch the tilemap entry covering a world pixel.
///
/// The `& 1` terms are the screen-selection: a 64-wide map is two 1 KiB
/// screens side by side, a 64-tall map is two stacked, and a 64×64 map is
/// all four in the order top-left, top-right, bottom-left, bottom-right.
fn tilemap_entry(ppu: &Ppu, bg: &BgLayer, world_x: u16, world_y: u16, tile_px: u16) -> u16 {
    let tile_x = world_x / tile_px;
    let tile_y = world_y / tile_px;

    let wide = bg.tilemap_size & 0x01 != 0;
    let tall = bg.tilemap_size & 0x02 != 0;

    let screen_x = if wide { (tile_x / 32) & 1 } else { 0 };
    let screen_y = if tall { (tile_y / 32) & 1 } else { 0 };
    let screen = screen_x + screen_y * if wide { 2 } else { 1 };

    let within = (tile_y % 32) * 32 + (tile_x % 32);
    ppu.vram_word(
        bg.tilemap_base
            .wrapping_add(screen * 0x400)
            .wrapping_add(within),
    )
}

/// Read one pixel's colour index out of character data.
///
/// SNES tiles are **bitplane-interleaved in pairs**, and the pairs are 8
/// words apart: planes 0/1 at the tile's start, planes 2/3 at +8, planes
/// 4/5 at +16, planes 6/7 at +24. A 4bpp tile is therefore not "two 2bpp
/// tiles" in memory order, and an 8bpp tile is not "two 4bpp tiles" —
/// which is the mistake that makes graphics come out with the right
/// shapes in scrambled colours.
#[must_use]
pub fn fetch_pixel(ppu: &Ppu, char_base: u16, character: u16, px: u16, py: u16, depth: u8) -> u8 {
    let words_per_tile = match depth {
        8 => 32u16,
        4 => 16,
        _ => 8,
    };
    let tile_word = char_base.wrapping_add(character.wrapping_mul(words_per_tile));
    let bit = 7 - px;

    let mut colour = 0u16;
    // One pass per bitplane PAIR: 1 pair for 2bpp, 2 for 4bpp, 4 for 8bpp.
    for pair in 0..(depth / 2) {
        let word = ppu.vram_word(tile_word.wrapping_add(u16::from(pair) * 8).wrapping_add(py));
        let lo = (word >> bit) & 1;
        let hi = (word >> (8 + bit)) & 1;
        colour |= lo << (pair * 2);
        colour |= hi << (pair * 2 + 1);
    }
    colour as u8
}

/// Per-column scroll offsets read from BG3's tilemap (modes 2, 4 and 6).
///
/// ## What offset-per-tile actually does
///
/// In these modes BG3 is not drawn. Its tilemap instead supplies, for
/// each 8-pixel COLUMN of the screen, a replacement horizontal and/or
/// vertical scroll value for BG1 and BG2. That is how a game bends a
/// background into columns that scroll at different rates without any
/// per-scanline interrupt work.
///
/// The entry format is the same 16 bits a tilemap entry always has, read
/// differently:
///
/// * bits 0-9 — the offset value
/// * bit 13 — apply to BG1
/// * bit 14 — apply to BG2
/// * bit 15 — **mode 4 only**: 0 = this entry is horizontal, 1 = vertical
///
/// Modes 2 and 6 read TWO entries per column — horizontal first, then
/// vertical 32 entries later — because their format has no bit-15
/// selector. Mode 4 reads one and lets bit 15 choose. Getting that
/// difference wrong is the classic offset-per-tile bug: the picture
/// looks almost right, with the vertical offsets applied horizontally.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColumnOffset {
    pub h: Option<u16>,
    pub v: Option<u16>,
}

/// Resolve the offset entry covering screen column `column` for layer
/// `bg` (0 = BG1, 1 = BG2).
#[must_use]
pub fn offset_per_tile(ppu: &Ppu, bg: usize, column: u16) -> ColumnOffset {
    let mut out = ColumnOffset::default();
    if !uses_offset_per_tile(ppu.bg_mode) || column == 0 {
        // The first column has no preceding entry to read, so hardware
        // leaves it on the layer's ordinary scroll.
        return out;
    }
    let offset_layer = &ppu.bgs[2];
    // BG3's own scroll selects which part of the offset table this
    // column reads — the offsets scroll with it.
    let index = (column - 1).wrapping_add(offset_layer.hofs / 8) & 0x1F;
    let base = offset_layer.tilemap_base;
    let applies = |entry: u16| entry & (0x2000 << bg) != 0;

    if ppu.bg_mode == 4 {
        let entry = ppu.vram_word(base.wrapping_add(index));
        if applies(entry) {
            if entry & 0x8000 == 0 {
                out.h = Some(entry & 0x03FF);
            } else {
                out.v = Some(entry & 0x03FF);
            }
        }
    } else {
        let h_entry = ppu.vram_word(base.wrapping_add(index));
        let v_entry = ppu.vram_word(base.wrapping_add(index).wrapping_add(32));
        if applies(h_entry) {
            out.h = Some(h_entry & 0x03FF);
        }
        if applies(v_entry) {
            out.v = Some(v_entry & 0x03FF);
        }
    }
    out
}
