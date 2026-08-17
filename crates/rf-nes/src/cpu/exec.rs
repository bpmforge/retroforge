//! Opcode dispatch: decodes one opcode byte into its addressing-mode bus
//! sequence (`addressing.rs`) plus its register/ALU effect (`ops.rs`), and
//! (ticket W1-01b) the NMI/IRQ interrupt-entry sequences.
//!
//! Byte->mnemonic->addressing-mode assignments verified against the
//! opcode-matrix table in nesdev.org/6502_cpu.txt ("6510 Instructions by
//! Addressing Modes") for the 151 official opcodes, and against
//! nesdev.org/wiki/CPU_unofficial_opcodes's opcode table (mnemonics +
//! addressing modes) plus the nes6502 SingleStepTests vectors themselves
//! (which pin down exact byte-cycle-count-behavior, including for the
//! magic constants nesdev's own pages don't specify — see `ops.rs`'s
//! per-op doc comments) for the 105 unofficial ones — every one of the
//! 256 possible opcode bytes has an arm below (cross-checked by
//! `tests::opcode_table::dispatch_covers_all_256_opcodes`).
//!
//! ## Unofficial opcodes (ticket W1-01b, EMULATION_CORES.md §2.1)
//!
//! - *Stable* illegals (`LAX`, `SAX`, `DCP`, `ISC`, `SLO`, `RLA`, `SRE`,
//!   `RRA`, the NOP/SKB/IGN family, `ANC`, `ALR`, `ARR`, `SBX`) reuse the
//!   same addressing-mode helpers (`addressing.rs`) as the official
//!   opcodes that share their addressing mode, combined with new pure ops
//!   in `ops.rs` — their bus traces are therefore automatically correct
//!   wherever the addressing helper is.
//! - *Unstable* ops (`ANE`/`XAA`, `LXA`, `SHA`/`AHX`, `SHX`, `SHY`, `TAS`,
//!   `LAS`) use magic constants/formulas verified directly against the
//!   nes6502 vectors (see `ops.rs`'s per-op doc comments for citations)
//!   and set [`super::Cpu::unstable_op`] — the marker a future trace
//!   logger (W1-03) consumes; this ticket does not build that logger (see
//!   `dispatch`'s "flagged in the trace" note below for why).
//! - `KIL`/`JAM` opcodes halt the CPU — see the `jam` function doc for the
//!   representation this ticket chose.
//!
//! ## Interrupts (ticket W1-01b, EMULATION_CORES.md §2.1, nesdev.org/wiki/CPU_interrupts)
//!
//! NMI is edge-detected (latches on an asserted transition, stays latched
//! until serviced); IRQ is level-sensitive (re-evaluated fresh every poll,
//! gated by the `I` flag). Both are polled once per instruction, using the
//! interrupt-line status *as of the end of the second-to-last cycle* —
//! not the last cycle, which is why an interrupt arriving on an
//! instruction's final cycle doesn't fire until after the *next*
//! instruction too. [`CountingBus`] implements this generically (see its
//! doc) by sampling `nmi_line()`/`irq_line()` after every bus op and
//! keeping a one-sample-delayed "previous" snapshot, so it applies
//! correctly to every opcode's exact cycle count — including
//! data-dependent ones (page-cross extra cycles, taken/not-taken
//! branches) — without per-arm instrumentation. [`run_cycled`] is the
//! single place that commits the resulting snapshot into `Cpu`, shared by
//! normal opcode dispatch and the hardware-forced NMI/IRQ entry
//! sequences (interrupt entries are seven-cycle "instructions" for
//! polling purposes too, per nesdev's own cycle tables).
use super::bus::CpuBus;
use super::{Cpu, FLAG_B, FLAG_C, FLAG_N, FLAG_V};

/// Wraps the caller's bus so `dispatch` can report a cycle count without
/// threading a counter through every addressing-mode helper by hand — each
/// `read`/`write` that reaches real hardware is exactly one clock cycle
/// (module doc: "cycle-stepped... not 'execute then add N cycles'"), so
/// counting bus operations *is* counting cycles, not an approximation of it.
///
/// It also carries the interrupt-line sampling nesdev's "polled on the
/// second-to-last cycle" rule needs (see module doc). Every `read`/`write`
/// re-samples `nmi_line()`/`irq_line()` immediately after the real bus op,
/// and keeps both the *current* sample and the sample from one op ago:
/// after `M` total bus ops inside this wrapper, `count == 1 + M` (the `+1`
/// accounts for the opcode fetch that happened *outside*, before this
/// wrapper existed — see `dispatch`/`run_cycled`), and the "one op ago"
/// snapshot therefore always reflects the state as of cycle `count - 1`,
/// i.e. the penultimate cycle, regardless of how many ops the instruction
/// turns out to take. `edge_latched_curr`/`edge_latched_prev` are NMI's
/// sticky edge-detector output (monotonic within one instruction, carried
/// in from `Cpu::nmi_edge_latched` so a latch from an earlier, still
/// unserviced instruction stays visible); `irq_raw_curr`/`irq_raw_prev`
/// are IRQ's raw asserted level (no latching — level-sensitive).
struct CountingBus<'a> {
    inner: &'a mut dyn CpuBus,
    count: u32,
    nmi_prev_asserted: bool,
    edge_latched_curr: bool,
    edge_latched_prev: bool,
    irq_raw_curr: bool,
    irq_raw_prev: bool,
}

impl<'a> CountingBus<'a> {
    /// `nmi_prev_asserted_in`/`edge_latched_in` are `Cpu::nmi_prev_asserted`/
    /// `Cpu::nmi_edge_latched` as of just before this instruction — the
    /// initial sample taken here (before any bus op inside this wrapper)
    /// represents "end of cycle 1" (the opcode fetch that already
    /// happened outside), matching a 2-cycle instruction's penultimate
    /// cycle being cycle 1 itself.
    fn new(inner: &'a mut dyn CpuBus, nmi_prev_asserted_in: bool, edge_latched_in: bool) -> Self {
        let nmi_now = inner.nmi_line();
        let irq_now = inner.irq_line();
        let edge_now = edge_latched_in || (!nmi_prev_asserted_in && nmi_now);
        CountingBus {
            inner,
            count: 1,
            nmi_prev_asserted: nmi_now,
            edge_latched_curr: edge_now,
            edge_latched_prev: edge_now,
            irq_raw_curr: irq_now,
            irq_raw_prev: irq_now,
        }
    }

    /// Shift current -> previous, then resample. Called after every real
    /// bus op (nesdev: the edge/level detectors sample "during φ2 of each
    /// CPU cycle").
    fn sample(&mut self) {
        self.edge_latched_prev = self.edge_latched_curr;
        self.irq_raw_prev = self.irq_raw_curr;
        let nmi_now = self.inner.nmi_line();
        let irq_now = self.inner.irq_line();
        if !self.nmi_prev_asserted && nmi_now {
            self.edge_latched_curr = true;
        }
        self.nmi_prev_asserted = nmi_now;
        self.irq_raw_curr = irq_now;
    }
}

impl CpuBus for CountingBus<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        self.count += 1;
        let value = self.inner.read(addr);
        self.sample();
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.count += 1;
        self.inner.write(addr, value);
        self.sample();
    }

    fn nmi_line(&self) -> bool {
        self.inner.nmi_line()
    }

    /// The sticky edge latch, not the level — see [`CpuBus::nmi_edge_pending`]
    /// for why the hijack decision must not ask `nmi_line()`. Seeded from
    /// `Cpu::nmi_edge_latched` at construction, so an edge latched by an
    /// earlier still-unserviced instruction stays visible, and cleared
    /// when `Cpu::step` services it.
    fn nmi_edge_pending(&self) -> bool {
        self.edge_latched_curr
    }

    fn irq_line(&self) -> bool {
        self.inner.irq_line()
    }
}

/// Runs `body` (either normal-opcode `dispatch` or a hardware NMI/IRQ
/// entry sequence) wrapped in a [`CountingBus`], then commits the
/// resulting penultimate-cycle snapshot into `cpu`. This is the one place
/// that decides what `Cpu::step` will do *next call* — every instruction
/// (interrupt entries included, so a *further* pending interrupt chains
/// correctly) goes through here exactly once.
///
/// `i_flag_poll_snapshot` defaults to the **pre**-instruction `I` flag,
/// captured before `body` runs — correct per nesdev for the general case
/// and explicitly correct for `SEI`/`CLI`/`PLP`, which nesdev documents as
/// polling with the *old* `I` ("change the I flag after polling for
/// interrupts"). Two places override this snapshot after it's taken,
/// because they change `I` themselves partway through `body`: `RTI`
/// (polls with the newly-restored `I` — set inside `dispatch`'s `0x40`
/// arm, right after calling `set_p`) and every `BRK`/`IRQ`/`NMI` entry
/// (nesdev's cycle table sets `I` at cycle 6, *before* that same
/// instruction's own penultimate-cycle poll at cycle 6->7 — set inside
/// `finish_interrupt_entry`, right after its own `set_flag(FLAG_I, true)`).
///
/// `nmi_hijack_consumed` (also set from inside `body`, by
/// `finish_interrupt_entry`'s hijack branch) forces both
/// `nmi_edge_latched` and `pending_nmi_after` to `false` regardless of
/// what `cb` observed — the hijack *is* that edge's servicing, and (like
/// the plain-NMI-service clear in `Cpu::step`) a mid-`body` write to
/// `cpu.nmi_edge_latched` can't reach `cb`'s own bookkeeping, so it would
/// otherwise be silently overwritten by the commit below.
fn run_cycled(
    cpu: &mut Cpu,
    bus: &mut dyn CpuBus,
    body: impl FnOnce(&mut Cpu, &mut dyn CpuBus),
) -> u32 {
    cpu.unstable_op = None;
    cpu.jammed = false;
    cpu.i_flag_poll_snapshot = cpu.flag(super::FLAG_I);
    cpu.nmi_hijack_consumed = false;
    cpu.in_interrupt_entry = false;

    let mut cb = CountingBus::new(bus, cpu.nmi_prev_asserted, cpu.nmi_edge_latched);
    body(cpu, &mut cb);

    cpu.nmi_prev_asserted = cb.nmi_prev_asserted;
    cpu.nmi_edge_latched = cb.edge_latched_curr;
    // An interrupt-entry sequence does not poll (see
    // `Cpu::in_interrupt_entry`): the latch above still carries an edge
    // that arrived mid-sequence into the next instruction, but nothing
    // becomes pending *now*, so the handler always gets its first
    // instruction.
    cpu.pending_nmi_after = !cpu.in_interrupt_entry && cb.edge_latched_prev;
    if cpu.nmi_hijack_consumed {
        cpu.nmi_edge_latched = false;
        cpu.pending_nmi_after = false;
    }
    cpu.pending_irq_after = !cpu.in_interrupt_entry && cb.irq_raw_prev && !cpu.i_flag_poll_snapshot;
    cb.count
}

/// Run the instruction `opcode` (already fetched from `PC` by
/// [`Cpu::step`], which is cycle 1) to completion and return the total
/// bus-cycle count including that fetch.
pub(super) fn execute(cpu: &mut Cpu, bus: &mut dyn CpuBus, opcode: u8) -> u32 {
    run_cycled(cpu, bus, |cpu, bus| dispatch(cpu, bus, opcode))
}

/// Hardware NMI entry (nesdev.org/wiki/CPU_interrupts "IRQ/NMI" cycle
/// table): always vectors through `$FFFA`/`$FFFB`, never checks `I`
/// (non-maskable), and is never itself hijacked (nesdev only documents
/// NMI hijacking an in-flight `BRK`/`IRQ`, not the reverse — there's no
/// "which vector" decision for an NMI-triggered entry, so no hijack
/// window is implemented here). [`Cpu::step`] has already performed cycle
/// 1 (the discarded opcode-fetch-shaped read at `PC`, not incrementing
/// `PC`) on the raw bus before calling this, for the same reason
/// `dispatch`'s cycle 1 happens outside its `CountingBus` — see
/// `run_cycled`'s doc.
pub(super) fn nmi_sequence(cpu: &mut Cpu, bus: &mut dyn CpuBus) -> u32 {
    // `Cpu::step` already cleared `nmi_edge_latched` before calling this
    // (see its doc) — `run_cycled` below seeds `cb` from that
    // already-cleared value, so a fresh edge arriving *during* this
    // sequence still latches correctly for a subsequent instruction.
    run_cycled(cpu, bus, |cpu, bus| {
        bus.read(cpu.pc); // cycle 2: discarded, PC increment suppressed
        finish_interrupt_entry(cpu, bus, false, 0xFFFA, false);
    })
}

/// Hardware IRQ entry: vectors through `$FFFE`/`$FFFF` unless hijacked by
/// a pending NMI at the decision point (see `finish_interrupt_entry`).
/// Only reached when `Cpu::step` already found `IRQ` asserted with `I`
/// clear (the level+mask check happened at the *previous* instruction's
/// poll, committed to `Cpu::pending_irq_after` — see `run_cycled`), so
/// this never re-checks `I` itself.
pub(super) fn irq_sequence(cpu: &mut Cpu, bus: &mut dyn CpuBus) -> u32 {
    run_cycled(cpu, bus, |cpu, bus| {
        bus.read(cpu.pc); // cycle 2: discarded, PC increment suppressed
        finish_interrupt_entry(cpu, bus, false, 0xFFFE, true);
    })
}

/// The normal-opcode decode table (moved out of `execute` so both it and
/// the hardware interrupt sequences can share [`run_cycled`]'s
/// `CountingBus` setup/commit).
///
/// ## "flagged in the trace" (EMULATION_CORES.md §2.1)
///
/// There is no trace logger in this ticket's scope — the nestest-format
/// logger is W1-03 (`trace logger in nestest.log format`, FR-DBG-003),
/// which depends on W1-02 (system/bus wiring), two tickets away. The
/// unstable-op arms below set [`Cpu::unstable_op`] instead — the marker a
/// future trace logger consumes; see `ops.rs`'s per-op doc comments for
/// the exact constants and their nesdev/vector citations.
fn dispatch(cpu: &mut Cpu, bus: &mut dyn CpuBus, opcode: u8) {
    match opcode {
        // ---- LDA ----------------------------------------------------
        0xA9 => {
            let v = cpu.fetch(bus);
            cpu.op_lda(v);
        }
        0xA5 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_lda(v);
        }
        0xB5 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_lda(v);
        }
        0xAD => {
            let v = cpu.am_abs_read(bus);
            cpu.op_lda(v);
        }
        0xBD => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_lda(v);
        }
        0xB9 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_lda(v);
        }
        0xA1 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_lda(v);
        }
        0xB1 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_lda(v);
        }

        // ---- LDX ----------------------------------------------------
        0xA2 => {
            let v = cpu.fetch(bus);
            cpu.op_ldx(v);
        }
        0xA6 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_ldx(v);
        }
        0xB6 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.y);
            let v = bus.read(addr);
            cpu.op_ldx(v);
        }
        0xAE => {
            let v = cpu.am_abs_read(bus);
            cpu.op_ldx(v);
        }
        0xBE => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_ldx(v);
        }

        // ---- LDY ----------------------------------------------------
        0xA0 => {
            let v = cpu.fetch(bus);
            cpu.op_ldy(v);
        }
        0xA4 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_ldy(v);
        }
        0xB4 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_ldy(v);
        }
        0xAC => {
            let v = cpu.am_abs_read(bus);
            cpu.op_ldy(v);
        }
        0xBC => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_ldy(v);
        }

        // ---- STA ----------------------------------------------------
        0x85 => {
            let addr = cpu.am_zp_addr(bus);
            bus.write(addr, cpu.a);
        }
        0x95 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            bus.write(addr, cpu.a);
        }
        0x8D => {
            let addr = cpu.am_abs_addr(bus);
            bus.write(addr, cpu.a);
        }
        0x9D => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            bus.write(addr, cpu.a);
        }
        0x99 => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.y);
            bus.write(addr, cpu.a);
        }
        0x81 => {
            let addr = cpu.am_indx_addr(bus);
            bus.write(addr, cpu.a);
        }
        0x91 => {
            let addr = cpu.am_indy_addr_slow(bus);
            bus.write(addr, cpu.a);
        }

        // ---- STX / STY ------------------------------------------------
        0x86 => {
            let addr = cpu.am_zp_addr(bus);
            bus.write(addr, cpu.x);
        }
        0x96 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.y);
            bus.write(addr, cpu.x);
        }
        0x8E => {
            let addr = cpu.am_abs_addr(bus);
            bus.write(addr, cpu.x);
        }
        0x84 => {
            let addr = cpu.am_zp_addr(bus);
            bus.write(addr, cpu.y);
        }
        0x94 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            bus.write(addr, cpu.y);
        }
        0x8C => {
            let addr = cpu.am_abs_addr(bus);
            bus.write(addr, cpu.y);
        }

        // ---- ADC ----------------------------------------------------
        0x69 => {
            let v = cpu.fetch(bus);
            cpu.op_adc(v);
        }
        0x65 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_adc(v);
        }
        0x75 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_adc(v);
        }
        0x6D => {
            let v = cpu.am_abs_read(bus);
            cpu.op_adc(v);
        }
        0x7D => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_adc(v);
        }
        0x79 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_adc(v);
        }
        0x61 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_adc(v);
        }
        0x71 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_adc(v);
        }

        // ---- SBC ----------------------------------------------------
        0xE9 => {
            let v = cpu.fetch(bus);
            cpu.op_sbc(v);
        }
        0xE5 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_sbc(v);
        }
        0xF5 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_sbc(v);
        }
        0xED => {
            let v = cpu.am_abs_read(bus);
            cpu.op_sbc(v);
        }
        0xFD => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_sbc(v);
        }
        0xF9 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_sbc(v);
        }
        0xE1 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_sbc(v);
        }
        0xF1 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_sbc(v);
        }

        // ---- CMP ----------------------------------------------------
        0xC9 => {
            let v = cpu.fetch(bus);
            cpu.op_cmp(v);
        }
        0xC5 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_cmp(v);
        }
        0xD5 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_cmp(v);
        }
        0xCD => {
            let v = cpu.am_abs_read(bus);
            cpu.op_cmp(v);
        }
        0xDD => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_cmp(v);
        }
        0xD9 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_cmp(v);
        }
        0xC1 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_cmp(v);
        }
        0xD1 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_cmp(v);
        }

        // ---- CPX / CPY ------------------------------------------------
        0xE0 => {
            let v = cpu.fetch(bus);
            cpu.op_cpx(v);
        }
        0xE4 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_cpx(v);
        }
        0xEC => {
            let v = cpu.am_abs_read(bus);
            cpu.op_cpx(v);
        }
        0xC0 => {
            let v = cpu.fetch(bus);
            cpu.op_cpy(v);
        }
        0xC4 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_cpy(v);
        }
        0xCC => {
            let v = cpu.am_abs_read(bus);
            cpu.op_cpy(v);
        }

        // ---- AND ----------------------------------------------------
        0x29 => {
            let v = cpu.fetch(bus);
            cpu.op_and(v);
        }
        0x25 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_and(v);
        }
        0x35 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_and(v);
        }
        0x2D => {
            let v = cpu.am_abs_read(bus);
            cpu.op_and(v);
        }
        0x3D => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_and(v);
        }
        0x39 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_and(v);
        }
        0x21 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_and(v);
        }
        0x31 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_and(v);
        }

        // ---- ORA ----------------------------------------------------
        0x09 => {
            let v = cpu.fetch(bus);
            cpu.op_ora(v);
        }
        0x05 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_ora(v);
        }
        0x15 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_ora(v);
        }
        0x0D => {
            let v = cpu.am_abs_read(bus);
            cpu.op_ora(v);
        }
        0x1D => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_ora(v);
        }
        0x19 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_ora(v);
        }
        0x01 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_ora(v);
        }
        0x11 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_ora(v);
        }

        // ---- EOR ----------------------------------------------------
        0x49 => {
            let v = cpu.fetch(bus);
            cpu.op_eor(v);
        }
        0x45 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_eor(v);
        }
        0x55 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_eor(v);
        }
        0x4D => {
            let v = cpu.am_abs_read(bus);
            cpu.op_eor(v);
        }
        0x5D => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_eor(v);
        }
        0x59 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_eor(v);
        }
        0x41 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_eor(v);
        }
        0x51 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_eor(v);
        }

        // ---- BIT ----------------------------------------------------
        0x24 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_bit(v);
        }
        0x2C => {
            let v = cpu.am_abs_read(bus);
            cpu.op_bit(v);
        }

        // ---- ASL / LSR / ROL / ROR (accumulator + RMW memory) --------
        0x0A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.op_asl(cpu.a);
        }
        0x06 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_asl);
        }
        0x16 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_asl);
        }
        0x0E => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_asl);
        }
        0x1E => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_asl);
        }

        0x4A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.op_lsr(cpu.a);
        }
        0x46 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_lsr);
        }
        0x56 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_lsr);
        }
        0x4E => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_lsr);
        }
        0x5E => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_lsr);
        }

        0x2A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.op_rol(cpu.a);
        }
        0x26 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rol);
        }
        0x36 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_rol);
        }
        0x2E => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rol);
        }
        0x3E => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_rol);
        }

        0x6A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.op_ror(cpu.a);
        }
        0x66 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_ror);
        }
        0x76 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_ror);
        }
        0x6E => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_ror);
        }
        0x7E => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_ror);
        }

        // ---- INC / DEC (memory) ---------------------------------------
        0xE6 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_inc);
        }
        0xF6 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_inc);
        }
        0xEE => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_inc);
        }
        0xFE => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_inc);
        }
        0xC6 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_dec);
        }
        0xD6 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_dec);
        }
        0xCE => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_dec);
        }
        0xDE => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_dec);
        }

        // ---- register increment/decrement (implied) --------------------
        0xE8 => {
            cpu.implied_dummy_read(bus);
            cpu.x = cpu.x.wrapping_add(1);
            cpu.set_nz(cpu.x);
        }
        0xC8 => {
            cpu.implied_dummy_read(bus);
            cpu.y = cpu.y.wrapping_add(1);
            cpu.set_nz(cpu.y);
        }
        0xCA => {
            cpu.implied_dummy_read(bus);
            cpu.x = cpu.x.wrapping_sub(1);
            cpu.set_nz(cpu.x);
        }
        0x88 => {
            cpu.implied_dummy_read(bus);
            cpu.y = cpu.y.wrapping_sub(1);
            cpu.set_nz(cpu.y);
        }

        // ---- register transfers (implied) -------------------------------
        0xAA => {
            cpu.implied_dummy_read(bus);
            cpu.x = cpu.a;
            cpu.set_nz(cpu.x);
        }
        0xA8 => {
            cpu.implied_dummy_read(bus);
            cpu.y = cpu.a;
            cpu.set_nz(cpu.y);
        }
        0x8A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.x;
            cpu.set_nz(cpu.a);
        }
        0x98 => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.y;
            cpu.set_nz(cpu.a);
        }
        0xBA => {
            cpu.implied_dummy_read(bus);
            cpu.x = cpu.s;
            cpu.set_nz(cpu.x);
        }
        0x9A => {
            // TXS does not touch any flag (nesdev.org/6502_cpu.txt notes
            // TXS "is not an arithmetic operation").
            cpu.implied_dummy_read(bus);
            cpu.s = cpu.x;
        }

        // ---- flag ops (implied) ------------------------------------
        0x18 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(FLAG_C, false);
        }
        0x38 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(FLAG_C, true);
        }
        0x58 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(super::FLAG_I, false);
        }
        0x78 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(super::FLAG_I, true);
        }
        0xB8 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(FLAG_V, false);
        }
        0xD8 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(super::FLAG_D, false);
        }
        0xF8 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(super::FLAG_D, true);
        }

        // ---- NOP (the one official NOP) --------------------------------
        0xEA => {
            cpu.implied_dummy_read(bus);
        }

        // ---- branches -------------------------------------------------
        0x10 => branch(cpu, bus, !cpu.flag(FLAG_N)),
        0x30 => branch(cpu, bus, cpu.flag(FLAG_N)),
        0x50 => branch(cpu, bus, !cpu.flag(FLAG_V)),
        0x70 => branch(cpu, bus, cpu.flag(FLAG_V)),
        0x90 => branch(cpu, bus, !cpu.flag(FLAG_C)),
        0xB0 => branch(cpu, bus, cpu.flag(FLAG_C)),
        0xD0 => branch(cpu, bus, !cpu.flag(super::FLAG_Z)),
        0xF0 => branch(cpu, bus, cpu.flag(super::FLAG_Z)),

        // ---- jumps / subroutine / stack-frame control -------------------
        0x4C => {
            cpu.pc = cpu.am_abs_addr(bus);
        }
        0x6C => jmp_indirect(cpu, bus),
        0x20 => jsr(cpu, bus),
        0x60 => rts(cpu, bus),
        0x40 => {
            rti(cpu, bus);
            // nesdev.org/wiki/CPU_interrupts: RTI restores I *before*
            // polling for interrupts, unlike SEI/CLI/PLP (which poll
            // using the pre-instruction I — `run_cycled`'s default,
            // correct for them unmodified). Re-snapshot here so
            // `run_cycled`'s commit uses the newly-restored value.
            cpu.i_flag_poll_snapshot = cpu.flag(super::FLAG_I);
        }
        0x00 => brk(cpu, bus),

        // ---- stack ------------------------------------------------------
        0x48 => {
            cpu.implied_dummy_read(bus);
            cpu.push(bus, cpu.a);
        }
        0x08 => {
            cpu.implied_dummy_read(bus);
            let pushed = cpu.p | FLAG_B;
            cpu.push(bus, pushed);
        }
        0x68 => {
            let v = pull(cpu, bus);
            cpu.a = v;
            cpu.set_nz(v);
        }
        0x28 => {
            let v = pull(cpu, bus);
            cpu.set_p(v);
        }

        // ==== unofficial/illegal opcodes (ticket W1-01b) =================
        // ---- SLO / RLA / SRE / RRA (stable RMW combo ops) --------------
        0x03 => {
            let addr = cpu.am_indx_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_slo);
        }
        0x07 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_slo);
        }
        0x0F => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_slo);
        }
        0x13 => {
            let addr = cpu.am_indy_addr_slow(bus);
            cpu.rmw(bus, addr, Cpu::op_slo);
        }
        0x17 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_slo);
        }
        0x1B => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.y);
            cpu.rmw(bus, addr, Cpu::op_slo);
        }
        0x1F => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_slo);
        }

        0x23 => {
            let addr = cpu.am_indx_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rla);
        }
        0x27 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rla);
        }
        0x2F => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rla);
        }
        0x33 => {
            let addr = cpu.am_indy_addr_slow(bus);
            cpu.rmw(bus, addr, Cpu::op_rla);
        }
        0x37 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_rla);
        }
        0x3B => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.y);
            cpu.rmw(bus, addr, Cpu::op_rla);
        }
        0x3F => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_rla);
        }

        0x43 => {
            let addr = cpu.am_indx_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_sre);
        }
        0x47 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_sre);
        }
        0x4F => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_sre);
        }
        0x53 => {
            let addr = cpu.am_indy_addr_slow(bus);
            cpu.rmw(bus, addr, Cpu::op_sre);
        }
        0x57 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_sre);
        }
        0x5B => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.y);
            cpu.rmw(bus, addr, Cpu::op_sre);
        }
        0x5F => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_sre);
        }

        0x63 => {
            let addr = cpu.am_indx_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rra);
        }
        0x67 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rra);
        }
        0x6F => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rra);
        }
        0x73 => {
            let addr = cpu.am_indy_addr_slow(bus);
            cpu.rmw(bus, addr, Cpu::op_rra);
        }
        0x77 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_rra);
        }
        0x7B => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.y);
            cpu.rmw(bus, addr, Cpu::op_rra);
        }
        0x7F => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_rra);
        }

        // ---- SAX (store, stable) ---------------------------------------
        0x83 => {
            let addr = cpu.am_indx_addr(bus);
            bus.write(addr, cpu.a & cpu.x);
        }
        0x87 => {
            let addr = cpu.am_zp_addr(bus);
            bus.write(addr, cpu.a & cpu.x);
        }
        0x8F => {
            let addr = cpu.am_abs_addr(bus);
            bus.write(addr, cpu.a & cpu.x);
        }
        0x97 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.y);
            bus.write(addr, cpu.a & cpu.x);
        }

        // ---- LAX (load, stable) -----------------------------------------
        0xA3 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_lax(v);
        }
        0xA7 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_lax(v);
        }
        0xAF => {
            let v = cpu.am_abs_read(bus);
            cpu.op_lax(v);
        }
        0xB3 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_lax(v);
        }
        0xB7 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.y);
            let v = bus.read(addr);
            cpu.op_lax(v);
        }
        0xBF => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_lax(v);
        }

        // ---- DCP / ISC (stable RMW combo ops) --------------------------
        0xC3 => {
            let addr = cpu.am_indx_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_dcp);
        }
        0xC7 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_dcp);
        }
        0xCF => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_dcp);
        }
        0xD3 => {
            let addr = cpu.am_indy_addr_slow(bus);
            cpu.rmw(bus, addr, Cpu::op_dcp);
        }
        0xD7 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_dcp);
        }
        0xDB => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.y);
            cpu.rmw(bus, addr, Cpu::op_dcp);
        }
        0xDF => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_dcp);
        }

        0xE3 => {
            let addr = cpu.am_indx_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_isc);
        }
        0xE7 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_isc);
        }
        0xEF => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_isc);
        }
        0xF3 => {
            let addr = cpu.am_indy_addr_slow(bus);
            cpu.rmw(bus, addr, Cpu::op_isc);
        }
        0xF7 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_isc);
        }
        0xFB => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.y);
            cpu.rmw(bus, addr, Cpu::op_isc);
        }
        0xFF => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_isc);
        }

        // ---- stable immediate combo ops --------------------------------
        0x0B | 0x2B => {
            let v = cpu.fetch(bus);
            cpu.op_anc(v);
        }
        0x4B => {
            let v = cpu.fetch(bus);
            cpu.op_alr(v);
        }
        0x6B => {
            let v = cpu.fetch(bus);
            cpu.op_arr(v);
        }
        0xCB => {
            let v = cpu.fetch(bus);
            cpu.op_sbx(v);
        }
        0xEB => {
            // Undocumented duplicate of the official $E9 SBC.
            let v = cpu.fetch(bus);
            cpu.op_sbc(v);
        }

        // ---- unstable ops (magic constants — see ops.rs citations) -----
        0x8B => {
            let v = cpu.fetch(bus);
            cpu.op_ane(v);
        }
        0xAB => {
            let v = cpu.fetch(bus);
            cpu.op_lxa(v);
        }
        0xBB => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_las(v);
        }
        0x93 => {
            let (addr, hi, crossed) = cpu.am_indy_unstable(bus);
            let value = cpu.op_sha_value(hi);
            let final_addr = if crossed {
                u16::from_le_bytes([addr as u8, value])
            } else {
                addr
            };
            bus.write(final_addr, value);
        }
        0x9F => {
            let (addr, hi, crossed) = cpu.am_absi_unstable(bus, cpu.y);
            let value = cpu.op_sha_value(hi);
            let final_addr = if crossed {
                u16::from_le_bytes([addr as u8, value])
            } else {
                addr
            };
            bus.write(final_addr, value);
        }
        0x9E => {
            let (addr, hi, crossed) = cpu.am_absi_unstable(bus, cpu.y);
            let value = cpu.op_shx_value(hi);
            let final_addr = if crossed {
                u16::from_le_bytes([addr as u8, value])
            } else {
                addr
            };
            bus.write(final_addr, value);
        }
        0x9C => {
            let (addr, hi, crossed) = cpu.am_absi_unstable(bus, cpu.x);
            let value = cpu.op_shy_value(hi);
            let final_addr = if crossed {
                u16::from_le_bytes([addr as u8, value])
            } else {
                addr
            };
            bus.write(final_addr, value);
        }
        0x9B => {
            let (addr, hi, crossed) = cpu.am_absi_unstable(bus, cpu.y);
            let value = cpu.op_tas_value(hi);
            let final_addr = if crossed {
                u16::from_le_bytes([addr as u8, value])
            } else {
                addr
            };
            bus.write(final_addr, value);
        }

        // ---- NOP / SKB / IGN variants (undocumented, no side effect) ---
        0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA => {
            cpu.implied_dummy_read(bus);
        }
        0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => {
            let _ = cpu.fetch(bus);
        }
        0x04 | 0x44 | 0x64 => {
            let _ = cpu.am_zp_read(bus);
        }
        0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let _ = bus.read(addr);
        }
        0x0C => {
            let _ = cpu.am_abs_read(bus);
        }
        0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => {
            let _ = cpu.am_absi_read(bus, cpu.x);
        }

        // ---- KIL/JAM ----------------------------------------------------
        0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => {
            jam(cpu, bus)
        } // No `_` wildcard: every one of the 256 possible opcode bytes has
          // an explicit arm above, and rustc's own exhaustiveness checker
          // proves it (a wildcard arm here is rejected as provably
          // unreachable) — a stronger, compile-time version of the coverage
          // check `tests::opcode_table::dispatch_covers_all_256_opcodes`
          // also performs at the byte-table level (OFFICIAL_OPCODES ∪
          // UNOFFICIAL_OPCODES == 0..=255, no gaps/overlap).
    }
}

/// Relative-branch bus sequence (nesdev.org/6502_cpu.txt "Relative
/// addressing"): fetch the offset, then — only if `taken` — a dummy read
/// at the not-yet-adjusted PC, and (only if that crosses a page) a second
/// dummy read at the wrong-page address before committing the real target.
fn branch(cpu: &mut Cpu, bus: &mut dyn CpuBus, taken: bool) {
    let offset = cpu.fetch(bus) as i8;
    if taken {
        bus.read(cpu.pc);
        let old_pc = cpu.pc;
        let new_pc = old_pc.wrapping_add(offset as i16 as u16);
        if new_pc & 0xFF00 != old_pc & 0xFF00 {
            let wrong = (old_pc & 0xFF00) | (new_pc & 0x00FF);
            bus.read(wrong);
        }
        cpu.pc = new_pc;
    }
}

/// `JMP (indirect)`: reproduces the famous page-wrap bug — if the pointer's
/// low byte is `$FF`, the high byte is fetched from `pointer & $FF00`
/// rather than `pointer + 1` (nesdev.org/6502_cpu.txt "Indirect addressing
/// modes do not handle page boundary crossing at all").
fn jmp_indirect(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    let ptr = cpu.am_abs_addr(bus);
    let lo = bus.read(ptr);
    let hi_addr = (ptr & 0xFF00) | (ptr.wrapping_add(1) & 0x00FF);
    let hi = bus.read(hi_addr);
    cpu.pc = u16::from_le_bytes([lo, hi]);
}

/// `JSR` (nesdev.org/6502_cpu.txt "JSR"): note the low address byte is
/// fetched *before* the internal stack cycle and the high byte *after*
/// both pushes — `PC` at push time equals the address of the instruction's
/// own last byte (the classic "JSR pushes return-address-minus-one").
fn jsr(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    let lo = cpu.fetch(bus);
    bus.read(cpu.stack_addr()); // cycle 3: "internal operation"
    let return_addr = cpu.pc;
    cpu.push(bus, (return_addr >> 8) as u8);
    cpu.push(bus, return_addr as u8);
    let hi = bus.read(cpu.pc);
    cpu.pc = u16::from_le_bytes([lo, hi]);
}

fn rts(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    cpu.implied_dummy_read(bus);
    bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let lo = bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let hi = bus.read(cpu.stack_addr());
    let addr = u16::from_le_bytes([lo, hi]);
    bus.read(addr);
    cpu.pc = addr.wrapping_add(1);
}

fn rti(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    cpu.implied_dummy_read(bus);
    bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let p = bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let lo = bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let hi = bus.read(cpu.stack_addr());
    cpu.set_p(p);
    cpu.pc = u16::from_le_bytes([lo, hi]);
}

/// `BRK` (nesdev.org/wiki/CPU_interrupts "BRK" cycle table): cycles 1-2
/// (opcode fetch, padding-byte fetch — both increment `PC`, unlike the
/// hardware-forced entries) happen here; [`finish_interrupt_entry`] does
/// the shared cycles 3-7, including NMI hijacking (a software `BRK` is
/// just as hijackable as a hardware `IRQ` — nesdev's hijack diagram marks
/// all four of `BRK`'s own first four cycles as the hijack window).
fn brk(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    bus.read(cpu.pc); // cycle 2: the padding byte after BRK's opcode
    cpu.pc = cpu.pc.wrapping_add(1);
    finish_interrupt_entry(cpu, bus, true, 0xFFFE, true);
}

/// Shared cycles 3 onward of the interrupt-entry sequence — identical for
/// software `BRK` and the hardware NMI/IRQ entries from here on
/// (nesdev.org/wiki/CPU_interrupts "BRK" / "IRQ/NMI" cycle tables:
/// cycle 3 push `PCH`, cycle 4 push `PCL`, [decision point], cycle 5 push
/// `P`, cycle 6 fetch vector-lo + set `I`, cycle 7 fetch vector-hi).
///
/// `push_b_set` distinguishes software `BRK` (`B` pushed set) from a
/// hardware-forced entry (`B` pushed clear) — nesdev: "push P on stack
/// (with B flag set)" vs "...(with B flag *clear*)". Since [`Cpu::p`]
/// always keeps its own `B` bit at 0 (module doc on `FLAG_B`), "clear" is
/// just `cpu.p` unmodified.
///
/// ## Hijack model
///
/// At the documented decision point (after `PCL` is pushed, before `P` is
/// pushed), NMI hijacks a hijackable (`BRK`/`IRQ`) sequence if an NMI
/// **edge has been latched and not yet serviced** as of that point —
/// `bus.nmi_edge_pending()`, which for the real `CountingBus` is its
/// sticky edge latch sampled through cycle 4, seeded from
/// `cpu.nmi_edge_latched` so an edge from an earlier unserviced
/// instruction still counts.
///
/// ## Why not `bus.nmi_line()` (ticket W2-20 — this was the bug)
///
/// Until W2-20 this asked whether the line was *currently asserted*,
/// justified as a "hold or already-latched" model. That is wrong on the
/// NES specifically, because the PPU holds NMI asserted for the whole of
/// vblank — until `$2002` is read or `PPUCTRL` bit 7 is cleared — so the
/// level stays true long after the edge it represents has been serviced.
/// Any `BRK` executed in the remainder of vblank was therefore hijacked
/// by an interrupt that had already run.
///
/// blargg's `cpu_interrupts_v2` `2-nmi_and_brk` is built to catch this:
/// its NMI handler ends `bit SNDCHN`/`rti`, never touching `$2002`, and
/// its first three delay steps put the NMI *before* the `BRK`. Under the
/// level test those three ran the NMI, returned, then had the following
/// `BRK` hijacked — the handler ran a second time and overwrote
/// `nmi_flag` with `BRK`'s own B-set push, so the ROM's expected
/// `27/26/26  36` printed as `36  00` and its 5-clock hijack window
/// measured 8 clocks wide.
///
/// The sub-cycle caveat the old note raised still stands and is
/// unrelated: this engine samples once per bus op, so a pulse that both
/// asserts and releases inside a single cycle is invisible. What changed
/// is that a *serviced* edge no longer keeps hijacking.
fn finish_interrupt_entry(
    cpu: &mut Cpu,
    bus: &mut dyn CpuBus,
    push_b_set: bool,
    default_vector: u16,
    hijackable: bool,
) {
    // nesdev: an interrupt sequence performs no interrupt polling of its
    // own — `run_cycled` reads this to suppress the poll it would
    // otherwise commit (see `Cpu::in_interrupt_entry`). Set here rather
    // than in the three callers so `BRK`, `IRQ` and `NMI` cannot drift
    // apart, since all three share these cycles.
    cpu.in_interrupt_entry = true;
    cpu.push(bus, (cpu.pc >> 8) as u8); // push PCH
    cpu.push(bus, cpu.pc as u8); // push PCL
                                 // *** decision point: NMI asserted up to here hijacks the vector ***
    let vector = if hijackable && (bus.nmi_edge_pending() || cpu.nmi_edge_latched) {
        // This hijack *is* that edge's servicing — `nmi_hijack_consumed`
        // tells `run_cycled`'s post-instruction commit to force
        // `nmi_edge_latched`/`pending_nmi_after` false (a direct write
        // here can't reach `cb`'s bookkeeping — see `run_cycled`'s doc).
        cpu.nmi_hijack_consumed = true;
        0xFFFA
    } else {
        default_vector
    };
    let pushed = if push_b_set { cpu.p | FLAG_B } else { cpu.p };
    cpu.push(bus, pushed);
    let lo = bus.read(vector);
    cpu.set_flag(super::FLAG_I, true);
    // nesdev's cycle table sets I *at* cycle 6, and this instruction's own
    // penultimate-cycle poll (cycle 6 -> 7, committed by `run_cycled`)
    // must see it already set — re-snapshot here (same override pattern
    // `dispatch`'s `0x40`/RTI arm uses) so an IRQ line held asserted
    // through entry doesn't immediately re-fire before the handler runs
    // even one instruction.
    cpu.i_flag_poll_snapshot = cpu.flag(super::FLAG_I);
    let hi = bus.read(vector.wrapping_add(1));
    cpu.pc = u16::from_le_bytes([lo, hi]);
}

/// The `PLA`/`PLP` stack-read sequence: a dummy read at the not-yet-bumped
/// `S`, then the pulled byte at `S+1` (nesdev.org/6502_cpu.txt "PLA, PLP").
fn pull(cpu: &mut Cpu, bus: &mut dyn CpuBus) -> u8 {
    cpu.implied_dummy_read(bus);
    bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    bus.read(cpu.stack_addr())
}

/// `KIL`/`JAM`/`HLT`: the 12 opcode bytes that lock the NMOS 6502's
/// instruction-fetch state machine — real hardware never recovers without
/// a reset (nesdev.org/wiki/CPU_unofficial_opcodes notes some unofficial
/// opcodes "halt the CPU until reset" without detailing which bytes or the
/// exact bus pattern). The precise "endlessly re-reads the byte after the
/// `JAM` opcode, PC never advancing" behavior modeled below comes directly
/// from the nes6502 SingleStepTests vectors (every sampled `JAM` case:
/// both post-opcode reads target the same address, `final.pc` equals
/// `initial.pc`), not a nesdev citation — see `cpu/exec.rs` module doc's
/// general note on where the vectors substitute for nesdev prose.
///
/// ## Representation (ticket W1-01b decision)
///
/// This engine executes one whole instruction per [`Cpu::step`] call, so a
/// literal infinite bus-read loop isn't representable inside one call.
/// Instead: fetch (already done, `PC` now points one past the `JAM`
/// byte), then two reads at that address (matching the vector evidence
/// above), then **reset `PC` back to the `JAM` opcode's own address**. The
/// next `Cpu::step` call therefore re-fetches the same `JAM` opcode and
/// replays the exact same 3-cycle bus trace forever — an accurate
/// emulation of "stuck re-reading the same address indefinitely" without
/// an actual unbounded loop inside one call. [`Cpu::jammed`] additionally
/// flags this for a future debugger; nothing in `cpu/**` reads it to skip
/// bus behavior (deliberately — see `cpu/mod.rs`'s doc on that field).
///
/// ## Known divergence from real hardware: interrupts don't recover a JAM
///
/// On real silicon a jammed CPU never completes an instruction, so it
/// never reaches a penultimate-cycle poll — only `RESET` recovers it, not
/// `NMI`/`IRQ`. In this model, `jam` *does* complete as an ordinary
/// 3-cycle instruction, `run_cycled` commits its (never-set, since `jam`
/// touches no interrupt state) poll normally, and a pending NMI would
/// vector out of the "jam" on the next `Cpu::step` the same as it would
/// after any other instruction. This is a known, undocumented-by-design
/// simplification (not gating the poll on `Cpu::jammed` was a deliberate
/// choice — an untested edge case bought at the cost of extra branching);
/// flagged here rather than silently shipped.
fn jam(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    bus.read(cpu.pc);
    bus.read(cpu.pc);
    cpu.pc = cpu.pc.wrapping_sub(1);
    cpu.jammed = true;
}
