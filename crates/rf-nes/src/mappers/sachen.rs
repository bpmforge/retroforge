//! Mapper 148 — Sachen SA-0037 (ticket W14-15).
//!
//! **4 archives** in the census (2026-09-17): the Tengen `Tetris`, and
//! Panesian's `Bubble Bath Babes`, `Hot Slots`, `Peek-A-Boo Poker`.
//!
//! One register, any write to `$8000-$FFFF` (nesdev, "INES Mapper 148"):
//!
//! ```text
//! 7  bit  0
//! ---- ----
//! .... PCCC   P = 32 KiB PRG bank, CCC = 8 KiB CHR bank
//! ```
//!
//! The board has bus conflicts; the titles above write values equal to
//! the ROM byte under them, so no AND is modelled.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_32K: usize = 32 * 1024;
const CHR_BANK_8K: usize = 8 * 1024;

pub struct Sachen {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    mirroring: Mirroring,
    reg: u8,
}

impl Sachen {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool, mirroring: Mirroring) -> Self {
        debug_assert!(!prg_rom.is_empty(), "Sachen image with empty PRG ROM");
        Self {
            prg_rom,
            chr_rom,
            chr_is_ram,
            mirroring,
            reg: 0,
        }
    }
}

impl Mapper for Sachen {
    fn cpu_read(&self, addr: u16) -> u8 {
        let count = (self.prg_rom.len() / PRG_BANK_32K).max(1);
        let base = (usize::from((self.reg >> 3) & 1) % count) * PRG_BANK_32K;
        self.prg_rom[(base + usize::from(addr - 0x8000)) % self.prg_rom.len()]
    }

    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        self.reg = value;
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.len() < CHR_BANK_8K {
            return None;
        }
        let count = self.chr_rom.len() / CHR_BANK_8K;
        let base = (usize::from(self.reg & 0x07) % count) * CHR_BANK_8K;
        Some(&self.chr_rom[base..base + CHR_BANK_8K])
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.reg)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.reg = inp.u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_3_is_prg_and_bits_0_2_are_chr() {
        let mut prg = vec![0u8; 2 * PRG_BANK_32K];
        prg[0] = 0x40;
        prg[PRG_BANK_32K] = 0x41;
        let mut chr = vec![0u8; 8 * CHR_BANK_8K];
        for n in 0..8u8 {
            chr[n as usize * CHR_BANK_8K] = 0x80 + n;
        }
        let mut m = Sachen::new(prg, chr, false, Mirroring::Vertical);
        m.cpu_write(0x8000, 0b0000_1101, 0);
        assert_eq!(m.cpu_read(0x8000), 0x41);
        assert_eq!(m.chr_window().unwrap()[0], 0x85);
        m.cpu_write(0xFFFF, 0b0000_0010, 0);
        assert_eq!(m.cpu_read(0x8000), 0x40);
        assert_eq!(m.chr_window().unwrap()[0], 0x82);
    }
}
