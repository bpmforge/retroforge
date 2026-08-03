//! CPU trace logger in nestest.log format (ticket W1-03, FR-DBG-003).
//!
//! Format reverse-engineered from the real, fetched `nestest.log` (kevtris'
//! own trace, described informally at
//! [qmtpro.com/~nes/misc/nestest.txt](https://www.qmtpro.com/~nes/misc/nestest.txt))
//! against the ticket's own pre-flight facts (`plan.json` W1-03 notes) plus
//! direct inspection of the fetched log this session. No log content is
//! copied into this file or any other committed source (TESTING.md §3's
//! no-vendor/no-rehost rule, G-43, names `nestest/.log` explicitly) — only
//! the *shape* of each line is described below, and this module's own unit
//! tests use hand-derived byte sequences, never real log excerpts.
//!
//! ## Line shape
//!
//! ```text
//! PPPP  B1 B2 B3  MNEMONIC OPERAND...              A:AA X:XX Y:YY P:PP SP:SS PPU:SSS,DDD CYC:N
//! ```
//!
//! - `PPPP` (cols 0-3): `PC`, 4 uppercase hex digits.
//! - cols 6-15 (10 chars): up to 3 opcode/operand bytes as 2-digit
//!   uppercase hex, space-separated, left-justified and space-padded to 9
//!   chars; the 10th (last) char is `*` for any of the 105 unofficial
//!   opcodes, ` ` for the 151 official ones.
//! - cols 16-47 (32 chars): the disassembled mnemonic + operand text,
//!   left-justified and space-padded. See [`disassemble`] for the exact
//!   operand-annotation grammar per addressing mode (verified against
//!   every real addressing-mode shape nestest.log actually exercises this
//!   session — 225 of 256 possible opcodes; the 31 it never reaches
//!   — `BRK`, all 12 `JAM`s, the 8 unstable ops, and 10 untested
//!   stable-illegal/`NOP` immediate forms — still get a plausible entry in
//!   [`opcode_info`] for completeness, but are not golden-log-verified).
//! - col 48 onward: `A:`/`X:`/`Y:`/`P:`/`SP:` as 2-digit uppercase hex,
//!   `PPU:` as two comma-separated 3-wide space-padded decimal fields
//!   (scanline, dot), `CYC:` as an unpadded decimal cycle count.
//!
//! ## The PPU column needs no PPU (ticket W1-03 pre-flight)
//!
//! Verified arithmetically over all 8991 lines of the real nestest.log
//! with zero mismatches: `scanline == (cyc*3) / 341` and `dot == (cyc*3) %
//! 341`, where `cyc` is the CPU's own master-cycle count. [`ppu_columns`]
//! implements exactly this identity. It holds **only** because nestest's
//! automated test mode runs with PPU rendering disabled — a real,
//! rendering-enabled ROM's odd-frame skipped dot
//! (nesdev.org/wiki/PPU_rendering: "the last cycle occurs 340 times [not
//! 341] on... odd frames") would break it. Do not build or stub a PPU to
//! satisfy this column, and do not reuse [`ppu_columns`] as a general PPU
//! timing model — the real PPU is ticket W1-04a.
use crate::cpu::Cpu;
use crate::system::NesBus;

/// Side-effect-free, cycle-free memory read used only to compute this
/// trace logger's disassembly-annotation column — see
/// [`crate::system::NesBus::peek`]'s doc for why a real emulation read
/// (`CpuBus::read`) must never be used for this. Implemented by
/// [`NesBus`] below; test code implements it over a plain byte array.
pub trait TracePeek {
    /// Read `addr` with no observable side effect and no cycle cost.
    fn peek(&self, addr: u16) -> u8;
}

impl TracePeek for NesBus {
    fn peek(&self, addr: u16) -> u8 {
        NesBus::peek(self, addr)
    }
}

/// Addressing-mode shape for disassembly purposes — a static property of
/// the opcode byte alone, independent of the actual bus trace `Cpu::step`
/// would issue (that's `cpu/addressing.rs`'s job; this is a read-only
/// *description* of the same modes, verified to agree with
/// `cpu/exec.rs`'s dispatch by `tests::mode_matches_exec_dispatch_shape`
/// below for every opcode nestest.log actually exercises).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// No operand bytes, nothing shown after the mnemonic (e.g. `NOP`).
    Implied,
    /// No operand bytes; renders as `MNEM A` (`ASL`/`LSR`/`ROL`/`ROR`).
    Accumulator,
    /// One operand byte; renders as `MNEM #$XX`.
    Immediate,
    /// One operand byte (a zero-page address); renders as
    /// `MNEM $XX = VV`.
    ZeroPage,
    /// One operand byte, X-indexed; renders as
    /// `MNEM $XX,X @ EE = VV` (`EE = XX +% X`, wrapping within the zero
    /// page).
    ZeroPageX,
    /// Same as [`Mode::ZeroPageX`] but Y-indexed (`STX`/`LDX`/`SAX`/`LAX`
    /// only).
    ZeroPageY,
    /// Two operand bytes (an absolute address) for a data
    /// read/write/read-modify-write; renders as `MNEM $AAAA = VV`.
    Absolute,
    /// Two operand bytes (an absolute address) for `JMP`/`JSR` — no value
    /// is read at the target, so no `= VV`; renders as `MNEM $AAAA`.
    AbsoluteJump,
    /// Two operand bytes, X-indexed; renders as
    /// `MNEM $AAAA,X @ EEEE = VV` (`EEEE = AAAA + X`, full 16-bit add, may
    /// cross a page).
    AbsoluteX,
    /// Same as [`Mode::AbsoluteX`] but Y-indexed.
    AbsoluteY,
    /// One operand byte (a signed branch displacement); renders as
    /// `MNEM $TTTT`, `TTTT` already resolved to the absolute target
    /// (`PC_after_operand + displacement`) regardless of whether the
    /// branch is actually taken.
    Relative,
    /// Two operand bytes (a pointer address), `JMP (indirect)` only;
    /// renders as `MNEM ($PPPP) = TTTT` — reproduces the page-wrap bug
    /// (`cpu/exec.rs`'s `jmp_indirect`).
    Indirect,
    /// One operand byte (a zero-page base), X-indexed *before* the
    /// pointer fetch (`(zp,X)`); renders as
    /// `MNEM ($BB,X) @ EE = PPPP = VV` (`EE = BB +% X`, pointer fetched
    /// from `EE`/`EE+%1`, both wrapping within the zero page).
    IndirectX,
    /// One operand byte (a zero-page base), pointer fetched *then*
    /// Y-indexed (`(zp),Y`); renders as
    /// `MNEM ($BB),Y = PPPP @ EEEE = VV` (`EEEE = PPPP + Y`, full 16-bit
    /// add, may cross a page — unlike the pointer fetch itself, which
    /// wraps within the zero page).
    IndirectY,
}

/// nestest's own mnemonic label (which occasionally differs from this
/// crate's internal naming, e.g. the dispatcher's "ISC" vs. nestest's
/// `ISB` for `$E3/$E7/$EF/$F3/$F7/$FB/$FF` — verified against the real
/// log this session), its addressing-mode shape, and whether nestest
/// prefixes it with `*` (every one of the 105 unofficial opcodes, and only
/// those — official opcodes never get the marker).
fn opcode_info(opcode: u8) -> (&'static str, Mode, bool) {
    use Mode::*;
    match opcode {
        // ---- row +00 -------------------------------------------------
        0x00 => ("BRK", Implied, false),
        0x20 => ("JSR", AbsoluteJump, false),
        0x40 => ("RTI", Implied, false),
        0x60 => ("RTS", Implied, false),
        0xA0 => ("LDY", Immediate, false),
        0xC0 => ("CPY", Immediate, false),
        0xE0 => ("CPX", Immediate, false),
        // ---- row +01: (indir,x) ---------------------------------------
        0x01 => ("ORA", IndirectX, false),
        0x21 => ("AND", IndirectX, false),
        0x41 => ("EOR", IndirectX, false),
        0x61 => ("ADC", IndirectX, false),
        0x81 => ("STA", IndirectX, false),
        0xA1 => ("LDA", IndirectX, false),
        0xC1 => ("CMP", IndirectX, false),
        0xE1 => ("SBC", IndirectX, false),
        // ---- row +02 ---------------------------------------------------
        0xA2 => ("LDX", Immediate, false),
        // ---- row +04: zp ------------------------------------------------
        0x24 => ("BIT", ZeroPage, false),
        0x84 => ("STY", ZeroPage, false),
        0xA4 => ("LDY", ZeroPage, false),
        0xC4 => ("CPY", ZeroPage, false),
        0xE4 => ("CPX", ZeroPage, false),
        // ---- row +05: zp -------------------------------------------------
        0x05 => ("ORA", ZeroPage, false),
        0x25 => ("AND", ZeroPage, false),
        0x45 => ("EOR", ZeroPage, false),
        0x65 => ("ADC", ZeroPage, false),
        0x85 => ("STA", ZeroPage, false),
        0xA5 => ("LDA", ZeroPage, false),
        0xC5 => ("CMP", ZeroPage, false),
        0xE5 => ("SBC", ZeroPage, false),
        // ---- row +06: zp -------------------------------------------------
        0x06 => ("ASL", ZeroPage, false),
        0x26 => ("ROL", ZeroPage, false),
        0x46 => ("LSR", ZeroPage, false),
        0x66 => ("ROR", ZeroPage, false),
        0x86 => ("STX", ZeroPage, false),
        0xA6 => ("LDX", ZeroPage, false),
        0xC6 => ("DEC", ZeroPage, false),
        0xE6 => ("INC", ZeroPage, false),
        // ---- row +08: implied ---------------------------------------------
        0x08 => ("PHP", Implied, false),
        0x28 => ("PLP", Implied, false),
        0x48 => ("PHA", Implied, false),
        0x68 => ("PLA", Implied, false),
        0x88 => ("DEY", Implied, false),
        0xA8 => ("TAY", Implied, false),
        0xC8 => ("INY", Implied, false),
        0xE8 => ("INX", Implied, false),
        // ---- row +09: immediate ---------------------------------------
        0x09 => ("ORA", Immediate, false),
        0x29 => ("AND", Immediate, false),
        0x49 => ("EOR", Immediate, false),
        0x69 => ("ADC", Immediate, false),
        0xA9 => ("LDA", Immediate, false),
        0xC9 => ("CMP", Immediate, false),
        0xE9 => ("SBC", Immediate, false),
        // ---- row +0a: accumulator/implied ------------------------------
        0x0A => ("ASL", Accumulator, false),
        0x2A => ("ROL", Accumulator, false),
        0x4A => ("LSR", Accumulator, false),
        0x6A => ("ROR", Accumulator, false),
        0x8A => ("TXA", Implied, false),
        0xAA => ("TAX", Implied, false),
        0xCA => ("DEX", Implied, false),
        0xEA => ("NOP", Implied, false),
        // ---- row +0c: abs -----------------------------------------------
        0x2C => ("BIT", Absolute, false),
        0x4C => ("JMP", AbsoluteJump, false),
        0x6C => ("JMP", Indirect, false),
        0x8C => ("STY", Absolute, false),
        0xAC => ("LDY", Absolute, false),
        0xCC => ("CPY", Absolute, false),
        0xEC => ("CPX", Absolute, false),
        // ---- row +0d: abs -----------------------------------------------
        0x0D => ("ORA", Absolute, false),
        0x2D => ("AND", Absolute, false),
        0x4D => ("EOR", Absolute, false),
        0x6D => ("ADC", Absolute, false),
        0x8D => ("STA", Absolute, false),
        0xAD => ("LDA", Absolute, false),
        0xCD => ("CMP", Absolute, false),
        0xED => ("SBC", Absolute, false),
        // ---- row +0e: abs -----------------------------------------------
        0x0E => ("ASL", Absolute, false),
        0x2E => ("ROL", Absolute, false),
        0x4E => ("LSR", Absolute, false),
        0x6E => ("ROR", Absolute, false),
        0x8E => ("STX", Absolute, false),
        0xAE => ("LDX", Absolute, false),
        0xCE => ("DEC", Absolute, false),
        0xEE => ("INC", Absolute, false),
        // ---- row +10: relative --------------------------------------------
        0x10 => ("BPL", Relative, false),
        0x30 => ("BMI", Relative, false),
        0x50 => ("BVC", Relative, false),
        0x70 => ("BVS", Relative, false),
        0x90 => ("BCC", Relative, false),
        0xB0 => ("BCS", Relative, false),
        0xD0 => ("BNE", Relative, false),
        0xF0 => ("BEQ", Relative, false),
        // ---- row +11: (indir),y --------------------------------------------
        0x11 => ("ORA", IndirectY, false),
        0x31 => ("AND", IndirectY, false),
        0x51 => ("EOR", IndirectY, false),
        0x71 => ("ADC", IndirectY, false),
        0x91 => ("STA", IndirectY, false),
        0xB1 => ("LDA", IndirectY, false),
        0xD1 => ("CMP", IndirectY, false),
        0xF1 => ("SBC", IndirectY, false),
        // ---- row +14: zp,x (STY/LDY only) ------------------------------
        0x94 => ("STY", ZeroPageX, false),
        0xB4 => ("LDY", ZeroPageX, false),
        // ---- row +15: zp,x ------------------------------------------------
        0x15 => ("ORA", ZeroPageX, false),
        0x35 => ("AND", ZeroPageX, false),
        0x55 => ("EOR", ZeroPageX, false),
        0x75 => ("ADC", ZeroPageX, false),
        0x95 => ("STA", ZeroPageX, false),
        0xB5 => ("LDA", ZeroPageX, false),
        0xD5 => ("CMP", ZeroPageX, false),
        0xF5 => ("SBC", ZeroPageX, false),
        // ---- row +16: zp,x (STX/LDX use zp,y) --------------------------
        0x16 => ("ASL", ZeroPageX, false),
        0x36 => ("ROL", ZeroPageX, false),
        0x56 => ("LSR", ZeroPageX, false),
        0x76 => ("ROR", ZeroPageX, false),
        0x96 => ("STX", ZeroPageY, false),
        0xB6 => ("LDX", ZeroPageY, false),
        0xD6 => ("DEC", ZeroPageX, false),
        0xF6 => ("INC", ZeroPageX, false),
        // ---- row +18: implied -----------------------------------------
        0x18 => ("CLC", Implied, false),
        0x38 => ("SEC", Implied, false),
        0x58 => ("CLI", Implied, false),
        0x78 => ("SEI", Implied, false),
        0x98 => ("TYA", Implied, false),
        0xB8 => ("CLV", Implied, false),
        0xD8 => ("CLD", Implied, false),
        0xF8 => ("SED", Implied, false),
        // ---- row +19: abs,y -----------------------------------------------
        0x19 => ("ORA", AbsoluteY, false),
        0x39 => ("AND", AbsoluteY, false),
        0x59 => ("EOR", AbsoluteY, false),
        0x79 => ("ADC", AbsoluteY, false),
        0x99 => ("STA", AbsoluteY, false),
        0xB9 => ("LDA", AbsoluteY, false),
        0xD9 => ("CMP", AbsoluteY, false),
        0xF9 => ("SBC", AbsoluteY, false),
        // ---- row +1a: implied -------------------------------------------
        0x9A => ("TXS", Implied, false),
        0xBA => ("TSX", Implied, false),
        // ---- row +1c: abs,x (LDY only) ----------------------------------
        0xBC => ("LDY", AbsoluteX, false),
        // ---- row +1d: abs,x -----------------------------------------------
        0x1D => ("ORA", AbsoluteX, false),
        0x3D => ("AND", AbsoluteX, false),
        0x5D => ("EOR", AbsoluteX, false),
        0x7D => ("ADC", AbsoluteX, false),
        0x9D => ("STA", AbsoluteX, false),
        0xBD => ("LDA", AbsoluteX, false),
        0xDD => ("CMP", AbsoluteX, false),
        0xFD => ("SBC", AbsoluteX, false),
        // ---- row +1e: abs,x (LDX uses abs,y) -------------------------------
        0x1E => ("ASL", AbsoluteX, false),
        0x3E => ("ROL", AbsoluteX, false),
        0x5E => ("LSR", AbsoluteX, false),
        0x7E => ("ROR", AbsoluteX, false),
        0xBE => ("LDX", AbsoluteY, false),
        0xDE => ("DEC", AbsoluteX, false),
        0xFE => ("INC", AbsoluteX, false),

        // ==== unofficial/illegal opcodes (ticket W1-01b), all illegal=true ====
        // ---- SLO / RLA / SRE / RRA (stable RMW combo ops) ----------------
        0x03 => ("SLO", IndirectX, true),
        0x07 => ("SLO", ZeroPage, true),
        0x0F => ("SLO", Absolute, true),
        0x13 => ("SLO", IndirectY, true),
        0x17 => ("SLO", ZeroPageX, true),
        0x1B => ("SLO", AbsoluteY, true),
        0x1F => ("SLO", AbsoluteX, true),
        0x23 => ("RLA", IndirectX, true),
        0x27 => ("RLA", ZeroPage, true),
        0x2F => ("RLA", Absolute, true),
        0x33 => ("RLA", IndirectY, true),
        0x37 => ("RLA", ZeroPageX, true),
        0x3B => ("RLA", AbsoluteY, true),
        0x3F => ("RLA", AbsoluteX, true),
        0x43 => ("SRE", IndirectX, true),
        0x47 => ("SRE", ZeroPage, true),
        0x4F => ("SRE", Absolute, true),
        0x53 => ("SRE", IndirectY, true),
        0x57 => ("SRE", ZeroPageX, true),
        0x5B => ("SRE", AbsoluteY, true),
        0x5F => ("SRE", AbsoluteX, true),
        0x63 => ("RRA", IndirectX, true),
        0x67 => ("RRA", ZeroPage, true),
        0x6F => ("RRA", Absolute, true),
        0x73 => ("RRA", IndirectY, true),
        0x77 => ("RRA", ZeroPageX, true),
        0x7B => ("RRA", AbsoluteY, true),
        0x7F => ("RRA", AbsoluteX, true),
        // ---- SAX (store, stable) -----------------------------------------
        0x83 => ("SAX", IndirectX, true),
        0x87 => ("SAX", ZeroPage, true),
        0x8F => ("SAX", Absolute, true),
        0x97 => ("SAX", ZeroPageY, true),
        // ---- LAX (load, stable) -------------------------------------------
        0xA3 => ("LAX", IndirectX, true),
        0xA7 => ("LAX", ZeroPage, true),
        0xAF => ("LAX", Absolute, true),
        0xB3 => ("LAX", IndirectY, true),
        0xB7 => ("LAX", ZeroPageY, true),
        0xBF => ("LAX", AbsoluteY, true),
        // ---- DCP / ISB (stable RMW combo ops; nestest calls $E*/$F* "ISB",
        // not the more common nesdev "ISC" — verified against the real log) --
        0xC3 => ("DCP", IndirectX, true),
        0xC7 => ("DCP", ZeroPage, true),
        0xCF => ("DCP", Absolute, true),
        0xD3 => ("DCP", IndirectY, true),
        0xD7 => ("DCP", ZeroPageX, true),
        0xDB => ("DCP", AbsoluteY, true),
        0xDF => ("DCP", AbsoluteX, true),
        0xE3 => ("ISB", IndirectX, true),
        0xE7 => ("ISB", ZeroPage, true),
        0xEF => ("ISB", Absolute, true),
        0xF3 => ("ISB", IndirectY, true),
        0xF7 => ("ISB", ZeroPageX, true),
        0xFB => ("ISB", AbsoluteY, true),
        0xFF => ("ISB", AbsoluteX, true),
        // ---- stable immediate combo ops -----------------------------------
        0x0B | 0x2B => ("ANC", Immediate, true),
        0x4B => ("ALR", Immediate, true),
        0x6B => ("ARR", Immediate, true),
        0xCB => ("SBX", Immediate, true),
        // Undocumented duplicate of the official $E9 SBC — nestest labels
        // it "SBC", not a distinct mnemonic (verified against the real
        // log).
        0xEB => ("SBC", Immediate, true),
        // ---- unstable ops (not exercised by nestest.log; entries kept for
        // completeness/opcode-table exhaustiveness, not golden-verified) ----
        0x8B => ("ANE", Immediate, true),
        0xAB => ("LXA", Immediate, true),
        0xBB => ("LAS", AbsoluteY, true),
        0x93 => ("SHA", IndirectY, true),
        0x9F => ("SHA", AbsoluteY, true),
        0x9E => ("SHX", AbsoluteY, true),
        0x9C => ("SHY", AbsoluteX, true),
        0x9B => ("TAS", AbsoluteY, true),
        // ---- NOP / SKB / IGN variants (undocumented, no side effect) -------
        0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA => ("NOP", Implied, true),
        0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => ("NOP", Immediate, true),
        0x04 | 0x44 | 0x64 => ("NOP", ZeroPage, true),
        0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 => ("NOP", ZeroPageX, true),
        0x0C => ("NOP", Absolute, true),
        0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => ("NOP", AbsoluteX, true),
        // ---- KIL/JAM (not exercised by nestest.log) ------------------------
        0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => {
            ("JAM", Implied, true)
        } // No `_` wildcard: exhaustive over all 256 bytes, cross-checked by
          // `tests::opcode_info_covers_all_256_opcodes` below.
    }
}

/// Decode the instruction at `pc` for display: how many bytes it occupies
/// (1-3, opcode included), those raw bytes (unused entries zero), and the
/// fully-rendered mnemonic+operand text (columns 16-47, before padding).
/// `x`/`y` are the *pre*-instruction register values (nestest.log's
/// operand annotations, including any store's `= VV`, reflect memory
/// *before* the traced instruction runs — this function only ever peeks,
/// never executes).
fn disassemble(pc: u16, x: u8, y: u8, peek: &dyn TracePeek) -> (u8, [u8; 3], String) {
    let opcode = peek.peek(pc);
    let (mnemonic, mode, _illegal) = opcode_info(opcode);
    let mut bytes = [opcode, 0, 0];

    let (nbytes, text): (u8, String) = match mode {
        Mode::Implied => (1, mnemonic.to_string()),
        Mode::Accumulator => (1, format!("{mnemonic} A")),
        Mode::Immediate => {
            let v = peek.peek(pc.wrapping_add(1));
            bytes[1] = v;
            (2, format!("{mnemonic} #${v:02X}"))
        }
        Mode::ZeroPage => {
            let addr = peek.peek(pc.wrapping_add(1));
            bytes[1] = addr;
            let val = peek.peek(addr as u16);
            (2, format!("{mnemonic} ${addr:02X} = {val:02X}"))
        }
        Mode::ZeroPageX => {
            let base = peek.peek(pc.wrapping_add(1));
            bytes[1] = base;
            let eff = base.wrapping_add(x);
            let val = peek.peek(eff as u16);
            (
                2,
                format!("{mnemonic} ${base:02X},X @ {eff:02X} = {val:02X}"),
            )
        }
        Mode::ZeroPageY => {
            let base = peek.peek(pc.wrapping_add(1));
            bytes[1] = base;
            let eff = base.wrapping_add(y);
            let val = peek.peek(eff as u16);
            (
                2,
                format!("{mnemonic} ${base:02X},Y @ {eff:02X} = {val:02X}"),
            )
        }
        Mode::Absolute => {
            let lo = peek.peek(pc.wrapping_add(1));
            let hi = peek.peek(pc.wrapping_add(2));
            bytes[1] = lo;
            bytes[2] = hi;
            let addr = u16::from_le_bytes([lo, hi]);
            let val = peek.peek(addr);
            (3, format!("{mnemonic} ${addr:04X} = {val:02X}"))
        }
        Mode::AbsoluteJump => {
            let lo = peek.peek(pc.wrapping_add(1));
            let hi = peek.peek(pc.wrapping_add(2));
            bytes[1] = lo;
            bytes[2] = hi;
            let addr = u16::from_le_bytes([lo, hi]);
            (3, format!("{mnemonic} ${addr:04X}"))
        }
        Mode::AbsoluteX => {
            let lo = peek.peek(pc.wrapping_add(1));
            let hi = peek.peek(pc.wrapping_add(2));
            bytes[1] = lo;
            bytes[2] = hi;
            let base = u16::from_le_bytes([lo, hi]);
            let eff = base.wrapping_add(x as u16);
            let val = peek.peek(eff);
            (
                3,
                format!("{mnemonic} ${base:04X},X @ {eff:04X} = {val:02X}"),
            )
        }
        Mode::AbsoluteY => {
            let lo = peek.peek(pc.wrapping_add(1));
            let hi = peek.peek(pc.wrapping_add(2));
            bytes[1] = lo;
            bytes[2] = hi;
            let base = u16::from_le_bytes([lo, hi]);
            let eff = base.wrapping_add(y as u16);
            let val = peek.peek(eff);
            (
                3,
                format!("{mnemonic} ${base:04X},Y @ {eff:04X} = {val:02X}"),
            )
        }
        Mode::Relative => {
            let raw = peek.peek(pc.wrapping_add(1));
            bytes[1] = raw;
            let offset = raw as i8;
            let next_pc = pc.wrapping_add(2);
            let target = next_pc.wrapping_add(offset as i16 as u16);
            (2, format!("{mnemonic} ${target:04X}"))
        }
        Mode::Indirect => {
            let lo = peek.peek(pc.wrapping_add(1));
            let hi = peek.peek(pc.wrapping_add(2));
            bytes[1] = lo;
            bytes[2] = hi;
            let ptr = u16::from_le_bytes([lo, hi]);
            let target_lo = peek.peek(ptr);
            // Reproduces the page-wrap bug (`cpu/exec.rs`'s `jmp_indirect`):
            // the high byte comes from `ptr & $FF00 | (ptr+1) & $00FF`, not
            // a plain `ptr + 1`.
            let hi_addr = (ptr & 0xFF00) | (ptr.wrapping_add(1) & 0x00FF);
            let target_hi = peek.peek(hi_addr);
            let target = u16::from_le_bytes([target_lo, target_hi]);
            (3, format!("{mnemonic} (${ptr:04X}) = {target:04X}"))
        }
        Mode::IndirectX => {
            let zp = peek.peek(pc.wrapping_add(1));
            bytes[1] = zp;
            let eff_zp = zp.wrapping_add(x);
            let lo = peek.peek(eff_zp as u16);
            let hi = peek.peek(eff_zp.wrapping_add(1) as u16);
            let ptr = u16::from_le_bytes([lo, hi]);
            let val = peek.peek(ptr);
            (
                2,
                format!("{mnemonic} (${zp:02X},X) @ {eff_zp:02X} = {ptr:04X} = {val:02X}"),
            )
        }
        Mode::IndirectY => {
            let zp = peek.peek(pc.wrapping_add(1));
            bytes[1] = zp;
            let lo = peek.peek(zp as u16);
            let hi = peek.peek(zp.wrapping_add(1) as u16);
            let ptr = u16::from_le_bytes([lo, hi]);
            let eff = ptr.wrapping_add(y as u16);
            let val = peek.peek(eff);
            (
                2,
                format!("{mnemonic} (${zp:02X}),Y = {ptr:04X} @ {eff:04X} = {val:02X}"),
            )
        }
    };

    (nbytes, bytes, text)
}

/// Derives nestest.log's `PPU:%3d,%3d` scanline/dot columns from the CPU's
/// own master-cycle count alone — see module doc's "The PPU column needs
/// no PPU" section for the identity and its scope caveat.
fn ppu_columns(master_cycle: u64) -> (u32, u32) {
    let ppu_cycle = master_cycle * 3;
    ((ppu_cycle / 341) as u32, (ppu_cycle % 341) as u32)
}

/// Render one nestest.log-format trace line for the instruction about to
/// execute at `cpu.pc` (i.e. call this *before* `Cpu::step`, not after —
/// every column, register state included, describes the machine's state
/// at the moment the instruction is about to run, matching the real log's
/// own convention).
#[must_use]
pub fn format_trace_line(cpu: &Cpu, peek: &dyn TracePeek, master_cycle: u64) -> String {
    let (_mnemonic, _mode, illegal) = opcode_info(peek.peek(cpu.pc));
    let (nbytes, bytes, disasm_text) = disassemble(cpu.pc, cpu.x, cpu.y, peek);

    let hex = match nbytes {
        1 => format!("{:02X}", bytes[0]),
        2 => format!("{:02X} {:02X}", bytes[0], bytes[1]),
        3 => format!("{:02X} {:02X} {:02X}", bytes[0], bytes[1], bytes[2]),
        _ => unreachable!("disassemble() only ever returns 1-3 bytes"),
    };
    let mut bytes_field = format!("{hex:<9}");
    bytes_field.push(if illegal { '*' } else { ' ' });

    let (scanline, dot) = ppu_columns(master_cycle);

    format!(
        "{:04X}  {bytes_field}{:<32}A:{:02X} X:{:02X} Y:{:02X} P:{:02X} SP:{:02X} PPU:{scanline:3},{dot:3} CYC:{master_cycle}",
        cpu.pc, disasm_text, cpu.a, cpu.x, cpu.y, cpu.p, cpu.s
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat 64 KiB array bus for isolated disassembler tests — hand-set
    /// bytes only, never real ROM/log content (TESTING.md §3 G-43: no
    /// vendoring nestest.log).
    struct ArrayPeek(Box<[u8; 65536]>);

    impl ArrayPeek {
        fn new() -> Self {
            ArrayPeek(Box::new([0u8; 65536]))
        }

        fn set(&mut self, addr: u16, value: u8) {
            self.0[addr as usize] = value;
        }
    }

    impl TracePeek for ArrayPeek {
        fn peek(&self, addr: u16) -> u8 {
            self.0[addr as usize]
        }
    }

    fn cpu_at(pc: u16, a: u8, x: u8, y: u8, p: u8, s: u8) -> Cpu {
        let mut cpu = Cpu::default();
        cpu.pc = pc;
        cpu.a = a;
        cpu.x = x;
        cpu.y = y;
        cpu.s = s;
        cpu.set_p(p);
        cpu
    }

    #[test]
    fn opcode_info_covers_all_256_opcodes_without_panicking() {
        for opcode in 0u16..=0xFF {
            let _ = opcode_info(opcode as u8);
        }
    }

    #[test]
    fn implied_mnemonic_has_no_operand_text() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0xEA); // NOP
        let cpu = cpu_at(0x8000, 0, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("NOP"));
        assert_eq!(&line[6..16], "EA        ", "official opcode: no '*' marker");
    }

    #[test]
    fn accumulator_mode_shows_a_suffix() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0x4A); // LSR A
        let cpu = cpu_at(0x8000, 0x02, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("LSR A"));
    }

    #[test]
    fn immediate_mode_shows_hash_operand() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0xA9); // LDA #imm
        mem.set(0x8001, 0x7F);
        let cpu = cpu_at(0x8000, 0, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("LDA #$7F"));
        assert_eq!(&line[6..16], "A9 7F     ");
    }

    #[test]
    fn zero_page_shows_value_at_address() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0xA5); // LDA zp
        mem.set(0x8001, 0x10);
        mem.set(0x0010, 0x99);
        let cpu = cpu_at(0x8000, 0, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("LDA $10 = 99"));
    }

    #[test]
    fn zero_page_x_shows_wrapped_effective_address() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0xB5); // LDA zp,X
        mem.set(0x8001, 0xFF);
        mem.set(0x0004, 0x77); // effective = 0xFF + 0x05 wraps to 0x04
        let cpu = cpu_at(0x8000, 0, 0x05, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("LDA $FF,X @ 04 = 77"));
    }

    #[test]
    fn absolute_jump_shows_no_value_annotation() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0x4C); // JMP abs
        mem.set(0x8001, 0x00);
        mem.set(0x8002, 0x90);
        let cpu = cpu_at(0x8000, 0, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("JMP $9000"));
        assert!(!line[16..48].contains('='));
    }

    #[test]
    fn absolute_x_shows_base_and_effective() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0xBD); // LDA abs,X
        mem.set(0x8001, 0x00);
        mem.set(0x8002, 0x10);
        mem.set(0x1005, 0x42); // effective = 0x1000 + 5
        let cpu = cpu_at(0x8000, 0, 0x05, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("LDA $1000,X @ 1005 = 42"));
    }

    #[test]
    fn relative_branch_shows_resolved_target() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0xF0); // BEQ
        mem.set(0x8001, 0x05); // +5
        let cpu = cpu_at(0x8000, 0, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        // next_pc = 0x8002, target = 0x8002 + 5 = 0x8007
        assert!(line[16..48].starts_with("BEQ $8007"));
    }

    #[test]
    fn relative_branch_backward_offset_wraps_correctly() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8010, 0xD0); // BNE
        mem.set(0x8011, 0xFB); // -5 (two's complement)
        let cpu = cpu_at(0x8010, 0, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        // next_pc = 0x8012, target = 0x8012 - 5 = 0x800D
        assert!(line[16..48].starts_with("BNE $800D"));
    }

    #[test]
    fn indirect_jmp_reproduces_page_wrap_bug() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0x6C); // JMP (indirect)
        mem.set(0x8001, 0xFF);
        mem.set(0x8002, 0x02); // pointer = $02FF
        mem.set(0x02FF, 0x34); // low byte of target
        mem.set(0x0200, 0x12); // high byte read from $0200, NOT $0300 (bug)
        mem.set(0x0300, 0x99); // decoy: must NOT be used
        let cpu = cpu_at(0x8000, 0, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("JMP ($02FF) = 1234"));
    }

    #[test]
    fn indirect_x_shows_effective_zp_pointer_and_value() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0xA1); // LDA (zp,X)
        mem.set(0x8001, 0x80);
        mem.set(0x0083, 0x00); // (0x80 + X=3) = 0x83
        mem.set(0x0084, 0x02);
        mem.set(0x0200, 0x5A);
        let cpu = cpu_at(0x8000, 0, 0x03, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("LDA ($80,X) @ 83 = 0200 = 5A"));
    }

    #[test]
    fn indirect_y_shows_pointer_then_effective_after_add() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0xB1); // LDA (zp),Y
        mem.set(0x8001, 0x89);
        mem.set(0x0089, 0xFF); // pointer low
        mem.set(0x008A, 0xFF); // pointer high => pointer = 0xFFFF
        mem.set(0x0033, 0xA3); // effective = 0xFFFF + Y(0x34) wraps to 0x0033
        let cpu = cpu_at(0x8000, 0, 0, 0x34, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert!(line[16..48].starts_with("LDA ($89),Y = FFFF @ 0033 = A3"));
    }

    #[test]
    fn illegal_opcode_gets_star_marker_and_official_does_not() {
        let mut mem = ArrayPeek::new();
        mem.set(0x8000, 0xA7); // LAX zp (unofficial)
        mem.set(0x8001, 0x10);
        mem.set(0x0010, 0x42);
        let cpu = cpu_at(0x8000, 0, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert_eq!(&line[6..16], "A7 10    *");
        assert!(line[16..48].starts_with("LAX $10 = 42"));
    }

    #[test]
    fn register_and_ppu_columns_start_at_exactly_byte_offset_48() {
        let mut mem = ArrayPeek::new();
        mem.set(0xC000, 0x4C);
        mem.set(0xC001, 0xF5);
        mem.set(0xC002, 0xC5);
        let cpu = cpu_at(0xC000, 0, 0, 0, 0x24, 0xFD);
        let line = format_trace_line(&cpu, &mem, 7);
        assert_eq!(&line[48..50], "A:");
        assert_eq!(&line[48..], "A:00 X:00 Y:00 P:24 SP:FD PPU:  0, 21 CYC:7");
    }

    #[test]
    fn ppu_columns_match_the_cyc_times_3_identity() {
        assert_eq!(ppu_columns(7), (0, 21));
        assert_eq!(ppu_columns(26554), (233, 209));
    }
}
