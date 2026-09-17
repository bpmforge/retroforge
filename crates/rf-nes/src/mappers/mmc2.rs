//! Mapper 9 — MMC2 / PNROM (ticket W14-13).
//!
//! **4 archives** in the census of 1281 real NES archives (2026-09-17):
//! `Punch-Out!!` and `Mike Tyson's Punch-Out!!`, two revisions each.
//! MMC2 was made for exactly one game.
//!
//! Registers (nesdev, "MMC2"):
//!
//! ```text
//! $A000-$AFFF  PRG ROM bank select: ...B BBBB, an 8 KiB bank at $8000-$9FFF
//!              ($A000-$FFFF is fixed to the last three 8 KiB banks)
//! $B000-$BFFF  CHR ROM $FD/0000 bank select: ...C CCCC (4 KiB at $0000, latch $FD)
//! $C000-$CFFF  CHR ROM $FE/0000 bank select                      (latch $FE)
//! $D000-$DFFF  CHR ROM $FD/1000 bank select (4 KiB at $1000, latch $FD)
//! $E000-$EFFF  CHR ROM $FE/1000 bank select                      (latch $FE)
//! $F000-$FFFF  Mirroring: .... ...M  (0 = vertical, 1 = horizontal)
//! ```
//!
//! **Which bank is visible is not this mapper's decision to make in
//! time.** The latches flip when the PPU *fetches* tile `$FD` or `$FE`
//! ("PPU reads $0FD8: latch 0 is set to $FD; ... $1FE8-$1FEF: latch 1 is
//! set to $FE"), mid-scanline, between two tile fetches. This crate's
//! mappers push a materialised CHR window into the PPU after CPU writes,
//! which is instruction granularity — one to two tile fetches late, so
//! the tiles right after every `$FD`/`$FE` tile would come from the wrong
//! bank. So the PPU holds the four banks and flips the selection itself
//! at the read ([`crate::ppu::Ppu`], `mem.rs`); this mapper supplies them
//! through [`Mapper::chr_latch`] and is told the selection back once per
//! instruction so a save state carries it.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::{ChrLatchView, Mapper};

const PRG_BANK_8K: usize = 8 * 1024;
const CHR_BANK_4K: usize = 4 * 1024;

pub struct Mmc2 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    prg_bank: u8,
    /// `[$B000, $C000, $D000, $E000]` as written, five bits each.
    chr_regs: [u8; 4],
    mirroring_reg: u8,
    /// The PPU's latch selection as last reported (`note_chr_latch`), so
    /// it survives a save state; also what a fresh push starts from.
    selected: [bool; 2],
}

impl Mmc2 {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(PRG_BANK_8K),
            "MMC2 PRG ROM must be a nonzero multiple of 8 KiB"
        );
        Self {
            prg_rom,
            chr_rom,
            prg_bank: 0,
            chr_regs: [0; 4],
            mirroring_reg: 0,
            selected: [false, false],
        }
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK_8K).max(1)
    }

    fn chr_bank(&self, reg: usize) -> &[u8] {
        let count = (self.chr_rom.len() / CHR_BANK_4K).max(1);
        let bank = usize::from(self.chr_regs[reg] & 0x1F) % count;
        let start = bank * CHR_BANK_4K;
        &self.chr_rom[start..(start + CHR_BANK_4K).min(self.chr_rom.len())]
    }
}

impl Mapper for Mmc2 {
    fn cpu_read(&self, addr: u16) -> u8 {
        let count = self.prg_bank_count();
        let window = usize::from((addr - 0x8000) / PRG_BANK_8K as u16);
        let bank = match window {
            0 => usize::from(self.prg_bank & 0x0F) % count,
            // $A000-$FFFF: the last three banks, fixed.
            w => (count + w).saturating_sub(4) % count,
        };
        self.prg_rom[bank * PRG_BANK_8K + usize::from(addr - 0x8000) % PRG_BANK_8K]
    }

    fn cpu_write(&mut self, addr: u16, value: u8, _cycle: u64) {
        match addr & 0xF000 {
            0xA000 => self.prg_bank = value & 0x0F,
            0xB000..=0xE000 => self.chr_regs[usize::from((addr >> 12) - 0xB)] = value & 0x1F,
            0xF000 => self.mirroring_reg = value & 0x01,
            _ => {}
        }
    }

    fn mirroring(&self) -> Mirroring {
        if self.mirroring_reg & 1 != 0 {
            Mirroring::Horizontal
        } else {
            Mirroring::Vertical
        }
    }

    /// The PPU's flat window is not used: the latch view below is.
    fn chr_window(&self) -> Option<&[u8]> {
        None
    }

    fn chr_latch(&self) -> Option<ChrLatchView<'_>> {
        if self.chr_rom.is_empty() {
            return None;
        }
        Some(ChrLatchView {
            banks: [
                self.chr_bank(0),
                self.chr_bank(1),
                self.chr_bank(2),
                self.chr_bank(3),
            ],
            selected: self.selected,
        })
    }

    fn note_chr_latch(&mut self, selected: [bool; 2]) {
        self.selected = selected;
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.prg_bank)?;
        for r in self.chr_regs {
            out.u8(r)?;
        }
        out.u8(self.mirroring_reg)?;
        out.bool(self.selected[0])?;
        out.bool(self.selected[1])
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.prg_bank = inp.u8()?;
        for r in &mut self.chr_regs {
            *r = inp.u8()?;
        }
        self.mirroring_reg = inp.u8()?;
        self.selected = [inp.bool()?, inp.bool()?];
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prg(banks: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks as usize * PRG_BANK_8K];
        for n in 0..banks {
            data[n as usize * PRG_BANK_8K] = 0x40 + n;
        }
        data
    }

    fn chr(banks: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks as usize * CHR_BANK_4K];
        for n in 0..banks {
            data[n as usize * CHR_BANK_4K] = 0x80 + n;
        }
        data
    }

    #[test]
    fn a000_switches_8000_and_the_last_three_banks_are_fixed() {
        let mut m = Mmc2::new(prg(16), chr(32));
        assert_eq!(m.cpu_read(0x8000), 0x40);
        assert_eq!(m.cpu_read(0xA000), 0x40 + 13);
        assert_eq!(m.cpu_read(0xC000), 0x40 + 14);
        assert_eq!(m.cpu_read(0xE000), 0x40 + 15);
        m.cpu_write(0xA123, 0x05, 0);
        assert_eq!(m.cpu_read(0x8000), 0x45);
        assert_eq!(m.cpu_read(0xA000), 0x40 + 13, "fixed banks do not move");
    }

    #[test]
    fn the_four_chr_registers_feed_the_latch_view_in_order() {
        let mut m = Mmc2::new(prg(16), chr(32));
        m.cpu_write(0xB000, 3, 0);
        m.cpu_write(0xC000, 7, 0);
        m.cpu_write(0xD000, 11, 0);
        m.cpu_write(0xE000, 31, 0);
        let view = m.chr_latch().expect("CHR ROM has a latch view");
        assert_eq!(view.banks[0][0], 0x83);
        assert_eq!(view.banks[1][0], 0x87);
        assert_eq!(view.banks[2][0], 0x8B);
        assert_eq!(view.banks[3][0], 0x9F);
        assert_eq!(view.selected, [false, false]);
        assert!(m.chr_window().is_none(), "the flat window is not used");
    }

    #[test]
    fn mirroring_bit_and_reported_selection_survive_a_state_round_trip() {
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
                out.copy_from_slice(&self.buf[self.at..end]);
                self.at = end;
                Ok(())
            }
        }
        let mut m = Mmc2::new(prg(16), chr(32));
        m.cpu_write(0xF000, 1, 0);
        m.cpu_write(0xC000, 9, 0);
        m.note_chr_latch([true, false]);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        m.save_state(&mut StateOut::new(&mut stream)).unwrap();
        let mut n = Mmc2::new(prg(16), chr(32));
        n.load_state(&mut StateIn::new(&mut stream)).unwrap();
        assert_eq!(n.mirroring(), Mirroring::Horizontal);
        let view = n.chr_latch().unwrap();
        assert_eq!(view.selected, [true, false]);
        assert_eq!(view.banks[1][0], 0x89);
    }
}
