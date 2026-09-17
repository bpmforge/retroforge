//! Mapper 66 — GxROM / MHROM (ticket W14-12).
//!
//! **5 games** in the census of 1281 real NES archives (2026-09-16):
//! `Super Mario Bros. + Duck Hunt`, `Dragon Power`, `Gumshoe`,
//! `Thunder & Lightning`.
//!
//! One register, written anywhere in `$8000-$FFFF` (nesdev, "GxROM"):
//!
//! ```text
//! 7  bit  0
//! ---- ----
//! xxPP xxCC
//!   ||   ||
//!   ||   ++- Select 8 KiB CHR ROM bank for PPU $0000-$1FFF
//!   ++------ Select 32 KiB PRG ROM bank for CPU $8000-$FFFF
//! ```
//!
//! Two bits each, so 128 KiB of PRG and 32 KiB of CHR at most; the
//! `% bank_count` on use is the same small-cartridge mirroring every
//! mapper here applies. nesdev notes bus conflicts on some GxROM boards;
//! the games above write values that already match the ROM byte under
//! them, which is why software written for a conflicting board works
//! on one without, and this model does not AND the write with ROM.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_32K: usize = 32 * 1024;
const CHR_BANK_8K: usize = 8 * 1024;

pub struct GxRom {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    mirroring: Mirroring,
    /// The one register, as written.
    bank: u8,
}

impl GxRom {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool, mirroring: Mirroring) -> Self {
        debug_assert!(!prg_rom.is_empty(), "GxROM image with empty PRG ROM");
        Self {
            prg_rom,
            chr_rom,
            chr_is_ram,
            mirroring,
            bank: 0,
        }
    }

    fn prg_bank(&self) -> usize {
        let count = (self.prg_rom.len() / PRG_BANK_32K).max(1);
        usize::from((self.bank >> 4) & 0x03) % count
    }

    fn chr_bank(&self) -> usize {
        let count = (self.chr_rom.len() / CHR_BANK_8K).max(1);
        usize::from(self.bank & 0x03) % count
    }
}

impl Mapper for GxRom {
    fn cpu_read(&self, addr: u16) -> u8 {
        let base = self.prg_bank() * PRG_BANK_32K;
        let offset = (addr as usize - 0x8000) % PRG_BANK_32K;
        self.prg_rom[(base + offset) % self.prg_rom.len()]
    }

    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        self.bank = value;
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.len() < CHR_BANK_8K {
            return None;
        }
        let base = self.chr_bank() * CHR_BANK_8K;
        Some(&self.chr_rom[base..base + CHR_BANK_8K])
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.bank)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.bank = inp.u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn prg(banks: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks as usize * PRG_BANK_32K];
        for n in 0..banks {
            data[n as usize * PRG_BANK_32K] = 0x40 + n;
            data[n as usize * PRG_BANK_32K + PRG_BANK_32K - 1] = 0xA0 + n;
        }
        data
    }

    fn chr(banks: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks as usize * CHR_BANK_8K];
        for n in 0..banks {
            data[n as usize * CHR_BANK_8K] = 0x80 + n;
        }
        data
    }

    #[test]
    fn bits_4_5_select_the_32k_prg_bank_and_bits_0_1_the_chr_bank() {
        let mut m = GxRom::new(prg(4), chr(4), false, Mirroring::Vertical);
        assert_eq!(m.cpu_read(0x8000), 0x40);
        assert_eq!(m.cpu_read(0xFFFF), 0xA0);
        m.cpu_write(0x8000, 0x32, 0); // PRG 3, CHR 2
        assert_eq!(m.cpu_read(0x8000), 0x43);
        assert_eq!(m.cpu_read(0xFFFF), 0xA3, "the whole 32 KiB window moves");
        assert_eq!(m.chr_window().unwrap()[0], 0x82);
        m.cpu_write(0xC123, 0x11, 0); // any address in $8000-$FFFF
        assert_eq!(m.cpu_read(0x8000), 0x41);
        assert_eq!(m.chr_window().unwrap()[0], 0x81);
    }

    #[test]
    fn a_small_cartridge_mirrors_instead_of_reading_out_of_bounds() {
        let mut m = GxRom::new(prg(2), chr(1), false, Mirroring::Horizontal);
        m.cpu_write(0x8000, 0x33, 0);
        assert_eq!(m.cpu_read(0x8000), 0x41, "PRG 3 % 2");
        assert_eq!(m.chr_window().unwrap()[0], 0x80, "CHR 3 % 1");
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
    }

    #[test]
    fn the_register_survives_a_state_round_trip() {
        let mut m = GxRom::new(prg(4), chr(4), false, Mirroring::Vertical);
        m.cpu_write(0x8000, 0x21, 0);
        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        m.save_state(&mut StateOut::new(&mut stream)).unwrap();
        let mut n = GxRom::new(prg(4), chr(4), false, Mirroring::Vertical);
        n.load_state(&mut StateIn::new(&mut stream)).unwrap();
        assert_eq!(n.cpu_read(0x8000), 0x42);
        assert_eq!(n.chr_window().unwrap()[0], 0x81);
    }

    #[test]
    fn chr_ram_has_no_window_to_push() {
        let m = GxRom::new(prg(1), vec![0; CHR_BANK_8K], true, Mirroring::Vertical);
        assert!(m.chr_window().is_none());
    }
}
