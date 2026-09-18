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

// -----------------------------------------------------------------------
// Mode 7, HDMA and DSP views (ticket W13-02c; DEBUGGER.md §3's remaining
// SNES cells).
// -----------------------------------------------------------------------

/// The mode-7 playfield is a fixed 128x128 tiles of 8x8 pixels.
pub const MODE7_SIDE: usize = 1024;

/// One pixel of the mode-7 playfield, at playfield coordinates.
///
/// **The tilemap is in the EVEN bytes of VRAM and the character data in
/// the ODD bytes of the same words** — interleaved, not two regions. A
/// viewer that read them as separate blocks would draw the map out of
/// tiles it never referenced.
///
/// Returns the raw 8bpp index; 0 is transparent, as everywhere else.
#[must_use]
pub fn mode7_pixel(vram: &[u8], px: u16, py: u16) -> u8 {
    if vram.is_empty() {
        return 0;
    }
    let px = px & 0x3FF;
    let py = py & 0x3FF;
    let tile_index = (py / 8) * 128 + px / 8;
    let tile = vram[(usize::from(tile_index) * 2) % vram.len()];
    mode7_character_pixel(vram, tile, px & 7, py & 7)
}

/// One pixel of a mode-7 character: 8bpp, read from the ODD bytes.
#[must_use]
pub fn mode7_character_pixel(vram: &[u8], tile: u8, x: u16, y: u16) -> u8 {
    if vram.is_empty() {
        return 0;
    }
    let at = (usize::from(tile) * 64 + usize::from(y) * 8 + usize::from(x)) * 2 + 1;
    vram[at % vram.len()]
}

/// Project a screen position onto the mode-7 playfield.
///
/// The single implementation of the matrix multiply — `ppu::mode7::
/// render_scanline` calls it for every sample it draws, and the debugger's
/// camera trapezoid calls it for the screen's four corners. Two callers,
/// one formula, so the outline a viewer draws cannot disagree with the
/// picture the renderer produced.
///
/// `cx_fixed` is screen x in 8.8 with `(hofs - x0) * 256` already folded
/// in; `cy` is the integer screen y with `vofs - y0` folded in. Products
/// are computed in `i64` because a perfectly ordinary matrix overflows
/// `i32` — see `render_scanline`'s own doc for the worked example.
#[must_use]
pub fn mode7_project(m: &crate::ppu::mode7::Mode7, cx_fixed: i32, cy: i32) -> (i32, i32) {
    let vx = ((i64::from(m.a) * i64::from(cx_fixed)) / 256) as i32
        + i32::from(m.b) * cy
        + (i32::from(m.x0) * 256);
    let vy = ((i64::from(m.c) * i64::from(cx_fixed)) / 256) as i32
        + i32::from(m.d) * cy
        + (i32::from(m.y0) * 256);
    (vx, vy)
}

/// Where the screen's four corners land on the playfield, clockwise from
/// top-left — DEBUGGER.md §3's "camera trapezoid".
///
/// A trapezoid rather than a rectangle because that is what a rotated or
/// perspective-scaled matrix produces, and seeing its shape is the entire
/// value of the view: a game whose corners cross each other has a matrix
/// that folds the plane over itself.
#[must_use]
pub fn mode7_camera_corners(
    m: &crate::ppu::mode7::Mode7,
    width: u16,
    height: u16,
) -> [(i32, i32); 4] {
    let corner = |sx: u16, sy: u16| {
        let sx = if m.flip_x { width - 1 - sx } else { sx };
        let sy = if m.flip_y { 255 - sy } else { sy };
        let cy = i32::from(sy) + i32::from(m.vofs) - i32::from(m.y0);
        let cx_fixed = i32::from(sx) * 256 + ((i32::from(m.hofs) - i32::from(m.x0)) * 256);
        let (vx, vy) = mode7_project(m, cx_fixed, cy);
        (vx >> 8, vy >> 8)
    };
    [
        corner(0, 0),
        corner(width - 1, 0),
        corner(width - 1, height - 1),
        corner(0, height - 1),
    ]
}

/// Promote a live Mode 7 register file to the generic, cross-console
/// frame-bundle shape (ticket W16-09; `rf_core_api::Mode7Registers`'s own
/// doc explains why the type carries no SNES-specific name). A pure field
/// copy — [`crate::ppu::mode7::Mode7`] stays the only place these values
/// are interpreted from hardware writes; this never becomes a second
/// source of truth for what they mean, only a shape conversion.
#[must_use]
pub fn mode7_registers(m: &crate::ppu::mode7::Mode7) -> rf_core_api::Mode7Registers {
    rf_core_api::Mode7Registers {
        a: m.a,
        b: m.b,
        c: m.c,
        d: m.d,
        x0: m.x0,
        y0: m.y0,
        hofs: m.hofs,
        vofs: m.vofs,
        flip_x: m.flip_x,
        flip_y: m.flip_y,
    }
}

/// Render `tiles_w`x`tiles_h` tiles of the Mode 7 playfield (starting at
/// playfield tile `(0, 0)`) to a plain RGBA8 texture, at `density`x the
/// hardware's native per-tile resolution — ticket W16-09's "ground
/// texture" input to `rf_renderer`'s diorama pass.
///
/// **bsnes-hd's documented approach, applied to the playfield instead of
/// the screen**: re-run the hardware's own sampling ([`mode7_pixel`] /
/// [`cgram_rgb`], unchanged) at a finer step rather than filtering or
/// inventing detail between real samples — the same stance
/// `crate::ppu::mode7::render_scanline`'s own doc takes for the
/// SCREEN-space HD path ("no smoothing, interpolation or invented
/// detail"). `density` repeats each native pixel as a `density`x`density`
/// block of identical colour: there is no more information in an 8x8,
/// 8bpp-indexed character than that, so "HD" here can only mean
/// supersampling the existing data.
///
/// Output is `tiles_w * 8 * density` by `tiles_h * 8 * density` pixels,
/// row-major, straight-alpha RGBA8 — transparent (`[0, 0, 0, 0]`)
/// wherever [`mode7_pixel`] reads palette index 0, the same "index 0 is
/// nothing drawn here" rule every other layer in this codebase follows
/// (Mode 7 has no backdrop colour of its own).
#[must_use]
pub fn render_mode7_plane_rgba(
    vram: &[u8],
    cgram: &[u8],
    tiles_w: u16,
    tiles_h: u16,
    density: u32,
) -> Vec<u8> {
    let density = density.max(1) as usize;
    let out_w = usize::from(tiles_w) * 8 * density;
    let out_h = usize::from(tiles_h) * 8 * density;
    let mut out = vec![0u8; out_w * out_h * 4];
    for oy in 0..out_h {
        let py = (oy / density) as u16;
        for ox in 0..out_w {
            let px = (ox / density) as u16;
            let index = mode7_pixel(vram, px, py);
            if index == 0 {
                continue; // Already zeroed -- transparent, no tile pixel here.
            }
            let [r, g, b] = cgram_rgb(cgram, index);
            let at = (oy * out_w + ox) * 4;
            out[at] = r;
            out[at + 1] = g;
            out[at + 2] = b;
            out[at + 3] = 255;
        }
    }
    out
}

/// Which HDMA channels transferred on each scanline of the last frame.
///
/// One byte per hardware line, bit `n` set when channel `n` ran a transfer
/// unit on it — DEBUGGER.md §3's "HDMA channel lanes per scanline".
///
/// Recorded unconditionally rather than behind the debug capture, and the
/// measurement is why: it is 262 bytes and at most eight bit-ORs per line,
/// against the ~245 KB frame the same loop is already producing. Gating it
/// would cost more in branch and plumbing than it saves.
pub type HdmaLanes = Vec<u8>;

/// One DSP voice, reduced to what a viewer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceView {
    pub index: u8,
    pub srcn: u8,
    /// Where this voice's BRR data starts in ARAM, as resolved at key-on.
    pub start: u16,
    pub loop_addr: u16,
    pub vol_left: i8,
    pub vol_right: i8,
    /// `$1000` is 1.0 — 32 kHz playback.
    pub pitch: u16,
    pub keyed_on: bool,
    pub envelope_level: u16,
    pub last_output: i16,
}

/// Reduce the DSP's eight voices to what a viewer shows.
///
/// A projection rather than a borrow, because this crosses a thread on the
/// frame message like every other debug view (law 4).
#[must_use]
pub fn voice_views(dsp: &crate::apu::dsp::Dsp) -> Vec<VoiceView> {
    dsp.voices
        .iter()
        .enumerate()
        .map(|(i, v)| VoiceView {
            index: i as u8,
            srcn: v.srcn,
            start: v.start,
            loop_addr: v.loop_addr,
            vol_left: v.vol_left,
            vol_right: v.vol_right,
            pitch: v.pitch,
            keyed_on: v.keyed_on,
            // The DSP's level is 11-bit and signed in the struct; a
            // viewer wants the magnitude, and a negative level is a
            // transient the envelope clamps rather than something to show
            // as a huge unsigned number.
            envelope_level: v.envelope.level.max(0) as u16,
            last_output: v.last_output,
        })
        .collect()
}

/// One decoded BRR block: nine bytes in, sixteen samples out.
///
/// **A BRR block is 9 bytes, not 8**: one header plus eight of packed
/// nibbles. The header carries the shift, the filter, and the end/loop
/// flags — a decoder that assumed 8-byte blocks would drift a byte per
/// block and turn a sample into noise within a few dozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrrBlock {
    pub shift: u8,
    pub filter: u8,
    pub end: bool,
    pub loops: bool,
    pub samples: [i16; 16],
}

/// Decode one BRR block from ARAM at `addr`.
///
/// `prev` is the two previous samples the filters need (`(older, newer)`),
/// which is why decoding a run of blocks has to be sequential rather than
/// random-access: filters 1-3 are recursive.
#[must_use]
pub fn decode_brr_block(aram: &[u8], addr: u16, prev: (i16, i16)) -> BrrBlock {
    let at = usize::from(addr);
    let byte = |i: usize| aram.get((at + i) % aram.len().max(1)).copied().unwrap_or(0);
    let header = byte(0);
    let shift = header >> 4;
    let filter = (header >> 2) & 0x03;

    let (mut older, mut newer) = prev;
    let mut samples = [0i16; 16];
    for (i, slot) in samples.iter_mut().enumerate() {
        let packed = byte(1 + i / 2);
        let nibble = if i.is_multiple_of(2) {
            packed >> 4
        } else {
            packed & 0x0F
        };
        // The nibble is SIGNED 4-bit, so 8..15 are negative.
        let mut s = i32::from((nibble as i8) << 4 >> 4);
        // Shift 13-15 are invalid on hardware and behave as a very large
        // shift of the sign bit; clamping keeps a corrupt sample from
        // producing a wild value in a viewer.
        s = if shift <= 12 {
            (s << shift) >> 1
        } else {
            (s >> 3) << 11
        };
        let (o, n) = (i32::from(older), i32::from(newer));
        s += match filter {
            1 => n + ((-n) >> 4),
            2 => (n * 2) + ((-n * 3) >> 5) - o + (o >> 4),
            3 => (n * 2) + ((-n * 13) >> 6) - o + ((o * 3) >> 4),
            _ => 0,
        };
        let clamped = s.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        *slot = clamped;
        older = newer;
        newer = clamped;
    }
    BrrBlock {
        shift,
        filter,
        end: header & 0x01 != 0,
        loops: header & 0x02 != 0,
        samples,
    }
}

#[cfg(test)]
mod w13_02c_tests {
    use super::*;
    use crate::ppu::mode7::Mode7;

    /// The interleave: tilemap in the EVEN bytes, characters in the ODD
    /// bytes of the same words.
    #[test]
    fn the_mode7_playfield_is_interleaved_not_two_regions() {
        let mut vram = vec![0u8; 0x10000];
        // Tilemap entry (0,0) -> tile 2, at the EVEN byte of word 0.
        vram[0] = 2;
        // Tile 2, pixel (0,0) -> colour 0x55, at the ODD byte.
        vram[(2 * 64) * 2 + 1] = 0x55;
        assert_eq!(mode7_pixel(&vram, 0, 0), 0x55);
        // The even byte beside it is tilemap data, not a pixel.
        assert_eq!(mode7_character_pixel(&vram, 2, 0, 0), 0x55);
        // The playfield wraps at 1024.
        assert_eq!(mode7_pixel(&vram, 1024, 1024), 0x55);
    }

    /// The identity matrix projects the screen onto itself, so the
    /// trapezoid is the screen rectangle — the case where a sign error is
    /// most visible.
    #[test]
    fn an_identity_matrix_gives_a_rectangle_the_size_of_the_screen() {
        let m = Mode7 {
            a: 0x0100,
            b: 0,
            c: 0,
            d: 0x0100,
            ..Mode7::default()
        };
        let corners = mode7_camera_corners(&m, 256, 224);
        assert_eq!(corners[0], (0, 0));
        assert_eq!(corners[1], (255, 0));
        assert_eq!(corners[2], (255, 223));
        assert_eq!(corners[3], (0, 223));
    }

    /// A rotation produces a genuine trapezoid — the whole point of
    /// drawing the outline rather than a rectangle.
    #[test]
    fn a_rotated_matrix_produces_corners_that_are_not_axis_aligned() {
        let m = Mode7 {
            a: 0x00B5,
            b: 0xFF4B_u16 as i16,
            c: 0x00B5,
            d: 0x00B5,
            ..Mode7::default()
        };
        let corners = mode7_camera_corners(&m, 256, 224);
        assert_ne!(
            corners[0].1, corners[1].1,
            "the top edge must not stay flat under rotation"
        );
    }

    /// A BRR block is NINE bytes: one header plus eight of nibble pairs.
    /// The filter-0 case is the one with no recursion, so it pins the
    /// unpacking on its own.
    #[test]
    fn a_brr_block_is_nine_bytes_and_filter_zero_is_pure_unpacking() {
        let mut aram = vec![0u8; 0x10000];
        // shift 0, filter 0, no flags.
        aram[0] = 0x00;
        // First nibble 1, second nibble -1 (0xF).
        aram[1] = 0x1F;
        let block = decode_brr_block(&aram, 0, (0, 0));
        assert_eq!(block.shift, 0);
        assert_eq!(block.filter, 0);
        assert!(!block.end && !block.loops);
        // shift 0 is `(s << 0) >> 1`, so 1 -> 0 and -1 -> -1.
        assert_eq!(block.samples[0], 0);
        assert_eq!(block.samples[1], -1);

        // The header's low two bits are the END and LOOP flags.
        aram[0] = 0x03;
        let block = decode_brr_block(&aram, 0, (0, 0));
        assert!(block.end && block.loops);

        // The NINTH byte belongs to this block, not the next one: a
        // decoder assuming 8 would read the next block's header as data.
        aram[0] = 0x40; // shift 4
        aram[8] = 0x70;
        let block = decode_brr_block(&aram, 0, (0, 0));
        assert_ne!(block.samples[14], 0, "byte 8 supplies samples 14 and 15");
    }

    /// Ticket W16-09: a pure field copy, nothing decided twice.
    #[test]
    fn mode7_registers_is_a_plain_promotion_of_every_field() {
        let m = Mode7 {
            a: 0x0140,
            b: -256,
            c: 12,
            d: 0x00F0,
            x0: 100,
            y0: -50,
            hofs: 7,
            vofs: -7,
            flip_x: true,
            flip_y: false,
            ..Mode7::default()
        };
        let r = mode7_registers(&m);
        assert_eq!(
            r,
            rf_core_api::Mode7Registers {
                a: 0x0140,
                b: -256,
                c: 12,
                d: 0x00F0,
                x0: 100,
                y0: -50,
                hofs: 7,
                vofs: -7,
                flip_x: true,
                flip_y: false,
            }
        );
    }

    /// Ticket W16-09: density 1 must reproduce `mode7_pixel`/`cgram_rgb`
    /// exactly, pixel for pixel — the "hardware path is bit-identical"
    /// stance `render_scanline`'s own doc takes for the screen-space HD
    /// path, restated here for the playfield-space texture.
    #[test]
    fn render_mode7_plane_rgba_at_density_one_matches_mode7_pixel_and_cgram_rgb() {
        let mut vram = vec![0u8; 0x10000];
        vram[0] = 2; // tilemap (0,0) -> tile 2
        vram[(2 * 64) * 2 + 1] = 0x55; // tile 2, pixel (0,0) -> index 0x55
        let mut cgram = vec![0u8; 512];
        let word: u16 = 0x1F << 10; // pure blue, per cgram_rgb's own test
        cgram[0x55 * 2] = word as u8;
        cgram[0x55 * 2 + 1] = (word >> 8) as u8;

        let rgba = render_mode7_plane_rgba(&vram, &cgram, 1, 1, 1);
        assert_eq!(rgba.len(), 8 * 8 * 4);
        assert_eq!(&rgba[0..4], &[0, 0, 255, 255], "pixel (0,0) is opaque blue");

        // A tile pixel this test never wrote is index 0 -> transparent.
        let at = (0 * 8 + 4) * 4; // pixel (4, 0)
        assert_eq!(&rgba[at..at + 4], &[0, 0, 0, 0]);
    }

    /// Ticket W16-09: "HD" is supersampling, never invented detail — each
    /// source pixel becomes an NxN block of the SAME colour.
    #[test]
    fn render_mode7_plane_rgba_density_n_repeats_blocks_not_new_detail() {
        let mut vram = vec![0u8; 0x10000];
        vram[0] = 1;
        vram[64 * 2 + 1] = 0x03;
        let mut cgram = vec![0u8; 512];
        cgram[0x03 * 2] = 0xFF;
        cgram[0x03 * 2 + 1] = 0x7F; // full-scale in every channel

        let density = 3;
        let rgba = render_mode7_plane_rgba(&vram, &cgram, 1, 1, density);
        let side = 8 * density as usize;
        assert_eq!(rgba.len(), side * side * 4);
        // Every pixel in the top-left density x density block must be the
        // identical opaque colour -- a block, not a gradient.
        let first = &rgba[0..4];
        assert_ne!(first[3], 0, "must be opaque");
        for oy in 0..density as usize {
            for ox in 0..density as usize {
                let at = (oy * side + ox) * 4;
                assert_eq!(
                    &rgba[at..at + 4],
                    first,
                    "({ox},{oy}) must match the block colour"
                );
            }
        }
    }
}
