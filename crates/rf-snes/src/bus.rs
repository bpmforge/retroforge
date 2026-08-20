//! The SNES bus (ticket W6-02a; FR-CORE-035).
//!
//! Owns the machine's memory and registers, and implements
//! [`crate::cpu::CpuBus`] so the 65816 verified in W6-01a/b can run
//! against a real machine instead of a flat array.
//!
//! ## `read` vs `peek`
//!
//! Several registers here have read side effects — `$4211` acknowledges
//! the timer IRQ, and the WRAM port auto-increments. [`SnesBus::peek`]
//! therefore does **not** route to them: it is what a debugger, tracer or
//! save-state inspector uses, and it must be able to look at any address
//! without changing the machine. This is the contract `CpuBus::peek`
//! spells out, and the trap it warns about (a trace logger perturbing the
//! machine it is tracing) is live from this ticket onward, because before
//! it there were no side-effecting reads to get wrong.

use rf_cart::SnesMapMode;

use crate::cpu::CpuBus;
use crate::dma::{Dma, CYCLES_PER_BYTE, CYCLES_PER_CHANNEL};
use crate::mapping::{map, Target, WRAM_LEN};
use crate::regs::{IrqTimer, MathUnit, NmiTimen, WramPort};

/// The machine's memory, cartridge and registers.
pub struct SnesBus {
    pub rom: Vec<u8>,
    pub sram: Vec<u8>,
    pub wram: Vec<u8>,
    pub mode: SnesMapMode,
    pub math: MathUnit,
    pub nmitimen: NmiTimen,
    pub irq: IrqTimer,
    pub wram_port: WramPort,
    pub dma: Dma,
    /// `$420D` bit 0 — feeds [`crate::cpu::access_cycles`].
    pub fast_rom: bool,
    /// Last value driven on the bus, returned for unmapped reads. Open
    /// bus is not zero, and a game that reads an unmapped address sees
    /// this; returning 0 would be a different (wrong) answer that happens
    /// to look tidier.
    pub open_bus: u8,
    /// `$420B` write that is pending execution.
    pending_dma: u8,
}

impl SnesBus {
    #[must_use]
    pub fn new(rom: Vec<u8>, sram_len: usize, mode: SnesMapMode) -> Self {
        Self {
            rom,
            sram: vec![0; sram_len],
            wram: vec![0; WRAM_LEN],
            mode,
            math: MathUnit::default(),
            nmitimen: NmiTimen::default(),
            irq: IrqTimer::default(),
            wram_port: WramPort::default(),
            dma: Dma::default(),
            fast_rom: false,
            open_bus: 0,
            pending_dma: 0,
        }
    }

    fn target(&self, addr: u32) -> Target {
        map(
            self.mode,
            ((addr >> 16) & 0xFF) as u8,
            addr as u16,
            self.rom.len(),
            self.sram.len(),
        )
    }

    /// Register reads WITHOUT side effects, shared by `read` and `peek`.
    fn read_register_pure(&self, offset: u16) -> Option<u8> {
        Some(match offset {
            0x4214 => self.math.rddiv as u8,
            0x4215 => (self.math.rddiv >> 8) as u8,
            0x4216 => self.math.rdmpy as u8,
            0x4217 => (self.math.rdmpy >> 8) as u8,
            0x4200 => self.nmitimen.0,
            0x420D => u8::from(self.fast_rom),
            0x2180 => self.wram[(self.wram_port.address as usize) % WRAM_LEN],
            _ => return None,
        })
    }

    fn read_register(&mut self, offset: u16) -> u8 {
        match offset {
            // $4211 TIMEUP: reading ACKNOWLEDGES and clears. This is why
            // `peek` cannot share this path.
            0x4211 => self.irq.read_timeup(),
            // $2180 WMDATA: reading auto-increments the port.
            0x2180 => {
                let v = self.wram[(self.wram_port.address as usize) % WRAM_LEN];
                self.wram_port.advance();
                v
            }
            _ => self.read_register_pure(offset).unwrap_or(self.open_bus),
        }
    }

    fn write_register(&mut self, offset: u16, value: u8) {
        match offset {
            0x2180 => {
                let at = (self.wram_port.address as usize) % WRAM_LEN;
                self.wram[at] = value;
                self.wram_port.advance();
            }
            0x2181 => self.wram_port.set_low(value),
            0x2182 => self.wram_port.set_mid(value),
            0x2183 => self.wram_port.set_high(value),
            0x4200 => self.nmitimen = NmiTimen(value),
            0x4202 => self.math.wrmpya = value,
            0x4203 => self.math.start_multiply(value),
            0x4204 => self.math.wrdiv = (self.math.wrdiv & 0xFF00) | u16::from(value),
            0x4205 => self.math.wrdiv = (self.math.wrdiv & 0x00FF) | (u16::from(value) << 8),
            0x4206 => self.math.start_divide(value),
            0x4207 => self.irq.htime = (self.irq.htime & 0x100) | u16::from(value),
            0x4208 => self.irq.htime = (self.irq.htime & 0x0FF) | (u16::from(value & 1) << 8),
            0x4209 => self.irq.vtime = (self.irq.vtime & 0x100) | u16::from(value),
            0x420A => self.irq.vtime = (self.irq.vtime & 0x0FF) | (u16::from(value & 1) << 8),
            0x420B => self.pending_dma = value,
            0x420D => self.fast_rom = value & 1 != 0,
            0x4300..=0x437F => self.write_dma_register(offset, value),
            _ => {}
        }
    }

    fn write_dma_register(&mut self, offset: u16, value: u8) {
        let ch = ((offset >> 4) & 0x07) as usize;
        let c = &mut self.dma.channels[ch];
        match offset & 0x000F {
            0x0 => c.control = value,
            0x1 => c.b_address = value,
            0x2 => c.a_address = (c.a_address & 0x00FF_FF00) | u32::from(value),
            0x3 => c.a_address = (c.a_address & 0x00FF_00FF) | (u32::from(value) << 8),
            0x4 => c.a_address = (c.a_address & 0x0000_FFFF) | (u32::from(value) << 16),
            0x5 => c.count = (c.count & 0xFF00) | u16::from(value),
            0x6 => c.count = (c.count & 0x00FF) | (u16::from(value) << 8),
            _ => {}
        }
    }

    /// Run any DMA armed by a `$420B` write, returning its master-cycle
    /// cost. Called by the system after each instruction — DMA on
    /// hardware halts the CPU, so running it between instructions is the
    /// right granularity for a core that is not yet cycle-stepped.
    pub fn service_dma(&mut self) -> u64 {
        let enabled = std::mem::take(&mut self.pending_dma);
        if enabled == 0 {
            return 0;
        }
        let mut cycles = 0;
        for ch in 0..8 {
            if enabled & (1 << ch) == 0 {
                continue;
            }
            cycles += CYCLES_PER_CHANNEL + self.run_channel(ch) * CYCLES_PER_BYTE;
        }
        cycles
    }

    /// Transfer one channel to completion; returns the byte count.
    fn run_channel(&mut self, ch: usize) -> u64 {
        let c = self.dma.channels[ch];
        // A count of 0 means 65536 bytes — the one place where "no bytes"
        // and "all the bytes" share an encoding.
        let total = if c.count == 0 {
            0x1_0000u32
        } else {
            u32::from(c.count)
        };
        let pattern = c.pattern();
        let step = c.a_step();
        let mut a = c.a_address;
        let mut moved = 0u64;

        for i in 0..total {
            let b = 0x2100u32
                + u32::from(
                    c.b_address
                        .wrapping_add(pattern[i as usize % pattern.len()]),
                );
            if c.reverse() {
                let v = self.read(b);
                self.write(a, v);
            } else {
                let v = self.read(a);
                self.write(b, v);
            }
            // Only the low 16 bits step; DMA does not cross banks.
            let low = ((a as u16) as i32 + step) as u16;
            a = (a & 0x00FF_0000) | u32::from(low);
            moved += 1;
        }

        self.dma.channels[ch].a_address = a;
        self.dma.channels[ch].count = 0;
        moved
    }

    /// Advance the math unit by `cycles` CPU cycles.
    pub fn tick_math(&mut self, cycles: u32) {
        for _ in 0..cycles {
            if !self.math.busy() {
                break;
            }
            self.math.step();
        }
    }
}

impl CpuBus for SnesBus {
    fn read(&mut self, addr: u32) -> u8 {
        let value = match self.target(addr) {
            Target::Rom(i) => self.rom[i],
            Target::Wram(i) => self.wram[i],
            Target::Sram(i) => self.sram[i],
            Target::Register(offset) => self.read_register(offset),
            Target::Open => self.open_bus,
        };
        self.open_bus = value;
        value
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.open_bus = value;
        match self.target(addr) {
            Target::Wram(i) => self.wram[i] = value,
            Target::Sram(i) => self.sram[i] = value,
            Target::Register(offset) => self.write_register(offset, value),
            // ROM is read-only; a write is dropped rather than panicking,
            // because real cartridges ignore it and a game doing it by
            // accident must not take the emulator down (FR-CORE-013's
            // "never a crash" applies to the whole core, not just
            // headers).
            Target::Rom(_) | Target::Open => {}
        }
    }

    fn peek(&self, addr: u32) -> u8 {
        match self.target(addr) {
            Target::Rom(i) => self.rom[i],
            Target::Wram(i) => self.wram[i],
            Target::Sram(i) => self.sram[i],
            // Only the side-effect-free subset. An address whose read has
            // consequences reports open bus rather than firing them.
            Target::Register(offset) => self.read_register_pure(offset).unwrap_or(self.open_bus),
            Target::Open => self.open_bus,
        }
    }
}
