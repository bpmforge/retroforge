//! Test harness: test-ROM fetcher, blargg `$6000`/`$6004` protocol runner,
//! golden-frame compare, the accuracy-table/waiver enforcement layer
//! (ticket W0-03), and the local-evidence gate (ticket W0-07; see
//! `docs/ARCHITECTURE.md` and `docs/TESTING.md`).
//!
//! W0-03 built this crate's infrastructure before any real emulator core
//! existed, so the blargg runner and golden-frame helpers are exercised
//! in this crate's own tests against `rf-core-api`'s traits with a mock
//! core/synthetic memory (see `crates/rf-harness/tests/blargg_protocol.rs`
//! and `crates/rf-core-api/tests/mock_core.rs` for that pattern) rather
//! than a real ROM. `rf-nes`'s 6502 CPU landed at W1-01a/b; this crate now
//! depends on it directly for [`nes6502_evidence::run_all`] (the local
//! evidence generator, ticket W0-07) and, since W1-03,
//! [`nestest_evidence::run`] (the second Tier-A-local suite's local
//! evidence generator, the nestest golden-trace diff) -
//! `scripts/validate-arch.sh`'s layer rule exempts "the test harness"
//! from the "cores only via rf-core-api" restriction by name, alongside
//! the app shell and the cores themselves.
//!
//! Do not add public API here without a ticket in plan.json.

mod accuracy;
mod blargg;
mod fetch;
mod golden_frame;
mod json;
mod manifest;
pub mod nes6502_evidence;
pub mod nestest_evidence;
mod vector_json;

pub use accuracy::{
    build_report, AccuracyRow, Counts, Report, ReportError, ReportRow, RowStatus, Waiver,
    WaiverFile,
};
pub use blargg::{
    run_blargg_protocol, BlarggOutcome, BlarggStatus, RunnerError, VALIDITY_SIGNATURE,
};
pub use fetch::{
    fetch_artifact, fetch_git_artifact, git_rev_parse_head, git_tree_is_clean, hex_sha256,
    AttemptFailure, CurlDownloader, Downloader, FetchAttempt, FetchError, GitFetchError, GitRunner,
    SystemGitRunner,
};
pub use golden_frame::{
    find_first_divergence, hash_frame_palette_indices, DivergenceReport, FrameCapture,
};
pub use json::Json;
pub use manifest::{
    Artifact, Console, GitArtifact, LicenseStatus, Manifest, Protocol, Suite, SuiteRom, Tier,
    ValidationError,
};

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-harness";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-harness");
    }
}
