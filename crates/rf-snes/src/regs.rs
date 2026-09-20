//! 5A22 registers: multiply/divide, NMITIMEN, H/V IRQ, WRAM port
//! (ticket W6-02a; `docs/design/EMULATION_CORES.md` §3.1).
//!
//! ## Latency is the point, not an optimisation
//!
//! The hardware multiplier and divider are not instantaneous: a write to
//! `$4203` starts an 8-step multiply, a write to `$4206` starts a 16-step
//! divide, and **the result registers are readable while that is still
//! running**. Software that reads too early sees a partial shift-register
//! state, not a stale result and not the final answer. Games rely on the
//! timing (the standard idiom is to do useful work between the write and
//! the read, counted in cycles), so a model that completed the operation
//! instantly would be wrong in the one direction that matters.
//!
//! The stepping below is the shift-and-add / shift-and-subtract the
//! hardware performs, one step at a time, so an intermediate read gets
//! whatever the register genuinely holds at that moment rather than an
//! interpolation. Per fullsnes "Math Registers".
//!
//! **What is verified here and what is not:** the final results and the
//! latency boundary are asserted in this crate's tests. The exact
//! intermediate bit patterns are not independently verifiable without
//! hardware, and their oracle is gilyon `cputest` — which is **W6-02b**,
//! the very next ticket. Said out loud because "intermediates implemented"
//! reads as "intermediates verified" if nobody separates them.

/// Cycles a multiply takes.
pub const MUL_STEPS: u8 = 8;
/// Cycles a divide takes.
pub const DIV_STEPS: u8 = 16;

/// The multiply/divide unit.
#[derive(Debug, Clone, Default)]
pub struct MathUnit {
    /// `$4202` multiplicand.
    pub wrmpya: u8,
    /// `$4204`/`$4205` dividend.
    pub wrdiv: u16,
    /// `$4214`/`$4215` — quotient after a divide, and the shift register
    /// during either operation.
    pub rddiv: u16,
    /// `$4216`/`$4217` — product after a multiply, remainder after a
    /// divide.
    pub rdmpy: u16,
    shift: u32,
    mpy_steps: u8,
    div_steps: u8,
    /// Master cycles accrued toward the next internal-cycle step (ticket
    /// W14-24). See [`MathUnit::tick`].
    carry: u32,
}

impl MathUnit {
    /// Is an operation still running? Reads during this window see
    /// intermediate state.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.mpy_steps > 0 || self.div_steps > 0
    }

    /// `$4203` write — begins an 8-step multiply.
    pub fn start_multiply(&mut self, multiplier: u8) {
        self.rdmpy = 0;
        if self.busy() {
            // Hardware ignores a retrigger while the unit is running.
            return;
        }
        self.rddiv = (u16::from(multiplier) << 8) | u16::from(self.wrmpya);
        self.shift = u32::from(multiplier);
        self.mpy_steps = MUL_STEPS;
    }

    /// `$4206` write — begins a 16-step divide of `wrdiv` by `divisor`.
    pub fn start_divide(&mut self, divisor: u8) {
        self.rdmpy = self.wrdiv;
        if self.busy() {
            return;
        }
        self.rddiv = self.wrdiv;
        self.shift = u32::from(divisor) << 16;
        self.div_steps = DIV_STEPS;
    }

    /// Advance one step. Called once per CPU cycle by the bus.
    pub fn step(&mut self) {
        if self.mpy_steps > 0 {
            self.mpy_steps -= 1;
            if self.rddiv & 1 != 0 {
                self.rdmpy = self.rdmpy.wrapping_add(self.shift as u16);
            }
            self.rddiv >>= 1;
            self.shift <<= 1;
        } else if self.div_steps > 0 {
            self.div_steps -= 1;
            self.rddiv <<= 1;
            self.shift >>= 1;
            if u32::from(self.rdmpy) >= self.shift {
                self.rdmpy = (u32::from(self.rdmpy) - self.shift) as u16;
                self.rddiv |= 1;
            }
        }
    }

    /// Run to completion. For callers that do not model cycles yet — and
    /// named so that using it is a visible choice rather than an accident.
    pub fn settle(&mut self) {
        while self.busy() {
            self.step();
        }
    }

    /// Advance by `master_cycles` — this instruction's bus-access cost,
    /// in master cycles, as charged by `AccessCost` (ticket W14-24).
    ///
    /// ## What the real hardware does, and what this does NOT model
    ///
    /// Per fullsnes ("SNES Maths Multiply/Divide"): "set WRDIVB, wait 16
    /// clk cycles, then read the ... result" (8 for multiply), and "the
    /// 42xxh Ports are clocked by the CPU Clock, meaning that one needs
    /// the same amount of 'wait' opcodes no matter if the CPU Clock is
    /// 3.5MHz or 2.6MHz" — i.e. the latency is a CPU-cycle count,
    /// independent of whether any given cycle is fast or slow on the
    /// bus, or internal. Modelling that correctly needs a per-
    /// instruction real CPU-cycle count, internal cycles included — the
    /// cycle-accurate executor `speed.rs`'s own doc and W6-02a's close
    /// note both defer.
    ///
    /// This crate does not have that yet. `AccessCost` (correctly, per
    /// its own doc) charges **bus accesses only**: an instruction with
    /// more real CPU cycles than bus accesses — `INX`, `INY`, `NOP`,
    /// `TAX`, `XBA`'s second cycle, any register-only op — contributes
    /// nothing extra to `master_cycles` for its unmodelled internal
    /// cycles. So `master_cycles` is never a CPU-cycle count; it is
    /// always an *undercount* of one, by exactly the internal cycles an
    /// instruction spent.
    ///
    /// ## What this method actually does about that
    ///
    /// It steps [`step`](Self::step) once per [`crate::cpu::speed::FAST`]
    /// (6) master cycles of `master_cycles` accrued, rather than once per
    /// call (`SnesBus::tick_math`'s previous contract, one step per bus
    /// access regardless of that access's cost). Since real accesses on
    /// this machine cost 6, 8 or 12 master cycles (`speed.rs`'s table),
    /// re-bucketing at 6 makes a `SLOW` (8) access worth 8/6 ≈ 1.33 steps
    /// and an `XSLOW` (12) access worth 2 — an over-credit on every
    /// access slower than `FAST`, not a measurement of internal cycles at
    /// all. That over-credit is a deliberate, coarse compensation for the
    /// undercount described above, not a fix for it: it cannot make the
    /// unit finish *later* than hardware (it only ever adds steps
    /// hardware would also eventually deliver, sooner), so software that
    /// waits out the documented latency before reading is unaffected,
    /// while software that reads a partial result — as the bug below
    /// did — can end up seeing a completed one instead.
    ///
    /// Traced to Super Mario RPG's boot-upload packet-count computation
    /// (`docs/TESTING.md`, W14-24): a `$4206` divide-start followed by
    /// `INY; INY; STY $3D; NOP; NOP; LDX #$FFFF` — the ordinary,
    /// documented idiom for waiting out the divider's 16-cycle latency —
    /// then `LDA $4215`/`LDA $4214`. Under the *old* per-access model,
    /// that filler (all internal-only, contributing zero access-based
    /// steps) left the divide still running at the `$4215` read
    /// ([`Self::busy`] true), so the game got a partial shift-register
    /// high byte (`0x80` instead of the correct `0x00`) and computed a
    /// packet count of `0x8021` instead of `33` — the ARAM overflow this
    /// ticket traced (`title_probe`'s `PROBE_MATHPC` on PCs
    /// `$C4:04FD`/`$C4:0505`). Under the access-cost-in-6-cycle-buckets
    /// model here, the same window's `SLOW`/`XSLOW` accesses (`STY`,
    /// `LDX`'s operand fetch, the opcode fetches themselves) total enough
    /// re-bucketed steps to finish the divide before that read.
    ///
    /// **Known residual, not fixed here:** the `$4206` write that calls
    /// [`Self::start_divide`] happens mid-instruction (inside
    /// `SnesBus::write`), but `SnesSystem::step` calls this method with
    /// that whole instruction's `master_cycles` only *after* `Cpu::step`
    /// returns — so the accesses the triggering instruction made
    /// *before* reaching the `$4206` write (its own opcode/operand
    /// fetches) are also credited toward the divide's latency, over-
    /// crediting the first `tick` call by a few steps. Harmless for
    /// every case checked here (`gilyon_cputest` and the SMRPG fix both
    /// have comfortable margin), but a title timed exactly against the
    /// 16-step boundary could still see a read complete a step or two
    /// early. A true fix needs sub-instruction timing (the cycle-
    /// accurate executor W6-02a defers), not a change to this method.
    pub fn tick(&mut self, master_cycles: u32) {
        if !self.busy() {
            self.carry = 0;
            return;
        }
        const CYCLE: u32 = crate::cpu::speed::FAST as u32;
        self.carry += master_cycles;
        while self.carry >= CYCLE {
            if !self.busy() {
                break;
            }
            self.carry -= CYCLE;
            self.step();
        }
        if !self.busy() {
            self.carry = 0;
        }
    }
}

/// `$4200` NMITIMEN — NMI, IRQ mode and auto-joypad enables.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NmiTimen(pub u8);

impl NmiTimen {
    #[must_use]
    pub fn nmi_enabled(self) -> bool {
        self.0 & 0x80 != 0
    }
    /// Bits 4-5: 0 = off, 1 = H, 2 = V, 3 = H and V.
    #[must_use]
    pub fn irq_mode(self) -> IrqMode {
        match (self.0 >> 4) & 0x03 {
            0 => IrqMode::Off,
            1 => IrqMode::Horizontal,
            2 => IrqMode::Vertical,
            _ => IrqMode::Both,
        }
    }
    #[must_use]
    pub fn auto_joypad(self) -> bool {
        self.0 & 0x01 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqMode {
    Off,
    Horizontal,
    Vertical,
    Both,
}

/// H/V IRQ comparison state: `$4207`-`$420A` targets and the `$4211`
/// acknowledge flag.
#[derive(Debug, Clone, Copy, Default)]
pub struct IrqTimer {
    /// `$4207`/`$4208` — 9 bits.
    pub htime: u16,
    /// `$4209`/`$420A` — 9 bits.
    pub vtime: u16,
    /// Latched when the comparison fires; cleared by reading `$4211`.
    pub fired: bool,
}

impl IrqTimer {
    /// Does the configured comparison match this position?
    #[must_use]
    pub fn matches(&self, mode: IrqMode, h_dot: u16, v_line: u16) -> bool {
        match mode {
            IrqMode::Off => false,
            IrqMode::Horizontal => h_dot == self.htime,
            IrqMode::Vertical => v_line == self.vtime && h_dot == 0,
            IrqMode::Both => h_dot == self.htime && v_line == self.vtime,
        }
    }

    /// `$4211` TIMEUP: returns the flag in bit 7 and **clears it** —
    /// reading acknowledges. A `peek` must not route here.
    pub fn read_timeup(&mut self) -> u8 {
        let v = u8::from(self.fired) << 7;
        self.fired = false;
        v
    }
}

/// `$2180`-`$2183` WRAM port: a 17-bit auto-incrementing address into
/// work RAM, letting the CPU reach all 128 KiB without banking.
#[derive(Debug, Clone, Copy, Default)]
pub struct WramPort {
    /// 17 bits — one more than a bank offset, which is the whole point.
    pub address: u32,
}

impl WramPort {
    /// Advance after an access, wrapping within the 128 KiB.
    pub fn advance(&mut self) {
        self.address = (self.address + 1) & 0x0001_FFFF;
    }
    pub fn set_low(&mut self, v: u8) {
        self.address = (self.address & 0x0001_FF00) | u32::from(v);
    }
    pub fn set_mid(&mut self, v: u8) {
        self.address = (self.address & 0x0001_00FF) | (u32::from(v) << 8);
    }
    pub fn set_high(&mut self, v: u8) {
        self.address = (self.address & 0x0000_FFFF) | ((u32::from(v) & 1) << 16);
    }
}

impl MathUnit {
    /// Serialise the multiply/divide unit (ticket W7-09).
    ///
    /// `shift`, `mpy_steps` and `div_steps` are mid-operation state: the
    /// unit takes real cycles, and a game that starts a divide and reads
    /// the result a few instructions later depends on how far it has got.
    /// Saving only the operands would restart the arithmetic and hand back
    /// a stale `rddiv`.
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        o.u8(self.wrmpya)?;
        o.u16(self.wrdiv)?;
        o.u16(self.rddiv)?;
        o.u16(self.rdmpy)?;
        o.u32(self.shift)?;
        o.u8(self.mpy_steps)?;
        o.u8(self.div_steps)?;
        o.u32(self.carry)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        self.wrmpya = i.u8()?;
        self.wrdiv = i.u16()?;
        self.rddiv = i.u16()?;
        self.rdmpy = i.u16()?;
        self.shift = i.u32()?;
        self.mpy_steps = i.u8()?;
        self.div_steps = i.u8()?;
        self.carry = i.u32()?;
        Ok(())
    }
}

/// The H/V counter latch: `$2137` SLHV, `$213C`/`$213D` OPHCT/OPVCT,
/// `$213F` STAT78 and the `$4201` WRIO edge (ticket W14-10).
///
/// fullsnes, "SNES PPU Interrupts and Timers":
///
/// * `2137h - SLHV - Latch H/V-Counter by Software (R)` — reading latches
///   the current H/V counters, exactly as a 1-to-0 transition of WRIO
///   bit 7 does. The read itself returns open bus.
/// * `213Ch - OPHCT` / `213Dh - OPVCT` — each is read in two halves
///   through its own flip-flop: the first read gives bits 0-7, the second
///   gives bit 8 in bit 0 with PPU2 open bus above it.
/// * `213Fh - STAT78` — bit 7 interlace field, bit 6 "H/V-Counter latch
///   flag (0=No, 1=Latched)", bit 4 "Frame rate (0=NTSC, 1=PAL)", bits
///   0-3 the PPU2 version. Reading it resets both OPHCT/OPVCT flip-flops
///   and clears the latch flag.
/// * `4201h - WRIO` — "bit 7: latch H/V counters on 1-to-0 transition".
///
/// Final Fantasy Mystic Quest reads `$2137`, `$213F`, `$213D` in a loop
/// and spins until the latched line is even; with none of these mapped
/// it read open bus for ever.
#[derive(Debug, Clone, Copy)]
pub struct HvLatch {
    /// Latched dot (0-339) and line (0-261/311).
    pub h: u16,
    pub v: u16,
    /// STAT78 bit 6.
    pub latched: bool,
    /// Which half the next `$213C` / `$213D` read returns.
    pub h_second: bool,
    pub v_second: bool,
    /// Last `$4201` value, for the bit-7 falling edge. `$FF` at reset:
    /// both I/O port lines idle high.
    pub wrio: u8,
}

impl Default for HvLatch {
    fn default() -> Self {
        Self {
            h: 0,
            v: 0,
            latched: false,
            h_second: false,
            v_second: false,
            wrio: 0xFF,
        }
    }
}

impl HvLatch {
    /// Capture the beam position and raise the latch flag.
    pub fn latch(&mut self, dot: u16, line: u16) {
        self.h = dot;
        self.v = line;
        self.latched = true;
    }

    /// `$213C` OPHCT: low byte, then bit 8 over PPU2 open bus.
    pub fn read_ophct(&mut self, open_bus: u8) -> u8 {
        let v = Self::half(self.h, self.h_second, open_bus);
        self.h_second = !self.h_second;
        v
    }

    /// `$213D` OPVCT: low byte, then bit 8 over PPU2 open bus.
    pub fn read_opvct(&mut self, open_bus: u8) -> u8 {
        let v = Self::half(self.v, self.v_second, open_bus);
        self.v_second = !self.v_second;
        v
    }

    fn half(counter: u16, second: bool, open_bus: u8) -> u8 {
        if second {
            (open_bus & 0xFE) | ((counter >> 8) as u8 & 1)
        } else {
            counter as u8
        }
    }

    /// `$213F` STAT78. Reading resets both flip-flops and the latch flag.
    pub fn read_stat78(&mut self, pal: bool, open_bus: u8) -> u8 {
        let v = self.peek_stat78(pal, open_bus);
        self.latched = false;
        self.h_second = false;
        self.v_second = false;
        v
    }

    /// STAT78 without the read's side effects, for `peek`.
    #[must_use]
    pub fn peek_stat78(&self, pal: bool, open_bus: u8) -> u8 {
        // Bit 7 (interlace field) is not modelled: this core does not
        // track fields. Bit 5 is PPU2 open bus. PPU2 version 3 — the
        // value on every retail 5C78 after the earliest revision.
        (u8::from(self.latched) << 6) | (open_bus & 0x20) | (u8::from(pal) << 4) | 0x03
    }

    /// `$4201` WRIO write. Returns `true` when bit 7 went 1-to-0, which
    /// is the caller's cue to latch.
    pub fn write_wrio(&mut self, value: u8) -> bool {
        let falling = self.wrio & 0x80 != 0 && value & 0x80 == 0;
        self.wrio = value;
        falling
    }
}
