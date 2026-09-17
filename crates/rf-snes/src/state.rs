//! SNES save-state regions (ticket W7-09; FR-STATE-001/002/003).
//!
//! ## Why this crate writes bytes and not chunks
//!
//! `scripts/validate-arch.sh` forbids a core from importing `rf-state`, so
//! nothing here knows what a `.rfstate` chunk is. A core writes its raw
//! state in a fixed order through [`rf_core_api::StateWriter`] and reads it
//! back in the same order; framing, versioning and compression are
//! `rf-state`'s job. The region-to-tag table lives in the shell, which is
//! the only layer allowed to see both — `retroforge::snes_save_state`.
//!
//! This mirrors `rf-nes::state` deliberately. Two cores solving the same
//! problem two different ways would make the shell's job the union of both.
//!
//! ## Frame boundaries only, and what that buys
//!
//! FR-STATE-001 takes states at frame boundaries only. That is what lets
//! [`Ppu::line_state`] stay out of these payloads: it is per-frame scratch,
//! cleared at every frame start and refilled scanline by scanline as HDMA
//! runs, so at a frame boundary it holds last frame's leftovers and is
//! about to be discarded. Saving it would serialise 239 optional structs
//! that the next frame overwrites before reading. **This is a real
//! assumption, not a shortcut** — a mid-frame save would need it, and the
//! roundtrip test in the shell is what would catch its absence.
//!
//! ## Every field, or the hash test fails
//!
//! FR-STATE-002 wants a restored machine to be hash-identical to one that
//! never stopped. A missed field usually survives a one-frame check and
//! diverges later, so the shell's roundtrip runs N frames after the
//! restore and compares against an uninterrupted run of the same length.

use rf_core_api::{StateError, StateReader, StateWriter};

/// One save-state region. Each maps 1:1 to a chunk tag in
/// `docs/design/SAVE_STATES.md` §2's table; the tag names live in
/// `rf-state`'s registry, not here.
///
/// The SNES has two memories the NES does not — 64 KiB of ARAM and eight
/// DMA channels — and §2's table has no tag for either. They ride in the
/// chunk that owns them rather than inventing tags the format spec does
/// not define: **ARAM is part of `APU_`** (it is the APU's RAM, and the
/// SPC700 is meaningless without it) and **the DMA channels are part of
/// `CPU_`** (they are CPU-side bus hardware, and HDMA's per-line walk
/// state advances with the raster, not with the cartridge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateRegion {
    /// `CPU_` — the 65816 register file, plus the bus-side counters that
    /// advance in lock-step with it: raster timing, the maths unit, IRQ/
    /// NMI enables, the WRAM port, joypad latches, open bus, and all
    /// eight DMA/HDMA channels.
    Cpu,
    /// `PPU_` — every PPU register, including mode 7, the windows,
    /// colour math, mosaic and SETINI. Not the memories below.
    Ppu,
    /// `APU_` — the SPC700 register file, its 64 KiB of ARAM, the three
    /// timers, the port pairs, the IPL boot handshake and the whole S-DSP
    /// (eight voices with their BRR decode cursors, the echo ring and the
    /// noise LFSR).
    Apu,
    /// `WRAM` — 128 KiB of console work RAM.
    Wram,
    /// `VRAM` — 64 KiB of video RAM.
    Vram,
    /// `OAM_` — the sprite table, low and high halves.
    Oam,
    /// `CGRM` — 256 BGR555 palette entries.
    Cgram,
    /// `MAPR` — the cartridge map mode. The SNES equivalent of a mapper
    /// number: it decides how every address decodes, so a state restored
    /// against the wrong one would read the whole ROM through the wrong
    /// window.
    Mapper,
    /// `CART` — battery-backed SRAM.
    Cart,
}

impl StateRegion {
    /// Every region, in the order a full state writes them.
    pub const ALL: [StateRegion; 9] = [
        StateRegion::Cpu,
        StateRegion::Ppu,
        StateRegion::Apu,
        StateRegion::Wram,
        StateRegion::Vram,
        StateRegion::Oam,
        StateRegion::Cgram,
        StateRegion::Mapper,
        StateRegion::Cart,
    ];
}

/// Little-endian byte sink over a [`StateWriter`].
pub struct StateOut<'a> {
    inner: &'a mut dyn StateWriter,
}

impl<'a> StateOut<'a> {
    pub fn new(inner: &'a mut dyn StateWriter) -> Self {
        Self { inner }
    }
    pub fn u8(&mut self, v: u8) -> Result<(), StateError> {
        self.inner.write_all(&[v])
    }
    pub fn i8(&mut self, v: i8) -> Result<(), StateError> {
        self.u8(v as u8)
    }
    pub fn bool(&mut self, v: bool) -> Result<(), StateError> {
        self.u8(u8::from(v))
    }
    pub fn u16(&mut self, v: u16) -> Result<(), StateError> {
        self.inner.write_all(&v.to_le_bytes())
    }
    pub fn i16(&mut self, v: i16) -> Result<(), StateError> {
        self.inner.write_all(&v.to_le_bytes())
    }
    pub fn u32(&mut self, v: u32) -> Result<(), StateError> {
        self.inner.write_all(&v.to_le_bytes())
    }
    pub fn u64(&mut self, v: u64) -> Result<(), StateError> {
        self.inner.write_all(&v.to_le_bytes())
    }
    pub fn usize(&mut self, v: usize) -> Result<(), StateError> {
        self.u32(v as u32)
    }
    pub fn bytes(&mut self, v: &[u8]) -> Result<(), StateError> {
        self.inner.write_all(v)
    }
    /// A length-prefixed byte block, for the memories whose size depends
    /// on the cartridge (SRAM).
    pub fn blob(&mut self, v: &[u8]) -> Result<(), StateError> {
        self.u32(v.len() as u32)?;
        self.bytes(v)
    }
    /// `Option<u8>`, as a present-flag then the value. Used for the
    /// write-twice latches, where "no pending byte" and "a pending zero"
    /// are different machines.
    pub fn opt_u8(&mut self, v: Option<u8>) -> Result<(), StateError> {
        self.bool(v.is_some())?;
        self.u8(v.unwrap_or(0))
    }
}

/// Little-endian byte source over a [`StateReader`].
pub struct StateIn<'a> {
    inner: &'a mut dyn StateReader,
}

impl<'a> StateIn<'a> {
    pub fn new(inner: &'a mut dyn StateReader) -> Self {
        Self { inner }
    }
    pub fn u8(&mut self) -> Result<u8, StateError> {
        let mut b = [0u8; 1];
        self.inner.read_exact(&mut b)?;
        Ok(b[0])
    }
    pub fn i8(&mut self) -> Result<i8, StateError> {
        Ok(self.u8()? as i8)
    }
    pub fn bool(&mut self) -> Result<bool, StateError> {
        Ok(self.u8()? != 0)
    }
    pub fn u16(&mut self) -> Result<u16, StateError> {
        let mut b = [0u8; 2];
        self.inner.read_exact(&mut b)?;
        Ok(u16::from_le_bytes(b))
    }
    pub fn i16(&mut self) -> Result<i16, StateError> {
        Ok(self.u16()? as i16)
    }
    pub fn u32(&mut self) -> Result<u32, StateError> {
        let mut b = [0u8; 4];
        self.inner.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    pub fn u64(&mut self) -> Result<u64, StateError> {
        let mut b = [0u8; 8];
        self.inner.read_exact(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    pub fn usize(&mut self) -> Result<usize, StateError> {
        Ok(self.u32()? as usize)
    }
    pub fn fill(&mut self, buf: &mut [u8]) -> Result<(), StateError> {
        self.inner.read_exact(buf)
    }
    /// Read a length-prefixed block into an existing buffer, **refusing a
    /// size that disagrees** rather than resizing.
    ///
    /// A state whose SRAM is a different size than the cartridge now
    /// mapped is a state for a different cartridge, and silently growing
    /// the buffer would hand the game a save file it never wrote.
    pub fn blob_into(&mut self, buf: &mut [u8], what: &'static str) -> Result<(), StateError> {
        let len = self.usize()?;
        if len != buf.len() {
            return Err(StateError::Corrupt(format!(
                "{what}: state holds {len} bytes but this cartridge has {}",
                buf.len()
            )));
        }
        self.fill(buf)
    }
    pub fn opt_u8(&mut self) -> Result<Option<u8>, StateError> {
        let present = self.bool()?;
        let v = self.u8()?;
        Ok(present.then_some(v))
    }
}

impl crate::system::SnesSystem {
    /// Write one region's bytes (ticket W7-09, FR-STATE-001).
    ///
    /// # Errors
    /// Propagates the sink's [`StateError::Io`].
    pub fn save_region(
        &self,
        region: StateRegion,
        w: &mut dyn StateWriter,
    ) -> Result<(), StateError> {
        let o = &mut StateOut::new(w);
        match region {
            StateRegion::Cpu => {
                self.cpu.save(o)?;
                o.u64(self.master_cycles)?;
                o.bool(self.pending_nmi)?;
                let b = &self.bus;
                o.u16(b.vram_address)?;
                o.u8(b.vmain)?;
                b.math.save(o)?;
                o.u8(b.nmitimen.0)?;
                o.u16(b.irq.htime)?;
                o.u16(b.irq.vtime)?;
                o.bool(b.irq.fired)?;
                o.u32(b.wram_port.address)?;
                b.dma.save(o)?;
                o.bool(b.fast_rom)?;
                o.u8(b.open_bus)?;
                o.u64(b.apu_debt)?;
                o.u64(b.apu_overspent)?;
                o.u16(b.hv.h)?;
                o.u16(b.hv.v)?;
                o.bool(b.hv.latched)?;
                o.bool(b.hv.h_second)?;
                o.bool(b.hv.v_second)?;
                o.u8(b.hv.wrio)?;
                b.timing.save(o)?;
                b.joypads.save(o)?;
                o.bool(b.manual_latch)?;
                o.u16(b.manual_shift[0])?;
                o.u16(b.manual_shift[1])?;
                o.u8(b.hdmaen)
            }
            StateRegion::Ppu => self.bus.ppu.save(o),
            StateRegion::Apu => self.bus.apu.save(o),
            StateRegion::Wram => o.bytes(&self.bus.wram),
            StateRegion::Vram => o.bytes(&self.bus.ppu.vram),
            StateRegion::Oam => o.bytes(&self.bus.ppu.oam),
            StateRegion::Cgram => {
                for c in self.bus.ppu.cgram {
                    o.u16(c)?;
                }
                Ok(())
            }
            StateRegion::Mapper => o.u8(map_mode_bits(self.bus.mode)),
            StateRegion::Cart => o.blob(&self.bus.sram),
        }
    }

    /// Read one region's bytes back.
    ///
    /// # Errors
    /// [`StateError::Io`] on a short stream, or [`StateError::Corrupt`]
    /// when a payload names something this build cannot represent — an
    /// unknown map mode, a window combiner outside 0-3, an SRAM size that
    /// disagrees with the mounted cartridge.
    pub fn load_region(
        &mut self,
        region: StateRegion,
        r: &mut dyn StateReader,
    ) -> Result<(), StateError> {
        let i = &mut StateIn::new(r);
        match region {
            StateRegion::Cpu => {
                self.cpu.load(i)?;
                self.master_cycles = i.u64()?;
                self.pending_nmi = i.bool()?;
                self.bus.vram_address = i.u16()?;
                self.bus.vmain = i.u8()?;
                self.bus.math.load(i)?;
                let nmitimen = i.u8()?;
                self.bus.nmitimen = crate::regs::NmiTimen(nmitimen);
                self.bus.irq.htime = i.u16()?;
                self.bus.irq.vtime = i.u16()?;
                self.bus.irq.fired = i.bool()?;
                self.bus.wram_port.address = i.u32()?;
                self.bus.dma.load(i)?;
                self.bus.fast_rom = i.bool()?;
                self.bus.open_bus = i.u8()?;
                self.bus.apu_debt = i.u64()?;
                self.bus.apu_overspent = i.u64()?;
                self.bus.hv.h = i.u16()?;
                self.bus.hv.v = i.u16()?;
                self.bus.hv.latched = i.bool()?;
                self.bus.hv.h_second = i.bool()?;
                self.bus.hv.v_second = i.bool()?;
                self.bus.hv.wrio = i.u8()?;
                self.bus.timing.load(i)?;
                self.bus.joypads.load(i)?;
                self.bus.manual_latch = i.bool()?;
                self.bus.manual_shift[0] = i.u16()?;
                self.bus.manual_shift[1] = i.u16()?;
                self.bus.hdmaen = i.u8()?;
                Ok(())
            }
            StateRegion::Ppu => self.bus.ppu.load(i),
            StateRegion::Apu => self.bus.apu.load(i),
            StateRegion::Wram => i.fill(&mut self.bus.wram),
            StateRegion::Vram => i.fill(&mut self.bus.ppu.vram),
            StateRegion::Oam => i.fill(&mut self.bus.ppu.oam),
            StateRegion::Cgram => {
                for c in &mut self.bus.ppu.cgram {
                    *c = i.u16()?;
                }
                Ok(())
            }
            StateRegion::Mapper => {
                let bits = i.u8()?;
                self.bus.mode = map_mode_from_bits(bits)?;
                Ok(())
            }
            StateRegion::Cart => i.blob_into(&mut self.bus.sram, "CART (battery SRAM)"),
        }
    }
}

/// The map mode, as one byte.
///
/// Written explicitly rather than by casting the enum: a `as u8` would
/// silently renumber every saved state the day a variant is inserted in
/// the middle, and the failure would be a game reading its own ROM through
/// the wrong window.
fn map_mode_bits(mode: rf_cart::SnesMapMode) -> u8 {
    match mode {
        rf_cart::SnesMapMode::LoRom => 0,
        rf_cart::SnesMapMode::HiRom => 1,
    }
}

fn map_mode_from_bits(bits: u8) -> Result<rf_cart::SnesMapMode, StateError> {
    Ok(match bits {
        0 => rf_cart::SnesMapMode::LoRom,
        1 => rf_cart::SnesMapMode::HiRom,
        other => {
            return Err(StateError::Corrupt(format!(
                "map mode {other} is not one of LoROM/HiROM"
            )))
        }
    })
}
