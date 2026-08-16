//! Bus-level serialization and the per-region entry points every save-state
//! caller goes through (ticket W2-04). See `crate::state`'s module doc for
//! why regions, not chunks, are this crate's unit.

use rf_core_api::{StateError, StateReader, StateWriter};

use crate::cpu::Cpu;
use crate::state::{StateIn, StateOut, StateRegion};
use crate::system::{Controller, NesBus};

impl NesBus {
    /// Write one region's payload. `cpu` is passed in because the machine
    /// is `(Cpu, NesBus)` — the CPU register file and the bus counters that
    /// advance in lock-step with it share the `CPU_` chunk, and splitting
    /// them across two calls would let a caller save half a machine.
    ///
    /// # Errors
    /// Returns [`StateError`] if `w` rejects a write, or (for
    /// [`StateRegion::Ppu`]) if the PPU still holds undrained output — see
    /// `crate::ppu::Ppu::save_state`'s frame-boundary rule.
    pub fn save_region(
        &self,
        cpu: &Cpu,
        region: StateRegion,
        w: &mut dyn StateWriter,
    ) -> Result<(), StateError> {
        let mut out = StateOut::new(w);
        match region {
            StateRegion::Cpu => {
                cpu.save_state(&mut out)?;
                self.save_bus(&mut out)
            }
            StateRegion::Ppu => self.ppu.save_state(&mut out),
            StateRegion::Apu => self.apu.save_state(&mut out),
            StateRegion::Wram => out.bytes(&self.ram),
            StateRegion::Vram => self.ppu.save_vram(&mut out),
            StateRegion::Oam => self.ppu.save_oam(&mut out),
            StateRegion::Cgram => self.ppu.save_palette(&mut out),
            StateRegion::Mapper => self.mapper.save_state(&mut out),
            StateRegion::Cart => out.bytes(&self.prg_ram),
        }
    }

    /// Restore one region's payload, the mirror of [`NesBus::save_region`].
    ///
    /// # Errors
    /// Returns [`StateError`] if the stream is exhausted early or carries a
    /// value this machine cannot accept (a CHR-RAM size that disagrees with
    /// the loaded cartridge, an unknown enum discriminant, ...).
    pub fn load_region(
        &mut self,
        cpu: &mut Cpu,
        region: StateRegion,
        r: &mut dyn StateReader,
    ) -> Result<(), StateError> {
        let mut inp = StateIn::new(r);
        match region {
            StateRegion::Cpu => {
                cpu.load_state(&mut inp)?;
                self.load_bus(&mut inp)
            }
            StateRegion::Ppu => self.ppu.load_state(&mut inp),
            StateRegion::Apu => self.apu.load_state(&mut inp),
            StateRegion::Wram => inp.bytes(&mut self.ram),
            StateRegion::Vram => self.ppu.load_vram(&mut inp),
            StateRegion::Oam => self.ppu.load_oam(&mut inp),
            StateRegion::Cgram => self.ppu.load_palette(&mut inp),
            StateRegion::Mapper => self.mapper.load_state(&mut inp),
            StateRegion::Cart => inp.bytes(&mut self.prg_ram),
        }
    }

    /// Battery-backed PRG-RAM (`$6000-$7FFF`) — the bytes FR-CORE-012
    /// persists to disk alongside the ROM, independent of any save state.
    #[must_use]
    pub fn battery_ram(&self) -> &[u8] {
        &self.prg_ram
    }

    /// Replace battery-backed PRG-RAM, e.g. from a `.sav` file loaded at
    /// startup.
    ///
    /// # Errors
    /// Returns [`StateError::Corrupt`] if `bytes` is not exactly this
    /// machine's PRG-RAM size — a short or long `.sav` is a real problem
    /// (wrong game, truncated write) and is refused rather than padded.
    pub fn set_battery_ram(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        if bytes.len() != self.prg_ram.len() {
            return Err(StateError::Corrupt(format!(
                "battery RAM is {} bytes, this machine has {}",
                bytes.len(),
                self.prg_ram.len()
            )));
        }
        self.prg_ram.copy_from_slice(bytes);
        Ok(())
    }

    /// The non-CPU half of the `CPU_` chunk: the bus counters and latches
    /// that advance in lock-step with the processor.
    fn save_bus(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        // Exhaustive, no `..` — see `crate::state`'s module doc.
        let NesBus {
            master_cycle,
            ram: _, // WRAM chunk
            open_bus,
            ppu: _, // PPU_/VRAM/OAM_/CGRM chunks
            controllers,
            prg_ram: _, // CART chunk
            // Cartridge data, not state: the ROM is reloaded before a state
            // is applied, and a state that could replace it would be a way
            // to smuggle a different game into a session.
            rom: _,
            mapper: _, // MAPR chunk
            last_oam_dma_stall,
            nmi_level_latch,
            apu: _, // APU_ chunk
        } = self;

        out.u64(*master_cycle)?;
        out.u8(*open_bus)?;
        for controller in controllers {
            controller.save_state(out)?;
        }
        out.opt_u32(*last_oam_dma_stall)?;
        out.bool(*nmi_level_latch)
    }

    fn load_bus(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.master_cycle = inp.u64()?;
        self.open_bus = inp.u8()?;
        for index in 0..self.controllers.len() {
            self.controllers[index].load_state(inp)?;
        }
        self.last_oam_dma_stall = inp.opt_u32()?;
        self.nmi_level_latch = inp.bool()?;
        Ok(())
    }
}

impl Controller {
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        let Controller {
            buttons,
            strobe,
            latched,
            shift_index,
        } = self;
        out.u8(*buttons)?;
        out.bool(*strobe)?;
        out.u8(*latched)?;
        out.u8(*shift_index)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.buttons = inp.u8()?;
        self.strobe = inp.bool()?;
        self.latched = inp.u8()?;
        self.shift_index = inp.u8()?;
        Ok(())
    }
}

impl NesBus {
    /// The nametable decoded as ASCII (ticket W2-05): blargg's older shells
    /// write character codes straight into VRAM, so for those ROMs this is
    /// the only place their result exists. Whitespace-normalized.
    #[must_use]
    pub fn ppu_vram_ascii(&self) -> String {
        let vram = &self.ppu.vram;
        let text: String = (0..30 * 32)
            .map(|i| {
                let tile = vram[i];
                if (0x20..0x7F).contains(&tile) {
                    tile as char
                } else {
                    ' '
                }
            })
            .collect();
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}
