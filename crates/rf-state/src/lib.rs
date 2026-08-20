//! Versioned save states, deterministic replay recording/playback
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.
//!
//! Scope (ticket W0-05): the `.rfstate` container — TLV envelope, chunk
//! tag registry, bincode-2 payload helpers, and load-time rules
//! (FR-STATE-001, FR-STATE-004, NFR-008). See `docs/design/SAVE_STATES.md`
//! §2 for the normative format and `docs/design/CONTRACTS.md` §3 for the
//! public-surface commitment (this format is versioned from first release
//! and is not to be changed casually).
//!
//! This crate does not depend on `rf-core-api`: chunk payloads are opaque
//! `Vec<u8>` in and out, and `StateWriter`/`StateReader`
//! (`rf-core-api::state_view`) are byte-stream adapters meant for a core
//! that writes/reads its own fixed-order register bytes — no core exists
//! yet to consume them. Wiring rf-state to those traits belongs in a
//! later ticket landing alongside the first core crate.

mod chunk;
mod container;
mod cursor;
mod error;
mod header;
mod migrate;
mod payload;
mod rewind;
mod tags;

pub use chunk::Chunk;
pub use container::Container;
pub use error::{ContainerError, LoadWarning};
pub use header::Header;
pub use migrate::{MigrateFn, MigrationRegistry};
pub use payload::{decode_payload, encode_payload, ReplayCursor};
pub use rewind::{RewindConfig, RewindRing};
pub use tags::{is_required, tag_info, TagInfo, TAG_REGISTRY};

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-state";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-state");
    }
}
