//! MMC1 / SxROM (mapper 1) —
//! [nesdev.org/wiki/MMC1](https://www.nesdev.org/wiki/MMC1). The most
//! stateful of this ticket's four mappers: a 5-bit serial shift register
//! fed one bit per `$8000-$FFFF` write, committed into one of four
//! internal registers (selected by address bits 14-13) on the 5th write,
//! plus a bit-7 reset and a consecutive-write-ignore quirk.
//!
//! ## Load register (`$8000-$FFFF`), quoted verbatim from nesdev
//!
//! ```text
//! 7  bit  0
//! ---- ----
//! Rxxx xxxD
//! |       |
//! |       +- Data bit to be shifted into shift register, LSB first
//! +--------- A write with bit set will reset shift register
//!             and write Control with (Control OR $0C),
//!             locking PRG-ROM at $C000-$FFFF to the last bank.
//! ```
//! "When the serial port is written to on consecutive cycles, it ignores
//! every write after the first. ... The bit 7 reset is never ignored."
//! Real RMW instructions (`ASL $8000`, etc.) write the *unmodified* value
//! on one cycle and the *modified* value on the immediately next cycle —
//! the source of the documented *Bill & Ted's Excellent Adventure*
//! bug-bait this quirk exists to reproduce.
//!
//! ## Control (internal, `$8000-$9FFF`), quoted verbatim from nesdev
//!
//! ```text
//! 4bit0
//! -----
//! CPPMM
//! |||||
//! |||++- Nametable arrangement: (0: one-screen, screen A; 1: one-screen,
//! |||               screen B; 2: horizontal arrangement ("vertical mirroring",
//! |||               PPU A10); 3: vertical arrangement ("horizontal mirroring",
//! |||               PPU A11) )
//! |++--- PRG-ROM bank mode (0, 1: switch 32 KB at $8000, ignoring low bit of
//! |                         bank number; 2: fix first bank at $8000 and switch
//! |                         16 KB bank at $C000; 3: fix last bank at $C000 and
//! |                         switch 16 KB bank at $8000)
//! +----- CHR-ROM bank mode (0: switch 8 KB at a time; 1: switch two separate
//!                           4 KB banks)
//! ```
//! nesdev's own "arrangement" wording for values 2/3 is famously
//! confusing (it names the *visual layout* of the two mirrored screens,
//! which is the inverse of the "mirroring type" terminology
//! [nesdev.org/wiki/Mirroring](https://www.nesdev.org/wiki/Mirroring) and
//! this crate's own `rf_cart::Mirroring`/`ppu/mem.rs` use) — nesdev's own
//! parenthetical disambiguates it back to the standard terms: value 2 is
//! "vertical mirroring" (`Mirroring::Vertical`, CIRAM A10 = PPU A10, the
//! same mechanism `ppu/mem.rs`'s `Mirroring::Vertical` arm implements:
//! nametables 0/2 share a bank, 1/3 share a bank), value 3 is "horizontal
//! mirroring" (`Mirroring::Horizontal`, CIRAM A10 = PPU A11: nametables
//! 0/1 share a bank, 2/3 share a bank) — cross-checked directly against
//! this crate's own `ppu/mem.rs::nametable_offset`, not taken on faith
//! from the wiki prose alone.
//!
//! ## CHR Bank 0/1 (`$A000-$BFFF`/`$C000-$DFFF`) and PRG Bank (`$E000-$FFFF`)
//!
//! CHR Bank 0/1: bits 4-0 select a 4 KiB (dual-bank mode) or 8 KiB
//! (single-bank mode, low bit of CHR Bank 0 ignored) window at PPU
//! `$0000`/`$1000`. PRG Bank: bits 3-0 select a 16 KiB window (bit 4
//! differs by MMC1 revision — PRG-RAM enable on MMC1B — not modeled here;
//! see this file's `chr_window`/PRG-RAM notes below).
//!
//! ## What this implementation deliberately does not model
//!
//! - **PRG-RAM enable/disable** (PRG Bank register bit 4, MMC1B) — this
//!   ticket's acceptance criteria list "PRG banking, CHR banking, and
//!   MMC1's mirroring control" only;
//!   [`crate::system::NesBus`]'s PRG RAM stays "always backed, 8 KiB" for
//!   every mapper, unchanged from ticket W1-02.
//! - **SOROM/SUROM 512 KiB PRG variants' extra CHR-bank-driven PRG-RAM/
//!   PRG-ROM banking bits** — `docs/design/EMULATION_CORES.md` §2.4's own
//!   launch-set table calls these out as "later"; this implementation
//!   handles any PRG ROM size that's a multiple of 16 KiB generically,
//!   but has no SOROM/SUROM-specific extra bank bit logic.
//! - **CHR-RAM bank switching** — see [`Mapper::chr_window`]'s doc and
//!   this crate's `mappers` module doc.
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK: usize = 16 * 1024;
const CHR_BANK_4K: usize = 4 * 1024;
const CHR_BANK_8K: usize = 8 * 1024;
/// The shift register's sentinel "empty" state: a single 1 bit in
/// position 4, the load register diagram's own convention (each write
/// shifts a new bit in from the top and the old bit 0 out; after 5
/// writes the sentinel has shifted all the way out, which is exactly the
/// "5th write" detection this file's `cpu_write` uses).
const RESET_SHIFT: u8 = 0b10000;
/// Conventional emulator default for the Control register's power-on
/// value (not nesdev-specified — nesdev documents no power-on register
/// state for MMC1). `0x0C` is exactly what a bit-7 reset write ORs in
/// (`CPPMM = 0_11_00`: PRG mode 3, CHR 8 KiB mode, mirroring bits left at
/// 0/one-screen-A), so a fresh `Mmc1` behaves as if it had just seen a
/// reset write.
const CONTROL_POWER_ON: u8 = 0x0C;

pub struct Mmc1 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,

    shift: u8,
    control: u8,
    chr_bank0: u8,
    chr_bank1: u8,
    prg_bank: u8,
    /// The bus cycle of the most recently processed write (accepted OR
    /// ignored — see `cpu_write`'s doc), for the consecutive-cycle-ignore
    /// quirk. `None` until the first write.
    last_write_cycle: Option<u64>,

    /// Materialized current 8 KiB CHR view, recomputed by
    /// `recompute_chr_view` after every committed register write that
    /// could affect it. Needed (rather than returning a plain sub-slice
    /// of `chr_rom` the way [`super::Cnrom::chr_window`] does) because
    /// 4 KiB dual-bank mode's two halves are, in general, non-adjacent
    /// regions of `chr_rom` — no single contiguous sub-slice can express
    /// that view, but [`Mapper::chr_window`]'s `&self -> Option<&[u8]>`
    /// signature needs something to borrow from.
    chr_view: [u8; CHR_BANK_8K],
}

impl Mmc1 {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(PRG_BANK),
            "MMC1 PRG ROM must be a nonzero multiple of 16 KiB"
        );
        debug_assert!(
            !chr_rom.is_empty() && chr_rom.len().is_multiple_of(CHR_BANK_4K),
            "MMC1 CHR data must be a nonzero multiple of 4 KiB"
        );
        let mut mapper = Mmc1 {
            prg_rom,
            chr_rom,
            chr_is_ram,
            shift: RESET_SHIFT,
            control: CONTROL_POWER_ON,
            chr_bank0: 0,
            chr_bank1: 0,
            prg_bank: 0,
            last_write_cycle: None,
            chr_view: [0u8; CHR_BANK_8K],
        };
        mapper.recompute_chr_view();
        mapper
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK).max(1)
    }

    fn prg_mode(&self) -> u8 {
        (self.control >> 2) & 0b11
    }

    fn chr_mode_is_4k(&self) -> bool {
        self.control & 0x10 != 0
    }

    fn recompute_chr_view(&mut self) {
        if self.chr_rom.is_empty() {
            return;
        }
        if self.chr_mode_is_4k() {
            let bank_count = (self.chr_rom.len() / CHR_BANK_4K).max(1);
            let b0 = (self.chr_bank0 as usize % 0x20) % bank_count;
            let b1 = (self.chr_bank1 as usize % 0x20) % bank_count;
            self.chr_view[0..CHR_BANK_4K]
                .copy_from_slice(&self.chr_rom[b0 * CHR_BANK_4K..(b0 + 1) * CHR_BANK_4K]);
            self.chr_view[CHR_BANK_4K..CHR_BANK_8K]
                .copy_from_slice(&self.chr_rom[b1 * CHR_BANK_4K..(b1 + 1) * CHR_BANK_4K]);
        } else {
            let bank_count = (self.chr_rom.len() / CHR_BANK_8K).max(1);
            // Low bit of CHR Bank 0 ignored in 8 KiB mode (nesdev).
            let b = ((self.chr_bank0 as usize % 0x20) >> 1) % bank_count;
            self.chr_view
                .copy_from_slice(&self.chr_rom[b * CHR_BANK_8K..(b + 1) * CHR_BANK_8K]);
        }
    }
}

impl Mapper for Mmc1 {
    fn cpu_read(&self, addr: u16) -> u8 {
        let bank_count = self.prg_bank_count();
        // Bits 3-0 of the PRG Bank register are the bank number; bit 4 is
        // MMC1B's PRG-RAM-enable, not modeled here (module doc) and
        // deliberately excluded from bank arithmetic.
        let bank16 = (self.prg_bank as usize) & 0x0F;
        let idx = match self.prg_mode() {
            0 | 1 => {
                let bank32_count = (bank_count / 2).max(1);
                let bank32 = (bank16 >> 1) % bank32_count;
                bank32 * (2 * PRG_BANK) + (addr as usize - 0x8000)
            }
            2 => match addr {
                0x8000..=0xBFFF => addr as usize - 0x8000, // fixed: first bank
                _ => (bank16 % bank_count) * PRG_BANK + (addr as usize - 0xC000),
            },
            _ => match addr {
                0xC000..=0xFFFF => (bank_count - 1) * PRG_BANK + (addr as usize - 0xC000), // fixed: last bank
                _ => (bank16 % bank_count) * PRG_BANK + (addr as usize - 0x8000),
            },
        };
        self.prg_rom[idx]
    }

    /// Serial-port write per this file's module doc: bit 7 set resets the
    /// shift register and forces PRG mode 3 (`Control |= 0x0C`); otherwise
    /// one data bit shifts in, and the 5th such write since the last
    /// reset/commit latches the accumulated 5 bits into the register
    /// address bits 14-13 select. Writes on the cycle immediately after
    /// another write are ignored entirely (nesdev's documented quirk) —
    /// except a bit-7 reset, which is never ignored.
    fn cpu_write(&mut self, addr: u16, value: u8, cycle: u64) {
        if value & 0x80 != 0 {
            self.shift = RESET_SHIFT;
            self.control |= 0x0C;
            self.last_write_cycle = Some(cycle);
            self.recompute_chr_view();
            return;
        }

        if self.last_write_cycle == Some(cycle.wrapping_sub(1)) {
            // Consecutive-cycle write: ignored (nesdev quirk), but still
            // recorded as "the last write" so a third write one cycle
            // later is judged against *this* write, keeping an arbitrary
            // run of consecutive writes all ignored after the first.
            self.last_write_cycle = Some(cycle);
            return;
        }
        self.last_write_cycle = Some(cycle);

        let commit = self.shift & 1 == 1;
        self.shift = (self.shift >> 1) | ((value & 1) << 4);
        if commit {
            let committed = self.shift & 0x1F;
            match (addr >> 13) & 0b11 {
                0 => self.control = committed,
                1 => self.chr_bank0 = committed,
                2 => self.chr_bank1 = committed,
                3 => self.prg_bank = committed,
                _ => unreachable!("`& 0b11` bounds this to 0..=3"),
            }
            self.shift = RESET_SHIFT;
            self.recompute_chr_view();
        }
    }

    fn mirroring(&self) -> Mirroring {
        match self.control & 0b11 {
            0 => Mirroring::OneScreenLower,
            1 => Mirroring::OneScreenUpper,
            2 => Mirroring::Vertical,
            3 => Mirroring::Horizontal,
            _ => unreachable!("`& 0b11` bounds this to 0..=3"),
        }
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.is_empty() {
            None
        } else {
            Some(&self.chr_view[..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `banks` 16 KiB PRG banks, bank `n`'s first byte is `0x40 + n`.
    fn prg(banks: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks as usize * PRG_BANK];
        for n in 0..banks {
            data[n as usize * PRG_BANK] = 0x40 + n;
        }
        data
    }

    /// `banks` 4 KiB CHR banks, bank `n`'s first byte is `0x80 + n`.
    fn chr4k(banks: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks as usize * CHR_BANK_4K];
        for n in 0..banks {
            data[n as usize * CHR_BANK_4K] = 0x80 + n;
        }
        data
    }

    /// Runs the real 5-write serial-port sequence (LSB first, as real
    /// hardware/software does) for `value`'s low 5 bits into `addr`,
    /// using distinct, non-consecutive cycle numbers (`base`, `base+2`,
    /// `base+4`, ... — real 6502 code never issues single-cycle-apart
    /// writes except via an RMW instruction's own two writes, which this
    /// helper deliberately avoids so it isn't accidentally exercising the
    /// ignore-quirk).
    fn write_register(m: &mut Mmc1, addr: u16, value: u8, base_cycle: u64) {
        for i in 0..5u64 {
            let bit = (value >> i) & 1;
            m.cpu_write(addr, bit, base_cycle + i * 2);
        }
    }

    #[test]
    fn five_writes_commit_the_correct_five_bit_value_lsb_first() {
        let mut m = Mmc1::new(prg(4), chr4k(2), false);
        // Select CHR Bank 0 = 0b01101 = 13 (arbitrary, chosen to exercise
        // every bit position at least once).
        write_register(&mut m, 0xA000, 0b01101, 100);
        assert_eq!(
            m.chr_bank0, 0b01101,
            "the committed 5-bit value must be exactly what was shifted in, bit-exact"
        );
    }

    #[test]
    fn prg_mode_3_switches_8000_and_fixes_c000_to_the_last_bank() {
        let mut m = Mmc1::new(prg(4), chr4k(2), false);
        // Power-on default is already PRG mode 3 (CONTROL_POWER_ON = 0x0C).
        assert_eq!(
            m.cpu_read(0xC000),
            0x40 + 3,
            "fixed to the last (bank 3) at construction"
        );
        write_register(&mut m, 0xE000, 1, 100); // PRG Bank = 1
        assert_eq!(
            m.cpu_read(0x8000),
            0x40 + 1,
            "the mapper's own PRG bank register must have selected bank 1 -- a no-op \
             mapper that ignores writes would still read bank 0's marker here"
        );
        assert_eq!(m.cpu_read(0xC000), 0x40 + 3, "still fixed to the last bank");
    }

    #[test]
    fn prg_mode_2_fixes_8000_to_the_first_bank_and_switches_c000() {
        let mut m = Mmc1::new(prg(4), chr4k(2), false);
        // Control = 0b0_10_00 = PRG mode 2, CHR 8K mode, mirroring 0.
        write_register(&mut m, 0x8000, 0b01000, 100);
        write_register(&mut m, 0xE000, 2, 200); // PRG Bank = 2
        assert_eq!(m.cpu_read(0x8000), 0x40, "fixed to the first (bank 0)");
        assert_eq!(
            m.cpu_read(0xC000),
            0x40 + 2,
            "$C000 switches to the selected bank in PRG mode 2"
        );
    }

    #[test]
    fn chr_4k_mode_selects_independent_banks_for_each_half() {
        let mut m = Mmc1::new(prg(2), chr4k(4), false);
        // Control = 0b1_11_00: CHR 4K mode, PRG mode 3, mirroring 0.
        write_register(&mut m, 0x8000, 0b11100, 100);
        write_register(&mut m, 0xA000, 1, 200); // CHR Bank 0 = 1
        write_register(&mut m, 0xC000, 3, 300); // CHR Bank 1 = 3
        let view = m.chr_window().expect("CHR ROM cartridge");
        assert_eq!(view[0], 0x80 + 1, "low 4 KiB half from CHR Bank 0");
        assert_eq!(
            view[CHR_BANK_4K],
            0x80 + 3,
            "high 4 KiB half from CHR Bank 1"
        );
    }

    #[test]
    fn chr_8k_mode_ignores_chr_bank_0s_low_bit() {
        // 6 x 4 KiB banks = 3 x 8 KiB banks, so an 8 KiB bank index of 1
        // (register 3 >> 1) is distinguishable from bank 0 -- with fewer
        // banks the register value would just wrap back to bank 0 and the
        // low-bit-ignored assertion below would be vacuously true.
        let mut m = Mmc1::new(prg(2), chr4k(6), false);
        // Control already defaults to CHR 8K mode (bit 4 clear).
        write_register(&mut m, 0xA000, 0b00011, 100); // CHR Bank 0 = 3 -> 3>>1 = 1
        let view = m.chr_window().unwrap();
        assert_eq!(
            view[0],
            0x80 + 2,
            "8 KiB mode selects bank (register >> 1) = 1, i.e. the 4 KiB pair starting at \
             4 KiB-bank index 2 -- a no-op mapper (always bank 0) would read 0x80 here instead"
        );
    }

    #[test]
    fn mirroring_control_bits_select_all_four_modes() {
        let mut m = Mmc1::new(prg(2), chr4k(2), false);
        write_register(&mut m, 0x8000, 0b00000, 100);
        assert_eq!(m.mirroring(), Mirroring::OneScreenLower);
        write_register(&mut m, 0x8000, 0b00001, 200);
        assert_eq!(m.mirroring(), Mirroring::OneScreenUpper);
        write_register(&mut m, 0x8000, 0b00010, 300);
        assert_eq!(m.mirroring(), Mirroring::Vertical);
        write_register(&mut m, 0x8000, 0b00011, 400);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
    }

    #[test]
    fn bit7_reset_locks_prg_mode_3_but_leaves_mirroring_bits_alone() {
        let mut m = Mmc1::new(prg(4), chr4k(2), false);
        write_register(&mut m, 0x8000, 0b00011, 100); // mirroring = Horizontal, PRG mode 0
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
        m.cpu_write(0x8000, 0xFF, 500); // bit 7 set: reset
        assert_eq!(
            m.mirroring(),
            Mirroring::Horizontal,
            "reset only ORs in $0C -- mirroring bits (0/1) are untouched"
        );
        assert_eq!(
            m.cpu_read(0xC000),
            0x40 + 3,
            "reset forces PRG mode 3 (fixed last bank)"
        );
    }

    #[test]
    fn reset_mid_sequence_discards_partial_shift_progress() {
        let mut m = Mmc1::new(prg(2), chr4k(2), false);
        let before = m.prg_bank;
        m.cpu_write(0xE000, 1, 100);
        m.cpu_write(0xE000, 0, 102);
        m.cpu_write(0xE000, 1, 104); // 3 of 5 writes into PRG Bank
        m.cpu_write(0x8000, 0xFF, 106); // reset before the 5th write
        m.cpu_write(0xE000, 1, 200);
        m.cpu_write(0xE000, 1, 202);
        m.cpu_write(0xE000, 1, 204);
        m.cpu_write(0xE000, 1, 206);
        m.cpu_write(0xE000, 1, 208); // a fresh, complete 5-write sequence: 0b11111
        assert_eq!(
            m.prg_bank, 0b11111,
            "the reset sequence's partial bits must not have leaked into this commit"
        );
        assert_ne!(m.prg_bank, before);
    }

    #[test]
    fn consecutive_cycle_write_after_the_first_is_ignored() {
        // Real RMW instructions (`ASL $E000`, etc.) write the unmodified
        // value on one cycle and the modified value on the very next --
        // this test reproduces exactly that shape: a bit-1 write at cycle
        // 100, immediately followed by a bit-0 write at cycle 101 (one
        // cycle later). nesdev's quirk says the second write must be
        // dropped entirely, so the accepted bit stream is 1,1,1,1,1 (five
        // *accepted* writes at cycles 100,103,105,107,109 -- all spaced
        // apart -- committing 0b11111). If the ignore-quirk were NOT
        // implemented, all 6 writes (1,0,1,1,1,1) would be accepted and
        // the register would commit early, on the 5th call (cycle 107),
        // to 0b11101 (29) instead -- a genuinely different value, not
        // just a different cycle count, so this assertion fails loudly
        // against a mapper that ignores the quirk.
        let mut m = Mmc1::new(prg(4), chr4k(2), false);
        m.cpu_write(0xE000, 1, 100); // 1st accepted data bit: 1
        m.cpu_write(0xE000, 0, 101); // consecutive with the write above: ignored
        m.cpu_write(0xE000, 1, 103); // 2nd accepted data bit: 1
        m.cpu_write(0xE000, 1, 105); // 3rd accepted data bit: 1
        m.cpu_write(0xE000, 1, 107); // 4th accepted data bit: 1
        m.cpu_write(0xE000, 1, 109); // 5th accepted data bit: 1 -- commits
        assert_eq!(
            m.prg_bank, 0b11111,
            "the write at a consecutive cycle must have been fully ignored, not merely \
             counted with the wrong bit value -- an implementation without the quirk \
             would have committed 0b11101 two writes earlier"
        );
    }

    #[test]
    fn chr_window_is_none_for_chr_ram() {
        let m = Mmc1::new(prg(2), vec![0u8; CHR_BANK_8K], true);
        assert!(m.chr_window().is_none());
    }
}
