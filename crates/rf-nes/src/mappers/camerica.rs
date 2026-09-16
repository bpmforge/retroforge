//! Mapper 71 — Camerica / Codemasters BF9093, BF9097 (ticket W14-04).
//!
//! **17 games** in the census of 1281 real NES archives (2026-09-15):
//! `Micro Machines`, `Fire Hawk`, `MiG 29 - Soviet Fighter`, the Quattro
//! compilations.
//!
//! # A UxROM that listens in a different place
//!
//! The PRG side is UxROM's: a switchable 16 KiB bank at `$8000-$BFFF`, the
//! **last** bank fixed at `$C000-$FFFF`. What differs is where the write
//! has to land (nesdev, "INES Mapper 071"):
//!
//! ```text
//! $8000-$9FFF  Mirroring control (BF9097 only)
//! $A000-$BFFF  (ignored)
//! $C000-$FFFF  PRG bank select
//! ```
//!
//! **Only `$C000-$FFFF` selects a bank**, which matters because the fixed
//! bank lives there: a game writes its selector through the half of the
//! address space that never moves, so the write cannot pull the code doing
//! it out from under itself.
//!
//! # The mirroring register, and why it is one-way
//!
//! The BF9097 board (`Fire Hawk` is the one that needs it) adds
//! single-screen mirroring at `$9000-$9FFF`: bit 4 picks which nametable.
//! nesdev is explicit that **only Fire Hawk uses it** and that a cartridge
//! without the feature ignores those writes.
//!
//! This implementation takes the write whenever it arrives and lets the
//! header decide the starting state, because the alternative needs a
//! submapper this project does not read and the failure modes are not
//! symmetric: a board that ignores the register and a game that never
//! writes it behave identically, while a game that *does* write it and is
//! ignored renders its status bar into the wrong nametable. Following the
//! write is the case that can be right.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const BANK_SIZE: usize = 16 * 1024;

pub struct Camerica {
    prg_rom: Vec<u8>,
    /// The header's mirroring, kept so a reset can return to it and so a
    /// cartridge that never writes `$9000` behaves exactly as its header
    /// says.
    header_mirroring: Mirroring,
    /// Set once the game writes the BF9097 mirroring register; `None`
    /// until then.
    single_screen: Option<Mirroring>,
    bank: u8,
}

impl Camerica {
    #[must_use]
    pub fn new(prg_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(BANK_SIZE),
            "Camerica PRG ROM must be a nonzero multiple of 16 KiB"
        );
        Camerica {
            prg_rom,
            header_mirroring: mirroring,
            single_screen: None,
            bank: 0,
        }
    }

    fn bank_count(&self) -> usize {
        (self.prg_rom.len() / BANK_SIZE).max(1)
    }
}

impl Mapper for Camerica {
    fn cpu_read(&self, addr: u16) -> u8 {
        let (bank, offset) = match addr {
            0x8000..=0xBFFF => ((self.bank as usize) % self.bank_count(), addr - 0x8000),
            0xC000..=0xFFFF => (self.bank_count() - 1, addr - 0xC000),
            _ => unreachable!("cpu_read is only ever called for $8000-$FFFF"),
        };
        self.prg_rom[bank * BANK_SIZE + offset as usize]
    }

    fn cpu_write(&mut self, addr: u16, value: u8, _cycle: u64) {
        match addr {
            // BF9097 mirroring latch. Bit 4 picks the nametable.
            0x9000..=0x9FFF => {
                self.single_screen = Some(if value & 0x10 == 0 {
                    Mirroring::OneScreenLower
                } else {
                    Mirroring::OneScreenUpper
                });
            }
            // Deliberately nothing: $8000-$8FFF and $A000-$BFFF are not
            // registers on this board, and treating a stray write there as
            // a bank select is how a game ends up executing from the wrong
            // bank after an unrelated store.
            0x8000..=0x8FFF | 0xA000..=0xBFFF => {}
            _ => self.bank = value,
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.single_screen.unwrap_or(self.header_mirroring)
    }

    fn chr_window(&self) -> Option<&[u8]> {
        None
    }

    /// `MAPR` (ticket W2-04): the PRG bank plus the BF9097 latch, which is
    /// machine state a save must carry — restoring into header mirroring
    /// would move Fire Hawk's status bar.
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.bank)?;
        out.u8(match self.single_screen {
            None => 0,
            Some(Mirroring::OneScreenLower) => 1,
            _ => 2,
        })
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.bank = inp.u8()?;
        self.single_screen = match inp.u8()? {
            0 => None,
            1 => Some(Mirroring::OneScreenLower),
            _ => Some(Mirroring::OneScreenUpper),
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cart(banks: u8) -> Camerica {
        let mut prg = vec![0u8; banks as usize * BANK_SIZE];
        for n in 0..banks {
            prg[n as usize * BANK_SIZE] = 0xB0 | n;
        }
        Camerica::new(prg, Mirroring::Vertical)
    }

    #[test]
    fn only_c000_and_above_selects_a_bank() {
        let mut m = cart(4);
        m.cpu_write(0xC000, 2, 0);
        assert_eq!(m.cpu_read(0x8000), 0xB0 | 2);

        // The three addresses that are NOT the bank register. Each would
        // silently misbank the game if it were treated as one.
        for addr in [0x8000u16, 0x8FFF, 0xA000, 0xBFFF] {
            m.cpu_write(addr, 1, 0);
            assert_eq!(
                m.cpu_read(0x8000),
                0xB0 | 2,
                "a write to ${addr:04X} must not move the PRG bank"
            );
        }
    }

    #[test]
    fn the_last_bank_is_fixed_at_c000() {
        let mut m = cart(4);
        for bank in 0..4u8 {
            m.cpu_write(0xF000, bank, 0);
            assert_eq!(
                m.cpu_read(0xC000),
                0xB0 | 3,
                "the high half is always the last bank"
            );
        }
    }

    /// Fire Hawk's register: before any write the header decides, after
    /// one the latch does.
    #[test]
    fn the_bf9097_latch_overrides_header_mirroring_once_written() {
        let mut m = cart(2);
        assert_eq!(m.mirroring(), Mirroring::Vertical, "header until written");

        m.cpu_write(0x9000, 0x00, 0);
        assert_eq!(m.mirroring(), Mirroring::OneScreenLower);
        m.cpu_write(0x9FFF, 0x10, 0);
        assert_eq!(m.mirroring(), Mirroring::OneScreenUpper);
    }

    #[test]
    fn a_bank_beyond_the_cartridge_wraps() {
        let mut m = cart(2);
        m.cpu_write(0xC000, 0xFF, 0);
        assert_eq!(m.cpu_read(0x8000), 0xB0 | 1);
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

    /// The mirroring latch is machine state, not header state: a save made
    /// after Fire Hawk sets it must come back with it set.
    #[test]
    fn bank_and_mirroring_latch_both_survive_a_state_round_trip() {
        let mut m = cart(4);
        m.cpu_write(0xC000, 3, 0);
        m.cpu_write(0x9000, 0x10, 0);

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        {
            let mut out = StateOut::new(&mut stream);
            m.save_state(&mut out).expect("writes");
        }
        let mut restored = cart(4);
        {
            let mut inp = StateIn::new(&mut stream);
            restored.load_state(&mut inp).expect("reads");
        }
        assert_eq!(restored.cpu_read(0x8000), 0xB0 | 3);
        assert_eq!(restored.mirroring(), Mirroring::OneScreenUpper);
    }
}
