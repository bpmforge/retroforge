//! CPU-visible register read/write (`$2000-$2007`) and the loopy `v`/`t`/
//! `x`/`w` scroll-register bit manipulation
//! ([nesdev.org/wiki/PPU_scrolling](https://www.nesdev.org/wiki/PPU_scrolling)).
//!
//! `v`/`t` bit layout (nesdev's own diagram, reproduced verbatim):
//! ```text
//! yyy NN YYYYY XXXXX
//! ||| || ||||| +++++-- coarse X scroll
//! ||| || +++++-------- coarse Y scroll
//! ||| ++-------------- nametable select
//! +++----------------- fine Y scroll
//! ```
use super::{Ppu, STATUS_VBLANK};

impl Ppu {
    /// Dispatch a real (side-effecting) CPU read of register `index`
    /// (`addr & 7`). Only `$2002`/`$2004`/`$2007` are ever driven by the
    /// PPU; every other register is write-only and returns `open_bus`
    /// unchanged, matching real hardware (and the pre-existing `ppu_stub`
    /// behavior this ticket replaces, for the registers this ticket still
    /// doesn't implement reads for).
    pub fn read_register(&mut self, index: u8, open_bus: u8) -> u8 {
        match index {
            2 => self.read_status(open_bus),
            4 => self.oam[self.oam_addr as usize],
            7 => self.read_data(open_bus),
            _ => open_bus,
        }
    }

    /// Side-effect-free read of register `index`, for
    /// [`crate::system::NesBus::peek`] (the trace logger's disassembly-only
    /// path — see that method's doc for why it must never mutate PPU
    /// state). Mirrors [`Controller::peek_bit`](crate::system::Controller)'s
    /// existing peek/read split convention.
    pub fn peek_register(&self, index: u8, open_bus: u8) -> u8 {
        match index {
            2 => (self.status & 0xE0) | (open_bus & 0x1F),
            4 => self.oam[self.oam_addr as usize],
            7 => self.read_buffer,
            _ => open_bus,
        }
    }

    /// Dispatch a CPU write to register `index`.
    pub fn write_register(&mut self, index: u8, value: u8) {
        match index {
            0 => self.write_ctrl(value),
            1 => self.mask = value,
            3 => self.oam_addr = value,
            4 => self.write_oam_data(value),
            5 => self.write_scroll(value),
            6 => self.write_addr(value),
            7 => self.write_data(value),
            _ => {}
        }
    }

    /// `$2000` write: "t: ...GH.. ........ <- d: ......GH" (nesdev) — the
    /// nametable-select bits become `t` bits 10-11. The rest of `ctrl` is
    /// kept verbatim for the increment-size/pattern-table/NMI-enable bits.
    fn write_ctrl(&mut self, value: u8) {
        self.ctrl = value;
        self.t = (self.t & 0xF3FF) | (((value & 0x03) as u16) << 10);
    }

    /// `$2002` read: bits 7-5 are the real vblank/sprite-0/overflow flags,
    /// bits 4-0 pass through `open_bus` (undriven). Reading clears the
    /// vblank flag and resets the `w` write-toggle latch — nesdev.org/wiki/
    /// PPU_registers: "Reading this register has the side effect of
    /// clearing the PPU's internal w register" and reading vblank "return[s]
    /// the current state of this flag and then clear[s] it". Sprite-0-hit
    /// and overflow are NOT cleared by this read (only by the pre-render
    /// line's dot-1 auto-clear — see [`Ppu::tick`]'s doc).
    fn read_status(&mut self, open_bus: u8) -> u8 {
        let result = (self.status & 0xE0) | (open_bus & 0x1F);
        self.status &= !STATUS_VBLANK;
        self.w = false;
        result
    }

    /// `$2005` write, first/second per the `w` toggle (nesdev, `d` = the
    /// written byte `HGFEDCBA` MSB-first):
    /// ```text
    /// first write  (w=0): t: ....... ...HGFED <- d: HGFEDCBA[7:3]; x <- d[2:0]; w <- 1
    /// second write (w=1): t: CBA..HG FED..... <- d: HGFEDCBA;      w <- 0
    /// ```
    /// The second write's mask must clear fine Y (t bits 12-14) **and**
    /// coarse Y (t bits 5-9) before OR-ing the new bits in — those are
    /// exactly the fields the `CBA..HGFED.....` diagram marks as replaced;
    /// `..` (bits 10-11, nametable select) and `.....` (bits 0-4, coarse X)
    /// must survive untouched. That clear-mask is `0x03E0 | 0x7000 =
    /// 0x73E0`; keep-mask (ticket W1-04b fix — re-verified against
    /// [nesdev.org/wiki/PPU_scrolling](https://www.nesdev.org/wiki/PPU_scrolling)'s
    /// literal second-write diagram while building this ticket's golden
    /// frame): `!0x73E0 = 0x8C1F`. The keep-mask this replaced, `0x8FFF`,
    /// left bits 5-9 (coarse Y) set instead of clearing them, so a second
    /// write OR'd its new coarse Y onto whatever coarse Y bits `t` already
    /// held (from an earlier `$2005`/`$2006` write) instead of replacing
    /// them — invisible in every existing test here because they all start
    /// from `t == 0`, where OR and replace give the same result, but a real
    /// scroll-reset sequence starting from a nonzero `t` (e.g. right after
    /// pointing `$2006` at a palette address to seed VRAM, as this ticket's
    /// golden-frame fixture does) would leave stale coarse-Y bits behind.
    fn write_scroll(&mut self, value: u8) {
        if !self.w {
            self.t = (self.t & 0xFFE0) | ((value >> 3) as u16);
            self.x = value & 0x07;
        } else {
            self.t = (self.t & 0x8C1F)
                | (((value & 0x07) as u16) << 12)
                | (((value & 0xF8) as u16) << 2);
        }
        self.w = !self.w;
    }

    /// `$2006` write, first/second per the `w` toggle (nesdev):
    /// ```text
    /// first write  (w=0): t: .FEDCBA ........ <- d: ..FEDCBA; t[14] <- 0; w <- 1
    /// second write (w=1): t: ....... HGFEDCBA <- d: HGFEDCBA; v <- t;     w <- 0
    /// ```
    fn write_addr(&mut self, value: u8) {
        if !self.w {
            self.t = (self.t & 0x00FF) | (((value & 0x3F) as u16) << 8);
        } else {
            self.t = (self.t & 0xFF00) | value as u16;
            self.v = self.t;
        }
        self.w = !self.w;
    }

    /// `$2007` read: delayed by one read (returns the *previous* buffered
    /// byte) except for palette addresses (`$3F00-$3FFF`), which bypass the
    /// buffer and return the 6-bit palette value immediately — nesdev.org/
    /// wiki/PPU_registers: "reading from PPUDATA... returns the contents of
    /// an internal read buffer... effectively delaying PPUDATA reads by
    /// one", and for palette addresses "the referenced 6-bit palette data
    /// is returned immediately... the PPU also performs a normal read from
    /// PPU memory at the specified address, 'underneath' the palette data,
    /// and the result of this read goes into the read buffer". nesdev
    /// doesn't give an exact address formula for that "underneath" read;
    /// this masks the address to the `$2xxx` nametable range (`& 0x2FFF`),
    /// the address-decode aliasing every reference PPU implementation
    /// derives it from (real hardware's VRAM address pins can't distinguish
    /// `$3Fxx` from `$2Fxx` at that stage). Every read advances `v` by
    /// [`Ppu::increment_vram_addr`] regardless of which path was taken.
    fn read_data(&mut self, open_bus: u8) -> u8 {
        let addr = self.v & 0x3FFF;
        let result = if addr >= 0x3F00 {
            let value = self.palette_read(addr) & 0x3F;
            self.read_buffer = self.mem_read(addr & 0x2FFF);
            value | (open_bus & 0xC0)
        } else {
            let value = self.read_buffer;
            self.read_buffer = self.mem_read(addr);
            value
        };
        self.increment_vram_addr();
        result
    }

    /// `$2007` write: writes through to the PPU memory bus at `v`, then
    /// advances `v` the same way a read does.
    fn write_data(&mut self, value: u8) {
        let addr = self.v & 0x3FFF;
        self.mem_write(addr, value);
        self.increment_vram_addr();
    }

    /// `$2000` bit 2 selects the post-access `v` increment: 0 => +1 (across
    /// a row), 1 => +32 (down a column) — nesdev.org/wiki/PPU_registers.
    fn increment_vram_addr(&mut self) {
        let step = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
        self.v = self.v.wrapping_add(step) & 0x7FFF;
    }

    /// nesdev.org/wiki/PPU_scrolling, verbatim:
    /// ```text
    /// if ((v & 0x001F) == 31)  // if coarse X == 31
    ///   v &= ~0x001F           // coarse X = 0
    ///   v ^= 0x0400            // switch horizontal nametable
    /// else
    ///   v += 1                 // increment coarse X
    /// ```
    pub(super) fn increment_coarse_x(&mut self) {
        if self.v & 0x001F == 31 {
            self.v &= !0x001F;
            self.v ^= 0x0400;
        } else {
            self.v += 1;
        }
    }

    /// nesdev.org/wiki/PPU_scrolling, verbatim:
    /// ```text
    /// if ((v & 0x7000) != 0x7000)        // if fine Y < 7
    ///   v += 0x1000                      // increment fine Y
    /// else
    ///   v &= ~0x7000                     // fine Y = 0
    ///   let y = (v & 0x03E0) >> 5        // let y = coarse Y
    ///   if (y == 29)
    ///     y = 0                          // coarse Y = 0, switch vertical nametable
    ///     v ^= 0x0800
    ///   elif (y == 31)
    ///     y = 0                          // coarse Y = 0, nametable not switched
    ///   else
    ///     y += 1                         // increment coarse Y
    ///   v = (v & ~0x03E0) | (y << 5)      // put coarse Y back into v
    /// ```
    pub(super) fn increment_y(&mut self) {
        if self.v & 0x7000 != 0x7000 {
            self.v += 0x1000;
        } else {
            self.v &= !0x7000;
            let mut y = (self.v & 0x03E0) >> 5;
            if y == 29 {
                y = 0;
                self.v ^= 0x0800;
            } else if y == 31 {
                y = 0;
            } else {
                y += 1;
            }
            self.v = (self.v & !0x03E0) | (y << 5);
        }
    }

    /// nesdev.org/wiki/PPU_rendering, dot 257: "v: ....A.. ...BCDEF <- t:
    /// ....A.. ...BCDEF" — copies the horizontal nametable bit (10) and
    /// coarse X (bits 0-4) from `t` into `v`.
    pub(super) fn copy_horizontal(&mut self) {
        self.v = (self.v & !0x041F) | (self.t & 0x041F);
    }

    /// nesdev.org/wiki/PPU_rendering, pre-render dots 280-304: "v: GHIA.BC
    /// DEF..... <- t: GHIA.BC DEF....." — copies fine Y (bits 12-14), the
    /// vertical nametable bit (11), and coarse Y (bits 5-9) from `t` into
    /// `v`.
    pub(super) fn copy_vertical(&mut self) {
        self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
    }
}
