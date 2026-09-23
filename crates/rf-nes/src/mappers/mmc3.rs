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
use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK: usize = 8 * 1024;
const CHR_BANK_1K: usize = 1024;
const CHR_VIEW_SIZE: usize = 8 * 1024;
/// TQROM's on-board CHR-RAM chip (ticket W14-58; nesdev.org/wiki/
/// INES_Mapper_119): a fixed 8 KiB, addressed in 1 KiB pages by a bank
/// register's bits 0-2 whenever that register's bit 6 is set.
const CHR_RAM_SIZE: usize = 8 * 1024;
const CHR_RAM_PAGES: usize = 8;

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
    /// TxSROM (mapper 118, ticket W14-12): CIRAM A10 is wired to CHR A17,
    /// bit 7 of the CHR bank registers, so mirroring comes from whichever
    /// register maps the nametable's PPU A10-A12 and `$A000` is ignored.
    /// nesdev, "INES Mapper 118": with `$8000` bit 7 clear, R0 selects
    /// the page for `$2000-$27FF` and R1 for `$2800-$2FFF`; with it set,
    /// R2-R5 select `$2000`, `$2400`, `$2800`, `$2C00` one each.
    txsrom: bool,

    /// TQROM (mapper 119, ticket W14-58): the six CHR bank registers mix
    /// CHR-ROM and 8 KiB on-board CHR-RAM pages, bit 6 of the raw register
    /// byte selecting which. See [`Mmc3::new_tqrom`] and this module's
    /// "TQROM" doc section.
    tqrom: bool,
    /// 8 KiB CHR-RAM chip TQROM boards carry alongside CHR-ROM.
    /// Zero-sized (never read) for every other variant.
    chr_ram: [u8; CHR_RAM_SIZE],
    /// Bit `n` set: window slot `n` (the same 8 one-KiB slots
    /// [`Mmc3::chr_view`] lays out) is currently backed by `chr_ram`
    /// rather than `chr_rom` — [`Mapper::chr_ram_page_mask`]'s answer,
    /// recomputed by [`Mmc3::recompute_chr_view_tqrom`].
    chr_ram_mask: u8,
    /// For each window slot with its `chr_ram_mask` bit set: which of
    /// `chr_ram`'s 8 pages backs it — needed by
    /// [`Mmc3::chr_window_writeback`] to know where a PPU-side write to
    /// that slot belongs once the window moves on.
    chr_ram_slot_page: [u8; 8],

    /// Mapper 47 (ticket W14-58; nesdev.org/wiki/INES_Mapper_047): MMC3
    /// wired behind a `$6000-$7FFF`-selected 128 KiB PRG / 128 KiB CHR
    /// outer bank, bit 0 of any write there. See [`Mmc3::new_mapper47`].
    mapper47: bool,
    /// Mapper 47's outer bank select (0 or 1) — meaningless (stays 0) for
    /// every other variant.
    outer_bank: u8,

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
            txsrom: false,
            tqrom: false,
            chr_ram: [0u8; CHR_RAM_SIZE],
            chr_ram_mask: 0,
            chr_ram_slot_page: [0u8; 8],
            mapper47: false,
            outer_bank: 0,
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

    /// A TxSROM board (mapper 118): MMC3 with nametable selection wired
    /// to CHR bank bit 7 — see the `txsrom` field.
    pub fn new_txsrom(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool) -> Self {
        let mut mapper = Self::new(
            prg_rom,
            chr_rom,
            chr_is_ram,
            Mirroring::Vertical,
            Mmc3Revision::B,
        );
        mapper.txsrom = true;
        mapper
    }

    /// A TQROM board (mapper 119, ticket W14-58) —
    /// [nesdev.org/wiki/INES_Mapper_119](https://www.nesdev.org/wiki/INES_Mapper_119):
    /// plain MMC3 PRG banking, IRQ and mirroring, but each of the six CHR
    /// bank registers independently selects either a CHR-ROM page (bit 6
    /// clear, bits 0-5, up to 64 KiB) or one of the 8 pages of an on-board
    /// 8 KiB CHR-RAM chip (bit 6 set, bits 0-2) — see this module's
    /// "TQROM" doc section for how the resulting mixed window round-trips
    /// PPU-side writes through [`Mapper::chr_window_writeback`].
    pub fn new_tqrom(prg_rom: Vec<u8>, chr_rom: Vec<u8>) -> Self {
        debug_assert!(
            !chr_rom.is_empty() && chr_rom.len().is_multiple_of(CHR_BANK_1K),
            "TQROM CHR ROM must be a nonzero multiple of 1 KiB"
        );
        debug_assert!(
            chr_rom.len() <= 64 * 1024,
            "TQROM CHR-ROM bank field is 6 bits -- 64 KiB max"
        );
        let mut mapper = Self::new(
            prg_rom,
            chr_rom,
            false, // chr_is_ram: false -- CHR-ROM exists; RAM is the separate on-board chip
            Mirroring::Vertical,
            Mmc3Revision::B,
        );
        mapper.tqrom = true;
        mapper.recompute_chr_view_tqrom();
        mapper
    }

    /// MMC3 2-in-1 (mapper 47, ticket W14-58) —
    /// [nesdev.org/wiki/INES_Mapper_047](https://www.nesdev.org/wiki/INES_Mapper_047):
    /// two full 128 KiB PRG / 128 KiB CHR MMC3 cartridges on one board, a
    /// `$6000-$7FFF` write's bit 0 selecting which half the ordinary MMC3
    /// bank registers address into. No PRG-RAM (that range is the outer
    /// bank latch instead, per [`Mmc3::cpu_write_wram`]).
    ///
    /// The real board is a fixed 256 KiB PRG / 128 KiB CHR image, but
    /// [`Mmc3::prg_bank`]/[`Mmc3::chr_bank_index`] only ever divide
    /// whatever was actually passed in half — no assertion on the exact
    /// size here, so `system::tests::rom_loading`'s
    /// `every_emulated_mapper_loads_through_both_gates` (a generic 32
    /// KiB PRG / 8 KiB CHR fixture run against every entry in
    /// `EMULATED_MAPPERS`) still builds a bus rather than panicking.
    pub fn new_mapper47(prg_rom: Vec<u8>, chr_rom: Vec<u8>) -> Self {
        let mut mapper = Self::new(
            prg_rom,
            chr_rom,
            false,
            Mirroring::Vertical,
            Mmc3Revision::B,
        );
        mapper.mapper47 = true;
        mapper
    }

    /// TxSROM's mirroring, one page per nametable from CHR bank bit 7.
    fn txsrom_mirroring(&self) -> Mirroring {
        let page = |r: usize| self.registers[r] >> 7;
        if self.chr_a12_inverted() {
            Mirroring::PerTable([page(2), page(3), page(4), page(5)])
        } else {
            Mirroring::PerTable([page(0), page(0), page(1), page(1)])
        }
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
    ///
    /// Mapper 47 (ticket W14-58): `index` addresses within the CURRENT
    /// half only (nesdev.org/wiki/INES_Mapper_047: "the MMC3 registers
    /// otherwise behave normally" within whichever 128 KiB half `$6000`
    /// bit 0 selected), so it is confined to `prg_bank_count() / 2` banks
    /// and offset by `outer_bank` halves before returning an absolute
    /// index into `prg_rom`.
    fn prg_bank(&self, index: usize) -> usize {
        if self.mapper47 {
            let half = (self.prg_bank_count() / 2).max(1);
            self.outer_bank as usize * half + index % half
        } else {
            index % self.prg_bank_count()
        }
    }

    fn recompute_prg_reads(&self) -> [usize; 4] {
        // Mapper 47: "last"/"second-last" are relative to the selected
        // half too, not the whole 256 KiB image -- same reasoning as
        // `prg_bank`'s doc.
        let (half, offset) = if self.mapper47 {
            let half = (self.prg_bank_count() / 2).max(1);
            (half, self.outer_bank as usize * half)
        } else {
            (self.prg_bank_count(), 0)
        };
        let last = offset + half - 1;
        let second_last = offset + half.saturating_sub(2) % half;
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

    /// CHR 1 KiB bank `raw` (register bits, already masked to the
    /// register's own granularity by the caller), resolved to an absolute
    /// index into `chr_rom`.
    ///
    /// Mapper 47 (ticket W14-58): confined to the current half, the same
    /// "outer bank picks which 64 KiB CHR half the inner registers
    /// address" rule [`Mmc3::prg_bank`]'s doc describes for PRG.
    fn chr_bank_index(&self, raw: usize) -> usize {
        let count = self.chr_bank_count_1k();
        if self.mapper47 {
            let half = (count / 2).max(1);
            self.outer_bank as usize * half + raw % half
        } else {
            raw % count
        }
    }

    fn recompute_chr_view(&mut self) {
        if self.chr_rom.is_empty() {
            return;
        }
        // Same six raw register values regardless of A12-inversion; only
        // which 1 KiB *window* each lands in differs (nesdev: "two 2 KB
        // banks... four 1 KB banks", with the two halves swapping which
        // granularity they get when bit7 is set).
        let r0_even = self.chr_bank_index((self.registers[0] & 0xFE) as usize);
        let r0_odd = self.chr_bank_index((self.registers[0] | 0x01) as usize);
        let r1_even = self.chr_bank_index((self.registers[1] & 0xFE) as usize);
        let r1_odd = self.chr_bank_index((self.registers[1] | 0x01) as usize);
        let banks_2k = [r0_even, r0_odd, r1_even, r1_odd];
        let banks_1k = [
            self.chr_bank_index(self.registers[2] as usize),
            self.chr_bank_index(self.registers[3] as usize),
            self.chr_bank_index(self.registers[4] as usize),
            self.chr_bank_index(self.registers[5] as usize),
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

    /// TQROM's CHR-window materializer (ticket W14-58) — lays out the same
    /// eight 1 KiB window slots [`Mmc3::recompute_chr_view`] does (the
    /// register-to-window mapping and the A12-inversion swap are identical
    /// MMC3 hardware, nesdev.org/wiki/INES_Mapper_119: "CHR banking is
    /// implemented similar to how MMC3 implements it"), but resolves each
    /// slot's raw register BYTE (bit 6 intact, not yet masked to a page
    /// number) into either `chr_rom` or `chr_ram` depending on that bit,
    /// and records which in `chr_ram_mask`/`chr_ram_slot_page` for
    /// [`Mmc3::chr_window_writeback`] and [`Mapper::chr_ram_page_mask`] to
    /// use.
    ///
    /// Deliberately NOT called from [`Mmc3::cpu_write`] the way
    /// [`Mmc3::recompute_chr_view`] is for every other variant — see
    /// `cpu_write`'s own comment and this module's "TQROM" doc section for
    /// why the recompute has to wait for
    /// [`Mmc3::chr_window_writeback`] to run first.
    fn recompute_chr_view_tqrom(&mut self) {
        let count = self.chr_bank_count_1k();
        let r0_even = self.registers[0] & 0xFE;
        let r0_odd = self.registers[0] | 0x01;
        let r1_even = self.registers[1] & 0xFE;
        let r1_odd = self.registers[1] | 0x01;
        let banks_2k = [r0_even, r0_odd, r1_even, r1_odd];
        let banks_1k = [
            self.registers[2],
            self.registers[3],
            self.registers[4],
            self.registers[5],
        ];
        let windows: [u8; 8] = if self.chr_a12_inverted() {
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
        self.chr_ram_mask = 0;
        for (slot, &raw) in windows.iter().enumerate() {
            let dst = slot * CHR_BANK_1K;
            if raw & 0x40 != 0 {
                // Bit 6 set: 8 KiB CHR-RAM page, bits 0-2.
                let page = (raw & 0x07) as usize % CHR_RAM_PAGES;
                self.chr_ram_mask |= 1 << slot;
                self.chr_ram_slot_page[slot] = page as u8;
                let src = page * CHR_BANK_1K;
                self.chr_view[dst..dst + CHR_BANK_1K]
                    .copy_from_slice(&self.chr_ram[src..src + CHR_BANK_1K]);
            } else {
                // Bit 6 clear: CHR-ROM page, bits 0-5 (64 KiB max).
                let page = (raw & 0x3F) as usize % count;
                let src = page * CHR_BANK_1K;
                self.chr_view[dst..dst + CHR_BANK_1K]
                    .copy_from_slice(&self.chr_rom[src..src + CHR_BANK_1K]);
            }
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
                // Ticket W14-58: TQROM defers the CHR recompute to
                // `chr_window_writeback` (called by `NesBus` right before
                // `chr_window` is asked for the new view) instead of
                // doing it eagerly here -- an eager recompute would
                // overwrite `chr_ram_mask`/`chr_ram_slot_page` for the
                // NEW window before this same write's `push_mapper_view`
                // gets a chance to hand back the OLD window's live PPU
                // buffer, corrupting whichever RAM page the OLD mapping
                // pointed at (see `Mapper::chr_window_writeback`'s doc).
                // Every other variant is unaffected: nothing else reads
                // `chr_view` between this write and the next
                // `push_mapper_view` call.
                if !self.tqrom {
                    self.recompute_chr_view();
                }
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
        if self.txsrom {
            return self.txsrom_mirroring();
        }
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

    /// Mapper 47's outer bank select (ticket W14-58; nesdev.org/wiki/
    /// INES_Mapper_047): bit 0 of any `$6000-$7FFF` write picks the 128
    /// KiB PRG / 128 KiB CHR half `prg_bank`/`chr_bank_index` confine the
    /// ordinary MMC3 registers to. `NesBus` still stores the byte in its
    /// own PRG-RAM array regardless (this board has none, but nothing
    /// reads it back) -- this is purely an observer, the same shape
    /// `NINA-001`/Jaleco JF already use for their own `$6000-$7FFF`
    /// registers (this module's `super` doc, "Mapper scope"). No-op for
    /// every other variant.
    fn cpu_write_wram(&mut self, addr: u16, value: u8) {
        let _ = addr;
        if self.mapper47 {
            self.outer_bank = value & 0x01;
            self.recompute_chr_view();
        }
    }

    /// Ticket W14-58 — see [`Mapper::chr_window_writeback`]'s doc and this
    /// module's "TQROM" section. No-op for every non-TQROM variant.
    fn chr_window_writeback(&mut self, ppu_chr: &[u8]) {
        if !self.tqrom {
            return;
        }
        for slot in 0..8usize {
            if self.chr_ram_mask & (1 << slot) == 0 {
                continue;
            }
            let page = self.chr_ram_slot_page[slot] as usize;
            let src = slot * CHR_BANK_1K;
            let dst = page * CHR_BANK_1K;
            self.chr_ram[dst..dst + CHR_BANK_1K].copy_from_slice(&ppu_chr[src..src + CHR_BANK_1K]);
        }
        // Now that any live RAM edits are folded back into `chr_ram`,
        // materialize the window this write's OWN register change (if
        // any -- see `cpu_write`'s `0x8001` arm) selected.
        self.recompute_chr_view_tqrom();
    }

    /// Ticket W14-58 — see [`Mapper::chr_ram_page_mask`]'s doc. `0` (every
    /// page read-only) for every non-TQROM variant, matching the default.
    fn chr_ram_page_mask(&self) -> u8 {
        if self.tqrom {
            self.chr_ram_mask
        } else {
            0
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

    /// `MAPR` (ticket W2-04): bank select + the eight bank registers, the
    /// mirroring and PRG-RAM protect registers, the whole IRQ unit
    /// (latch, counter, reload flag, enable, pending) and the materialized
    /// CHR window. `revision` is not state -- it is how the cartridge was
    /// constructed (`crate::mappers::mmc3`'s "Which revision does a real
    /// cartridge get?" section), and a state that could silently flip it
    /// would change IRQ behavior invisibly.
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.bank_select)?;
        out.bytes(&self.registers)?;
        out.u8(self.mirroring_reg)?;
        out.u8(self.prg_ram_protect)?;
        out.u8(self.irq_latch)?;
        out.u8(self.irq_counter)?;
        out.bool(self.irq_reload_flag)?;
        out.bool(self.irq_enabled)?;
        out.bool(self.irq_pending)?;
        out.bytes(&self.chr_view)?;
        // Ticket W14-58: appended after every pre-existing field, without
        // renumbering any of them (this module's ticket note) -- TQROM's
        // 8 KiB CHR-RAM chip plus the mask/page bookkeeping
        // `chr_window_writeback` needs to fold PPU-side writes back into
        // the right page, and mapper 47's outer bank select. All three
        // are `0`/all-zero for every other variant, so this costs a few
        // harmless bytes on the common case rather than a format version
        // flag.
        out.u8(self.chr_ram_mask)?;
        out.bytes(&self.chr_ram_slot_page)?;
        out.bytes(&self.chr_ram)?;
        out.u8(self.outer_bank)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.bank_select = inp.u8()?;
        inp.bytes(&mut self.registers)?;
        self.mirroring_reg = inp.u8()?;
        self.prg_ram_protect = inp.u8()?;
        self.irq_latch = inp.u8()?;
        self.irq_counter = inp.u8()?;
        self.irq_reload_flag = inp.bool()?;
        self.irq_enabled = inp.bool()?;
        self.irq_pending = inp.bool()?;
        inp.bytes(&mut self.chr_view)?;
        self.chr_ram_mask = inp.u8()?;
        inp.bytes(&mut self.chr_ram_slot_page)?;
        inp.bytes(&mut self.chr_ram)?;
        self.outer_bank = inp.u8()?;
        Ok(())
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

    /// **TxSROM: nametable pages come from CHR bank bit 7, per the
    /// register that maps each table's A10-A12** (ticket W14-12; nesdev
    /// "INES Mapper 118"). `$A000` is ignored.
    #[test]
    fn txsrom_mirroring_follows_chr_bank_bit_7_in_both_a12_modes() {
        let mut m = Mmc3::new_txsrom(prg(4), chr(64), false);
        m.cpu_write(0xA000, 0x01, 0); // would be horizontal on plain MMC3
                                      // A12 not inverted: R0 -> $2000/$2400, R1 -> $2800/$2C00.
        select(&mut m, 0, 0x80 | 2);
        select(&mut m, 1, 0x00 | 4);
        assert_eq!(m.mirroring(), Mirroring::PerTable([1, 1, 0, 0]));
        select(&mut m, 0, 0x00 | 2);
        select(&mut m, 1, 0x80 | 4);
        assert_eq!(m.mirroring(), Mirroring::PerTable([0, 0, 1, 1]));
        // A12 inverted: R2..R5 -> one table each.
        select_with_mode(&mut m, 0x80, 2, 0x80);
        select_with_mode(&mut m, 0x80, 3, 0x00);
        select_with_mode(&mut m, 0x80, 4, 0x00);
        select_with_mode(&mut m, 0x80, 5, 0x80);
        assert_eq!(m.mirroring(), Mirroring::PerTable([1, 0, 0, 1]));
        // The bank bit itself still banks CHR (masked by the bank count).
        assert_eq!(m.chr_window().unwrap()[0], 0x80, "R2 = $80 -> bank 0 of 64");
    }

    // ---- TQROM (mapper 119, ticket W14-58) ----

    /// Drives `m` through exactly the sequence `NesBus::push_mapper_view`
    /// does (module doc, "TQROM" section): hand the mapper back whatever
    /// the PPU buffer currently holds FIRST, then ask for -- and copy in
    /// -- the freshly materialized window. Every TQROM test below uses
    /// this instead of reading `m.chr_window()` directly, because a raw
    /// `chr_window()` read between two register writes can observe a
    /// window `chr_window_writeback` hasn't recomputed yet (`cpu_write`'s
    /// `0x8001` arm comment).
    fn push(m: &mut Mmc3, ppu_chr: &mut [u8; CHR_VIEW_SIZE]) {
        Mapper::chr_window_writeback(m, ppu_chr);
        ppu_chr.copy_from_slice(m.chr_window().expect("TQROM always has CHR"));
    }

    #[test]
    fn tqrom_window_mixes_rom_and_ram_pages_side_by_side() {
        let mut m = Mmc3::new_tqrom(prg(2), chr(64)); // 64 x 1 KiB CHR-ROM pages
        let mut ppu_chr = [0u8; CHR_VIEW_SIZE];
        select(&mut m, 0, 4); // R0 (2K, slots 0-1): CHR-ROM page 4
        select(&mut m, 2, 0x40 | 2); // R2 (1K, slot 4): CHR-RAM page 2
        push(&mut m, &mut ppu_chr);

        let mask = Mapper::chr_ram_page_mask(&m);
        assert_eq!(mask & 0b0000_0011, 0, "slots 0-1 (R0's ROM pair) stay ROM");
        assert_ne!(mask & (1 << 4), 0, "slot 4 (R2, bit6 set) is RAM");
        assert_eq!(
            ppu_chr[0],
            0x80 + 4,
            "ROM slot's bytes come straight from chr_rom"
        );
        assert_eq!(
            ppu_chr[4 * CHR_BANK_1K],
            0,
            "RAM slot starts zeroed, not chr_rom garbage"
        );
    }

    #[test]
    fn tqrom_ram_page_write_is_ignored_when_the_slot_is_rom() {
        let mut m = Mmc3::new_tqrom(prg(2), chr(64));
        let mut ppu_chr = [0u8; CHR_VIEW_SIZE];
        select(&mut m, 0, 4); // R0 -> ROM page 4 pair (slots 0-1)
        push(&mut m, &mut ppu_chr);
        assert_eq!(ppu_chr[0], 0x80 + 4);

        // A PPU-side write lands in the buffer directly in this harness
        // (ticket W14-58's real gate is `ppu::mem::chr_write`'s mask
        // check, tested separately in `crate::ppu::mem`'s test module) --
        // what this test proves is that `chr_window_writeback` never
        // copies a ROM slot's bytes anywhere, so the "write" has nowhere
        // to land: the next materialize re-derives the slot from
        // `chr_rom`, byte-for-byte, regardless of what the buffer held.
        ppu_chr[0] = 0xFF;
        select(&mut m, 2, 0x40 | 1); // switch something else, forcing a push
        push(&mut m, &mut ppu_chr);
        select(&mut m, 0, 4); // switch back to the same ROM bank
        push(&mut m, &mut ppu_chr);
        assert_eq!(
            ppu_chr[0],
            0x80 + 4,
            "ROM page content is unaffected by the earlier PPU-side write"
        );
    }

    #[test]
    fn tqrom_ram_page_write_survives_a_bank_switch_away_and_back() {
        let mut m = Mmc3::new_tqrom(prg(2), chr(64));
        let mut ppu_chr = [0u8; CHR_VIEW_SIZE];
        select(&mut m, 2, 0x40 | 3); // R2 (slot 4) -> CHR-RAM page 3
        push(&mut m, &mut ppu_chr);
        assert_ne!(Mapper::chr_ram_page_mask(&m) & (1 << 4), 0);

        ppu_chr[4 * CHR_BANK_1K] = 0xAB; // PPU-side write into RAM page 3

        select(&mut m, 2, 0x40 | 5); // bank-switch AWAY to RAM page 5
        push(&mut m, &mut ppu_chr);
        assert_eq!(
            ppu_chr[4 * CHR_BANK_1K],
            0,
            "now showing page 5, untouched by the earlier write"
        );

        select(&mut m, 2, 0x40 | 3); // switch BACK to page 3
        push(&mut m, &mut ppu_chr);
        assert_eq!(
            ppu_chr[4 * CHR_BANK_1K],
            0xAB,
            "page 3's earlier PPU-side write survived the round trip"
        );
    }

    #[test]
    fn tqrom_save_load_round_trip_mid_frame_reads_back_a_ram_write() {
        // This module's ticket note names the exact acceptance sequence:
        // write to a RAM page, bank-switch, save, load, read back. The
        // bank-switch matters, not just for plot -- it is what runs
        // `chr_window_writeback` (`NesBus::push_mapper_view`'s doc; a
        // direct-through-`Mmc3` test drives it via this module's `push`
        // helper) and folds the edit into `chr_ram` BEFORE `save_state`
        // ever runs. `save_state`/`save_region` take `&self` -- see
        // `crate::system::state`'s `StateRegion::Mapper` arm doc for why
        // a save strictly BETWEEN two register writes, with no
        // bank-switch to flush an edit first, is this ticket's one
        // documented gap instead.
        let mut m = Mmc3::new_tqrom(prg(2), chr(64));
        let mut ppu_chr = [0u8; CHR_VIEW_SIZE];
        select(&mut m, 2, 0x40 | 3); // R2 (slot 4) -> CHR-RAM page 3
        push(&mut m, &mut ppu_chr);
        ppu_chr[4 * CHR_BANK_1K + 7] = 0x99; // a live PPU-side edit

        // Bank-switch: same slot, same RAM page -- still runs the
        // writeback/materialize cycle, flushing the edit into `chr_ram`.
        select(&mut m, 2, 0x40 | 3);
        push(&mut m, &mut ppu_chr);

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        m.save_state(&mut StateOut::new(&mut stream)).unwrap();

        let mut restored = Mmc3::new_tqrom(prg(2), chr(64));
        restored.load_state(&mut StateIn::new(&mut stream)).unwrap();
        assert_eq!(
            restored.chr_window().unwrap()[4 * CHR_BANK_1K + 7],
            0x99,
            "the RAM write survived a save/load round trip"
        );
        // And it keeps surviving a further bank switch away and back,
        // exactly as it did before the round trip.
        let mut restored_ppu_chr = [0u8; CHR_VIEW_SIZE];
        restored_ppu_chr.copy_from_slice(restored.chr_window().unwrap());
        select(&mut restored, 2, 0x40 | 5);
        push(&mut restored, &mut restored_ppu_chr);
        select(&mut restored, 2, 0x40 | 3);
        push(&mut restored, &mut restored_ppu_chr);
        assert_eq!(restored_ppu_chr[4 * CHR_BANK_1K + 7], 0x99);
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

    // ---- Mapper 47 (MMC3 2-in-1 outer bank, ticket W14-58) ----

    /// Marks bank `n` of each 128 KiB half distinctly: PRG bank `n` of
    /// half `h` starts with `0x40 + n` in half 0, `0x60 + n` in half 1;
    /// CHR 1 KiB page `n` of half `h` starts with `0x80 + n` in half 0,
    /// `0xA0 + n` in half 1 -- so a test can tell which half's data it's
    /// actually reading without needing to inspect `outer_bank` itself.
    fn mark_mapper47_halves(prg: &mut [u8], chr: &mut [u8]) {
        let prg_half = prg.len() / 2;
        let prg_bank_count = prg_half / PRG_BANK;
        for half in 0..2u8 {
            for n in 0..prg_bank_count {
                let base = half as usize * prg_half + n * PRG_BANK;
                prg[base] = (if half == 0 { 0x40 } else { 0x60 }) + n as u8;
            }
        }
        let chr_half = chr.len() / 2;
        let chr_bank_count = chr_half / CHR_BANK_1K;
        for half in 0..2u8 {
            for n in 0..chr_bank_count {
                let base = half as usize * chr_half + n * CHR_BANK_1K;
                chr[base] = (if half == 0 { 0x80 } else { 0xA0 }) + n as u8;
            }
        }
    }

    #[test]
    fn mapper47_outer_bank_select_changes_which_half_prg_and_chr_address() {
        let mut prg = vec![0u8; 256 * 1024];
        let mut chr = vec![0u8; 128 * 1024];
        mark_mapper47_halves(&mut prg, &mut chr);
        let mut m = Mmc3::new_mapper47(prg, chr);

        select(&mut m, 6, 2); // R6 -> PRG bank 2 within the current half
        select(&mut m, 2, 3); // R2 (1K slot 4) -> CHR page 3 within the half
        assert_eq!(m.cpu_read(0x8000), 0x40 + 2, "half 0 PRG bank 2");
        assert_eq!(
            m.chr_window().unwrap()[4 * CHR_BANK_1K],
            0x80 + 3,
            "half 0 CHR page 3"
        );

        m.cpu_write_wram(0x6000, 0x01); // outer bank -> half 1
        assert_eq!(
            m.cpu_read(0x8000),
            0x60 + 2,
            "same R6 value, now half 1's PRG bank 2"
        );
        assert_eq!(
            m.chr_window().unwrap()[4 * CHR_BANK_1K],
            0xA0 + 3,
            "same R2 value, now half 1's CHR page 3"
        );

        m.cpu_write_wram(0x7FFF, 0x00); // any $6000-$7FFF address, bit0 -> half 0
        assert_eq!(m.cpu_read(0x8000), 0x40 + 2, "back to half 0");
    }
}
