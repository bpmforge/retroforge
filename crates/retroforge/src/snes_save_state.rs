//! SNES `.rfstate` containers (ticket W7-09; FR-STATE-001/002/003).
//!
//! The sibling of [`crate::save_state`], and deliberately the same shape:
//! `rf-snes` writes raw region bytes through `rf-core-api`'s
//! `StateWriter`, `rf-state` owns framing and versioning, and **this
//! module owns the region-to-tag table** — the one piece of knowledge
//! neither crate is allowed to have. `scripts/validate-arch.sh` forbids a
//! core from importing `rf-state`, and `rf-state` depends on nothing but
//! bincode, sha2 and zstd so that it knows no core.
//!
//! ## The console byte is what tells the two payload families apart
//!
//! A SNES `PPU_` chunk and a NES `PPU_` chunk share a tag and share the
//! registry's version number, but their payloads have nothing in common.
//! The header's `console` byte is the discriminator, and
//! [`Container::verify_rom`] plus the normalized ROM hash make a
//! cross-console mix-up unreachable in practice — a state's hash cannot
//! match a cartridge from the other machine.
//!
//! **A wrinkle worth knowing before changing a payload**: `TagInfo`
//! carries ONE `current_version` per tag, not one per (console, tag). A
//! future change to the SNES `PPU_` payload therefore has to bump a number
//! the NES `PPU_` payload also reads, forcing NES states to migrate for a
//! change that did not touch them. Recorded rather than worked around,
//! because the fix is a format decision (a per-console version, or
//! console-specific tags) and not this ticket's to take.

use rf_core_api::StateError;
use rf_snes::state::StateRegion;
use rf_snes::system::SnesSystem;
use rf_state::{Chunk, Container, LoadWarning};

use crate::save_state::SaveStateError;

/// Console byte for SNES states (`header.console`; NES is 0).
pub const CONSOLE_SNES: u8 = 1;

/// Region-to-tag mapping. The tags are `rf-state`'s registry values; the
/// regions are `rf-snes`'s. This table is the whole of what this module
/// knows that neither crate can.
///
/// Nine entries, matching `SAVE_STATES.md` §2's nine core tags exactly.
/// The SNES's two extra memories have no tag of their own and ride inside
/// the chunk that owns them — ARAM in `APU_`, the DMA channels in `CPU_`
/// — see [`StateRegion`]'s doc for why those are the right homes rather
/// than merely the available ones.
const REGION_TAGS: [(StateRegion, [u8; 4]); 9] = [
    (StateRegion::Cpu, *b"CPU_"),
    (StateRegion::Ppu, *b"PPU_"),
    (StateRegion::Apu, *b"APU_"),
    (StateRegion::Wram, *b"WRAM"),
    (StateRegion::Vram, *b"VRAM"),
    (StateRegion::Oam, *b"OAM_"),
    (StateRegion::Cgram, *b"CGRM"),
    (StateRegion::Mapper, *b"MAPR"),
    (StateRegion::Cart, *b"CART"),
];

fn chunk_version(tag: [u8; 4]) -> u16 {
    rf_state::tag_info(tag)
        .unwrap_or_else(|| {
            panic!(
                "chunk tag {} is not in rf-state's registry — REGION_TAGS and TAG_REGISTRY \
                 have drifted",
                String::from_utf8_lossy(&tag)
            )
        })
        .current_version
}

#[derive(Default)]
struct PayloadBuf {
    bytes: Vec<u8>,
}

impl rf_core_api::StateWriter for PayloadBuf {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), StateError> {
        self.bytes.extend_from_slice(buf);
        Ok(())
    }
}

struct PayloadCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl rf_core_api::StateReader for PayloadCursor<'_> {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), StateError> {
        let end = self.pos + buf.len();
        if end > self.bytes.len() {
            return Err(StateError::Io(format!(
                "chunk payload exhausted: wanted {} more bytes, {} remain",
                buf.len(),
                self.bytes.len() - self.pos
            )));
        }
        buf.copy_from_slice(&self.bytes[self.pos..end]);
        self.pos = end;
        Ok(())
    }
}

/// Serialise a SNES machine into a container.
///
/// # Errors
/// Returns [`SaveStateError::Core`] if a region refuses to serialise, or
/// [`SaveStateError::Container`] if a chunk cannot be added.
pub fn build_container(
    system: &SnesSystem,
    rom_sha256: [u8; 32],
    emu_version: &str,
    timestamp: u64,
) -> Result<Container, SaveStateError> {
    let mut container = Container::new(CONSOLE_SNES, rom_sha256, emu_version, timestamp);
    for (region, tag) in REGION_TAGS {
        let mut payload = PayloadBuf::default();
        system.save_region(region, &mut payload)?;
        container.add_chunk(tag, chunk_version(tag), payload.bytes)?;
    }
    Ok(container)
}

/// Apply a container to a SNES machine.
///
/// **Every chunk must be consumed whole.** A payload with bytes left over
/// means the writer and reader disagree about the region's shape, and the
/// half that was read is a plausible-looking wrong machine — so a
/// trailing-bytes check is worth more here than almost anywhere else.
///
/// # Errors
/// [`SaveStateError::MissingChunk`] if a required chunk is absent,
/// [`SaveStateError::Core`] if a payload is malformed or the wrong length.
pub fn apply_container(
    system: &mut SnesSystem,
    container: &Container,
) -> Result<Vec<LoadWarning>, SaveStateError> {
    for (region, tag) in REGION_TAGS {
        let chunk: &Chunk = container
            .chunk(tag)
            .ok_or(SaveStateError::MissingChunk(tag))?;
        let mut cursor = PayloadCursor {
            bytes: &chunk.payload,
            pos: 0,
        };
        system.load_region(region, &mut cursor)?;
        if cursor.pos != chunk.payload.len() {
            return Err(SaveStateError::Core(StateError::Corrupt(format!(
                "chunk {} has {} bytes but the {region:?} region read {}",
                String::from_utf8_lossy(&tag),
                chunk.payload.len(),
                cursor.pos
            ))));
        }
    }
    Ok(Vec::new())
}

/// Save, refusing nothing — the machine is always serialisable.
///
/// # Errors
/// See [`build_container`].
pub fn save_state(
    system: &SnesSystem,
    rom_sha256: [u8; 32],
    timestamp: u64,
) -> Result<Container, SaveStateError> {
    build_container(system, rom_sha256, env!("CARGO_PKG_VERSION"), timestamp)
}

/// Load, **refusing a state saved against a different ROM**
/// (FR-STATE-003).
///
/// # Errors
/// [`SaveStateError::Container`] carrying rf-state's ROM
/// mismatch, before any part of the machine is touched.
pub fn load_state(
    system: &mut SnesSystem,
    rom_sha256: [u8; 32],
    container: &Container,
) -> Result<Vec<LoadWarning>, SaveStateError> {
    // Checked FIRST, so a refused load leaves the running machine exactly
    // as it was rather than half-overwritten by the wrong game's state.
    container.verify_rom(rom_sha256)?;
    apply_container(system, container)
}
