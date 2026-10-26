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
pub const MAX_TEMPS: usize = 32;

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
    /// A read at an address that may be wrong: the operand read itself when
    /// an indexed access stays in its page, a thrown-away read when it does
    /// not (a later `Data` read then follows).
    Probe,
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
        let use_val = |v: Val, set: &[bool; MAX_TEMPS], in_group: &[usize]| match v {
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
        let write = |d: Dst, set: &mut [bool; MAX_TEMPS], group: &mut Vec<usize>, cond: bool| {
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

/// The meaning of an ALU op, in one place: the evaluator, the constant folder
/// and the emitter's prelude all agree because they share this definition.
#[inline]
#[must_use]
pub fn alu_eval(op: AluOp, w: Width, a: u16, b: u16) -> u16 {
    let r = match op {
        AluOp::Add => a.wrapping_add(b),
        AluOp::Sub => a.wrapping_sub(b),
        AluOp::And => a & b,
        AluOp::Or => a | b,
        AluOp::Xor => a ^ b,
        AluOp::Not => !a,
        AluOp::Shl => a.wrapping_shl(u32::from(b)),
        AluOp::Shr => a.wrapping_shr(u32::from(b)),
        AluOp::Eq => u16::from(a == b),
        AluOp::Ne => u16::from(a != b),
        AluOp::Ge => u16::from(a >= b),
        AluOp::Gt => u16::from(a > b),
        AluOp::Sext8 => i16::from(a as u8 as i8) as u16,
        AluOp::Join => (a & 0xFF) | ((b & 0xFF) << 8),
    };
    w.mask(r)
}

/// Specialise a program to the operand bytes an image holds: each read of a
/// [`Cycle::Fetch`] byte at the program counter keeps its bus cycle but its
/// value becomes a constant, and ALU ops whose inputs are then constant are
/// folded away. `operands` is the instruction's bytes after the opcode.
///
/// The result is for readers and emitters (addresses like `$2002` become
/// visible); the evaluator runs the generic program and takes operand bytes
/// from the bus, so a bank switch under the instruction cannot be missed.
/// Constants are only propagated outside conditional groups; a temp written
/// inside one is unknown afterwards.
#[must_use]
pub fn bake(ops: &[Op], operands: &[u8], pc: Reg) -> Vec<Op> {
    let mut known: [Option<u16>; MAX_TEMPS] = [None; MAX_TEMPS];
    let mut out = Vec::with_capacity(ops.len());
    let mut fetched = operands.iter();
    let mut idx = 0usize;
    let mut group_end = 0usize;
    let sub = |v: Val, known: &[Option<u16>; MAX_TEMPS]| match v {
        Val::Tmp(t) => known[usize::from(t.0)].map_or(v, Val::Imm),
        _ => v,
    };
    for op in ops {
        let in_group = idx < group_end;
        idx += 1;
        let new = match *op {
            Op::Read { dst, addr, kind } => {
                let addr = sub(addr, &known);
                if kind == Cycle::Fetch && addr == Val::Reg(pc) {
                    if let (Some(Dst::Tmp(t)), Some(&byte)) = (dst, fetched.next()) {
                        if !in_group {
                            known[usize::from(t.0)] = Some(u16::from(byte));
                        }
                        out.push(Op::Read {
                            dst: None,
                            addr,
                            kind,
                        });
                        continue;
                    }
                }
                if let Some(Dst::Tmp(t)) = dst {
                    known[usize::from(t.0)] = None;
                }
                Op::Read { dst, addr, kind }
            }
            Op::Write { addr, val, kind } => Op::Write {
                addr: sub(addr, &known),
                val: sub(val, &known),
                kind,
            },
            Op::Set { dst, src } => {
                let src = sub(src, &known);
                if let (Dst::Tmp(t), Val::Imm(c), false) = (dst, src, in_group) {
                    known[usize::from(t.0)] = Some(c);
                    continue;
                }
                if let Dst::Tmp(t) = dst {
                    known[usize::from(t.0)] = None;
                }
                Op::Set { dst, src }
            }
            Op::Alu { dst, op, w, a, b } => {
                let (a, b) = (sub(a, &known), sub(b, &known));
                if let (Dst::Tmp(t), Val::Imm(x), Val::Imm(y), false) = (dst, a, b, in_group) {
                    known[usize::from(t.0)] = Some(alu_eval(op, w, x, y));
                    continue;
                }
                if let Dst::Tmp(t) = dst {
                    known[usize::from(t.0)] = None;
                }
                Op::Alu { dst, op, w, a, b }
            }
            Op::SetFlag { flag, src } => Op::SetFlag {
                flag,
                src: sub(src, &known),
            },
            Op::SkipUnless { cond, n } => {
                group_end = group_end.max(idx + usize::from(n));
                Op::SkipUnless {
                    cond: sub(cond, &known),
                    n,
                }
            }
            Op::Machine(m) => {
                if let MachineOp::SelectVector { dst, .. } = m {
                    known[usize::from(dst.0)] = None;
                }
                Op::Machine(m)
            }
        };
        out.push(new);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PC: Reg = Reg(4);

    #[test]
    fn alu_wraps_to_width_and_shares_one_definition() {
        assert_eq!(alu_eval(AluOp::Add, Width::W8, 0xFF, 2), 1);
        assert_eq!(alu_eval(AluOp::Add, Width::W16, 0xFF, 2), 0x101);
        assert_eq!(alu_eval(AluOp::Sub, Width::W8, 0, 1), 0xFF);
        assert_eq!(alu_eval(AluOp::Not, Width::W8, 0x0F, 0), 0xF0);
        assert_eq!(alu_eval(AluOp::Sext8, Width::W16, 0x80, 0), 0xFF80);
        assert_eq!(alu_eval(AluOp::Sext8, Width::W16, 0x7F, 0), 0x007F);
        assert_eq!(alu_eval(AluOp::Join, Width::W16, 0x34, 0x12), 0x1234);
        assert_eq!(alu_eval(AluOp::Gt, Width::W16, 0x100, 0xFF), 1);
        assert_eq!(alu_eval(AluOp::Ge, Width::W8, 3, 3), 1);
    }

    #[test]
    fn validate_rejects_unset_temps_and_runaway_skips() {
        let read_unset = [Op::Write {
            addr: Val::Tmp(Tmp(0)),
            val: Val::Imm(0),
            kind: Cycle::Data,
        }];
        assert_eq!(validate(&read_unset), Err(IrError::TempUnset(Tmp(0))));
        let skip = [Op::SkipUnless {
            cond: Val::Imm(1),
            n: 2,
        }];
        assert_eq!(validate(&skip), Err(IrError::SkipPastEnd));
        let wide = [Op::Set {
            dst: Dst::Tmp(Tmp(MAX_TEMPS as u8)),
            src: Val::Imm(0),
        }];
        assert_eq!(
            validate(&wide),
            Err(IrError::TempOutOfRange(Tmp(MAX_TEMPS as u8)))
        );
    }

    #[test]
    fn a_temp_written_only_inside_a_skipped_group_is_not_usable_after_it() {
        let ops = [
            Op::SkipUnless {
                cond: Val::Imm(0),
                n: 1,
            },
            Op::Set {
                dst: Dst::Tmp(Tmp(0)),
                src: Val::Imm(1),
            },
            Op::Write {
                addr: Val::Tmp(Tmp(0)),
                val: Val::Imm(0),
                kind: Cycle::Data,
            },
        ];
        assert_eq!(validate(&ops), Err(IrError::TempUnset(Tmp(0))));
    }

    #[test]
    fn cycle_range_counts_conditional_cycles_only_in_max() {
        let ops = vec![
            Op::Read {
                dst: None,
                addr: Val::Imm(0),
                kind: Cycle::Dummy,
            },
            Op::SkipUnless {
                cond: Val::Imm(0),
                n: 1,
            },
            Op::Read {
                dst: None,
                addr: Val::Imm(0),
                kind: Cycle::Dummy,
            },
        ];
        let insn = InsnIr {
            opcode: 0,
            len: 1,
            lowering: Lowering::Ir(ops),
        };
        assert_eq!(insn.cycle_range(), Some((1, 2)));
        let jam = InsnIr {
            opcode: 2,
            len: 1,
            lowering: Lowering::Interpret(InterpretReason::Halt),
        };
        assert_eq!(jam.cycle_range(), None);
    }

    #[test]
    fn bake_turns_fetched_operands_into_constants_but_keeps_the_cycles() {
        // t0 = fetch; t1 = t0 + 1 (folds); write [t1] <- t0
        let ops = [
            Op::Read {
                dst: Some(Dst::Tmp(Tmp(0))),
                addr: Val::Reg(PC),
                kind: Cycle::Fetch,
            },
            Op::Alu {
                dst: Dst::Tmp(Tmp(1)),
                op: AluOp::Add,
                w: Width::W8,
                a: Val::Tmp(Tmp(0)),
                b: Val::Imm(1),
            },
            Op::Write {
                addr: Val::Tmp(Tmp(1)),
                val: Val::Tmp(Tmp(0)),
                kind: Cycle::Data,
            },
        ];
        let baked = bake(&ops, &[0x41], PC);
        assert_eq!(baked.len(), 2, "the fold dropped one op");
        assert!(matches!(
            baked[0],
            Op::Read {
                dst: None,
                kind: Cycle::Fetch,
                ..
            }
        ));
        assert_eq!(
            baked[1],
            Op::Write {
                addr: Val::Imm(0x42),
                val: Val::Imm(0x41),
                kind: Cycle::Data
            }
        );
    }
}
