//! Mapper 34 — BNROM, and NINA-001 sharing its number (ticket W14-15).
//!
//! **3 archives** in the census (2026-09-17): `Deadly Towers` and `Wally
//! Bear and the No! Gang` are BNROM; `Impossible Mission II` is NINA-001.
//! iNES gave both the same number; nesdev's own rule for telling them
//! apart without NES 2.0 is the one used here: **CHR ROM present means
//! NINA-001, CHR RAM means BNROM.**
//!
//! BNROM (nesdev, "BNROM"): one register anywhere in `$8000-$FFFF`, bits
//! 0-1 select a 32 KiB PRG bank; 8 KiB CHR RAM, no CHR banking.
//!
//! NINA-001 (nesdev, "NINA-001"): registers live in the PRG-RAM range,
//! which is why this ticket added [`Mapper::cpu_write_wram`]:
//!
//! ```text
//! $7FFD  bit 0     32 KiB PRG bank
//! $7FFE  bits 0-3  4 KiB CHR bank at PPU $0000
//! $7FFF  bits 0-3  4 KiB CHR bank at PPU $1000
//! ```
//!
//! The writes also land in PRG RAM, as on the board.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_32K: usize = 32 * 1024;
const CHR_BANK_4K: usize = 4 * 1024;
const CHR_VIEW_SIZE: usize = 8 * 1024;

pub struct Bnrom {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    mirroring: Mirroring,
    nina: bool,
    prg_bank: u8,
    chr_banks: [u8; 2],
    chr_view: [u8; CHR_VIEW_SIZE],
}

impl Bnrom {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool, mirroring: Mirroring) -> Self {
        debug_assert!(!prg_rom.is_empty(), "BNROM image with empty PRG ROM");
        let mut m = Self {
            nina: !chr_is_ram && !chr_rom.is_empty(),
            prg_rom,
            chr_rom,
            mirroring,
            prg_bank: 0,
            chr_banks: [0; 2],
            chr_view: [0; CHR_VIEW_SIZE],
        };
        m.recompute_chr_view();
        m
    }

    /// `true` for the NINA-001 board (CHR ROM present).
    pub fn is_nina(&self) -> bool {
        self.nina
    }

    fn recompute_chr_view(&mut self) {
        if !self.nina {
            return;
        }
        let count = (self.chr_rom.len() / CHR_BANK_4K).max(1);
        for (half, &bank) in self.chr_banks.iter().enumerate() {
            let src = (usize::from(bank & 0x0F) % count) * CHR_BANK_4K;
            let dst = half * CHR_BANK_4K;
            self.chr_view[dst..dst + CHR_BANK_4K]
                .copy_from_slice(&self.chr_rom[src..src + CHR_BANK_4K]);
        }
    }
}

impl Mapper for Bnrom {
    fn cpu_read(&self, addr: u16) -> u8 {
        let count = (self.prg_rom.len() / PRG_BANK_32K).max(1);
        let base = (usize::from(self.prg_bank) % count) * PRG_BANK_32K;
        self.prg_rom[(base + usize::from(addr - 0x8000)) % self.prg_rom.len()]
    }

    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        if !self.nina {
            self.prg_bank = value & 0x03;
        }
    }

    fn cpu_write_wram(&mut self, addr: u16, value: u8) {
        if !self.nina {
            return;
        }
        match addr {
            0x7FFD => self.prg_bank = value & 0x01,
            0x7FFE => {
                self.chr_banks[0] = value;
                self.recompute_chr_view();
            }
            0x7FFF => {
                self.chr_banks[1] = value;
                self.recompute_chr_view();
            }
            _ => {}
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.nina {
            Some(&self.chr_view[..])
        } else {
            None
        }
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.prg_bank)?;
        out.bytes(&self.chr_banks)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.prg_bank = inp.u8()?;
        inp.bytes(&mut self.chr_banks)?;
        self.recompute_chr_view();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prg(banks: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks as usize * PRG_BANK_32K];
        for n in 0..banks {
            data[n as usize * PRG_BANK_32K] = 0x40 + n;
        }
        data
    }

    fn chr(banks_4k: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks_4k as usize * CHR_BANK_4K];
        for n in 0..banks_4k {
            data[n as usize * CHR_BANK_4K] = 0x80 + n;
        }
        data
    }

    #[test]
    fn bnrom_switches_32k_from_8000_and_ignores_the_ram_range() {
        let mut m = Bnrom::new(prg(4), vec![0; 8 * 1024], true, Mirroring::Vertical);
        assert!(!m.is_nina());
        m.cpu_write(0xC000, 0x03, 0);
        assert_eq!(m.cpu_read(0x8000), 0x43);
        m.cpu_write_wram(0x7FFD, 0x00);
        assert_eq!(m.cpu_read(0x8000), 0x43, "BNROM has no register at $7FFD");
        assert!(m.chr_window().is_none());
    }

    #[test]
    fn nina_001_takes_its_three_registers_from_7ffd_and_leaves_8000_alone() {
        let mut m = Bnrom::new(prg(2), chr(16), false, Mirroring::Horizontal);
        assert!(m.is_nina());
        m.cpu_write(0x8000, 0x01, 0);
        assert_eq!(m.cpu_read(0x8000), 0x40, "$8000 is not a register here");
        m.cpu_write_wram(0x7FFD, 0x01);
        m.cpu_write_wram(0x7FFE, 0x05);
        m.cpu_write_wram(0x7FFF, 0x0A);
        assert_eq!(m.cpu_read(0x8000), 0x41);
        let w = m.chr_window().unwrap();
        assert_eq!(w[0], 0x85);
        assert_eq!(w[CHR_BANK_4K], 0x8A);
    }
}
