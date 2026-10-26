//! The semantic instruction IR (ticket W36-12; `docs/design/REBUILD_LAB.md`
//! section 4).
//!
//! One lowered instruction is a flat list of [`Op`]s in execution order. The
//! types name no console: a core's lowering decides what register `Reg(3)` or
//! flag `Flag(0x80)` means. What the IR makes explicit, and the cores' own
//! instruction code leaves implicit in control flow:
//!
//! - **every bus cycle** is an op ([`Op::Read`] / [`Op::Write`]), in hardware
//!   order, with its [`Cycle`] kind. A dummy read or an RMW write-back is
//!   marked, never dropped: devices tick on each cycle, so a tier that skips
//!   or merges cycles is not the same machine.
//! - **register and flag updates** are [`Op::Set`], [`Op::Alu`] and
//!   [`Op::SetFlag`]; a flag is computed by ALU ops, there is no "set N and Z
//!   from this value" shorthand.
//! - **conditional cycles** (page-cross re-reads, taken-branch cycles) are
//!   [`Op::SkipUnless`]: a forward-only skip, so a program always terminates
//!   in at most `ops.len()` steps.
//! - **interrupt sampling hooks** are [`Op::Machine`]: effects on the core's
//!   interrupt-poll bookkeeping that are part of the instruction's timing
//!   contract rather than of its data flow.
//!
//! Instructions the IR does not describe are [`Lowering::Interpret`]: the
//! executor runs the core's own code for them (6502 `JAM`, the unstable
//! unofficial opcodes whose result depends on analog constants).
//!
//! Values are `u16` throughout; [`Width`] says where an ALU result wraps.

/// A per-instruction scratch slot. Lowerings use at most [`MAX_TEMPS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tmp(pub u8);

/// A machine register, numbered by the ISA's lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reg(pub u8);

/// A one-bit flag, identified by the ISA's lowering (the 6502 uses the bit
/// mask in the status register).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Flag(pub u8);

/// Most temps one instruction may use (the evaluator's scratch size).
pub const MAX_TEMPS: usize = 16;

/// An operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Val {
    Imm(u16),
    Reg(Reg),
    Tmp(Tmp),
    /// A flag as 0 or 1.
    Flag(Flag),
}

/// Where a result goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dst {
    Reg(Reg),
    Tmp(Tmp),
}

/// Where an ALU result wraps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    W8,
    W16,
}

impl Width {
    /// Truncate `v` to this width.
    #[inline]
    #[must_use]
    pub const fn mask(self, v: u16) -> u16 {
        match self {
            Width::W8 => v & 0xFF,
            Width::W16 => v,
        }
    }
}

/// Binary (and, for `Not`/`Sext8`, unary on `a`) arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    /// Bitwise complement of `a`; `b` is ignored.
    Not,
    Shl,
    Shr,
    /// 1 when `a == b`, else 0.
    Eq,
    /// 1 when `a != b`, else 0.
    Ne,
    /// Unsigned `a >= b` as 0/1.
    Ge,
    /// Unsigned `a > b` as 0/1.
    Gt,
    /// Sign-extend the low byte of `a` to 16 bits; `b` is ignored.
    Sext8,
    /// `a` as the low byte and `b` as the high byte of a 16-bit value.
    Join,
}

/// What a bus cycle is for. Informational for the evaluator (every kind is a
/// real bus access); it is what the emitter names in its comments and what a
/// reviewer reads to check a lowering against the hardware cycle table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cycle {
    /// Opcode or operand byte fetched from the instruction stream.
    Fetch,
    /// Zero-page pointer byte.
    Pointer,
    /// The operand or a store/RMW target.
    Data,
    /// Stack access.
    Stack,
    /// Interrupt vector byte.
    Vector,
    /// The hardware performs this access but its value is thrown away
    /// (unindexed probe, wrong-page read, implied-mode re-read, RMW
    /// write-back of the old value, ...). Still a bus cycle.
    Dummy,
}

/// Effects on the core's interrupt-poll bookkeeping (see `cpu/exec.rs`
/// `run_cycled`). Each is a statement about *when* the instruction lets an
/// interrupt in, so they are ops, not folded into reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineOp {
    /// A taken branch that does not cross a page polls one cycle earlier
    /// (blargg `5-branch_delays_irq`).
    BranchPollsEarly,
    /// Re-take the interrupt-disable poll snapshot from the live flag, after
    /// the instruction changed it (`RTI`, interrupt entry).
    PollMaskFromFlags,
    /// This sequence is an interrupt entry: it does not poll for another
    /// interrupt itself.
    InterruptEntry,
    /// Pick the vector: `normal`, or the NMI vector when a pending NMI edge
    /// hijacks this entry. `hijack` consumes the edge.
    SelectVector {
        dst: Tmp,
        normal: u16,
        nmi: u16,
        hijackable: bool,
    },
}

/// One IR operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// A read cycle. `dst` receives the value when the program uses it.
    Read {
        dst: Option<Dst>,
        addr: Val,
        kind: Cycle,
    },
    /// A write cycle.
    Write {
        addr: Val,
        val: Val,
        kind: Cycle,
    },
    /// `dst = src`, truncated to the destination's width.
    Set {
        dst: Dst,
        src: Val,
    },
    /// `dst = (a op b)` wrapped to `w`.
    Alu {
        dst: Dst,
        op: AluOp,
        w: Width,
        a: Val,
        b: Val,
    },
    /// `flag = (src != 0)`.
    SetFlag {
        flag: Flag,
        src: Val,
    },
    /// If `cond == 0`, skip the next `n` ops. Forward only.
    SkipUnless {
        cond: Val,
        n: u8,
    },
    Machine(MachineOp),
}

/// Why an instruction is not lowered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpretReason {
    /// The CPU locks up (`JAM`/`KIL`): the core's own sequence keeps the bus
    /// trace right on every later step.
    Halt,
    /// The result depends on an analog constant or a bus-dependent corruption
    /// the core documents as its best guess; named so a reviewer sees which.
    Unstable(&'static str),
}

/// A lowered instruction, or the instruction to leave to the interpreter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lowering {
    Ir(Vec<Op>),
    Interpret(InterpretReason),
}

/// One instruction's lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsnIr {
    /// Opcode byte (first byte of the instruction).
    pub opcode: u8,
    /// Length in bytes in the instruction stream.
    pub len: u8,
    pub lowering: Lowering,
}

impl InsnIr {
    /// The ops, or `None` for an instruction left to the interpreter.
    #[must_use]
    pub fn ops(&self) -> Option<&[Op]> {
        match &self.lowering {
            Lowering::Ir(ops) => Some(ops),
            Lowering::Interpret(_) => None,
        }
    }

    /// Bus cycles the straight-line path takes with every conditional cycle
    /// skipped (`min`) and taken (`max`). `None` for interpreted code.
    #[must_use]
    pub fn cycle_range(&self) -> Option<(u32, u32)> {
        let ops = self.ops()?;
        let max = ops.iter().filter(|o| is_cycle(o)).count() as u32;
        let mut min = 0u32;
        let mut it = ops.iter();
        while let Some(op) = it.next() {
            match op {
                Op::SkipUnless { n, .. } => {
                    // Skipping consumes `n` further ops; `for` advances the
                    // iterator on every path, so this terminates.
                    for _ in 0..*n {
                        it.next();
                    }
                }
                o if is_cycle(o) => min += 1,
                _ => {}
            }
        }
        Some((min, max))
    }
}

fn is_cycle(op: &Op) -> bool {
    matches!(op, Op::Read { .. } | Op::Write { .. })
}

/// A located instruction of an atlas function, lowered with the operand bytes
/// the image holds (the emitter and block views use these; the evaluator
/// takes operands from the bus).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredInsn {
    pub rom: u32,
    pub cpu: u32,
    pub ir: InsnIr,
    /// The instruction's bytes as read from the image.
    pub bytes: Vec<u8>,
}

/// A straight-line run of lowered instructions (an atlas block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredBlock {
    pub start_cpu: u32,
    pub insns: Vec<LoweredInsn>,
}

/// A lowered atlas function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredFunction {
    pub entry_cpu: u32,
    pub blocks: Vec<LoweredBlock>,
}

/// Why a program is malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrError {
    TempOutOfRange(Tmp),
    /// A temp is read before any op wrote it.
    TempUnset(Tmp),
    /// A skip runs past the end of the program.
    SkipPastEnd,
}

/// Check a program: temps in range and written before they are read on every
/// path, skips stay inside the program. Evaluators may then index scratch
/// without checks.
///
/// # Errors
/// The first problem found.
pub fn validate(ops: &[Op]) -> Result<(), IrError> {
    // Temps written on *every* path so far. A write inside a skipped group
    // only counts after the group (when it was certainly unconditional the
    // group's own tail); to stay simple a temp written conditionally is
    // treated as set afterwards only if it was also set before the group.
    let mut set = [false; MAX_TEMPS];
    let mut idx = 0usize;
    let mut group_end = 0usize;
    let mut group_writes: Vec<usize> = Vec::new();
    for op in ops {
        if idx == group_end {
            group_writes.clear();
        }
        let mut use_val = |v: Val, set: &[bool; MAX_TEMPS], in_group: &[usize]| match v {
            Val::Tmp(t) => {
                let i = usize::from(t.0);
                if i >= MAX_TEMPS {
                    Err(IrError::TempOutOfRange(t))
                } else if !set[i] && !in_group.contains(&i) {
                    Err(IrError::TempUnset(t))
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        };
        let mut write =
            |d: Dst, set: &mut [bool; MAX_TEMPS], group: &mut Vec<usize>, cond: bool| {
                if let Dst::Tmp(t) = d {
                    let i = usize::from(t.0);
                    if i >= MAX_TEMPS {
                        return Err(IrError::TempOutOfRange(t));
                    }
                    if cond {
                        group.push(i);
                    } else {
                        set[i] = true;
                    }
                }
                Ok(())
            };
        let cond = idx < group_end;
        match *op {
            Op::Read { dst, addr, .. } => {
                use_val(addr, &set, &group_writes)?;
                if let Some(d) = dst {
                    write(d, &mut set, &mut group_writes, cond)?;
                }
            }
            Op::Write { addr, val, .. } => {
                use_val(addr, &set, &group_writes)?;
                use_val(val, &set, &group_writes)?;
            }
            Op::Set { dst, src } => {
                use_val(src, &set, &group_writes)?;
                write(dst, &mut set, &mut group_writes, cond)?;
            }
            Op::Alu { dst, a, b, .. } => {
                use_val(a, &set, &group_writes)?;
                use_val(b, &set, &group_writes)?;
                write(dst, &mut set, &mut group_writes, cond)?;
            }
            Op::SetFlag { src, .. } => use_val(src, &set, &group_writes)?,
            Op::SkipUnless { cond: c, n } => {
                use_val(c, &set, &group_writes)?;
                if idx + 1 + usize::from(n) > ops.len() {
                    return Err(IrError::SkipPastEnd);
                }
                group_end = group_end.max(idx + 1 + usize::from(n));
            }
            Op::Machine(MachineOp::SelectVector { dst, .. }) => {
                write(Dst::Tmp(dst), &mut set, &mut group_writes, cond)?;
            }
            Op::Machine(_) => {}
        }
        idx += 1;
        if idx == group_end {
            // Conditional writes are not reliable after the group.
            group_writes.clear();
        }
    }
    Ok(())
}
