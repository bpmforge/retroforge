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

/// Bits per pixel for each layer in the active mode.
#[must_use]
pub fn bit_depths(mode: u8) -> [u8; 4] {
    match mode {
        1 => [4, 4, 2, 0],
        // Mode 0 and, for now, anything this ticket does not implement:
        // rendering as mode 0 is a visible, debuggable wrong picture,
        // where rendering nothing would look like a dead PPU.
        _ => [2, 2, 2, 2],
    }
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
        out.layers[bg] = render_layer(ppu, &ppu.bgs[bg], y, depth, palette_base(ppu.bg_mode, bg));
    }
    out
}

fn render_layer(ppu: &Ppu, bg: &BgLayer, y: u16, depth: u8, palette_base: u8) -> BgScanline {
    let mut out = BgScanline::default();
    let tile_px = if bg.tile_size_16 { 16u16 } else { 8 };

    for x in 0..WIDTH {
        let world_x = (x as u16).wrapping_add(bg.hofs);
        let world_y = y.wrapping_add(bg.vofs);

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
            let colours_per_palette = if depth == 4 { 16 } else { 4 };
            out.pixels[x] = Some(palette_base.wrapping_add(palette * colours_per_palette + colour));
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
/// SNES tiles are **bitplane-interleaved**, and not uniformly: planes 0
/// and 1 sit together at the top of the tile, and planes 2 and 3 follow
/// 16 bytes later. A 4bpp tile is therefore not "two 2bpp tiles" in
/// memory order, which is the mistake that makes 4bpp graphics come out
/// with the right shapes in scrambled colours.
#[must_use]
pub fn fetch_pixel(ppu: &Ppu, char_base: u16, character: u16, px: u16, py: u16, depth: u8) -> u8 {
    let words_per_tile = if depth == 4 { 16u16 } else { 8 };
    let tile_word = char_base.wrapping_add(character.wrapping_mul(words_per_tile));
    let bit = 7 - px;

    let plane01 = ppu.vram_word(tile_word.wrapping_add(py));
    let mut colour = ((plane01 >> bit) & 1) | (((plane01 >> (8 + bit)) & 1) << 1);
    if depth == 4 {
        let plane23 = ppu.vram_word(tile_word.wrapping_add(8).wrapping_add(py));
        colour |= (((plane23 >> bit) & 1) << 2) | (((plane23 >> (8 + bit)) & 1) << 3);
    }
    colour as u8
}
