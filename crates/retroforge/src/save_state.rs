//! `.rfstate` save/load and battery-SRAM persistence (ticket W2-04,
//! FR-STATE-002/003/007, FR-CORE-012).
//!
//! ## Where the seam is
//!
//! `rf-nes` owns the *payload* encoding of each machine-state region and
//! may not name a chunk tag (`rf-state` is an upper-layer crate for a core
//! — `scripts/validate-arch.sh`); `rf-state` owns the TLV envelope, the tag
//! registry, zstd and the load rules, and knows nothing about the NES. This
//! module is the only place that knows both, which is exactly what
//! `docs/design/SAVE_STATES.md` §2's chunk table describes once you account
//! for that layer rule: it pairs [`rf_nes::StateRegion`] with its tag and
//! hands the bytes over.
//!
//! ## Load rules implemented here
//!
//! - **Wrong ROM is refused** (FR-STATE-003) via
//!   [`rf_state::Container::verify_rom`] against the *normalized* ROM hash
//!   (`rf_cart::identity_nes`), the same convention the manifest and
//!   `.rfreplay` use.
//! - **Every required core chunk must be present**; a missing one is a hard
//!   error naming the chunk, per SAVE_STATES.md §2.
//! - **Optional chunks this build cannot consume are skipped with a
//!   warning**, never silently — which is what makes FR-STATE-007's
//!   "an Accuracy-mode session can load a state saved in Enhanced mode"
//!   true: the `ENHC`/`PROF`/`INPT`/`RPLY` chunks come back as warnings and
//!   the machine state loads unchanged.

use std::path::{Path, PathBuf};

use rf_nes::{Cpu, NesBus, StateRegion};
use rf_state::{Container, ContainerError, LoadWarning};

use crate::stepper::EmuStepper;

/// Console byte for the NES in the container header (SAVE_STATES.md §2's
/// `console u8`).
pub const CONSOLE_NES: u8 = 0;

/// Chunk-payload version this build writes for every core chunk. Bumping it
/// requires a migration fn or an explicit "cannot migrate" error
/// (SAVE_STATES.md §2).
pub const CHUNK_VERSION: u16 = 1;

/// The `hash_kind` value `.rfreplay` records for hashes produced by
/// [`EmuStepper::state_hash`] (SAVE_STATES.md §3).
///
/// **This changed in W2-04 and the change is deliberate and visible.**
/// Before this ticket the hash could only reach WRAM, OAM, PRG-RAM, the CPU
/// registers and two counters — PPU and APU internals were structurally
/// unreachable — and it was recorded as `reachable-v1`. Now that `rf-nes`
/// serializes the whole machine, the hash covers all of it, so the field
/// gets a NEW VALUE rather than the old one quietly meaning something
/// different: SAVE_STATES.md §3 says outright that a full-machine hash is
/// "a later ticket's upgrade, **not a silent redefinition of this field**".
pub const HASH_KIND: &str = "full-v1";

/// Region-to-tag mapping. The tags are `rf-state`'s registry values; the
/// regions are `rf-nes`'s. This table is the whole of what this module
/// knows that neither crate can.
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

/// Everything that can go wrong saving or loading a state, kept separate
/// from `rf-state`'s container errors and `rf-core-api`'s stream errors so
/// a caller can tell "the file is wrong" from "this machine refused it".
#[derive(Debug)]
pub enum SaveStateError {
    /// The container itself is malformed, or its ROM hash does not match.
    Container(ContainerError),
    /// A required core chunk is missing from an otherwise valid container.
    MissingChunk([u8; 4]),
    /// The core rejected a chunk payload (short stream, unknown
    /// discriminant, CHR-RAM size disagreement, ...).
    Core(rf_core_api::StateError),
    /// Battery-RAM file I/O failed.
    Io(String),
}

impl std::fmt::Display for SaveStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveStateError::Container(e) => write!(f, "{e}"),
            SaveStateError::MissingChunk(tag) => write!(
                f,
                "save state is missing required chunk {}",
                String::from_utf8_lossy(tag)
            ),
            SaveStateError::Core(e) => write!(f, "core refused the state: {e}"),
            SaveStateError::Io(msg) => write!(f, "battery RAM I/O failed: {msg}"),
        }
    }
}

impl std::error::Error for SaveStateError {}

impl From<ContainerError> for SaveStateError {
    fn from(e: ContainerError) -> Self {
        SaveStateError::Container(e)
    }
}

impl From<rf_core_api::StateError> for SaveStateError {
    fn from(e: rf_core_api::StateError) -> Self {
        SaveStateError::Core(e)
    }
}

/// Collects a region's payload bytes.
#[derive(Default)]
struct PayloadBuf {
    bytes: Vec<u8>,
}

impl rf_core_api::StateWriter for PayloadBuf {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), rf_core_api::StateError> {
        self.bytes.extend_from_slice(buf);
        Ok(())
    }
}

/// Replays a region's payload bytes.
struct PayloadCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl rf_core_api::StateReader for PayloadCursor<'_> {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), rf_core_api::StateError> {
        let end = self.pos + buf.len();
        if end > self.bytes.len() {
            return Err(rf_core_api::StateError::Io(format!(
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

/// Builds a full `.rfstate` container from a machine.
///
/// `timestamp` is caller-supplied and never sampled from the clock here —
/// SAVE_STATES.md §2's determinism rule ("the timestamp is a
/// caller-supplied constructor argument, never read from the clock inside
/// the writer") is what makes golden fixtures possible at all.
///
/// # Errors
/// Returns [`SaveStateError`] if the core refuses to serialize (e.g. the
/// PPU still holds undrained output — states are frame-boundary only) or a
/// payload is too large for the container's `u32` length field.
pub fn build_container(
    cpu: &Cpu,
    bus: &NesBus,
    rom_sha256: [u8; 32],
    emu_version: &str,
    timestamp: u64,
) -> Result<Container, SaveStateError> {
    let mut container = Container::new(CONSOLE_NES, rom_sha256, emu_version, timestamp);
    for (region, tag) in REGION_TAGS {
        let mut payload = PayloadBuf::default();
        bus.save_region(cpu, region, &mut payload)?;
        container.add_chunk(tag, CHUNK_VERSION, payload.bytes)?;
    }
    Ok(container)
}

/// Applies a container to a machine, returning the warnings a caller should
/// surface (skipped optional chunks, migrated versions).
///
/// The caller is expected to have verified the ROM first (see
/// [`EmuStepper::load_state`], which does).
///
/// # Errors
/// Returns [`SaveStateError::MissingChunk`] if a required core chunk is
/// absent, or [`SaveStateError::Core`] if a payload is malformed.
pub fn apply_container(
    cpu: &mut Cpu,
    bus: &mut NesBus,
    container: &Container,
) -> Result<Vec<LoadWarning>, SaveStateError> {
    let mut warnings = Vec::new();

    for (region, tag) in REGION_TAGS {
        let chunk = container
            .chunk(tag)
            .ok_or(SaveStateError::MissingChunk(tag))?;
        let mut cursor = PayloadCursor {
            bytes: &chunk.payload,
            pos: 0,
        };
        bus.load_region(cpu, region, &mut cursor)?;
    }

    // Every non-core chunk is skipped, with a warning that names it. This
    // is FR-STATE-007's mechanism: an Enhanced-mode state carries `ENHC`
    // (and possibly `PROF`), an Accuracy-mode session has nothing to do
    // with them, and the machine state above still loads exactly.
    for chunk in container.chunks() {
        if !REGION_TAGS.iter().any(|(_, tag)| *tag == chunk.tag) {
            warnings.push(LoadWarning::UnknownChunk { tag: chunk.tag });
        }
    }

    Ok(warnings)
}

/// Where a cartridge's battery-backed SRAM lives: `<dir>/<rom-sha256>.sav`,
/// keyed by the normalized ROM hash so the same cartridge dumped twice
/// shares one save (FR-CORE-012).
#[must_use]
pub fn battery_ram_path(dir: &Path, rom_sha256_hex: &str) -> PathBuf {
    dir.join(format!("{rom_sha256_hex}.sav"))
}

/// Writes battery-backed SRAM to disk.
///
/// # Errors
/// Returns [`SaveStateError::Io`] if the directory cannot be created or the
/// file cannot be written.
pub fn write_battery_ram(
    bus: &NesBus,
    dir: &Path,
    rom_sha256_hex: &str,
) -> Result<PathBuf, SaveStateError> {
    std::fs::create_dir_all(dir).map_err(|e| SaveStateError::Io(e.to_string()))?;
    let path = battery_ram_path(dir, rom_sha256_hex);
    std::fs::write(&path, bus.battery_ram()).map_err(|e| SaveStateError::Io(e.to_string()))?;
    Ok(path)
}

/// Reads battery-backed SRAM from disk into a machine. `Ok(false)` means
/// "no save file for this ROM", which is the normal first-boot case and not
/// an error.
///
/// # Errors
/// Returns [`SaveStateError::Io`] if the file exists but cannot be read,
/// or [`SaveStateError::Core`] if it is the wrong size for this machine —
/// a truncated or foreign `.sav` is refused rather than padded.
pub fn read_battery_ram(
    bus: &mut NesBus,
    dir: &Path,
    rom_sha256_hex: &str,
) -> Result<bool, SaveStateError> {
    let path = battery_ram_path(dir, rom_sha256_hex);
    if !path.is_file() {
        return Ok(false);
    }
    let bytes = std::fs::read(&path).map_err(|e| SaveStateError::Io(e.to_string()))?;
    bus.set_battery_ram(&bytes)?;
    Ok(true)
}

impl EmuStepper {
    /// Serialize this machine into a `.rfstate` container (FR-STATE-002).
    ///
    /// # Errors
    /// See [`build_container`].
    pub fn save_state(&self, timestamp: u64) -> Result<Container, SaveStateError> {
        build_container(
            self.cpu_for_state(),
            self.bus_for_state(),
            self.rom_sha256(),
            env!("CARGO_PKG_VERSION"),
            timestamp,
        )
    }

    /// Restore this machine from a container, refusing one saved against a
    /// different ROM (FR-STATE-003) and reporting skipped optional chunks
    /// (FR-STATE-007).
    ///
    /// # Errors
    /// Returns [`SaveStateError::Container`] on a ROM mismatch,
    /// [`SaveStateError::MissingChunk`] if a required chunk is absent, or
    /// [`SaveStateError::Core`] if a payload is malformed.
    pub fn load_state(
        &mut self,
        container: &Container,
    ) -> Result<Vec<LoadWarning>, SaveStateError> {
        container.verify_rom(self.rom_sha256())?;
        let (cpu, bus) = self.machine_for_state();
        apply_container(cpu, bus, container)
    }
}
