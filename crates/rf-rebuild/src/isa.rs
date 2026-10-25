//! The decode contract the atlas builder is generic over (ticket W36-02;
//! `docs/design/REBUILD_LAB.md` §6).
//!
//! A core owns its opcode table, so the core crate implements [`Isa`] and
//! this crate never learns a mnemonic: the atlas only needs to know how long
//! an instruction is and where control can go next.

use bincode::{Decode, Encode};

/// Where control goes after an instruction. Targets are CPU addresses (the
/// atlas resolves them to ROM offsets through the recorded bank windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode)]
pub enum Flow {
    /// Continues at the next instruction.
    Fall,
    /// Conditional relative branch: falls through or goes to `target`.
    Branch { target: u32 },
    /// Unconditional jump to a statically known address.
    Jump { target: u32 },
    /// Subroutine call to a statically known address; execution is expected
    /// to resume at the next instruction.
    Call { target: u32 },
    /// Jump through a pointer: the target is only known from the recording.
    IndirectJump,
    /// Subroutine return (target comes off the stack).
    Return,
    /// Return from interrupt.
    ReturnInterrupt,
    /// Software interrupt (`BRK`): goes through a vector.
    Break,
    /// The CPU locks up or leaves the instruction stream (6502 `JAM`).
    Halt,
    /// 65816 `JML`/`JMP long`: jump to a full 24-bit address (`Jump` is a
    /// 16-bit target inside the current program bank).
    JumpLong { target: u32 },
    /// 65816 `JSL`: call a full 24-bit address; resumes at the next
    /// instruction after an `RTL`.
    CallLong { target: u32 },
    /// 65816 `JSR (abs,X)`: call through a table; resumes at the next
    /// instruction.
    IndirectCall,
    /// 65816 `RTL`: return to a 24-bit address off the stack.
    ReturnLong,
}

/// What an instruction does to the [`DecodeMode`] of the code after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeAfter {
    /// Unchanged (every 6502 instruction; most 65816 ones).
    Same,
    /// Becomes this mode (a 65816 `REP`/`SEP` with a known immediate).
    Set(DecodeMode),
    /// Not known from the bytes (`XCE`, `PLP`): the code after it is
    /// decoded only in modes the recording saw it run in.
    Unknown,
}

/// One decoded instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    /// Length in bytes, at least 1.
    pub len: u8,
    pub flow: Flow,
    /// The decode mode in force after this instruction.
    pub after: ModeAfter,
}

impl Decoded {
    /// An instruction that leaves the decode mode alone.
    #[must_use]
    pub const fn plain(len: u8, flow: Flow) -> Self {
        Self {
            len,
            flow,
            after: ModeAfter::Same,
        }
    }
}

/// Extra state that changes how bytes decode: an index below
/// [`Isa::mode_count`]. The 6502 has one mode (0) and ignores it. The 65816's
/// operand width depends on the M, X and E flags in force at the instruction
/// (W36-08), which makes five modes (0 = emulation; 1..=4 native, see
/// `rf_core_api::coverage`); the atlas keys code by `(ROM offset, mode)`, so
/// the same bytes entered with two widths are two functions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub struct DecodeMode(pub u8);

/// An instruction set the atlas builder can walk.
pub trait Isa {
    /// Longest instruction in bytes (bounds how many bytes `decode` needs).
    fn max_len(&self) -> usize;

    /// Mask applied to computed CPU addresses (`0xFFFF` for the 6502). The
    /// bits above it name the program bank and never change by falling
    /// through or branching (65816: a fetch at `$xx:FFFF` is followed by
    /// `$xx:0000`).
    fn addr_mask(&self) -> u32;

    /// How many decode modes exist (1 for the 6502, 5 for the 65816).
    fn mode_count(&self) -> usize {
        1
    }

    /// True when the cartridge maps CPU addresses to ROM statically (SNES
    /// LoROM/HiROM). The builder then resolves targets with
    /// [`Self::rom_offset`] instead of the recorded bank windows.
    fn fixed_mapping(&self) -> bool {
        false
    }

    /// The ROM offset the 24-bit CPU address `cpu` reads, `None` when it is
    /// not ROM. Only meaningful when [`Self::fixed_mapping`] is true.
    fn rom_offset(&self, cpu: u32) -> Option<u32> {
        let _ = cpu;
        None
    }

    /// A canonical CPU address that reads ROM offset `rom` (the lowest bank
    /// alias the ISA prefers). Only meaningful when [`Self::fixed_mapping`].
    fn cpu_for_rom(&self, rom: u32) -> u32 {
        rom
    }

    /// Decode the instruction at the start of `bytes`, located at CPU
    /// address `cpu` (needed for relative branches). `None` when `bytes` is
    /// too short to hold the whole instruction (end of image). Every byte
    /// value must otherwise decode to something: an undefined opcode is
    /// still an instruction that occupies bytes.
    fn decode(&self, bytes: &[u8], cpu: u32, mode: DecodeMode) -> Option<Decoded>;

    /// The shape-hash mask rule (ticket W36-05). `insn` is one whole
    /// instruction, opcode first. Returns the range of `insn` holding an
    /// operand that names an **absolute address** (absolute, absolute
    /// indexed, indirect, long): bytes that change when the code is moved or
    /// the surrounding build is re-linked, so the function's shape hash
    /// ignores them. Zero-page, immediate and relative operands are kept (a
    /// relative branch is the same after a move; an immediate or a
    /// zero-page address is the game's own constant). An empty range masks
    /// nothing.
    ///
    /// The ISA decides because it knows its addressing modes: the 65816
    /// masks 16- and 24-bit absolute and long operands but keeps immediates
    /// of the same width, a distinction only the decode mode can make.
    fn masked_operand(&self, insn: &[u8], mode: DecodeMode) -> std::ops::Range<usize>;
}
