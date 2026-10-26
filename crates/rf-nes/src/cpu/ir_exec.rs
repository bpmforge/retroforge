//! The IR interpreter tier (ticket W36-12): evaluates a lowered instruction
//! (`rf_rebuild::ir`, lowering in `crate::rebuild::lower`) in place of
//! `exec::dispatch`'s hand-written arms.
//!
//! It runs inside `exec::run_cycled`'s body, so the cycle counting, the
//! interrupt-line sampling after every bus operation and the commit of the
//! poll results are the same code the interpreter uses. All the IR adds is a
//! different way to decode and apply an instruction: the same bus operations
//! in the same order, which `tests/ir_tier.rs` proves against the
//! SingleStepTests vectors and nestest.
//!
//! Instructions the lowering marks `Interpret` (`JAM`, the unstable
//! unofficial opcodes) run the interpreter's own arm.

use rf_rebuild::ir::{alu_eval, Dst, Lowering, MachineOp, Op, Val, MAX_TEMPS};

use super::bus::CpuBus;
use super::{exec, Cpu, FLAG_I};
use crate::rebuild::lower::{self, InterruptKind, A, P, PC, S, X, Y};

/// Which executor runs instructions. The IR tier is off by default; the
/// switch exists for tests, benches and the lockstep checker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExecTier {
    /// The hand-written interpreter (`exec::dispatch`).
    #[default]
    Interpreter,
    /// Lowered IR evaluated by this module.
    Ir,
}

/// [`ExecTier`] as a `Cpu` field. A session setting like the coverage
/// recorder: never saved, and ignored by `Cpu` equality, so two CPUs in the
/// same architectural state compare equal whichever tier ran them.
#[derive(Debug, Clone, Copy, Default)]
pub struct TierSelect(pub ExecTier);

impl PartialEq for TierSelect {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for TierSelect {}

/// `exec::dispatch` replacement for the IR tier.
pub(super) fn dispatch_ir(cpu: &mut Cpu, bus: &mut dyn CpuBus, opcode: u8) {
    match &lower::table()[usize::from(opcode)].lowering {
        Lowering::Ir(ops) => run(cpu, bus, ops),
        Lowering::Interpret(_) => exec::dispatch(cpu, bus, opcode),
    }
}

/// Hardware NMI/IRQ entry body as IR (after `Cpu::step`'s cycle 1).
pub(super) fn interrupt_ir(cpu: &mut Cpu, bus: &mut dyn CpuBus, kind: InterruptKind) {
    run(cpu, bus, lower::interrupt_program(kind));
}

#[inline]
fn reg(cpu: &Cpu, r: rf_rebuild::ir::Reg) -> u16 {
    match r {
        A => u16::from(cpu.a),
        X => u16::from(cpu.x),
        Y => u16::from(cpu.y),
        S => u16::from(cpu.s),
        PC => cpu.pc,
        P => u16::from(cpu.p),
        other => unreachable!("6502 has no register {other:?}"),
    }
}

#[inline]
fn set_reg(cpu: &mut Cpu, r: rf_rebuild::ir::Reg, v: u16) {
    match r {
        A => cpu.a = v as u8,
        X => cpu.x = v as u8,
        Y => cpu.y = v as u8,
        S => cpu.s = v as u8,
        PC => cpu.pc = v,
        // PLP/RTI: bit 5 pinned set, bit 4 pinned clear.
        P => cpu.set_p(v as u8),
        other => unreachable!("6502 has no register {other:?}"),
    }
}

#[inline]
fn val(cpu: &Cpu, t: &[u16; MAX_TEMPS], v: Val) -> u16 {
    match v {
        Val::Imm(x) => x,
        Val::Reg(r) => reg(cpu, r),
        Val::Tmp(i) => t[usize::from(i.0)],
        Val::Flag(f) => u16::from(cpu.p & f.0 != 0),
    }
}

#[inline]
fn store(cpu: &mut Cpu, t: &mut [u16; MAX_TEMPS], d: Dst, v: u16) {
    match d {
        Dst::Reg(r) => set_reg(cpu, r, v),
        Dst::Tmp(i) => t[usize::from(i.0)] = v,
    }
}

/// Evaluate `ops` in order. Terminates in at most `ops.len()` steps: the
/// iterator advances before every op is looked at, and a skip only consumes
/// further ops, never rewinds (law 8).
fn run(cpu: &mut Cpu, bus: &mut dyn CpuBus, ops: &[Op]) {
    let mut t = [0u16; MAX_TEMPS];
    let mut it = ops.iter();
    while let Some(op) = it.next() {
        match *op {
            Op::Read { dst, addr, .. } => {
                let a = val(cpu, &t, addr);
                let v = u16::from(bus.read(a));
                if let Some(d) = dst {
                    store(cpu, &mut t, d, v);
                }
            }
            Op::Write { addr, val: v, .. } => {
                let a = val(cpu, &t, addr);
                let x = val(cpu, &t, v);
                bus.write(a, x as u8);
            }
            Op::Set { dst, src } => {
                let v = val(cpu, &t, src);
                store(cpu, &mut t, dst, v);
            }
            Op::Alu { dst, op, w, a, b } => {
                let v = alu_eval(op, w, val(cpu, &t, a), val(cpu, &t, b));
                store(cpu, &mut t, dst, v);
            }
            Op::SetFlag { flag, src } => {
                let on = val(cpu, &t, src) != 0;
                cpu.set_flag(flag.0, on);
            }
            Op::SkipUnless { cond, n } => {
                if val(cpu, &t, cond) == 0 {
                    for _ in 0..n {
                        it.next();
                    }
                }
            }
            Op::Machine(m) => match m {
                MachineOp::BranchPollsEarly => cpu.branch_polls_one_cycle_early = true,
                MachineOp::PollMaskFromFlags => cpu.i_flag_poll_snapshot = cpu.flag(FLAG_I),
                MachineOp::InterruptEntry => cpu.in_interrupt_entry = true,
                MachineOp::SelectVector {
                    dst,
                    normal,
                    nmi,
                    hijackable,
                } => {
                    let hijack = hijackable && (bus.nmi_edge_pending() || cpu.nmi_edge_latched);
                    if hijack {
                        cpu.nmi_hijack_consumed = true;
                    }
                    t[usize::from(dst.0)] = if hijack { nmi } else { normal };
                }
            },
        }
    }
}
