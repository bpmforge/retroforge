//! Pure register/ALU semantics, decoupled from bus timing.
//!
//! Every function here takes the operand byte already fetched by
//! `addressing.rs` and updates registers/flags only — no bus access. This
//! keeps the nesdev-verified *timing* (addressing.rs, exec.rs) separate
//! from the nesdev-verified *arithmetic* (this file), so a mistake in one
//! is easy to localize against the SingleStepTests vectors.
use super::{Cpu, UnstableOp, FLAG_C, FLAG_N, FLAG_V, FLAG_Z};

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

    // ---- unofficial: stable RMW combo ops (nesdev.org/wiki/
    // Programming_with_unofficial_opcodes) — each does the RMW's normal
    // read-modify-write half (reusing the matching official op, whose own
    // flag effects get superseded below where relevant) then folds the
    // result into `A` via the named ALU op. ----------------------------
    /// `SLO` (`ASO`): `ASL` the operand, then `ORA` the shifted result
    /// into `A`. `op_asl`'s own `N`/`Z` (from the shifted *memory* value)
    /// are superseded by the final `set_nz(self.a)`; its `C` (bit 7
    /// shifted out) is the instruction's final `C`.
    pub(super) fn op_slo(&mut self, value: u8) -> u8 {
        let shifted = self.op_asl(value);
        self.a |= shifted;
        self.set_nz(self.a);
        shifted
    }

    /// `RLA`: `ROL` the operand, then `AND` the rotated result into `A`.
    pub(super) fn op_rla(&mut self, value: u8) -> u8 {
        let rotated = self.op_rol(value);
        self.a &= rotated;
        self.set_nz(self.a);
        rotated
    }

    /// `SRE` (`LSE`): `LSR` the operand, then `EOR` the shifted result
    /// into `A`.
    pub(super) fn op_sre(&mut self, value: u8) -> u8 {
        let shifted = self.op_lsr(value);
        self.a ^= shifted;
        self.set_nz(self.a);
        shifted
    }

    /// `RRA`: `ROR` the operand, then `ADC` the rotated result into `A`.
    /// Unlike `SLO`/`RLA`/`SRE`, the *second* half's flags win outright
    /// (`op_adc` sets `C`/`V`/`N`/`Z` from the addition, overwriting
    /// whatever `op_ror` set) — matches real hardware and the nes6502
    /// vectors.
    pub(super) fn op_rra(&mut self, value: u8) -> u8 {
        let rotated = self.op_ror(value);
        self.op_adc(rotated);
        rotated
    }

    /// `DCP` (`DCM`): decrement the operand, then `CMP` it against `A`.
    /// The decrement itself never touches flags here (unlike plain
    /// `DEC`) — only the comparison does.
    pub(super) fn op_dcp(&mut self, value: u8) -> u8 {
        let decremented = value.wrapping_sub(1);
        self.compare(self.a, decremented);
        decremented
    }

    /// `ISC` (`ISB`/`INS`): increment the operand, then `SBC` it from `A`.
    pub(super) fn op_isc(&mut self, value: u8) -> u8 {
        let incremented = value.wrapping_add(1);
        self.op_sbc(incremented);
        incremented
    }

    // ---- unofficial: combined load (stable) --------------------------
    /// `LAX`: `LDA` and `LDX` from the same fetch. A *stable* illegal op
    /// (unlike `LXA`/`$AB`, its immediate-mode cousin below, which is
    /// unstable).
    pub(super) fn op_lax(&mut self, value: u8) {
        self.a = value;
        self.x = value;
        self.set_nz(value);
    }

    // ---- unofficial: stable immediate combo ops -----------------------
    /// `ANC`: `AND`, then copy the result's sign bit into `C` (as if
    /// followed by an `ASL`/`ROL` into a 9th bit). Two opcodes (`$0B`,
    /// `$2B`) share this behavior — verified against 1000 cases each of
    /// the nes6502 SingleStepTests `0b.json`/`2b.json` this session.
    pub(super) fn op_anc(&mut self, value: u8) {
        self.a &= value;
        self.set_nz(self.a);
        self.set_flag(FLAG_C, self.a & 0x80 != 0);
    }

    /// `ALR` (`ASR`): `AND` then logical-shift-right `A`.
    pub(super) fn op_alr(&mut self, value: u8) {
        self.a &= value;
        self.a = self.op_lsr(self.a);
    }

    /// `ARR`: `AND`, then rotate right *through carry*, but with `C`/`V`
    /// taken from bits 6/5 of the rotated result rather than the rotate's
    /// own carry-out (nesdev.org/wiki/Programming_with_unofficial_opcodes
    /// "ARR" binary-mode formula — the 2A03 never runs the BCD-mode
    /// variant since it has no decimal ALU, `cpu/mod.rs` module doc).
    /// Verified bit-for-bit against 2000 nes6502 SingleStepTests
    /// `6b.json` cases this session.
    pub(super) fn op_arr(&mut self, value: u8) {
        let carry_in = self.flag(FLAG_C) as u8;
        let anded = self.a & value;
        let rotated = (anded >> 1) | (carry_in << 7);
        self.a = rotated;
        self.set_flag(FLAG_N, rotated & 0x80 != 0);
        self.set_flag(FLAG_Z, rotated == 0);
        self.set_flag(FLAG_C, rotated & 0x40 != 0);
        self.set_flag(FLAG_V, ((rotated >> 6) ^ (rotated >> 5)) & 1 != 0);
    }

    /// `SBX` (`AXS`/`SAX` immediate): `X = (A & X) - operand`, `C` set on
    /// no borrow — a `CMP`-style subtraction (no input carry; `C` means
    /// "no borrow" the same way `CMP`'s does), not `SBC`.
    pub(super) fn op_sbx(&mut self, value: u8) {
        let anded = self.a & self.x;
        self.set_flag(FLAG_C, anded >= value);
        self.x = anded.wrapping_sub(value);
        self.set_nz(self.x);
    }

    // ---- unofficial: unstable ops (EMULATION_CORES.md §2.1) -----------
    // Real hardware ANDs in an extra "magic" constant here that varies by
    // chip temperature/manufacturing batch — not fully deterministic on
    // real silicon. nesdev.org/wiki/Visual6502wiki/6502_Opcode_8B_(XAA,_ANE)
    // gives the formula as `A = (A | magic) & X & imm` and lists observed
    // "magic" values across different chip samples ($FF, $FE, $EE, $00)
    // without ranking their commonality; LXA isn't covered by that page at
    // all. $EE is the specific value the nes6502 SingleStepTests vectors
    // were generated with — verified bit-for-bit against 1000 cases each
    // of `8b.json`/`ab.json` this session (both formulas hold exactly
    // with `magic = $EE` and fail otherwise), which is what pins the
    // exact byte down; nesdev only establishes that it's a plausible
    // value, not that it's *the* value.
    /// `ANE`/`XAA` (`$8B`) — unstable: `A' = (A | $EE) & X & operand`.
    /// Sets [`Cpu::unstable_op`] to [`UnstableOp::Ane`].
    pub(super) fn op_ane(&mut self, value: u8) {
        self.a = (self.a | 0xEE) & self.x & value;
        self.set_nz(self.a);
        self.unstable_op = Some(UnstableOp::Ane);
    }

    /// `LXA` (`LAX` immediate, `$AB`) — unstable, same magic constant and
    /// formula shape as `ANE`: `A = X = (A | $EE) & operand`. Sets
    /// [`Cpu::unstable_op`] to [`UnstableOp::Lxa`].
    pub(super) fn op_lxa(&mut self, value: u8) {
        let result = (self.a | 0xEE) & value;
        self.a = result;
        self.x = result;
        self.set_nz(result);
        self.unstable_op = Some(UnstableOp::Lxa);
    }

    /// `LAS`/`LAR` (`$BB`, abs,Y) — unstable per EMULATION_CORES.md §2.1's
    /// classification, though the formula itself (`A = X = S = M & S`) is
    /// deterministic — no magic constant involved, unlike `ANE`/`LXA`.
    /// nesdev's unofficial-opcode reference pages list `LAS`'s mnemonic
    /// and addressing mode but don't spell out this formula; verified
    /// directly against 1000 `bb.json` nes6502 SingleStepTests cases this
    /// session instead. Sets [`Cpu::unstable_op`] to [`UnstableOp::Las`]
    /// to match the ticket's classification even though no magic constant
    /// is involved.
    pub(super) fn op_las(&mut self, value: u8) {
        let result = value & self.s;
        self.a = result;
        self.x = result;
        self.s = result;
        self.set_nz(result);
        self.unstable_op = Some(UnstableOp::Las);
    }

    // ---- unofficial: unstable address-dependent stores -----------------
    // `SHA`/`SHX`/`SHY`/`TAS` don't touch any flag (verified: `p` is
    // unchanged across every sampled nes6502 vector case for `$93`/`$9F`/
    // `$9E`/`$9C`/`$9B`) — each just computes the byte written, using the
    // *base* address's high byte from `addressing.rs`'s
    // `am_absi_unstable`/`am_indy_unstable` (whether that byte ends up at
    // the effective address or corrupts it on a page cross is decided by
    // the caller in `exec.rs`, since only it knows which happened).
    /// `SHA`/`AHX` (`$93`, `$9F`): `A & X & (hi+1)`. Sets
    /// [`Cpu::unstable_op`] to [`UnstableOp::Sha`].
    pub(super) fn op_sha_value(&mut self, hi: u8) -> u8 {
        let value = self.a & self.x & hi.wrapping_add(1);
        self.unstable_op = Some(UnstableOp::Sha);
        value
    }

    /// `SHX`/`SXA` (`$9E`): `X & (hi+1)`. Sets [`Cpu::unstable_op`] to
    /// [`UnstableOp::Shx`].
    pub(super) fn op_shx_value(&mut self, hi: u8) -> u8 {
        let value = self.x & hi.wrapping_add(1);
        self.unstable_op = Some(UnstableOp::Shx);
        value
    }

    /// `SHY`/`SYA` (`$9C`): `Y & (hi+1)`. Sets [`Cpu::unstable_op`] to
    /// [`UnstableOp::Shy`].
    pub(super) fn op_shy_value(&mut self, hi: u8) -> u8 {
        let value = self.y & hi.wrapping_add(1);
        self.unstable_op = Some(UnstableOp::Shy);
        value
    }

    /// `TAS`/`SHS` (`$9B`): `S = A & X` (unconditionally, even if the
    /// write itself gets address-corrupted), then the written byte is
    /// `S & (hi+1)`. Sets [`Cpu::unstable_op`] to [`UnstableOp::Tas`].
    pub(super) fn op_tas_value(&mut self, hi: u8) -> u8 {
        self.s = self.a & self.x;
        let value = self.s & hi.wrapping_add(1);
        self.unstable_op = Some(UnstableOp::Tas);
        value
    }
}
