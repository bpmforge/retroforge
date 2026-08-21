//! The SPC700 — the SNES sound CPU (ticket W6-04a;
//! `docs/design/EMULATION_CORES.md` §3.4, FR-CORE-031).
//!
//! ## What makes it not a 6502
//!
//! It looks 6502-ish and is not, in ways that bite:
//!
//! 1. **`YA` is a real 16-bit register.** `Y` is the high half and `A`
//!    the low, and `MOVW`/`ADDW`/`SUBW`/`MUL`/`DIV` operate on the pair.
//! 2. **The direct page is selectable.** `PSW` bit 5 (`P`) chooses
//!    `$0000-$00FF` or `$0100-$01FF`. Every direct-page address is
//!    therefore relative to a base that instructions can change.
//! 3. **`$00F0-$00FF` are I/O registers, inside the address space.** They
//!    are not memory; reads and writes there hit the timers, the DSP port
//!    and the CPU-facing ports.
//! 4. **Its carry convention for subtraction matches the 6502** (borrow
//!    is `!C`), but `CMP` and the `H` half-carry flag do not behave the
//!    way a 6502 programmer expects.
//!
//! ## Vectors
//!
//! SingleStepTests `spc700`: 256 opcode files, 1000 cases each. Schema is
//! simpler than the 65816's — `pc`, `a`, `x`, `y`, `sp`, `psw` and a
//! sparse RAM list. As with the 65816 suite, this runner compares
//! **registers and memory, not the cycle trace**; the cycle-accurate
//! executor is not this ticket's.

pub mod ops;

/// `PSW` bits.
pub mod flags {
    pub const C: u8 = 0x01;
    pub const Z: u8 = 0x02;
    /// Interrupt enable. The SPC700 has no usable interrupt source, so
    /// this is settable and observable but never acted on.
    pub const I: u8 = 0x04;
    pub const H: u8 = 0x08;
    pub const B: u8 = 0x10;
    /// Direct-page select: 0 = `$0000-$00FF`, 1 = `$0100-$01FF`.
    pub const P: u8 = 0x20;
    pub const V: u8 = 0x40;
    pub const N: u8 = 0x80;
}

/// What the SPC700 talks to.
pub trait ApuBus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    /// Side-effect-free read, for debuggers and tests.
    fn peek(&self, addr: u16) -> u8;
}

/// A flat 64 KiB bus with no I/O, for the vector suite.
///
/// The vectors exercise the instruction set, not the hardware around it,
/// and several of them poke `$00F0-$00FF` as ordinary memory — so the
/// runner must NOT route those to timers. That is why this exists
/// separately from [`crate::apu::Apu`]'s bus.
pub struct FlatApuBus {
    pub mem: Vec<u8>,
}

impl Default for FlatApuBus {
    fn default() -> Self {
        Self::new()
    }
}

impl FlatApuBus {
    #[must_use]
    pub fn new() -> Self {
        Self {
            mem: vec![0; 0x1_0000],
        }
    }
}

impl ApuBus for FlatApuBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }
    fn write(&mut self, addr: u16, value: u8) {
        self.mem[addr as usize] = value;
    }
    fn peek(&self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }
}

/// The SPC700 register file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spc700 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub psw: u8,
    /// Set by `STOP`/`SLEEP`.
    pub stopped: bool,
}

impl Default for Spc700 {
    fn default() -> Self {
        Self::new()
    }
}

impl Spc700 {
    #[must_use]
    pub fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xEF,
            pc: 0xFFC0,
            psw: 0,
            stopped: false,
        }
    }

    #[must_use]
    pub fn flag(&self, mask: u8) -> bool {
        self.psw & mask != 0
    }

    pub fn set_flag(&mut self, mask: u8, on: bool) {
        if on {
            self.psw |= mask;
        } else {
            self.psw &= !mask;
        }
    }

    /// `YA`, the 16-bit pair: `Y` high, `A` low.
    #[must_use]
    pub fn ya(&self) -> u16 {
        (u16::from(self.y) << 8) | u16::from(self.a)
    }

    pub fn set_ya(&mut self, v: u16) {
        self.a = v as u8;
        self.y = (v >> 8) as u8;
    }

    /// The base of the direct page, chosen by `PSW` bit 5.
    #[must_use]
    pub fn dp_base(&self) -> u16 {
        if self.flag(flags::P) {
            0x0100
        } else {
            0x0000
        }
    }

    /// A direct-page address. Wraps **within the page**, so `$FF + 1` is
    /// `$00` of the same page rather than the first byte of the next.
    #[must_use]
    pub fn dp(&self, offset: u8) -> u16 {
        self.dp_base() | u16::from(offset)
    }

    pub fn set_nz(&mut self, v: u8) {
        self.set_flag(flags::Z, v == 0);
        self.set_flag(flags::N, v & 0x80 != 0);
    }

    pub fn set_nz16(&mut self, v: u16) {
        self.set_flag(flags::Z, v == 0);
        self.set_flag(flags::N, v & 0x8000 != 0);
    }

    pub fn fetch8(&mut self, bus: &mut dyn ApuBus) -> u8 {
        let v = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }

    pub fn fetch16(&mut self, bus: &mut dyn ApuBus) -> u16 {
        let lo = self.fetch8(bus);
        let hi = self.fetch8(bus);
        u16::from(lo) | (u16::from(hi) << 8)
    }

    /// The stack lives in page 1 always — `$0100 | SP` — and `SP` is only
    /// 8 bits, so it wraps inside that page.
    pub fn push8(&mut self, bus: &mut dyn ApuBus, v: u8) {
        bus.write(0x0100 | u16::from(self.sp), v);
        self.sp = self.sp.wrapping_sub(1);
    }

    pub fn pull8(&mut self, bus: &mut dyn ApuBus) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(0x0100 | u16::from(self.sp))
    }

    pub fn push16(&mut self, bus: &mut dyn ApuBus, v: u16) {
        self.push8(bus, (v >> 8) as u8);
        self.push8(bus, v as u8);
    }

    pub fn pull16(&mut self, bus: &mut dyn ApuBus) -> u16 {
        let lo = self.pull8(bus);
        let hi = self.pull8(bus);
        u16::from(lo) | (u16::from(hi) << 8)
    }

    /// Read a 16-bit word from the direct page, wrapping within it.
    pub fn read_dp16(&mut self, bus: &mut dyn ApuBus, offset: u8) -> u16 {
        let lo = bus.read(self.dp(offset));
        let hi = bus.read(self.dp(offset.wrapping_add(1)));
        u16::from(lo) | (u16::from(hi) << 8)
    }

    pub fn write_dp16(&mut self, bus: &mut dyn ApuBus, offset: u8, v: u16) {
        bus.write(self.dp(offset), v as u8);
        bus.write(self.dp(offset.wrapping_add(1)), (v >> 8) as u8);
    }

    /// Execute one instruction.
    ///
    /// # Errors
    /// Returns the opcode if it is not implemented.
    pub fn step(&mut self, bus: &mut dyn ApuBus) -> Result<(), u8> {
        if self.stopped {
            return Ok(());
        }
        let opcode = self.fetch8(bus);
        ops::execute(self, bus, opcode)
    }
}

impl Spc700 {
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        o.u8(self.a)?;
        o.u8(self.x)?;
        o.u8(self.y)?;
        o.u8(self.sp)?;
        o.u16(self.pc)?;
        o.u8(self.psw)?;
        o.bool(self.stopped)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        self.a = i.u8()?;
        self.x = i.u8()?;
        self.y = i.u8()?;
        self.sp = i.u8()?;
        self.pc = i.u16()?;
        self.psw = i.u8()?;
        self.stopped = i.bool()?;
        Ok(())
    }
}
