//! Mapper 11 — Color Dreams (ticket W14-04).
//!
//! # Why this one first
//!
//! Chosen by measurement, not by fame. A census of 1281 real NES archives
//! (2026-09-15) bucketed every one of the 223 the loader refused by mapper
//! number, and mapper 11 was the largest single bucket at **54 games** —
//! more than twice the next. They are the unlicensed Color Dreams and
//! Wisdom Tree catalogue: `Bible Adventures`, `Menace Beach`, `Metal
//! Fighter`.
//!
//! # The registers
//!
//! One register, and every write to `$8000-$FFFF` hits it (nesdev, "INES
//! Mapper 011"):
//!
//! ```text
//! 7  bit  0
//! ---- ----
//! CCCC LLPP
//! |||| ||||
//! |||| ||++- Select 32 KiB PRG ROM bank for CPU $8000-$FFFF
//! |||| ++--- Used by "Crystal Mines 2" (unlicensed) as extra PRG lines
//! ++++------ Select 8 KiB CHR ROM bank for PPU $0000-$1FFF
//! ```
//!
//! **The full two bits are used for PRG and the full four for CHR**, then
//! masked by the cartridge's real bank count, exactly as `UxROM` and
//! `CNROM` already do here. Mirroring is fixed by the header — this board
//! has no mirroring control at all.
//!
//! # Bus conflicts, and why they are not modelled
//!
//! Color Dreams boards have **bus conflicts**: the ROM drives the data bus
//! during a write to `$8000-$FFFF`, so the value latched is the AND of
//! what the CPU wrote and what the ROM holds at that address. Real games
//! avoid the problem by writing a value to an address that already
//! contains it, which makes the AND a no-op — so modelling it changes
//! nothing for software that works on hardware, and modelling it *wrongly*
//! would break software that works. `UxROM` (mapper 2) has the same
//! property and takes the same position; see its module doc.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_SIZE: usize = 32 * 1024;
const CHR_BANK_SIZE: usize = 8 * 1024;

pub struct ColorDreams {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    mirroring: Mirroring,
    /// The one register, stored raw. Masked to the real bank count on use,
    /// so a cartridge smaller than the register can address still behaves.
    bank_select: u8,
    /// Mapper 144, Death Race (ticket W14-15): the same register, but the
    /// board's bus conflict is wired so that bit 0 of what lands is bit 0
    /// of the ROM byte under the write, whatever the CPU drove (nesdev,
    /// "INES Mapper 144"). Cygnus Force and Death Race depend on it.
    death_race: bool,
}

impl ColorDreams {
    #[must_use]
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool, mirroring: Mirroring) -> Self {
        debug_assert!(!prg_rom.is_empty(), "Color Dreams image with empty PRG ROM");
        ColorDreams {
            prg_rom,
            chr_rom,
            chr_is_ram,
            mirroring,
            bank_select: 0,
            death_race: false,
        }
    }

    /// Mapper 144: Color Dreams with the ROM-driven bit 0 (see the field).
    pub fn new_death_race(
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        chr_is_ram: bool,
        mirroring: Mirroring,
    ) -> Self {
        let mut m = Self::new(prg_rom, chr_rom, chr_is_ram, mirroring);
        m.death_race = true;
        m
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK_SIZE).max(1)
    }

    fn chr_bank_count(&self) -> usize {
        (self.chr_rom.len() / CHR_BANK_SIZE).max(1)
    }
}

impl Mapper for ColorDreams {
    fn cpu_read(&self, addr: u16) -> u8 {
        debug_assert!(
            !self.prg_rom.is_empty(),
            "Color Dreams image with empty PRG ROM"
        );
        let bank = (self.bank_select & 0x03) as usize % self.prg_bank_count();
        let offset = (addr as usize - 0x8000) % PRG_BANK_SIZE;
        // Modulo the ROM length as well, so a cartridge whose PRG is not a
        // whole number of 32 KiB banks mirrors rather than panicking.
        self.prg_rom[(bank * PRG_BANK_SIZE + offset) % self.prg_rom.len()]
    }

    fn cpu_write(&mut self, addr: u16, value: u8, _cycle: u64) {
        self.bank_select = if self.death_race {
            (value & 0xFE) | (self.cpu_read(addr) & 0x01)
        } else {
            value
        };
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.is_empty() {
            return None;
        }
        let bank = ((self.bank_select >> 4) & 0x0F) as usize % self.chr_bank_count();
        let start = bank * CHR_BANK_SIZE;
        Some(&self.chr_rom[start..start + CHR_BANK_SIZE])
    }

    /// `MAPR` (ticket W2-04): the single bank-select register. Mirroring
    /// is header-fixed and comes back with the ROM; CHR bytes are
    /// cartridge data.
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.bank_select)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.bank_select = inp.u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `prg_banks` 32 KiB banks and `chr_banks` 8 KiB banks, each stamped
    /// with its own index in its first byte so no bank can be mistaken for
    /// another — including bank 0, which a zero-filled image would hide.
    fn cart(prg_banks: u8, chr_banks: u8) -> ColorDreams {
        let mut prg = vec![0u8; prg_banks as usize * PRG_BANK_SIZE];
        for n in 0..prg_banks {
            prg[n as usize * PRG_BANK_SIZE] = 0xA0 | n;
        }
        let mut chr = vec![0u8; chr_banks as usize * CHR_BANK_SIZE];
        for n in 0..chr_banks {
            chr[n as usize * CHR_BANK_SIZE] = 0xC0 | n;
        }
        ColorDreams::new(prg, chr, false, Mirroring::Vertical)
    }

    #[test]
    fn the_low_two_bits_select_a_32k_prg_bank() {
        let mut m = cart(4, 1);
        assert_eq!(m.cpu_read(0x8000), 0xA0, "reset selects bank 0");
        for bank in 0..4u8 {
            m.cpu_write(0x8000, bank, 0);
            assert_eq!(
                m.cpu_read(0x8000),
                0xA0 | bank,
                "bank {bank} must be selected by its own index"
            );
        }
    }

    #[test]
    fn the_high_four_bits_select_an_8k_chr_bank() {
        let mut m = cart(1, 8);
        for bank in 0..8u8 {
            m.cpu_write(0x8000, bank << 4, 0);
            assert_eq!(
                m.chr_window().expect("CHR ROM")[0],
                0xC0 | bank,
                "CHR bank {bank} must come from the high nibble"
            );
        }
    }

    /// The two halves of the register are independent, which is the whole
    /// point of one register carrying both: a game switching graphics must
    /// not move its own code out from under itself.
    #[test]
    fn prg_and_chr_selection_do_not_disturb_each_other() {
        let mut m = cart(4, 8);
        m.cpu_write(0x8000, (5 << 4) | 2, 0);
        assert_eq!(m.cpu_read(0x8000), 0xA0 | 2);
        assert_eq!(m.chr_window().expect("CHR ROM")[0], 0xC0 | 5);
    }

    /// A register value larger than the cartridge wraps rather than
    /// reading out of bounds — the same masking `UxROM` and `CNROM` apply.
    #[test]
    fn a_bank_beyond_the_cartridge_wraps() {
        let mut m = cart(2, 2);
        m.cpu_write(0x8000, 0xFF, 0);
        assert_eq!(m.cpu_read(0x8000), 0xA0 | 1, "PRG bank 3 of 2 wraps to 1");
        assert_eq!(
            m.chr_window().expect("CHR ROM")[0],
            0xC0 | 1,
            "CHR bank 15 of 2 wraps to 1"
        );
    }

    #[test]
    fn chr_ram_has_no_window() {
        let m = ColorDreams::new(
            vec![0u8; PRG_BANK_SIZE],
            Vec::new(),
            true,
            Mirroring::Horizontal,
        );
        assert!(m.chr_window().is_none());
    }

    /// The same in-memory stream `action53`'s round-trip test uses.
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

    #[test]
    fn the_bank_register_survives_a_state_round_trip() {
        let mut m = cart(4, 4);
        m.cpu_write(0x9ABC, 0x32, 0);

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        {
            let mut out = StateOut::new(&mut stream);
            m.save_state(&mut out).expect("writes");
        }
        let mut restored = cart(4, 4);
        {
            let mut inp = StateIn::new(&mut stream);
            restored.load_state(&mut inp).expect("reads");
        }
        assert_eq!(restored.cpu_read(0x8000), m.cpu_read(0x8000));
        assert_eq!(
            restored.chr_window().expect("CHR"),
            m.chr_window().expect("CHR")
        );
    }

    /// **Death Race's bit 0 comes from the ROM** (ticket W14-15; nesdev
    /// mapper 144).
    #[test]
    fn death_race_takes_bit_0_from_the_rom_byte_under_the_write() {
        let mut prg = vec![0u8; 4 * 32 * 1024];
        for n in 0..4u8 {
            prg[n as usize * 32 * 1024] = 0x40 + n;
        }
        prg[0x0100] = 0x01; // ROM bit 0 set at $8100
        prg[0x0200] = 0x00; // clear at $8200
        let mut m = ColorDreams::new_death_race(prg, vec![0; 8 * 1024], true, Mirroring::Vertical);
        m.cpu_write(0x8100, 0x02, 0); // CPU drives bank 2, ROM forces bit 0 -> 3
        assert_eq!(m.cpu_read(0x8000), 0x43);
        m.cpu_write(0x8200, 0x03, 0); // CPU drives 3, ROM clears bit 0 -> 2
        assert_eq!(m.cpu_read(0x8000), 0x42);
    }
}
