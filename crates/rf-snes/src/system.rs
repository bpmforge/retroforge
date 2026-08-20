//! The SNES machine: CPU plus bus (ticket W6-02a; FR-CORE-035).
//!
//! ## Loading a cartridge, and refusing one
//!
//! [`SnesSystem::load`] goes through `rf-cart`, which already detects
//! enhancement chips and unsupported map modes and reports them as
//! [`rf_cart::CartError::UnsupportedChip`] — the FR-CORE-013 diagnostic.
//! This crate does not re-implement that detection; it propagates it. The
//! requirement is "fail with a diagnostic naming the chip, **never a
//! crash**", so the failure has to arrive as a `Result` from the entry
//! point a front-end actually calls, which is here rather than three
//! layers down.

use rf_cart::{CartError, Cartridge, SnesMapMode};

use crate::bus::SnesBus;
use crate::cpu::{access_cycles, Cpu, CpuBus};

/// A whole SNES.
pub struct SnesSystem {
    pub cpu: Cpu,
    pub bus: SnesBus,
    /// Master cycles elapsed since reset.
    pub master_cycles: u64,
    /// An NMI edge seen but not yet dispatched.
    pending_nmi: bool,
}

impl SnesSystem {
    /// Load a cartridge image.
    ///
    /// # Errors
    /// Returns the `rf-cart` diagnostic for an unparseable header, an
    /// unsupported map mode, or an enhancement chip this core does not
    /// implement (FR-CORE-013 / FR-CORE-035).
    pub fn load(raw: &[u8]) -> Result<Self, CartError> {
        let cart = Cartridge::load(raw)?;
        let Cartridge::Snes { header, .. } = cart else {
            return Err(CartError::InvalidHeader(
                "not a SNES cartridge (an iNES header was found)".to_string(),
            ));
        };

        // A copier header shifts every offset by 512 bytes. rf-cart
        // reports whether it stripped one; the ROM image the bus maps
        // must be stripped to match, or every address is 512 bytes wrong
        // — which would look like a mapping bug rather than a header one.
        let rom = if header.had_copier_header {
            raw[512..].to_vec()
        } else {
            raw.to_vec()
        };

        let mut system = Self {
            cpu: Cpu::new(),
            bus: SnesBus::new(rom, header.ram_size, header.map_mode),
            master_cycles: 0,
            pending_nmi: false,
        };
        system.bus.fast_rom = header.fast_rom;
        system.reset();
        Ok(system)
    }

    /// Build a system directly from a ROM image and map mode, bypassing
    /// header detection.
    ///
    /// For tests that want a specific mapping regardless of what a header
    /// claims. Not the path a front-end should use — [`Self::load`] is,
    /// because it is the one that produces the FR-CORE-013 diagnostic.
    #[must_use]
    pub fn from_rom(rom: Vec<u8>, mode: SnesMapMode, sram_len: usize) -> Self {
        let mut system = Self {
            cpu: Cpu::new(),
            bus: SnesBus::new(rom, sram_len, mode),
            master_cycles: 0,
            pending_nmi: false,
        };
        system.reset();
        system
    }

    /// Reset: emulation mode, and PC from the vector at `$00:FFFC`.
    ///
    /// The vector is fetched THROUGH THE MAPPING, not read from a file
    /// offset. For a 32 KiB LoROM that only resolves because undersized
    /// ROMs mirror — which makes this the first thing that breaks if the
    /// mirroring is wrong, and a useful canary.
    pub fn reset(&mut self) {
        self.cpu = Cpu::new();
        let lo = self.bus.read(0x00_FFFC);
        let hi = self.bus.read(0x00_FFFD);
        self.cpu.pc = u16::from(lo) | (u16::from(hi) << 8);
        self.cpu.pbr = 0;
        self.master_cycles = 0;
        self.pending_nmi = false;
        self.bus.timing = crate::timing::Timing::new();
    }

    /// Execute one instruction, charging its bus accesses in master
    /// cycles and servicing any DMA it armed.
    ///
    /// # Errors
    /// Returns the opcode if the CPU does not implement it. As of W6-01b
    /// all 256 are implemented, so this cannot currently happen — see the
    /// note on `ops::execute`'s catch-all arm.
    pub fn step(&mut self) -> Result<(), u8> {
        let fast_rom = self.bus.fast_rom;
        let mut counting = crate::cpu::AccessCost::new(&mut self.bus, fast_rom);
        let result = self.cpu.step(&mut counting);
        let spent = counting.master_cycles;
        let accesses = counting.accesses;
        self.master_cycles += spent;

        // The math unit advances in CPU cycles, not master cycles. One
        // bus access is one CPU cycle, which is what the vector traces
        // show; internal cycles are not modelled yet (W6-02a's note on
        // the cycle-accurate executor).
        self.bus.tick_math(accesses as u32);
        let dma_cycles = self.bus.service_dma();
        self.master_cycles += dma_cycles;

        // Advance the frame clock by everything this instruction spent,
        // DMA included — DMA halts the CPU but the raster keeps going,
        // and a model that froze time during a transfer would let a game
        // DMA through vblank without ever leaving it.
        let irq_mode = self.bus.nmitimen.irq_mode();
        let auto_joypad = self.bus.nmitimen.auto_joypad();
        let timer = self.bus.irq;
        let events = self
            .bus
            .timing
            .advance(spent + dma_cycles, auto_joypad, |dot, line| {
                timer.matches(irq_mode, dot, line)
            });

        if events.irq {
            self.bus.irq.fired = true;
        }
        if events.auto_joypad_done {
            self.bus.joypads.latch();
        }

        // Delivery. NMI is edge-triggered on the vblank transition and
        // ignores the I flag; IRQ is level-ish and masked by it.
        if events.vblank_started && self.bus.nmitimen.nmi_enabled() {
            self.pending_nmi = true;
        }
        if self.pending_nmi {
            self.pending_nmi = false;
            self.cpu.interrupt(&mut self.bus, true);
        } else if self.bus.irq.fired && !self.cpu.flag(crate::cpu::flags::I) {
            self.cpu.interrupt(&mut self.bus, false);
        }

        result
    }

    /// Run up to `max_instructions`, stopping early if the CPU halts
    /// (`WAI`/`STP`) or reaches `stop_pc` in bank 0.
    ///
    /// Returns how many instructions actually ran. A bounded runner
    /// rather than a loop: a fixture that never reaches its end must fail
    /// the test, not hang it.
    ///
    /// # Errors
    /// Propagates an unimplemented opcode.
    pub fn run_until(&mut self, max_instructions: u64, stop_pc: Option<u16>) -> Result<u64, u8> {
        for n in 0..max_instructions {
            if self.cpu.stopped {
                return Ok(n);
            }
            if stop_pc == Some(self.cpu.pc) && self.cpu.pbr == 0 {
                return Ok(n);
            }
            self.step()?;
        }
        Ok(max_instructions)
    }

    /// Run until the start of the next vblank, then compose the visible
    /// frame.
    ///
    /// Rendering at vblank rather than mid-frame is what makes the result
    /// stable: a ROM sets registers during vblank and expects them to
    /// hold for the frame, so composing at any other moment can catch a
    /// half-updated tilemap.
    ///
    /// # Errors
    /// Propagates an unimplemented opcode.
    pub fn render_frame(&mut self, max_instructions: u64) -> Result<Vec<Vec<u8>>, u8> {
        // Get into vblank...
        let start = self.bus.timing.frame;
        let mut n = 0;
        while !self.bus.timing.in_vblank() && n < max_instructions {
            self.step()?;
            n += 1;
        }
        // ...and out again, so composition happens on a settled frame.
        while (self.bus.timing.in_vblank() || self.bus.timing.frame == start)
            && n < max_instructions
        {
            self.step()?;
            n += 1;
        }

        let mut frame = Vec::with_capacity(usize::from(crate::ppu::VISIBLE_LINES));
        for y in 0..crate::ppu::VISIBLE_LINES {
            frame.push(
                self.bus
                    .ppu
                    .render_scanline(y)
                    .pixels
                    .iter()
                    .map(|p| p.palette_index)
                    .collect(),
            );
        }
        Ok(frame)
    }

    /// CGRAM as RGB888, for tests and tools that need to look at a frame.
    ///
    /// CGRAM is BGR555; the 5-bit channels are scaled to 8 bits by
    /// replicating the high bits (`v << 3 | v >> 2`) rather than shifting
    /// alone, so full-scale input maps to full-scale output instead of
    /// topping out at 248.
    #[must_use]
    pub fn palette_rgb(&self) -> Vec<[u8; 3]> {
        self.bus
            .ppu
            .cgram
            .iter()
            .map(|&c| {
                let ch = |v: u16| ((v & 0x1F) as u8) << 3 | ((v & 0x1F) as u8) >> 2;
                [ch(c), ch(c >> 5), ch(c >> 10)]
            })
            .collect()
    }

    /// Master cycles one access at `addr` would cost right now.
    #[must_use]
    pub fn access_cost(&self, addr: u32) -> u8 {
        access_cycles(addr, self.bus.fast_rom)
    }
}
