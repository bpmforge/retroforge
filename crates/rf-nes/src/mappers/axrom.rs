//! AxROM (mapper 7) —
//! [nesdev.org/wiki/AxROM](https://www.nesdev.org/wiki/AxROM): a whole
//! **32 KiB** PRG bank at `$8000-$FFFF` (unlike UxROM's 16 KiB window plus
//! a fixed last bank — AxROM has no fixed window at all), plus
//! mapper-controlled **single-screen** mirroring. CHR is always 8 KiB of
//! CHR RAM, unbanked, so [`AxRom::chr_window`] returns `None` (see
//! [`super::Mapper::chr_window`]).
//!
//! Register — any write to `$8000-$FFFF` (nesdev's diagram):
//!
//! ```text
//! 7  bit  0
//! ---- ----
//! xxxM xPPP
//!    |  |||
//!    |  +++- Select 32 KiB PRG ROM bank for CPU $8000-$FFFF
//!    +------ Select 1 KiB VRAM page for all four nametables
//! ```
//!
//! The `M` bit is what makes this mapper interesting: it switches the
//! *whole* nametable space between one-screen-lower and one-screen-upper,
//! which is how AxROM games scroll in both axes without a mirroring seam.
//! Those two [`Mirroring`] variants already existed for MMC1 (ticket
//! W2-02) and are already honoured by
//! [`crate::ppu::Ppu::set_mirroring`], so this mapper needed no PPU
//! change at all.
//!
//! **Bus conflicts are not modeled**, and for this mapper that is a real
//! board-dependent choice rather than a blanket simplification: nesdev
//! records ANROM/AN1ROM/AMROM as having bus conflicts and **AOROM as not**
//! having them. An iNES/NES 2.0 header cannot distinguish those boards —
//! they all declare mapper 7 — so, like most emulators, this takes the
//! conflict-free reading. AOROM is also the largest and most common of the
//! family. See `crate::mappers` module doc's "MapperBus/BusValue" section
//! for the general position.
//!
//! Added on Brad's request (2026-08-06) after a real mapper-7 ROM was
//! refused; mapper 7 sits outside FR-CORE-025's original 0/1/2/3/4 set,
//! which was amended in the same commit.
use rf_cart::Mirroring;

use super::Mapper;

/// AxROM switches a full 32 KiB window — there is no fixed bank.
const BANK_SIZE: usize = 32 * 1024;

/// Bits 0-2 select the PRG bank: 8 banks x 32 KiB = 256 KiB, which is
/// AOROM's maximum and therefore the family's.
const BANK_MASK: u8 = 0x07;

/// Bit 4 selects which single-screen nametable page all four slots show.
const MIRROR_BIT: u8 = 0x10;

pub struct AxRom {
    prg_rom: Vec<u8>,
    bank: u8,
    mirroring: Mirroring,
}

impl AxRom {
    /// `mirroring` is deliberately **not** taken from the header: AxROM
    /// drives mirroring entirely from its own register, and the header's
    /// declared value is meaningless for this board. Power-on state is
    /// one-screen-lower, matching a zeroed register.
    pub fn new(prg_rom: Vec<u8>) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(BANK_SIZE),
            "AxROM PRG ROM must be a nonzero multiple of 32 KiB"
        );
        AxRom {
            prg_rom,
            bank: 0,
            mirroring: Mirroring::OneScreenLower,
        }
    }

    fn bank_count(&self) -> usize {
        self.prg_rom.len() / BANK_SIZE
    }
}

impl Mapper for AxRom {
    fn cpu_read(&self, addr: u16) -> u8 {
        debug_assert!(
            (0x8000..=0xFFFF).contains(&addr),
            "cpu_read is only ever called for $8000-$FFFF"
        );
        let bank = (self.bank as usize) % self.bank_count();
        self.prg_rom[bank * BANK_SIZE + (addr - 0x8000) as usize]
    }

    /// Any write to `$8000-$FFFF` sets both the PRG bank (bits 0-2) and
    /// the single-screen page (bit 4) at once — they share one register,
    /// so a game cannot change one without rewriting the other.
    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        self.bank = value & BANK_MASK;
        self.mirroring = if value & MIRROR_BIT != 0 {
            Mirroring::OneScreenUpper
        } else {
            Mirroring::OneScreenLower
        };
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// `None`: CHR RAM, unbanked — the PPU's own flat buffer is already
    /// the permanently-correct view.
    fn chr_window(&self) -> Option<&[u8]> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four 32 KiB banks, each filled with its own index so a read can
    /// name which bank it came from.
    fn axrom(banks: u8) -> AxRom {
        let mut prg = Vec::new();
        for b in 0..banks {
            prg.extend(std::iter::repeat_n(b, BANK_SIZE));
        }
        AxRom::new(prg)
    }

    #[test]
    fn power_on_shows_bank_zero_and_one_screen_lower() {
        let m = axrom(4);
        assert_eq!(m.cpu_read(0x8000), 0);
        assert_eq!(m.cpu_read(0xFFFF), 0);
        assert_eq!(m.mirroring(), Mirroring::OneScreenLower);
    }

    /// The whole 32 KiB window moves together — there is no fixed bank,
    /// which is the difference from UxROM and the thing most likely to be
    /// implemented wrong by analogy.
    #[test]
    fn bank_select_moves_the_entire_32k_window_with_no_fixed_half() {
        let mut m = axrom(4);
        m.cpu_write(0x8000, 2, 0);
        assert_eq!(m.cpu_read(0x8000), 2, "low half must follow the bank");
        assert_eq!(
            m.cpu_read(0xC000),
            2,
            "high half must ALSO follow — AxROM has no fixed last bank"
        );
        assert_eq!(m.cpu_read(0xFFFF), 2);
    }

    #[test]
    fn mirror_bit_switches_single_screen_page_both_ways() {
        let mut m = axrom(4);
        m.cpu_write(0x8000, MIRROR_BIT, 0);
        assert_eq!(m.mirroring(), Mirroring::OneScreenUpper);
        m.cpu_write(0x8000, 0, 0);
        assert_eq!(
            m.mirroring(),
            Mirroring::OneScreenLower,
            "clearing the bit must switch back, not latch permanently"
        );
    }

    /// Bank and mirroring share one register, so a write that changes the
    /// page must not disturb the bank and vice versa — asserted together
    /// because an implementation that masked the value wrongly could pass
    /// either check alone.
    #[test]
    fn bank_and_mirroring_are_set_by_the_same_write_independently() {
        let mut m = axrom(4);
        m.cpu_write(0x8000, MIRROR_BIT | 3, 0);
        assert_eq!(
            m.cpu_read(0x8000),
            3,
            "bank bits must survive the mirror bit"
        );
        assert_eq!(m.mirroring(), Mirroring::OneScreenUpper);
    }

    /// A bank number past the cartridge's real size must wrap rather than
    /// panic — Marble Madness is 128 KiB (4 banks) while the register can
    /// address 8.
    #[test]
    fn bank_number_wraps_modulo_the_real_bank_count() {
        let mut m = axrom(4);
        m.cpu_write(0x8000, 6, 0); // 6 % 4 == 2
        assert_eq!(m.cpu_read(0x8000), 2);
    }

    /// Only bits 0-2 select the bank; the unused high bits must be
    /// ignored, not folded in.
    #[test]
    fn bits_outside_the_documented_fields_are_ignored() {
        let mut m = axrom(4);
        m.cpu_write(0x8000, 0xE0 | 1, 0); // 0xE0 is all "x" bits
        assert_eq!(m.cpu_read(0x8000), 1);
    }
}
