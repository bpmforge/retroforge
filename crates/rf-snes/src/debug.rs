//! Debug-facing decoders over raw SNES memory (ticket W13-02b;
//! `docs/design/DEBUGGER.md` §3's SNES column).
//!
//! ## Why these live in `rf-snes` and not in `rf-debugger`
//!
//! `rf-debugger` may not depend on a core (`scripts/validate-arch.sh`
//! rule 3), and its NES decoders get away with it because NES CHR is
//! *just bytes*: two bitplanes, eight bytes apart, no register state.
//! SNES tile data is not. A pixel's value depends on the layer's bit
//! depth (2/4/8, and which one depends on the BG mode), and the bitplane
//! pairs are interleaved at +0, +8, +16, +24 words — "a 4bpp tile is not
//! two 2bpp tiles in memory order", as [`crate::ppu::bg::fetch_pixel`]'s
//! own doc puts it, and getting it wrong produces the right shapes in
//! scrambled colours.
//!
//! Reimplementing that in `rf-debugger` over raw bytes would be a second
//! copy of this crate's most easily-mis-transcribed knowledge, free to
//! drift from the renderer that actually draws the game. So it lives
//! here, next to the renderer, and [`crate::ppu::bg::fetch_pixel`]
//! delegates to [`tile_pixel`] — **one implementation, two callers**.
//!
//! ## Everything here takes byte slices, not `&Ppu`
//!
//! The debugger reads a **snapshot** that travelled to the UI thread on a
//! frame message (project law 4: nothing reaches into a running core).
//! Taking `&[u8]` rather than `&Ppu` is what makes these callable on that
//! snapshot at all — and it keeps them trivially testable without
//! building a machine.

/// One 16-bit VRAM word, wrapping like the hardware's address counter.
#[must_use]
pub fn vram_word(vram: &[u8], word_addr: u16) -> u16 {
    if vram.is_empty() {
        return 0;
    }
    let at = usize::from(word_addr) * 2 % vram.len();
    u16::from(vram[at]) | (u16::from(vram[at + 1]) << 8)
}

/// The palette index of one pixel of one tile.
///
/// `depth` is 2, 4 or 8 — from [`crate::ppu::bg::bit_depths`] for a BG, or
/// always 4 for OBJ. `char_base` and `character` are in words, matching
/// the registers they come from.
///
/// **The bitplane pairs are interleaved**: planes 0/1 at +0 words, 2/3 at
/// +8, 4/5 at +16, 6/7 at +24. This is the single implementation of that
/// layout in the crate.
#[must_use]
pub fn tile_pixel(vram: &[u8], char_base: u16, character: u16, px: u16, py: u16, depth: u8) -> u8 {
    let words_per_tile = match depth {
        8 => 32u16,
        4 => 16,
        _ => 8,
    };
    let tile_word = char_base.wrapping_add(character.wrapping_mul(words_per_tile));
    let bit = 7 - (px & 7);

    let mut colour = 0u16;
    for pair in 0..(depth / 2) {
        let word = vram_word(
            vram,
            tile_word
                .wrapping_add(u16::from(pair) * 8)
                .wrapping_add(py & 7),
        );
        let lo = (word >> bit) & 1;
        let hi = (word >> (8 + bit)) & 1;
        colour |= lo << (pair * 2);
        colour |= hi << (pair * 2 + 1);
    }
    colour as u8
}

/// One tilemap cell, as the 16-bit entry encodes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TilemapEntry {
    pub character: u16,
    pub palette: u8,
    /// `$2107`-style bit 13 — draws above the layer's normal priority.
    pub priority: bool,
    pub flip_x: bool,
    pub flip_y: bool,
}

/// Decode the tilemap cell at `(tx, ty)` in tiles.
///
/// `size` is the register's 0-3 code: 0 = 32×32, 1 = 64×32, 2 = 32×64,
/// 3 = 64×64. **The quadrants of a large map are not contiguous**: each
/// 32×32 screen is 0x400 words, and a 64-wide map puts the right half at
/// +0x400, a 64-tall one the bottom half at +0x400 (or +0x800 when both).
/// A viewer that ignored that would show the same quadrant four times.
#[must_use]
pub fn tilemap_entry(vram: &[u8], base: u16, size: u8, tx: u16, ty: u16) -> TilemapEntry {
    let wide = size & 1 != 0;
    let tall = size & 2 != 0;
    let x = tx & if wide { 0x3F } else { 0x1F };
    let y = ty & if tall { 0x3F } else { 0x1F };

    let mut screen = 0u16;
    if wide && x >= 32 {
        screen += 1;
    }
    if tall && y >= 32 {
        screen += if wide { 2 } else { 1 };
    }
    let word = base
        .wrapping_add(screen * 0x400)
        .wrapping_add((y & 0x1F) * 32)
        .wrapping_add(x & 0x1F);
    let entry = vram_word(vram, word);
    TilemapEntry {
        character: entry & 0x03FF,
        palette: ((entry >> 10) & 0x07) as u8,
        priority: entry & 0x2000 != 0,
        flip_x: entry & 0x4000 != 0,
        flip_y: entry & 0x8000 != 0,
    }
}

/// How many tiles wide and tall a tilemap of `size` is.
#[must_use]
pub fn tilemap_dimensions(size: u8) -> (u16, u16) {
    (
        if size & 1 != 0 { 64 } else { 32 },
        if size & 2 != 0 { 64 } else { 32 },
    )
}

/// One CGRAM colour as RGB888.
///
/// CGRAM is BGR555 — five bits each, **blue in the high bits**, which is
/// the byte order `$2122` writes and the opposite of what a reader
/// expecting RGB would guess. Each channel is scaled by `*255/31` rather
/// than shifted left by 3, so full-scale 31 becomes 255 and not 248.
#[must_use]
pub fn cgram_rgb(cgram: &[u8], index: u8) -> [u8; 3] {
    let at = usize::from(index) * 2;
    if at + 1 >= cgram.len() {
        return [0, 0, 0];
    }
    let word = u16::from(cgram[at]) | (u16::from(cgram[at + 1]) << 8);
    let ch = |shift: u16| -> u8 { ((((word >> shift) & 0x1F) * 255) / 31) as u8 };
    [ch(0), ch(5), ch(10)]
}

/// One OAM entry, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteEntry {
    pub index: u8,
    /// Signed: a sprite can hang off the left edge.
    pub x: i16,
    pub y: u8,
    pub tile: u16,
    pub palette: u8,
    pub priority: u8,
    pub flip_x: bool,
    pub flip_y: bool,
    /// From the high table's size bit — which of the two configured sizes
    /// this sprite uses.
    pub large: bool,
    /// `attr` bit 0: the second 4 KiB character page.
    pub second_page: bool,
}

/// The two sprite sizes `$2101` bits 5-7 select, in pixels.
#[must_use]
pub fn obj_sizes(select: u8) -> ((u16, u16), (u16, u16)) {
    match select & 0x07 {
        0 => ((8, 8), (16, 16)),
        1 => ((8, 8), (32, 32)),
        2 => ((8, 8), (64, 64)),
        3 => ((16, 16), (32, 32)),
        4 => ((16, 16), (64, 64)),
        5 => ((32, 32), (64, 64)),
        6 => ((16, 32), (32, 64)),
        _ => ((16, 32), (32, 32)),
    }
}

/// Decode all 128 sprites from an OAM snapshot.
///
/// `oam` is 544 bytes: 512 of four-byte entries, then the 32-byte **high
/// table** that packs X's ninth bit and the size select two bits per
/// sprite, four sprites to a byte. A decoder that stopped at 512 would
/// put every sprite past x=255 on the wrong side of the screen and give
/// them all the same size.
#[must_use]
pub fn decode_oam(oam: &[u8]) -> Vec<SpriteEntry> {
    (0..128u8)
        .map(|index| {
            let base = usize::from(index) * 4;
            let get = |i: usize| oam.get(i).copied().unwrap_or(0);
            let low_x = get(base);
            let y = get(base + 1);
            let tile_low = get(base + 2);
            let attr = get(base + 3);

            let high_byte = get(0x0200 + usize::from(index) / 4);
            let shift = (index % 4) * 2;
            let x_high = (high_byte >> shift) & 1;
            let large = (high_byte >> (shift + 1)) & 1 != 0;

            let x9 = (u16::from(x_high) << 8) | u16::from(low_x);
            let x = if x9 >= 256 {
                x9 as i16 - 512
            } else {
                x9 as i16
            };

            SpriteEntry {
                index,
                x,
                y,
                tile: u16::from(tile_low) | (u16::from(attr & 0x01) << 8),
                palette: (attr >> 1) & 0x07,
                priority: (attr >> 4) & 0x03,
                flip_x: attr & 0x40 != 0,
                flip_y: attr & 0x80 != 0,
                large,
                second_page: attr & 0x01 != 0,
            }
        })
        .collect()
}

/// Sprites per scanline, and whether the hardware's 32-per-line limit was
/// exceeded — the SNES equivalent of the NES viewer's 8-per-line bar.
///
/// The `$213E` range-over flag the PPU reports is the authority for a
/// *rendered* frame; this answers the same question for an arbitrary line
/// the user is pointing at, which is what a viewer needs.
pub const OBJ_PER_LINE_LIMIT: usize = 32;

/// Count the sprites covering scanline `y`.
#[must_use]
pub fn line_occupancy(sprites: &[SpriteEntry], y: u16, obj_size: u8) -> usize {
    let (small, big) = obj_sizes(obj_size);
    sprites
        .iter()
        .filter(|s| {
            let (_, height) = if s.large { big } else { small };
            // A sprite's Y wraps: one at y=250 with height 32 covers the
            // top of the screen, which is how games slide sprites in from
            // above.
            let top = u16::from(s.y);
            let dy = y.wrapping_sub(top) & 0xFF;
            dy < height
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interleaved bitplane layout, in the shape that gets it wrong:
    /// a 4bpp tile is NOT two 2bpp tiles in memory order.
    #[test]
    fn a_4bpp_pixel_reads_planes_from_plus_0_and_plus_8_words() {
        let mut vram = vec![0u8; 0x10000];
        // Tile 0 at char base 0. Row 0: planes 0/1 at word 0, planes 2/3
        // at word 8. Set the top bit of plane 0 and of plane 3.
        vram[0] = 0x80; // plane 0, row 0
        vram[8 * 2 + 1] = 0x80; // plane 3, row 0
        let px = tile_pixel(&vram, 0, 0, 0, 0, 4);
        assert_eq!(px, 0b1001, "plane 0 and plane 3, not planes 0 and 1");
        // The same bytes read as 2bpp see only the first pair.
        assert_eq!(tile_pixel(&vram, 0, 0, 0, 0, 2), 0b01);
    }

    #[test]
    fn tilemap_quadrants_are_not_contiguous() {
        let mut vram = vec![0u8; 0x10000];
        let put = |v: &mut Vec<u8>, word: usize, value: u16| {
            v[word * 2] = value as u8;
            v[word * 2 + 1] = (value >> 8) as u8;
        };
        put(&mut vram, 0, 0x0001); // screen 0, tile (0,0)
        put(&mut vram, 0x400, 0x0002); // screen 1 — the right half
        put(&mut vram, 0x800, 0x0003); // screen 2 — the bottom half of a 64x64

        // 64x32: x=32 is the second screen, not word 32.
        assert_eq!(tilemap_entry(&vram, 0, 1, 0, 0).character, 1);
        assert_eq!(tilemap_entry(&vram, 0, 1, 32, 0).character, 2);
        // 32x64: y=32 is the second screen.
        assert_eq!(tilemap_entry(&vram, 0, 2, 0, 32).character, 2);
        // 64x64: the bottom-left quadrant is the THIRD screen.
        assert_eq!(tilemap_entry(&vram, 0, 3, 0, 32).character, 3);
        assert_eq!(tilemap_dimensions(3), (64, 64));
        assert_eq!(tilemap_dimensions(0), (32, 32));
    }

    #[test]
    fn a_tilemap_entry_carries_flip_priority_and_palette() {
        let mut vram = vec![0u8; 0x1000];
        // character 0x123, palette 5, priority, both flips.
        let entry: u16 = 0x123 | (5 << 10) | 0x2000 | 0x4000 | 0x8000;
        vram[0] = entry as u8;
        vram[1] = (entry >> 8) as u8;
        let cell = tilemap_entry(&vram, 0, 0, 0, 0);
        assert_eq!(cell.character, 0x123);
        assert_eq!(cell.palette, 5);
        assert!(cell.priority && cell.flip_x && cell.flip_y);
    }

    /// CGRAM is BGR555 with blue in the HIGH bits — the opposite of what a
    /// reader expecting RGB would guess.
    #[test]
    fn cgram_is_bgr555_and_full_scale_is_255_not_248() {
        // Pure blue: bits 10-14 set.
        let blue: u16 = 0x1F << 10;
        let cgram = [blue as u8, (blue >> 8) as u8];
        assert_eq!(cgram_rgb(&cgram, 0), [0, 0, 255]);
        // Pure red: bits 0-4.
        let red: u16 = 0x1F;
        let cgram = [red as u8, (red >> 8) as u8];
        assert_eq!(cgram_rgb(&cgram, 0), [255, 0, 0]);
        // Out of range reads black rather than panicking.
        assert_eq!(cgram_rgb(&cgram, 200), [0, 0, 0]);
    }

    /// The high table is where X's ninth bit and the size select live. A
    /// decoder that stopped at 512 bytes would put every sprite past
    /// x=255 on the wrong side of the screen.
    #[test]
    fn the_oam_high_table_supplies_the_ninth_x_bit_and_the_size() {
        let mut oam = vec![0u8; 544];
        oam[0] = 0x10; // sprite 0 low x
        oam[1] = 0x20; // y
                       // Sprite 0's high-table bits: x8 = 1, large = 1.
        oam[0x200] = 0b11;
        let sprites = decode_oam(&oam);
        // X is a SIGNED 9-bit value: 0x110 is 272, which is >= 256, so it
        // means -240 — the sprite hangs off the LEFT edge. This matches
        // `crate::ppu::obj::decode_sprite`, which is the point: the
        // debugger must not disagree with the renderer about where a
        // sprite is.
        assert_eq!(sprites[0].x, -240);
        assert!(sprites[0].large);
        assert_eq!(sprites[0].y, 0x20);
        // Sprite 1 shares that byte and must not inherit its bits.
        assert_eq!(sprites[1].x, 0);
        assert!(!sprites[1].large);
        assert_eq!(sprites.len(), 128);
    }

    #[test]
    fn line_occupancy_counts_covering_sprites_including_ones_that_wrap() {
        let mut oam = vec![0u8; 544];
        // Three 8x8 sprites on line 16, and one at y=250 that wraps.
        for i in 0..3usize {
            oam[i * 4 + 1] = 16;
        }
        oam[3 * 4 + 1] = 250;
        let sprites = decode_oam(&oam);
        // Only the three at y=16. The other 125 default to y=0 with
        // height 8, so they cover lines 0-7 and not this one.
        assert_eq!(line_occupancy(&sprites, 16, 0), 3);
        // The one at y=250 with height 8 covers 250-257 — line 252 is
        // inside it, and nothing else is.
        assert_eq!(line_occupancy(&sprites, 252, 0), 1);
        // Nothing is on a line no sprite reaches.
        assert_eq!(line_occupancy(&sprites, 100, 0), 0);
    }
}
