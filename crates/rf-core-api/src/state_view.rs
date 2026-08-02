//! Read-only machine-state view and the save-state stream traits
//! (ARCHITECTURE §5; FR-CORE-001).
use crate::error::StateError;

/// Borrowed, read-only snapshot of machine state, valid only between frames
/// (ARCHITECTURE §5: "Valid only between frames").
///
/// Every field is a shared slice — there is no `&mut` anywhere in this type
/// and no interior mutability, so "read-only" is enforced by the type
/// system, not by convention. Layout/encoding of each region (register
/// order, tile format, etc.) is core-defined; consumers (debugger,
/// enhancement side) are expected to know the layout of the specific core
/// they are attached to.
#[derive(Debug, Clone, Copy)]
pub struct StateView<'a> {
    /// Serialized CPU register file (core-defined field order).
    pub cpu_regs: &'a [u8],
    /// Main work RAM.
    pub wram: &'a [u8],
    /// Video RAM (tile/tilemap data).
    pub vram: &'a [u8],
    /// Color/palette RAM (CGRAM on SNES; NES exposes its much smaller
    /// palette RAM through the same field).
    pub cgram: &'a [u8],
    /// Object Attribute Memory (sprite table).
    pub oam: &'a [u8],
    /// Serialized PPU register file (core-defined field order).
    pub ppu_regs: &'a [u8],
    /// Cartridge mapper state (bank registers, IRQ counters, etc.); empty
    /// slice for mappers with no persistent state.
    pub mapper_state: &'a [u8],
}

/// Object-safe sink for [`crate::EmulatorCore::save_state`].
///
/// Deliberately minimal: framing, chunking, versioning and compression are
/// `rf-state`'s job (see `docs/design/SAVE_STATES.md`), not this crate's —
/// a core just writes its raw state bytes out in a fixed order and reads
/// them back the same way.
pub trait StateWriter {
    /// Write `buf` to the state stream in full.
    ///
    /// # Errors
    /// Returns [`StateError::Io`] if the underlying sink cannot accept all
    /// of `buf`.
    fn write_all(&mut self, buf: &[u8]) -> Result<(), StateError>;
}

/// Object-safe source for [`crate::EmulatorCore::load_state`]. See
/// [`StateWriter`] for the framing note.
pub trait StateReader {
    /// Fill `buf` completely from the state stream.
    ///
    /// # Errors
    /// Returns [`StateError::Io`] if the stream is exhausted before `buf`
    /// is full.
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), StateError>;
}
