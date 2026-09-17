//! Mapper 64 — Tengen RAMBO-1 (ticket W14-14).
//!
//! **9 archives** in the census of 1281 real NES archives (2026-09-17),
//! all Tengen's unlicensed line: `Klax`, `Shinobi`, `Rolling Thunder`,
//! `Road Runner`, `Skull & Crossbones`, `Hard Drivin'`, `Xybots`.
//!
//! An MMC3 relative with more registers and a second IRQ clock (nesdev,
//! "RAMBO-1"):
//!
//! ```text
//! $8000 (even)  CPKx RRRR  R = register 0-15 selected for the next $8001
//!               C = CHR A12 inversion (swap the two halves, as MMC3)
//!               P = PRG mode: 0 -> R6 @ $8000, R7 @ $A000, R15 @ $C000
//!                             1 -> R15 @ $8000, R6 @ $A000, R7 @ $C000
//!               K = CHR mode: 0 -> R0/R1 are 2 KiB at $0000/$0800
//!                             1 -> R0, R8, R1, R9 are 1 KiB at $0000..$0C00
//!               $E000-$FFFF is always the last bank
//! $8001 (odd)   register data (R0-R5, R8, R9 CHR; R6, R7, R15 PRG)
//! $A000 (even)  mirroring bit 0: 0 vertical, 1 horizontal
//! $C000 (even)  IRQ latch      $C001 (odd)  reload, and bit 0 picks the clock:
//!                              0 = scanline (PPU A12), 1 = CPU cycles / 4
//! $E000 (even)  IRQ acknowledge + disable      $E001 (odd)  IRQ enable
//! ```
//!
//! Both clocks feed one counter: the A12 pull [`Mapper::clock_irq_counter`]
//! the bus already makes, and [`Mapper::tick_cpu_cycles`] through a
//! four-cycle prescaler. The counter itself follows MMC3 revision B's
//! rule (reload at zero or on request, fire when it lands on zero).
//!
//! **Not modelled, and said so:** RAMBO-1 delays the IRQ by one clock
//! after the counter reaches zero and reloads with `latch + 1` in one
//! corner nesdev documents; this model fires on the zero. Titles that
//! time a raster split to the cycle may show it a line off.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_8K: usize = 8 * 1024;
const CHR_BANK_1K: usize = 1024;
const CHR_VIEW_SIZE: usize = 8 * 1024;

pub struct Rambo1 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    select: u8,
    registers: [u8; 16],
    mirroring_reg: u8,
    irq_latch: u8,
    irq_counter: u8,
    irq_reload: bool,
    irq_enabled: bool,
    irq_pending: bool,
    /// `$C001` bit 0: `true` = CPU-cycle clock.
    cycle_mode: bool,
    prescaler: u8,
    chr_view: [u8; CHR_VIEW_SIZE],
}

impl Rambo1 {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(PRG_BANK_8K),
            "RAMBO-1 PRG ROM must be a nonzero multiple of 8 KiB"
        );
        let mut m = Self {
            prg_rom,
            chr_rom,
            chr_is_ram,
            select: 0,
            registers: [0; 16],
            mirroring_reg: 0,
            irq_latch: 0,
            irq_counter: 0,
            irq_reload: false,
            irq_enabled: false,
            irq_pending: false,
            cycle_mode: false,
            prescaler: 0,
            chr_view: [0; CHR_VIEW_SIZE],
        };
        m.recompute_chr_view();
        m
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK_8K).max(1)
    }

    fn prg_windows(&self) -> [usize; 4] {
        let count = self.prg_bank_count();
        let r = |i: usize| usize::from(self.registers[i]) % count;
        let (r6, r7, r15, last) = (r(6), r(7), r(15), count - 1);
        if self.select & 0x40 != 0 {
            [r15, r6, r7, last]
        } else {
            [r6, r7, r15, last]
        }
    }

    fn recompute_chr_view(&mut self) {
        if self.chr_rom.is_empty() {
            return;
        }
        let count = (self.chr_rom.len() / CHR_BANK_1K).max(1);
        let r = |i: usize| usize::from(self.registers[i]) % count;
        // The low half in 2 KiB mode is R0 (even, odd), R1 (even, odd);
        // in 1 KiB mode it is R0, R8, R1, R9. The high half is R2-R5.
        let low: [usize; 4] = if self.select & 0x20 != 0 {
            [r(0), r(8), r(1), r(9)]
        } else {
            [
                usize::from(self.registers[0] & 0xFE) % count,
                usize::from(self.registers[0] | 0x01) % count,
                usize::from(self.registers[1] & 0xFE) % count,
                usize::from(self.registers[1] | 0x01) % count,
            ]
        };
        let high = [r(2), r(3), r(4), r(5)];
        let windows: [usize; 8] = if self.select & 0x80 != 0 {
            [
                high[0], high[1], high[2], high[3], low[0], low[1], low[2], low[3],
            ]
        } else {
            [
                low[0], low[1], low[2], low[3], high[0], high[1], high[2], high[3],
            ]
        };
        for (slot, &bank) in windows.iter().enumerate() {
            let src = bank * CHR_BANK_1K;
            let dst = slot * CHR_BANK_1K;
            self.chr_view[dst..dst + CHR_BANK_1K]
                .copy_from_slice(&self.chr_rom[src..src + CHR_BANK_1K]);
        }
    }

    /// One IRQ clock from either source: MMC3 revision B's rule.
    fn clock(&mut self) {
        if self.irq_counter == 0 || self.irq_reload {
            self.irq_counter = self.irq_latch;
        } else {
            self.irq_counter -= 1;
        }
        self.irq_reload = false;
        if self.irq_counter == 0 && self.irq_enabled {
            self.irq_pending = true;
        }
    }
}

impl Mapper for Rambo1 {
    fn cpu_read(&self, addr: u16) -> u8 {
        let window = usize::from((addr - 0x8000) / PRG_BANK_8K as u16);
        let bank = self.prg_windows()[window];
        self.prg_rom[bank * PRG_BANK_8K + usize::from(addr - 0x8000) % PRG_BANK_8K]
    }

    fn cpu_write(&mut self, addr: u16, value: u8, _cycle: u64) {
        match addr & 0xE001 {
            0x8000 => {
                self.select = value;
                self.recompute_chr_view();
            }
            0x8001 => {
                self.registers[usize::from(self.select & 0x0F)] = value;
                self.recompute_chr_view();
            }
            0xA000 => self.mirroring_reg = value & 0x01,
            0xC000 => self.irq_latch = value,
            0xC001 => {
                self.cycle_mode = value & 0x01 != 0;
                self.irq_reload = true;
                self.prescaler = 0;
            }
            0xE000 => {
                self.irq_enabled = false;
                self.irq_pending = false;
            }
            0xE001 => self.irq_enabled = true,
            _ => unreachable!("`& 0xE001` bounds this to the 8 listed values"),
        }
    }

    fn mirroring(&self) -> Mirroring {
        if self.mirroring_reg & 1 != 0 {
            Mirroring::Horizontal
        } else {
            Mirroring::Vertical
        }
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.is_empty() {
            None
        } else {
            Some(&self.chr_view[..])
        }
    }

    fn clock_irq_counter(&mut self) {
        if !self.cycle_mode {
            self.clock();
        }
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending
    }

    fn tick_cpu_cycles(&mut self, cycles: u32) {
        if !self.cycle_mode {
            return;
        }
        for _ in 0..cycles {
            self.prescaler = (self.prescaler + 1) & 0x03;
            if self.prescaler == 0 {
                self.clock();
            }
        }
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.select)?;
        out.bytes(&self.registers)?;
        out.u8(self.mirroring_reg)?;
        out.u8(self.irq_latch)?;
        out.u8(self.irq_counter)?;
        out.bool(self.irq_reload)?;
        out.bool(self.irq_enabled)?;
        out.bool(self.irq_pending)?;
        out.bool(self.cycle_mode)?;
        out.u8(self.prescaler)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.select = inp.u8()?;
        inp.bytes(&mut self.registers)?;
        self.mirroring_reg = inp.u8()?;
        self.irq_latch = inp.u8()?;
        self.irq_counter = inp.u8()?;
        self.irq_reload = inp.bool()?;
        self.irq_enabled = inp.bool()?;
        self.irq_pending = inp.bool()?;
        self.cycle_mode = inp.bool()?;
        self.prescaler = inp.u8()?;
        self.recompute_chr_view();
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

    fn chr(banks_1k: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks_1k as usize * CHR_BANK_1K];
        for n in 0..banks_1k {
            data[n as usize * CHR_BANK_1K] = 0x80 + n;
        }
        data
    }

    fn set(m: &mut Rambo1, mode_bits: u8, reg: u8, value: u8) {
        m.cpu_write(0x8000, mode_bits | reg, 0);
        m.cpu_write(0x8001, value, 0);
    }

    #[test]
    fn three_prg_registers_and_the_p_bit_rotate_them() {
        let mut m = Rambo1::new(prg(16), chr(64), false);
        set(&mut m, 0, 6, 1);
        set(&mut m, 0, 7, 2);
        set(&mut m, 0, 15, 3);
        assert_eq!(m.cpu_read(0x8000), 0x41);
        assert_eq!(m.cpu_read(0xA000), 0x42);
        assert_eq!(m.cpu_read(0xC000), 0x43);
        assert_eq!(m.cpu_read(0xE000), 0x40 + 15);
        set(&mut m, 0x40, 15, 3); // P = 1
        assert_eq!(m.cpu_read(0x8000), 0x43);
        assert_eq!(m.cpu_read(0xA000), 0x41);
        assert_eq!(m.cpu_read(0xC000), 0x42);
    }

    #[test]
    fn the_k_bit_splits_the_2k_banks_into_r0_r8_r1_r9() {
        let mut m = Rambo1::new(prg(16), chr(64), false);
        set(&mut m, 0, 0, 10);
        set(&mut m, 0, 1, 20);
        set(&mut m, 0, 8, 30);
        set(&mut m, 0, 9, 40);
        let w = m.chr_window().unwrap();
        assert_eq!(
            [w[0], w[0x400], w[0x800], w[0xC00]],
            [0x8A, 0x8B, 0x94, 0x95]
        );
        m.cpu_write(0x8000, 0x20, 0); // K = 1
        let w = m.chr_window().unwrap();
        assert_eq!(
            [w[0], w[0x400], w[0x800], w[0xC00]],
            [0x8A, 0x9E, 0x94, 0xA8]
        );
        m.cpu_write(0x8000, 0xA0, 0); // K = 1, C = 1: halves swap
        let w = m.chr_window().unwrap();
        assert_eq!(w[0x1000], 0x8A);
    }

    #[test]
    fn cycle_mode_clocks_the_counter_once_per_four_cpu_cycles() {
        let mut m = Rambo1::new(prg(16), chr(64), false);
        m.cpu_write(0xC000, 2, 0); // latch 2
        m.cpu_write(0xC001, 1, 0); // cycle mode, reload
        m.cpu_write(0xE001, 0, 0); // enable
        m.tick_cpu_cycles(4); // reload -> 2
        m.tick_cpu_cycles(4); // -> 1
        assert!(!m.irq_pending());
        m.tick_cpu_cycles(3);
        assert!(!m.irq_pending(), "not yet four cycles");
        m.tick_cpu_cycles(1); // -> 0: fires
        assert!(m.irq_pending());
        m.cpu_write(0xE000, 0, 0);
        assert!(!m.irq_pending(), "$E000 acknowledges and disables");
        // In scanline mode the A12 clock drives it and cycles do nothing.
        m.cpu_write(0xC001, 0, 0);
        m.cpu_write(0xE001, 0, 0);
        m.tick_cpu_cycles(1000);
        assert!(!m.irq_pending());
        m.clock_irq_counter(); // reload -> 2
        m.clock_irq_counter(); // 1
        m.clock_irq_counter(); // 0
        assert!(m.irq_pending());
    }
}
