//! Mode 7: the affine-transformed background (ticket W7-04;
//! `docs/design/EMULATION_CORES.md` §3.3, FR-CORE-034).
//!
//! ## The transform
//!
//! Mode 7 replaces tilemap scrolling with a 2x2 matrix applied per pixel.
//! For screen position `(sx, sy)`:
//!
//! ```text
//! [ vx ]   [ A B ] [ sx + HOFS - X0 ]     [ X0 ]
//! [ vy ] = [ C D ] [ sy + VOFS - Y0 ]  +  [ Y0 ]
//! ```
//!
//! `A`-`D` are **signed 8.8 fixed point** — `$0100` is 1.0 — so the
//! products carry eight fractional bits that are shifted off to reach a
//! playfield coordinate. `X0`/`Y0` (`$211F`/`$2120`) are the centre the
//! rotation happens about, and every one of `HOFS`, `VOFS`, `X0` and `Y0`
//! is a **13-bit signed** value, not a 16-bit one. Sign-extending them
//! wrongly is the classic mode-7 bug: the picture is recognisable but
//! swims away from the centre it should pivot around.
//!
//! ## The playfield
//!
//! 128x128 tiles of 8x8 pixels — 1024x1024 — stored **interleaved** in
//! VRAM: the tilemap in the even bytes, character data in the odd bytes
//! of the same words. A decoder that reads them as two separate regions
//! gets tile indices that look like pixel data.
//!
//! ## Screen-over (`$211A` bits 6-7)
//!
//! What happens outside the 1024x1024 playfield:
//!
//! | bits | behaviour |
//! |------|-----------|
//! | `00`, `01` | wrap — the playfield repeats infinitely |
//! | `10` | transparent — nothing is drawn |
//! | `11` | the character at tile 0 is repeated |
//!
//! `10` and `11` differ only outside the playfield, which is exactly
//! where most test content never looks — so they are asserted separately.

use super::Ppu;

/// Mode 7 register state.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mode7 {
    /// `$211B`-`$211E`, signed 8.8 fixed point.
    pub a: i16,
    pub b: i16,
    pub c: i16,
    pub d: i16,
    /// `$211F`/`$2120` centre of rotation, 13-bit signed.
    pub x0: i16,
    pub y0: i16,
    /// `$210D`/`$210E` written while in mode 7 — also 13-bit signed.
    pub hofs: i16,
    pub vofs: i16,
    /// `$211A` bit 0 / bit 1.
    pub flip_x: bool,
    pub flip_y: bool,
    /// `$211A` bits 6-7.
    pub screen_over: u8,
    /// The shared write-twice latch every mode-7 register goes through.
    pub latch: u8,
}

/// Sign-extend a 13-bit value.
///
/// `HOFS`, `VOFS`, `X0` and `Y0` are 13-bit, so bit 12 is the sign and
/// bits 13-15 are not part of the number at all.
#[must_use]
pub fn sign_extend_13(v: u16) -> i16 {
    let v = v & 0x1FFF;
    if v & 0x1000 != 0 {
        (v | 0xE000) as i16
    } else {
        v as i16
    }
}

impl Mode7 {
    /// Write one of `$211A`-`$2120`.
    ///
    /// The matrix registers and the centre/scroll registers share ONE
    /// latch, so a write to `$211B` changes what the next `$211F` write
    /// produces. Modelling them as independent 16-bit registers looks
    /// right until a game writes them in an unusual order.
    pub fn write_register(&mut self, offset: u16, value: u8) {
        match offset {
            0x211A => {
                self.flip_x = value & 0x01 != 0;
                self.flip_y = value & 0x02 != 0;
                self.screen_over = (value >> 6) & 0x03;
            }
            0x211B..=0x211E => {
                let word = (u16::from(value) << 8) | u16::from(self.latch);
                self.latch = value;
                let v = word as i16;
                match offset {
                    0x211B => self.a = v,
                    0x211C => self.b = v,
                    0x211D => self.c = v,
                    _ => self.d = v,
                }
            }
            0x211F | 0x2120 => {
                let word = (u16::from(value) << 8) | u16::from(self.latch);
                self.latch = value;
                let v = sign_extend_13(word);
                if offset == 0x211F {
                    self.x0 = v;
                } else {
                    self.y0 = v;
                }
            }
            _ => {}
        }
    }

    /// `$2134`-`$2136` MPYL/MPYM/MPYH — the signed product of `M7A`
    /// (16-bit) and the **high byte** of `M7B` (8-bit), as 24 bits.
    ///
    /// This is a general-purpose multiplier games use for arithmetic that
    /// has nothing to do with mode 7, which is why it is readable in every
    /// mode rather than only this one.
    #[must_use]
    pub fn product(&self) -> i32 {
        i32::from(self.a) * i32::from((self.b >> 8) as i8)
    }
}

/// One transformed scanline of palette indices, `None` where transparent.
pub type Mode7Scanline = Vec<Option<u8>>;

/// Render scanline `y` of the mode-7 playfield.
///
/// `samples_per_pixel` is the **HD-Mode-7 density**: 1 is hardware, and
/// larger values evaluate the identical matrix at proportionally finer
/// steps, returning a correspondingly wider scanline. There is no
/// smoothing, interpolation or invented detail — the extra samples are
/// real evaluations of the same transform, which is exactly what makes
/// this class of enhancement faithful rather than a filter.
///
/// **Accuracy Mode always passes 1** (law 6: enhancements are opt-in
/// overlays over an unmodified simulation, and a fresh install boots in
/// Accuracy Mode). [`Ppu::render_scanline`] never passes anything else;
/// only a caller that has explicitly opted in does.
#[must_use]
pub fn render_scanline(ppu: &Ppu, y: u16, samples_per_pixel: u32) -> Mode7Scanline {
    let m = &ppu.mode7;
    let width = super::WIDTH as u32 * samples_per_pixel;
    let mut out = Vec::with_capacity(width as usize);

    let sy = if m.flip_y { 255 - y } else { y };
    let cy = i32::from(sy) + i32::from(m.vofs) - i32::from(m.y0);

    for i in 0..width {
        // Sub-pixel stepping: at density 1 this is exactly the integer
        // screen x, so the hardware path is bit-identical rather than
        // "the HD path with a scale of one".
        let sx_num = if m.flip_x {
            (width - 1 - i) as i32
        } else {
            i as i32
        };
        // Screen x in 8.8, so a density of N steps 256/N per sample.
        let sx_fixed = (sx_num * 256) / samples_per_pixel as i32;
        let cx_fixed = sx_fixed + ((i32::from(m.hofs) - i32::from(m.x0)) * 256);

        // **The products are computed in i64, and that is a fix, not a
        // widening for tidiness.** `a` and `c` are 8.8 signed (up to
        // +-32767) and `cx_fixed` carries `(hofs - x0) * 256`, so a
        // perfectly ordinary matrix overflows i32: Perspective's line 0
        // presents a = 20480 (80.0) with x0 = 512 and hofs = 0, giving
        // cx_fixed = -131072 and a product of -2,684,354,560 — past
        // i32::MIN, and a debug-build PANIC on registers the ROM is
        // entitled to set.
        //
        // Nothing visible changes. Every sample that overflowed lands
        // outside the 0..1024 playfield whether the product wraps or is
        // exact, so screen-over resolves it to transparent either way;
        // RotZoom, Perspective and StarWars all hash byte-identically
        // before and after. It was a latent crash, not a wrong picture,
        // which is why a release build never noticed and the goldens
        // could be pinned over it.
        let vx = ((i64::from(m.a) * i64::from(cx_fixed)) / 256) as i32
            + i32::from(m.b) * cy
            + (i32::from(m.x0) * 256);
        let vy = ((i64::from(m.c) * i64::from(cx_fixed)) / 256) as i32
            + i32::from(m.d) * cy
            + (i32::from(m.y0) * 256);

        out.push(sample(ppu, vx >> 8, vy >> 8));
    }
    out
}

/// Sample the playfield at a pixel coordinate, applying screen-over.
fn sample(ppu: &Ppu, px: i32, py: i32) -> Option<u8> {
    let outside = !(0..1024).contains(&px) || !(0..1024).contains(&py);
    let (px, py) = match (outside, ppu.mode7.screen_over) {
        // Transparent outside the playfield.
        (true, 2) => return None,
        // Repeat character 0 outside the playfield.
        (true, 3) => {
            let colour = character_pixel(ppu, 0, (px & 7) as u16, (py & 7) as u16);
            return (colour != 0).then_some(colour);
        }
        // Wrap: the playfield repeats.
        _ => (px & 0x3FF, py & 0x3FF),
    };

    // Tilemap in the EVEN bytes, character data in the ODD bytes of the
    // same VRAM words — interleaved, not two separate regions.
    let tile_index = ((py as u16) / 8) * 128 + (px as u16) / 8;
    let tile = ppu.vram[(usize::from(tile_index) * 2) % ppu.vram.len()];
    let colour = character_pixel(ppu, tile, (px & 7) as u16, (py & 7) as u16);
    (colour != 0).then_some(colour)
}

/// One pixel of a mode-7 character: 8bpp, read from the ODD bytes.
fn character_pixel(ppu: &Ppu, tile: u8, x: u16, y: u16) -> u8 {
    let at = (usize::from(tile) * 64 + usize::from(y) * 8 + usize::from(x)) * 2 + 1;
    ppu.vram[at % ppu.vram.len()]
}

impl Mode7 {
    /// Serialise the matrix (ticket W7-09).
    ///
    /// `latch` is the shared write-twice latch every matrix register
    /// feeds. It is one byte of real machine state: a state saved between
    /// the two halves of an `M7A` write and restored without it would
    /// combine the new high byte with a zero low byte.
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        for v in [
            self.a, self.b, self.c, self.d, self.x0, self.y0, self.hofs, self.vofs,
        ] {
            o.i16(v)?;
        }
        o.bool(self.flip_x)?;
        o.bool(self.flip_y)?;
        o.u8(self.screen_over)?;
        o.u8(self.latch)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        self.a = i.i16()?;
        self.b = i.i16()?;
        self.c = i.i16()?;
        self.d = i.i16()?;
        self.x0 = i.i16()?;
        self.y0 = i.i16()?;
        self.hofs = i.i16()?;
        self.vofs = i.i16()?;
        self.flip_x = i.bool()?;
        self.flip_y = i.bool()?;
        self.screen_over = i.u8()?;
        self.latch = i.u8()?;
        Ok(())
    }
}
