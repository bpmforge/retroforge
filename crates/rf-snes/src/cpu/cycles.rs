//! Per-instruction internal-cycle accounting for the 65C816 (ticket
//! W14-39; WDC W65C816S datasheet §instruction timing, cross-checked
//! against the SingleStepTests `65816` vectors' `cycles` arrays).
//!
//! ## Why this is a separate pre-pass rather than a change to `ops::execute`
//!
//! `speed::AccessCost` prices bus accesses; it has no way to price a
//! cycle that touches no address at all. Threading a running total
//! through every opcode arm in `ops.rs` — a file already verified against
//! 5,080,000 vectors — risked disturbing behaviour that is not this
//! ticket's to touch. Instead, [`internal_cycles`] runs *before*
//! `ops::execute`, right after the opcode byte is fetched, and works out
//! the answer by **peeking** the same operand bytes `ops::execute` is
//! about to fetch for real. `CpuBus::peek` is side-effect-free and
//! untimed by contract (see its doc on [`super::CpuBus`]), so this can
//! read ahead without perturbing the machine or double-charging a cycle
//! `AccessCost` will charge again for real a moment later.
//!
//! This works because every fact the penalties below depend on is
//! decided *before* the instruction runs: `D`, `X`, `Y`, `E`, `M`/`X`
//! width and the flags a branch tests are all pre-instruction state, and
//! the operand bytes are fixed in ROM/RAM regardless of whether they are
//! read once (here) or twice (again in `ops::execute`).
//!
//! ## The oracle-derived model
//!
//! Reverse-engineered from a histogram of `cycles.len() - accesses`
//! across all 5,080,000 SingleStepTests cases (`docs/TESTING.md`'s
//! W14-39 section carries the summary and the per-class counts), then
//! cross-checked against the WDC datasheet's penalty notes. The
//! penalties are additive and each has a single cause:
//!
//! * **DP low byte nonzero, +1.** Any direct-page-relative effective
//!   address (`dp`, `dp,X`, `dp,Y`, `(dp)`, `(dp,X)`, `(dp),Y`, `[dp]`,
//!   `[dp],Y`, `PEI`) costs one more internal cycle when `D`'s low byte
//!   is nonzero — computing the address needs an extra half-adder pass.
//!   Long-indirect forms (`[dp]`, `[dp],Y`) get this and *nothing else*:
//!   their pointer is a full 24-bit value, so the indexed form never
//!   carries the page-cross surcharge below.
//! * **Direct-page indexing, +1 fixed.** `dp,X`, `dp,Y` and `(dp,X)`
//!   (indexed *before* the indirection) always cost one more cycle than
//!   their non-indexed sibling, DP-penalty aside — the index add itself
//!   is the extra cycle, not a maybe.
//! * **Indexed-absolute / `(dp),Y` page cross.** `abs,X`, `abs,Y` and
//!   `(dp),Y` (indexed *after* the indirection) add one cycle if the
//!   index addition carries out of the low byte — **but only for a
//!   read**. A store or a read-modify-write through the same addressing
//!   mode always pays that cycle, crossed or not, because the CPU must
//!   have the correct effective address before it can write, and cannot
//!   defer the fix-up the way a read can. `abs,X`/`abs,Y` writes are the
//!   flat "+1 fixed" case; `(dp),Y` writes add the DP penalty as well.
//! * **RMW's extra cycle, +1 fixed.** Every read-modify-write through
//!   memory (`ASL`/`LSR`/`ROL`/`ROR`/`INC`/`DEC`/`TRB`/`TSB`) spends one
//!   more cycle than an equivalent read: the modify step itself. The
//!   accumulator form of each has no address at all and is priced as an
//!   ordinary one-byte implied instruction instead (+1).
//! * **Branch taken, +1; taken and crossing a page in emulation mode,
//!   +1 more.** `BRA`/`BRL` are unconditional (`BRL` never pays the
//!   crossing surcharge — it is a 16-bit relative jump, not the 8-bit
//!   form the surcharge exists for).
//! * **Stack-relative addressing, +1 fixed; `(sr,S),Y`, +2 fixed.**
//!   Neither depends on `D`, width or crossing — SP-relative addressing
//!   has no page concept.
//! * **A one-byte implied/register instruction is never fewer than 2
//!   cycles** (the opcode fetch plus one internal cycle) — `NOP`, flag
//!   ops, transfers, `INX`-family, pushes, `XCE`, `REP`/`SEP`. Pulls cost
//!   one more than pushes (+2 vs +1): hardware spends a throwaway read at
//!   the old stack top before incrementing `S`, which a push never needs.
//! * **Fixed per-opcode costs** for everything addressing doesn't touch:
//!   jumps, calls, returns, interrupts, `WAI`/`STP`, `PEA`/`PEI`/`PER`.
//!   Immediate, plain absolute, absolute long and long-indexed modes
//!   contribute **zero** — their extra bytes for 16-bit operands are
//!   already bus accesses `AccessCost` prices, not internal cycles.
//!
//! ## What is deliberately NOT vector-pinned
//!
//! `MVN`/`MVP` (`$54`/`$44`) are excluded from the vector oracle itself
//! (`cpu/tests/vectors.rs`'s `EXCLUDED`, unchanged by this ticket — the
//! SingleStepTests cases cut a block move off mid-iteration). This module
//! still charges them a documented **2** internal cycles per byte moved:
//! fullsnes ("SNES Reset/Interrupts/Timing" table entry, and separately
//! "65816 MVN/MVP") gives 7 cycles per iteration; `ops::execute`'s
//! `0x54`/`0x44` arm re-fetches the opcode and both bank bytes on every
//! iteration (PC rewinds by 3) and does one read and one write, which is
//! 5 bus accesses — 7 minus 5 is 2. Not oracle-checked, but not a guess
//! either.

use super::addressing as am;
use super::{Cpu, CpuBus};

fn dl_penalty(cpu: &Cpu) -> u8 {
    u8::from(cpu.d & 0x00FF != 0)
}

/// Does adding `index` to `offset` carry out of the low byte? The 6502/
/// 65816 "page boundary crossed" test, applied uniformly to whatever
/// value is actually in the index register — which is why this needs no
/// separate 8-/16-bit-index case: an 8-bit index register already reads
/// as 0-255, so the same 16-bit comparison is correct either width.
fn crosses(offset: u16, index: u16) -> bool {
    (offset & 0xFF00) != (offset.wrapping_add(index) & 0xFF00)
}

/// Peek one byte at `PC + offset` in the *current* program bank, without
/// consuming it — the non-destructive twin of [`Cpu::fetch8`]. `PC` has
/// already advanced past the opcode byte by the time this runs, so
/// `offset` is relative to the first operand byte.
fn peek_operand8(cpu: &Cpu, bus: &dyn CpuBus, offset: u16) -> u8 {
    let pc = cpu.pc.wrapping_add(offset);
    bus.peek(am::bank(cpu.pbr, pc))
}

fn peek_operand16(cpu: &Cpu, bus: &dyn CpuBus, offset: u16) -> u16 {
    let lo = peek_operand8(cpu, bus, offset);
    let hi = peek_operand8(cpu, bus, offset.wrapping_add(1));
    u16::from(lo) | (u16::from(hi) << 8)
}

/// Peek a bank-0-wrapping 16-bit pointer at `at` — the non-destructive
/// twin of [`am::read_pointer16`], used to see the `(dp),Y` pointer
/// without performing the real dereference `ops::execute` will do a
/// moment later.
fn peek_pointer16(bus: &dyn CpuBus, at: am::Addr) -> u16 {
    let lo = bus.peek(at);
    let hi = bus.peek(am::Addr::from((at as u16).wrapping_add(1)));
    u16::from(lo) | (u16::from(hi) << 8)
}

/// `abs,X`/`abs,Y` or `(dp),Y`, as a READ.
///
/// With an 8-bit index (`Cpu::i8`: emulation mode, or native with `X`
/// set) this is conditional on the index addition crossing a page — the
/// classic 6502-style penalty. With a **16-bit** index (native, `X`
/// clear) it is unconditional: the CPU cannot add a full 16-bit index in
/// one cycle, so the extra cycle is always spent regardless of whether
/// the add happens to cross. Confirmed against the vectors — every
/// 16-bit-index case pays it, crossed or not; e.g. `79 n 1479`
/// (`ADC $....,Y`, offset `$240B` + `Y=$0073` stays on the same page)
/// still shows 5 cycles, not 4, because `X` is clear there.
fn abs_indexed_read(cpu: &Cpu, bus: &dyn CpuBus, index: u16) -> u8 {
    if !cpu.i8() {
        return 1;
    }
    let offset = peek_operand16(cpu, bus, 0);
    u8::from(crosses(offset, index))
}

fn dp_indirect_y_read(cpu: &Cpu, bus: &dyn CpuBus) -> u8 {
    if !cpu.i8() {
        return dl_penalty(cpu) + 1;
    }
    let dp_offset = peek_operand8(cpu, bus, 0);
    let ptr = peek_pointer16(bus, am::direct(cpu, dp_offset));
    dl_penalty(cpu) + u8::from(crosses(ptr, cpu.y))
}

/// A conditional-taken branch (`BPL`..`BEQ`): 0 if not taken, +1 if
/// taken, +1 more if taken and (emulation mode only) the branch crosses
/// a page — mirrors `ops::branch`'s own arithmetic exactly, but reads
/// the offset with `peek` instead of `fetch8`.
fn branch_cost(cpu: &Cpu, bus: &dyn CpuBus, taken: bool) -> u8 {
    if !taken {
        return 0;
    }
    let offset = peek_operand8(cpu, bus, 0) as i8;
    let mut cost = 1;
    if cpu.e {
        let base = cpu.pc.wrapping_add(1);
        let target = base.wrapping_add_signed(i16::from(offset));
        if (base & 0xFF00) != (target & 0xFF00) {
            cost += 1;
        }
    }
    cost
}

/// The internal (no-bus-access) cycles opcode `opcode` will spend, given
/// the CPU's state right after its opcode byte was fetched (`cpu.pc`
/// points at the first operand byte, if any). Called from [`Cpu::step`]
/// before [`super::ops::execute`] runs — see the module doc for why this
/// order, and why peeking rather than restructuring execution.
#[allow(clippy::too_many_lines)]
pub(super) fn internal_cycles(cpu: &Cpu, bus: &dyn CpuBus, opcode: u8) -> u8 {
    match opcode {
        // ---- Flat: no addressing penalty at all -----------------------
        // Immediate forms.
        0xA9 | 0xA2 | 0xA0 | 0xC9 | 0xE0 | 0xC0 | 0x89 | 0x29 | 0x09 | 0x49 | 0x69 | 0xE9
        // Plain absolute (non-indexed, non-RMW).
        | 0xAD | 0x8D | 0xAE | 0x8E | 0xAC | 0x8C | 0x9C | 0x2C | 0xCD | 0xEC | 0xCC | 0x2D
        | 0x0D | 0x4D | 0x6D | 0xED
        // Absolute long, indexed or not — the 24-bit add never carries a
        // cross surcharge.
        | 0xAF | 0x8F | 0xCF | 0x2F | 0x0F | 0x4F | 0x6F | 0xEF
        | 0xBF | 0x9F | 0xDF | 0x3F | 0x1F | 0x5F | 0x7F | 0xFF
        // JMP abs, JMP long, JMP (abs), JML [abs].
        | 0x4C | 0x5C | 0x6C | 0xDC
        // PEA, WDM, BRK, COP.
        | 0xF4 | 0x42 | 0x00 | 0x02 => 0,

        // ---- Direct page, no index: DP-low-byte penalty only ----------
        0xA5 | 0x85 | 0xA6 | 0x86 | 0xA4 | 0x84 | 0x64 | 0x24 | 0xC5 | 0xE4 | 0xC4 | 0x25
        | 0x05 | 0x45 | 0x65 | 0xE5
        // (dp)
        | 0xB2 | 0x92 | 0xD2 | 0x32 | 0x12 | 0x52 | 0x72 | 0xF2
        // [dp]
        | 0xA7 | 0x87 | 0xC7 | 0x27 | 0x07 | 0x47 | 0x67 | 0xE7
        // [dp],Y — long-indexed, DP penalty but never a cross surcharge.
        | 0xB7 | 0x97 | 0xD7 | 0x37 | 0x17 | 0x57 | 0x77 | 0xF7
        // PEI
        | 0xD4 => dl_penalty(cpu),

        // ---- Direct page indexed by X/Y, and (dp,X): fixed +1 + DP -----
        0xB5 | 0x95 | 0xB6 | 0x96 | 0xB4 | 0x94 | 0x74 | 0x34 | 0xD5 | 0x35 | 0x15 | 0x55
        | 0x75 | 0xF5
        | 0xA1 | 0x81 | 0xC1 | 0x21 | 0x01 | 0x41 | 0x61 | 0xE1 => 1 + dl_penalty(cpu),

        // ---- Stack relative: fixed +1, no DP involvement ---------------
        0xA3 | 0x83 | 0xC3 | 0x23 | 0x03 | 0x43 | 0x63 | 0xE3 => 1,
        // ---- (sr,S),Y: fixed +2 -----------------------------------------
        0xB3 | 0x93 | 0xD3 | 0x33 | 0x13 | 0x53 | 0x73 | 0xF3 => 2,

        // ---- abs,X / abs,Y reads: conditional on crossing --------------
        0xBD | 0xDD | 0x3D | 0x1D | 0x5D | 0x7D | 0xFD | 0xBC => abs_indexed_read(cpu, bus, cpu.x),
        0xB9 | 0xBE | 0xD9 | 0x39 | 0x19 | 0x59 | 0x79 | 0xF9 => abs_indexed_read(cpu, bus, cpu.y),
        0x3C => abs_indexed_read(cpu, bus, cpu.x),

        // ---- abs,X / abs,Y stores: fixed +1 -----------------------------
        0x9D | 0x99 | 0x9E => 1,

        // ---- (dp),Y read: DP penalty + conditional crossing ------------
        0xB1 | 0xD1 | 0x31 | 0x11 | 0x51 | 0x71 | 0xF1 => dp_indirect_y_read(cpu, bus),
        // ---- (dp),Y store: DP penalty + fixed +1 -----------------------
        0x91 => dl_penalty(cpu) + 1,

        // ---- RMW: direct page (+1 modify + DP) -------------------------
        0x06 | 0x46 | 0x26 | 0x66 | 0xE6 | 0xC6 | 0x04 | 0x14 => 1 + dl_penalty(cpu),
        // ---- RMW: direct page indexed (+1 modify + 1 fixed + DP) -------
        0x16 | 0x56 | 0x36 | 0x76 | 0xF6 | 0xD6 => 2 + dl_penalty(cpu),
        // ---- RMW: absolute (+1 modify) ---------------------------------
        0x0E | 0x4E | 0x2E | 0x6E | 0xEE | 0xCE | 0x0C | 0x1C => 1,
        // ---- RMW: absolute indexed (+1 modify + 1 fixed) ---------------
        0x1E | 0x5E | 0x3E | 0x7E | 0xFE | 0xDE => 2,
        // ---- RMW: accumulator form — priced as a plain implied op ------
        0x0A | 0x4A | 0x2A | 0x6A | 0x1A | 0x3A => 1,

        // ---- One-byte implied/register ops: fixed +1 -------------------
        0x18 | 0x38 | 0x58 | 0x78 | 0xB8 | 0xD8 | 0xF8 // flags
        | 0xE8 | 0xCA | 0xC8 | 0x88 // INX/DEX/INY/DEY
        | 0xAA | 0xA8 | 0x8A | 0x98 | 0x9B | 0xBB | 0xBA | 0x9A | 0x5B | 0x7B | 0x1B | 0x3B // transfers
        | 0xEA // NOP
        | 0xFB // XCE
        | 0xC2 | 0xE2 // REP/SEP
        | 0x48 | 0xDA | 0x5A | 0x08 | 0x8B | 0x0B | 0x4B // PHA/PHX/PHY/PHP/PHB/PHD/PHK
        | 0x7C // JMP (abs,X)
        | 0x20 // JSR abs
        | 0xFC // JSR (abs,X)
        | 0x22 // JSL
        | 0x62 // PER
        => 1,

        // ---- Pulls: fixed +2 (a throwaway read a push never needs) -----
        0x68 | 0xFA | 0x7A | 0x28 | 0xAB | 0x2B // PLA/PLX/PLY/PLP/PLB/PLD
        | 0xEB // XBA
        | 0x40 // RTI
        | 0x6B // RTL
        => 2,

        // ---- Fixed +3: RTS, WAI, STP ------------------------------------
        0x60 | 0xCB | 0xDB => 3,

        // ---- Conditional branches ---------------------------------------
        0x10 => branch_cost(cpu, bus, !cpu.flag(super::flags::N)),
        0x30 => branch_cost(cpu, bus, cpu.flag(super::flags::N)),
        0x50 => branch_cost(cpu, bus, !cpu.flag(super::flags::V)),
        0x70 => branch_cost(cpu, bus, cpu.flag(super::flags::V)),
        0x90 => branch_cost(cpu, bus, !cpu.flag(super::flags::C)),
        0xB0 => branch_cost(cpu, bus, cpu.flag(super::flags::C)),
        0xD0 => branch_cost(cpu, bus, !cpu.flag(super::flags::Z)),
        0xF0 => branch_cost(cpu, bus, cpu.flag(super::flags::Z)),
        0x80 => branch_cost(cpu, bus, true),
        // BRL: unconditional, fixed +1 — never a crossing surcharge (a
        // 16-bit relative jump, not the 8-bit form the surcharge exists
        // for; confirmed constant across all vector cases).
        0x82 => 1,

        // ---- Block moves: not vector-pinned (excluded opcodes) ---------
        // See the module doc's closing section.
        0x54 | 0x44 => 2,
    }
}
