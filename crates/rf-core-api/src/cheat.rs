//! Cheat primitives that cross the core boundary (ticket W41-01;
//! `docs/design/CHEATS_ACHIEVEMENTS.md` section 2).
//!
//! Two plain data types, no logic. Parsing of user-facing code formats lives
//! in `rf-cheats`; applying them lives in the cores (ROM overrides) and the
//! app's mod-tier write door (RAM writes).

/// Most ROM overrides a core accepts at once (ticket W41-02). A sanity bound,
/// not hardware: the NES Game Genie took 3 codes, the SNES one 5.
pub const MAX_ROM_OVERRIDES: usize = 16;

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

/// The byte a CPU read of cartridge ROM at `addr` returns when the cartridge
/// itself supplied `cartridge_byte` (ticket W41-02). The first override whose
/// `addr` matches and whose `compare` is `None` or equals `cartridge_byte`
/// wins; otherwise the cartridge byte passes through.
///
/// Shared by the cores so both consoles resolve a list identically. Callers
/// keep a fast path (`if overrides.is_empty()`) around it.
#[inline]
#[must_use]
pub fn resolve_rom_override(overrides: &[RomOverride], addr: u32, cartridge_byte: u8) -> u8 {
    overrides
        .iter()
        .find(|o| o.addr == addr && o.compare.is_none_or(|c| c == cartridge_byte))
        .map_or(cartridge_byte, |o| o.value)
}

/// Replace `slot` with `new` unless `new` is longer than
/// [`MAX_ROM_OVERRIDES`], in which case nothing changes and this returns
/// `false`. An empty `new` clears the slot. The cores' shared body of
/// `EmulatorCore::set_rom_overrides`.
pub fn install_rom_overrides(slot: &mut Vec<RomOverride>, new: &[RomOverride]) -> bool {
    if new.len() > MAX_ROM_OVERRIDES {
        return false;
    }
    slot.clear();
    slot.extend_from_slice(new);
    true
}
