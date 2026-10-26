//! 6502 lowering to the semantic IR (ticket W36-12;
//! `rf_rebuild::ir`).
//!
//! [`lower_opcode`] turns one opcode into the ops of cycles 2..n: cycle 1, the
//! opcode fetch, happens in `Cpu::step` before any body runs (it is where the
//! pending-interrupt check lives), so the IR starts after it. The programs
//! mirror `cpu/addressing.rs`, `cpu/ops.rs` and `cpu/exec.rs` cycle for
//! cycle; the SingleStepTests vectors and nestest are the oracle that they
//! do (`cpu/tests/ir_tier.rs`).
//!
//! Left to the interpreter ([`Lowering::Interpret`]): the twelve `JAM`
//! opcodes and the eight unstable unofficial ones (`ANE`, `LXA`, `SHA`,
//! `SHX`, `SHY`, `TAS`, `LAS`), whose results the core documents as
//! best-guess constants. Interrupt entry (`BRK`, hardware `NMI`/`IRQ`) is
//! lowered, with the poll-bookkeeping effects as [`MachineOp`]s.
//!
//! Hardware references: cycle tables in nesdev.org/6502_cpu.txt, interrupt
//! polling in nesdev.org/wiki/CPU_interrupts.

use rf_rebuild::ir::{
    bake, AluOp, Cycle, Dst, Flag, InsnIr, InterpretReason, LoweredBlock, LoweredFunction,
    LoweredInsn, Lowering, MachineOp, Op, Reg, Tmp, Val, Width, MAX_TEMPS,
};
use rf_rebuild::{Atlas, Block, Function};

use crate::cpu::{FLAG_C, FLAG_I, FLAG_N, FLAG_V, FLAG_Z};
use crate::trace::instruction_len;

pub const A: Reg = Reg(0);
pub const X: Reg = Reg(1);
pub const Y: Reg = Reg(2);
pub const S: Reg = Reg(3);
pub const PC: Reg = Reg(4);
pub const P: Reg = Reg(5);

pub const F_C: Flag = Flag(FLAG_C);
pub const F_Z: Flag = Flag(FLAG_Z);
pub const F_I: Flag = Flag(FLAG_I);
pub const F_D: Flag = Flag(crate::cpu::FLAG_D);
pub const F_V: Flag = Flag(FLAG_V);
pub const F_N: Flag = Flag(FLAG_N);

/// NMI, IRQ/BRK vectors (nesdev.org/wiki/CPU_interrupts).
const NMI_VECTOR: u16 = 0xFFFA;
const IRQ_VECTOR: u16 = 0xFFFE;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Imm,
    Zp,
    ZpX,
    ZpY,
    Abs,
    AbsX,
    AbsY,
    IndX,
    IndY,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rd {
    Lda,
    Ldx,
    Ldy,
    Lax,
    Adc,
    Sbc,
    Cmp,
    Cpx,
    Cpy,
    And,
    Ora,
    Eor,
    Bit,
    Anc,
    Alr,
    Arr,
    Sbx,
    Nop,
    /// Left to the interpreter (unstable).
    Unstable(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rm {
    Asl,
    Lsr,
    Rol,
    Ror,
    Inc,
    Dec,
    Slo,
    Rla,
    Sre,
    Rra,
    Dcp,
    Isc,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    A,
    X,
    Y,
    /// `A & X` (SAX).
    Ax,
}

use Mode::{Abs, AbsX, AbsY, Imm, IndX, IndY, Zp, ZpX, ZpY};

/// Read-type instructions: opcode, operation, addressing mode.
#[rustfmt::skip]
const READS: &[(u8, Rd, Mode)] = &[
    (0x01, Rd::Ora, IndX), (0x05, Rd::Ora, Zp), (0x09, Rd::Ora, Imm), (0x0B, Rd::Anc, Imm),
    (0x0D, Rd::Ora, Abs), (0x11, Rd::Ora, IndY), (0x15, Rd::Ora, ZpX), (0x19, Rd::Ora, AbsY),
    (0x1D, Rd::Ora, AbsX), (0x21, Rd::And, IndX), (0x24, Rd::Bit, Zp), (0x25, Rd::And, Zp),
    (0x29, Rd::And, Imm), (0x2B, Rd::Anc, Imm), (0x2C, Rd::Bit, Abs), (0x2D, Rd::And, Abs),
    (0x31, Rd::And, IndY), (0x35, Rd::And, ZpX), (0x39, Rd::And, AbsY), (0x3D, Rd::And, AbsX),
    (0x41, Rd::Eor, IndX), (0x45, Rd::Eor, Zp), (0x49, Rd::Eor, Imm), (0x4B, Rd::Alr, Imm),
    (0x4D, Rd::Eor, Abs), (0x51, Rd::Eor, IndY), (0x55, Rd::Eor, ZpX), (0x59, Rd::Eor, AbsY),
    (0x5D, Rd::Eor, AbsX), (0x61, Rd::Adc, IndX), (0x65, Rd::Adc, Zp), (0x69, Rd::Adc, Imm),
    (0x6B, Rd::Arr, Imm), (0x6D, Rd::Adc, Abs), (0x71, Rd::Adc, IndY), (0x75, Rd::Adc, ZpX),
    (0x79, Rd::Adc, AbsY), (0x7D, Rd::Adc, AbsX), (0x8B, Rd::Unstable("ANE"), Imm),
    (0xA0, Rd::Ldy, Imm), (0xA1, Rd::Lda, IndX), (0xA2, Rd::Ldx, Imm), (0xA3, Rd::Lax, IndX),
    (0xA4, Rd::Ldy, Zp), (0xA5, Rd::Lda, Zp), (0xA6, Rd::Ldx, Zp), (0xA7, Rd::Lax, Zp),
    (0xA9, Rd::Lda, Imm), (0xAB, Rd::Unstable("LXA"), Imm), (0xAC, Rd::Ldy, Abs),
    (0xAD, Rd::Lda, Abs), (0xAE, Rd::Ldx, Abs), (0xAF, Rd::Lax, Abs), (0xB1, Rd::Lda, IndY),
    (0xB3, Rd::Lax, IndY), (0xB4, Rd::Ldy, ZpX), (0xB5, Rd::Lda, ZpX), (0xB6, Rd::Ldx, ZpY),
    (0xB7, Rd::Lax, ZpY), (0xB9, Rd::Lda, AbsY), (0xBB, Rd::Unstable("LAS"), AbsY),
    (0xBC, Rd::Ldy, AbsX), (0xBD, Rd::Lda, AbsX), (0xBE, Rd::Ldx, AbsY), (0xBF, Rd::Lax, AbsY),
    (0xC0, Rd::Cpy, Imm), (0xC1, Rd::Cmp, IndX), (0xC4, Rd::Cpy, Zp), (0xC5, Rd::Cmp, Zp),
    (0xC9, Rd::Cmp, Imm), (0xCB, Rd::Sbx, Imm), (0xCC, Rd::Cpy, Abs), (0xCD, Rd::Cmp, Abs),
    (0xD1, Rd::Cmp, IndY), (0xD5, Rd::Cmp, ZpX), (0xD9, Rd::Cmp, AbsY), (0xDD, Rd::Cmp, AbsX),
    (0xE0, Rd::Cpx, Imm), (0xE1, Rd::Sbc, IndX), (0xE4, Rd::Cpx, Zp), (0xE5, Rd::Sbc, Zp),
    (0xE9, Rd::Sbc, Imm), (0xEB, Rd::Sbc, Imm), (0xEC, Rd::Cpx, Abs), (0xED, Rd::Sbc, Abs),
    (0xF1, Rd::Sbc, IndY), (0xF5, Rd::Sbc, ZpX), (0xF9, Rd::Sbc, AbsY), (0xFD, Rd::Sbc, AbsX),
    // NOP / SKB / IGN (unofficial): the operand is read and thrown away.
    (0x80, Rd::Nop, Imm), (0x82, Rd::Nop, Imm), (0x89, Rd::Nop, Imm), (0xC2, Rd::Nop, Imm),
    (0xE2, Rd::Nop, Imm), (0x04, Rd::Nop, Zp), (0x44, Rd::Nop, Zp), (0x64, Rd::Nop, Zp),
    (0x14, Rd::Nop, ZpX), (0x34, Rd::Nop, ZpX), (0x54, Rd::Nop, ZpX), (0x74, Rd::Nop, ZpX),
    (0xD4, Rd::Nop, ZpX), (0xF4, Rd::Nop, ZpX), (0x0C, Rd::Nop, Abs), (0x1C, Rd::Nop, AbsX),
    (0x3C, Rd::Nop, AbsX), (0x5C, Rd::Nop, AbsX), (0x7C, Rd::Nop, AbsX), (0xDC, Rd::Nop, AbsX),
    (0xFC, Rd::Nop, AbsX),
];

/// Read-modify-write instructions (official shifts/inc/dec and the unofficial
/// RMW combos).
#[rustfmt::skip]
const RMWS: &[(u8, Rm, Mode)] = &[
    (0x03, Rm::Slo, IndX), (0x06, Rm::Asl, Zp), (0x07, Rm::Slo, Zp), (0x0E, Rm::Asl, Abs),
    (0x0F, Rm::Slo, Abs), (0x13, Rm::Slo, IndY), (0x16, Rm::Asl, ZpX), (0x17, Rm::Slo, ZpX),
    (0x1B, Rm::Slo, AbsY), (0x1E, Rm::Asl, AbsX), (0x1F, Rm::Slo, AbsX), (0x23, Rm::Rla, IndX),
    (0x26, Rm::Rol, Zp), (0x27, Rm::Rla, Zp), (0x2E, Rm::Rol, Abs), (0x2F, Rm::Rla, Abs),
    (0x33, Rm::Rla, IndY), (0x36, Rm::Rol, ZpX), (0x37, Rm::Rla, ZpX), (0x3B, Rm::Rla, AbsY),
    (0x3E, Rm::Rol, AbsX), (0x3F, Rm::Rla, AbsX), (0x43, Rm::Sre, IndX), (0x46, Rm::Lsr, Zp),
    (0x47, Rm::Sre, Zp), (0x4E, Rm::Lsr, Abs), (0x4F, Rm::Sre, Abs), (0x53, Rm::Sre, IndY),
    (0x56, Rm::Lsr, ZpX), (0x57, Rm::Sre, ZpX), (0x5B, Rm::Sre, AbsY), (0x5E, Rm::Lsr, AbsX),
    (0x5F, Rm::Sre, AbsX), (0x63, Rm::Rra, IndX), (0x66, Rm::Ror, Zp), (0x67, Rm::Rra, Zp),
    (0x6E, Rm::Ror, Abs), (0x6F, Rm::Rra, Abs), (0x73, Rm::Rra, IndY), (0x76, Rm::Ror, ZpX),
    (0x77, Rm::Rra, ZpX), (0x7B, Rm::Rra, AbsY), (0x7E, Rm::Ror, AbsX), (0x7F, Rm::Rra, AbsX),
    (0xC3, Rm::Dcp, IndX), (0xC6, Rm::Dec, Zp), (0xC7, Rm::Dcp, Zp), (0xCE, Rm::Dec, Abs),
    (0xCF, Rm::Dcp, Abs), (0xD3, Rm::Dcp, IndY), (0xD6, Rm::Dec, ZpX), (0xD7, Rm::Dcp, ZpX),
    (0xDB, Rm::Dcp, AbsY), (0xDE, Rm::Dec, AbsX), (0xDF, Rm::Dcp, AbsX), (0xE3, Rm::Isc, IndX),
    (0xE6, Rm::Inc, Zp), (0xE7, Rm::Isc, Zp), (0xEE, Rm::Inc, Abs), (0xEF, Rm::Isc, Abs),
    (0xF3, Rm::Isc, IndY), (0xF6, Rm::Inc, ZpX), (0xF7, Rm::Isc, ZpX), (0xFB, Rm::Isc, AbsY),
    (0xFE, Rm::Inc, AbsX), (0xFF, Rm::Isc, AbsX),
];

#[rustfmt::skip]
const STORES: &[(u8, St, Mode)] = &[
    (0x81, St::A, IndX), (0x83, St::Ax, IndX), (0x84, St::Y, Zp), (0x85, St::A, Zp),
    (0x86, St::X, Zp), (0x87, St::Ax, Zp), (0x8C, St::Y, Abs), (0x8D, St::A, Abs),
    (0x8E, St::X, Abs), (0x8F, St::Ax, Abs), (0x91, St::A, IndY), (0x94, St::Y, ZpX),
    (0x95, St::A, ZpX), (0x96, St::X, ZpY), (0x97, St::Ax, ZpY), (0x99, St::A, AbsY),
    (0x9D, St::A, AbsX),
];

/// Unstable stores the interpreter keeps (SHA, SHX, SHY, TAS).
const UNSTABLE_STORES: &[(u8, &str)] = &[
    (0x93, "SHA"),
    (0x9F, "SHA"),
    (0x9E, "SHX"),
    (0x9C, "SHY"),
    (0x9B, "TAS"),
];

const JAMS: [u8; 12] = [
    0x02, 0x12, 0x22, 0x32, 0x42, 0x52, 0x62, 0x72, 0x92, 0xB2, 0xD2, 0xF2,
];

/// Program builder: ops plus a temp allocator.
struct B {
    ops: Vec<Op>,
    temps: u8,
}

impl B {
    fn new() -> Self {
        Self {
            ops: Vec::new(),
            temps: 0,
        }
    }

    fn tmp(&mut self) -> Tmp {
        assert!(usize::from(self.temps) < MAX_TEMPS, "lowering out of temps");
        self.temps += 1;
        Tmp(self.temps - 1)
    }

    /// Read cycle into a fresh temp.
    fn read(&mut self, addr: Val, kind: Cycle) -> Val {
        let t = self.tmp();
        self.read_into(t, addr, kind);
        Val::Tmp(t)
    }

    fn read_into(&mut self, t: Tmp, addr: Val, kind: Cycle) {
        self.ops.push(Op::Read {
            dst: Some(Dst::Tmp(t)),
            addr,
            kind,
        });
    }

    /// Read cycle whose value the hardware throws away.
    fn dummy_read(&mut self, addr: Val) {
        self.ops.push(Op::Read {
            dst: None,
            addr,
            kind: Cycle::Dummy,
        });
    }

    fn write(&mut self, addr: Val, val: Val, kind: Cycle) {
        self.ops.push(Op::Write { addr, val, kind });
    }

    /// Fetch the next instruction-stream byte and step `PC` past it.
    fn fetch(&mut self) -> Val {
        let v = self.read(Val::Reg(PC), Cycle::Fetch);
        self.ops.push(Op::Alu {
            dst: Dst::Reg(PC),
            op: AluOp::Add,
            w: Width::W16,
            a: Val::Reg(PC),
            b: Val::Imm(1),
        });
        v
    }

    fn alu(&mut self, op: AluOp, w: Width, a: Val, b: Val) -> Val {
        let t = self.tmp();
        self.ops.push(Op::Alu {
            dst: Dst::Tmp(t),
            op,
            w,
            a,
            b,
        });
        Val::Tmp(t)
    }

    fn alu_to(&mut self, dst: Reg, op: AluOp, w: Width, a: Val, b: Val) {
        self.ops.push(Op::Alu {
            dst: Dst::Reg(dst),
            op,
            w,
            a,
            b,
        });
    }

    fn set(&mut self, dst: Reg, src: Val) {
        self.ops.push(Op::Set {
            dst: Dst::Reg(dst),
            src,
        });
    }

    fn flag(&mut self, flag: Flag, src: Val) {
        self.ops.push(Op::SetFlag { flag, src });
    }

    fn imm(v: u16) -> Val {
        Val::Imm(v)
    }

    fn reg(r: Reg) -> Val {
        Val::Reg(r)
    }

    fn and8(&mut self, a: Val, b: Val) -> Val {
        self.alu(AluOp::And, Width::W8, a, b)
    }

    /// `N` from bit 7 and `Z` from zero of the 8-bit value `v`.
    fn nz(&mut self, v: Val) {
        let n = self.and8(v, Self::imm(0x80));
        self.flag(F_N, n);
        let z = self.alu(AluOp::Eq, Width::W8, v, Self::imm(0));
        self.flag(F_Z, z);
    }

    /// Run `body` only when `cond` is non-zero.
    fn when(&mut self, cond: Val, body: impl FnOnce(&mut Self)) {
        let at = self.ops.len();
        self.ops.push(Op::SkipUnless { cond, n: 0 });
        body(self);
        let n = self.ops.len() - at - 1;
        self.ops[at] = Op::SkipUnless {
            cond,
            n: u8::try_from(n).expect("conditional group fits a byte"),
        };
    }

    fn stack_addr(&mut self) -> Val {
        self.alu(AluOp::Or, Width::W16, Self::imm(0x100), Self::reg(S))
    }

    fn push(&mut self, val: Val) {
        let a = self.stack_addr();
        self.write(a, val, Cycle::Stack);
        self.alu_to(S, AluOp::Sub, Width::W8, Self::reg(S), Self::imm(1));
    }

    fn inc_s(&mut self) {
        self.alu_to(S, AluOp::Add, Width::W8, Self::reg(S), Self::imm(1));
    }

    /// `ADC`: two 16-bit adds so the carry out is bit 8 (the 2A03 has no
    /// decimal mode, nesdev.org/wiki/CPU).
    fn adc(&mut self, v: Val) {
        let s1 = self.alu(AluOp::Add, Width::W16, Self::reg(A), v);
        let s2 = self.alu(AluOp::Add, Width::W16, s1, Val::Flag(F_C));
        let res = self.alu(AluOp::And, Width::W16, s2, Self::imm(0xFF));
        let carry = self.alu(AluOp::Gt, Width::W16, s2, Self::imm(0xFF));
        self.flag(F_C, carry);
        // V: operands had the same sign and the result's differs.
        let same = self.alu(AluOp::Xor, Width::W8, Self::reg(A), v);
        let same = self.alu(AluOp::Not, Width::W8, same, Self::imm(0));
        let diff = self.alu(AluOp::Xor, Width::W8, Self::reg(A), res);
        let ov = self.and8(same, diff);
        let ov = self.and8(ov, Self::imm(0x80));
        self.flag(F_V, ov);
        self.set(A, res);
        self.nz(res);
    }

    fn sbc(&mut self, v: Val) {
        let nv = self.alu(AluOp::Not, Width::W8, v, Self::imm(0));
        self.adc(nv);
    }

    fn compare(&mut self, reg: Reg, v: Val) {
        let diff = self.alu(AluOp::Sub, Width::W8, Self::reg(reg), v);
        let ge = self.alu(AluOp::Ge, Width::W8, Self::reg(reg), v);
        self.flag(F_C, ge);
        self.nz(diff);
    }

    fn asl(&mut self, v: Val) -> Val {
        let c = self.and8(v, Self::imm(0x80));
        self.flag(F_C, c);
        let r = self.alu(AluOp::Shl, Width::W8, v, Self::imm(1));
        self.nz(r);
        r
    }

    fn lsr(&mut self, v: Val) -> Val {
        let c = self.and8(v, Self::imm(1));
        self.flag(F_C, c);
        let r = self.alu(AluOp::Shr, Width::W8, v, Self::imm(1));
        self.nz(r);
        r
    }

    fn rol(&mut self, v: Val) -> Val {
        // The old carry is read before the new one is stored.
        let cin = self.alu(AluOp::Or, Width::W8, Val::Flag(F_C), Self::imm(0));
        let c = self.and8(v, Self::imm(0x80));
        self.flag(F_C, c);
        let sh = self.alu(AluOp::Shl, Width::W8, v, Self::imm(1));
        let r = self.alu(AluOp::Or, Width::W8, sh, cin);
        self.nz(r);
        r
    }

    fn ror(&mut self, v: Val) -> Val {
        let cin = self.alu(AluOp::Or, Width::W8, Val::Flag(F_C), Self::imm(0));
        let c = self.and8(v, Self::imm(1));
        self.flag(F_C, c);
        let sh = self.alu(AluOp::Shr, Width::W8, v, Self::imm(1));
        let top = self.alu(AluOp::Shl, Width::W8, cin, Self::imm(7));
        let r = self.alu(AluOp::Or, Width::W8, sh, top);
        self.nz(r);
        r
    }

    fn a_op(&mut self, op: AluOp, v: Val) {
        let r = self.alu(op, Width::W8, Self::reg(A), v);
        self.set(A, r);
        self.nz(r);
    }

    // ---- addressing modes (cpu/addressing.rs) ------------------------

    fn abs_addr(&mut self) -> Val {
        let lo = self.fetch();
        let hi = self.fetch();
        self.alu(AluOp::Join, Width::W16, lo, hi)
    }

    /// Indexed-absolute and (zp),Y share the uncorrected-then-corrected
    /// read: `base` is the pre-index 16-bit address, `lo`/`hi` its bytes.
    /// Returns the operand value.
    fn indexed_read(&mut self, lo: Val, hi: Val, idx: Reg) -> Val {
        let base = self.alu(AluOp::Join, Width::W16, lo, hi);
        let plo = self.alu(AluOp::Add, Width::W16, lo, Self::reg(idx));
        let crossed = self.alu(AluOp::Gt, Width::W16, plo, Self::imm(0xFF));
        let partial = self.alu(AluOp::Join, Width::W16, plo, hi);
        let v = self.tmp();
        self.read_into(v, partial, Cycle::Probe);
        self.when(crossed, |b| {
            let eff = b.alu(AluOp::Add, Width::W16, base, Self::reg(idx));
            b.read_into(v, eff, Cycle::Data);
        });
        Val::Tmp(v)
    }

    /// Indexed-absolute and (zp),Y target for stores and RMW: the dummy read
    /// at the uncorrected address is unconditional (6502_cpu.txt).
    fn indexed_target(&mut self, lo: Val, hi: Val, idx: Reg) -> Val {
        let base = self.alu(AluOp::Join, Width::W16, lo, hi);
        let plo = self.alu(AluOp::Add, Width::W16, lo, Self::reg(idx));
        let partial = self.alu(AluOp::Join, Width::W16, plo, hi);
        self.dummy_read(partial);
        self.alu(AluOp::Add, Width::W16, base, Self::reg(idx))
    }

    fn indx_addr(&mut self) -> Val {
        let ptr = self.fetch();
        self.dummy_read(ptr);
        let px = self.alu(AluOp::Add, Width::W8, ptr, Self::reg(X));
        let lo = self.read(px, Cycle::Pointer);
        let px1 = self.alu(AluOp::Add, Width::W8, px, Self::imm(1));
        let hi = self.read(px1, Cycle::Pointer);
        self.alu(AluOp::Join, Width::W16, lo, hi)
    }

    fn zp_indexed_addr(&mut self, idx: Reg) -> Val {
        let base = self.fetch();
        self.dummy_read(base);
        self.alu(AluOp::Add, Width::W8, base, Self::reg(idx))
    }

    fn indy_pointer(&mut self) -> (Val, Val) {
        let ptr = self.fetch();
        let lo = self.read(ptr, Cycle::Pointer);
        let p1 = self.alu(AluOp::Add, Width::W8, ptr, Self::imm(1));
        let hi = self.read(p1, Cycle::Pointer);
        (lo, hi)
    }

    /// The operand of a read-type instruction, every cycle included.
    fn operand(&mut self, mode: Mode) -> Val {
        match mode {
            Imm => self.fetch(),
            Zp => {
                let a = self.fetch();
                self.read(a, Cycle::Data)
            }
            ZpX | ZpY => {
                let a = self.zp_indexed_addr(if mode == ZpX { X } else { Y });
                self.read(a, Cycle::Data)
            }
            Abs => {
                let a = self.abs_addr();
                self.read(a, Cycle::Data)
            }
            AbsX | AbsY => {
                let lo = self.fetch();
                let hi = self.fetch();
                self.indexed_read(lo, hi, if mode == AbsX { X } else { Y })
            }
            IndX => {
                let a = self.indx_addr();
                self.read(a, Cycle::Data)
            }
            IndY => {
                let (lo, hi) = self.indy_pointer();
                self.indexed_read(lo, hi, Y)
            }
        }
    }

    /// The effective address of a store or RMW.
    fn target(&mut self, mode: Mode) -> Val {
        match mode {
            Imm => unreachable!("no immediate stores"),
            Zp => self.fetch(),
            ZpX | ZpY => self.zp_indexed_addr(if mode == ZpX { X } else { Y }),
            Abs => self.abs_addr(),
            AbsX | AbsY => {
                let lo = self.fetch();
                let hi = self.fetch();
                self.indexed_target(lo, hi, if mode == AbsX { X } else { Y })
            }
            IndX => self.indx_addr(),
            IndY => {
                let (lo, hi) = self.indy_pointer();
                self.indexed_target(lo, hi, Y)
            }
        }
    }

    fn apply_read(&mut self, rd: Rd, v: Val) {
        match rd {
            Rd::Lda => {
                self.set(A, v);
                self.nz(v);
            }
            Rd::Ldx => {
                self.set(X, v);
                self.nz(v);
            }
            Rd::Ldy => {
                self.set(Y, v);
                self.nz(v);
            }
            Rd::Lax => {
                self.set(A, v);
                self.set(X, v);
                self.nz(v);
            }
            Rd::Adc => self.adc(v),
            Rd::Sbc => self.sbc(v),
            Rd::Cmp => self.compare(A, v),
            Rd::Cpx => self.compare(X, v),
            Rd::Cpy => self.compare(Y, v),
            Rd::And => self.a_op(AluOp::And, v),
            Rd::Ora => self.a_op(AluOp::Or, v),
            Rd::Eor => self.a_op(AluOp::Xor, v),
            Rd::Bit => {
                let m = self.and8(Self::reg(A), v);
                let z = self.alu(AluOp::Eq, Width::W8, m, Self::imm(0));
                self.flag(F_Z, z);
                let ov = self.and8(v, Self::imm(0x40));
                self.flag(F_V, ov);
                let n = self.and8(v, Self::imm(0x80));
                self.flag(F_N, n);
            }
            Rd::Anc => {
                self.a_op(AluOp::And, v);
                let c = self.and8(Self::reg(A), Self::imm(0x80));
                self.flag(F_C, c);
            }
            Rd::Alr => {
                let an = self.and8(Self::reg(A), v);
                let r = self.lsr(an);
                self.set(A, r);
            }
            Rd::Arr => {
                let cin = self.alu(AluOp::Or, Width::W8, Val::Flag(F_C), Self::imm(0));
                let an = self.and8(Self::reg(A), v);
                let sh = self.alu(AluOp::Shr, Width::W8, an, Self::imm(1));
                let top = self.alu(AluOp::Shl, Width::W8, cin, Self::imm(7));
                let r = self.alu(AluOp::Or, Width::W8, sh, top);
                self.set(A, r);
                let n = self.and8(r, Self::imm(0x80));
                self.flag(F_N, n);
                let z = self.alu(AluOp::Eq, Width::W8, r, Self::imm(0));
                self.flag(F_Z, z);
                let c = self.and8(r, Self::imm(0x40));
                self.flag(F_C, c);
                let b6 = self.alu(AluOp::Shr, Width::W8, r, Self::imm(6));
                let b5 = self.alu(AluOp::Shr, Width::W8, r, Self::imm(5));
                let x = self.alu(AluOp::Xor, Width::W8, b6, b5);
                let ov = self.and8(x, Self::imm(1));
                self.flag(F_V, ov);
            }
            Rd::Sbx => {
                let an = self.and8(Self::reg(A), Self::reg(X));
                let ge = self.alu(AluOp::Ge, Width::W8, an, v);
                self.flag(F_C, ge);
                let r = self.alu(AluOp::Sub, Width::W8, an, v);
                self.set(X, r);
                self.nz(r);
            }
            Rd::Nop | Rd::Unstable(_) => {}
        }
    }

    /// The RMW's ALU half: returns the value written back.
    fn apply_rmw(&mut self, rm: Rm, v: Val) -> Val {
        match rm {
            Rm::Asl => self.asl(v),
            Rm::Lsr => self.lsr(v),
            Rm::Rol => self.rol(v),
            Rm::Ror => self.ror(v),
            Rm::Inc => {
                let r = self.alu(AluOp::Add, Width::W8, v, Self::imm(1));
                self.nz(r);
                r
            }
            Rm::Dec => {
                let r = self.alu(AluOp::Sub, Width::W8, v, Self::imm(1));
                self.nz(r);
                r
            }
            Rm::Slo => {
                let r = self.asl(v);
                self.a_op(AluOp::Or, r);
                r
            }
            Rm::Rla => {
                let r = self.rol(v);
                self.a_op(AluOp::And, r);
                r
            }
            Rm::Sre => {
                let r = self.lsr(v);
                self.a_op(AluOp::Xor, r);
                r
            }
            Rm::Rra => {
                let r = self.ror(v);
                self.adc(r);
                r
            }
            Rm::Dcp => {
                let r = self.alu(AluOp::Sub, Width::W8, v, Self::imm(1));
                self.compare(A, r);
                r
            }
            Rm::Isc => {
                let r = self.alu(AluOp::Add, Width::W8, v, Self::imm(1));
                self.sbc(r);
                r
            }
        }
    }

    /// Interrupt entry from the push of `PC` on: `BRK`, hardware `IRQ` and
    /// `NMI` (cpu/exec.rs `finish_interrupt_entry`).
    fn interrupt_entry(&mut self, push_b: bool, vector: u16, hijackable: bool) {
        self.ops.push(Op::Machine(MachineOp::InterruptEntry));
        let hi = self.alu(AluOp::Shr, Width::W16, Self::reg(PC), Self::imm(8));
        self.push(hi);
        let lo = self.alu(AluOp::And, Width::W16, Self::reg(PC), Self::imm(0xFF));
        self.push(lo);
        let vec = self.tmp();
        if hijackable {
            self.ops.push(Op::Machine(MachineOp::SelectVector {
                dst: vec,
                normal: vector,
                nmi: NMI_VECTOR,
                hijackable,
            }));
        } else {
            self.ops.push(Op::Set {
                dst: Dst::Tmp(vec),
                src: Self::imm(vector),
            });
        }
        let pushed = if push_b {
            self.alu(AluOp::Or, Width::W8, Self::reg(P), Self::imm(0x10))
        } else {
            Self::reg(P)
        };
        self.push(pushed);
        let vlo = self.read(Val::Tmp(vec), Cycle::Vector);
        self.flag(F_I, Self::imm(1));
        self.ops.push(Op::Machine(MachineOp::PollMaskFromFlags));
        let v1 = self.alu(AluOp::Add, Width::W16, Val::Tmp(vec), Self::imm(1));
        let vhi = self.read(v1, Cycle::Vector);
        let pc = self.alu(AluOp::Join, Width::W16, vlo, vhi);
        self.set(PC, pc);
    }

    /// Implied-mode dummy read (`PC` is not advanced).
    fn implied(&mut self) {
        self.dummy_read(Self::reg(PC));
    }
}

/// Which interrupt entry [`lower_interrupt`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptKind {
    Nmi,
    Irq,
}

/// The body of a hardware interrupt entry, after the discarded cycle-1 read
/// `Cpu::step` performs (cycle 2 onwards).
#[must_use]
pub fn lower_interrupt(kind: InterruptKind) -> Vec<Op> {
    let mut b = B::new();
    b.implied(); // cycle 2: discarded, PC increment suppressed
    match kind {
        InterruptKind::Nmi => b.interrupt_entry(false, NMI_VECTOR, false),
        InterruptKind::Irq => b.interrupt_entry(false, IRQ_VECTOR, true),
    }
    b.ops
}

fn branch(b: &mut B, flag: Flag, want_set: bool) {
    let off = b.fetch();
    let taken = if want_set {
        Val::Flag(flag)
    } else {
        b.alu(AluOp::Xor, Width::W8, Val::Flag(flag), B::imm(1))
    };
    b.when(taken, |b| {
        b.dummy_read(B::reg(PC)); // cycle 3
        let disp = b.alu(AluOp::Sext8, Width::W16, off, B::imm(0));
        let new_pc = b.alu(AluOp::Add, Width::W16, B::reg(PC), disp);
        let old_page = b.alu(AluOp::And, Width::W16, B::reg(PC), B::imm(0xFF00));
        let new_page = b.alu(AluOp::And, Width::W16, new_pc, B::imm(0xFF00));
        let crossed = b.alu(AluOp::Ne, Width::W16, old_page, new_page);
        b.when(crossed, |b| {
            // cycle 4: the read at the not-yet-corrected address
            let low = b.alu(AluOp::And, Width::W16, new_pc, B::imm(0x00FF));
            let wrong = b.alu(AluOp::Or, Width::W16, old_page, low);
            b.dummy_read(wrong);
        });
        let stays = b.alu(AluOp::Xor, Width::W16, crossed, B::imm(1));
        b.when(stays, |b| {
            b.ops.push(Op::Machine(MachineOp::BranchPollsEarly));
        });
        b.set(PC, new_pc);
    });
}

fn lower_body(opcode: u8) -> Lowering {
    if JAMS.contains(&opcode) {
        return Lowering::Interpret(InterpretReason::Halt);
    }
    if let Some(&(_, name)) = UNSTABLE_STORES.iter().find(|e| e.0 == opcode) {
        return Lowering::Interpret(InterpretReason::Unstable(name));
    }
    let mut b = B::new();
    if let Some(&(_, rd, mode)) = READS.iter().find(|e| e.0 == opcode) {
        if let Rd::Unstable(name) = rd {
            return Lowering::Interpret(InterpretReason::Unstable(name));
        }
        let v = b.operand(mode);
        b.apply_read(rd, v);
        return Lowering::Ir(b.ops);
    }
    if let Some(&(_, rm, mode)) = RMWS.iter().find(|e| e.0 == opcode) {
        let addr = b.target(mode);
        let old = b.read(addr, Cycle::Data);
        b.write(addr, old, Cycle::Dummy); // write-back of the unmodified value
        let new = b.apply_rmw(rm, old);
        b.write(addr, new, Cycle::Data);
        return Lowering::Ir(b.ops);
    }
    if let Some(&(_, st, mode)) = STORES.iter().find(|e| e.0 == opcode) {
        let addr = b.target(mode);
        let val = match st {
            St::A => B::reg(A),
            St::X => B::reg(X),
            St::Y => B::reg(Y),
            St::Ax => b.and8(B::reg(A), B::reg(X)),
        };
        b.write(addr, val, Cycle::Data);
        return Lowering::Ir(b.ops);
    }
    match opcode {
        // accumulator shifts
        0x0A | 0x4A | 0x2A | 0x6A => {
            b.implied();
            let a = B::reg(A);
            let r = match opcode {
                0x0A => b.asl(a),
                0x4A => b.lsr(a),
                0x2A => b.rol(a),
                _ => b.ror(a),
            };
            b.set(A, r);
        }
        // INX INY DEX DEY
        0xE8 | 0xC8 | 0xCA | 0x88 => {
            b.implied();
            let (reg, op) = match opcode {
                0xE8 => (X, AluOp::Add),
                0xC8 => (Y, AluOp::Add),
                0xCA => (X, AluOp::Sub),
                _ => (Y, AluOp::Sub),
            };
            let r = b.alu(op, Width::W8, B::reg(reg), B::imm(1));
            b.set(reg, r);
            b.nz(r);
        }
        // TAX TAY TXA TYA TSX
        0xAA | 0xA8 | 0x8A | 0x98 | 0xBA => {
            b.implied();
            let (dst, src) = match opcode {
                0xAA => (X, A),
                0xA8 => (Y, A),
                0x8A => (A, X),
                0x98 => (A, Y),
                _ => (X, S),
            };
            b.set(dst, B::reg(src));
            b.nz(B::reg(dst));
        }
        0x9A => {
            b.implied();
            b.set(S, B::reg(X)); // TXS sets no flags
        }
        0x18 | 0x38 | 0x58 | 0x78 | 0xB8 | 0xD8 | 0xF8 => {
            b.implied();
            let (f, on) = match opcode {
                0x18 => (F_C, 0),
                0x38 => (F_C, 1),
                0x58 => (F_I, 0),
                0x78 => (F_I, 1),
                0xB8 => (F_V, 0),
                0xD8 => (F_D, 0),
                _ => (F_D, 1),
            };
            b.flag(f, B::imm(on));
        }
        0xEA | 0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA => b.implied(),
        0x10 => branch(&mut b, F_N, false),
        0x30 => branch(&mut b, F_N, true),
        0x50 => branch(&mut b, F_V, false),
        0x70 => branch(&mut b, F_V, true),
        0x90 => branch(&mut b, F_C, false),
        0xB0 => branch(&mut b, F_C, true),
        0xD0 => branch(&mut b, F_Z, false),
        0xF0 => branch(&mut b, F_Z, true),
        0x4C => {
            let a = b.abs_addr();
            b.set(PC, a);
        }
        0x6C => {
            let ptr = b.abs_addr();
            let lo = b.read(ptr, Cycle::Data);
            // The high pointer byte comes from the same page (6502 bug).
            let page = b.alu(AluOp::And, Width::W16, ptr, B::imm(0xFF00));
            let next = b.alu(AluOp::Add, Width::W16, ptr, B::imm(1));
            let next = b.alu(AluOp::And, Width::W16, next, B::imm(0x00FF));
            let hi_addr = b.alu(AluOp::Or, Width::W16, page, next);
            let hi = b.read(hi_addr, Cycle::Data);
            let pc = b.alu(AluOp::Join, Width::W16, lo, hi);
            b.set(PC, pc);
        }
        0x20 => {
            let lo = b.fetch();
            let sa = b.stack_addr();
            b.dummy_read(sa); // cycle 3: internal operation
            let ret_hi = b.alu(AluOp::Shr, Width::W16, B::reg(PC), B::imm(8));
            b.push(ret_hi);
            let ret_lo = b.alu(AluOp::And, Width::W16, B::reg(PC), B::imm(0xFF));
            b.push(ret_lo);
            let hi = b.read(B::reg(PC), Cycle::Fetch); // PC is not advanced
            let pc = b.alu(AluOp::Join, Width::W16, lo, hi);
            b.set(PC, pc);
        }
        0x60 => {
            b.implied();
            let sa = b.stack_addr();
            b.dummy_read(sa);
            b.inc_s();
            let sa = b.stack_addr();
            let lo = b.read(sa, Cycle::Stack);
            b.inc_s();
            let sa = b.stack_addr();
            let hi = b.read(sa, Cycle::Stack);
            let addr = b.alu(AluOp::Join, Width::W16, lo, hi);
            b.dummy_read(addr);
            b.alu_to(PC, AluOp::Add, Width::W16, addr, B::imm(1));
        }
        0x40 => {
            b.implied();
            let sa = b.stack_addr();
            b.dummy_read(sa);
            b.inc_s();
            let sa = b.stack_addr();
            let p = b.read(sa, Cycle::Stack);
            b.inc_s();
            let sa = b.stack_addr();
            let lo = b.read(sa, Cycle::Stack);
            b.inc_s();
            let sa = b.stack_addr();
            let hi = b.read(sa, Cycle::Stack);
            b.set(P, p);
            let pc = b.alu(AluOp::Join, Width::W16, lo, hi);
            b.set(PC, pc);
            // RTI polls with the restored I flag.
            b.ops.push(Op::Machine(MachineOp::PollMaskFromFlags));
        }
        0x00 => {
            let _padding = b.read(B::reg(PC), Cycle::Fetch);
            b.alu_to(PC, AluOp::Add, Width::W16, B::reg(PC), B::imm(1));
            b.interrupt_entry(true, IRQ_VECTOR, true);
        }
        0x48 => {
            b.implied();
            b.push(B::reg(A));
        }
        0x08 => {
            b.implied();
            let p = b.alu(AluOp::Or, Width::W8, B::reg(P), B::imm(0x10));
            b.push(p);
        }
        0x68 | 0x28 => {
            b.implied();
            let sa = b.stack_addr();
            b.dummy_read(sa);
            b.inc_s();
            let sa = b.stack_addr();
            let v = b.read(sa, Cycle::Stack);
            if opcode == 0x68 {
                b.set(A, v);
                b.nz(v);
            } else {
                b.set(P, v);
            }
        }
        _ => unreachable!("opcode {opcode:#04x} has no lowering and is not interpreted"),
    }
    Lowering::Ir(b.ops)
}

/// Lower one opcode to its generic program (operand bytes come off the bus).
#[must_use]
pub fn lower_opcode(opcode: u8) -> InsnIr {
    InsnIr {
        opcode,
        len: instruction_len(opcode),
        lowering: lower_body(opcode),
    }
}

/// Lower one instruction found in an image: the generic program specialised
/// to `bytes` (opcode first) by [`bake`], so addresses and immediates are
/// constants. `bytes` shorter than the instruction leaves the missing
/// operands generic.
#[must_use]
pub fn lower_insn(bytes: &[u8]) -> InsnIr {
    let generic = lower_opcode(bytes[0]);
    let lowering = match generic.lowering {
        Lowering::Ir(ops) => Lowering::Ir(bake(&ops, &bytes[1..], PC)),
        other => other,
    };
    InsnIr {
        lowering,
        ..generic
    }
}

/// Lower one atlas block against the PRG image.
#[must_use]
pub fn lower_block(block: &Block, prg: &[u8]) -> LoweredBlock {
    let insns = block
        .insns
        .iter()
        .filter_map(|rec| {
            let at = usize::try_from(rec.rom).ok()?;
            let bytes = prg.get(at..at + usize::from(rec.len))?;
            Some(LoweredInsn {
                rom: rec.rom,
                cpu: rec.cpu,
                ir: lower_insn(bytes),
                bytes: bytes.to_vec(),
            })
        })
        .collect();
    LoweredBlock {
        start_cpu: block.start_cpu,
        insns,
    }
}

/// Lower every block of an atlas function.
#[must_use]
pub fn lower_function(atlas: &Atlas, f: &Function, prg: &[u8]) -> LoweredFunction {
    LoweredFunction {
        entry_cpu: f.entry_cpu,
        blocks: f
            .blocks
            .iter()
            .filter_map(|&i| atlas.blocks.get(i as usize))
            .map(|blk| lower_block(blk, prg))
            .collect(),
    }
}

/// All 256 generic programs, built once.
#[must_use]
pub fn table() -> &'static [InsnIr; 256] {
    static T: std::sync::OnceLock<Box<[InsnIr; 256]>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let v: Vec<InsnIr> = (0..=255u8).map(lower_opcode).collect();
        v.try_into().expect("256 entries")
    })
}

/// The hardware NMI and IRQ entry programs, built once.
#[must_use]
pub fn interrupt_program(kind: InterruptKind) -> &'static [Op] {
    static T: std::sync::OnceLock<[Vec<Op>; 2]> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        [
            lower_interrupt(InterruptKind::Nmi),
            lower_interrupt(InterruptKind::Irq),
        ]
    });
    match kind {
        InterruptKind::Nmi => &t[0],
        InterruptKind::Irq => &t[1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_rebuild::ir::validate;

    #[test]
    fn every_opcode_lowers_or_is_marked_interpret() {
        let mut lowered = 0;
        let mut interpreted = Vec::new();
        for op in 0..=255u8 {
            let ir = lower_opcode(op);
            assert_eq!(ir.len, instruction_len(op));
            match &ir.lowering {
                Lowering::Ir(ops) => {
                    validate(ops).unwrap_or_else(|e| panic!("opcode {op:#04x}: {e:?}"));
                    lowered += 1;
                }
                Lowering::Interpret(r) => interpreted.push((op, *r)),
            }
        }
        // 12 JAM + ANE LXA LAS (reads) + SHA SHA SHX SHY TAS (stores).
        assert_eq!(interpreted.len(), 20, "{interpreted:?}");
        assert_eq!(lowered, 236);
        for p in [InterruptKind::Nmi, InterruptKind::Irq] {
            validate(interrupt_program(p)).expect("interrupt program is well formed");
        }
    }

    #[test]
    fn the_test_tier_switch_reaches_every_default_cpu() {
        let want = if std::env::var("RF_NES_TEST_TIER").is_ok_and(|v| v == "ir") {
            crate::cpu::ExecTier::Ir
        } else {
            crate::cpu::ExecTier::Interpreter
        };
        assert_eq!(crate::cpu::Cpu::default().exec_tier(), want);
    }

    #[test]
    fn cycle_counts_match_the_hardware_tables() {
        // (opcode, total cycles incl. the opcode fetch: min, max)
        let cases = [
            (0xA9, 2, 2),
            (0xBD, 4, 5),
            (0x9D, 5, 5),
            (0x1E, 7, 7),
            (0xD0, 2, 4),
            (0x20, 6, 6),
            (0x60, 6, 6),
            (0x40, 6, 6),
            (0x00, 7, 7),
            (0xB1, 5, 6),
            (0x6C, 5, 5),
        ];
        for (op, lo, hi) in cases {
            let (min, max) = lower_opcode(op).cycle_range().expect("lowered");
            assert_eq!((min + 1, max + 1), (lo, hi), "opcode {op:#04x}");
        }
    }

    #[test]
    fn baked_instruction_carries_its_operand_as_a_constant() {
        // LDA $2002
        let ir = lower_insn(&[0xAD, 0x02, 0x20]);
        let ops = ir.ops().expect("lowered");
        assert!(
            ops.iter().any(|o| matches!(
                o,
                Op::Read {
                    addr: Val::Imm(0x2002),
                    kind: Cycle::Data,
                    ..
                }
            )),
            "{ops:?}"
        );
        validate(ops).expect("still well formed");
    }
}
