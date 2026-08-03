//! Ricoh 2A03 CPU core — official opcodes + addressing modes only.
//!
//! The 2A03 is a stock NMOS 6502 with the BCD (decimal) ALU mode disabled:
//! the `D` flag still exists and is settable, but `ADC`/`SBC` never apply
//! the decimal fixups regardless of its state (nesdev.org/wiki/CPU, "2A03
//! ... lacks decimal mode"). Everything else — registers, flags, addressing
//! modes, per-cycle bus behavior including dummy reads/writes — matches the
//! NMOS 6502 exactly (EMULATION_CORES.md §2.1).
//!
//! Scope (ticket W1-01a): the 256 **official** opcodes, all addressing
//! modes, and the cycle-accurate bus trace they produce. Unofficial/illegal
//! opcodes and hardware interrupt polling (NMI edge, IRQ level, BRK/IRQ
//! hijacking) are ticket W1-01b — [`Cpu::step`] panics with a named-opcode
//! message if it ever decodes one of those bytes, which is a deliberate
//! seam, not a bug.
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

/// The 6502/2A03 register file plus the cycle-stepped instruction
/// dispatcher. Holds no bus/memory state of its own — every access crosses
/// [`CpuBus`].
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
    /// Interrupt polling (NMI/IRQ) is out of scope for this ticket (see
    /// module doc) — this only ever executes the instruction already at
    /// `PC`.
    pub fn step(&mut self, bus: &mut dyn CpuBus) -> u32 {
        let opcode = self.fetch(bus);
        exec::execute(self, bus, opcode)
    }
}
