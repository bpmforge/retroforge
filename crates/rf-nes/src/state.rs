//! NES machine-state serialization (ticket W2-04) — the payload side of
//! `docs/design/SAVE_STATES.md` §2's chunk table.
//!
//! ## Why this crate writes payloads, not chunks
//!
//! SAVE_STATES.md §2 says "each crate serializes/deserializes only its own
//! chunks", and lists `CPU_ PPU_ APU_ WRAM VRAM OAM_ CGRM MAPR CART` as
//! core-owned. But `rf-state` — which owns the TLV envelope, the tag
//! registry and the zstd body — is in `scripts/validate-arch.sh`'s
//! `FORBIDDEN` list for core crates, so `rf-nes` **cannot** name a chunk tag
//! without breaking the layer rule (ARCHITECTURE §6, inviolable).
//!
//! The split that satisfies both: this module owns each region's **payload
//! encoding**, one region per chunk, exposed through [`StateRegion`]; the
//! app shell (`retroforge`, which may depend on both crates) pairs each
//! region with its tag and hands the bytes to `rf_state::Container`. Nothing
//! about the encoding lives outside this crate, and nothing about the
//! container lives inside it.
//!
//! ## Encoding
//!
//! Hand-rolled, little-endian, fixed field order — no serde, no bincode.
//! That is exactly what [`StateWriter`]'s own doc asks for ("a core just
//! writes its raw state bytes out in a fixed order and reads them back the
//! same way"), and it keeps `rf-nes`'s dependency list unchanged.
//!
//! **Completeness is compiler-enforced, not reviewed.** Every `save_*`
//! function destructures its type with an exhaustive pattern and no `..`
//! rest, so adding a field to `Ppu`, `Apu`, `Cpu`, `NesBus` or any of their
//! sub-structs **fails the build** until it is either written out or
//! explicitly bound to `_` with a reason. A save state that silently drops a
//! new field is the single most likely way this ticket's roundtrip
//! invariant could rot, and a review checklist would not catch it.
//!
//! ## Frame-boundary rule
//!
//! SAVE_STATES.md §2: states are "taken at frame boundaries only, which
//! keeps chunks simple and deterministic". Two PPU queues are therefore
//! **output, not state**, and are not serialized: the completed-scanline
//! queue (drained by the caller every frame — the same argument
//! `retroforge::stepper`'s `state_hash` doc already makes for the rendered
//! framebuffer) and the event queue. Saving mid-frame with either non-empty
//! is refused with a diagnostic rather than silently losing them.

use rf_core_api::{StateError, StateReader, StateWriter};

/// One save-state region. Each maps 1:1 to a chunk tag in
/// `docs/design/SAVE_STATES.md` §2's table; the tag names live in
/// `rf-state`'s registry, not here (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateRegion {
    /// `CPU_` — the 6502 register file and interrupt latches, plus the
    /// bus-level counters that advance in lock-step with it
    /// (`master_cycle`, the open-bus latch, controller shift state).
    Cpu,
    /// `PPU_` — every PPU field except the memories below.
    Ppu,
    /// `APU_` — all five channels plus the frame counter.
    Apu,
    /// `WRAM` — the 2 KiB of console work RAM.
    Wram,
    /// `VRAM` — the 4 KiB nametable RAM, plus CHR **RAM** when the
    /// cartridge has it (CHR ROM is cartridge data, not state).
    Vram,
    /// `OAM_` — the 256-byte sprite table.
    Oam,
    /// `CGRM` — the NES's 32-byte palette RAM (the SNES's CGRAM slot).
    Cgram,
    /// `MAPR` — mapper bank registers, IRQ counters and latches.
    Mapper,
    /// `CART` — battery-backed PRG-RAM (`$6000-$7FFF`), the region
    /// FR-CORE-012 persists to disk.
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
///
/// Public because [`crate::Mapper`] is a public trait and its state hooks
/// take one — a mapper implemented outside this crate must be able to
/// serialize itself the same way the built-in ones do.
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

    pub fn bool(&mut self, v: bool) -> Result<(), StateError> {
        self.u8(u8::from(v))
    }

    pub fn u16(&mut self, v: u16) -> Result<(), StateError> {
        self.inner.write_all(&v.to_le_bytes())
    }

    pub fn u32(&mut self, v: u32) -> Result<(), StateError> {
        self.inner.write_all(&v.to_le_bytes())
    }

    pub fn u64(&mut self, v: u64) -> Result<(), StateError> {
        self.inner.write_all(&v.to_le_bytes())
    }

    /// `Option<u32>` as a presence byte then the value (0 when absent, so
    /// the encoding is fixed-width and diffable).
    pub fn opt_u32(&mut self, v: Option<u32>) -> Result<(), StateError> {
        self.bool(v.is_some())?;
        self.u32(v.unwrap_or(0))
    }

    pub fn opt_u64(&mut self, v: Option<u64>) -> Result<(), StateError> {
        self.bool(v.is_some())?;
        self.u64(v.unwrap_or(0))
    }

    pub fn bytes(&mut self, v: &[u8]) -> Result<(), StateError> {
        self.inner.write_all(v)
    }

    /// Length-prefixed byte block, for the one variable-length region
    /// (CHR RAM).
    pub fn block(&mut self, v: &[u8]) -> Result<(), StateError> {
        let len = u32::try_from(v.len()).map_err(|_| {
            StateError::Corrupt("state block longer than u32::MAX cannot be encoded".to_string())
        })?;
        self.u32(len)?;
        self.bytes(v)
    }
}

/// Little-endian byte source over a [`StateReader`]. Public for the same
/// reason as [`StateOut`].
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

    pub fn bool(&mut self) -> Result<bool, StateError> {
        Ok(self.u8()? != 0)
    }

    pub fn u16(&mut self) -> Result<u16, StateError> {
        let mut b = [0u8; 2];
        self.inner.read_exact(&mut b)?;
        Ok(u16::from_le_bytes(b))
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

    pub fn opt_u32(&mut self) -> Result<Option<u32>, StateError> {
        let present = self.bool()?;
        let value = self.u32()?;
        Ok(present.then_some(value))
    }

    pub fn opt_u64(&mut self) -> Result<Option<u64>, StateError> {
        let present = self.bool()?;
        let value = self.u64()?;
        Ok(present.then_some(value))
    }

    pub fn bytes(&mut self, out: &mut [u8]) -> Result<(), StateError> {
        self.inner.read_exact(out)
    }

    /// Reads a [`StateOut::block`]. `max` bounds the allocation against a
    /// corrupt or hostile length field — the same rule `rf-state`'s
    /// container applies to chunk lengths (SAVE_STATES.md §2,
    /// "Robustness (untrusted input)").
    pub fn block(&mut self, max: usize) -> Result<Vec<u8>, StateError> {
        let len = self.u32()? as usize;
        if len > max {
            return Err(StateError::Corrupt(format!(
                "state block length {len} exceeds this region's maximum {max}"
            )));
        }
        let mut out = vec![0u8; len];
        self.bytes(&mut out)?;
        Ok(out)
    }
}
