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

use super::{BgLayer, Ppu, MAX_WIDTH, WIDTH};

/// How wide this render is, and whether this layer may use it.
///
/// One value rather than two parameters: the width and the permission are
/// never meaningful apart — a widened frame with a layer that declined is
/// still a widened frame, and a caller that passed one without the other
/// would silently widen a layer policy had refused.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub width: usize,
    pub widen: bool,
}

impl Span {
    /// Where the 256-dot picture starts inside `width`.
    #[must_use]
    pub fn pad(self) -> i32 {
        (self.width.saturating_sub(WIDTH) / 2) as i32
    }
}

/// One background's contribution to a scanline.
#[derive(Debug, Clone)]
pub struct BgScanline {
    /// Palette index per x, or `None` where the tile pixel is
    /// transparent (colour 0).
    /// Sized to [`MAX_WIDTH`]; only the first `width` entries of a given
    /// render are meaningful. A fixed array keeps this off the heap on a
    /// per-scanline path.
    pub pixels: [Option<u8>; MAX_WIDTH],
    /// The tilemap priority bit of whatever produced that pixel.
    pub priority: [u8; MAX_WIDTH],
}

impl Default for BgScanline {
    fn default() -> Self {
        Self {
            pixels: [None; MAX_WIDTH],
            priority: [0; MAX_WIDTH],
        }
    }
}

/// All four layers for one scanline.
#[derive(Debug, Clone, Default)]
pub struct BgScanlines {
    pub layers: [BgScanline; 4],
}

/// Which half-dot of a **true hires** line this fetch samples.
///
/// Modes 5 and 6 fetch backgrounds at DOUBLE the horizontal rate: one
/// 512-wide picture, whose even columns are composed into the sub screen
/// and odd columns into the main screen. Both screens still produce 256
/// dots, so nothing downstream of this module changes — only *which*
/// 512-column each dot samples. Interleaving the two 256-wide results
/// (sub on the even half-dot, main on the odd) reconstructs the one
/// 512-wide picture the hardware drew.
///
/// **This is the difference between true hires and pseudo-hires, and
/// getting it wrong is not a subtle error.** Pseudo-hires (`SETINI` bit
/// 3) really is two independent 256-wide screens interleaved, so it uses
/// [`HiresPhase::None`] for both. Rendering modes 5/6 that way instead
/// makes the sub and main screens fetch the same BG at the same scroll,
/// so the 512 picture becomes the 256 picture with every column
/// DUPLICATED. Measured on `InterlaceFont` before this existed: 55,842 of
/// 57,344 half-dot pairs identical (97.4%), each plane individually
/// sharp, glyph edges serrated wherever the remaining 2.6% disagreed.
///
/// Scroll is doubled with the space: bsnes shifts `hscroll` left by one
/// for hires, so `BGnHOFS` stays a 256-space value and this module walks
/// 512-space. Tile width does NOT change — an 8-pixel tile is 8
/// half-dots, which is exactly why 64 tiles span the line instead of 32.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HiresPhase {
    /// Not a true-hires line: one BG pixel per dot, scroll in 256-space.
    /// Pseudo-hires uses this for BOTH screens — see above.
    #[default]
    None,
    /// Modes 5/6 SUB screen — the even (left) half-dot of each pair.
    Even,
    /// Modes 5/6 MAIN screen — the odd (right) half-dot.
    Odd,
}

impl HiresPhase {
    /// `(horizontal scale, half-dot offset)` for this phase.
    fn scale(self) -> (u16, u16) {
        match self {
            HiresPhase::None => (1, 0),
            HiresPhase::Even => (2, 0),
            HiresPhase::Odd => (2, 1),
        }
    }
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
///
/// `phase` is [`HiresPhase::None`] for every ordinary line; modes 5 and 6
/// call this twice, once per half-dot. See [`HiresPhase`].
#[must_use]
pub fn render_backgrounds(
    ppu: &Ppu,
    y: u16,
    phase: HiresPhase,
    width: usize,
    widen: [bool; 4],
) -> BgScanlines {
    let mut out = BgScanlines::default();
    let depths = bit_depths(ppu.bg_mode);
    for (bg, &depth) in depths.iter().enumerate() {
        if depth == 0 || !ppu.bgs[bg].enabled {
            continue;
        }
        out.layers[bg] = render_layer(
            ppu,
            bg,
            y,
            depth,
            palette_base(ppu.bg_mode, bg),
            phase,
            Span {
                width,
                widen: widen[bg],
            },
        );
    }
    out
}

fn render_layer(
    ppu: &Ppu,
    bg_index: usize,
    y: u16,
    depth: u8,
    palette_base: u8,
    phase: HiresPhase,
    span: Span,
) -> BgScanline {
    let (width, widen) = (span.width, span.widen);
    let bg = &ppu.bgs[bg_index];
    let mut out = BgScanline::default();
    // (2, 0) or (2, 1) on a true-hires line, (1, 0) otherwise — so every
    // expression below is the non-hires one unchanged when scale is 1.
    let (scale, half_dot) = phase.scale();

    // **Horizontal and vertical tile size are not the same thing in modes
    // 5 and 6.** `$2105`'s per-layer size bit selects 8×8 or 16×16
    // everywhere else, but in true hires it selects 16×8 or 16×16 — the
    // tile is SIXTEEN half-dots wide either way, carrying sixteen pixels
    // of character data, which is what makes the extra horizontal
    // resolution real. The tilemap therefore still spans the line in 32
    // tiles, exactly as it does at 256.
    //
    // Treating a hires tile as 8 half-dots wide is a specific and
    // recognisable failure: 64 tiles are read where 32 exist, so a 32-wide
    // tilemap wraps and **the whole line renders twice side by side**.
    // That is what this code did before the fix, and the doubled picture
    // is how it was caught.
    let hires = scale == 2;
    let tile_w = if hires || bg.tile_size_16 { 16u16 } else { 8 };
    let tile_h = if bg.tile_size_16 { 16u16 } else { 8 };

    // **Widening puts the extra columns EITHER SIDE of the 256-dot
    // picture, not on one end.** Output column `x` is 256-space column
    // `x - pad`, so the centre is byte-identical to a 256 render and the
    // new area is scenery the tilemap already contained. That is
    // bsnes-hd's model, and it is why this is a fetch and not a stretch:
    // `hofs + sx` addresses the tilemap modularly, so a negative or
    // over-wide `sx` lands on real tiles rather than off the end.
    let pad = span.pad();
    for x in 0..width {
        let sx = x as i32 - pad;
        // **A layer the policy declined to widen stops at the 4:3 frame.**
        // Leaving it out of the margins is what makes a refusal visible:
        // a HUD that should not scroll simply is not there, rather than
        // being smeared or repeated into the new columns.
        if !widen && !(0..WIDTH as i32).contains(&sx) {
            continue;
        }
        // Offset-per-tile replaces this column's scroll wholesale rather
        // than adding to it (modes 2/4/6 only; a no-op elsewhere).
        //
        // **The OPT column stays in 256-space and an OPT-supplied scroll
        // is NOT doubled**, while `bg.hofs` is. That is deliberate and it
        // is the one asymmetry here: the OPT fetch happens once per eight
        // DOTS at the dot rate, and `offset_per_tile` masks its index to
        // the 32 columns that implies, so handing it a 512-space column
        // would wrap it. Mode 6 is the only mode that is hires AND
        // offset-per-tile, and no ROM in this suite uses it — so this
        // pairing is UNVERIFIED and is written to leave the 256-space
        // behaviour exactly as it was rather than to guess at hires.
        let column = (sx.rem_euclid(256) / 8) as u16;
        let offsets = offset_per_tile(ppu, bg_index, column);
        let hofs = offsets.h.unwrap_or_else(|| bg.hofs.wrapping_mul(scale));
        let vofs = offsets.v.unwrap_or(bg.vofs);

        // Mosaic snaps the SOURCE coordinate, so a block of pixels all
        // fetch the same texel. Snapping the output instead would blur
        // rather than blockify.
        //
        // **The snap is taken in the SAME space the fetch walks** — 512
        // on a true-hires line, 256 otherwise — so a size-N block is N
        // half-dots of one colour.
        //
        // The alternative (snap the dot, then scale) was tried and is
        // wrong, which MosaicMode5 shows directly: it makes both
        // half-dots of a block resolve to the same *pair* of source
        // pixels rather than the same pixel, so every block fills with a
        // two-colour vertical stripe instead of a flat colour. That is a
        // mosaic that does not mosaic. Checked by rendering both and
        // looking at them, not by argument — the reasoning for the wrong
        // one was perfectly plausible.
        // `sx as u16` truncates a negative column to the top of the
        // 16-bit range, which is exactly the wrap the tilemap addressing
        // wants: column -1 fetches the tile at 65535, and `world_x`
        // masks down to the map's own size. So the left-hand extra
        // columns show the scenery that is genuinely to the left.
        let mx = ppu.mosaic.snap(
            bg_index,
            (sx as u16).wrapping_mul(scale).wrapping_add(half_dot),
        );
        let my = ppu.mosaic.snap(bg_index, y);
        let world_x = mx.wrapping_add(hofs);
        let world_y = my.wrapping_add(vofs);

        let entry = tilemap_entry(ppu, bg, world_x, world_y, tile_w, tile_h);
        let character = entry & 0x03FF;
        let palette = ((entry >> 10) & 0x07) as u8;
        let priority = ((entry >> 13) & 0x01) as u8;
        let flip_x = entry & 0x4000 != 0;
        let flip_y = entry & 0x8000 != 0;

        // Position within the tile, honouring flips.
        let mut px = world_x % tile_w;
        let mut py = world_y % tile_h;
        if flip_x {
            px = tile_w - 1 - px;
        }
        if flip_y {
            py = tile_h - 1 - py;
        }

        // A 16-wide tile is two 8×8 characters side by side, and a
        // 16-tall one repeats that a full 16 characters further on — not
        // 2, because the character map is 16 wide. The same expression
        // covers all four shapes this mode can ask for (8×8, 16×16, and
        // hires 16×8 / 16×16) because each term is zero when that
        // dimension is only 8 wide.
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
fn tilemap_entry(
    ppu: &Ppu,
    bg: &BgLayer,
    world_x: u16,
    world_y: u16,
    tile_w: u16,
    tile_h: u16,
) -> u16 {
    let tile_x = world_x / tile_w;
    let tile_y = world_y / tile_h;

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
    // Ticket W13-02b: ONE implementation of the interleaved bitplane
    // layout, shared with the debugger's tile viewer. The layout above is
    // the most easily mis-transcribed thing in this crate — a second copy
    // in `rf-debugger` would have been free to drift from the renderer
    // that actually draws the game.
    crate::debug::tile_pixel(&ppu.vram, char_base, character, px, py, depth)
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
