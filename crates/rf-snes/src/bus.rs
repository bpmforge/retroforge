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

use crate::apu::Apu;
use crate::cpu::CpuBus;
use crate::dma::{Dma, CYCLES_PER_BYTE, CYCLES_PER_CHANNEL};
use crate::mapping::{map, Target, WRAM_LEN};
use crate::ppu::Ppu;
use crate::regs::{IrqTimer, MathUnit, NmiTimen, WramPort};
use crate::timing::{Joypads, Timing};

/// The machine's memory, cartridge and registers.
pub struct SnesBus {
    pub rom: Vec<u8>,
    pub sram: Vec<u8>,
    pub wram: Vec<u8>,
    /// `$2116`/`$2117` VMADD — a WORD address into the PPU's VRAM.
    ///
    /// The port lives here (W6-02b built it as a memory-mapped register)
    /// but the STORAGE belongs to the PPU. Keeping a second buffer on the
    /// bus would have been a silent disaster: writes would land here
    /// while rendering read `ppu.vram`, so every game would draw a black
    /// screen with nothing obviously wrong anywhere.
    pub vram_address: u16,
    /// `$2115` VMAIN — increment step and which port write advances it.
    pub vmain: u8,
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
    pub ppu: Ppu,
    pub apu: Apu,
    /// Master cycles the APU still owes.
    ///
    /// **The APU is never free-running** (§3.4). It is caught up
    /// immediately before any `$2140`-`$2143` access, so the CPU can only
    /// ever observe a port state the APU has actually reached. Letting it
    /// run ahead on its own clock and sampling whatever it had got to is
    /// how port handshakes become timing-dependent and games become
    /// flaky on some runs and not others.
    pub apu_debt: u64,
    pub timing: Timing,
    pub joypads: Joypads,
    /// `$4016` strobe latch, and the serial shift position per port.
    manual_latch: bool,
    manual_shift: [u16; 2],
    /// `$420C` HDMAEN. HDMA itself is W7-07; this records what a ROM
    /// asked for so a mode-7 golden can say WHY it renders flat.
    pub hdmaen: u8,
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
            vram_address: 0,
            vmain: 0,
            mode,
            math: MathUnit::default(),
            nmitimen: NmiTimen::default(),
            irq: IrqTimer::default(),
            wram_port: WramPort::default(),
            dma: Dma::default(),
            fast_rom: false,
            open_bus: 0,
            ppu: Ppu::new(),
            apu: Apu::new(),
            apu_debt: 0,
            timing: Timing::new(),
            joypads: Joypads::default(),
            manual_latch: false,
            manual_shift: [0; 2],
            hdmaen: 0,
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
            0x4212 => self.timing.read_hvbjoy(),
            0x213E => self.ppu.read_stat77(),
            // $2134-$2136 MPYL/MPYM/MPYH — the M7A x M7B product.
            //
            // Wiring these is not optional even for a ROM that never uses
            // mode 7: it is the SNES's general-purpose signed multiplier,
            // and PeterLemon's RotZoom computes its rotation matrix with
            // it. Leaving them unmapped returned open bus — which is the
            // last value driven, i.e. $21 from the register address
            // itself — so every matrix element came out as $2121 and the
            // playfield rendered as diagonal stripes.
            0x2134..=0x2136 => {
                let p = self.ppu.mode7.product();
                ((p >> (8 * (offset - 0x2134))) & 0xFF) as u8
            }
            // Safe to peek: reading a port has no side effect, and peek
            // must never trigger the catch-up that `read` does.
            0x2140..=0x2143 => self.apu.cpu_read_port(usize::from(offset - 0x2140)),
            0x4218..=0x421F => {
                let port = ((offset - 0x4218) / 2) as usize;
                let word = self.joypads.latched[port];
                if offset & 1 == 0 {
                    word as u8
                } else {
                    (word >> 8) as u8
                }
            }
            _ => return None,
        })
    }

    fn read_register(&mut self, offset: u16) -> u8 {
        match offset {
            // $4211 TIMEUP: reading ACKNOWLEDGES and clears. This is why
            // `peek` cannot share this path.
            0x4211 => self.irq.read_timeup(),
            // $4210 RDNMI: reading CLEARS the vblank flag. gilyon
            // cputest's wait_for_vblank depends on it — see timing.rs.
            0x4210 => self.timing.read_rdnmi(),
            // $4016/$4017 manual joypad read: each read shifts out one
            // bit, so this cannot be a pure read either.
            0x4016 | 0x4017 => {
                let port = usize::from(offset - 0x4016);
                let bit = (self.manual_shift[port] & 0x8000) >> 15;
                self.manual_shift[port] <<= 1;
                bit as u8
            }
            // $2180 WMDATA: reading auto-increments the port.
            0x2180 => {
                let v = self.wram[(self.wram_port.address as usize) % WRAM_LEN];
                self.wram_port.advance();
                v
            }
            // $2140-$2143: catch the APU up FIRST, so what the CPU reads
            // is a state the APU actually reached.
            0x2140..=0x2143 => {
                self.catch_up_apu();
                self.apu.cpu_read_port(usize::from(offset - 0x2140))
            }
            _ => self.read_register_pure(offset).unwrap_or(self.open_bus),
        }
    }

    fn write_register(&mut self, offset: u16, value: u8) {
        match offset {
            // $2100-$213F belong to the PPU, EXCEPT the VRAM port below,
            // which W6-02b built and which stores into `self.vram`.
            0x2100..=0x2114 | 0x211A..=0x213F => self.ppu.write_register(offset, value),
            0x2140..=0x2143 => {
                self.catch_up_apu();
                self.apu.cpu_write_port(usize::from(offset - 0x2140), value);
            }
            0x2115 => self.vmain = value,
            0x2116 => self.vram_address = (self.vram_address & 0xFF00) | u16::from(value),
            0x2117 => self.vram_address = (self.vram_address & 0x00FF) | (u16::from(value) << 8),
            0x2118 => {
                let at = (self.vram_address as usize * 2) % self.ppu.vram.len();
                self.ppu.vram[at] = value;
                if self.vmain & 0x80 == 0 {
                    self.step_vram_address();
                }
            }
            0x2119 => {
                let at = (self.vram_address as usize * 2 + 1) % self.ppu.vram.len();
                self.ppu.vram[at] = value;
                if self.vmain & 0x80 != 0 {
                    self.step_vram_address();
                }
            }
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
            0x420C => self.hdmaen = value,
            0x420D => self.fast_rom = value & 1 != 0,
            0x4016 => {
                // Strobe: while high, the shift registers reload.
                let strobe = value & 1 != 0;
                if self.manual_latch && !strobe {
                    self.manual_shift = [self.joypads.ports[0], self.joypads.ports[1]];
                }
                self.manual_latch = strobe;
                if strobe {
                    self.manual_shift = [self.joypads.ports[0], self.joypads.ports[1]];
                }
            }
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

    /// Run the APU forward by everything it is owed.
    ///
    /// The SPC700 runs at ~1.024 MHz against a 21.477 MHz master clock,
    /// so one SPC cycle is about 21 master cycles.
    pub fn catch_up_apu(&mut self) {
        const MASTER_PER_SPC_CYCLE: u64 = 21;
        let mut spc_cycles = self.apu_debt / MASTER_PER_SPC_CYCLE;
        self.apu_debt -= spc_cycles * MASTER_PER_SPC_CYCLE;
        // Bound the work a single catch-up can do. A long DMA or a paused
        // debugger can otherwise hand the APU millions of cycles at once,
        // and grinding through them inside one bus access would stall the
        // whole emulator at exactly the moment a game is polling a port.
        spc_cycles = spc_cycles.min(64);
        for _ in 0..spc_cycles {
            if self.apu.cpu.stopped || !self.apu.boot.is_running() {
                // Nothing to execute: either halted, or the HLE boot
                // handshake still owns the machine.
                self.apu.tick_timers(1);
                continue;
            }
            let _ = self.apu.step();
        }
    }

    /// VMAIN bits 0-1 select the address increment: 1, 32, 128, 128
    /// words. Bit 7 selects which of the two data ports triggers it,
    /// which is why the callers above differ.
    fn step_vram_address(&mut self) {
        let step = match self.vmain & 0x03 {
            0 => 1u16,
            1 => 32,
            _ => 128,
        };
        self.vram_address = self.vram_address.wrapping_add(step);
    }

    /// Read the low bytes of `len` VRAM words starting at `word_addr`.
    ///
    /// Tile-map entries keep the character in the low byte, so this is
    /// what a ROM that "wrote text" actually wrote.
    #[must_use]
    pub fn vram_low_bytes(&self, word_addr: u16, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| self.ppu.vram[((word_addr as usize + i) * 2) % self.ppu.vram.len()])
            .collect()
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
