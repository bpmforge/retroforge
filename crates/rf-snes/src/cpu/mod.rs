//! The 65C816 (ticket W6-01a; `docs/design/EMULATION_CORES.md` §3.1,
//! FR-CORE-030).
//!
//! ## What makes this not a 6502 with more registers
//!
//! Three things, and every one of them is a place a 6502-shaped
//! implementation goes subtly wrong:
//!
//! 1. **Register width is runtime state.** The `M` and `X` status flags
//!    decide whether the accumulator and the index registers are 8 or 16
//!    bits *right now*. The same opcode byte reads a one-byte or a
//!    two-byte immediate depending on a flag the program set earlier, so
//!    instruction length is not a property of the opcode. [`Cpu::step`]
//!    therefore asks the flags, never a table.
//! 2. **Emulation mode is not a mode bit you can ignore.** After reset
//!    the chip is a 6502: `E = 1` forces `M = X = 1`, pins the stack into
//!    page 1, and clears the index high bytes. Software leaves it with
//!    `CLC; XCE`. Modelling `E` as merely "another flag" lets a program
//!    run 16-bit code in emulation mode, which no real machine does.
//! 3. **Addresses are 24-bit and come from two different bank
//!    registers.** Code fetches use `PBR`, data accesses use `DBR`, and
//!    direct-page and stack accesses are always bank 0. Getting that
//!    wrong reads plausible bytes from the wrong bank.
//!
//! ## Scope of this ticket
//!
//! W6-01a is "core ops, emulation/native modes, addressing". Vectors,
//! interrupts and the master-cycle memory-speed model are **W6-01b** and
//! are deliberately absent rather than stubbed — a half-implemented
//! interrupt path that silently did nothing would be worse than one that
//! does not exist, because the next ticket would inherit it as though it
//! were finished. [`Cpu::step`] returns the opcode it could not execute
//! instead of guessing.

pub mod addressing;
pub mod bus;
pub mod ops;

pub use bus::{Access, CpuBus, FlatBus};

/// Status register bits.
///
/// `M` and `X` are the two that make this chip what it is: in native mode
/// they select 8- or 16-bit accumulator and index registers. In emulation
/// mode both are forced set and `X`'s bit position doubles as the 6502's
/// `B` (break) flag — which is why [`Cpu::set_emulation`] exists rather
/// than callers poking `e` directly.
pub mod flags {
    pub const C: u8 = 0x01;
    pub const Z: u8 = 0x02;
    pub const I: u8 = 0x04;
    pub const D: u8 = 0x08;
    /// Index width in native mode (1 = 8-bit); the 6502's B flag in
    /// emulation mode.
    pub const X: u8 = 0x10;
    /// Accumulator width in native mode (1 = 8-bit). Has no emulation-
    /// mode meaning; forced set.
    pub const M: u8 = 0x20;
    pub const V: u8 = 0x40;
    pub const N: u8 = 0x80;
}

/// The 65C816 register file and mode state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cpu {
    /// Full 16-bit accumulator. In 8-bit mode only the low half is
    /// visible to most operations, but the high half (`B`) **is
    /// preserved** — that is what makes `XBA` and `TCD` work, and
    /// truncating it on every 8-bit write is the single most common way
    /// to get this chip wrong.
    pub a: u16,
    pub x: u16,
    pub y: u16,
    /// Stack pointer. 16-bit in native mode; in emulation mode the high
    /// byte is pinned to `$01`.
    pub sp: u16,
    /// Direct page register. Adds to every direct-page address, and is
    /// NOT restricted to page boundaries — a misaligned `D` costs a cycle
    /// on hardware and shifts every direct address by an odd amount.
    pub d: u16,
    /// Data bank: the bank absolute data accesses land in.
    pub dbr: u8,
    /// Program bank: the bank instructions are fetched from.
    pub pbr: u8,
    pub pc: u16,
    pub p: u8,
    /// Emulation mode. See the module doc — this is not just a flag.
    pub e: bool,
    /// Set when the CPU has stopped (`STP`) or is waiting (`WAI`).
    pub stopped: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// A CPU in the state a 65C816 powers up in: **emulation mode**, with
    /// interrupts disabled.
    ///
    /// Not "all registers zero": the chip comes out of reset as a 6502,
    /// and a core that started in native mode would run the first
    /// instruction of every ROM with the wrong register widths.
    #[must_use]
    pub fn new() -> Self {
        let mut cpu = Cpu {
            a: 0,
            x: 0,
            y: 0,
            sp: 0x01FF,
            d: 0,
            dbr: 0,
            pbr: 0,
            pc: 0,
            p: flags::I,
            e: true,
            stopped: false,
        };
        cpu.apply_emulation_constraints();
        cpu
    }

    #[must_use]
    pub fn flag(&self, mask: u8) -> bool {
        self.p & mask != 0
    }

    pub fn set_flag(&mut self, mask: u8, on: bool) {
        if on {
            self.p |= mask;
        } else {
            self.p &= !mask;
        }
    }

    /// Is the accumulator 8 bits wide right now?
    ///
    /// Always true in emulation mode, regardless of `M` — the flag is
    /// forced, and asking `e` first means a caller cannot observe an
    /// impossible state even if something cleared `M` by mistake.
    #[must_use]
    pub fn a8(&self) -> bool {
        self.e || self.flag(flags::M)
    }

    /// Are the index registers 8 bits wide right now?
    #[must_use]
    pub fn i8(&self) -> bool {
        self.e || self.flag(flags::X)
    }

    /// Enter or leave emulation mode, applying everything that implies.
    ///
    /// This is the only supported way to change `e`, because the mode
    /// change has three side effects a bare assignment would miss: `M`
    /// and `X` are forced set, the stack pointer's high byte is pinned to
    /// `$01`, and the index registers' high bytes are cleared. A program
    /// that entered emulation mode with `X = $1234` and later read `X`
    /// would see `$0034` on hardware.
    pub fn set_emulation(&mut self, e: bool) {
        self.e = e;
        if e {
            self.apply_emulation_constraints();
        }
    }

    fn apply_emulation_constraints(&mut self) {
        if !self.e {
            return;
        }
        self.p |= flags::M | flags::X;
        self.sp = 0x0100 | (self.sp & 0x00FF);
        self.x &= 0x00FF;
        self.y &= 0x00FF;
    }

    /// The full 24-bit address of the next instruction.
    #[must_use]
    pub fn pc24(&self) -> u32 {
        (u32::from(self.pbr) << 16) | u32::from(self.pc)
    }

    /// Fetch one byte at `PC` and advance.
    ///
    /// `PC` wraps **within its bank**: a fetch at `$xx:FFFF` is followed
    /// by one at `$xx:0000`, not `$xx+1:0000`. The program bank only
    /// changes through `JML`/`JSL`/`RTL` and interrupts, never by
    /// falling off the end of a bank.
    pub fn fetch8(&mut self, bus: &mut dyn CpuBus) -> u8 {
        let addr = self.pc24();
        self.pc = self.pc.wrapping_add(1);
        bus.read(addr)
    }

    /// Fetch a little-endian 16-bit operand.
    pub fn fetch16(&mut self, bus: &mut dyn CpuBus) -> u16 {
        let lo = self.fetch8(bus);
        let hi = self.fetch8(bus);
        u16::from(lo) | (u16::from(hi) << 8)
    }

    /// Set N and Z from an 8- or 16-bit result.
    pub fn set_nz(&mut self, value: u16, eight: bool) {
        if eight {
            self.set_flag(flags::Z, value & 0xFF == 0);
            self.set_flag(flags::N, value & 0x80 != 0);
        } else {
            self.set_flag(flags::Z, value == 0);
            self.set_flag(flags::N, value & 0x8000 != 0);
        }
    }

    /// Push one byte.
    ///
    /// In emulation mode the stack is confined to page 1: only the low
    /// byte of `SP` moves, so a push at `$0100` wraps to `$01FF` rather
    /// than descending into page 0. Native mode uses the full 16 bits.
    pub fn push8(&mut self, bus: &mut dyn CpuBus, value: u8) {
        bus.write(u32::from(self.sp), value);
        if self.e {
            self.sp = 0x0100 | u16::from((self.sp as u8).wrapping_sub(1));
        } else {
            self.sp = self.sp.wrapping_sub(1);
        }
    }

    pub fn pull8(&mut self, bus: &mut dyn CpuBus) -> u8 {
        if self.e {
            self.sp = 0x0100 | u16::from((self.sp as u8).wrapping_add(1));
        } else {
            self.sp = self.sp.wrapping_add(1);
        }
        bus.read(u32::from(self.sp))
    }

    /// Push 16 bits, high byte first — the order every 65x push uses, and
    /// the order `pull16` therefore has to undo.
    pub fn push16(&mut self, bus: &mut dyn CpuBus, value: u16) {
        self.push8(bus, (value >> 8) as u8);
        self.push8(bus, value as u8);
    }

    pub fn pull16(&mut self, bus: &mut dyn CpuBus) -> u16 {
        let lo = self.pull8(bus);
        let hi = self.pull8(bus);
        u16::from(lo) | (u16::from(hi) << 8)
    }

    /// Execute one instruction.
    ///
    /// Returns `Err(opcode)` for anything this ticket does not implement.
    /// Deliberately not a silent NOP: W6-01b inherits this core, and an
    /// unimplemented opcode that quietly did nothing would look like a
    /// working CPU running wrong code.
    ///
    /// # Errors
    /// Returns the opcode byte when it is not implemented here.
    pub fn step(&mut self, bus: &mut dyn CpuBus) -> Result<(), u8> {
        if self.stopped {
            return Ok(());
        }
        let opcode = self.fetch8(bus);
        ops::execute(self, bus, opcode)
    }
}

#[cfg(test)]
mod tests;
