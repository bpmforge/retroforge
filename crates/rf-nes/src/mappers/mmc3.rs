//! MMC3 / TxROM (mapper 4) —
//! [nesdev.org/wiki/MMC3](https://www.nesdev.org/wiki/MMC3). PRG banking
//! (two 8 KiB switchable + two 8 KiB fixed windows, swap side selectable),
//! CHR banking (two 2 KiB + four 1 KiB windows, with an A12-inversion bit
//! swapping which half gets which granularity), a $A000 mirroring register,
//! a $A001 PRG-RAM-protect register, and a PPU-A12-clocked scanline IRQ
//! counter — ticket W2-03.
//!
//! **A12 rising-edge detection and its low-period filter live in
//! [`crate::ppu::Ppu`], not here** (`Ppu::observe_ppu_bus_address`,
//! `ppu/mem.rs`) — see `super`'s module doc "MMC3 additions" section for
//! the full push/pull design and why. This file only implements what
//! happens once a filtered edge is *reported*: [`Mmc3::clock_irq_counter`].
//!
//! ## Bank Select ($8000-$9FFE, even) and Bank Data ($8001-$9FFF, odd)
//!
//! Quoted verbatim from nesdev:
//! ```text
//! $8000: CPMx xRRR
//!        |||   +++- Register to update on next $8001 write (0-7)
//!        ||+------- PRG ROM bank mode (0: $8000 swappable, $C000 fixed to
//!        ||           second-last; 1: $C000 swappable, $8000 fixed to
//!        ||           second-last)
//!        |+-------- (unused on MMC3; MMC6 WRAM enable)
//!        +--------- CHR A12 inversion (0: two 2K banks at $0000, four 1K
//!                     banks at $1000; 1: two 2K banks at $1000, four 1K
//!                     banks at $0000)
//! ```
//! "2KB banks may only select even numbered CHR banks (the lowest bit is
//! ignored)." "R6 and R7 will ignore the top two bits... R0 and R1 ignore
//! the bottom bit." Register-to-window mapping (0-1: CHR 2K banks, 2-5:
//! CHR 1K banks, 6: PRG $8000/$C000 depending on mode, 7: PRG $A000) and
//! the exact per-register-to-1KB/8KB-window layout below are cross-checked
//! against [Mesen2's `MMC3.h`](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/Mappers/Nintendo/MMC3.h)
//! (`UpdateChrMapping`/`UpdatePrgMapping`), which nesdev's prose alone
//! doesn't spell out at register-index granularity.
//!
//! ## Mirroring ($A000-$BFFE, even) — resolving a genuine nesdev
//! terminology trap (same one MMC1's own module doc already names)
//!
//! nesdev's literal text: `"xxxx xxxN | Nametable arrangement (0:
//! horizontal (A10); 1: vertical (A11))"`. Read naively this says bit0=0 ->
//! [`Mirroring::Horizontal`]. That is **wrong** for this crate's
//! `Mirroring` enum, and [`super::mmc1`]'s own module doc already
//! documents exactly this trap for MMC1's nametable-arrangement bits:
//! nesdev's mapper pages routinely name the physical *arrangement* of the
//! two nametables (which "looks like" the opposite of the standard
//! mirroring-type name), not the standard term this crate's
//! `Mirroring`/`ppu/mem.rs::nametable_offset` use. MMC1's page
//! disambiguates in its own parenthetical ("horizontal arrangement" =
//! "vertical mirroring", PPU A10; "vertical arrangement" = "horizontal
//! mirroring", PPU A11) — applying that SAME A10<->vertical / A11<->horizontal
//! correspondence to MMC3's page (which omits the disambiguating word but
//! keeps the same A10/A11 annotations) gives bit0=0 -> A10 -> **Vertical**,
//! bit0=1 -> A11 -> **Horizontal**. Cross-checked independently against
//! [Mesen2's `MMC3.h`](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/Mappers/Nintendo/MMC3.h)
//! `UpdateMirroring`: `(RegA000 & 1) == 1 ? Horizontal : Vertical` — the
//! same polarity, from an independently-tested implementation, not derived
//! from the same possibly-mistranscribed wiki prose. "This bit has no
//! effect on cartridges with hardwired 4-screen VRAM" (nesdev) — modeled by
//! [`Mmc3::mirroring`] short-circuiting to [`Mirroring::FourScreen`]
//! whenever the header declared it, exactly like
//! [Mesen2's `UpdateMirroring`](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/Mappers/Nintendo/MMC3.h)'s
//! own `if(GetMirroringType() != MirroringType::FourScreens)` guard.
//!
//! ## PRG-RAM protect ($A001-$BFFF, odd) — latched, not gated (honest gap)
//!
//! ```text
//! RWXX xxxx
//! ||
//! |+- Write protection (0: allow writes; 1: deny writes)
//! +-- PRG RAM chip enable (0: disable; 1: enable)
//! ```
//! [`Mmc3::prg_ram_protect`] stores this byte faithfully, but — see
//! `super`'s module doc "MMC3 additions" section — nothing in
//! [`crate::system::NesBus`]'s `$6000-$7FFF` handling consults it this
//! ticket: zero sub-ROM in either fetched oracle (`mmc3_test_2`,
//! `mmc3_irq_tests`) ever writes `$A001`, so there is no oracle to build
//! the gate against, and every sub-ROM's own `$6000` blargg-protocol
//! result depends on `$6000-$7FFF` staying writable regardless.
//!
//! ## IRQ registers ($C000/$C001/$E000/$E001) and the counter itself
//!
//! Quoted verbatim from nesdev:
//! - `$C000` (latch, even): "specifies the IRQ counter reload value. When
//!   the IRQ counter is zero (or a reload is requested through $C001), this
//!   value will be copied into the IRQ counter at the NEXT rising edge of
//!   the PPU address."
//! - `$C001` (reload, odd): "writing any value to this register clears the
//!   MMC3 IRQ counter immediately, and then reloads it at the NEXT rising
//!   edge of filtered A12."
//! - `$E000` (disable, even): "writing any value to this register will
//!   disable MMC3 interrupts AND acknowledge any pending interrupts."
//! - `$E001` (enable, odd): "writing any value to this register will
//!   enable MMC3 interrupts."
//! - Clock logic: "if [the counter is] zero or the reload flag is true,
//!   it's reloaded with the IRQ latched value at $C000; otherwise, it
//!   decrements. If the IRQ counter is zero and IRQs are enabled, an IRQ is
//!   triggered." "The exact number of scanlines between IRQs is N+1, where
//!   N is the value written to $C000."
//!
//! The readme bundled with this ticket's fetched `mmc3_test_2`/
//! `mmc3_irq_tests` oracles (Shay Green, christopherpow/nes-test-roms,
//! pinned commit — see `tests/rom-manifest.toml`) adds two facts nesdev's
//! page doesn't state as plainly, both verified directly against that
//! suite's own asm source (`5-MMC3.s`/`6-MMC3_alt.s`,
//! `5.MMC3_rev_A.nes`/`6.MMC3_rev_B.nes` naming) — the test ROM is the tie-
//! breaker per MASTER_PROMPT where these two facts are exactly what
//! distinguishes the two real chip families:
//! - **Revision B** (SMB3, Mega Man 3 — [`Mmc3Revision::B`]): "the IRQ flag
//!   is set" every time the counter's value *after* a clock (whether by
//!   reload or decrement) is zero and IRQs are enabled — a reload that
//!   lands on zero fires just as much as a decrement that does.
//! - **Revision A** (Crystalis — [`Mmc3Revision::A`]): only fires when the
//!   counter *naturally decrements* to zero, OR when a reload to zero was
//!   itself explicitly requested via `$C001` since the last clock — a
//!   reload to zero caused merely by "the counter already read zero when
//!   clocked" does NOT fire. [Mesen2's `MMC3.h`](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/Mappers/Nintendo/MMC3.h)
//!   expresses this as `(count_before_this_clock > 0 || reload_was_flagged)
//!   && counter_after == 0`, which [`Mmc3::clock_irq_counter`] implements
//!   verbatim.
//! - The readme explicitly documents a THIRD, more obscure "double-$C001"
//!   pathological quirk and explicitly recommends against implementing it
//!   ("I put a check in my emulator and none of the several games I tested
//!   ever caused this situation to occur, so it's probably not a good idea
//!   to implement this") — deliberately not built here either, per that
//!   same recommendation.
//!
//! ## Which revision does a real cartridge get?
//!
//! Real hardware disambiguates A/B by which physical chip is on the board,
//! which the iNES/NES 2.0 header cannot express (verified: all six
//! `mmc3_test_2` sub-ROMs, including `5-MMC3.nes`/`6-MMC3_alt.nes` which
//! require OPPOSITE revision behavior to pass, share byte-identical
//! headers — `xxd -l16` on all six shows `02 01 41 00` for
//! prg/chr/flags6/flags7). [`crate::system::NesBus::new`]'s mapper-4 arm
//! therefore always builds [`Mmc3Revision::B`] (the common case — SMB3,
//! Mega Man 3, Kirby's Adventure), the same "no game database, pick the
//! common case, document the gap" resolution
//! [Mesen2 itself uses](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/Mappers/Nintendo/MMC3.h)
//! (`_forceMmc3RevAIrqs` driven by a game database this crate has no
//! equivalent of).
//! [`crate::system::NesBus::new_forcing_mmc3_revision_a`] exists solely so
//! the two "which revision" oracle ROMs (`mmc3_test_2`'s `6-MMC3_alt.nes`
//! and `mmc3_irq_tests`' `5.MMC3_rev_A.nes` — headers indistinguishable
//! from their revision-B siblings) can be run against the correct
//! revision; it is not, and must never become, a general-purpose public
//! API.
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK: usize = 8 * 1024;
const CHR_BANK_1K: usize = 1024;
const CHR_VIEW_SIZE: usize = 8 * 1024;

/// Which of the two documented MMC3 chip families' IRQ-fire rule this
/// instance implements — see this module's doc "IRQ registers" section.
/// The counter/reload arithmetic itself (what value the counter holds
/// after a clock) is identical between revisions; only whether a
/// reload-that-lands-on-zero fires an IRQ differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmc3Revision {
    /// SMB3/Mega Man 3-style: fires whenever the counter is zero
    /// immediately after any clock (reload or decrement), if enabled.
    B,
    /// Crystalis-style: fires only on a natural decrement-to-zero, or a
    /// reload-to-zero that was itself explicitly requested via `$C001`
    /// since the last clock.
    A,
}

pub struct Mmc3 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    four_screen: bool,
    revision: Mmc3Revision,

    bank_select: u8,
    /// R0-R7 (nesdev's own register numbering): R0/R1 CHR 2K banks
    /// (bottom bit ignored, enforced at write time), R2-R5 CHR 1K banks,
    /// R6/R7 PRG 8K banks (top two bits ignored, enforced at write time).
    registers: [u8; 8],
    mirroring_reg: u8,
    prg_ram_protect: u8,

    irq_latch: u8,
    irq_counter: u8,
    irq_reload_flag: bool,
    irq_enabled: bool,
    irq_pending: bool,

    /// Materialized current 8 KiB CHR view — same "can't express a
    /// non-contiguous view as a sub-slice" reasoning as
    /// [`super::Mmc1::chr_view`], amplified here (up to 6 independently
    /// selected 1-2 KiB pieces rather than 2).
    chr_view: [u8; CHR_VIEW_SIZE],
}

impl Mmc3 {
    pub fn new(
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        chr_is_ram: bool,
        mirroring: Mirroring,
        revision: Mmc3Revision,
    ) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(PRG_BANK),
            "MMC3 PRG ROM must be a nonzero multiple of 8 KiB"
        );
        let mut mapper = Mmc3 {
            prg_rom,
            chr_rom,
            chr_is_ram,
            four_screen: mirroring == Mirroring::FourScreen,
            revision,
            bank_select: 0,
            registers: [0; 8],
            mirroring_reg: 0,
            prg_ram_protect: 0,
            irq_latch: 0,
            irq_counter: 0,
            irq_reload_flag: false,
            irq_enabled: false,
            irq_pending: false,
            chr_view: [0u8; CHR_VIEW_SIZE],
        };
        mapper.recompute_chr_view();
        mapper
    }

    /// Raw `$A001` byte as last written (bit7 chip-enable, bit6
    /// write-protect) — see this module's doc "PRG-RAM protect" section
    /// for why nothing gates real `$6000-$7FFF` behavior on it yet.
    pub fn prg_ram_protect(&self) -> u8 {
        self.prg_ram_protect
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK).max(1)
    }

    fn chr_bank_count_1k(&self) -> usize {
        (self.chr_rom.len() / CHR_BANK_1K).max(1)
    }

    fn prg_mode(&self) -> bool {
        self.bank_select & 0x40 != 0
    }

    fn chr_a12_inverted(&self) -> bool {
        self.bank_select & 0x80 != 0
    }

    /// PRG bank `index` (already resolved to nesdev's `-1`/`-2`-relative
    /// convention as a real 0-based bank number by the caller), masked to
    /// this cartridge's real bank count.
    fn prg_bank(&self, index: usize) -> usize {
        index % self.prg_bank_count()
    }

    fn recompute_prg_reads(&self) -> [usize; 4] {
        let count = self.prg_bank_count();
        let last = count - 1;
        let second_last = count.saturating_sub(2) % count;
        let r6 = self.prg_bank((self.registers[6] & 0x3F) as usize);
        let r7 = self.prg_bank((self.registers[7] & 0x3F) as usize);
        if self.prg_mode() {
            // Bit6 set: $C000 swappable (R6), $8000 fixed to second-last.
            [second_last, r7, r6, last]
        } else {
            // Bit6 clear: $8000 swappable (R6), $C000 fixed to second-last.
            [r6, r7, second_last, last]
        }
    }

    fn recompute_chr_view(&mut self) {
        if self.chr_rom.is_empty() {
            return;
        }
        let count = self.chr_bank_count_1k();
        // Same six raw register values regardless of A12-inversion; only
        // which 1 KiB *window* each lands in differs (nesdev: "two 2 KB
        // banks... four 1 KB banks", with the two halves swapping which
        // granularity they get when bit7 is set).
        let r0_even = (self.registers[0] & 0xFE) as usize % count;
        let r0_odd = (self.registers[0] | 0x01) as usize % count;
        let r1_even = (self.registers[1] & 0xFE) as usize % count;
        let r1_odd = (self.registers[1] | 0x01) as usize % count;
        let banks_2k = [r0_even, r0_odd, r1_even, r1_odd];
        let banks_1k = [
            self.registers[2] as usize % count,
            self.registers[3] as usize % count,
            self.registers[4] as usize % count,
            self.registers[5] as usize % count,
        ];
        let windows: [usize; 8] = if self.chr_a12_inverted() {
            [
                banks_1k[0],
                banks_1k[1],
                banks_1k[2],
                banks_1k[3],
                banks_2k[0],
                banks_2k[1],
                banks_2k[2],
                banks_2k[3],
            ]
        } else {
            [
                banks_2k[0],
                banks_2k[1],
                banks_2k[2],
                banks_2k[3],
                banks_1k[0],
                banks_1k[1],
                banks_1k[2],
                banks_1k[3],
            ]
        };
        for (slot, &bank) in windows.iter().enumerate() {
            let src = bank * CHR_BANK_1K;
            let dst = slot * CHR_BANK_1K;
            self.chr_view[dst..dst + CHR_BANK_1K]
                .copy_from_slice(&self.chr_rom[src..src + CHR_BANK_1K]);
        }
    }
}

impl Mapper for Mmc3 {
    fn cpu_read(&self, addr: u16) -> u8 {
        debug_assert!(!self.prg_rom.is_empty(), "MMC3 image with empty PRG ROM");
        let window = ((addr - 0x8000) / PRG_BANK as u16) as usize;
        let offset = (addr as usize - 0x8000) % PRG_BANK;
        let bank = self.recompute_prg_reads()[window];
        self.prg_rom[bank * PRG_BANK + offset]
    }

    /// Dispatches by `addr & 0xE001` (even/odd within each of the four
    /// $2000-wide register pairs), nesdev's own addressing convention —
    /// every mirror of each register (e.g. `$8000` and `$8002`) behaves
    /// identically. `cycle` is unused: unlike MMC1, MMC3 has no
    /// consecutive-write-ignore quirk.
    fn cpu_write(&mut self, addr: u16, value: u8, _cycle: u64) {
        match addr & 0xE001 {
            0x8000 => self.bank_select = value,
            0x8001 => {
                let reg = (self.bank_select & 0x07) as usize;
                self.registers[reg] = value;
                self.recompute_chr_view();
            }
            0xA000 => self.mirroring_reg = value,
            0xA001 => self.prg_ram_protect = value,
            0xC000 => self.irq_latch = value,
            0xC001 => {
                self.irq_counter = 0;
                self.irq_reload_flag = true;
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
        // See this module's doc "Mirroring" section for the A10/A11
        // polarity resolution.
        if self.four_screen {
            Mirroring::FourScreen
        } else if self.mirroring_reg & 0x01 != 0 {
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

    /// One filtered PPU-A12 rising edge (from [`crate::ppu::Ppu`], via
    /// [`crate::system::NesBus`] — see `super`'s module doc). Implements
    /// nesdev's clock rule plus the revision-A/B fire-condition split —
    /// see this module's doc "IRQ registers" section for both, cited.
    fn clock_irq_counter(&mut self) {
        let counter_before = self.irq_counter;
        let reload_was_requested = self.irq_reload_flag;
        if self.irq_counter == 0 || self.irq_reload_flag {
            self.irq_counter = self.irq_latch;
        } else {
            self.irq_counter -= 1;
        }
        self.irq_reload_flag = false;

        let fires = match self.revision {
            Mmc3Revision::B => self.irq_counter == 0,
            Mmc3Revision::A => {
                (counter_before > 0 || reload_was_requested) && self.irq_counter == 0
            }
        };
        if fires && self.irq_enabled {
            self.irq_pending = true;
        }
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `prg_banks` 8 KiB PRG banks, bank `n`'s first byte is `0x40 + n`.
    fn prg(prg_banks: u8) -> Vec<u8> {
        let mut data = vec![0u8; prg_banks as usize * PRG_BANK];
        for n in 0..prg_banks {
            data[n as usize * PRG_BANK] = 0x40 + n;
        }
        data
    }

    /// `chr_banks_1k` 1 KiB CHR banks, bank `n`'s first byte is `0x80 + n`.
    fn chr(chr_banks_1k: u16) -> Vec<u8> {
        let mut data = vec![0u8; chr_banks_1k as usize * CHR_BANK_1K];
        for n in 0..chr_banks_1k {
            data[n as usize * CHR_BANK_1K] = (0x80 + n) as u8;
        }
        data
    }

    fn mmc3(prg_banks: u8, chr_banks_1k: u16) -> Mmc3 {
        Mmc3::new(
            prg(prg_banks),
            chr(chr_banks_1k),
            false,
            Mirroring::Vertical,
            Mmc3Revision::B,
        )
    }

    fn select(m: &mut Mmc3, reg: u8, value: u8) {
        m.cpu_write(0x8000, reg, 0);
        m.cpu_write(0x8001, value, 0);
    }

    /// Like [`select`], but ORs `mode_bits` (bit6 PRG mode and/or bit7 CHR
    /// A12 inversion) into every `$8000` write — `$8000` is one shared
    /// register, so real software (and every call site here that needs a
    /// non-default mode) must repeat the mode bits on every register
    /// select, not just the first.
    fn select_with_mode(m: &mut Mmc3, mode_bits: u8, reg: u8, value: u8) {
        m.cpu_write(0x8000, mode_bits | reg, 0);
        m.cpu_write(0x8001, value, 0);
    }

    // ---- PRG banking ----

    #[test]
    fn prg_mode_0_switches_8000_via_r6_and_fixes_c000_to_second_last() {
        let mut m = mmc3(8, 8); // 8 x 8KiB PRG banks: 0..=7
        select(&mut m, 6, 3); // R6 = 3
        assert_eq!(m.cpu_read(0x8000), 0x40 + 3, "R6 selects $8000");
        assert_eq!(
            m.cpu_read(0xC000),
            0x40 + 6,
            "fixed to second-last (bank 6 of 0..=7)"
        );
        assert_eq!(m.cpu_read(0xE000), 0x40 + 7, "$E000 always the last bank");
    }

    #[test]
    fn prg_mode_1_switches_c000_via_r6_and_fixes_8000_to_second_last() {
        let mut m = mmc3(8, 8);
        select_with_mode(&mut m, 0x40, 6, 2); // bit6 set (PRG mode 1), R6 = 2
        assert_eq!(
            m.cpu_read(0x8000),
            0x40 + 6,
            "fixed to second-last in mode 1"
        );
        assert_eq!(m.cpu_read(0xC000), 0x40 + 2, "R6 now selects $C000");
    }

    #[test]
    fn r7_always_selects_a000_regardless_of_prg_mode() {
        let mut m = mmc3(8, 8);
        select(&mut m, 7, 5);
        assert_eq!(m.cpu_read(0xA000), 0x40 + 5);
        select_with_mode(&mut m, 0x40, 6, 3); // flip PRG mode, R6 = 3 (proves the mode bit really took)
        assert_eq!(m.cpu_read(0xA000), 0x40 + 5, "unaffected by PRG mode");
        assert_eq!(
            m.cpu_read(0xC000),
            0x40 + 3,
            "mode bit did take effect (R6 now drives $C000)"
        );
    }

    #[test]
    fn r6_and_r7_ignore_the_top_two_bits() {
        let mut m = mmc3(4, 8); // only banks 0..=3 exist
        select(&mut m, 6, 0xC2); // 0xC2 & 0x3F = 2
        assert_eq!(m.cpu_read(0x8000), 0x40 + 2);
    }

    // ---- CHR banking ----

    #[test]
    fn chr_mode_0_lays_out_two_2k_banks_at_0000_then_four_1k_at_1000() {
        let mut m = mmc3(2, 16); // 16 x 1KiB CHR banks: 0..=15
        select(&mut m, 0, 4); // R0 = 4 -> even/odd pair (4,5)
        select(&mut m, 1, 6); // R1 = 6 -> pair (6,7)
        select(&mut m, 2, 8);
        select(&mut m, 3, 9);
        select(&mut m, 4, 10);
        select(&mut m, 5, 11);
        let view = m.chr_window().unwrap();
        assert_eq!(view[0], 0x80 + 4, "$0000: R0 & 0xFE");
        assert_eq!(view[CHR_BANK_1K], 0x80 + 5, "$0400: R0 | 1");
        assert_eq!(view[2 * CHR_BANK_1K], 0x80 + 6, "$0800: R1 & 0xFE");
        assert_eq!(view[3 * CHR_BANK_1K], 0x80 + 7, "$0C00: R1 | 1");
        assert_eq!(view[4 * CHR_BANK_1K], 0x80 + 8, "$1000: R2");
        assert_eq!(view[5 * CHR_BANK_1K], 0x80 + 9, "$1400: R3");
        assert_eq!(view[6 * CHR_BANK_1K], 0x80 + 10, "$1800: R4");
        assert_eq!(view[7 * CHR_BANK_1K], 0x80 + 11, "$1C00: R5");
    }

    #[test]
    fn chr_a12_inversion_swaps_which_half_gets_2k_vs_1k_granularity() {
        let mut m = mmc3(2, 16);
        // bit7 set on EVERY $8000 write (it's one shared register -- a
        // later write without bit7 would silently clear the inversion).
        select_with_mode(&mut m, 0x80, 0, 4);
        select_with_mode(&mut m, 0x80, 1, 6);
        select_with_mode(&mut m, 0x80, 2, 8);
        select_with_mode(&mut m, 0x80, 3, 9);
        select_with_mode(&mut m, 0x80, 4, 10);
        select_with_mode(&mut m, 0x80, 5, 11);
        let view = m.chr_window().unwrap();
        assert_eq!(
            view[0],
            0x80 + 8,
            "$0000 now gets R2 (1 KiB banks moved here)"
        );
        assert_eq!(
            view[4 * CHR_BANK_1K],
            0x80 + 4,
            "$1000 now gets R0 & 0xFE (2 KiB banks moved here)"
        );
    }

    #[test]
    fn r0_and_r1_ignore_the_bottom_bit() {
        let mut m = mmc3(2, 16);
        select(&mut m, 0, 5); // odd value: low bit must be ignored -> pair (4,5)
        let view = m.chr_window().unwrap();
        assert_eq!(view[0], 0x80 + 4, "low bit forced off for the even half");
        assert_eq!(
            view[CHR_BANK_1K],
            0x80 + 5,
            "and forced on for the odd half"
        );
    }

    #[test]
    fn chr_window_is_none_for_chr_ram() {
        let m = Mmc3::new(
            prg(2),
            vec![0u8; CHR_VIEW_SIZE],
            true,
            Mirroring::Vertical,
            Mmc3Revision::B,
        );
        assert!(m.chr_window().is_none());
    }

    // ---- Mirroring ----

    #[test]
    fn a000_bit0_zero_is_vertical_one_is_horizontal() {
        let mut m = mmc3(2, 8);
        m.cpu_write(0xA000, 0, 0);
        assert_eq!(m.mirroring(), Mirroring::Vertical);
        m.cpu_write(0xA000, 1, 0);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
    }

    #[test]
    fn four_screen_header_overrides_a000_permanently() {
        let mut m = Mmc3::new(
            prg(2),
            chr(8),
            false,
            Mirroring::FourScreen,
            Mmc3Revision::B,
        );
        m.cpu_write(0xA000, 1, 0); // would select Horizontal if not four-screen
        assert_eq!(m.mirroring(), Mirroring::FourScreen);
    }

    // ---- PRG-RAM protect (storage only -- see module doc) ----

    #[test]
    fn a001_is_latched_and_readable_back() {
        let mut m = mmc3(2, 8);
        assert_eq!(m.prg_ram_protect(), 0, "power-on default");
        m.cpu_write(0xA001, 0xC0, 0);
        assert_eq!(m.prg_ram_protect(), 0xC0);
    }

    // ---- IRQ counter/reload/fire logic (revision B, the default) ----

    #[test]
    fn first_clock_after_a_c001_write_reloads_rather_than_decrements() {
        let mut m = mmc3(2, 8);
        m.cpu_write(0xC000, 5, 0); // latch = 5
        m.cpu_write(0xC001, 0, 0); // clear + request reload
        m.cpu_write(0xE001, 0, 0); // enable
        m.clock_irq_counter();
        assert_eq!(
            m.irq_counter, 5,
            "reloaded, not decremented from some stale value"
        );
        assert!(!m.irq_pending(), "5 != 0, no fire yet");
        m.clock_irq_counter();
        assert_eq!(m.irq_counter, 4, "second clock decrements normally");
    }

    #[test]
    fn irq_fires_when_decremented_to_zero_and_enabled() {
        let mut m = mmc3(2, 8);
        m.cpu_write(0xC000, 1, 0);
        m.cpu_write(0xC001, 0, 0);
        m.cpu_write(0xE001, 0, 0);
        m.clock_irq_counter(); // reload to 1
        assert!(!m.irq_pending());
        m.clock_irq_counter(); // decrement to 0
        assert!(m.irq_pending(), "counter hit zero while enabled");
    }

    #[test]
    fn irq_never_fires_while_disabled_even_as_counter_reaches_zero() {
        let mut m = mmc3(2, 8);
        m.cpu_write(0xC000, 1, 0);
        m.cpu_write(0xC001, 0, 0);
        // Deliberately never write $E001.
        for _ in 0..10 {
            m.clock_irq_counter();
        }
        assert!(!m.irq_pending());
    }

    #[test]
    fn e000_disables_and_acknowledges_pending() {
        let mut m = mmc3(2, 8);
        m.cpu_write(0xC000, 0, 0);
        m.cpu_write(0xC001, 0, 0);
        m.cpu_write(0xE001, 0, 0);
        m.clock_irq_counter(); // reload with 0, fires immediately (revision B)
        assert!(m.irq_pending());
        m.cpu_write(0xE000, 0, 0);
        assert!(!m.irq_pending(), "$E000 acknowledges");
        assert!(!m.irq_enabled, "and disables");
    }

    #[test]
    fn counter_keeps_running_while_irq_is_disabled() {
        let mut m = mmc3(2, 8);
        m.cpu_write(0xC000, 2, 0);
        m.cpu_write(0xC001, 0, 0);
        m.clock_irq_counter(); // reload to 2 (disabled, no fire)
        m.clock_irq_counter(); // decrement to 1
        m.clock_irq_counter(); // decrement to 0
        assert_eq!(m.irq_counter, 0);
        assert!(!m.irq_pending(), "never enabled, so never latched");
        m.cpu_write(0xE001, 0, 0);
        m.clock_irq_counter(); // reload to 2 again (now enabled, but 2 != 0)
        assert!(!m.irq_pending());
    }

    #[test]
    fn revision_b_fires_on_every_clock_when_latch_is_zero() {
        let mut m = mmc3(2, 8);
        m.cpu_write(0xC000, 0, 0);
        m.cpu_write(0xC001, 0, 0);
        m.cpu_write(0xE001, 0, 0);
        for _ in 0..3 {
            m.clock_irq_counter();
            assert!(m.irq_pending(), "revision B fires every clock when latch=0");
            m.cpu_write(0xE000, 0, 0); // ack for the next iteration
            m.cpu_write(0xE001, 0, 0);
        }
    }

    #[test]
    fn revision_a_does_not_fire_when_reload_to_zero_was_not_explicitly_requested() {
        let mut m = Mmc3::new(prg(2), chr(8), false, Mirroring::Vertical, Mmc3Revision::A);
        m.cpu_write(0xC000, 2, 0);
        m.cpu_write(0xC001, 0, 0);
        m.cpu_write(0xE001, 0, 0);
        m.clock_irq_counter(); // reload to 2
        m.clock_irq_counter(); // decrement to 1
        m.clock_irq_counter(); // decrement to 0 -- natural decrement, must fire
        assert!(m.irq_pending());
        m.cpu_write(0xE000, 0, 0);
        m.cpu_write(0xE001, 0, 0);
        m.cpu_write(0xC000, 0, 0); // latch = 0, but no $C001 write follows
        m.clock_irq_counter(); // counter was 0 -> reloads to 0, NOT explicitly requested
        assert!(
            !m.irq_pending(),
            "revision A: reload-to-zero from counter-already-zero must not fire"
        );
    }

    #[test]
    fn revision_a_fires_when_reload_to_zero_was_explicitly_requested_via_c001() {
        let mut m = Mmc3::new(prg(2), chr(8), false, Mirroring::Vertical, Mmc3Revision::A);
        m.cpu_write(0xC000, 0, 0);
        m.cpu_write(0xC001, 0, 0); // explicit reload request, counter was already 0
        m.cpu_write(0xE001, 0, 0);
        m.clock_irq_counter();
        assert!(
            m.irq_pending(),
            "revision A: an explicit $C001-requested reload to zero DOES fire"
        );
    }
}
