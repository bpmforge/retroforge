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
}

/// One decoded instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    /// Length in bytes, at least 1.
    pub len: u8,
    pub flow: Flow,
}

/// Extra state that changes how bytes decode. The 6502 ignores it. It exists
/// because the 65816's operand width depends on the M, X and E flags in
/// force at the instruction (W36-08): the SNES `Isa` reads its bits, the
/// atlas keys code by it, and the NES builder always passes the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
pub struct DecodeMode(pub u8);

/// An instruction set the atlas builder can walk.
pub trait Isa {
    /// Longest instruction in bytes (bounds how many bytes `decode` needs).
    fn max_len(&self) -> usize;

    /// Mask applied to computed CPU addresses (`0xFFFF` for the 6502).
    fn addr_mask(&self) -> u32;

    /// Decode the instruction at the start of `bytes`, located at CPU
    /// address `cpu` (needed for relative branches). `None` when `bytes` is
    /// too short to hold the whole instruction (end of image). Every byte
    /// value must otherwise decode to something: an undefined opcode is
    /// still an instruction that occupies bytes.
    fn decode(&self, bytes: &[u8], cpu: u32, mode: DecodeMode) -> Option<Decoded>;
}
