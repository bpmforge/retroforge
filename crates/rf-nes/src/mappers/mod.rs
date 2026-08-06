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
use rf_cart::Mirroring;

mod axrom;
mod cnrom;
mod mmc1;
mod mmc3;
mod nrom;
mod uxrom;

#[cfg(test)]
mod integration_tests;

pub use axrom::AxRom;
pub use cnrom::Cnrom;
pub use mmc1::Mmc1;
pub use mmc3::{Mmc3, Mmc3Revision};
pub use nrom::Nrom;
pub use uxrom::UxRom;

/// One cartridge mapper's CPU-side and CHR-bank-selection behavior. See
/// this module's doc for exactly which `docs/design/EMULATION_CORES.md`
/// §2.4 trait members are implemented here and which are deliberately
/// deferred, and why.
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
}
