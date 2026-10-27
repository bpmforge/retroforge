//! Cheat primitives that cross the core boundary (ticket W41-01;
//! `docs/design/CHEATS_ACHIEVEMENTS.md` section 2).
//!
//! Two plain data types, no logic. Parsing of user-facing code formats lives
//! in `rf-cheats`; applying them lives in the cores (ROM overrides) and the
//! app's mod-tier write door (RAM writes).

/// A ROM-read override: a CPU read of cartridge ROM at `addr` returns
/// `value`, but only when the cartridge's own byte equals `compare` if that is
/// `Some`.
///
/// This is what the real Game Genie does: it sits between cartridge and
/// console and answers CPU reads at one address, optionally gated on the
/// byte the cartridge would have supplied (nesdev wiki, "Game Genie":
/// <https://www.nesdev.org/wiki/Game_Genie>). Patching the ROM image instead
/// would be wrong for bank-switched games, where one CPU address maps to
/// many ROM offsets and the compare byte exists to hit only the mapped bank.
///
/// `addr` is the CPU address: 16-bit (`$8000-$FFFF`) on the NES, 24-bit
/// (bank plus offset) on the SNES.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RomOverride {
    /// CPU address the override answers.
    pub addr: u32,
    /// Byte returned to the CPU.
    pub value: u8,
    /// When `Some`, apply only if the cartridge byte equals this.
    pub compare: Option<u8>,
}

/// A RAM write applied once per frame through the mod-tier `poke` door (the
/// same one hook scripts and the memory editor use, so it is ledgered).
///
/// The write happens only when the current byte equals `compare`, if that is
/// `Some`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RamWrite {
    /// CPU address written.
    pub addr: u32,
    /// Byte written.
    pub value: u8,
    /// When `Some`, write only if the current byte equals this.
    pub compare: Option<u8>,
}
