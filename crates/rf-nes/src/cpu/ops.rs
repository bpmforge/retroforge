//! Pure register/ALU semantics, decoupled from bus timing.
//!
//! Every function here takes the operand byte already fetched by
//! `addressing.rs` and updates registers/flags only — no bus access. This
//! keeps the nesdev-verified *timing* (addressing.rs, exec.rs) separate
//! from the nesdev-verified *arithmetic* (this file), so a mistake in one
//! is easy to localize against the SingleStepTests vectors.
use super::{Cpu, FLAG_C, FLAG_N, FLAG_V, FLAG_Z};

impl Cpu {
    // ---- loads -----------------------------------------------------
    pub(super) fn op_lda(&mut self, value: u8) {
        self.a = value;
        self.set_nz(value);
    }

    pub(super) fn op_ldx(&mut self, value: u8) {
        self.x = value;
        self.set_nz(value);
    }

    pub(super) fn op_ldy(&mut self, value: u8) {
        self.y = value;
        self.set_nz(value);
    }

    // ---- logical -----------------------------------------------------
    pub(super) fn op_and(&mut self, value: u8) {
        self.a &= value;
        self.set_nz(self.a);
    }

    pub(super) fn op_ora(&mut self, value: u8) {
        self.a |= value;
        self.set_nz(self.a);
    }

    pub(super) fn op_eor(&mut self, value: u8) {
        self.a ^= value;
        self.set_nz(self.a);
    }

    /// `BIT`: `Z` from `A & value`, `N`/`V` copied straight from bits 7/6
    /// of `value` (nesdev.org/6502_cpu.txt "the BIT instruction affects
    /// the Negative flag just like arithmetic operations" plus bit 6 -> V,
    /// per nesdev.org/wiki/Instruction_reference#BIT).
    pub(super) fn op_bit(&mut self, value: u8) {
        self.set_flag(FLAG_Z, (self.a & value) == 0);
        self.set_flag(FLAG_V, value & 0x40 != 0);
        self.set_flag(FLAG_N, value & 0x80 != 0);
    }

    // ---- arithmetic ----------------------------------------------------
    /// `ADC`. The 2A03 never applies BCD fixups regardless of the `D` flag
    /// (module doc), so this is always the binary-mode formula
    /// (nesdev.org/6502_cpu.txt "Decimal mode in NMOS 6500 series", binary
    /// case): carry out from bit 7, signed overflow from
    /// `!(a^value) & (a^result) & 0x80`.
    pub(super) fn op_adc(&mut self, value: u8) {
        let a = self.a;
        let carry_in: u16 = self.flag(FLAG_C) as u16;
        let sum = a as u16 + value as u16 + carry_in;
        let result = sum as u8;
        self.set_flag(FLAG_C, sum > 0xFF);
        self.set_flag(FLAG_V, (!(a ^ value) & (a ^ result) & 0x80) != 0);
        self.a = result;
        self.set_nz(result);
    }

    /// `SBC`. Binary-mode subtraction is `ADC` with the operand inverted
    /// (`A - M - (1-C) == A + !M + C`); on the 2A03 that identity holds
    /// unconditionally since decimal mode never applies (module doc), so
    /// `SBC` is implemented directly in terms of [`Self::op_adc`].
    pub(super) fn op_sbc(&mut self, value: u8) {
        self.op_adc(!value);
    }

    fn compare(&mut self, reg: u8, value: u8) {
        let result = reg.wrapping_sub(value);
        self.set_flag(FLAG_C, reg >= value);
        self.set_nz(result);
    }

    pub(super) fn op_cmp(&mut self, value: u8) {
        self.compare(self.a, value);
    }

    pub(super) fn op_cpx(&mut self, value: u8) {
        self.compare(self.x, value);
    }

    pub(super) fn op_cpy(&mut self, value: u8) {
        self.compare(self.y, value);
    }

    // ---- shifts/rotates (accumulator or RMW memory operand) -----------
    pub(super) fn op_asl(&mut self, value: u8) -> u8 {
        self.set_flag(FLAG_C, value & 0x80 != 0);
        let result = value << 1;
        self.set_nz(result);
        result
    }

    pub(super) fn op_lsr(&mut self, value: u8) -> u8 {
        self.set_flag(FLAG_C, value & 0x01 != 0);
        let result = value >> 1;
        self.set_nz(result);
        result
    }

    pub(super) fn op_rol(&mut self, value: u8) -> u8 {
        let carry_in = self.flag(FLAG_C) as u8;
        self.set_flag(FLAG_C, value & 0x80 != 0);
        let result = (value << 1) | carry_in;
        self.set_nz(result);
        result
    }

    pub(super) fn op_ror(&mut self, value: u8) -> u8 {
        let carry_in = self.flag(FLAG_C) as u8;
        self.set_flag(FLAG_C, value & 0x01 != 0);
        let result = (value >> 1) | (carry_in << 7);
        self.set_nz(result);
        result
    }

    pub(super) fn op_inc(&mut self, value: u8) -> u8 {
        let result = value.wrapping_add(1);
        self.set_nz(result);
        result
    }

    pub(super) fn op_dec(&mut self, value: u8) -> u8 {
        let result = value.wrapping_sub(1);
        self.set_nz(result);
        result
    }
}
