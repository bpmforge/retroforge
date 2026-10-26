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

pub mod emit;
pub mod lower;

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

/// [`build_atlas`] with the hardware vectors added as seeds (ticket W37-01):
/// RESET, NMI and IRQ/BRK are entry points of every cartridge, so they are
/// decoded statically even where no recording has run. An empty `log`
/// ([`CoverageLog::new`] of any length) gives the vectors-only atlas a game
/// can start under before anything has been recorded; its nodes are all
/// `Inferred`. Behaviour is unaffected: the executor is the interpreter with
/// a map, and an unplaced or wrong seed only costs a fallback.
#[must_use]
pub fn build_seeded_atlas(prg: &[u8], log: &CoverageLog) -> Atlas {
    let mut seeded = log.clone();
    add_vector_seeds(prg, &mut seeded);
    build_atlas(prg, &seeded)
}

/// Add the RESET/NMI/IRQ vectors of `prg` to `log` as interrupt entries.
///
/// Where the vectors are depends on the mapper's bank at `$F000-$FFFF`
/// (nesdev, "CPU memory map" and each mapper's page). If the log recorded
/// window maps, every distinct bank seen at `$F000` is read through the map
/// it was seen in. With none, the common layout is assumed: the last 16 KiB
/// at `$C000` and, for a 32 KiB or larger image, the 16 KiB before it at
/// `$8000` (a 16 KiB image is mirrored). That map is also added to the log so
/// the builder can place the seeds' static targets.
pub fn add_vector_seeds(prg: &[u8], log: &mut CoverageLog) {
    use rf_core_api::coverage::{
        CodeAddr, InterruptEntry, InterruptKind, WindowMap, NO_ROM, WINDOW_BASE, WINDOW_PAGES,
    };
    const PAGE: usize = 0x1000;
    if prg.len() < 4 * PAGE {
        return;
    }
    if log.windows.is_empty() {
        let mut m = WindowMap::EMPTY;
        for (page, slot) in m.0.iter_mut().enumerate() {
            let off = if prg.len() == 4 * PAGE {
                (page % 4) * PAGE
            } else if page >= 4 {
                prg.len() - 4 * PAGE + (page - 4) * PAGE
            } else {
                prg.len() - 8 * PAGE + page * PAGE
            };
            *slot = u32::try_from(off).unwrap_or(NO_ROM);
        }
        log.note_windows(m);
    }
    let mut seen_bases = Vec::new();
    for map in log.windows.clone() {
        let top = map.0[WINDOW_PAGES - 1];
        if top == NO_ROM || seen_bases.contains(&top) {
            continue;
        }
        seen_bases.push(top);
        let vec_at = top as usize + PAGE - 6;
        let Some(v) = prg.get(vec_at..vec_at + 6) else {
            continue;
        };
        let word = |i: usize| u16::from_le_bytes([v[i], v[i + 1]]);
        for (kind, cpu) in [
            (InterruptKind::Nmi, word(0)),
            (InterruptKind::Reset, word(2)),
            (InterruptKind::Irq, word(4)),
        ] {
            if cpu < WINDOW_BASE {
                continue;
            }
            let page = usize::from((cpu - WINDOW_BASE) >> 12);
            let rom = Some(map.0[page])
                .filter(|&b| b != NO_ROM)
                .map(|b| b + u32::from(cpu & 0xFFF))
                .filter(|&r| (r as usize) < prg.len());
            if rom.is_some() {
                log.note_interrupt(InterruptEntry {
                    kind,
                    target: CodeAddr {
                        cpu,
                        pbr: 0,
                        mode: 0,
                        rom,
                    },
                });
            }
        }
    }
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
        Some(Decoded::plain(len, flow))
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
                // 65816-only flows; the 6502 never produces them.
                Flow::JumpLong { .. }
                | Flow::CallLong { .. }
                | Flow::IndirectCall
                | Flow::ReturnLong => unreachable!("not a 6502 flow"),
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

    fn nrom(prg_len: usize, reset: u16, nmi: u16) -> Vec<u8> {
        let mut prg = vec![0xEAu8; prg_len]; // NOP sled
        let top = prg_len - 6;
        prg[top..top + 2].copy_from_slice(&nmi.to_le_bytes());
        prg[top + 2..top + 4].copy_from_slice(&reset.to_le_bytes());
        prg[top + 4..top + 6].copy_from_slice(&0u16.to_le_bytes());
        // reset: JSR $9000 ; JMP *   at the reset target.
        let r = prg_len - 0x4000 + usize::from(reset - 0xC000);
        prg[r..r + 6].copy_from_slice(&[0x20, 0x00, 0x90, 0x4C, r as u8, 0xC0]);
        prg
    }

    #[test]
    fn the_vectors_alone_seed_an_all_inferred_atlas() {
        let prg = nrom(0x8000, 0xC000, 0xC100);
        let a = build_seeded_atlas(&prg, &CoverageLog::new(0));
        assert_eq!(a.stats.observed_insns, 0);
        assert!(a.stats.inferred_insns > 2, "{:?}", a.stats);
        assert!(a.functions.len() >= 2, "reset and NMI at least");
        assert!(a.insns().all(|i| i.prov == Provenance::Inferred));
        // Reset and NMI are entries, tagged as such.
        let flags = |rom: u32| {
            a.functions
                .iter()
                .filter(|f| f.entry_rom == rom)
                .fold(0u8, |m, f| m | f.entry_kinds)
        };
        assert_ne!(flags(0x4000) & entry::RESET, 0);
        assert_ne!(flags(0x4100) & entry::NMI, 0);
    }

    #[test]
    fn a_16k_image_mirrors_and_a_tiny_one_is_left_alone() {
        let prg = nrom(0x4000, 0xC000, 0xC100);
        let a = build_seeded_atlas(&prg, &CoverageLog::new(0));
        assert!(a.functions.len() >= 2);
        let tiny = vec![0xEAu8; 0x2000];
        assert!(build_seeded_atlas(&tiny, &CoverageLog::new(0))
            .functions
            .is_empty());
    }

    #[test]
    fn recorded_windows_pick_the_bank_the_vectors_are_read_from() {
        use rf_core_api::coverage::{WindowMap, NO_ROM};
        // 64 KiB, 32 KiB banks (GxROM-like): bank 1 is on screen.
        let mut prg = vec![0xEAu8; 0x1_0000];
        prg[0xFFFA..0x1_0000].copy_from_slice(&[0x00, 0x90, 0x00, 0x80, 0x00, 0x90]);
        prg[0x7FFA..0x8000].copy_from_slice(&[0x00, 0xA0, 0x00, 0xA0, 0x00, 0xA0]);
        let mut log = CoverageLog::new(prg.len());
        let mut m = WindowMap::EMPTY;
        for (p, s) in m.0.iter_mut().enumerate() {
            *s = 0x8000 + (p as u32) * 0x1000;
        }
        log.note_windows(m);
        add_vector_seeds(&prg, &mut log);
        let reset = log
            .interrupts
            .iter()
            .find(|i| i.kind == rf_core_api::coverage::InterruptKind::Reset)
            .expect("reset seeded");
        assert_eq!((reset.target.cpu, reset.target.rom), (0x8000, Some(0x8000)));
        assert!(log.windows.iter().all(|w| w.0[0] != NO_ROM));
    }
}
