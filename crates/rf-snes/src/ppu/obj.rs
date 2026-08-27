//! OBJ (sprite) rendering, including the two per-line limits
//! (ticket W6-03a; `docs/design/EMULATION_CORES.md` §3.3).
//!
//! ## Two limits, and they are independent
//!
//! The SNES drops sprites for two different reasons, and a renderer that
//! implements one is not most of the way to implementing both:
//!
//! * **32 sprites per line.** Range evaluation walks all 128 OAM entries
//!   and keeps at most 32 that intersect the line. The 33rd is dropped
//!   however narrow it is. Sets `range_over` (`$213E` bit 6).
//! * **34 tile slivers per line.** Each kept sprite then costs one sliver
//!   per 8 pixels of its width — one for an 8-wide sprite, **eight** for
//!   a 64-wide one. Five 64-wide sprites are only five sprites but forty
//!   slivers, so they blow this budget while passing the count entirely.
//!   Sets `time_over` (`$213E` bit 7).
//!
//! Because a 64-wide sprite costs 8× what an 8-wide one does, a single
//! test that trips both limits at once would pass with either implemented
//! alone. They are tested separately.
//!
//! ## Evaluation order and OAMADDR rotation
//!
//! Evaluation starts at the sprite `$2102`/`$2103` selects, not at sprite
//! 0, and wraps. That is the hardware's priority rotation: which sprites
//! survive the limits depends on where the walk began, which is exactly
//! how games spread flicker around instead of always dropping the same
//! sprite. Sliver accounting then runs over the kept set in **reverse**,
//! so it is the earliest-evaluated (highest-priority) sprites that
//! survive a sliver overflow.
//!
//! ## Dropped sprites are reported, never drawn
//!
//! [`ObjScanline::dropped`] carries what the limits suppressed, for the
//! enhancement layer. It is deliberately a separate array from
//! [`ObjScanline::pixels`]: writing a dropped sprite into the real pixel
//! stream would displace what the CRT actually showed. See the `ppu`
//! module doc.

use super::{Ppu, MAX_WIDTH, OAM_LEN, WIDTH};

/// Maximum sprites evaluated onto one scanline.
pub const MAX_SPRITES_PER_LINE: usize = 32;
/// Maximum 8-pixel tile slivers fetched for one scanline.
pub const MAX_SLIVERS_PER_LINE: usize = 34;

/// One sprite, decoded from OAM.
#[derive(Debug, Clone, Copy)]
pub struct Sprite {
    pub index: u8,
    pub x: i16,
    pub y: u8,
    pub tile: u16,
    pub palette: u8,
    pub priority: u8,
    pub flip_x: bool,
    pub flip_y: bool,
    pub width: u16,
    pub height: u16,
    /// `$2101` name-select applies only to the second character page.
    pub second_page: bool,
}

/// The eight OBJ size pairs selected by `$2101` bits 5-7.
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

/// Decode sprite `index` from OAM.
#[must_use]
pub fn decode_sprite(ppu: &Ppu, index: u8) -> Sprite {
    let base = usize::from(index) * 4;
    let low_x = ppu.oam[base];
    let y = ppu.oam[base + 1];
    let tile_low = ppu.oam[base + 2];
    let attr = ppu.oam[base + 3];

    // The high table packs two bits per sprite: X's ninth bit and the
    // size select. Four sprites share each byte.
    let high_byte = ppu.oam[0x0200 + usize::from(index) / 4];
    let shift = (index % 4) * 2;
    let x_high = (high_byte >> shift) & 1;
    let large = (high_byte >> (shift + 1)) & 1 != 0;

    let (small, big) = obj_sizes(ppu.obj_size);
    let (width, height) = if large { big } else { small };

    // X is a signed 9-bit value: a sprite can hang off the left edge.
    let x9 = (u16::from(x_high) << 8) | u16::from(low_x);
    let x = if x9 >= 256 {
        x9 as i16 - 512
    } else {
        x9 as i16
    };

    Sprite {
        index,
        x,
        y,
        tile: u16::from(tile_low),
        palette: (attr >> 1) & 0x07,
        priority: (attr >> 4) & 0x03,
        flip_x: attr & 0x40 != 0,
        flip_y: attr & 0x80 != 0,
        width,
        height,
        second_page: attr & 0x01 != 0,
    }
}

/// One scanline's OBJ output.
#[derive(Debug, Clone)]
pub struct ObjScanline {
    /// `(palette index, sprite id)` per x, for sprites that survived.
    /// Sized to [`MAX_WIDTH`]; only the first `width` entries of a given
    /// render are meaningful.
    pub pixels: [Option<(u8, u8)>; MAX_WIDTH],
    pub priority: [u8; MAX_WIDTH],
    /// Palette index per x for sprites the limits DROPPED. Never merged
    /// into `pixels` — see the module doc.
    pub dropped: [Option<u8>; MAX_WIDTH],
    pub range_over: bool,
    pub time_over: bool,
}

impl Default for ObjScanline {
    fn default() -> Self {
        Self {
            pixels: [None; MAX_WIDTH],
            priority: [0; MAX_WIDTH],
            dropped: [None; MAX_WIDTH],
            range_over: false,
            time_over: false,
        }
    }
}

/// Evaluate and render sprites for scanline `y`.
#[must_use]
pub fn render_objects(ppu: &Ppu, y: u16, width: usize) -> ObjScanline {
    // Sprites keep their 256-space X and are shifted right by the same
    // pad the backgrounds use, so a sprite standing in the middle of the
    // picture stays in the middle. A sprite that hardware would clip at
    // the screen edge becomes visible in the widened area — which is the
    // honest consequence of showing more of the world, and exactly the
    // kind of thing a game's own profile may decline.
    let pad = (width.saturating_sub(WIDTH) / 2) as i16;
    let mut out = ObjScanline::default();
    if !ppu.obj_enabled {
        return out;
    }

    // Priority rotation: evaluation starts at the sprite OAMADDR selects.
    let first = if ppu.oam_priority_rotation {
        ((ppu.oam_addr >> 1) & 0x7F) as u8
    } else {
        0
    };

    // --- range evaluation: at most 32 sprites intersect the line -------
    let mut in_range: Vec<Sprite> = Vec::with_capacity(MAX_SPRITES_PER_LINE);
    let mut range_dropped: Vec<Sprite> = Vec::new();
    for step in 0..128u16 {
        let index = ((u16::from(first) + step) % 128) as u8;
        let s = decode_sprite(ppu, index);
        if !intersects(&s, y) {
            continue;
        }
        if in_range.len() < MAX_SPRITES_PER_LINE {
            in_range.push(s);
        } else {
            out.range_over = true;
            range_dropped.push(s);
        }
    }

    // --- sliver budget: at most 34 eight-pixel columns ------------------
    //
    // Counted over the kept set in REVERSE, so the earliest-evaluated
    // (highest-priority) sprites are the ones that survive an overflow.
    let mut slivers = 0usize;
    let mut drawable: Vec<Sprite> = Vec::with_capacity(in_range.len());
    let mut sliver_dropped: Vec<Sprite> = Vec::new();
    for s in in_range.iter().rev() {
        let cost = usize::from(s.width / 8);
        if slivers + cost <= MAX_SLIVERS_PER_LINE {
            slivers += cost;
            drawable.push(*s);
        } else {
            out.time_over = true;
            sliver_dropped.push(*s);
        }
    }

    // Draw survivors lowest-priority first so the earliest-evaluated
    // sprite ends up on top.
    for s in &drawable {
        draw_sprite(ppu, s, y, &mut out, false, width, pad);
    }
    for s in range_dropped.iter().chain(sliver_dropped.iter()) {
        draw_sprite(ppu, s, y, &mut out, true, width, pad);
    }
    out
}

fn intersects(s: &Sprite, y: u16) -> bool {
    // Sprite Y wraps at 256, so one near the bottom reappears at the top.
    let top = u16::from(s.y);
    let dy = y.wrapping_sub(top) & 0xFF;
    dy < s.height
}

fn draw_sprite(
    ppu: &Ppu,
    s: &Sprite,
    y: u16,
    out: &mut ObjScanline,
    dropped: bool,
    width: usize,
    pad: i16,
) {
    let row = {
        let dy = y.wrapping_sub(u16::from(s.y)) & 0xFF;
        if s.flip_y {
            s.height - 1 - dy
        } else {
            dy
        }
    };

    for dx in 0..s.width {
        let x = s.x + dx as i16 + pad;
        if x < 0 || x >= width as i16 {
            continue;
        }
        let x = x as usize;
        let col = if s.flip_x { s.width - 1 - dx } else { dx };

        // OBJ character data is a 16×16 grid of 8×8 tiles, so moving down
        // one tile row advances the character number by 16, not by the
        // sprite's width in tiles.
        let character = (s.tile + (row / 8) * 16 + (col / 8)) & 0x01FF;
        let base = ppu.obj_name_base << 13
            | if s.second_page {
                (ppu.obj_name_select + 1) << 12
            } else {
                0
            };
        let colour = super::bg::fetch_pixel(ppu, base, character, col % 8, row % 8, 4);
        if colour == 0 {
            continue;
        }
        // OBJ palettes are the upper half of CGRAM: 128 + 16 per palette.
        let index = 128 + s.palette * 16 + colour;

        if dropped {
            if out.dropped[x].is_none() {
                out.dropped[x] = Some(index);
            }
        } else {
            // Unconditional overwrite is deliberate: `drawable` is
            // ordered lowest-priority-first (it was built by walking the
            // kept set in reverse), so the LAST write at each x comes
            // from the earliest-evaluated sprite — which is the one
            // hardware shows.
            out.pixels[x] = Some((index, s.index));
            out.priority[x] = s.priority;
        }
    }
}

/// Total OAM bytes, re-exported so callers do not hardcode it.
#[must_use]
pub const fn oam_len() -> usize {
    OAM_LEN
}
