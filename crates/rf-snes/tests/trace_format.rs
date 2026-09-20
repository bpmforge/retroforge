//! **The SNES trace is diffable against a reference emulator**
//! (ticket W13-02g; `docs/design/DEBUGGER.md` §2).
//!
//! The golden fixture at the bottom is criterion 3: it pins the format so
//! it cannot drift silently, the way `nestest.log` pins the NES one.

use rf_snes::cpu::{flags, Cpu};
use rf_snes::trace::{disassemble, format_flags, format_trace_line, opcode_info, Mode, TracePeek};

/// A flat 16 MiB address space the test writes bytes into.
struct Mem(Vec<u8>);

impl TracePeek for Mem {
    fn peek(&self, addr: u32) -> u8 {
        self.0.get(addr as usize).copied().unwrap_or(0)
    }
}

fn mem(at: u32, bytes: &[u8]) -> Mem {
    let mut m = Mem(vec![0; 0x0100_0000]);
    for (i, b) in bytes.iter().enumerate() {
        m.0[at as usize + i] = *b;
    }
    m
}

/// **The reason this disassembler takes `P` at all.** `LDA #$12` is two
/// bytes with `M` set and three with it clear — and a wrong length does
/// not merely misprint one line, it starts every following line
/// mid-instruction.
#[test]
fn immediate_width_follows_m_and_x_and_so_does_the_instruction_length() {
    // A9 = LDA #imm_m, A2 = LDX #imm_x.
    let m = mem(0x0000_8000, &[0xA9, 0x34, 0x12, 0xA2, 0x78, 0x56]);

    let (len, _, text) = disassemble(0x00, 0x8000, flags::M, false, &m);
    assert_eq!((len, text.as_str()), (2, "LDA #$34"), "M set: 8-bit");

    let (len, _, text) = disassemble(0x00, 0x8000, 0, false, &m);
    assert_eq!((len, text.as_str()), (3, "LDA #$1234"), "M clear: 16-bit");

    // X governs the index registers independently of M.
    let (len, _, text) = disassemble(0x00, 0x8003, flags::M, false, &m);
    assert_eq!(
        (len, text.as_str()),
        (3, "LDX #$5678"),
        "M does not narrow an index immediate"
    );
    let (len, _, text) = disassemble(0x00, 0x8003, flags::X, false, &m);
    assert_eq!((len, text.as_str()), (2, "LDX #$78"));
}

/// Emulation mode pins both widths to 8 whatever `P` holds — a trace
/// taken before the first `XCE` would otherwise read every immediate as
/// 16-bit.
#[test]
fn emulation_mode_forces_eight_bit_immediates_whatever_p_says() {
    let m = mem(0x0000_8000, &[0xA9, 0x34, 0x12]);
    let (len, _, text) = disassemble(0x00, 0x8000, 0, true, &m);
    assert_eq!((len, text.as_str()), (2, "LDA #$34"));
}

/// `SEP`/`REP` are always one byte, however the widths stand — they are
/// the instructions that CHANGE them.
#[test]
fn sep_and_rep_are_always_one_operand_byte() {
    let m = mem(0x0000_8000, &[0xE2, 0x30, 0xC2, 0x30]);
    let (len, _, text) = disassemble(0x00, 0x8000, 0, false, &m);
    assert_eq!((len, text.as_str()), (2, "SEP #$30"));
    let (len, _, text) = disassemble(0x00, 0x8002, 0, false, &m);
    assert_eq!((len, text.as_str()), (2, "REP #$30"));
}

/// A branch target is relative to the byte AFTER the instruction and
/// stays in the current program bank — a branch cannot cross banks.
#[test]
fn a_branch_target_is_relative_to_the_next_instruction() {
    // 80 FE = BRA -2, the classic self-loop.
    let m = mem(0x0001_8000, &[0x80, 0xFE]);
    let (_, _, text) = disassemble(0x01, 0x8000, 0, false, &m);
    assert_eq!(text, "BRA $8000", "-2 from $8002 is $8000");

    // 82 = BRL, sixteen-bit.
    let m = mem(0x0000_8000, &[0x82, 0xFD, 0xFF]);
    let (len, _, text) = disassemble(0x00, 0x8000, 0, false, &m);
    assert_eq!((len, text.as_str()), (3, "BRL $8000"));
}

/// Long addressing is three operand bytes and prints six hex digits — the
/// mode that makes a 65816 trace unmistakable from a 6502 one.
#[test]
fn long_addressing_prints_a_full_24_bit_address() {
    let m = mem(0x0000_8000, &[0xAF, 0x34, 0x12, 0x7E]);
    let (len, _, text) = disassemble(0x00, 0x8000, 0, false, &m);
    assert_eq!((len, text.as_str()), (4, "LDA $7E1234"));
}

/// The block-move operands are encoded destination-first and printed
/// source-first. Getting this backwards is the classic block-move bug.
#[test]
fn block_move_prints_source_first_though_it_encodes_destination_first() {
    // 54 = MVN, then dest bank $7E, source bank $80.
    let m = mem(0x0000_8000, &[0x54, 0x7E, 0x80]);
    let (len, _, text) = disassemble(0x00, 0x8000, 0, false, &m);
    assert_eq!((len, text.as_str()), (3, "MVN $80,$7E"));
}

/// Every one of the 256 opcodes is defined — the 65816 has no illegal
/// instructions, and a hole in the table would be a disassembler that
/// silently mis-lengths one instruction in 256.
#[test]
fn the_opcode_table_is_complete_and_has_no_placeholder_entries() {
    for op in 0..=255u8 {
        let (mnemonic, _) = opcode_info(op);
        assert!(
            mnemonic.len() == 3 && mnemonic.chars().all(|c| c.is_ascii_uppercase()),
            "opcode ${op:02X} has a suspicious mnemonic {mnemonic:?}"
        );
    }
    // Spot-checks at the corners and at the two reserved-looking slots.
    assert_eq!(opcode_info(0x00), ("BRK", Mode::Imm8));
    assert_eq!(opcode_info(0x42), ("WDM", Mode::Imm8));
    assert_eq!(opcode_info(0xEA), ("NOP", Mode::Imp));
    assert_eq!(opcode_info(0xFF), ("SBC", Mode::LongX));
}

/// `M` and `X` are the two bits that change what the NEXT line means, so
/// a reader has to be able to see them at a glance.
#[test]
fn the_flag_string_shows_m_and_x_in_native_mode_and_b_in_emulation() {
    assert_eq!(format_flags(0, false), "nvmxdizc");
    assert_eq!(format_flags(0xFF, false), "NVMXDIZC");
    // In emulation mode bit 5 is unused and bit 4 is B, not X.
    assert_eq!(format_flags(0, true), "nv-bdizc");
    assert_eq!(format_flags(flags::X, true), "nv-Bdizc");
}

/// **The golden line** (criterion 3). If this changes, the change was
/// either deliberate or a drift that would have broken diffing against a
/// reference emulator — and either way somebody has to say which.
#[test]
fn the_trace_line_format_is_pinned() {
    let m = mem(0x0000_8000, &[0xAD, 0x34, 0x12]);
    let cpu = Cpu {
        a: 0x1234,
        x: 0x0056,
        y: 0x0078,
        sp: 0x01FF,
        d: 0x0000,
        dbr: 0x7E,
        pbr: 0x00,
        pc: 0x8000,
        p: flags::M | flags::X,
        e: false,
        stopped: false,
        wai: false,
        internal_cycles: 0,
    };
    assert_eq!(
        format_trace_line(&cpu, &m),
        "00:8000  AD 34 12    LDA $1234       A:1234 X:0056 Y:0078 S:01FF D:0000 DB:7E P:nvMXdizc"
    );
}
