//! Game profile system: hash-keyed, versioned, data-driven decode rules
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.
//!
//! Scope (ticket W4-02): schema v0 (`schema`), loader + unknown-key
//! warnings + newer-major rejection (`loader`, `shape`), and hash matching
//! against `rf-cart`'s already-computed hash bundle (`schema::Profile::
//! matches_rom`). No decoder ships here — `decode.kind = "metatile_screens"`
//! is data this crate can parse and express, not run; that is W5-02a,
//! gated on W5-01.

pub mod error;
pub mod loader;
pub mod schema;
mod shape;

pub use error::ProfileError;
pub use loader::{load_file, load_str, LoadOutcome};
pub use schema::{
    AntiFlicker, Atmosphere, AtmosphereLadder, AxisSpec, Camera, Capabilities, CollisionSpec,
    Console, Decode, Entities, EntityTable, FieldSpec, HudSpec, IdentityEntry, Loading,
    MemoryMapEntry, Meta, MetatileSpec, ModPatch, Mods, PageSpec, PalettesSpec, Plugins, Profile,
    Requires, RomMapEntry, ScreensSpec, UntilSpec, WaitLoop, WidescreenMode,
    SUPPORTED_PROFILE_MAJOR,
};

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-profiles";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-profiles");
    }
}
