//! Mapper 206 — DxROM / Namco 118 / Tengen MIMIC-1 (ticket W14-04).
//!
//! **17 games** in the census of 1281 real NES archives (2026-09-15):
//! `R.B.I. Baseball`, `Gauntlet`, `Dragon Buster`, `Karnov`.
//!
//! # The MMC3 without any of the parts that make MMC3 hard
//!
//! Mapper 206 is the ancestor MMC3 was built from, and the register file is
//! a strict subset (nesdev, "INES Mapper 206"). Same two ports, same
//! even-selects/odd-writes protocol:
//!
//! ```text
//! $8000-$9FFE, even   Bank select:  ....  .RRR   R = which register (0-7)
//! $8001-$9FFF, odd    Bank data:    the value for the selected register
//! ```
//!
//! What it does **not** have is the entire reason MMC3 is delicate:
//!
//! - **no A12 inversion** (`$8000` bit 7) — the CHR windows never swap
//!   halves, so there is one layout instead of two;
//! - **no PRG mode bit** (`$8000` bit 6) — `$C000` and `$E000` are always
//!   the last two banks;
//! - **no scanline IRQ**, so no A12 filtering, no reload quirk, and none of
//!   the revision-A/B disagreement that `Mmc3` has to model;
//! - **no mirroring register** and **no PRG-RAM protect** — the header
//!   decides mirroring and nothing guards the RAM.
//!
//! # The masks are the hardware, not defensive programming
//!
//! Namco 118 has four CHR lines and four PRG lines wired, so **CHR values
//! are 6 bits and PRG values are 4** (nesdev). A game writing a larger
//! value gets the truncated one on hardware; masking here is modelling
//! that, which is why it happens at the register and not at the lookup.
//! The further `% bank_count` on use is the separate, ordinary protection
//! every mapper in this crate applies so a small cartridge mirrors instead
//! of reading out of bounds.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_8K: usize = 8 * 1024;
const CHR_BANK_1K: usize = 1024;
/// The PPU's whole pattern-table space, which `chr_window` hands back as
/// one contiguous slice (see `Mmc3`, which composes the same way).
const CHR_VIEW_SIZE: usize = 8 * 1024;

/// CHR registers carry 6 bits: Namco 118 wires four CHR lines plus the two
/// the 1 KiB granularity needs.
const CHR_VALUE_MASK: u8 = 0x3F;
/// PRG registers carry 4 bits: 16 banks of 8 KiB, 128 KiB maximum.
const PRG_VALUE_MASK: u8 = 0x0F;

pub struct DxRom {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    mirroring: Mirroring,
    /// Which register the next odd write lands in.
    selected: u8,
    /// R0-R5 are CHR, R6-R7 are PRG.
    registers: [u8; 8],
    /// The composed 8 KiB pattern-table view, rebuilt whenever a CHR
    /// register changes. Same design as `Mmc3`: the PPU reads one flat
    /// buffer, so the mapper materialises its windows into one.
    chr_view: [u8; CHR_VIEW_SIZE],
}

impl DxRom {
    #[must_use]
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool, mirroring: Mirroring) -> Self {
        debug_assert!(!prg_rom.is_empty(), "DxROM image with empty PRG ROM");
        let mut mapper = DxRom {
            prg_rom,
            chr_rom,
            chr_is_ram,
            mirroring,
            selected: 0,
            registers: [0; 8],
            chr_view: [0; CHR_VIEW_SIZE],
        };
        mapper.recompute_chr_view();
        mapper
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK_8K).max(1)
    }

    fn chr_bank_count_1k(&self) -> usize {
        (self.chr_rom.len() / CHR_BANK_1K).max(1)
    }

    /// The eight 1 KiB windows, in the one layout this mapper has.
    ///
    /// R0 and R1 are 2 KiB banks, so each fills two windows and **its low
    /// bit is ignored** — a 2 KiB bank is two consecutive 1 KiB banks
    /// starting at an even index, which is what makes `& 0xFE` and `| 0x01`
    /// the right pair rather than `n` and `n + 1`.
    fn recompute_chr_view(&mut self) {
        if self.chr_rom.is_empty() {
            return;
        }
        let count = self.chr_bank_count_1k();
        let windows: [usize; 8] = [
            (self.registers[0] & 0xFE) as usize % count,
            (self.registers[0] | 0x01) as usize % count,
            (self.registers[1] & 0xFE) as usize % count,
            (self.registers[1] | 0x01) as usize % count,
            self.registers[2] as usize % count,
            self.registers[3] as usize % count,
            self.registers[4] as usize % count,
            self.registers[5] as usize % count,
        ];
        for (slot, &bank) in windows.iter().enumerate() {
            let src = bank * CHR_BANK_1K;
            let dst = slot * CHR_BANK_1K;
            self.chr_view[dst..dst + CHR_BANK_1K]
                .copy_from_slice(&self.chr_rom[src..src + CHR_BANK_1K]);
        }
    }
}

impl Mapper for DxRom {
    fn cpu_read(&self, addr: u16) -> u8 {
        let count = self.prg_bank_count();
        let (bank, offset) = match addr {
            0x8000..=0x9FFF => (self.registers[6] as usize % count, addr - 0x8000),
            0xA000..=0xBFFF => (self.registers[7] as usize % count, addr - 0xA000),
            // Always the last two banks: this mapper has no PRG mode bit.
            0xC000..=0xDFFF => (count.saturating_sub(2), addr - 0xC000),
            0xE000..=0xFFFF => (count - 1, addr - 0xE000),
            _ => unreachable!("cpu_read is only ever called for $8000-$FFFF"),
        };
        self.prg_rom[(bank * PRG_BANK_8K + offset as usize) % self.prg_rom.len()]
    }

    fn cpu_write(&mut self, addr: u16, value: u8, _cycle: u64) {
        // Only $8000-$9FFF decodes; everything above it is inert on this
        // board, unlike MMC3 where $A000 and $C000-$E000 carry mirroring,
        // RAM protect and the IRQ registers.
        if !(0x8000..=0x9FFF).contains(&addr) {
            return;
        }
        if addr.is_multiple_of(2) {
            self.selected = value & 0x07;
            return;
        }
        let index = self.selected as usize;
        self.registers[index] = if index < 6 {
            value & CHR_VALUE_MASK
        } else {
            value & PRG_VALUE_MASK
        };
        if index < 6 {
            self.recompute_chr_view();
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.is_empty() {
            None
        } else {
            Some(&self.chr_view[..])
        }
    }

    /// `MAPR` (ticket W2-04): the eight registers plus the select latch.
    /// The composed CHR view is derived, so it is rebuilt on load rather
    /// than stored — 8 KiB of a save state that says nothing new.
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.selected)?;
        for value in self.registers {
            out.u8(value)?;
        }
        Ok(())
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.selected = inp.u8()?;
        for index in 0..8 {
            self.registers[index] = inp.u8()?;
        }
        self.recompute_chr_view();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `prg_banks` 8 KiB PRG banks and `chr_1k` 1 KiB CHR banks, every
    /// bank stamped with its own index so no two are confusable.
    fn cart(prg_banks: usize, chr_1k: usize) -> DxRom {
        let mut prg = vec![0u8; prg_banks * PRG_BANK_8K];
        for n in 0..prg_banks {
            prg[n * PRG_BANK_8K] = 0x10 + n as u8;
        }
        let mut chr = vec![0u8; chr_1k * CHR_BANK_1K];
        for n in 0..chr_1k {
            chr[n * CHR_BANK_1K] = 0x40 + n as u8;
        }
        DxRom::new(prg, chr, false, Mirroring::Vertical)
    }

    fn select(m: &mut DxRom, register: u8, value: u8) {
        m.cpu_write(0x8000, register, 0);
        m.cpu_write(0x8001, value, 0);
    }

    #[test]
    fn r6_and_r7_bank_the_two_switchable_prg_windows() {
        let mut m = cart(8, 8);
        select(&mut m, 6, 3);
        select(&mut m, 7, 5);
        assert_eq!(m.cpu_read(0x8000), 0x10 + 3);
        assert_eq!(m.cpu_read(0xA000), 0x10 + 5);
    }

    /// No PRG mode bit: unlike MMC3, the high half never moves.
    #[test]
    fn the_last_two_banks_are_fixed_whatever_is_written() {
        let mut m = cart(8, 8);
        for value in [0u8, 1, 7, 0xFF] {
            select(&mut m, 6, value);
            select(&mut m, 7, value);
            assert_eq!(m.cpu_read(0xC000), 0x10 + 6, "second-to-last bank");
            assert_eq!(m.cpu_read(0xE000), 0x10 + 7, "last bank");
        }
    }

    /// R0 and R1 are 2 KiB banks, so their low bit is ignored: writing 5
    /// and writing 4 must produce the same pair of 1 KiB windows.
    #[test]
    fn r0_and_r1_are_two_kilobyte_banks_whose_low_bit_is_ignored() {
        let mut m = cart(4, 16);
        select(&mut m, 0, 4);
        let view = m.chr_window().expect("CHR");
        assert_eq!(view[0], 0x40 + 4);
        assert_eq!(view[CHR_BANK_1K], 0x40 + 5);

        select(&mut m, 0, 5);
        let view = m.chr_window().expect("CHR");
        assert_eq!(
            view[0],
            0x40 + 4,
            "an odd 2 KiB bank selects the same even-aligned pair"
        );
        assert_eq!(view[CHR_BANK_1K], 0x40 + 5);
    }

    #[test]
    fn r2_through_r5_are_one_kilobyte_banks() {
        let mut m = cart(4, 16);
        for (register, bank) in [(2u8, 9u8), (3, 10), (4, 11), (5, 12)] {
            select(&mut m, register, bank);
        }
        let view = m.chr_window().expect("CHR");
        for (slot, bank) in [(4usize, 9u8), (5, 10), (6, 11), (7, 12)] {
            assert_eq!(
                view[slot * CHR_BANK_1K],
                0x40 + bank,
                "window {slot} must carry bank {bank}"
            );
        }
    }

    /// The wired-line masks. A game writing a value wider than the board
    /// can carry gets the truncated one, on hardware and here.
    #[test]
    fn register_values_are_masked_to_the_lines_the_board_has() {
        let mut m = cart(16, 64);
        select(&mut m, 2, 0xFF);
        assert_eq!(
            m.chr_window().expect("CHR")[4 * CHR_BANK_1K],
            0x40 + CHR_VALUE_MASK,
            "CHR values are six bits: $FF becomes $3F"
        );

        select(&mut m, 6, 0xFF);
        assert_eq!(
            m.cpu_read(0x8000),
            0x10 + PRG_VALUE_MASK,
            "PRG values are four bits: $FF becomes $0F"
        );
    }

    /// $A000 and above are registers on MMC3 and inert here. Treating one
    /// as a bank select is how a game that shares code with an MMC3 title
    /// would silently misbank.
    #[test]
    fn writes_above_9fff_are_inert() {
        let mut m = cart(8, 8);
        select(&mut m, 6, 3);
        for addr in [0xA000u16, 0xA001, 0xC000, 0xE000, 0xFFFF] {
            m.cpu_write(addr, 0, 0);
            m.cpu_write(addr, 1, 0);
        }
        assert_eq!(m.cpu_read(0x8000), 0x10 + 3, "still the bank R6 selected");
    }

    #[test]
    fn chr_ram_has_no_window() {
        let m = DxRom::new(
            vec![0u8; 2 * PRG_BANK_8K],
            Vec::new(),
            true,
            Mirroring::Horizontal,
        );
        assert!(m.chr_window().is_none());
    }

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

    /// The CHR view is derived, so the round trip has to prove it is
    /// rebuilt on load and not silently left at its reset contents.
    #[test]
    fn registers_survive_a_state_round_trip_and_the_chr_view_is_rebuilt() {
        let mut m = cart(8, 16);
        select(&mut m, 0, 6);
        select(&mut m, 3, 11);
        select(&mut m, 6, 2);

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        {
            let mut out = StateOut::new(&mut stream);
            m.save_state(&mut out).expect("writes");
        }
        let mut restored = cart(8, 16);
        {
            let mut inp = StateIn::new(&mut stream);
            restored.load_state(&mut inp).expect("reads");
        }
        assert_eq!(restored.cpu_read(0x8000), m.cpu_read(0x8000));
        assert_eq!(
            restored.chr_window().expect("CHR"),
            m.chr_window().expect("CHR")
        );
        assert_eq!(restored.chr_window().expect("CHR")[0], 0x40 + 6);
    }
}
