//! 65C816 instruction implementations (ticket W6-01a).
//!
//! ## How an operand's width is decided
//!
//! Never by the opcode. `LDA #$12` and `LDA #$1234` are the same byte
//! (`$A9`); which one it is depends on the `M` flag when the instruction
//! executes. Every op below asks [`Cpu::a8`] or [`Cpu::i8`] at run time,
//! and immediates fetch one or two bytes accordingly — which is also why
//! instruction *length* is not a constant and a disassembler for this
//! chip has to track flags.
//!
//! ## What is here and what is not
//!
//! W6-01a's scope is "core ops, emulation/native modes, addressing".
//! Implemented: loads/stores, transfers, the ALU (with decimal mode),
//! shifts/rotates, compares, increments, the stack group, branches,
//! jumps and subroutine calls, flag ops, `XCE`, `REP`/`SEP`, `XBA`,
//! `NOP`, block moves.
//!
//! `BRK`, `COP`, `RTI`, `WAI` and `STP` were held back from W6-01a
//! deliberately — returning `Err(opcode)` rather than no-opping, so the
//! gap could not be mistaken for completeness — and land here in W6-01b
//! along with the memory-speed model in [`super::speed`].

use super::addressing as am;
use super::{flags, Cpu, CpuBus};

/// How an instruction gets at its operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Immediate,
    Direct,
    DirectX,
    DirectY,
    Absolute,
    AbsoluteX,
    AbsoluteY,
    Long,
    LongX,
    StackRel,
    StackRelIndY,
    IndirectDp,
    IndirectDpX,
    IndirectDpY,
    IndirectLong,
    IndirectLongY,
}

/// Resolve `mode` to an address, consuming its operand bytes.
///
/// `Immediate` has no address — it is handled by [`load_operand`], which
/// is why this returns `Option`.
/// Does a multi-byte access in this mode wrap inside bank 0?
///
/// Direct-page and stack-relative accesses are always in bank 0 and stay
/// there. Everything else does a 24-bit increment. See
/// [`am::read_value`] — this is the flag it takes.
fn wraps_in_bank0(mode: Mode) -> bool {
    matches!(
        mode,
        Mode::Direct | Mode::DirectX | Mode::DirectY | Mode::StackRel
    )
}

fn resolve(cpu: &mut Cpu, bus: &mut dyn CpuBus, mode: Mode) -> Option<(am::Addr, bool)> {
    let addr = match mode {
        Mode::Immediate => return None,
        Mode::Direct => {
            let o = cpu.fetch8(bus);
            am::direct(cpu, o)
        }
        Mode::DirectX => {
            let o = cpu.fetch8(bus);
            am::direct_indexed(cpu, o, cpu.x)
        }
        Mode::DirectY => {
            let o = cpu.fetch8(bus);
            am::direct_indexed(cpu, o, cpu.y)
        }
        Mode::Absolute => {
            let o = cpu.fetch16(bus);
            am::absolute(cpu, o)
        }
        Mode::AbsoluteX => {
            let o = cpu.fetch16(bus);
            am::absolute_indexed(cpu, o, cpu.x)
        }
        Mode::AbsoluteY => {
            let o = cpu.fetch16(bus);
            am::absolute_indexed(cpu, o, cpu.y)
        }
        Mode::Long => {
            let lo = cpu.fetch16(bus);
            let b = cpu.fetch8(bus);
            am::long((u32::from(b) << 16) | u32::from(lo))
        }
        Mode::LongX => {
            let lo = cpu.fetch16(bus);
            let b = cpu.fetch8(bus);
            am::long_indexed((u32::from(b) << 16) | u32::from(lo), cpu.x)
        }
        Mode::StackRel => {
            let o = cpu.fetch8(bus);
            am::stack_relative(cpu, o)
        }
        Mode::StackRelIndY => {
            let o = cpu.fetch8(bus);
            am::stack_relative_indirect_indexed(cpu, bus, o)
        }
        Mode::IndirectDp => {
            let o = cpu.fetch8(bus);
            am::direct_indirect(cpu, bus, o)
        }
        Mode::IndirectDpX => {
            let o = cpu.fetch8(bus);
            am::direct_indexed_indirect(cpu, bus, o)
        }
        Mode::IndirectDpY => {
            let o = cpu.fetch8(bus);
            am::direct_indirect_indexed(cpu, bus, o)
        }
        Mode::IndirectLong => {
            let o = cpu.fetch8(bus);
            am::direct_indirect_long(cpu, bus, o)
        }
        Mode::IndirectLongY => {
            let o = cpu.fetch8(bus);
            am::direct_indirect_long_indexed(cpu, bus, o)
        }
    };
    Some((addr, wraps_in_bank0(mode)))
}

/// Fetch an operand's VALUE, handling the immediate case's width.
fn load_operand(cpu: &mut Cpu, bus: &mut dyn CpuBus, mode: Mode, eight: bool) -> u16 {
    match resolve(cpu, bus, mode) {
        None => {
            if eight {
                u16::from(cpu.fetch8(bus))
            } else {
                cpu.fetch16(bus)
            }
        }
        Some((addr, wrap)) => am::read_value(bus, addr, eight, wrap),
    }
}

/// Merge an 8-bit result into the accumulator without disturbing `B`.
///
/// The 65816's high accumulator half survives 8-bit operations, and
/// `XBA` can bring it back. Truncating `A` to 8 bits here is the classic
/// bug: it works until a program uses `XBA`, then loses data with no
/// obvious cause.
fn set_a(cpu: &mut Cpu, value: u16, eight: bool) {
    if eight {
        cpu.a = (cpu.a & 0xFF00) | (value & 0x00FF);
    } else {
        cpu.a = value;
    }
}

/// The shared decimal-capable adder behind `ADC` and `SBC`.
///
/// ## Why this is one function and not two
///
/// `SBC` is `ADC` of the complement — on this chip that identity holds in
/// **decimal mode too**, not just binary. The only differences are the
/// direction of the per-digit correction (`-6` where `ADC` adds `+6`) and
/// the comparison that detects a digit needing it. Writing them as two
/// functions duplicated the subtle part and let the two copies drift,
/// which is exactly what happened: the decimal paths disagreed about
/// where `V` comes from.
///
/// ## Where V comes from, precisely
///
/// The adder is a chain of 4-bit stages. Each stage corrects its digit
/// and carries into the next — except the last, whose correction happens
/// **after** the overflow output is latched. So `V` is computed from a
/// sum that is decimal-corrected in every digit *but the top one*.
///
/// This is narrower than either obvious guess. "V from the fully adjusted
/// result" fails 13% of the `D = 1` vectors; "V from the pure binary sum"
/// fixes most of those and still fails 78 of 10000, because the low
/// digits' corrections really do reach it. Only the off-by-one-stage
/// version passes all 400000.
///
/// Structure follows the per-digit formulation used by bsnes/higan, which
/// is the form the hardware's carry chain actually takes.
fn add_with_carry(cpu: &mut Cpu, operand: u16, subtract: bool) {
    let eight = cpu.a8();
    let width_mask: i32 = if eight { 0xFF } else { 0xFFFF };
    let digits = if eight { 2 } else { 4 };
    let sign: i32 = if eight { 0x80 } else { 0x8000 };
    let top_shift = 4 * (digits - 1);

    let a = i32::from(cpu.a) & width_mask;
    let operand = i32::from(operand) & width_mask;
    let decimal = cpu.flag(flags::D);

    let mut result;
    if decimal {
        let mut carry = i32::from(cpu.flag(flags::C));
        result = 0;
        for digit in 0..digits {
            let shift = 4 * digit;
            let nibble = 0xF << shift;
            let below = (1 << shift) - 1;
            // Each stage re-adds the digits at its own position and
            // carries the already-corrected lower digits along unchanged.
            result = (a & nibble) + (operand & nibble) + (carry << shift) + (result & below);
            if digit + 1 == digits {
                break; // top digit: corrected after V, below.
            }
            let saturated = nibble | below;
            if subtract {
                if result <= saturated {
                    result -= 0x6 << shift;
                }
            } else if result > ((0x9 << shift) | below) {
                result += 0x6 << shift;
            }
            carry = i32::from(result > saturated);
        }
    } else {
        result = a + operand + i32::from(cpu.flag(flags::C));
    }

    // Latched here — see the doc comment. Moving this one line below the
    // top-digit correction is the 78-vector bug.
    cpu.set_flag(flags::V, (!(a ^ operand) & (a ^ result) & sign) != 0);

    if decimal {
        if subtract {
            if result <= width_mask {
                result -= 0x6 << top_shift;
            }
        } else if result > ((0x9 << top_shift) | ((1 << top_shift) - 1)) {
            result += 0x6 << top_shift;
        }
    }

    cpu.set_flag(flags::C, result > width_mask);
    let result = (result & width_mask) as u16;
    cpu.set_nz(result, eight);
    set_a(cpu, result, eight);
}

fn adc(cpu: &mut Cpu, value: u16) {
    add_with_carry(cpu, value, false);
}

fn sbc(cpu: &mut Cpu, value: u16) {
    // The complement identity, applied at the operand rather than by
    // duplicating the adder with the signs flipped.
    let mask = if cpu.a8() { 0xFF } else { 0xFFFF };
    add_with_carry(cpu, (!value) & mask, true);
}

fn compare(cpu: &mut Cpu, reg: u16, value: u16, eight: bool) {
    let (reg, value) = if eight {
        (reg & 0xFF, value & 0xFF)
    } else {
        (reg, value)
    };
    let diff = reg.wrapping_sub(value);
    cpu.set_flag(flags::C, reg >= value);
    cpu.set_nz(diff, eight);
}

fn branch(cpu: &mut Cpu, bus: &mut dyn CpuBus, take: bool) {
    let offset = cpu.fetch8(bus) as i8;
    if take {
        cpu.pc = cpu.pc.wrapping_add_signed(i16::from(offset));
    }
}

/// Read-modify-write through an address, or on the accumulator when
/// `addr` is `None`.
fn rmw(
    cpu: &mut Cpu,
    bus: &mut dyn CpuBus,
    addr: Option<(am::Addr, bool)>,
    eight: bool,
    f: impl FnOnce(&mut Cpu, u16) -> u16,
) {
    match addr {
        None => {
            let v = if eight { cpu.a & 0xFF } else { cpu.a };
            let r = f(cpu, v);
            set_a(cpu, r, eight);
        }
        Some((addr, wrap)) => {
            let v = am::read_value(bus, addr, eight, wrap);
            let r = f(cpu, v);
            am::write_value(bus, addr, r, eight, wrap);
        }
    }
}

/// Execute `opcode`.
///
/// # Errors
/// Returns the opcode when it is not implemented in this ticket — see the
/// module doc for which ones and why.
#[allow(clippy::too_many_lines)]
pub fn execute(cpu: &mut Cpu, bus: &mut dyn CpuBus, opcode: u8) -> Result<(), u8> {
    let m8 = cpu.a8();
    let i8b = cpu.i8();

    match opcode {
        // ---- LDA ------------------------------------------------------
        0xA9 | 0xA5 | 0xB5 | 0xAD | 0xBD | 0xB9 | 0xAF | 0xBF | 0xA1 | 0xB1 | 0xB2 | 0xA7
        | 0xB7 | 0xA3 | 0xB3 => {
            let mode = match opcode {
                0xA9 => Mode::Immediate,
                0xA5 => Mode::Direct,
                0xB5 => Mode::DirectX,
                0xAD => Mode::Absolute,
                0xBD => Mode::AbsoluteX,
                0xB9 => Mode::AbsoluteY,
                0xAF => Mode::Long,
                0xBF => Mode::LongX,
                0xA1 => Mode::IndirectDpX,
                0xB1 => Mode::IndirectDpY,
                0xB2 => Mode::IndirectDp,
                0xA7 => Mode::IndirectLong,
                0xB7 => Mode::IndirectLongY,
                0xA3 => Mode::StackRel,
                _ => Mode::StackRelIndY,
            };
            let v = load_operand(cpu, bus, mode, m8);
            set_a(cpu, v, m8);
            cpu.set_nz(v, m8);
        }
        // ---- STA ------------------------------------------------------
        0x85 | 0x95 | 0x8D | 0x9D | 0x99 | 0x8F | 0x9F | 0x81 | 0x91 | 0x92 | 0x87 | 0x97
        | 0x83 | 0x93 => {
            let mode = match opcode {
                0x85 => Mode::Direct,
                0x95 => Mode::DirectX,
                0x8D => Mode::Absolute,
                0x9D => Mode::AbsoluteX,
                0x99 => Mode::AbsoluteY,
                0x8F => Mode::Long,
                0x9F => Mode::LongX,
                0x81 => Mode::IndirectDpX,
                0x91 => Mode::IndirectDpY,
                0x92 => Mode::IndirectDp,
                0x87 => Mode::IndirectLong,
                0x97 => Mode::IndirectLongY,
                0x83 => Mode::StackRel,
                _ => Mode::StackRelIndY,
            };
            let (addr, wrap) = resolve(cpu, bus, mode).expect("store modes are never immediate");
            am::write_value(bus, addr, cpu.a, m8, wrap);
        }
        // ---- LDX / LDY / STX / STY ------------------------------------
        0xA2 | 0xA6 | 0xB6 | 0xAE | 0xBE => {
            let mode = match opcode {
                0xA2 => Mode::Immediate,
                0xA6 => Mode::Direct,
                0xB6 => Mode::DirectY,
                0xAE => Mode::Absolute,
                _ => Mode::AbsoluteY,
            };
            let v = load_operand(cpu, bus, mode, i8b);
            cpu.x = if i8b { v & 0xFF } else { v };
            cpu.set_nz(cpu.x, i8b);
        }
        0xA0 | 0xA4 | 0xB4 | 0xAC | 0xBC => {
            let mode = match opcode {
                0xA0 => Mode::Immediate,
                0xA4 => Mode::Direct,
                0xB4 => Mode::DirectX,
                0xAC => Mode::Absolute,
                _ => Mode::AbsoluteX,
            };
            let v = load_operand(cpu, bus, mode, i8b);
            cpu.y = if i8b { v & 0xFF } else { v };
            cpu.set_nz(cpu.y, i8b);
        }
        0x86 | 0x96 | 0x8E => {
            let mode = match opcode {
                0x86 => Mode::Direct,
                0x96 => Mode::DirectY,
                _ => Mode::Absolute,
            };
            let (addr, wrap) = resolve(cpu, bus, mode).expect("not immediate");
            am::write_value(bus, addr, cpu.x, i8b, wrap);
        }
        0x84 | 0x94 | 0x8C => {
            let mode = match opcode {
                0x84 => Mode::Direct,
                0x94 => Mode::DirectX,
                _ => Mode::Absolute,
            };
            let (addr, wrap) = resolve(cpu, bus, mode).expect("not immediate");
            am::write_value(bus, addr, cpu.y, i8b, wrap);
        }
        // STZ — store zero.
        0x64 | 0x74 | 0x9C | 0x9E => {
            let mode = match opcode {
                0x64 => Mode::Direct,
                0x74 => Mode::DirectX,
                0x9C => Mode::Absolute,
                _ => Mode::AbsoluteX,
            };
            let (addr, wrap) = resolve(cpu, bus, mode).expect("not immediate");
            am::write_value(bus, addr, 0, m8, wrap);
        }
        // ---- ALU ------------------------------------------------------
        0x69 | 0x65 | 0x75 | 0x6D | 0x7D | 0x79 | 0x6F | 0x7F | 0x61 | 0x71 | 0x72 | 0x67
        | 0x77 | 0x63 | 0x73 => {
            let mode = alu_mode(opcode, 0x69);
            let v = load_operand(cpu, bus, mode, m8);
            adc(cpu, v);
        }
        0xE9 | 0xE5 | 0xF5 | 0xED | 0xFD | 0xF9 | 0xEF | 0xFF | 0xE1 | 0xF1 | 0xF2 | 0xE7
        | 0xF7 | 0xE3 | 0xF3 => {
            let mode = alu_mode(opcode, 0xE9);
            let v = load_operand(cpu, bus, mode, m8);
            sbc(cpu, v);
        }
        0x29 | 0x25 | 0x35 | 0x2D | 0x3D | 0x39 | 0x2F | 0x3F | 0x21 | 0x31 | 0x32 | 0x27
        | 0x37 | 0x23 | 0x33 => {
            let mode = alu_mode(opcode, 0x29);
            let v = load_operand(cpu, bus, mode, m8);
            let r = if m8 { (cpu.a & 0xFF) & v } else { cpu.a & v };
            set_a(cpu, r, m8);
            cpu.set_nz(r, m8);
        }
        0x09 | 0x05 | 0x15 | 0x0D | 0x1D | 0x19 | 0x0F | 0x1F | 0x01 | 0x11 | 0x12 | 0x07
        | 0x17 | 0x03 | 0x13 => {
            let mode = alu_mode(opcode, 0x09);
            let v = load_operand(cpu, bus, mode, m8);
            let r = if m8 { (cpu.a & 0xFF) | v } else { cpu.a | v };
            set_a(cpu, r, m8);
            cpu.set_nz(r, m8);
        }
        0x49 | 0x45 | 0x55 | 0x4D | 0x5D | 0x59 | 0x4F | 0x5F | 0x41 | 0x51 | 0x52 | 0x47
        | 0x57 | 0x43 | 0x53 => {
            let mode = alu_mode(opcode, 0x49);
            let v = load_operand(cpu, bus, mode, m8);
            let r = if m8 { (cpu.a & 0xFF) ^ v } else { cpu.a ^ v };
            set_a(cpu, r, m8);
            cpu.set_nz(r, m8);
        }
        0xC9 | 0xC5 | 0xD5 | 0xCD | 0xDD | 0xD9 | 0xCF | 0xDF | 0xC1 | 0xD1 | 0xD2 | 0xC7
        | 0xD7 | 0xC3 | 0xD3 => {
            let mode = alu_mode(opcode, 0xC9);
            let v = load_operand(cpu, bus, mode, m8);
            compare(cpu, cpu.a, v, m8);
        }
        0xE0 | 0xE4 | 0xEC => {
            let mode = match opcode {
                0xE0 => Mode::Immediate,
                0xE4 => Mode::Direct,
                _ => Mode::Absolute,
            };
            let v = load_operand(cpu, bus, mode, i8b);
            compare(cpu, cpu.x, v, i8b);
        }
        0xC0 | 0xC4 | 0xCC => {
            let mode = match opcode {
                0xC0 => Mode::Immediate,
                0xC4 => Mode::Direct,
                _ => Mode::Absolute,
            };
            let v = load_operand(cpu, bus, mode, i8b);
            compare(cpu, cpu.y, v, i8b);
        }
        // BIT. The immediate form sets only Z — N and V come from the
        // OPERAND's top bits for memory forms, which an immediate has no
        // business supplying.
        0x89 => {
            let v = load_operand(cpu, bus, Mode::Immediate, m8);
            let a = if m8 { cpu.a & 0xFF } else { cpu.a };
            cpu.set_flag(flags::Z, a & v == 0);
        }
        0x24 | 0x34 | 0x2C | 0x3C => {
            let mode = match opcode {
                0x24 => Mode::Direct,
                0x34 => Mode::DirectX,
                0x2C => Mode::Absolute,
                _ => Mode::AbsoluteX,
            };
            let v = load_operand(cpu, bus, mode, m8);
            let a = if m8 { cpu.a & 0xFF } else { cpu.a };
            cpu.set_flag(flags::Z, a & v == 0);
            let (nbit, vbit) = if m8 { (0x80, 0x40) } else { (0x8000, 0x4000) };
            cpu.set_flag(flags::N, v & nbit != 0);
            cpu.set_flag(flags::V, v & vbit != 0);
        }
        // TRB / TSB — test and reset/set bits.
        0x14 | 0x1C | 0x04 | 0x0C => {
            let (mode, set) = match opcode {
                0x14 => (Mode::Direct, false),
                0x1C => (Mode::Absolute, false),
                0x04 => (Mode::Direct, true),
                _ => (Mode::Absolute, true),
            };
            let (addr, wrap) = resolve(cpu, bus, mode).expect("not immediate");
            let v = am::read_value(bus, addr, m8, wrap);
            let a = if m8 { cpu.a & 0xFF } else { cpu.a };
            cpu.set_flag(flags::Z, a & v == 0);
            let r = if set { v | a } else { v & !a };
            am::write_value(bus, addr, r, m8, wrap);
        }
        // ---- shifts and rotates ---------------------------------------
        0x0A | 0x06 | 0x16 | 0x0E | 0x1E => {
            let addr = shift_target(cpu, bus, opcode, 0x0A);
            rmw(cpu, bus, addr, m8, |cpu, v| {
                let top = if m8 { 0x80 } else { 0x8000 };
                cpu.set_flag(flags::C, v & top != 0);
                let r = v << 1;
                cpu.set_nz(r, m8);
                r
            });
        }
        0x4A | 0x46 | 0x56 | 0x4E | 0x5E => {
            let addr = shift_target(cpu, bus, opcode, 0x4A);
            rmw(cpu, bus, addr, m8, |cpu, v| {
                cpu.set_flag(flags::C, v & 1 != 0);
                let r = v >> 1;
                cpu.set_nz(r, m8);
                r
            });
        }
        0x2A | 0x26 | 0x36 | 0x2E | 0x3E => {
            let addr = shift_target(cpu, bus, opcode, 0x2A);
            let carry_in = u16::from(cpu.flag(flags::C));
            rmw(cpu, bus, addr, m8, |cpu, v| {
                let top = if m8 { 0x80 } else { 0x8000 };
                cpu.set_flag(flags::C, v & top != 0);
                let r = (v << 1) | carry_in;
                cpu.set_nz(r, m8);
                r
            });
        }
        0x6A | 0x66 | 0x76 | 0x6E | 0x7E => {
            let addr = shift_target(cpu, bus, opcode, 0x6A);
            let carry_in = u16::from(cpu.flag(flags::C));
            rmw(cpu, bus, addr, m8, |cpu, v| {
                cpu.set_flag(flags::C, v & 1 != 0);
                let top = if m8 { 0x80 } else { 0x8000 };
                let r = (v >> 1) | (carry_in * top);
                cpu.set_nz(r, m8);
                r
            });
        }
        // ---- INC / DEC ------------------------------------------------
        0x1A | 0xE6 | 0xF6 | 0xEE | 0xFE => {
            let addr = shift_target(cpu, bus, opcode, 0x1A);
            rmw(cpu, bus, addr, m8, |cpu, v| {
                let r = v.wrapping_add(1);
                cpu.set_nz(r, m8);
                r
            });
        }
        0x3A | 0xC6 | 0xD6 | 0xCE | 0xDE => {
            let addr = shift_target(cpu, bus, opcode, 0x3A);
            rmw(cpu, bus, addr, m8, |cpu, v| {
                let r = v.wrapping_sub(1);
                cpu.set_nz(r, m8);
                r
            });
        }
        0xE8 => {
            cpu.x = index_step(cpu.x, 1, i8b);
            cpu.set_nz(cpu.x, i8b);
        }
        0xCA => {
            cpu.x = index_step(cpu.x, -1, i8b);
            cpu.set_nz(cpu.x, i8b);
        }
        0xC8 => {
            cpu.y = index_step(cpu.y, 1, i8b);
            cpu.set_nz(cpu.y, i8b);
        }
        0x88 => {
            cpu.y = index_step(cpu.y, -1, i8b);
            cpu.set_nz(cpu.y, i8b);
        }
        // ---- transfers -------------------------------------------------
        // Width rules differ per transfer and are NOT uniform: TAX/TAY use
        // the INDEX width, TXA/TYA the ACCUMULATOR width, and TCD/TCS/TDC/
        // TSC are always 16-bit regardless of flags. Getting this uniform
        // is a real bug on this chip.
        0xAA => {
            cpu.x = if i8b { cpu.a & 0xFF } else { cpu.a };
            cpu.set_nz(cpu.x, i8b);
        }
        0xA8 => {
            cpu.y = if i8b { cpu.a & 0xFF } else { cpu.a };
            cpu.set_nz(cpu.y, i8b);
        }
        0x8A => {
            let v = if m8 { cpu.x & 0xFF } else { cpu.x };
            set_a(cpu, v, m8);
            cpu.set_nz(v, m8);
        }
        0x98 => {
            let v = if m8 { cpu.y & 0xFF } else { cpu.y };
            set_a(cpu, v, m8);
            cpu.set_nz(v, m8);
        }
        0x9B => {
            cpu.y = if i8b { cpu.x & 0xFF } else { cpu.x };
            cpu.set_nz(cpu.y, i8b);
        }
        0xBB => {
            cpu.x = if i8b { cpu.y & 0xFF } else { cpu.y };
            cpu.set_nz(cpu.x, i8b);
        }
        0xBA => {
            cpu.x = if i8b { cpu.sp & 0xFF } else { cpu.sp };
            cpu.set_nz(cpu.x, i8b);
        }
        0x9A => {
            // TXS takes the full X in native mode; in emulation mode the
            // stack is pinned to page 1, so only the low byte moves.
            cpu.sp = if cpu.e {
                0x0100 | (cpu.x & 0x00FF)
            } else {
                cpu.x
            };
        }
        0x5B => {
            cpu.d = cpu.a;
            cpu.set_nz(cpu.d, false);
        }
        0x7B => {
            set_a(cpu, cpu.d, false);
            cpu.set_nz(cpu.d, false);
        }
        0x1B => {
            cpu.sp = if cpu.e {
                0x0100 | (cpu.a & 0x00FF)
            } else {
                cpu.a
            };
        }
        0x3B => {
            set_a(cpu, cpu.sp, false);
            cpu.set_nz(cpu.sp, false);
        }
        // XBA — swap the accumulator halves. Flags come from the NEW low
        // byte, and it is always an 8-bit-flavoured result regardless of M.
        0xEB => {
            cpu.a = cpu.a.rotate_right(8);
            cpu.set_nz(cpu.a & 0xFF, true);
        }
        // ---- stack -----------------------------------------------------
        0x48 => {
            if m8 {
                cpu.push8(bus, cpu.a as u8);
            } else {
                cpu.push16(bus, cpu.a);
            }
        }
        0x68 => {
            let v = if m8 {
                u16::from(cpu.pull8(bus))
            } else {
                cpu.pull16(bus)
            };
            set_a(cpu, v, m8);
            cpu.set_nz(v, m8);
        }
        0xDA => {
            if i8b {
                cpu.push8(bus, cpu.x as u8);
            } else {
                cpu.push16(bus, cpu.x);
            }
        }
        0xFA => {
            let v = if i8b {
                u16::from(cpu.pull8(bus))
            } else {
                cpu.pull16(bus)
            };
            cpu.x = v;
            cpu.set_nz(v, i8b);
        }
        0x5A => {
            if i8b {
                cpu.push8(bus, cpu.y as u8);
            } else {
                cpu.push16(bus, cpu.y);
            }
        }
        0x7A => {
            let v = if i8b {
                u16::from(cpu.pull8(bus))
            } else {
                cpu.pull16(bus)
            };
            cpu.y = v;
            cpu.set_nz(v, i8b);
        }
        0x08 => {
            cpu.push8(bus, cpu.p);
        }
        0x28 => {
            cpu.p = cpu.pull8(bus);
            // Pulling P cannot escape emulation mode's forced widths.
            if cpu.e {
                cpu.p |= flags::M | flags::X;
            }
            if cpu.i8() {
                cpu.x &= 0xFF;
                cpu.y &= 0xFF;
            }
        }
        0x8B => cpu.push8_flat(bus, cpu.dbr),
        0xAB => {
            cpu.dbr = cpu.pull8_flat(bus);
            cpu.set_nz(u16::from(cpu.dbr), true);
        }
        0x0B => cpu.push16_flat(bus, cpu.d),
        0x2B => {
            cpu.d = cpu.pull16_flat(bus);
            cpu.set_nz(cpu.d, false);
        }
        0x4B => cpu.push8_flat(bus, cpu.pbr),
        0xF4 => {
            let v = cpu.fetch16(bus);
            cpu.push16_flat(bus, v);
        }
        0xD4 => {
            let o = cpu.fetch8(bus);
            let v = am::read_pointer16(bus, am::direct(cpu, o));
            cpu.push16_flat(bus, v);
        }
        0x62 => {
            let rel = cpu.fetch16(bus);
            let target = cpu.pc.wrapping_add(rel);
            cpu.push16_flat(bus, target);
        }
        // ---- flags -----------------------------------------------------
        0x18 => cpu.set_flag(flags::C, false),
        0x38 => cpu.set_flag(flags::C, true),
        0x58 => cpu.set_flag(flags::I, false),
        0x78 => cpu.set_flag(flags::I, true),
        0xB8 => cpu.set_flag(flags::V, false),
        0xD8 => cpu.set_flag(flags::D, false),
        0xF8 => cpu.set_flag(flags::D, true),
        0xC2 => {
            let mask = cpu.fetch8(bus);
            cpu.p &= !mask;
            if cpu.e {
                cpu.p |= flags::M | flags::X;
            }
        }
        0xE2 => {
            let mask = cpu.fetch8(bus);
            cpu.p |= mask;
            // Narrowing the index registers TRUNCATES them immediately —
            // the high bytes are gone, not hidden. A core that only
            // masked on read would let them reappear after a later REP.
            if cpu.i8() {
                cpu.x &= 0xFF;
                cpu.y &= 0xFF;
            }
        }
        // XCE — exchange carry and emulation. The only way in or out of
        // emulation mode.
        0xFB => {
            let new_e = cpu.flag(flags::C);
            cpu.set_flag(flags::C, cpu.e);
            cpu.set_emulation(new_e);
        }
        // ---- branches ---------------------------------------------------
        0x10 => branch(cpu, bus, !cpu.flag(flags::N)),
        0x30 => branch(cpu, bus, cpu.flag(flags::N)),
        0x50 => branch(cpu, bus, !cpu.flag(flags::V)),
        0x70 => branch(cpu, bus, cpu.flag(flags::V)),
        0x90 => branch(cpu, bus, !cpu.flag(flags::C)),
        0xB0 => branch(cpu, bus, cpu.flag(flags::C)),
        0xD0 => branch(cpu, bus, !cpu.flag(flags::Z)),
        0xF0 => branch(cpu, bus, cpu.flag(flags::Z)),
        0x80 => branch(cpu, bus, true),
        0x82 => {
            let rel = cpu.fetch16(bus);
            cpu.pc = cpu.pc.wrapping_add(rel);
        }
        // ---- jumps and calls ---------------------------------------------
        0x4C => cpu.pc = cpu.fetch16(bus),
        0x6C => {
            let at = cpu.fetch16(bus);
            cpu.pc = am::read_pointer16(bus, u32::from(at));
        }
        0x7C => {
            let base = cpu.fetch16(bus);
            let at = am::bank(cpu.pbr, base.wrapping_add(cpu.x));
            cpu.pc = am::read_pointer16_in_bank(bus, at);
        }
        0x5C => {
            let lo = cpu.fetch16(bus);
            let b = cpu.fetch8(bus);
            cpu.pc = lo;
            cpu.pbr = b;
        }
        0xDC => {
            let at = cpu.fetch16(bus);
            let target = am::read_pointer24(bus, u32::from(at));
            cpu.pc = target as u16;
            cpu.pbr = (target >> 16) as u8;
        }
        0x20 => {
            let target = cpu.fetch16(bus);
            // The pushed address is the LAST byte of this instruction,
            // not the next one — RTS adds 1. Pushing pc directly would
            // return one byte late.
            cpu.push16(bus, cpu.pc.wrapping_sub(1));
            cpu.pc = target;
        }
        0xFC => {
            let base = cpu.fetch16(bus);
            cpu.push16(bus, cpu.pc.wrapping_sub(1));
            let at = am::bank(cpu.pbr, base.wrapping_add(cpu.x));
            cpu.pc = am::read_pointer16_in_bank(bus, at);
        }
        0x22 => {
            let lo = cpu.fetch16(bus);
            let b = cpu.fetch8(bus);
            cpu.push8_flat(bus, cpu.pbr);
            cpu.push16_flat(bus, cpu.pc.wrapping_sub(1));
            cpu.pc = lo;
            cpu.pbr = b;
        }
        0x60 => {
            let addr = cpu.pull16(bus);
            cpu.pc = addr.wrapping_add(1);
        }
        0x6B => {
            let addr = cpu.pull16_flat(bus);
            cpu.pbr = cpu.pull8_flat(bus);
            cpu.pc = addr.wrapping_add(1);
        }
        // ---- block moves --------------------------------------------------
        // MVN/MVP move one byte per execution and rewind PC so the
        // instruction repeats until A wraps below zero. Modelling them as
        // a loop inside one step would be wrong for interrupts, which can
        // land between iterations on hardware.
        0x54 | 0x44 => {
            let dst_bank = cpu.fetch8(bus);
            let src_bank = cpu.fetch8(bus);
            let byte = bus.read(am::bank(src_bank, cpu.x));
            bus.write(am::bank(dst_bank, cpu.y), byte);
            cpu.dbr = dst_bank;
            let step: i32 = if opcode == 0x54 { 1 } else { -1 };
            cpu.x = index_step(cpu.x, step, cpu.i8());
            cpu.y = index_step(cpu.y, step, cpu.i8());
            if cpu.a == 0 {
                cpu.a = 0xFFFF;
            } else {
                cpu.a = cpu.a.wrapping_sub(1);
                cpu.pc = cpu.pc.wrapping_sub(3);
            }
        }
        0xEA => {}
        // WDM is a reserved two-byte NOP; consuming its operand is the
        // one thing an implementation must get right about it.
        0x42 => {
            let _ = cpu.fetch8(bus);
        }
        // ---- interrupts and halts (W6-01b) -----------------------------
        //
        // `BRK` and `COP` differ only in which vector they take, so they
        // share one path. Three things about them are easy to get wrong
        // and are each verified against the vectors below:
        //
        // 1. **Both consume a signature byte.** `BRK` is a two-byte
        //    instruction even though the second byte is discarded, and the
        //    pushed PC points PAST it.
        // 2. **Emulation mode pushes three bytes, native pushes four.**
        //    Native pushes `PBR` first and clears it; emulation has no
        //    `PBR` to push. (`00.e` shows exactly 3 writes, `00.n` shows 4.)
        // 3. **The stack wraps in page 1.** These are 6502-era
        //    instructions, so they use the wrapping helpers, not the flat
        //    ones the new 65816 stack ops need. `02.e` proves it: a push
        //    at `$0100` is followed by one at `$01FF`.
        //
        // The pushed `P` goes out unmodified. In emulation mode the `B`
        // bit shares `X`'s position and `X` is already forced set, so
        // there is nothing to set; in native mode `X` is the index-width
        // flag and forcing it would corrupt the state `RTI` restores.
        0x00 | 0x02 => {
            let _signature = cpu.fetch8(bus);
            if !cpu.e {
                cpu.push8(bus, cpu.pbr);
            }
            cpu.push16(bus, cpu.pc);
            cpu.push8(bus, cpu.p);
            cpu.set_flag(flags::I, true);
            cpu.set_flag(flags::D, false);
            cpu.pbr = 0;
            let vector: u16 = match (opcode, cpu.e) {
                (0x00, true) => 0xFFFE,
                (0x00, false) => 0xFFE6,
                (_, true) => 0xFFF4,
                (_, false) => 0xFFE4,
            };
            cpu.pc = am::read_pointer16(bus, am::Addr::from(vector));
        }
        // RTI pulls P, then PC, then — in native mode only — PBR.
        //
        // Unlike RTS it does NOT add one to the pulled address: the
        // interrupt sequence pushed the address to resume AT, not the one
        // before it.
        0x40 => {
            cpu.p = cpu.pull8(bus);
            if cpu.e {
                // M and X are not restorable in emulation mode; the pulled
                // byte's bits in those positions are the 6502's B and
                // unused flags. Forcing them keeps the register file in a
                // state the chip can actually be in.
                cpu.p |= flags::M | flags::X;
            } else if cpu.p & flags::X != 0 {
                // A pulled X flag narrows the index registers, and that
                // truncation is immediate — same rule as SEP.
                cpu.x &= 0xFF;
                cpu.y &= 0xFF;
            }
            cpu.pc = cpu.pull16(bus);
            if !cpu.e {
                cpu.pbr = cpu.pull8(bus);
            }
        }
        // WAI waits for an interrupt, STP halts until reset. Both are one
        // byte and both leave PC after themselves; what distinguishes them
        // is what wakes them, which is the scheduler's business rather
        // than the CPU's. `stopped` is the CPU's half of that contract;
        // `wai` (ticket W14-28) marks which of the two this is, because
        // the scheduler wakes `WAI` on a masked IRQ (datasheet: the
        // interrupt merely is not dispatched) but must never wake `STP`
        // that way.
        0xCB => {
            cpu.stopped = true;
            cpu.wai = true;
        }
        0xDB => {
            cpu.stopped = true;
            cpu.wai = false;
        }
        // UNREACHABLE AS OF W6-01b, and deliberately kept.
        //
        // All 256 opcodes are implemented, so clippy is right that no
        // value reaches this arm — hence the allow. It stays for two
        // reasons that outlive its current deadness:
        //
        // 1. A `match` on `u8` must be exhaustive, so any future refactor
        //    that drops an opcode arm has to reinstate a catch-all. This
        //    one already exists, already returns the opcode, and already
        //    reports rather than silently no-ops — which is the behaviour
        //    W6-01a went out of its way to guarantee.
        // 2. The vector suite DISCOVERS coverage from it: an opcode whose
        //    every case fails with `unimplemented opcode` is counted as a
        //    gap rather than a failure. Removing the Err path would not
        //    just delete a dead branch, it would delete that mechanism
        //    and make the coverage line unconditionally "256 of 256".
        #[allow(unreachable_patterns)]
        _ => return Err(opcode),
    }
    Ok(())
}

/// Index registers wrap within their CURRENT width.
fn index_step(reg: u16, step: i32, eight: bool) -> u16 {
    if eight {
        let v = (reg as u8).wrapping_add_signed(step as i8);
        (reg & 0xFF00) | u16::from(v) & 0x00FF
    } else {
        reg.wrapping_add_signed(step as i16)
    }
}

/// The ALU group's opcodes share one layout offset from their immediate
/// form, so the mode falls out of the low bits rather than a 15-arm match
/// repeated six times.
fn alu_mode(opcode: u8, immediate: u8) -> Mode {
    // Offsets from the immediate form. Every ALU group (`ORA` $09, `AND`
    // $29, `EOR` $49, `ADC` $69, `CMP` $C9, `SBC` $E9) uses the same
    // layout, which is why one table serves all six.
    //
    // These were ALL wrong except `Immediate`, `Long` and `LongX` — every
    // other entry was off by exactly $20, and since the fallback arm is a
    // catch-all rather than a panic, eleven addressing modes silently
    // resolved as `StackRelIndY`. The suite did not catch it because the
    // opcode list it fetched was itself derived by grepping this file's
    // match arms, and the grep only saw the first opcode per line. A
    // self-referential gate cannot fail this way loudly, which is why the
    // fetch script now takes all 256 opcodes instead.
    match opcode.wrapping_sub(immediate) {
        0x00 => Mode::Immediate,
        0xFC => Mode::Direct,
        0x0C => Mode::DirectX,
        0x04 => Mode::Absolute,
        0x14 => Mode::AbsoluteX,
        0x10 => Mode::AbsoluteY,
        0x06 => Mode::Long,
        0x16 => Mode::LongX,
        0xF8 => Mode::IndirectDpX,
        0x08 => Mode::IndirectDpY,
        0x09 => Mode::IndirectDp,
        0xFE => Mode::IndirectLong,
        0x0E => Mode::IndirectLongY,
        0xFA => Mode::StackRel,
        0x0A => Mode::StackRelIndY,
        // Unreachable for the six real ALU groups; a panic here would be
        // a decoding bug, not bad input, so it is better than a silent
        // wrong mode — which is precisely the bug this table just had.
        other => {
            unreachable!("alu_mode: opcode {opcode:#04X} is not an ALU form (offset {other:#04X})")
        }
    }
}

/// Shift/INC/DEC share a layout too: the accumulator form has no
/// address, the rest resolve normally.
fn shift_target(
    cpu: &mut Cpu,
    bus: &mut dyn CpuBus,
    opcode: u8,
    accumulator_form: u8,
) -> Option<(am::Addr, bool)> {
    if opcode == accumulator_form {
        return None;
    }
    let mode = match opcode & 0x1F {
        0x06 => Mode::Direct,
        0x16 => Mode::DirectX,
        0x0E => Mode::Absolute,
        _ => Mode::AbsoluteX,
    };
    resolve(cpu, bus, mode)
}
