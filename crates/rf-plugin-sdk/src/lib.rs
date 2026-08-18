//! Plugin API: observation-first, capability-gated, WASM/Lua hosts later
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-plugin-sdk";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-plugin-sdk");
    }
}

pub mod host;
pub mod manifest;
pub mod sandbox;

pub use host::{Budget, LedgerEntry, PathRefusal, ScriptHost, ScriptState, WriteLedger};
pub use manifest::{Capabilities, FilesystemCap, Manifest, ManifestError};
pub use sandbox::ScriptLog;
