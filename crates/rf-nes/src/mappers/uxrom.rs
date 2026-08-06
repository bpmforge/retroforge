//! UxROM (mapper 2) —
//! [nesdev.org/wiki/UxROM](https://www.nesdev.org/wiki/UxROM): PRG-only
//! banking. `$8000-$BFFF` is a 16 KiB bank selected by the low bits of
//! any `$8000-$FFFF` write ("UNROM uses bits 2-0; UOROM uses bits 3-0" —
//! this implementation uses the full write byte modulo the cartridge's
//! actual bank count, a permissive superset of both variants, the
//! standard approach nesdev's own page notes most emulators take);
//! `$C000-$FFFF` is fixed to the *last* 16 KiB bank. CHR is "8K" with
//! "n/a" bank window — i.e. CHR RAM, unbanked — so [`UxRom::chr_window`]
//! always returns `None` (see [`super::Mapper::chr_window`]'s doc:
//! [`crate::ppu::Ppu`]'s own flat CHR buffer, seeded once at
//! construction, is already the permanently-correct view for a
//! non-banked cartridge). Mirroring is hardware-fixed by the board, not
//! mapper-controlled, so [`UxRom::mirroring`] just returns whatever the
//! header declared.
//!
//! Bus conflicts (nesdev: "the original boards were subject to bus
//! conflicts, and the relevant games all work around this in software")
//! are not modeled — see `crate::mappers` module doc's "MapperBus/
//! BusValue" section.
use rf_cart::Mirroring;

use super::Mapper;

const BANK_SIZE: usize = 16 * 1024;

pub struct UxRom {
    prg_rom: Vec<u8>,
    mirroring: Mirroring,
    bank: u8,
}

impl UxRom {
    pub fn new(prg_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(BANK_SIZE),
            "UxROM PRG ROM must be a nonzero multiple of 16 KiB"
        );
        UxRom {
            prg_rom,
            mirroring,
            bank: 0,
        }
    }

    fn bank_count(&self) -> usize {
        self.prg_rom.len() / BANK_SIZE
    }
}

impl Mapper for UxRom {
    fn cpu_read(&self, addr: u16) -> u8 {
        let (bank, offset) = match addr {
            0x8000..=0xBFFF => ((self.bank as usize) % self.bank_count(), addr - 0x8000),
            0xC000..=0xFFFF => (self.bank_count() - 1, addr - 0xC000),
            _ => unreachable!("cpu_read is only ever called for $8000-$FFFF"),
        };
        self.prg_rom[bank * BANK_SIZE + offset as usize]
    }

    /// Any write to `$8000-$FFFF` latches the low bank-select bank,
    /// applied (masked to the cartridge's real bank count) on the next
    /// read — nesdev's register diagram; see module doc for why the full
    /// byte is used rather than a fixed 3- or 4-bit mask.
    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        self.bank = value;
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `banks` 16 KiB PRG banks, bank `n`'s first byte is `n` (a marker
    /// distinguishing every bank from every other one, including bank 0).
    fn uxrom(banks: u8) -> UxRom {
        let mut prg_rom = vec![0u8; banks as usize * BANK_SIZE];
        for n in 0..banks {
            prg_rom[n as usize * BANK_SIZE] = n;
        }
        UxRom::new(prg_rom, Mirroring::Horizontal)
    }

    #[test]
    fn c000_is_fixed_to_the_last_bank_regardless_of_selection() {
        let mut m = uxrom(4);
        assert_eq!(
            m.cpu_read(0xC000),
            3,
            "before any write, still the last bank"
        );
        m.cpu_write(0x8000, 1, 0);
        assert_eq!(
            m.cpu_read(0xC000),
            3,
            "selecting bank 1 for $8000 must not move $C000"
        );
    }

    #[test]
    fn bank_select_write_changes_8000_window_and_nothing_else() {
        let mut m = uxrom(4);
        assert_eq!(m.cpu_read(0x8000), 0, "bank 0 selected at construction");
        m.cpu_write(0x8000, 2, 0);
        assert_eq!(
            m.cpu_read(0x8000),
            2,
            "the mapper's own bank register must have changed the visible bank -- a \
             no-op mapper that ignores writes would still read 0 here"
        );
        m.cpu_write(0xFFF0, 1, 0);
        assert_eq!(
            m.cpu_read(0x8000),
            1,
            "any $8000-$FFFF address latches the bank register, not just $8000 itself"
        );
    }

    #[test]
    fn bank_number_wraps_modulo_the_real_bank_count() {
        let mut m = uxrom(4);
        m.cpu_write(0x8000, 7, 0); // only 4 banks exist (0-3); 7 % 4 == 3
        assert_eq!(m.cpu_read(0x8000), 3);
    }

    #[test]
    fn chr_window_is_always_none() {
        let m = uxrom(2);
        assert!(m.chr_window().is_none());
    }
}
