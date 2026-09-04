//! 65C816 trace formatting (ticket W13-02g; `docs/design/DEBUGGER.md` §2:
//! "SNES format follows bsnes conventions (bank:addr, m/x-aware disasm)
//! for easy diffing against reference emulators").
//!
//! Mirrors `rf_nes::trace` deliberately: one formatter serving both the
//! on-screen trace viewer and any golden diff, so the two can never show
//! different text for the same instruction.
//!
//! ## Why a disassembler cannot be width-agnostic here
//!
//! On a 6502, `LDA #$12` is always two bytes. On a 65C816 it is two or
//! **three**, depending on the `M` flag — and `LDX #$12` on `X`. A
//! disassembler that guessed would not merely print the wrong operand: it
//! would report the wrong instruction *length*, and every subsequent line
//! of the trace would start mid-instruction. That is why
//! [`disassemble`] takes `p` and `e` rather than only bytes, and why the
//! immediate modes are split into [`Mode::ImmM`], [`Mode::ImmX`] and
//! [`Mode::Imm8`].
//!
//! **Emulation mode forces both widths to 8** regardless of what `P`
//! holds, which is why `e` is a parameter too: a trace taken before the
//! first `XCE` would otherwise read every immediate as 16-bit.
//!
//! Sources: the 65C816 opcode matrix as published in the W65C816S
//! datasheet and mirrored in fullsnes's CPU chapter (NFR-011 — hardware
//! claims cite published documentation).

use crate::cpu::{flags, Cpu};

/// How an opcode's operand is encoded — enough to know its length and to
/// render it in bsnes's operand syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No operand (`NOP`, `TAX`, `RTS`, ...).
    Imp,
    /// Accumulator (`ASL A`).
    Acc,
    /// `#$xx` or `#$xxxx` by the `M` flag.
    ImmM,
    /// `#$xx` or `#$xxxx` by the `X` flag.
    ImmX,
    /// Always one byte: `SEP`, `REP`, `BRK`, `COP`, `WDM`.
    Imm8,
    Dp,
    DpX,
    DpY,
    /// `($xx)`
    Ind,
    /// `($xx,X)`
    IndX,
    /// `($xx),Y`
    IndY,
    /// `[$xx]`
    IndLong,
    /// `[$xx],Y`
    IndLongY,
    /// `$xx,S`
    Stack,
    /// `($xx,S),Y`
    StackY,
    Abs,
    AbsX,
    AbsY,
    /// `$xxxxxx`
    Long,
    /// `$xxxxxx,X`
    LongX,
    /// `($xxxx)`
    AbsInd,
    /// `($xxxx,X)`
    AbsIndX,
    /// `[$xxxx]`
    AbsIndLong,
    /// Eight-bit signed branch.
    Rel,
    /// Sixteen-bit signed branch (`BRL`, `PER`).
    RelLong,
    /// Two operand bytes, destination then source (`MVN`, `MVP`).
    BlockMove,
    /// `PEA $xxxx` — an immediate word that is not width-dependent.
    Imm16,
}

impl Mode {
    /// Operand bytes, given the widths in force.
    ///
    /// `m8`/`x8` are the *effective* widths: emulation mode pins both to
    /// 8 whatever `P` says (see this module's doc).
    #[must_use]
    pub const fn operand_len(self, m8: bool, x8: bool) -> usize {
        match self {
            Mode::Imp | Mode::Acc => 0,
            Mode::ImmM => {
                if m8 {
                    1
                } else {
                    2
                }
            }
            Mode::ImmX => {
                if x8 {
                    1
                } else {
                    2
                }
            }
            Mode::Imm8
            | Mode::Dp
            | Mode::DpX
            | Mode::DpY
            | Mode::Ind
            | Mode::IndX
            | Mode::IndY
            | Mode::IndLong
            | Mode::IndLongY
            | Mode::Stack
            | Mode::StackY
            | Mode::Rel => 1,
            Mode::Abs
            | Mode::AbsX
            | Mode::AbsY
            | Mode::AbsInd
            | Mode::AbsIndX
            | Mode::AbsIndLong
            | Mode::RelLong
            | Mode::BlockMove
            | Mode::Imm16 => 2,
            Mode::Long | Mode::LongX => 3,
        }
    }
}

/// A non-perturbing read of the CPU's address space, bank included.
///
/// The same shape `rf_nes::trace::TracePeek` has, widened to 24 bits —
/// and with the same hard requirement: it must not touch a register with
/// a read side effect, or tracing would change the run it is describing.
pub trait TracePeek {
    fn peek(&self, addr: u32) -> u8;
}

include!("trace_table.rs");

/// Mnemonic and addressing mode for `opcode`.
#[must_use]
pub fn opcode_info(opcode: u8) -> (&'static str, Mode) {
    OPCODES[opcode as usize]
}

/// Disassemble the instruction at `pbr:pc`.
///
/// Returns the total instruction length (opcode included), the raw bytes,
/// and the rendered text.
#[must_use]
pub fn disassemble(
    pbr: u8,
    pc: u16,
    p: u8,
    e: bool,
    peek: &dyn TracePeek,
) -> (usize, Vec<u8>, String) {
    // Emulation mode forces both widths to 8 whatever P holds.
    let m8 = e || p & flags::M != 0;
    let x8 = e || p & flags::X != 0;

    let at = |i: usize| -> u8 {
        let addr = (u32::from(pbr) << 16) | u32::from(pc.wrapping_add(i as u16));
        peek.peek(addr)
    };
    let opcode = at(0);
    let (mnemonic, mode) = opcode_info(opcode);
    let n = mode.operand_len(m8, x8);
    let bytes: Vec<u8> = (0..=n).map(at).collect();

    let b1 = bytes.get(1).copied().unwrap_or(0);
    let b2 = bytes.get(2).copied().unwrap_or(0);
    let b3 = bytes.get(3).copied().unwrap_or(0);
    let word = u16::from(b1) | (u16::from(b2) << 8);
    let long = u32::from(word) | (u32::from(b3) << 16);

    let operand = match mode {
        Mode::Imp => String::new(),
        Mode::Acc => "A".to_string(),
        Mode::ImmM | Mode::ImmX => {
            if n == 1 {
                format!("#${b1:02X}")
            } else {
                format!("#${word:04X}")
            }
        }
        Mode::Imm8 => format!("#${b1:02X}"),
        Mode::Imm16 => format!("#${word:04X}"),
        Mode::Dp => format!("${b1:02X}"),
        Mode::DpX => format!("${b1:02X},X"),
        Mode::DpY => format!("${b1:02X},Y"),
        Mode::Ind => format!("(${b1:02X})"),
        Mode::IndX => format!("(${b1:02X},X)"),
        Mode::IndY => format!("(${b1:02X}),Y"),
        Mode::IndLong => format!("[${b1:02X}]"),
        Mode::IndLongY => format!("[${b1:02X}],Y"),
        Mode::Stack => format!("${b1:02X},S"),
        Mode::StackY => format!("(${b1:02X},S),Y"),
        Mode::Abs => format!("${word:04X}"),
        Mode::AbsX => format!("${word:04X},X"),
        Mode::AbsY => format!("${word:04X},Y"),
        Mode::Long => format!("${long:06X}"),
        Mode::LongX => format!("${long:06X},X"),
        Mode::AbsInd => format!("(${word:04X})"),
        Mode::AbsIndX => format!("(${word:04X},X)"),
        Mode::AbsIndLong => format!("[${word:04X}]"),
        Mode::Rel => {
            // The target is relative to the byte AFTER the instruction,
            // and it stays in the current program bank — a branch cannot
            // cross banks, which is why this wraps at 16 bits rather than
            // adding into a 24-bit address.
            let target = pc.wrapping_add(2).wrapping_add(b1 as i8 as u16);
            format!("${target:04X}")
        }
        Mode::RelLong => {
            let target = pc.wrapping_add(3).wrapping_add(word);
            format!("${target:04X}")
        }
        // Destination bank first in the encoding, and bsnes prints it
        // source-first — `MVN src,dst` — which is the order the mnemonic
        // reads in. Getting this backwards is the classic block-move bug.
        Mode::BlockMove => format!("${b2:02X},${b1:02X}"),
    };

    let text = if operand.is_empty() {
        mnemonic.to_string()
    } else {
        format!("{mnemonic} {operand}")
    };
    (n + 1, bytes, text)
}

/// Render `P` as bsnes does: a letter per set flag, lower-case when clear.
///
/// The `M`/`X` positions are what make a trace diffable at a glance —
/// they are the two bits that change what the *next* line means.
#[must_use]
pub fn format_flags(p: u8, e: bool) -> String {
    let bit = |mask: u8, set: char, clear: char| if p & mask != 0 { set } else { clear };
    let mut s = String::with_capacity(9);
    s.push(bit(flags::N, 'N', 'n'));
    s.push(bit(flags::V, 'V', 'v'));
    if e {
        // In emulation mode bit 5 is unused and bit 4 is B, not X — so
        // printing "M" and "X" there would be describing a register the
        // chip does not have in this mode.
        s.push('-');
        s.push(bit(flags::X, 'B', 'b'));
    } else {
        s.push(bit(flags::M, 'M', 'm'));
        s.push(bit(flags::X, 'X', 'x'));
    }
    s.push(bit(flags::D, 'D', 'd'));
    s.push(bit(flags::I, 'I', 'i'));
    s.push(bit(flags::Z, 'Z', 'z'));
    s.push(bit(flags::C, 'C', 'c'));
    s
}

/// One trace line, bsnes-shaped.
///
/// `bank:addr  raw  disasm  A:.... X:.... Y:.... S:.... D:.... DB:.. P:nvmxdizc`
#[must_use]
pub fn format_trace_line(cpu: &Cpu, peek: &dyn TracePeek) -> String {
    let (_len, bytes, text) = disassemble(cpu.pbr, cpu.pc, cpu.p, cpu.e, peek);
    let raw: String = bytes
        .iter()
        .map(|b| format!("{b:02X} "))
        .collect::<String>();
    format!(
        "{:02X}:{:04X}  {:<12}{:<16}A:{:04X} X:{:04X} Y:{:04X} S:{:04X} D:{:04X} DB:{:02X} P:{}",
        cpu.pbr,
        cpu.pc,
        raw.trim_end(),
        text,
        cpu.a,
        cpu.x,
        cpu.y,
        cpu.sp,
        cpu.d,
        cpu.dbr,
        format_flags(cpu.p, cpu.e)
    )
}
