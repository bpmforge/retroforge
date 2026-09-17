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
        o.u8(self.div_steps)
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
