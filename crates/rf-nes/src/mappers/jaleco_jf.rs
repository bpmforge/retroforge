//! Mapper 87 — Jaleco JF-05 through JF-10, Konami, Taito early boards
//! (ticket W14-15).
//!
//! **2 archives** in the census (2026-09-17): the two `Ninja JaJaMaru`
//! retro-collection dumps.
//!
//! One register in the PRG-RAM range, any write to `$6000-$7FFF` (nesdev,
//! "INES Mapper 087") — which is why this ticket added
//! [`Mapper::cpu_write_wram`]:
//!
//! ```text
//! 7  bit  0
//! ---- ----
//! .... ..HL   the 8 KiB CHR bank is (L << 1) | H: the two bits are swapped
//! ```
//!
//! PRG is fixed (16 or 32 KiB, mirrored like NROM).

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const CHR_BANK_8K: usize = 8 * 1024;

pub struct JalecoJf {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    mirroring: Mirroring,
    chr_bank: u8,
}

impl JalecoJf {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool, mirroring: Mirroring) -> Self {
        debug_assert!(!prg_rom.is_empty(), "JF image with empty PRG ROM");
        Self {
            prg_rom,
            chr_rom,
            chr_is_ram,
            mirroring,
            chr_bank: 0,
        }
    }
}

impl Mapper for JalecoJf {
    fn cpu_read(&self, addr: u16) -> u8 {
        self.prg_rom[usize::from(addr - 0x8000) % self.prg_rom.len()]
    }

    fn cpu_write(&mut self, _addr: u16, _value: u8, _cycle: u64) {}

    fn cpu_write_wram(&mut self, _addr: u16, value: u8) {
        self.chr_bank = ((value & 0x01) << 1) | ((value >> 1) & 0x01);
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.len() < CHR_BANK_8K {
            return None;
        }
        let count = self.chr_rom.len() / CHR_BANK_8K;
        let base = (usize::from(self.chr_bank) % count) * CHR_BANK_8K;
        Some(&self.chr_rom[base..base + CHR_BANK_8K])
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.chr_bank)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.chr_bank = inp.u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_chr_bits_are_swapped_and_come_from_a_wram_write() {
        let mut chr = vec![0u8; 4 * CHR_BANK_8K];
        for n in 0..4u8 {
            chr[n as usize * CHR_BANK_8K] = 0x80 + n;
        }
        let mut m = JalecoJf::new(vec![0xEA; 32 * 1024], chr, false, Mirroring::Vertical);
        m.cpu_write(0x8000, 0x01, 0);
        assert_eq!(m.chr_window().unwrap()[0], 0x80, "$8000 is not a register");
        m.cpu_write_wram(0x6000, 0b01);
        assert_eq!(m.chr_window().unwrap()[0], 0x82, "bit 0 is CHR A14");
        m.cpu_write_wram(0x7ABC, 0b10);
        assert_eq!(m.chr_window().unwrap()[0], 0x81, "bit 1 is CHR A13");
    }
}
