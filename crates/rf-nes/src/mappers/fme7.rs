//! Mapper 69 — Sunsoft FME-7 / 5A / 5B (ticket W14-14).
//!
//! **4 archives** in the census of 1281 real NES archives (2026-09-17):
//! `Batman - Return of the Joker` (two), `Mr. Gimmick` and `Ufouria`
//! prototypes.
//!
//! Two ports (nesdev, "Sunsoft FME-7"): a command at `$8000-$9FFF` (low
//! four bits) and its parameter at `$A000-$BFFF`:
//!
//! ```text
//! $0-$7  CHR 1 KiB bank for PPU $0000 + n*$400          (8 bits)
//! $8     $6000-$7FFF: bits 0-5 bank, bit 6 RAM select, bit 7 RAM enable
//! $9-$B  PRG 8 KiB bank at $8000 / $A000 / $C000          (bits 0-5)
//!        $E000-$FFFF is fixed to the last bank
//! $C     mirroring: 0 vertical, 1 horizontal, 2 one-screen A, 3 one-screen B
//! $D     IRQ control: bit 0 IRQ enable, bit 7 counter enable; writing acks
//! $E/$F  IRQ counter low / high
//! ```
//!
//! The counter decrements **once per CPU cycle** while enabled and raises
//! the IRQ when it wraps `$0000 -> $FFFF` with the IRQ enabled — which is
//! why this board needed [`Mapper::tick_cpu_cycles`]; nothing on the trait
//! could see CPU cycles before it.
//!
//! **Not modelled, and said so:** command `$8`'s ROM-at-`$6000` (the bus
//! has no `$6000-$7FFF` mapper hook; the region is the bus's own 8 KiB PRG
//! RAM, which is what these titles keep there), and the 5B's YM2149 audio
//! (`Mr. Gimmick`; expansion audio is not in this crate).

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_8K: usize = 8 * 1024;
const CHR_BANK_1K: usize = 1024;
const CHR_VIEW_SIZE: usize = 8 * 1024;

pub struct Fme7 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    command: u8,
    chr_banks: [u8; 8],
    /// Commands `$9`-`$B`.
    prg_banks: [u8; 3],
    mirroring: Mirroring,
    irq_enabled: bool,
    counter_enabled: bool,
    counter: u16,
    irq_pending: bool,
    chr_view: [u8; CHR_VIEW_SIZE],
}

impl Fme7 {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(PRG_BANK_8K),
            "FME-7 PRG ROM must be a nonzero multiple of 8 KiB"
        );
        let mut m = Self {
            prg_rom,
            chr_rom,
            chr_is_ram,
            command: 0,
            chr_banks: [0; 8],
            prg_banks: [0; 3],
            mirroring: Mirroring::Vertical,
            irq_enabled: false,
            counter_enabled: false,
            counter: 0,
            irq_pending: false,
            chr_view: [0; CHR_VIEW_SIZE],
        };
        m.recompute_chr_view();
        m
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK_8K).max(1)
    }

    fn recompute_chr_view(&mut self) {
        if self.chr_rom.is_empty() {
            return;
        }
        let count = (self.chr_rom.len() / CHR_BANK_1K).max(1);
        for (slot, &bank) in self.chr_banks.iter().enumerate() {
            let src = (usize::from(bank) % count) * CHR_BANK_1K;
            let dst = slot * CHR_BANK_1K;
            self.chr_view[dst..dst + CHR_BANK_1K]
                .copy_from_slice(&self.chr_rom[src..src + CHR_BANK_1K]);
        }
    }

    fn write_parameter(&mut self, value: u8) {
        match self.command & 0x0F {
            n @ 0x0..=0x7 => {
                self.chr_banks[usize::from(n)] = value;
                self.recompute_chr_view();
            }
            0x8 => {} // $6000-$7FFF banking: not modelled (module doc)
            n @ 0x9..=0xB => self.prg_banks[usize::from(n - 0x9)] = value & 0x3F,
            0xC => {
                self.mirroring = match value & 0x03 {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::OneScreenLower,
                    _ => Mirroring::OneScreenUpper,
                }
            }
            0xD => {
                self.irq_enabled = value & 0x01 != 0;
                self.counter_enabled = value & 0x80 != 0;
                self.irq_pending = false;
            }
            0xE => self.counter = (self.counter & 0xFF00) | u16::from(value),
            _ => self.counter = (self.counter & 0x00FF) | (u16::from(value) << 8),
        }
    }
}

impl Mapper for Fme7 {
    fn cpu_read(&self, addr: u16) -> u8 {
        let count = self.prg_bank_count();
        let window = usize::from((addr - 0x8000) / PRG_BANK_8K as u16);
        let bank = if window == 3 {
            count - 1
        } else {
            usize::from(self.prg_banks[window]) % count
        };
        self.prg_rom[bank * PRG_BANK_8K + usize::from(addr - 0x8000) % PRG_BANK_8K]
    }

    fn cpu_write(&mut self, addr: u16, value: u8, _cycle: u64) {
        match addr & 0xE000 {
            0x8000 => self.command = value & 0x0F,
            0xA000 => self.write_parameter(value),
            _ => {}
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

    fn irq_pending(&self) -> bool {
        self.irq_pending
    }

    fn tick_cpu_cycles(&mut self, cycles: u32) {
        if !self.counter_enabled {
            return;
        }
        for _ in 0..cycles {
            let wrapped = self.counter == 0;
            self.counter = self.counter.wrapping_sub(1);
            if wrapped && self.irq_enabled {
                self.irq_pending = true;
            }
        }
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.command)?;
        out.bytes(&self.chr_banks)?;
        out.bytes(&self.prg_banks)?;
        out.u8(match self.mirroring {
            Mirroring::Vertical => 0,
            Mirroring::Horizontal => 1,
            Mirroring::OneScreenLower => 2,
            _ => 3,
        })?;
        out.bool(self.irq_enabled)?;
        out.bool(self.counter_enabled)?;
        out.u16(self.counter)?;
        out.bool(self.irq_pending)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.command = inp.u8()?;
        inp.bytes(&mut self.chr_banks)?;
        inp.bytes(&mut self.prg_banks)?;
        self.mirroring = match inp.u8()? {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::OneScreenLower,
            _ => Mirroring::OneScreenUpper,
        };
        self.irq_enabled = inp.bool()?;
        self.counter_enabled = inp.bool()?;
        self.counter = inp.u16()?;
        self.irq_pending = inp.bool()?;
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

    fn cmd(m: &mut Fme7, command: u8, value: u8) {
        m.cpu_write(0x8000, command, 0);
        m.cpu_write(0xA000, value, 0);
    }

    #[test]
    fn three_prg_windows_switch_and_e000_is_the_last_bank() {
        let mut m = Fme7::new(prg(32), chr(64), false);
        cmd(&mut m, 0x9, 5);
        cmd(&mut m, 0xA, 6);
        cmd(&mut m, 0xB, 7);
        assert_eq!(m.cpu_read(0x8000), 0x45);
        assert_eq!(m.cpu_read(0xA000), 0x46);
        assert_eq!(m.cpu_read(0xC000), 0x47);
        assert_eq!(m.cpu_read(0xE000), 0x40 + 31);
    }

    #[test]
    fn eight_chr_commands_fill_the_window_and_c_sets_mirroring() {
        let mut m = Fme7::new(prg(32), chr(64), false);
        for n in 0..8u8 {
            cmd(&mut m, n, 63 - n);
        }
        let w = m.chr_window().unwrap();
        for n in 0..8usize {
            assert_eq!(w[n * CHR_BANK_1K], 0x80 + 63 - n as u8);
        }
        cmd(&mut m, 0xC, 2);
        assert_eq!(m.mirroring(), Mirroring::OneScreenLower);
        cmd(&mut m, 0xC, 1);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
    }

    #[test]
    fn the_counter_raises_the_irq_only_on_the_wrap_and_only_when_enabled() {
        let mut m = Fme7::new(prg(32), chr(64), false);
        cmd(&mut m, 0xE, 0x02);
        cmd(&mut m, 0xF, 0x00);
        cmd(&mut m, 0xD, 0x80); // counter on, IRQ off
        m.tick_cpu_cycles(10);
        assert!(!m.irq_pending(), "wrapped, but IRQ was not enabled");
        cmd(&mut m, 0xE, 0x02);
        cmd(&mut m, 0xF, 0x00); // the wrap above left the high byte at $FF
        cmd(&mut m, 0xD, 0x81); // both on; the write also acks
        m.tick_cpu_cycles(2);
        assert!(!m.irq_pending(), "0002 -> 0000 is not yet the wrap");
        m.tick_cpu_cycles(1);
        assert!(m.irq_pending(), "0000 -> FFFF raises it");
        cmd(&mut m, 0xD, 0x81);
        assert!(!m.irq_pending(), "writing $D acknowledges");
        cmd(&mut m, 0xD, 0x01); // counter off: nothing counts
        m.tick_cpu_cycles(100_000);
        assert!(!m.irq_pending());
    }
}
