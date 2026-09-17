//! Mapper 232 — Camerica BF9096, the Quattro carts (ticket W14-15).
//!
//! **3 archives** in the census (2026-09-17): `Quattro Adventure`,
//! `Quattro Arcade`, `Quattro Sports` — each four Codemasters games on a
//! 256 KiB PRG ROM, CHR RAM.
//!
//! Two registers (nesdev, "INES Mapper 232"):
//!
//! ```text
//! $8000-$BFFF  ...B B...  bits 3-4: the 64 KiB block (which game)
//! $C000-$FFFF  .... ..PP  bits 0-1: the 16 KiB bank at $8000 within it
//! ```
//!
//! `$C000-$FFFF` reads the block's last 16 KiB, so each game sees its own
//! fixed bank exactly as it would on a plain UNROM-style cartridge.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_16K: usize = 16 * 1024;

pub struct Quattro {
    prg_rom: Vec<u8>,
    mirroring: Mirroring,
    block: u8,
    bank: u8,
}

impl Quattro {
    pub fn new(prg_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        debug_assert!(!prg_rom.is_empty(), "Quattro image with empty PRG ROM");
        Self {
            prg_rom,
            mirroring,
            block: 0,
            bank: 0,
        }
    }

    fn bank_16k(&self, within_block: usize) -> usize {
        let count = (self.prg_rom.len() / PRG_BANK_16K).max(1);
        (usize::from(self.block) * 4 + within_block) % count
    }
}

impl Mapper for Quattro {
    fn cpu_read(&self, addr: u16) -> u8 {
        let bank = if addr < 0xC000 {
            self.bank_16k(usize::from(self.bank))
        } else {
            self.bank_16k(3)
        };
        self.prg_rom
            [(bank * PRG_BANK_16K + usize::from(addr - 0x8000) % PRG_BANK_16K) % self.prg_rom.len()]
    }

    fn cpu_write(&mut self, addr: u16, value: u8, _cycle: u64) {
        if addr < 0xC000 {
            self.block = (value >> 3) & 0x03;
        } else {
            self.bank = value & 0x03;
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        None
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.block)?;
        out.u8(self.bank)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.block = inp.u8()?;
        self.bank = inp.u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prg() -> Vec<u8> {
        let mut data = vec![0u8; 16 * PRG_BANK_16K];
        for n in 0..16u8 {
            data[n as usize * PRG_BANK_16K] = 0x40 + n;
        }
        data
    }

    #[test]
    fn the_block_picks_the_game_and_the_bank_moves_within_it() {
        let mut m = Quattro::new(prg(), Mirroring::Vertical);
        assert_eq!(m.cpu_read(0x8000), 0x40);
        assert_eq!(m.cpu_read(0xC000), 0x43, "block 0's last bank");
        m.cpu_write(0x8000, 0x10, 0); // block 2
        assert_eq!(m.cpu_read(0x8000), 0x48);
        assert_eq!(m.cpu_read(0xC000), 0x4B);
        m.cpu_write(0xC000, 0x02, 0); // bank 2 within block 2
        assert_eq!(m.cpu_read(0x8000), 0x4A);
        assert_eq!(
            m.cpu_read(0xC000),
            0x4B,
            "the fixed bank stays the block's last"
        );
    }
}
