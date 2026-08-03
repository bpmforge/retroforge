//! Ricoh 2A03 CPU core — official opcodes + addressing modes only.
//!
//! The 2A03 is a stock NMOS 6502 with the BCD (decimal) ALU mode disabled:
//! the `D` flag still exists and is settable, but `ADC`/`SBC` never apply
//! the decimal fixups regardless of its state (nesdev.org/wiki/CPU, "2A03
//! ... lacks decimal mode"). Everything else — registers, flags, addressing
//! modes, per-cycle bus behavior including dummy reads/writes — matches the
//! NMOS 6502 exactly (EMULATION_CORES.md §2.1).
//!
//! Scope: the 256 official opcodes (ticket W1-01a) plus the 105
//! unofficial/illegal opcodes and hardware interrupt polling — NMI edge,
//! IRQ level, BRK/IRQ hijacking (ticket W1-01b, see `cpu/exec.rs` and
//! `cpu/ops.rs` module docs for the per-opcode citations).
//!
//! Verified against nesdev.org/6502_cpu.txt (the classic cycle-by-cycle
//! bus-operation reference, also mirrored as the "64doc" NMOS 6502/6510
//! instruction timing document) and cross-checked against the
//! SingleStepTests `nes6502` JSON vectors (see `tests::vectors`) — all
//! 151 official opcodes pass all 10,000 vector cases each, bus trace
//! included, which is also empirical confirmation of the no-BCD claim
//! above: the vectors exercise `ADC`/`SBC` with `D` set, and a decimal
//! implementation would diverge on most of those cases.
//!
//! ## Stepping granularity (EMULATION_CORES.md §2.1)
//!
//! [`Cpu::step`] runs one whole instruction, issuing every bus read/write
//! it needs — dummy accesses included — in exact hardware cycle order via
//! [`CpuBus`]; every 6502 clock cycle is exactly one bus access, so
//! counting bus operations (see `exec::CountingBus`) *is* counting cycles,
//! not an approximation. There is no mid-instruction suspend point: this
//! ticket does not expose a way to stop after cycle N of M. Per-chip
//! catch-up interleaving (PPU/APU ticking alongside the CPU, "mandatory
//! for DMC DMA conflicts... OAM DMA alignment") is achieved by a future
//! `CpuBus` implementor ticking those chips from inside its own
//! `read`/`write`, one call per cycle — an additive change to the bus impl
//! W1-02 owns, not a rewrite of this instruction-granular stepper.
mod addressing;
mod exec;
mod ops;

pub use bus::CpuBus;

pub mod bus;

#[cfg(test)]
mod tests;

/// Carry flag: set/cleared by ALU ops, shifts/rotates, comparisons.
pub const FLAG_C: u8 = 0x01;
/// Zero flag: set when a load/ALU result is zero.
pub const FLAG_Z: u8 = 0x02;
/// Interrupt-disable flag: masks IRQ (not modeled by this ticket; the flag
/// itself is still fully readable/settable via `SEI`/`CLI`/`PLP`/`PHP`).
pub const FLAG_I: u8 = 0x04;
/// Decimal-mode flag. Settable (`SED`/`CLD`) but never consulted by
/// `ADC`/`SBC` on the 2A03 — see module doc.
pub const FLAG_D: u8 = 0x08;
/// "Break" flag. Not a real latch in the physical 6502 status register — it
/// only exists in the byte written to the stack by `PHP`/`BRK` (always 1
/// there). [`Cpu::p`] keeps it pinned to **0** at all other times (verified
/// against the `nes6502` SingleStepTests vectors: every `initial`/`final`
/// `p` in `08.json`/`28.json`/`00.json`/`40.json` has bit 4 clear — e.g.
/// case `08 7e 48` in `08.json` has `initial.p=164` and pushes `180` to the
/// stack, i.e. `p | 0x10`, while `final.p` stays `164`; case `28 8c 9e` in
/// `28.json` pulls a stack byte with bit 4 set but ends with `final.p`
/// bit 4 clear), matching the chip's lack of internal storage for it
/// (nesdev.org/wiki/Status_flags).
pub const FLAG_B: u8 = 0x10;
/// Unused flag. Not a real latch either, but unlike [`FLAG_B`] it always
/// reads back as 1 (same vector evidence: bit 5 is set in every
/// `initial`/`final` `p` observed).
pub const FLAG_U: u8 = 0x20;
/// Overflow flag: signed-arithmetic overflow from `ADC`/`SBC`, or bit 6 of
/// the operand after `BIT`.
pub const FLAG_V: u8 = 0x40;
/// Negative flag: copy of bit 7 of the last loaded/computed value.
pub const FLAG_N: u8 = 0x80;

/// Which "unstable" unofficial opcode last executed and had to fall back
/// to a commonly-observed hardware constant rather than a value fully
/// determined by documented logic (EMULATION_CORES.md §2.1: "Unstable ops
/// ... get the commonly-observed constants; flag them in the trace log").
///
/// This is the *marker*, not the logger: ticket W1-01b exposes this field
/// so a future trace logger can flag these instructions; it does not
/// itself write a trace (that's W1-03, `trace logger in nestest.log
/// format`, FR-DBG-003 — see `cpu/exec.rs` module doc for why building
/// one here would be out of scope).
///
/// Includes `Shx`/`Shy` even though EMULATION_CORES.md §2.1's example
/// list names only `XAA/ANE, LXA, AHX/SHA, TAS, LAS` — that parenthetical
/// is illustrative (the same paragraph's *stable*-illegal list is equally
/// non-exhaustive, omitting `ANC`/`ALR`/`ARR`/`SBX`). This implementation's
/// own reasoning for including them: `am_absi_unstable`'s page-cross
/// corruption formula (`cpu/addressing.rs`) is *identical* for
/// `SHA`/`SHX`/`SHY`/`TAS` — same "AND with base-high-byte+1, then on a
/// page cross substitute that value for the target's high byte" mechanism
/// — verified independently against the nes6502 SingleStepTests vectors
/// for all four (`$93`/`$9F`, `$9E`, `$9C`, `$9B`) this session; nesdev's
/// unofficial-opcode reference pages list `SHX`/`SHY`'s mnemonics but
/// don't group them with `SHA`/`TAS` in prose, so this classification is
/// this implementation's conclusion from the shared mechanism, not a
/// nesdev claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnstableOp {
    /// `ANE`/`XAA` (`$8B`).
    Ane,
    /// `LXA` (`$AB`).
    Lxa,
    /// `SHA`/`AHX` (`$93`, `$9F`).
    Sha,
    /// `SHX`/`SXA` (`$9E`).
    Shx,
    /// `SHY`/`SYA` (`$9C`).
    Shy,
    /// `TAS`/`SHS` (`$9B`).
    Tas,
    /// `LAS`/`LAR` (`$BB`).
    Las,
}

/// The 6502/2A03 register file plus the cycle-stepped instruction
/// dispatcher. Holds no bus/memory state of its own — every access crosses
/// [`CpuBus`].
///
/// Besides the six architectural registers, this carries interrupt-line
/// bookkeeping (NMI edge latch, penultimate-cycle poll results — see
/// `cpu/exec.rs`'s `CountingBus`/`run_cycled` module docs) and the
/// unstable-op/JAM markers. All of it derives `PartialEq`/`Eq` and will
/// participate in state comparisons and (eventually) save-state
/// (FR-CORE-002/003) — that's intentional: every one of these fields is
/// fully determined by prior register state, the executed opcode, and the
/// bus's line state, so two `Cpu`s reaching the same point via the same
/// inputs are still bit-identical, which is what determinism requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    /// Stack pointer (offset into page `$01xx`; `S` in nesdev's notation).
    pub s: u8,
    pub pc: u16,
    /// Processor status. Bit 5 (unused) always reads as 1, bit 4 (`B`)
    /// always reads as 0 — see [`FLAG_B`]/[`FLAG_U`]; use [`Cpu::set_p`]
    /// rather than assigning this field directly so that invariant holds.
    pub p: u8,

    /// Set by the just-executed instruction if it was an unstable
    /// unofficial opcode (see [`UnstableOp`]); cleared to `None` at the
    /// start of every instruction (including hardware interrupt entries,
    /// which are never unstable).
    pub unstable_op: Option<UnstableOp>,
    /// Set when the just-executed instruction was a `JAM`/`KIL` opcode
    /// (ticket W1-01b decision: represented by leaving `pc` pointing back
    /// at the `JAM` byte — see `cpu/exec.rs`'s `jam` doc comment — rather
    /// than a separate "don't execute" short-circuit, so the bus trace
    /// stays correct on every subsequent `step`). This field is purely
    /// informational, e.g. for a future debugger; nothing in `cpu/**`
    /// reads it to change behavior. Cleared to `false` at the start of
    /// every instruction.
    pub jammed: bool,

    /// Edge-detector raw-level bookkeeping and interrupt-poll state —
    /// private, see `cpu/exec.rs`'s `CountingBus`/`run_cycled` module docs
    /// for the full model. `nmi_prev_asserted` is the NMI line's asserted
    /// state as of the most recent sample (carried across instructions so
    /// a transition spanning an instruction boundary is still caught);
    /// `nmi_edge_latched` is the sticky "an edge occurred and hasn't been
    /// serviced yet" latch; `pending_nmi_after`/`pending_irq_after` are
    /// the penultimate-cycle poll results from the *last* instruction,
    /// consulted by `Cpu::step` to decide whether the next thing to run is
    /// an interrupt sequence; `i_flag_poll_snapshot` is the `I` flag value
    /// used to gate that IRQ poll (nesdev.org/wiki/CPU_interrupts: most
    /// instructions poll using the *pre*-instruction `I`; `RTI` is the one
    /// exception, polling with the newly-restored value, and `BRK`/`IRQ`/
    /// `NMI` entry itself is another, re-snapshotting right after setting
    /// `I` — see `exec.rs`). `nmi_hijack_consumed` is set mid-instruction
    /// by a `BRK`/`IRQ` entry's hijack decision (also `exec.rs`) to tell
    /// `run_cycled`'s post-instruction commit "this edge was just
    /// serviced by the hijack — don't also leave it latched or schedule a
    /// further NMI from it", since a plain field write from inside the
    /// `body` closure can't reach the `CountingBus`-derived commit values
    /// directly (mirrors the `i_flag_poll_snapshot`-override pattern used
    /// for `RTI`).
    nmi_prev_asserted: bool,
    nmi_edge_latched: bool,
    pending_nmi_after: bool,
    pending_irq_after: bool,
    i_flag_poll_snapshot: bool,
    nmi_hijack_consumed: bool,
}

/// A plausible cold-boot register file (`S=$FD`, `I` set, matching the
/// commonly-cited NMOS 6502 power-on values) — **not** a modeled reset
/// sequence. Real hardware reset reads the reset vector at `$FFFC`/`$FFFD`
/// and takes 7 cycles doing it (nesdev.org/6502_cpu.txt "RESET... lasts
/// probably 6 cycles after deactivating the signal"); that bus-visible
/// sequence, and `ResetKind::Soft` vs `Hard` (`rf_core_api::ResetKind`),
/// are for the ticket that wires this CPU into a full system (W1-02+).
impl Default for Cpu {
    fn default() -> Self {
        Cpu {
            a: 0,
            x: 0,
            y: 0,
            s: 0xFD,
            pc: 0,
            p: FLAG_I | FLAG_U,
            unstable_op: None,
            jammed: false,
            nmi_prev_asserted: false,
            nmi_edge_latched: false,
            pending_nmi_after: false,
            pending_irq_after: false,
            i_flag_poll_snapshot: false,
            nmi_hijack_consumed: false,
        }
    }
}

impl Cpu {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `p` from an arbitrary byte, pinning bit 5 to 1 and bit 4 to 0
    /// (see module doc). Used by `PLP`/`RTI` and by the vector-test harness
    /// when loading a test case's `initial.p`.
    pub fn set_p(&mut self, value: u8) {
        self.p = (value & !FLAG_B) | FLAG_U;
    }

    #[inline]
    pub fn flag(&self, mask: u8) -> bool {
        self.p & mask != 0
    }

    #[inline]
    pub fn set_flag(&mut self, mask: u8, on: bool) {
        if on {
            self.p |= mask;
        } else {
            self.p &= !mask;
        }
        self.p = (self.p & !FLAG_B) | FLAG_U;
    }

    /// Set `N` from bit 7 of `value` and `Z` from `value == 0` — the
    /// common flag update shared by every load and most ALU/RMW ops.
    #[inline]
    fn set_nz(&mut self, value: u8) {
        self.set_flag(FLAG_N, value & 0x80 != 0);
        self.set_flag(FLAG_Z, value == 0);
    }

    #[inline]
    fn stack_addr(&self) -> u16 {
        0x0100 | self.s as u16
    }

    /// Push `value`, then decrement `S` (wrapping — the stack pointer
    /// wraps within page `$01xx` on real hardware, never carries out of it).
    fn push(&mut self, bus: &mut dyn CpuBus, value: u8) {
        bus.write(self.stack_addr(), value);
        self.s = self.s.wrapping_sub(1);
    }

    /// Fetch the byte at `PC`, incrementing `PC`. Every instruction starts
    /// with this (nesdev 6502_cpu.txt: "fetch opcode, increment PC").
    fn fetch(&mut self, bus: &mut dyn CpuBus) -> u8 {
        let value = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        value
    }

    /// The dummy "read next instruction byte (and throw it away)" cycle
    /// shared by every implied/accumulator instruction and as the first
    /// extra cycle of PLA/PLP/RTS/RTI (nesdev.org/6502_cpu.txt). Unlike
    /// [`Cpu::fetch`], `PC` is *not* incremented — the byte is re-read as
    /// the next real opcode fetch.
    pub(super) fn implied_dummy_read(&mut self, bus: &mut dyn CpuBus) {
        bus.read(self.pc);
    }

    /// Execute exactly one instruction, driving `bus` one cycle at a time
    /// in hardware order (opcode fetch through the last write-back), and
    /// return the number of bus cycles it consumed.
    ///
    /// Before fetching a normal opcode, checks whether the *previous*
    /// instruction's penultimate-cycle poll (nesdev.org/wiki/CPU_interrupts)
    /// found a pending interrupt; if so, this call instead runs that
    /// hardware-forced NMI/IRQ entry sequence (NMI takes priority when
    /// both are pending). Either way, the first bus cycle is a real
    /// `bus.read` at `PC` — for a hardware interrupt it's the "fetch
    /// opcode (discarded, $00 forced)" cycle nesdev's IRQ/NMI table
    /// describes, done here (rather than inside `exec::nmi_sequence`/
    /// `irq_sequence`) so it participates in interrupt-line sampling the
    /// same way the normal opcode fetch does — see `cpu/exec.rs`'s
    /// `CountingBus` doc.
    pub fn step(&mut self, bus: &mut dyn CpuBus) -> u32 {
        if self.pending_nmi_after {
            self.pending_nmi_after = false;
            // Servicing consumes the latch. Done *here*, before
            // `nmi_sequence`'s `run_cycled` seeds its `CountingBus` from
            // `nmi_edge_latched`, not inside the sequence's own body —
            // the latch is `cb`'s for the duration of that call, so a
            // write to `self.nmi_edge_latched` from inside the body would
            // just be overwritten by `run_cycled`'s post-instruction
            // commit (`cpu.nmi_edge_latched = cb.edge_latched_curr`).
            self.nmi_edge_latched = false;
            bus.read(self.pc);
            return exec::nmi_sequence(self, bus);
        }
        if self.pending_irq_after {
            self.pending_irq_after = false;
            bus.read(self.pc);
            return exec::irq_sequence(self, bus);
        }
        let opcode = self.fetch(bus);
        exec::execute(self, bus, opcode)
    }

    /// Hardware reset/power-on sequence (ticket W1-03; nesdev.org/6502_cpu.txt
    /// "RESET": the reset line performs an interrupt-shaped sequence with
    /// writes turned into reads (`R/W` held high, since nothing may
    /// actually be written while the machine is resetting) and a fixed
    /// `$FFFC`/`$FFFD` vector instead of `$FFFA`/`$FFFB`/`$FFFE`/`$FFFF`,
    /// taking the same 7 cycles as `BRK`/`IRQ`/`NMI` entry).
    ///
    /// Starts from [`Cpu::default`]'s cold-boot register file — see that
    /// impl's doc for why `S` is *already* the conventional post-reset
    /// value ($FD) and this function must not decrement it a second time —
    /// then burns exactly the 7 bus-visible cycles real hardware does: two
    /// dummy opcode-fetch-shaped reads, three dummy "phantom push" reads at
    /// the addresses the conventional pre-reset stack pointer ($00,
    /// decrementing to $FD over the three cycles — the same convention
    /// [`Cpu::default`]'s doc cites) would have touched, then the real
    /// two-byte vector fetch. The `debug_assert_eq!` below is not a fixup:
    /// it's a self-check that the two conventions ($00-before,
    /// $FD-after) agree, so a future edit to either doesn't silently
    /// desync them.
    ///
    /// Ticket W1-03: this is nestest's own reset exit condition —
    /// `nestest.log`'s first line reads `PC=$C000 ... CYC:7`, documented
    /// as "the trace starts after a 7-cycle reset" — and W1-02 deliberately
    /// deferred building it (see `crate::system` module doc's "master-clock
    /// seam" section and `NesBus::run_oam_dma`'s doc on the DMA get/put
    /// phase anchor this also fixes for any caller that runs this before
    /// its first `$4014` write).
    ///
    /// Callers that need nestest's automated-test-mode entry (which
    /// bypasses the ROM's own reset-vector code and jumps straight to
    /// `$C000`) must override `pc` themselves *after* calling this — that
    /// override is nestest-specific test-harness behavior, not a general
    /// reset semantic, so it does not belong in this function.
    pub fn power_on(bus: &mut dyn CpuBus) -> Cpu {
        let mut cpu = Cpu::default();
        bus.read(cpu.pc); // cycle 1: dummy opcode-fetch-shaped read
        bus.read(cpu.pc); // cycle 2: dummy opcode-fetch-shaped read
        let mut phantom_s: u8 = 0; // conventional pre-reset S (see doc)
        for _ in 0..3 {
            // cycles 3-5: phantom "push" reads (R/W held high -> reads,
            // never writes, during reset).
            bus.read(0x0100 | phantom_s as u16);
            phantom_s = phantom_s.wrapping_sub(1);
        }
        debug_assert_eq!(
            phantom_s, cpu.s,
            "phantom pre-reset S convention must land on Cpu::default's S"
        );
        let lo = bus.read(0xFFFC); // cycle 6: reset vector low byte
        let hi = bus.read(0xFFFD); // cycle 7: reset vector high byte
        cpu.pc = u16::from_le_bytes([lo, hi]);
        cpu
    }
}
