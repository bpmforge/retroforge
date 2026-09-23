//! Mapper 18 — Jaleco SS88006 (ticket W14-50).
//!
//! **4 archives** tallied 2026-09-22 by a header scan of `~/Games/Roms/nes`
//! (not the orchestrator's census — see this ticket's plan.json note):
//! `Pizza Pop!`, `USA Ice Hockey in FC`, and two `Ninja JaJaMaru` dumps
//! (`... - The Legend of the Golden Castle` and `... - Operation Milky
//! Way`).
//!
//! Register model per
//! [nesdev.org/wiki/INES_Mapper_018](https://www.nesdev.org/wiki/INES_Mapper_018),
//! decoded on `addr & 0xF003` — that page's own "Range,Mask: $8000-FFFF,
//! $F003" note, meaning every address bit outside that mask is a mirror of
//! the canonical one:
//!
//! ```text
//! $8000/$8001  PRG bank at $8000-$9FFF, low/high nibble (8-bit bank #)
//! $8002/$8003  PRG bank at $A000-$BFFF, low/high nibble
//! $9000/$9001  PRG bank at $C000-$DFFF, low/high nibble
//!              $E000-$FFFF is fixed to the last bank
//! $9002        PRG RAM protect: bit 0 chip enable, bit 1 write protect
//! $A000/$A001  CHR 1 KiB bank at PPU $0000, low/high nibble
//! $A002/$A003  CHR 1 KiB bank at PPU $0400
//! $B000/$B001  CHR 1 KiB bank at PPU $0800
//! $B002/$B003  CHR 1 KiB bank at PPU $0C00
//! $C000/$C001  CHR 1 KiB bank at PPU $1000
//! $C002/$C003  CHR 1 KiB bank at PPU $1400
//! $D000/$D001  CHR 1 KiB bank at PPU $1800
//! $D002/$D003  CHR 1 KiB bank at PPU $1C00
//! $E000-$E003  IRQ reload value, 4 nibbles, least significant first
//! $F000        IRQ reload/acknowledge (copies the full 16-bit reload
//!              value into the counter regardless of the size select)
//! $F001        IRQ control: bit 0 enable, bits 1-3 counter-size select
//! $F002        mirroring: 0 horizontal, 1 vertical, 2 1ScA, 3 1ScB
//! $F003        expansion sound (uPD7755C/7756C ADPCM) — out of scope
//! ```
//!
//! Every PRG/CHR bank-select register is `[.... PPPP]`/`[.... CCCC]`: only
//! the low 4 bits of each write are significant, and a low/high register
//! pair combines into one 8-bit bank number, "same as CHR, $x000 low,
//! $x001 high" (Disch's notes, quoted on the wiki page above).
//!
//! ## The IRQ counter's size select
//!
//! Quoting the page's `$F001` bit diagram and the paragraph under it:
//!
//! ```text
//! 7  bit  0
//! ---------
//! .... FETC
//!      ||||
//!      |||+- 1: Enable counting
//!      ||+-- 1: Don't propagate counter borrow to bit 12; instead assert IRQ
//!      |+--- 1: Don't propagate counter borrow to bit 8; instead assert IRQ
//!      +---- 1: Don't propagate counter borrow to bit 4; instead assert IRQ
//! ```
//! "F overrides E overrides T. If none are set, the counter is 16 bits
//! wide." — i.e. bit 3 set picks 4-bit mode outright; else bit 2 set picks
//! 8-bit; else bit 1 set picks 12-bit; else 16-bit. Disch's notes restate
//! this as a priority-encoded 2-bit-wide field: `%000`=16, `%001`=12,
//! `%01x`=8, `%1xx`=4.
//!
//! "When enabled, the counter counts down [every CPU cycle]... If the
//! counter is less than 16 bits, the high bits are not altered by IRQ
//! counter clocking; they retain their value" and an IRQ fires exactly when
//! the *selected width's* low bits underflow — Disch's worked example:
//! `$1232` in 4-bit mode counts `$1232 -> $1231 -> $1230 -> $123F <-- IRQ
//! here -> $123E ...` (the high byte `$12` never moves). This is why
//! [`Ss88006::tick_cpu_cycles`] keeps one full `u16` counter and only masks
//! the low `width` bits when checking for the wrap, exactly like that
//! example, rather than three independently-sized counters.
//!
//! "Any write to $F000 or $F001 will acknowledge the IRQ."
//!
//! ## Not modelled, and said so
//!
//! `$F003`'s expansion ADPCM sound register (per this ticket's acceptance:
//! "the expansion sound pin is out of scope") — writes there are accepted
//! (so the decode table stays complete and no game's write is misrouted
//! into another register) but produce no audio and are not stored; no
//! title in this ticket's 4-archive set uses the sound IC (nesdev.org
//! names `The Lord of King`/`Magic John`/`Plasma Ball` as the boards that
//! do, none of which are in the census this ticket tallied).

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_8K: usize = 8 * 1024;
const CHR_BANK_1K: usize = 1024;
const CHR_VIEW_SIZE: usize = 8 * 1024;

pub struct Ss88006 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    /// Low/high nibble per PRG window ($8000-$9FFF/$A000-$BFFF/$C000-$DFFF).
    prg_low: [u8; 3],
    prg_high: [u8; 3],
    /// Low/high nibble per 1 KiB CHR slot, $0000..$1C00 in order.
    chr_low: [u8; 8],
    chr_high: [u8; 8],
    chr_view: [u8; CHR_VIEW_SIZE],
    prg_ram_chip_enable: bool,
    prg_ram_write_allow: bool,
    mirroring: Mirroring,
    irq_reload: u16,
    irq_counter: u16,
    irq_control: u8,
    irq_pending: bool,
}

impl Ss88006 {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(PRG_BANK_8K),
            "SS88006 PRG ROM must be a nonzero multiple of 8 KiB"
        );
        let mut m = Self {
            prg_rom,
            chr_rom,
            chr_is_ram,
            prg_low: [0; 3],
            prg_high: [0; 3],
            chr_low: [0; 8],
            chr_high: [0; 8],
            chr_view: [0; CHR_VIEW_SIZE],
            prg_ram_chip_enable: false,
            prg_ram_write_allow: false,
            mirroring: Mirroring::Vertical,
            irq_reload: 0,
            irq_counter: 0,
            irq_control: 0,
            irq_pending: false,
        };
        m.recompute_chr_view();
        m
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK_8K).max(1)
    }

    fn chr_bank_count(&self) -> usize {
        (self.chr_rom.len() / CHR_BANK_1K).max(1)
    }

    fn recompute_chr_view(&mut self) {
        if self.chr_rom.is_empty() {
            return;
        }
        let count = self.chr_bank_count();
        for slot in 0..8 {
            let bank = usize::from(self.chr_low[slot] | (self.chr_high[slot] << 4)) % count;
            let src = bank * CHR_BANK_1K;
            let dst = slot * CHR_BANK_1K;
            self.chr_view[dst..dst + CHR_BANK_1K]
                .copy_from_slice(&self.chr_rom[src..src + CHR_BANK_1K]);
        }
    }

    /// The `$F001` size-select bits, priority-encoded per this module's
    /// doc: bit 3 (4-bit) overrides bit 2 (8-bit) overrides bit 1
    /// (12-bit); with none set, the counter is the full 16 bits.
    fn counter_width_mask(&self) -> u16 {
        if self.irq_control & 0x08 != 0 {
            0x000F
        } else if self.irq_control & 0x04 != 0 {
            0x00FF
        } else if self.irq_control & 0x02 != 0 {
            0x0FFF
        } else {
            0xFFFF
        }
    }

    fn counting_enabled(&self) -> bool {
        self.irq_control & 0x01 != 0
    }
}

impl Mapper for Ss88006 {
    fn cpu_read(&self, addr: u16) -> u8 {
        let count = self.prg_bank_count();
        let window = usize::from((addr - 0x8000) / PRG_BANK_8K as u16);
        let bank = if window == 3 {
            count - 1
        } else {
            usize::from(self.prg_low[window] | (self.prg_high[window] << 4)) % count
        };
        self.prg_rom[bank * PRG_BANK_8K + usize::from(addr - 0x8000) % PRG_BANK_8K]
    }

    fn cpu_write(&mut self, addr: u16, value: u8, _cycle: u64) {
        let nibble = value & 0x0F;
        match addr & 0xF003 {
            0x8000 => self.prg_low[0] = nibble,
            0x8001 => self.prg_high[0] = nibble,
            0x8002 => self.prg_low[1] = nibble,
            0x8003 => self.prg_high[1] = nibble,
            0x9000 => self.prg_low[2] = nibble,
            0x9001 => self.prg_high[2] = nibble,
            0x9002 => {
                self.prg_ram_chip_enable = value & 0x01 != 0;
                self.prg_ram_write_allow = value & 0x02 != 0;
            }
            0xA000 => {
                self.chr_low[0] = nibble;
                self.recompute_chr_view();
            }
            0xA001 => {
                self.chr_high[0] = nibble;
                self.recompute_chr_view();
            }
            0xA002 => {
                self.chr_low[1] = nibble;
                self.recompute_chr_view();
            }
            0xA003 => {
                self.chr_high[1] = nibble;
                self.recompute_chr_view();
            }
            0xB000 => {
                self.chr_low[2] = nibble;
                self.recompute_chr_view();
            }
            0xB001 => {
                self.chr_high[2] = nibble;
                self.recompute_chr_view();
            }
            0xB002 => {
                self.chr_low[3] = nibble;
                self.recompute_chr_view();
            }
            0xB003 => {
                self.chr_high[3] = nibble;
                self.recompute_chr_view();
            }
            0xC000 => {
                self.chr_low[4] = nibble;
                self.recompute_chr_view();
            }
            0xC001 => {
                self.chr_high[4] = nibble;
                self.recompute_chr_view();
            }
            0xC002 => {
                self.chr_low[5] = nibble;
                self.recompute_chr_view();
            }
            0xC003 => {
                self.chr_high[5] = nibble;
                self.recompute_chr_view();
            }
            0xD000 => {
                self.chr_low[6] = nibble;
                self.recompute_chr_view();
            }
            0xD001 => {
                self.chr_high[6] = nibble;
                self.recompute_chr_view();
            }
            0xD002 => {
                self.chr_low[7] = nibble;
                self.recompute_chr_view();
            }
            0xD003 => {
                self.chr_high[7] = nibble;
                self.recompute_chr_view();
            }
            0xE000 => self.irq_reload = (self.irq_reload & 0xFFF0) | u16::from(nibble),
            0xE001 => self.irq_reload = (self.irq_reload & 0xFF0F) | (u16::from(nibble) << 4),
            0xE002 => self.irq_reload = (self.irq_reload & 0xF0FF) | (u16::from(nibble) << 8),
            0xE003 => self.irq_reload = (self.irq_reload & 0x0FFF) | (u16::from(nibble) << 12),
            0xF000 => {
                // "Any write to this register will immediately reload the
                // IRQ counter from the above reload value and acknowledge
                // the IRQ" -- the full 16 bits, "regardless of current 'S'
                // value" (Disch's notes).
                self.irq_counter = self.irq_reload;
                self.irq_pending = false;
            }
            0xF001 => {
                self.irq_control = value & 0x0F;
                self.irq_pending = false; // "Writes to this register also acknowledge the IRQ."
            }
            0xF002 => {
                self.mirroring = match value & 0x03 {
                    0 => Mirroring::Horizontal,
                    1 => Mirroring::Vertical,
                    2 => Mirroring::OneScreenLower, // 1ScA (Ground)
                    _ => Mirroring::OneScreenUpper, // 1ScB (Vcc)
                };
            }
            // $F003: expansion ADPCM sound -- out of scope (module doc).
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

    /// "When enabled, the counter counts down" every CPU cycle; when the
    /// selected width's low bits underflow, they wrap within that width
    /// (the untouched high bits are the module doc's `$1232` example) and
    /// the IRQ fires. Cycle count is bounded (DMA/instruction lengths are
    /// small), so this loop always terminates.
    fn tick_cpu_cycles(&mut self, cycles: u32) {
        if !self.counting_enabled() {
            return;
        }
        let mask = self.counter_width_mask();
        for _ in 0..cycles {
            let low = self.irq_counter & mask;
            if low == 0 {
                self.irq_counter = (self.irq_counter & !mask) | mask;
                self.irq_pending = true;
            } else {
                self.irq_counter = (self.irq_counter & !mask) | (low - 1);
            }
        }
    }

    fn prg_ram_write_enabled(&self) -> bool {
        self.prg_ram_chip_enable && self.prg_ram_write_allow
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.bytes(&self.prg_low)?;
        out.bytes(&self.prg_high)?;
        out.bytes(&self.chr_low)?;
        out.bytes(&self.chr_high)?;
        out.bool(self.prg_ram_chip_enable)?;
        out.bool(self.prg_ram_write_allow)?;
        out.u8(match self.mirroring {
            Mirroring::Horizontal => 0,
            Mirroring::Vertical => 1,
            Mirroring::OneScreenLower => 2,
            _ => 3,
        })?;
        out.u16(self.irq_reload)?;
        out.u16(self.irq_counter)?;
        out.u8(self.irq_control)?;
        out.bool(self.irq_pending)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        inp.bytes(&mut self.prg_low)?;
        inp.bytes(&mut self.prg_high)?;
        inp.bytes(&mut self.chr_low)?;
        inp.bytes(&mut self.chr_high)?;
        self.prg_ram_chip_enable = inp.bool()?;
        self.prg_ram_write_allow = inp.bool()?;
        self.mirroring = match inp.u8()? {
            0 => Mirroring::Horizontal,
            1 => Mirroring::Vertical,
            2 => Mirroring::OneScreenLower,
            _ => Mirroring::OneScreenUpper,
        };
        self.irq_reload = inp.u16()?;
        self.irq_counter = inp.u16()?;
        self.irq_control = inp.u8()?;
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
            data[n as usize * PRG_BANK_8K] = 0x40u8.wrapping_add(n);
        }
        data
    }

    fn chr(banks_1k: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks_1k as usize * CHR_BANK_1K];
        for n in 0..banks_1k {
            data[n as usize * CHR_BANK_1K] = 0x80u8.wrapping_add(n);
        }
        data
    }

    /// Write an 8-bit bank number as the documented low/high nibble pair.
    fn write_pair(m: &mut Ss88006, low_addr: u16, high_addr: u16, bank: u8) {
        m.cpu_write(low_addr, bank & 0x0F, 0);
        m.cpu_write(high_addr, bank >> 4, 0);
    }

    #[test]
    fn three_prg_windows_switch_and_e000_is_the_last_bank() {
        // 256 banks needs the full 8-bit (4+4 nibble) range to be exercised.
        let mut m = Ss88006::new(prg(200), chr(8), false);
        write_pair(&mut m, 0x8000, 0x8001, 130); // needs the high nibble
        write_pair(&mut m, 0x8002, 0x8003, 6);
        write_pair(&mut m, 0x9000, 0x9001, 7);
        assert_eq!(m.cpu_read(0x8000), 0x40u8.wrapping_add(130));
        assert_eq!(m.cpu_read(0xA000), 0x46);
        assert_eq!(m.cpu_read(0xC000), 0x47);
        assert_eq!(m.cpu_read(0xE000), 0x40u8.wrapping_add(199));
    }

    #[test]
    fn address_bits_outside_the_f003_mask_mirror_the_canonical_register() {
        // "Range,Mask: $8000-FFFF, $F003" -- $8010 must land on the same
        // register as $8000.
        let mut m = Ss88006::new(prg(8), chr(8), false);
        m.cpu_write(0x8010, 5, 0);
        assert_eq!(m.cpu_read(0x8000), 0x40 + 5);
    }

    #[test]
    fn eight_chr_pairs_fill_the_window_low_to_high() {
        let mut m = Ss88006::new(prg(4), chr(200), false);
        let regs: [(u16, u16); 8] = [
            (0xA000, 0xA001),
            (0xA002, 0xA003),
            (0xB000, 0xB001),
            (0xB002, 0xB003),
            (0xC000, 0xC001),
            (0xC002, 0xC003),
            (0xD000, 0xD001),
            (0xD002, 0xD003),
        ];
        for (slot, (lo, hi)) in regs.iter().enumerate() {
            write_pair(&mut m, *lo, *hi, 150 + slot as u8);
        }
        let w = m.chr_window().unwrap();
        for slot in 0..8usize {
            assert_eq!(
                w[slot * CHR_BANK_1K],
                0x80u8.wrapping_add(150).wrapping_add(slot as u8)
            );
        }
    }

    #[test]
    fn f002_mirroring_matches_the_00_horizontal_01_vertical_table() {
        let mut m = Ss88006::new(prg(4), chr(8), false);
        m.cpu_write(0xF002, 0, 0);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
        m.cpu_write(0xF002, 1, 0);
        assert_eq!(m.mirroring(), Mirroring::Vertical);
        m.cpu_write(0xF002, 2, 0);
        assert_eq!(m.mirroring(), Mirroring::OneScreenLower);
        m.cpu_write(0xF002, 3, 0);
        assert_eq!(m.mirroring(), Mirroring::OneScreenUpper);
    }

    #[test]
    fn prg_ram_needs_both_chip_enable_and_write_allow() {
        let m0 = Ss88006::new(prg(4), chr(8), false);
        assert!(!m0.prg_ram_write_enabled(), "power-on: both bits clear");

        let mut m = Ss88006::new(prg(4), chr(8), false);
        m.cpu_write(0x9002, 0x01, 0); // chip enable only
        assert!(!m.prg_ram_write_enabled());
        m.cpu_write(0x9002, 0x02, 0); // write-allow only
        assert!(!m.prg_ram_write_enabled());
        m.cpu_write(0x9002, 0x03, 0); // both
        assert!(m.prg_ram_write_enabled());
    }

    /// nesdev's own worked example: `$1232` in 4-bit mode counts `$1232 ->
    /// $1231 -> $1230 -> $123F <- IRQ -> $123E`, the high byte untouched.
    #[test]
    fn four_bit_mode_wraps_within_the_low_nibble_and_leaves_the_high_byte() {
        let mut m = Ss88006::new(prg(4), chr(8), false);
        m.irq_reload = 0x1232;
        m.cpu_write(0xF000, 0, 0); // reload
        m.cpu_write(0xF001, 0b1001, 0); // F(4-bit) + E(nable)
        m.tick_cpu_cycles(1);
        assert_eq!(m.irq_counter, 0x1231);
        assert!(!m.irq_pending());
        m.tick_cpu_cycles(1);
        assert_eq!(m.irq_counter, 0x1230);
        assert!(!m.irq_pending());
        m.tick_cpu_cycles(1);
        assert_eq!(m.irq_counter, 0x123F, "wraps within the low nibble only");
        assert!(m.irq_pending(), "the wrap raises the IRQ");
        m.tick_cpu_cycles(1);
        assert_eq!(m.irq_counter, 0x123E);
    }

    #[test]
    fn twelve_and_eight_bit_modes_pick_the_documented_widths() {
        // 12-bit: bit 1 set, bits 2-3 clear.
        let mut m12 = Ss88006::new(prg(4), chr(8), false);
        assert_eq!(
            {
                m12.irq_control = 0b0010;
                m12.counter_width_mask()
            },
            0x0FFF
        );
        // 8-bit: bit 2 set (bit 1 also set is still 8-bit -- E overrides T).
        let mut m8 = Ss88006::new(prg(4), chr(8), false);
        m8.irq_control = 0b0110;
        assert_eq!(m8.counter_width_mask(), 0x00FF);
        // 4-bit: bit 3 set overrides everything else.
        let mut m4 = Ss88006::new(prg(4), chr(8), false);
        m4.irq_control = 0b1111;
        assert_eq!(m4.counter_width_mask(), 0x000F);
        // none set: full 16-bit.
        let m16 = Ss88006::new(prg(4), chr(8), false);
        assert_eq!(m16.counter_width_mask(), 0xFFFF);
    }

    #[test]
    fn sixteen_bit_mode_wraps_like_fme7_and_disabled_counting_does_nothing() {
        let mut m = Ss88006::new(prg(4), chr(8), false);
        m.cpu_write(0xE000, 0x02, 0);
        m.cpu_write(0xE001, 0x00, 0);
        m.cpu_write(0xE002, 0x00, 0);
        m.cpu_write(0xE003, 0x00, 0);
        m.cpu_write(0xF000, 0, 0); // reload -> counter = 2
        m.cpu_write(0xF001, 0b0000, 0); // enable bit clear: no counting
        m.tick_cpu_cycles(10);
        assert!(!m.irq_pending(), "counting disabled entirely");
        m.cpu_write(0xF001, 0b0001, 0); // enable, 16-bit
        m.tick_cpu_cycles(2);
        assert!(!m.irq_pending(), "2 -> 0 is not yet the wrap");
        m.tick_cpu_cycles(1);
        assert!(m.irq_pending(), "0 -> FFFF raises it");
        m.cpu_write(0xF001, 0b0001, 0);
        assert!(!m.irq_pending(), "writing $F001 acknowledges");
    }

    #[test]
    fn f000_reload_acknowledges_regardless_of_size_select() {
        let mut m = Ss88006::new(prg(4), chr(8), false);
        m.irq_control = 0b1001; // 4-bit, enabled
        m.irq_counter = 0;
        m.tick_cpu_cycles(1); // wraps in 4-bit mode, sets irq_pending
        assert!(m.irq_pending());
        m.irq_reload = 0xABCD;
        m.cpu_write(0xF000, 0, 0);
        assert!(!m.irq_pending(), "F000 write acknowledges");
        assert_eq!(m.irq_counter, 0xABCD, "full 16 bits copied regardless of S");
    }
}
