//! The 6502 [`Isa`] for the code atlas (ticket W36-02).
//!
//! Instruction *length* comes from [`crate::trace::instruction_len`], the
//! same addressing-mode table the disassembler and the coverage recorder
//! use, so there is one length table in the crate. What this module adds is
//! the control-flow class of the ~30 opcodes that are not straight-line:
//! the eight relative branches, `JSR`, `JMP` (absolute and indirect), `RTS`,
//! `RTI`, `BRK` and the twelve `JAM` opcodes
//! (nesdev, "CPU unofficial opcodes" and "CPU instruction set"). The unit
//! tests cross-check every one of the 256 opcodes against the trace
//! disassembler's own mnemonic.
//!
//! The atlas builder itself lives in `rf-rebuild` and is generic; a caller
//! that sees both crates (`rf-harness`, `retroforge`) pairs them.

use rf_core_api::coverage::CoverageLog;
pub use rf_rebuild::{
    build_atlas as build_generic_atlas, entry, Atlas, Delta, Provenance, RebuildExec, TraceEntry,
    TraceKind, CACHE_ASSET, CACHE_PRODUCER, CACHE_SETTINGS,
};
use rf_rebuild::{DecodeMode, Decoded, Flow, Isa};

use crate::trace::instruction_len;

/// The atlas of an NES cartridge's PRG ROM (`prg` is
/// [`crate::NesRom::prg_rom`]) from a coverage recording: the generic builder
/// with the 6502 ISA. Re-exported here so a caller that already sees the NES
/// core does not need its own `rf-rebuild` dependency.
#[must_use]
pub fn build_atlas(prg: &[u8], log: &CoverageLog) -> Atlas {
    build_generic_atlas(&Mos6502, prg, log)
}

/// The NES CPU's instruction set (2A03 = 6502 without decimal mode; decode
/// is unaffected by that).
#[derive(Debug, Clone, Copy, Default)]
pub struct Mos6502;

impl Isa for Mos6502 {
    fn max_len(&self) -> usize {
        3
    }

    fn addr_mask(&self) -> u32 {
        0xFFFF
    }

    /// `mode` is ignored: 6502 operand widths are fixed by the opcode.
    fn decode(&self, bytes: &[u8], cpu: u32, _mode: DecodeMode) -> Option<Decoded> {
        let op = *bytes.first()?;
        let len = instruction_len(op);
        if bytes.len() < usize::from(len) {
            return None;
        }
        let abs = || u32::from(u16::from_le_bytes([bytes[1], bytes[2]]));
        let flow = match op {
            // BPL BMI BVC BVS BCC BCS BNE BEQ: signed displacement from the
            // address of the *next* instruction (nesdev, "Instruction
            // reference" branch section).
            0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 => {
                let disp = i32::from(bytes[1] as i8);
                Flow::Branch {
                    target: (cpu as i32 + 2 + disp) as u32 & 0xFFFF,
                }
            }
            0x20 => Flow::Call { target: abs() },
            0x4C => Flow::Jump { target: abs() },
            0x6C => Flow::IndirectJump,
            0x60 => Flow::Return,
            0x40 => Flow::ReturnInterrupt,
            0x00 => Flow::Break,
            // The twelve KIL/JAM opcodes halt the CPU until reset.
            0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => {
                Flow::Halt
            }
            _ => Flow::Fall,
        };
        Some(Decoded { len, flow })
    }

    /// Every 3-byte 6502 instruction is an absolute-family one: absolute,
    /// absolute,X, absolute,Y, `JMP (a)`, `JSR`, `JMP a` and the unofficial
    /// copies of those modes. Every 1-2 byte one is implied, accumulator,
    /// immediate, zero page (indexed or indirect) or relative, which stay
    /// (nesdev, "CPU addressing modes"). So the rule is by length.
    fn masked_operand(&self, insn: &[u8], _mode: DecodeMode) -> std::ops::Range<usize> {
        if insn.len() == 3 {
            1..3
        } else {
            0..0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::Cpu;
    use crate::trace::{format_trace_line, TracePeek};

    struct Bytes([u8; 3]);

    impl TracePeek for Bytes {
        fn peek(&self, addr: u16) -> u8 {
            self.0
                .get(usize::from(addr.wrapping_sub(0x8000)))
                .copied()
                .unwrap_or(0)
        }
    }

    /// Mnemonic and byte count the trace disassembler prints for `op`.
    fn trace_view(op: u8) -> (String, usize) {
        let mut cpu = Cpu::default();
        cpu.pc = 0x8000;
        let line = format_trace_line(&cpu, &Bytes([op, 0x10, 0x20]), 0);
        // cols 6-14: hex bytes; col 15: '*' for unofficial; 16..: text.
        let bytes = line[6..15].split_whitespace().count();
        let text = line[16..]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        (text, bytes)
    }

    #[test]
    fn lengths_match_instruction_len_and_the_trace_disassembler_for_all_256() {
        for op in 0..=255u8 {
            let d = Mos6502
                .decode(&[op, 0x10, 0x20], 0x8000, DecodeMode::default())
                .expect("three bytes always suffice");
            assert_eq!(d.len, instruction_len(op), "opcode {op:#04x}");
            assert_eq!(usize::from(d.len), trace_view(op).1, "opcode {op:#04x}");
        }
    }

    #[test]
    fn flow_class_matches_the_trace_mnemonic_for_all_256() {
        for op in 0..=255u8 {
            let (m, _) = trace_view(op);
            let d = Mos6502
                .decode(&[op, 0x10, 0x20], 0x8000, DecodeMode::default())
                .unwrap();
            let want = match m.as_str() {
                "BPL" | "BMI" | "BVC" | "BVS" | "BCC" | "BCS" | "BNE" | "BEQ" => "branch",
                "JSR" => "call",
                "JMP" if op == 0x4C => "jump",
                "JMP" => "indirect",
                "RTS" => "return",
                "RTI" => "rti",
                "BRK" => "break",
                "JAM" => "halt",
                _ => "fall",
            };
            let got = match d.flow {
                Flow::Branch { .. } => "branch",
                Flow::Call { .. } => "call",
                Flow::Jump { .. } => "jump",
                Flow::IndirectJump => "indirect",
                Flow::Return => "return",
                Flow::ReturnInterrupt => "rti",
                Flow::Break => "break",
                Flow::Halt => "halt",
                Flow::Fall => "fall",
            };
            assert_eq!(got, want, "opcode {op:#04x} ({m})");
        }
    }

    #[test]
    fn branch_target_is_relative_to_the_next_instruction_and_wraps() {
        let at = |cpu, disp: u8| {
            Mos6502
                .decode(&[0xD0, disp], cpu, DecodeMode::default())
                .unwrap()
                .flow
        };
        assert_eq!(at(0x8000, 0x05), Flow::Branch { target: 0x8007 });
        assert_eq!(at(0x8000, 0xFE), Flow::Branch { target: 0x8000 }); // branch to self
        assert_eq!(at(0xFFF0, 0x7F), Flow::Branch { target: 0x0071 }); // wraps past $FFFF
    }

    #[test]
    fn absolute_targets_and_truncation() {
        let d = Mos6502
            .decode(&[0x20, 0x34, 0x92], 0, DecodeMode::default())
            .unwrap();
        assert_eq!(d.flow, Flow::Call { target: 0x9234 });
        assert!(Mos6502
            .decode(&[0x20, 0x34], 0, DecodeMode::default())
            .is_none());
        assert!(Mos6502.decode(&[], 0, DecodeMode::default()).is_none());
    }
}
