//! Action 53 (mapper 28) —
//! [nesdev.org/wiki/INES_Mapper_028](https://www.nesdev.org/wiki/INES_Mapper_028):
//! the multicart mapper used by the Action 53 homebrew collections.
//!
//! ## Two address ranges, not one
//!
//! Unlike every other mapper in this crate, mapper 28 has a **register
//! select latch outside `$8000-$FFFF`**:
//!
//! * `$5000-$5FFF` selects *which* register a value goes to,
//! * `$8000-$FFFF` writes the value.
//!
//! That is why [`super::Mapper::cpu_write_expansion`] exists. There are
//! no bus conflicts.
//!
//! ## The four registers
//!
//! Selected by `$5000`'s bits 7 and 0 — `S` picks user vs supervisor
//! registers, `R` picks within the pair:
//!
//! | select | register | contents |
//! |---|---|---|
//! | `$00` | CHR bank | CHR RAM A14-A13, and mirroring bit 0 (see below) |
//! | `$01` | inner PRG bank | the bank a game switches within itself |
//! | `$80` | mode | mirroring, PRG bank mode, outer bank size |
//! | `$81` | outer PRG bank | the game-select bank; constant per game |
//!
//! In a multicart, `$00`/`$01` change banks within a game while
//! `$80`/`$81` stay fixed for that game's whole run.
//!
//! ## Mirroring has three write paths, and that is the point
//!
//! While the mirroring mode is 1-screen (0 or 1), **bit 0 of the mode can
//! be written in three places**: bit 0 of `$80`, bit 4 of `$00`, or bit 4
//! of `$01`. That is deliberate — it lets a game ported from AxROM keep
//! writing its old single-register mirroring control and still work.
//!
//! When mirroring is vertical or horizontal (2 or 3), D4 of `$00`/`$01`
//! is **ignored**. Applying it unconditionally is the obvious bug, and it
//! only shows up on games that use H/V mirroring *and* happen to set D4.
//!
//! ## Banking
//!
//! The outer bank size decides how many inner bits pass through:
//! size `S` gives an inner field of `S+1` bits, so the 16 KiB bank is
//! `(outer << (S + 1)) | (inner & inner_mask)`.
//!
//! The subtlety worth stating: **when a FIXED bank is accessed, the size
//! is temporarily forced to 32 KiB**, so all the outer bits pass straight
//! through and no inner bits do. That is what lets mode 3's fixed `$C000`
//! bank be any of banks 1, 3, 5 or 7 of a 128 KiB game rather than only
//! the last one.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const BANK_SIZE: usize = 16 * 1024;

pub struct Action53 {
    prg_rom: Vec<u8>,
    /// The `$5000` latch: which register `$8000-$FFFF` writes reach.
    select: u8,
    /// `$00`: CHR RAM bank, bits 0-1.
    chr_bank: u8,
    /// `$01`: inner PRG bank, bits 0-3.
    inner: u8,
    /// `$80`: `SSPPMM` — outer size, PRG mode, mirroring.
    mode: u8,
    /// `$81`: outer PRG bank, bits 0-5.
    outer: u8,
}

impl Action53 {
    #[must_use]
    pub fn new(prg_rom: Vec<u8>) -> Self {
        Self {
            prg_rom,
            select: 0,
            chr_bank: 0,
            inner: 0,
            // Power-up: nesdev specifies the mode register comes up with
            // the PRG mode bits set, so a game that never writes $80 sees
            // the fixed-$C000 (UNROM) layout its reset vector needs.
            mode: 0x0C,
            outer: 0x3F,
        }
    }

    /// Mirroring mode, `$80` bits 1-0.
    fn mirror_mode(&self) -> u8 {
        self.mode & 0x03
    }

    /// PRG bank mode, `$80` bits 3-2.
    fn prg_mode(&self) -> u8 {
        (self.mode >> 2) & 0x03
    }

    /// Outer bank size, `$80` bits 5-4.
    fn outer_size(&self) -> u8 {
        (self.mode >> 4) & 0x03
    }

    /// The 16 KiB bank a CPU address selects.
    fn bank_for(&self, addr: u16) -> usize {
        let size = self.outer_size();
        let inner_bits = u32::from(size) + 1;
        let inner_mask = (1u32 << inner_bits) - 1;
        let outer = u32::from(self.outer);
        let inner = u32::from(self.inner) & inner_mask;
        let high = addr >= 0xC000;

        // A FIXED bank forces the size to 32 KiB: every outer bit passes
        // through and no inner bit does.
        let fixed = |low_half: bool| -> u32 { (outer << 1) | u32::from(!low_half) };

        let bank = match self.prg_mode() {
            // 0/1: a 32 KiB window selected entirely by outer+inner, with
            // the low bit of the pair chosen by the address.
            0 | 1 => ((outer << inner_bits) | (inner & !1)) | u32::from(high),
            // 2: fixed bottom half of the outer bank at $8000,
            //    switchable at $C000.
            2 => {
                if high {
                    (outer << inner_bits) | inner
                } else {
                    fixed(true)
                }
            }
            // 3: switchable at $8000, fixed top half of the outer bank at
            //    $C000.
            _ => {
                if high {
                    fixed(false)
                } else {
                    (outer << inner_bits) | inner
                }
            }
        };
        bank as usize
    }

    /// Writes to `$00`/`$01` set mirroring bit 0 from D4 — but **only**
    /// while the mode is 1-screen.
    fn apply_axrom_style_mirroring(&mut self, value: u8) {
        if self.mirror_mode() < 2 {
            self.mode = (self.mode & !0x01) | ((value >> 4) & 0x01);
        }
    }
}

impl Mapper for Action53 {
    fn cpu_read(&self, addr: u16) -> u8 {
        if self.prg_rom.is_empty() {
            return 0;
        }
        let banks = self.prg_rom.len() / BANK_SIZE;
        let bank = if banks == 0 {
            0
        } else {
            self.bank_for(addr) % banks
        };
        let offset = usize::from(addr) & (BANK_SIZE - 1);
        self.prg_rom[bank * BANK_SIZE + offset]
    }

    fn cpu_write_expansion(&mut self, addr: u16, value: u8) {
        if (0x5000..=0x5FFF).contains(&addr) {
            // Only bits 7 and 0 matter; everything else is ignored.
            self.select = value & 0x81;
        }
    }

    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        match self.select {
            0x00 => {
                self.chr_bank = value & 0x03;
                self.apply_axrom_style_mirroring(value);
            }
            0x01 => {
                self.inner = value & 0x0F;
                self.apply_axrom_style_mirroring(value);
            }
            0x80 => self.mode = value & 0x3F,
            _ => self.outer = value & 0x3F,
        }
    }

    fn mirroring(&self) -> Mirroring {
        match self.mirror_mode() {
            0 => Mirroring::OneScreenLower,
            1 => Mirroring::OneScreenUpper,
            2 => Mirroring::Vertical,
            _ => Mirroring::Horizontal,
        }
    }

    /// CHR is RAM on this board, so there is nothing to push — the same
    /// documented gap AxROM records.
    fn chr_window(&self) -> Option<&[u8]> {
        None
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.select)?;
        out.u8(self.chr_bank)?;
        out.u8(self.inner)?;
        out.u8(self.mode)?;
        out.u8(self.outer)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.select = inp.u8()?;
        self.chr_bank = inp.u8()?;
        self.inner = inp.u8()?;
        self.mode = inp.u8()?;
        self.outer = inp.u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 128 KiB of PRG where every 16 KiB bank is filled with its own
    /// index, so a read says which bank it came from.
    fn rom() -> Action53 {
        let banks = 8;
        let mut prg = vec![0u8; banks * BANK_SIZE];
        for b in 0..banks {
            for i in 0..BANK_SIZE {
                prg[b * BANK_SIZE + i] = b as u8;
            }
        }
        Action53::new(prg)
    }

    fn select(m: &mut Action53, reg: u8, value: u8) {
        m.cpu_write_expansion(0x5000, reg);
        m.cpu_write(0x8000, value, 0);
    }

    /// The `$5000` latch keeps only bits 7 and 0 — everything else is
    /// ignored, so a game writing a whole byte still selects correctly.
    #[test]
    fn the_select_latch_keeps_only_bits_7_and_0() {
        let mut m = rom();
        m.cpu_write_expansion(0x5000, 0xFF);
        assert_eq!(m.select, 0x81);
        m.cpu_write_expansion(0x5ABC, 0x7E);
        assert_eq!(m.select, 0x00);
    }

    /// Register select lives at `$5000-$5FFF`, which no other mapper in
    /// this crate uses — a write to `$8000` must NOT change it.
    #[test]
    fn the_select_latch_is_only_writable_through_the_expansion_area() {
        let mut m = rom();
        m.cpu_write_expansion(0x5000, 0x81);
        m.cpu_write(0x8000, 0x00, 0);
        assert_eq!(m.select, 0x81, "an $8000 write is a VALUE, not a select");
    }

    /// Mode 3 (UNROM #2): switchable at `$8000`, fixed top half of the
    /// outer bank at `$C000`.
    #[test]
    fn mode_3_switches_low_and_fixes_high() {
        let mut m = rom();
        select(&mut m, 0x80, 0x0C); // PRG mode 3, size 0, mirroring 0
        select(&mut m, 0x81, 0x03); // outer = 3
        select(&mut m, 0x01, 0x01); // inner = 1
        assert_eq!(m.cpu_read(0x8000), (3 << 1) | 1, "switchable half");
        assert_eq!(m.cpu_read(0xC000), (3 << 1) | 1, "fixed top half of outer");
    }

    /// Mode 2 (UNROM #180) is the mirror image: fixed at `$8000`,
    /// switchable at `$C000`.
    #[test]
    fn mode_2_fixes_low_and_switches_high() {
        let mut m = rom();
        select(&mut m, 0x80, 0x08); // PRG mode 2
        select(&mut m, 0x81, 0x02);
        select(&mut m, 0x01, 0x01);
        assert_eq!(m.cpu_read(0x8000), 2 << 1, "fixed bottom half of outer");
        assert_eq!(m.cpu_read(0xC000), (2 << 1) | 1, "switchable half");
    }

    /// Modes 0 and 1 are a 32 KiB window: the two halves are consecutive
    /// banks and the address picks between them.
    #[test]
    fn modes_0_and_1_switch_a_whole_32k_window() {
        for mode in [0u8, 1] {
            let mut m = rom();
            select(&mut m, 0x80, mode << 2);
            select(&mut m, 0x81, 0x02);
            let lo = m.cpu_read(0x8000);
            let hi = m.cpu_read(0xC000);
            assert_eq!(hi, lo + 1, "mode {mode}: the halves must be consecutive");
        }
    }

    /// **A fixed bank forces the outer size to 32 KiB**, so every outer
    /// bit passes through and no inner bit does.
    ///
    /// That is what lets mode 3's fixed `$C000` bank be bank 1, 3, 5 or 7
    /// of a 128 KiB game rather than only the last one — the detail that
    /// separates this mapper from plain UNROM.
    #[test]
    fn a_fixed_bank_ignores_the_inner_bits_whatever_the_outer_size() {
        let mut m = rom();
        // PRG mode 3, outer size 2 (so the inner field is 3 bits wide).
        select(&mut m, 0x80, 0x0C | (2 << 4));
        select(&mut m, 0x81, 0x01); // outer = 1
        select(&mut m, 0x01, 0x07); // inner = 7, all bits set

        let fixed = m.cpu_read(0xC000);
        select(&mut m, 0x01, 0x00); // inner = 0
        assert_eq!(
            m.cpu_read(0xC000),
            fixed,
            "the fixed bank must not move when the inner bank changes"
        );
        assert_eq!(fixed, (1 << 1) | 1, "outer bits pass straight through");
    }

    /// The outer bank size widens the inner field: size S gives S+1 inner
    /// bits.
    #[test]
    fn the_outer_size_decides_how_many_inner_bits_pass_through() {
        for (size, inner, expected_low_bits) in
            [(0u8, 0x0Fu8, 0x01u8), (1, 0x0F, 0x03), (2, 0x0F, 0x07)]
        {
            let mut m = rom();
            select(&mut m, 0x80, 0x0C | (size << 4)); // mode 3
            select(&mut m, 0x81, 0x00);
            select(&mut m, 0x01, inner);
            assert_eq!(
                m.cpu_read(0x8000) & 0x0F,
                expected_low_bits,
                "size {size} should expose {} inner bits",
                size + 1
            );
        }
    }

    /// **Mirroring bit 0 has three write paths while 1-screen** — bit 0
    /// of `$80`, bit 4 of `$00`, and bit 4 of `$01` — which is what lets
    /// an AxROM port keep its old mirroring code.
    #[test]
    fn one_screen_mirroring_is_writable_from_three_registers() {
        let mut m = rom();
        select(&mut m, 0x80, 0x00); // 1-screen lower
        assert_eq!(m.mirroring(), Mirroring::OneScreenLower);

        select(&mut m, 0x00, 0x10); // D4 via the CHR register
        assert_eq!(m.mirroring(), Mirroring::OneScreenUpper);

        select(&mut m, 0x01, 0x00); // D4 clear via the inner-bank register
        assert_eq!(m.mirroring(), Mirroring::OneScreenLower);

        select(&mut m, 0x80, 0x01); // and directly
        assert_eq!(m.mirroring(), Mirroring::OneScreenUpper);
    }

    /// **D4 is IGNORED while mirroring is vertical or horizontal.**
    ///
    /// Applying it unconditionally is the obvious bug, and it only shows
    /// on games that use H/V mirroring AND happen to set D4 — so it is
    /// asserted directly.
    #[test]
    fn d4_is_ignored_when_mirroring_is_not_one_screen() {
        for (mode, expected) in [(2u8, Mirroring::Vertical), (3, Mirroring::Horizontal)] {
            let mut m = rom();
            select(&mut m, 0x80, mode);
            assert_eq!(m.mirroring(), expected);
            select(&mut m, 0x00, 0xFF); // D4 set, and everything else
            assert_eq!(
                m.mirroring(),
                expected,
                "mode {mode} must ignore D4 from register $00"
            );
            select(&mut m, 0x01, 0xFF);
            assert_eq!(
                m.mirroring(),
                expected,
                "mode {mode} must ignore D4 from register $01"
            );
        }
    }

    #[test]
    fn the_chr_bank_register_keeps_two_bits() {
        let mut m = rom();
        select(&mut m, 0x00, 0xFF);
        assert_eq!(m.chr_bank, 0x03);
    }

    /// CHR is RAM on this board, the same documented gap AxROM records.
    #[test]
    fn chr_is_ram_so_there_is_no_window_to_push() {
        assert!(rom().chr_window().is_none());
    }

    /// A minimal in-memory stream, so the round trip below exercises the
    /// real `StateOut`/`StateIn` path rather than comparing fields.
    struct MemStream {
        buf: Vec<u8>,
        at: usize,
    }

    impl rf_core_api::StateWriter for MemStream {
        fn write_all(&mut self, bytes: &[u8]) -> Result<(), StateError> {
            self.buf.extend_from_slice(bytes);
            Ok(())
        }
    }

    impl rf_core_api::StateReader for MemStream {
        fn read_exact(&mut self, out: &mut [u8]) -> Result<(), StateError> {
            let end = self.at + out.len();
            if end > self.buf.len() {
                return Err(StateError::Io("state stream exhausted".to_string()));
            }
            out.copy_from_slice(&self.buf[self.at..end]);
            self.at = end;
            Ok(())
        }
    }

    /// State round-trips: all four registers plus the select latch.
    #[test]
    fn state_round_trips() {
        let mut m = rom();
        m.cpu_write_expansion(0x5000, 0x81);
        select(&mut m, 0x80, 0x2E);
        select(&mut m, 0x81, 0x15);
        select(&mut m, 0x01, 0x09);
        select(&mut m, 0x00, 0x02);

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        {
            let mut out = StateOut::new(&mut stream);
            m.save_state(&mut out).expect("writes");
        }
        let mut restored = rom();
        {
            let mut inp = StateIn::new(&mut stream);
            restored.load_state(&mut inp).expect("reads");
        }
        assert_eq!(restored.select, m.select);
        assert_eq!(restored.mode, m.mode);
        assert_eq!(restored.outer, m.outer);
        assert_eq!(restored.inner, m.inner);
        assert_eq!(restored.chr_bank, m.chr_bank);
        assert_eq!(
            restored.cpu_read(0x8000),
            m.cpu_read(0x8000),
            "and the restored banking resolves identically"
        );
    }
}
