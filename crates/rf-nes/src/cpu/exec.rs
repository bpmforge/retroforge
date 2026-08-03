//! Opcode dispatch: decodes one opcode byte into its addressing-mode bus
//! sequence (`addressing.rs`) plus its register/ALU effect (`ops.rs`).
//!
//! Byte->mnemonic->addressing-mode assignments verified against the
//! opcode-matrix table in nesdev.org/6502_cpu.txt ("6510 Instructions by
//! Addressing Modes"): every official-opcode cell in that 32x8 grid is
//! accounted for below (151 opcodes; cross-checked by
//! `tests::opcode_table::official_opcode_count_is_151`), and every
//! unofficial/illegal cell (`*`, `**`, `t`) hits the `unofficial` arm
//! instead of being implemented — that's ticket W1-01b's job (see module
//! doc in `cpu/mod.rs`).
use super::bus::CpuBus;
use super::{Cpu, FLAG_B, FLAG_C, FLAG_N, FLAG_V};

/// Wraps the caller's bus so `execute` can report a cycle count without
/// threading a counter through every addressing-mode helper by hand — each
/// `read`/`write` that reaches real hardware is exactly one clock cycle
/// (module doc: "cycle-stepped... not 'execute then add N cycles'"), so
/// counting bus operations *is* counting cycles, not an approximation of it.
struct CountingBus<'a> {
    inner: &'a mut dyn CpuBus,
    count: u32,
}

impl CpuBus for CountingBus<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        self.count += 1;
        self.inner.read(addr)
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.count += 1;
        self.inner.write(addr, value);
    }
}

/// Run the instruction `opcode` (already fetched from `PC` by
/// [`Cpu::step`], which is cycle 1) to completion and return the total
/// bus-cycle count including that fetch.
pub(super) fn execute(cpu: &mut Cpu, bus: &mut dyn CpuBus, opcode: u8) -> u32 {
    let mut cb = CountingBus {
        inner: bus,
        count: 1,
    };
    let bus = &mut cb as &mut dyn CpuBus;

    match opcode {
        // ---- LDA ----------------------------------------------------
        0xA9 => {
            let v = cpu.fetch(bus);
            cpu.op_lda(v);
        }
        0xA5 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_lda(v);
        }
        0xB5 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_lda(v);
        }
        0xAD => {
            let v = cpu.am_abs_read(bus);
            cpu.op_lda(v);
        }
        0xBD => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_lda(v);
        }
        0xB9 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_lda(v);
        }
        0xA1 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_lda(v);
        }
        0xB1 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_lda(v);
        }

        // ---- LDX ----------------------------------------------------
        0xA2 => {
            let v = cpu.fetch(bus);
            cpu.op_ldx(v);
        }
        0xA6 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_ldx(v);
        }
        0xB6 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.y);
            let v = bus.read(addr);
            cpu.op_ldx(v);
        }
        0xAE => {
            let v = cpu.am_abs_read(bus);
            cpu.op_ldx(v);
        }
        0xBE => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_ldx(v);
        }

        // ---- LDY ----------------------------------------------------
        0xA0 => {
            let v = cpu.fetch(bus);
            cpu.op_ldy(v);
        }
        0xA4 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_ldy(v);
        }
        0xB4 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_ldy(v);
        }
        0xAC => {
            let v = cpu.am_abs_read(bus);
            cpu.op_ldy(v);
        }
        0xBC => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_ldy(v);
        }

        // ---- STA ----------------------------------------------------
        0x85 => {
            let addr = cpu.am_zp_addr(bus);
            bus.write(addr, cpu.a);
        }
        0x95 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            bus.write(addr, cpu.a);
        }
        0x8D => {
            let addr = cpu.am_abs_addr(bus);
            bus.write(addr, cpu.a);
        }
        0x9D => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            bus.write(addr, cpu.a);
        }
        0x99 => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.y);
            bus.write(addr, cpu.a);
        }
        0x81 => {
            let addr = cpu.am_indx_addr(bus);
            bus.write(addr, cpu.a);
        }
        0x91 => {
            let addr = cpu.am_indy_addr_slow(bus);
            bus.write(addr, cpu.a);
        }

        // ---- STX / STY ------------------------------------------------
        0x86 => {
            let addr = cpu.am_zp_addr(bus);
            bus.write(addr, cpu.x);
        }
        0x96 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.y);
            bus.write(addr, cpu.x);
        }
        0x8E => {
            let addr = cpu.am_abs_addr(bus);
            bus.write(addr, cpu.x);
        }
        0x84 => {
            let addr = cpu.am_zp_addr(bus);
            bus.write(addr, cpu.y);
        }
        0x94 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            bus.write(addr, cpu.y);
        }
        0x8C => {
            let addr = cpu.am_abs_addr(bus);
            bus.write(addr, cpu.y);
        }

        // ---- ADC ----------------------------------------------------
        0x69 => {
            let v = cpu.fetch(bus);
            cpu.op_adc(v);
        }
        0x65 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_adc(v);
        }
        0x75 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_adc(v);
        }
        0x6D => {
            let v = cpu.am_abs_read(bus);
            cpu.op_adc(v);
        }
        0x7D => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_adc(v);
        }
        0x79 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_adc(v);
        }
        0x61 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_adc(v);
        }
        0x71 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_adc(v);
        }

        // ---- SBC ----------------------------------------------------
        0xE9 => {
            let v = cpu.fetch(bus);
            cpu.op_sbc(v);
        }
        0xE5 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_sbc(v);
        }
        0xF5 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_sbc(v);
        }
        0xED => {
            let v = cpu.am_abs_read(bus);
            cpu.op_sbc(v);
        }
        0xFD => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_sbc(v);
        }
        0xF9 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_sbc(v);
        }
        0xE1 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_sbc(v);
        }
        0xF1 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_sbc(v);
        }

        // ---- CMP ----------------------------------------------------
        0xC9 => {
            let v = cpu.fetch(bus);
            cpu.op_cmp(v);
        }
        0xC5 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_cmp(v);
        }
        0xD5 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_cmp(v);
        }
        0xCD => {
            let v = cpu.am_abs_read(bus);
            cpu.op_cmp(v);
        }
        0xDD => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_cmp(v);
        }
        0xD9 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_cmp(v);
        }
        0xC1 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_cmp(v);
        }
        0xD1 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_cmp(v);
        }

        // ---- CPX / CPY ------------------------------------------------
        0xE0 => {
            let v = cpu.fetch(bus);
            cpu.op_cpx(v);
        }
        0xE4 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_cpx(v);
        }
        0xEC => {
            let v = cpu.am_abs_read(bus);
            cpu.op_cpx(v);
        }
        0xC0 => {
            let v = cpu.fetch(bus);
            cpu.op_cpy(v);
        }
        0xC4 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_cpy(v);
        }
        0xCC => {
            let v = cpu.am_abs_read(bus);
            cpu.op_cpy(v);
        }

        // ---- AND ----------------------------------------------------
        0x29 => {
            let v = cpu.fetch(bus);
            cpu.op_and(v);
        }
        0x25 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_and(v);
        }
        0x35 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_and(v);
        }
        0x2D => {
            let v = cpu.am_abs_read(bus);
            cpu.op_and(v);
        }
        0x3D => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_and(v);
        }
        0x39 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_and(v);
        }
        0x21 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_and(v);
        }
        0x31 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_and(v);
        }

        // ---- ORA ----------------------------------------------------
        0x09 => {
            let v = cpu.fetch(bus);
            cpu.op_ora(v);
        }
        0x05 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_ora(v);
        }
        0x15 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_ora(v);
        }
        0x0D => {
            let v = cpu.am_abs_read(bus);
            cpu.op_ora(v);
        }
        0x1D => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_ora(v);
        }
        0x19 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_ora(v);
        }
        0x01 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_ora(v);
        }
        0x11 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_ora(v);
        }

        // ---- EOR ----------------------------------------------------
        0x49 => {
            let v = cpu.fetch(bus);
            cpu.op_eor(v);
        }
        0x45 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_eor(v);
        }
        0x55 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            let v = bus.read(addr);
            cpu.op_eor(v);
        }
        0x4D => {
            let v = cpu.am_abs_read(bus);
            cpu.op_eor(v);
        }
        0x5D => {
            let v = cpu.am_absi_read(bus, cpu.x);
            cpu.op_eor(v);
        }
        0x59 => {
            let v = cpu.am_absi_read(bus, cpu.y);
            cpu.op_eor(v);
        }
        0x41 => {
            let v = cpu.am_indx_read(bus);
            cpu.op_eor(v);
        }
        0x51 => {
            let v = cpu.am_indy_read(bus);
            cpu.op_eor(v);
        }

        // ---- BIT ----------------------------------------------------
        0x24 => {
            let v = cpu.am_zp_read(bus);
            cpu.op_bit(v);
        }
        0x2C => {
            let v = cpu.am_abs_read(bus);
            cpu.op_bit(v);
        }

        // ---- ASL / LSR / ROL / ROR (accumulator + RMW memory) --------
        0x0A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.op_asl(cpu.a);
        }
        0x06 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_asl);
        }
        0x16 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_asl);
        }
        0x0E => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_asl);
        }
        0x1E => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_asl);
        }

        0x4A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.op_lsr(cpu.a);
        }
        0x46 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_lsr);
        }
        0x56 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_lsr);
        }
        0x4E => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_lsr);
        }
        0x5E => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_lsr);
        }

        0x2A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.op_rol(cpu.a);
        }
        0x26 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rol);
        }
        0x36 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_rol);
        }
        0x2E => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_rol);
        }
        0x3E => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_rol);
        }

        0x6A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.op_ror(cpu.a);
        }
        0x66 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_ror);
        }
        0x76 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_ror);
        }
        0x6E => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_ror);
        }
        0x7E => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_ror);
        }

        // ---- INC / DEC (memory) ---------------------------------------
        0xE6 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_inc);
        }
        0xF6 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_inc);
        }
        0xEE => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_inc);
        }
        0xFE => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_inc);
        }
        0xC6 => {
            let addr = cpu.am_zp_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_dec);
        }
        0xD6 => {
            let addr = cpu.am_zp_indexed_addr(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_dec);
        }
        0xCE => {
            let addr = cpu.am_abs_addr(bus);
            cpu.rmw(bus, addr, Cpu::op_dec);
        }
        0xDE => {
            let addr = cpu.am_absi_addr_slow(bus, cpu.x);
            cpu.rmw(bus, addr, Cpu::op_dec);
        }

        // ---- register increment/decrement (implied) --------------------
        0xE8 => {
            cpu.implied_dummy_read(bus);
            cpu.x = cpu.x.wrapping_add(1);
            cpu.set_nz(cpu.x);
        }
        0xC8 => {
            cpu.implied_dummy_read(bus);
            cpu.y = cpu.y.wrapping_add(1);
            cpu.set_nz(cpu.y);
        }
        0xCA => {
            cpu.implied_dummy_read(bus);
            cpu.x = cpu.x.wrapping_sub(1);
            cpu.set_nz(cpu.x);
        }
        0x88 => {
            cpu.implied_dummy_read(bus);
            cpu.y = cpu.y.wrapping_sub(1);
            cpu.set_nz(cpu.y);
        }

        // ---- register transfers (implied) -------------------------------
        0xAA => {
            cpu.implied_dummy_read(bus);
            cpu.x = cpu.a;
            cpu.set_nz(cpu.x);
        }
        0xA8 => {
            cpu.implied_dummy_read(bus);
            cpu.y = cpu.a;
            cpu.set_nz(cpu.y);
        }
        0x8A => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.x;
            cpu.set_nz(cpu.a);
        }
        0x98 => {
            cpu.implied_dummy_read(bus);
            cpu.a = cpu.y;
            cpu.set_nz(cpu.a);
        }
        0xBA => {
            cpu.implied_dummy_read(bus);
            cpu.x = cpu.s;
            cpu.set_nz(cpu.x);
        }
        0x9A => {
            // TXS does not touch any flag (nesdev.org/6502_cpu.txt notes
            // TXS "is not an arithmetic operation").
            cpu.implied_dummy_read(bus);
            cpu.s = cpu.x;
        }

        // ---- flag ops (implied) ------------------------------------
        0x18 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(FLAG_C, false);
        }
        0x38 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(FLAG_C, true);
        }
        0x58 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(super::FLAG_I, false);
        }
        0x78 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(super::FLAG_I, true);
        }
        0xB8 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(FLAG_V, false);
        }
        0xD8 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(super::FLAG_D, false);
        }
        0xF8 => {
            cpu.implied_dummy_read(bus);
            cpu.set_flag(super::FLAG_D, true);
        }

        // ---- NOP (the one official NOP) --------------------------------
        0xEA => {
            cpu.implied_dummy_read(bus);
        }

        // ---- branches -------------------------------------------------
        0x10 => branch(cpu, bus, !cpu.flag(FLAG_N)),
        0x30 => branch(cpu, bus, cpu.flag(FLAG_N)),
        0x50 => branch(cpu, bus, !cpu.flag(FLAG_V)),
        0x70 => branch(cpu, bus, cpu.flag(FLAG_V)),
        0x90 => branch(cpu, bus, !cpu.flag(FLAG_C)),
        0xB0 => branch(cpu, bus, cpu.flag(FLAG_C)),
        0xD0 => branch(cpu, bus, !cpu.flag(super::FLAG_Z)),
        0xF0 => branch(cpu, bus, cpu.flag(super::FLAG_Z)),

        // ---- jumps / subroutine / stack-frame control -------------------
        0x4C => {
            cpu.pc = cpu.am_abs_addr(bus);
        }
        0x6C => jmp_indirect(cpu, bus),
        0x20 => jsr(cpu, bus),
        0x60 => rts(cpu, bus),
        0x40 => rti(cpu, bus),
        0x00 => brk(cpu, bus),

        // ---- stack ------------------------------------------------------
        0x48 => {
            cpu.implied_dummy_read(bus);
            cpu.push(bus, cpu.a);
        }
        0x08 => {
            cpu.implied_dummy_read(bus);
            let pushed = cpu.p | FLAG_B;
            cpu.push(bus, pushed);
        }
        0x68 => {
            let v = pull(cpu, bus);
            cpu.a = v;
            cpu.set_nz(v);
        }
        0x28 => {
            let v = pull(cpu, bus);
            cpu.set_p(v);
        }

        _ => unofficial(opcode),
    }

    cb.count
}

/// Relative-branch bus sequence (nesdev.org/6502_cpu.txt "Relative
/// addressing"): fetch the offset, then — only if `taken` — a dummy read
/// at the not-yet-adjusted PC, and (only if that crosses a page) a second
/// dummy read at the wrong-page address before committing the real target.
fn branch(cpu: &mut Cpu, bus: &mut dyn CpuBus, taken: bool) {
    let offset = cpu.fetch(bus) as i8;
    if taken {
        bus.read(cpu.pc);
        let old_pc = cpu.pc;
        let new_pc = old_pc.wrapping_add(offset as i16 as u16);
        if new_pc & 0xFF00 != old_pc & 0xFF00 {
            let wrong = (old_pc & 0xFF00) | (new_pc & 0x00FF);
            bus.read(wrong);
        }
        cpu.pc = new_pc;
    }
}

/// `JMP (indirect)`: reproduces the famous page-wrap bug — if the pointer's
/// low byte is `$FF`, the high byte is fetched from `pointer & $FF00`
/// rather than `pointer + 1` (nesdev.org/6502_cpu.txt "Indirect addressing
/// modes do not handle page boundary crossing at all").
fn jmp_indirect(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    let ptr = cpu.am_abs_addr(bus);
    let lo = bus.read(ptr);
    let hi_addr = (ptr & 0xFF00) | (ptr.wrapping_add(1) & 0x00FF);
    let hi = bus.read(hi_addr);
    cpu.pc = u16::from_le_bytes([lo, hi]);
}

/// `JSR` (nesdev.org/6502_cpu.txt "JSR"): note the low address byte is
/// fetched *before* the internal stack cycle and the high byte *after*
/// both pushes — `PC` at push time equals the address of the instruction's
/// own last byte (the classic "JSR pushes return-address-minus-one").
fn jsr(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    let lo = cpu.fetch(bus);
    bus.read(cpu.stack_addr()); // cycle 3: "internal operation"
    let return_addr = cpu.pc;
    cpu.push(bus, (return_addr >> 8) as u8);
    cpu.push(bus, return_addr as u8);
    let hi = bus.read(cpu.pc);
    cpu.pc = u16::from_le_bytes([lo, hi]);
}

fn rts(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    cpu.implied_dummy_read(bus);
    bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let lo = bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let hi = bus.read(cpu.stack_addr());
    let addr = u16::from_le_bytes([lo, hi]);
    bus.read(addr);
    cpu.pc = addr.wrapping_add(1);
}

fn rti(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    cpu.implied_dummy_read(bus);
    bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let p = bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let lo = bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    let hi = bus.read(cpu.stack_addr());
    cpu.set_p(p);
    cpu.pc = u16::from_le_bytes([lo, hi]);
}

/// `BRK` (nesdev.org/6502_cpu.txt "BRK"). Software interrupt only — the
/// hardware NMI/IRQ "hijacking" of an in-flight BRK (EMULATION_CORES.md
/// §2.1) is out of scope for this ticket (see `cpu/mod.rs` module doc);
/// this always runs straight through to the `$FFFE`/`$FFFF` vector.
fn brk(cpu: &mut Cpu, bus: &mut dyn CpuBus) {
    bus.read(cpu.pc); // the padding byte after BRK's opcode
    cpu.pc = cpu.pc.wrapping_add(1);
    cpu.push(bus, (cpu.pc >> 8) as u8);
    cpu.push(bus, cpu.pc as u8);
    let pushed = cpu.p | FLAG_B;
    cpu.push(bus, pushed);
    let lo = bus.read(0xFFFE);
    let hi = bus.read(0xFFFF);
    cpu.pc = u16::from_le_bytes([lo, hi]);
    cpu.set_flag(super::FLAG_I, true);
}

/// The `PLA`/`PLP` stack-read sequence: a dummy read at the not-yet-bumped
/// `S`, then the pulled byte at `S+1` (nesdev.org/6502_cpu.txt "PLA, PLP").
fn pull(cpu: &mut Cpu, bus: &mut dyn CpuBus) -> u8 {
    cpu.implied_dummy_read(bus);
    bus.read(cpu.stack_addr());
    cpu.s = cpu.s.wrapping_add(1);
    bus.read(cpu.stack_addr())
}

/// Reached only for the 105 unofficial/illegal opcode bytes, which are
/// ticket W1-01b's responsibility (see `cpu/mod.rs` module doc) — this is
/// a deliberate seam, not a missing-case bug.
fn unofficial(opcode: u8) -> ! {
    panic!("rf-nes cpu: opcode ${opcode:02X} is unofficial/illegal — out of scope for W1-01a, see W1-01b")
}
