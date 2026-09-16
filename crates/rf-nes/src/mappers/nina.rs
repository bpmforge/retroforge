//! Mapper 79 — American Video Entertainment NINA-03 / NINA-06 (ticket
//! W14-04).
//!
//! **28 games** in the census of 1281 real NES archives (2026-09-15), the
//! second largest bucket: AVE's unlicensed catalogue — `Dudes with
//! Attitude`, `Krazy Kreatures`, `Blackjack`, `Deathbots`.
//!
//! # The register is in expansion space, not in ROM space
//!
//! This is the whole character of the board. Every other mapper in this
//! crate is written through `$8000-$FFFF`; NINA's single register lives at
//! **`$4100-$5FFF`**, decoded as `addr & $E100 == $4100` (nesdev, "INES
//! Mapper 079"). That is why it implements
//! [`Mapper::cpu_write_expansion`] rather than reacting inside
//! [`Mapper::cpu_write`] — the cartridge is listening in the range the CPU
//! normally uses for the APU, controllers and cartridge RAM.
//!
//! Writing outside ROM space also means **no bus conflicts**: nothing else
//! is driving the data bus, so unlike `UxROM` or `ColorDreams` the value
//! latched is simply the value written.
//!
//! ```text
//! 7  bit  0
//! ---- ----
//! xxxx PCCC
//!      ||||
//!      |+++- Select 8 KiB CHR ROM bank for PPU $0000-$1FFF
//!      +---- Select 32 KiB PRG ROM bank for CPU $8000-$FFFF
//! ```
//!
//! Mirroring is fixed by the header; the board has no control over it.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_SIZE: usize = 32 * 1024;
const CHR_BANK_SIZE: usize = 8 * 1024;

/// The address decode: `$4100`, `$4103`, `$5FFF` and everything in between
/// that matches these bits is the same register. The board decodes
/// partially, as cheap 1980s cartridge logic does, and a game may use any
/// address that satisfies it.
const REGISTER_MASK: u16 = 0xE100;
const REGISTER_MATCH: u16 = 0x4100;

pub struct Nina {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    mirroring: Mirroring,
    bank_select: u8,
}

impl Nina {
    #[must_use]
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool, mirroring: Mirroring) -> Self {
        debug_assert!(!prg_rom.is_empty(), "NINA image with empty PRG ROM");
        Nina {
            prg_rom,
            chr_rom,
            chr_is_ram,
            mirroring,
            bank_select: 0,
        }
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK_SIZE).max(1)
    }

    fn chr_bank_count(&self) -> usize {
        (self.chr_rom.len() / CHR_BANK_SIZE).max(1)
    }
}

impl Mapper for Nina {
    fn cpu_read(&self, addr: u16) -> u8 {
        debug_assert!(!self.prg_rom.is_empty(), "NINA image with empty PRG ROM");
        let bank = ((self.bank_select >> 3) & 0x01) as usize % self.prg_bank_count();
        let offset = (addr as usize - 0x8000) % PRG_BANK_SIZE;
        self.prg_rom[(bank * PRG_BANK_SIZE + offset) % self.prg_rom.len()]
    }

    /// Nothing. The register is not in ROM space — see
    /// [`Self::cpu_write_expansion`], which is where this board listens.
    fn cpu_write(&mut self, _addr: u16, _value: u8, _cycle: u64) {}

    /// `$4100-$5FFF`, decoded as `addr & $E100 == $4100`.
    fn cpu_write_expansion(&mut self, addr: u16, value: u8) {
        if addr & REGISTER_MASK == REGISTER_MATCH {
            self.bank_select = value;
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.is_empty() {
            return None;
        }
        let bank = (self.bank_select & 0x07) as usize % self.chr_bank_count();
        let start = bank * CHR_BANK_SIZE;
        Some(&self.chr_rom[start..start + CHR_BANK_SIZE])
    }

    /// `MAPR` (ticket W2-04): the single bank-select register.
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

    fn cart(prg_banks: u8, chr_banks: u8) -> Nina {
        let mut prg = vec![0u8; prg_banks as usize * PRG_BANK_SIZE];
        for n in 0..prg_banks {
            prg[n as usize * PRG_BANK_SIZE] = 0xD0 | n;
        }
        let mut chr = vec![0u8; chr_banks as usize * CHR_BANK_SIZE];
        for n in 0..chr_banks {
            chr[n as usize * CHR_BANK_SIZE] = 0xE0 | n;
        }
        Nina::new(prg, chr, false, Mirroring::Horizontal)
    }

    /// The defining property of this board, and the one a reviewer should
    /// check first: a write to ROM space does nothing at all.
    #[test]
    fn writes_to_rom_space_are_ignored() {
        let mut m = cart(2, 8);
        m.cpu_write(0x8000, 0xFF, 0);
        m.cpu_write(0xFFFF, 0xFF, 0);
        assert_eq!(m.cpu_read(0x8000), 0xD0, "still bank 0");
        assert_eq!(m.chr_window().expect("CHR")[0], 0xE0);
    }

    #[test]
    fn bit_three_selects_the_32k_prg_bank() {
        let mut m = cart(2, 1);
        m.cpu_write_expansion(0x4100, 0x08);
        assert_eq!(m.cpu_read(0x8000), 0xD0 | 1);
        m.cpu_write_expansion(0x4100, 0x00);
        assert_eq!(m.cpu_read(0x8000), 0xD0);
    }

    #[test]
    fn the_low_three_bits_select_the_8k_chr_bank() {
        let mut m = cart(1, 8);
        for bank in 0..8u8 {
            m.cpu_write_expansion(0x4100, bank);
            assert_eq!(m.chr_window().expect("CHR")[0], 0xE0 | bank);
        }
    }

    /// Partial decoding is the point: the board answers to a whole family
    /// of addresses, and a game is free to use any of them.
    #[test]
    fn every_address_matching_the_decode_is_the_same_register() {
        for addr in [0x4100u16, 0x4101, 0x41FF, 0x4300, 0x5F00, 0x5FFF] {
            let mut m = cart(2, 8);
            m.cpu_write_expansion(addr, 0x08 | 3);
            assert_eq!(
                m.cpu_read(0x8000),
                0xD0 | 1,
                "${addr:04X} must reach the register"
            );
            assert_eq!(m.chr_window().expect("CHR")[0], 0xE0 | 3);
        }
    }

    /// ...and an address that does not match must not. `$4000-$40FF` is
    /// the APU, and latching a bank from an APU write would corrupt the
    /// machine on ordinary sound code.
    #[test]
    fn an_address_outside_the_decode_is_not_the_register() {
        let mut m = cart(2, 8);
        for addr in [0x4000u16, 0x4016, 0x4017, 0x40FF, 0x6000] {
            m.cpu_write_expansion(addr, 0xFF);
            assert_eq!(
                m.cpu_read(0x8000),
                0xD0,
                "${addr:04X} must not be treated as the bank register"
            );
        }
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

    #[test]
    fn the_bank_register_survives_a_state_round_trip() {
        let mut m = cart(2, 8);
        m.cpu_write_expansion(0x4100, 0x08 | 5);

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        {
            let mut out = StateOut::new(&mut stream);
            m.save_state(&mut out).expect("writes");
        }
        let mut restored = cart(2, 8);
        {
            let mut inp = StateIn::new(&mut stream);
            restored.load_state(&mut inp).expect("reads");
        }
        assert_eq!(restored.cpu_read(0x8000), 0xD0 | 1);
        assert_eq!(restored.chr_window().expect("CHR")[0], 0xE0 | 5);
    }
}
