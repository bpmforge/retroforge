//! CNROM (mapper 3) —
//! [nesdev.org/wiki/CNROM](https://www.nesdev.org/wiki/CNROM): CHR-only
//! banking. PRG ROM is fixed and unbanked at `$8000-$FFFF` (16 KiB
//! mirrored into both halves, or 32 KiB straight through — identical
//! arithmetic to [`super::Nrom::cpu_read`], since CNROM's PRG side has no
//! mapper registers at all); CHR is an 8 KiB window into up to 32 KiB
//! (standard mapper 3) of CHR ROM, selected by the low bits of any
//! `$8000-$FFFF` write. Mirroring is hardware-fixed by the board's solder
//! pads, not mapper-controlled, so [`Cnrom::mirroring`] just returns
//! whatever the header declared.
//!
//! Bus conflicts (nesdev: "the original CNROM board is always subject to
//! AND-type bus conflicts") are not modeled — see `crate::mappers` module
//! doc's "MapperBus/BusValue" section.
//!
//! CHR RAM: [`Cnrom::chr_window`] returns `None` whenever this
//! cartridge's CHR is RAM rather than ROM, per [`super::Mapper::
//! chr_window`]'s doc — CNROM-with-CHR-RAM is not a configuration any
//! licensed game in `docs/design/EMULATION_CORES.md`'s launch-set table
//! uses, and the push/materialize design this ticket's write_scope
//! forces (see `crate::mappers` module doc) cannot safely bank RAM
//! without a round-trip write-back mechanism this ticket does not build.
use rf_cart::Mirroring;

use super::Mapper;

const CHR_BANK_SIZE: usize = 8 * 1024;

pub struct Cnrom {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    mirroring: Mirroring,
    chr_bank: u8,
}

impl Cnrom {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool, mirroring: Mirroring) -> Self {
        Cnrom {
            prg_rom,
            chr_rom,
            chr_is_ram,
            mirroring,
            chr_bank: 0,
        }
    }

    fn chr_bank_count(&self) -> usize {
        self.chr_rom.len() / CHR_BANK_SIZE
    }
}

impl Mapper for Cnrom {
    /// Identical mirror-or-map-through arithmetic to
    /// [`super::Nrom::cpu_read`] — CNROM's PRG side has no bank registers.
    fn cpu_read(&self, addr: u16) -> u8 {
        debug_assert!(!self.prg_rom.is_empty(), "CNROM image with empty PRG ROM");
        let offset = (addr as usize - 0x8000) % self.prg_rom.len();
        self.prg_rom[offset]
    }

    /// Any write to `$8000-$FFFF` latches the CHR bank register, applied
    /// (masked to the cartridge's real CHR bank count) the next time
    /// [`Cnrom::chr_window`] is polled.
    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        self.chr_bank = value;
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.is_empty() {
            return None;
        }
        let bank = (self.chr_bank as usize) % self.chr_bank_count();
        let start = bank * CHR_BANK_SIZE;
        Some(&self.chr_rom[start..start + CHR_BANK_SIZE])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `banks` 8 KiB CHR banks, bank `n`'s first byte is `0x10 + n` (a
    /// marker distinguishing every bank, including bank 0, from every
    /// other one and from a plain zeroed buffer).
    fn cnrom(chr_banks: u8) -> Cnrom {
        let prg_rom = vec![0xAAu8; 16 * 1024];
        let mut chr_rom = vec![0u8; chr_banks as usize * CHR_BANK_SIZE];
        for n in 0..chr_banks {
            chr_rom[n as usize * CHR_BANK_SIZE] = 0x10 + n;
        }
        Cnrom::new(prg_rom, chr_rom, false, Mirroring::Vertical)
    }

    #[test]
    fn chr_bank_select_write_changes_the_visible_window() {
        let mut m = cnrom(4);
        assert_eq!(
            m.chr_window().unwrap()[0],
            0x10,
            "bank 0 selected at construction"
        );
        m.cpu_write(0x8000, 2, 0);
        assert_eq!(
            m.chr_window().unwrap()[0],
            0x12,
            "the mapper's own bank register must have changed the visible CHR window -- \
             a no-op mapper that ignores writes would still read bank 0's marker here"
        );
    }

    #[test]
    fn chr_bank_number_wraps_modulo_the_real_bank_count() {
        let mut m = cnrom(4);
        m.cpu_write(0x8000, 6, 0); // only 4 banks exist (0-3); 6 % 4 == 2
        assert_eq!(m.chr_window().unwrap()[0], 0x12);
    }

    #[test]
    fn prg_is_fixed_and_unaffected_by_chr_bank_writes() {
        let mut m = cnrom(4);
        let before = m.cpu_read(0x8000);
        m.cpu_write(0x8000, 3, 0);
        assert_eq!(
            m.cpu_read(0x8000),
            before,
            "PRG has no bank registers on CNROM"
        );
    }

    #[test]
    fn chr_window_is_none_for_chr_ram() {
        let m = Cnrom::new(
            vec![0u8; 16 * 1024],
            vec![0u8; 8 * 1024],
            true,
            Mirroring::Horizontal,
        );
        assert!(m.chr_window().is_none());
    }
}
