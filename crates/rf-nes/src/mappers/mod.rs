//! The `Mapper` trait (ticket W2-02) and its four launch-set
//! implementations: NROM (0, extracted from `system/mod.rs`'s original
//! inline W1-02 code), MMC1 (1), UxROM (2), CNROM (3).
//! `docs/design/EMULATION_CORES.md` §2.4 is the design source; every
//! hardware behavior fact below cites nesdev.org per project law.
//!
//! ## Why this trait is narrower than §2.4's sketch, member by member
//!
//! §2.4 sketches:
//! ```text
//! fn cpu_read(&mut self, addr: u16, bus: &mut MapperBus) -> BusValue;
//! fn cpu_write(&mut self, addr: u16, val: u8, bus: &mut MapperBus);
//! fn ppu_read(&mut self, addr: u16) -> u8;
//! fn ppu_write(&mut self, addr: u16, val: u8);
//! fn ppu_a12(&mut self, rising_cycle: u64);
//! fn mirroring(&self) -> Mirroring;
//! fn irq_pending(&self) -> bool;
//! fn state_chunk(&self) -> MapperState;
//! ```
//! W1-02's own precedent (the note on its ticket) declined to build this
//! trait at all with only NROM to inform its shape, calling that
//! "unvalidated scaffolding". This ticket has three real implementors, but
//! still deliberately does not build every member above:
//!
//! - **`ppu_a12`/`irq_pending`** — MMC3's A12-rising-edge IRQ counter is
//!   the only consumer of either; MMC3 is ticket **W2-03**. Building them
//!   now, with nothing to drive them, would be exactly the "guessed
//!   signature" the ticket brief warns against — MMC3's real filtering
//!   requirements (the documented ~3-cycle low filter) aren't known to
//!   this ticket.
//! - **`state_chunk`** — the `MAPR` save-state chunk is ticket **W2-04**'s;
//!   no save-state format exists anywhere in this crate yet to inform its
//!   shape.
//! - **`ppu_read`/`ppu_write`** (full per-PPU-access CHR/nametable
//!   routing) — **not built as trait methods at all**, for a reason
//!   specific to this ticket rather than deferred to a later one: the
//!   only call sites that could invoke them (`crate::ppu::Ppu::tick`'s
//!   background/sprite fetch pipeline, in `ppu/mod.rs`/`background.rs`/
//!   `sprites.rs`/`scroll.rs`) are **outside this ticket's write_scope**
//!   (only `ppu/mem.rs` is in scope; see this crate's `plan.json` entry
//!   for W2-02). There is no reachable place to thread a `&mut dyn
//!   Mapper` parameter through the render pipeline. Instead, CHR routing
//!   is done by a **materialize/push** design: [`Mapper::chr_window`]
//!   exposes the mapper's *current* CHR bank as a byte slice, and
//!   [`crate::system::NesBus`] copies it into [`crate::ppu::Ppu`]'s own
//!   pre-existing flat `chr: Vec<u8>` buffer (via
//!   `Ppu::set_chr_window`, added in `ppu/mem.rs`) after every `$8000
//!   -$FFFF` write that could have changed the selected bank.
//!   `ppu/mem.rs`'s existing `chr_read`/`chr_write` addressing logic —
//!   unmodified — does the rest. See that file's module doc for the
//!   receiving half and its CHR-RAM caveat. A real MMC3 (W2-03) needs
//!   genuine per-access PPU-bus interception for its A12 hook, which this
//!   design cannot provide — that ticket will likely need to widen
//!   write_scope to the render-pipeline files this one couldn't touch.
//! - **`MapperBus`/`BusValue`** (open-bus-aware CPU reads, AND-type bus
//!   conflicts on writes) — not built. nesdev's own UxROM/CNROM pages
//!   note bus conflicts exist on the *original* boards but "the relevant
//!   games all work around this in software", so a plain, non-conflicting
//!   write is compatible with real cartridges; no acceptance criterion
//!   here requires modeling the conflict itself.
//! - **`cpu_read` takes `&self`, not `&mut self`** — the one deliberate
//!   signature deviation for a member this ticket DOES implement. None of
//!   NROM/MMC1/UxROM/CNROM has any read-time side effect (MMC1's serial
//!   shift register only ever advances on writes), and
//!   [`crate::system::NesBus::peek`] — the side-effect-free
//!   trace-disassembly read path nestest's byte-exact canary depends on —
//!   has an `&self` receiver it cannot widen (`crate::trace` is outside
//!   this ticket's write_scope). A future mapper whose reads genuinely
//!   need interior mutation (none of this ticket's four do) will have to
//!   revisit `peek`'s own contract to add a side-effect-free counterpart,
//!   the same split this crate already uses for
//!   [`crate::system::Controller::peek_bit`] vs `read_bit` and
//!   [`crate::ppu::Ppu::peek_register`] vs `read_register`.
//! - **`cpu_write` gains a `cycle: u64` parameter** not in §2.4's sketch —
//!   not scope creep: MMC1's documented "ignores writes on consecutive
//!   [CPU] cycles" quirk (nesdev.org/wiki/MMC1) needs it *now*, and it is
//!   the same input MMC3's A12 low-filter will need in W2-03, so it is a
//!   better-informed honest gap than inventing `MapperBus` to carry it.
//!
//! ## MMC3 additions (ticket W2-03) — `ppu_a12`/`irq_pending` built, but not
//! as sketched
//!
//! §2.4's sketch (quoted above) has `fn ppu_a12(&mut self, rising_cycle:
//! u64)` — a per-access hook the PPU would call directly on `&mut dyn
//! Mapper`. That still cannot work with this crate's ownership shape:
//! [`crate::system::NesBus`] owns both `ppu: Ppu` and `mapper: Box<dyn
//! Mapper>` as siblings, and [`crate::ppu::Ppu`]'s fetch call sites
//! (`background.rs`/`sprites.rs`/`scroll.rs`) have no reachable path to a
//! mapper reference without `Ppu` holding one — which would invert this
//! crate's layering (PPU depending on the mapper abstraction) for a
//! capability only one of five mappers uses.
//!
//! Instead, A12 rising-edge detection (with MMC3's documented low-period
//! filter) is computed entirely inside [`crate::ppu::Ppu`] — mapper-agnostic,
//! since A12 is a PPU-bus-electrical fact independent of what's listening —
//! and exposed as a drainable pulse count (`Ppu::take_a12_edges`, `pub(crate)`
//! to `crate::system` only). `NesBus::tick_master` drains it once per PPU dot
//! and forwards each pulse into the two new trait members below, which
//! **do** match §2.4's sketch member-for-member:
//!
//! - **`fn clock_irq_counter(&mut self) {}`** — called once per filtered A12
//!   rising edge. Default no-op, so NROM/MMC1/UxROM/CNROM need no changes.
//! - **`fn irq_pending(&self) -> bool { false }`** — `NesBus`'s
//!   `CpuBus::irq_line` is now a straight passthrough to this (replacing the
//!   "no IRQ source yet" comment/default that method carried since W1-02).
//!
//! This is the same push/pull convention `chr_window`/`mirroring` already
//! use (`NesBus` polls the mapper and pushes into the PPU after every
//! `$8000-$FFFF` write) — just inverted: here `NesBus` polls the *PPU* and
//! pushes into the *mapper*. Neither `Ppu` nor `Mapper` needs to know the
//! other exists; `NesBus` is the only thing that does, exactly as
//! `docs/ARCHITECTURE.md`'s layer rules already require.
//!
//! **`$A001` (PRG-RAM protect) is deliberately NOT a trait member.** MMC3's
//! [`Mmc3`] stores the two register bits (write-protect, chip
//! enable) faithfully, but nothing in `system/mod.rs`'s `$6000-$7FFF`
//! handling consults them — verified against every one of this ticket's
//! oracle ROMs (`grep -rn 'A001' mmc3_test_2/source mmc3_irq_tests/source`
//! upstream: zero hits) that no sub-ROM this ticket runs ever writes
//! `$A001` at all, so there is no oracle to build the gate against, and
//! MMC3 powers on with WRAM disabled per most emulator conventions — wiring
//! the gate blind risks silently breaking every sub-ROM's own `$6000`
//! result protocol (which depends on `$6000-$7FFF` staying writable) for a
//! behavior nothing here exercises. An honest, narrow gap — the same shape
//! as MMC1's un-modeled PRG-RAM-enable bit above — not a silent one.
use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

mod action53;
mod axrom;
mod bnrom;
mod camerica;
mod cnrom;
mod color_dreams;
mod dxrom;
mod fme7;
mod gxrom;
mod jaleco_jf;
mod jaleco_ss88006;
mod mmc1;
mod mmc2;
mod mmc3;
mod mmc5;
mod nina;
mod nrom;
mod quattro;
mod rambo1;
mod sachen;
mod uxrom;

#[cfg(test)]
mod integration_tests;

pub use action53::Action53;
pub use axrom::AxRom;
pub use bnrom::Bnrom;
pub use camerica::Camerica;
pub use cnrom::Cnrom;
pub use color_dreams::ColorDreams;
pub use dxrom::DxRom;
pub use fme7::Fme7;
pub use gxrom::GxRom;
pub use jaleco_jf::JalecoJf;
pub use jaleco_ss88006::Ss88006;
pub use mmc1::Mmc1;
pub use mmc2::Mmc2;
pub use mmc3::{Mmc3, Mmc3Revision};
pub use mmc5::Mmc5;
pub use nina::Nina;
pub use nrom::Nrom;
pub use quattro::Quattro;
pub use rambo1::Rambo1;
pub use sachen::Sachen;
pub use uxrom::UxRom;

/// One cartridge mapper's CPU-side and CHR-bank-selection behavior. See
/// this module's doc for exactly which `docs/design/EMULATION_CORES.md`
/// §2.4 trait members are implemented here and which are deliberately
/// deferred, and why.
/// What [`Mapper::chr_latch`] hands the PPU (ticket W14-13).
#[derive(Debug, Clone, Copy)]
pub struct ChrLatchView<'a> {
    /// `[left $FD, left $FE, right $FD, right $FE]`, 4 KiB each.
    pub banks: [&'a [u8]; 4],
    /// Per half: `false` = the `$FD` bank, `true` = the `$FE` bank.
    pub selected: [bool; 2],
}

pub trait Mapper {
    /// CPU-visible read of `$8000-$FFFF` (PRG ROM, through whatever
    /// banking this mapper does). Side-effect-free — see this module's
    /// doc for why `&self`, not §2.4's `&mut self`.
    fn cpu_read(&self, addr: u16) -> u8;

    /// CPU-visible write to `$8000-$FFFF` — the mapper's own bank-select
    /// registers; PRG ROM itself is never writable. `cycle` is the bus's
    /// master cycle at the moment of this write
    /// ([`crate::system::NesBus::master_cycle`], sampled *before* this
    /// write's own cycle tick — see that struct's `write_untimed` call
    /// site), needed by MMC1's consecutive-write-ignore quirk; NROM/
    /// UxROM/CNROM ignore it.
    fn cpu_write(&mut self, addr: u16, value: u8, cycle: u64);

    /// CPU write to the **expansion area** `$4020-$5FFF`.
    ///
    /// Defaulted to a no-op, because that region is open bus on every
    /// mapper this crate supported before Action 53 (mapper 28), which
    /// puts its register-select latch at `$5000-$5FFF`. Defaulting keeps
    /// NROM/UxROM/CNROM/MMC1/MMC3/AxROM byte-for-byte unchanged rather
    /// than making every mapper acknowledge a region only one of them
    /// uses.
    fn cpu_write_expansion(&mut self, addr: u16, value: u8) {
        let _ = (addr, value);
    }

    /// CPU write to `$6000-$7FFF` (ticket W14-15). The bus stores it in
    /// its PRG RAM regardless; this is an observer for the boards that
    /// keep registers there — NINA-001 at `$7FFD-$7FFF`, Jaleco JF at any
    /// address in the range. Default no-op.
    fn cpu_write_wram(&mut self, addr: u16, value: u8) {
        let _ = (addr, value);
    }

    /// CPU read from the expansion area `$4020-$5FFF` (ticket W14-16).
    /// `None` means open bus, the answer for every board but MMC5, which
    /// keeps its IRQ status, multiplier and register file there. `&mut`
    /// because reading MMC5's `$5204` acknowledges the IRQ.
    fn cpu_read_expansion(&mut self, addr: u16) -> Option<u8> {
        let _ = addr;
        None
    }

    /// The CPU wrote PPUCTRL (ticket W14-16). MMC5 snoops it for the
    /// sprite size, which decides which CHR bank set draws the
    /// background. Default no-op.
    fn ppu_ctrl_written(&mut self, value: u8) {
        let _ = value;
    }

    /// A second CHR window the PPU uses only for 8x16 sprite pattern
    /// fetches (ticket W14-16; MMC5's bank set A while set B draws the
    /// background). `None` means sprites use [`Mapper::chr_window`].
    fn chr_window_sprites(&self) -> Option<&[u8]> {
        None
    }

    /// MMC5's fill-mode nametable: `(tile, attribute byte)` served for
    /// every entry of a table mapped to kind `3` (ticket W14-16).
    fn fill_tile(&self) -> Option<(u8, u8)> {
        None
    }

    /// A `$8000-$FFFF` CPU window backed by cartridge PRG RAM rather than
    /// PRG ROM (ticket W14-17; MMC5's `$5114-$5117` bit 7 clear).
    /// `Some(offset)` into the bus's own PRG RAM backing store (ticket
    /// W14-22 widened this from a fixed 8 KiB array to a cartridge-sized
    /// buffer; see [`crate::system::NesBus`]'s module doc) when `addr`'s
    /// window selects RAM; `None` (default) means "PRG ROM as usual",
    /// which is every board but MMC5. See [`Mmc5`]'s module doc, "PRG RAM
    /// windows", for why the bus keeps owning the bytes instead of
    /// lending them to the mapper.
    fn prg_ram_window(&self, addr: u16) -> Option<usize> {
        let _ = addr;
        None
    }

    /// The cartridge's total declared PRG RAM size in bytes, pushed once
    /// right after construction (ticket W14-22, before any register
    /// write reaches the mapper) so a multi-chip board can do its own
    /// chip/page arithmetic. Default no-op: every board but MMC5 has
    /// exactly one chip and never needs to know how big it is (the bus
    /// itself owns the bounds check).
    fn set_prg_ram_len(&mut self, len: usize) {
        let _ = len;
    }

    /// Byte offset into the bus's own PRG RAM that a `$6000-$7FFF` CPU
    /// access lands at (ticket W14-22). Default: `addr - 0x6000` (bank 0
    /// of a single chip) -- correct for every board except MMC5, whose
    /// `$5113` selects among multiple 8 KiB chips/pages there (see
    /// [`Mmc5`]'s module doc).
    fn wram_offset(&self, addr: u16) -> usize {
        usize::from(addr - 0x6000)
    }

    /// Whether a CPU write to cartridge PRG RAM -- `$6000-$7FFF` or a
    /// RAM-selected `$8000-$FFFF` window -- should actually store (ticket
    /// W14-22; MMC5's `$5102`/`$5103` write-protect pair, nesdev.org/wiki/
    /// MMC5). Default `true`: every board but MMC5 always allows PRG RAM
    /// writes (the same "not modeled, nothing depends on it" shape this
    /// module's doc already documents for MMC3's `$A001`).
    fn prg_ram_write_enabled(&self) -> bool {
        true
    }

    /// MMC5 ExGrafix (`$5104` mode 1) and the vertical split
    /// (`$5200-$5202`) both pick an arbitrary 4 KiB CHR bank per tile
    /// (ticket W14-17) -- something [`Mapper::chr_window`]'s single 8 KiB
    /// materialized view cannot express. This exposes the mapper's whole
    /// CHR ROM instead, pushed once (the bytes never change after cart
    /// load) and consulted by the PPU only when
    /// [`Mapper::ext_attribute_mode`] or [`Mapper::vertical_split`] says
    /// either feature is active. `None` for every board without either
    /// (default), and for MMC5 itself when its CHR is RAM (nothing to
    /// bank).
    fn chr_rom_full(&self) -> Option<&[u8]> {
        None
    }

    /// MMC5 ExGrafix (`$5104` mode 1, ticket W14-17): background tiles
    /// take their CHR bank and palette from extended RAM instead of the
    /// ordinary attribute-table fetch. `Some($5130` bits 0-1, the high
    /// bits that widen ext RAM's 6-bit per-tile bank field to a full 4
    /// KiB bank index`)` when enabled; `None` (default) otherwise.
    fn ext_attribute_mode(&self) -> Option<u8> {
        None
    }

    /// MMC5's vertical split (`$5200-$5202`, ticket W14-17):
    /// `Some((right, split_tile, scroll, chr_bank))` when `$5200` bit 7 is
    /// set. `right` is `$5200` bit 6 (`false` = the split is on the left);
    /// `split_tile` is `$5200` bits 0-4 (the column threshold, see
    /// `crate::ppu::background`'s `split_column` for how the two sides
    /// read it); `scroll` is `$5201`; `chr_bank` is `$5202`, read as a
    /// plain 4 KiB bank index with no `$5130` involved. **Unverified
    /// against a local source** -- no MMC5 register-level page exists in
    /// `docs/research/` and no oracle ROM this ticket runs exercises the
    /// split (see `crate::mappers::Mmc5`'s module doc, "Slice 2"). `None`
    /// (default) disables the split for every other board.
    fn vertical_split(&self) -> Option<(bool, u8, u8, u8)> {
        None
    }

    /// Does this board back nametable kind `2` with 1 KiB of its own RAM,
    /// which the CPU reaches at `$5C00-$5FFF`? The PPU owns that buffer
    /// (it is what the PPU reads); the bus routes the CPU there.
    fn has_ext_nametable_ram(&self) -> bool {
        false
    }

    /// One scanline with rendering enabled began (ticket W14-16), pulled
    /// from the PPU once per CPU instruction like the A12 edges. MMC5's
    /// scanline counter and in-frame flag live on this. Default no-op.
    fn scanline_started(&mut self) {}

    /// Rendering left the visible frame (vblank began, or rendering was
    /// turned off). Default no-op.
    fn frame_ended(&mut self) {}

    /// This mapper's current nametable mirroring. Fixed (header-declared,
    /// passed in at construction) for NROM/UxROM/CNROM; live and
    /// register-driven for MMC1.
    fn mirroring(&self) -> Mirroring;

    /// The mapper's current 8 KiB CHR window (PPU `$0000-$1FFF`), if this
    /// mapper ever changes which physical CHR bytes are visible there.
    /// `None` means "nothing to push" — either this mapper has no CHR
    /// banking at all (NROM, UxROM: CHR is a single fixed region, and
    /// [`crate::ppu::Ppu`]'s own flat buffer, seeded once at construction,
    /// is already the permanently-correct view), or its CHR backing is
    /// RAM (this ticket's push/materialize design cannot let PPU-side
    /// writes round-trip back into a mapper-owned RAM buffer without
    /// reintroducing the write-scope problem `ppu_read`/`ppu_write`
    /// already ran into — see this module's doc — so CHR-RAM banking is
    /// an honest, narrow, documented gap here rather than a silently
    /// lossy one; CNROM/MMC1 with CHR ROM, the common case and the one
    /// every launch-set game in `docs/design/EMULATION_CORES.md`'s table
    /// actually ships, is unaffected).
    fn chr_window(&self) -> Option<&[u8]>;

    /// One filtered PPU-A12 rising edge occurred (ticket W2-03) — see this
    /// module's doc "MMC3 additions" section for why this is pulled by
    /// [`crate::system::NesBus`] from [`crate::ppu::Ppu`] rather than
    /// pushed in directly. Default no-op: only [`Mmc3`] overrides
    /// this; NROM/MMC1/UxROM/CNROM have no scanline counter to clock.
    fn clock_irq_counter(&mut self) {}

    /// Whether this mapper is currently asserting `/IRQ` (ticket W2-03) —
    /// [`crate::system::NesBus`]'s [`crate::cpu::CpuBus::irq_line`] is a
    /// straight passthrough to this. Default `false`: only [`Mmc3`]
    /// has an IRQ source.
    fn irq_pending(&self) -> bool {
        false
    }

    /// `cycles` CPU cycles elapsed (ticket W14-14). Driven from the one
    /// place the bus advances its master clock, so a mapper that counts
    /// CPU cycles for its IRQ — FME-7, RAMBO-1's cycle mode — sees every
    /// one, DMA-stolen cycles included. Default no-op.
    fn tick_cpu_cycles(&mut self, cycles: u32) {
        let _ = cycles;
    }

    /// Serialize this mapper's *state* into the `MAPR` chunk (ticket
    /// W2-04) — bank-select registers, IRQ counters, shift registers and
    /// any materialized CHR window, but never the PRG/CHR ROM bytes
    /// themselves, which are cartridge data the loaded ROM restores.
    ///
    /// This is the `state_chunk` hook §2.4 named and W2-02 deliberately
    /// left out ("left out rather than guessed at" -- see this module's
    /// doc), now that there is a save-state format for it to feed.
    ///
    /// Default: write nothing. That is correct, not a stub, for a mapper
    /// with no writable registers at all (`Nrom`).
    ///
    /// # Errors
    /// Returns [`StateError`] if the underlying writer rejects a write.
    /// An MMC2/MMC4-style CHR latch (ticket W14-13): four 4 KiB banks —
    /// `[left $FD, left $FE, right $FD, right $FE]` — plus which of each
    /// pair is selected now. `None` for every mapper without one.
    ///
    /// The PPU owns the switching: it flips the selection itself at the
    /// pattern fetch that triggers it (nesdev MMC2), because a pull at
    /// instruction granularity would land up to two tile fetches late.
    /// The mapper only supplies the banks and, via
    /// [`Mapper::note_chr_latch`], remembers the selection for its save
    /// state.
    fn chr_latch(&self) -> Option<ChrLatchView<'_>> {
        None
    }

    /// The PPU's current latch selection, copied back once per CPU
    /// instruction so it survives a save state. Default no-op.
    fn note_chr_latch(&mut self, selected: [bool; 2]) {
        let _ = selected;
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let _ = out;
        Ok(())
    }

    /// Restore what [`Mapper::save_state`] wrote. Default: read nothing.
    ///
    /// # Errors
    /// Returns [`StateError`] if the stream is exhausted or holds a value
    /// this mapper cannot accept.
    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        let _ = inp;
        Ok(())
    }
}
