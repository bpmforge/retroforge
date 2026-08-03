//! Test harness: test-ROM fetcher, blargg `$6000`/`$6004` protocol runner,
//! golden-frame compare, and the accuracy-table/waiver enforcement layer
//! (ticket W0-03; see `docs/ARCHITECTURE.md` and `docs/TESTING.md`).
//!
//! This ticket builds **infrastructure only**: there is no working
//! emulator core yet (`rf-nes` is an empty skeleton; the 6502 CPU ticket
//! W1-01 hasn't started), so the blargg runner and golden-frame helpers
//! are exercised here against `rf-core-api`'s traits with a mock
//! core/synthetic memory (see `crates/rf-harness/tests/blargg_protocol.rs`
//! and `crates/rf-core-api/tests/mock_core.rs` for the pattern this
//! follows). Real pass/fail against real ROMs becomes meaningful at
//! W1-01/W1-02.
//!
//! Do not add public API here without a ticket in plan.json.

mod accuracy;
mod blargg;
mod fetch;
mod golden_frame;
mod json;
mod manifest;

pub use accuracy::{
    build_report, AccuracyRow, Counts, Report, ReportError, ReportRow, RowStatus, Waiver,
    WaiverFile,
};
pub use blargg::{
    run_blargg_protocol, BlarggOutcome, BlarggStatus, RunnerError, VALIDITY_SIGNATURE,
};
pub use fetch::{
    fetch_artifact, hex_sha256, AttemptFailure, CurlDownloader, Downloader, FetchAttempt,
    FetchError,
};
pub use golden_frame::{
    find_first_divergence, hash_frame_palette_indices, DivergenceReport, FrameCapture,
};
pub use json::Json;
pub use manifest::{
    Artifact, Console, LicenseStatus, Manifest, Protocol, Suite, SuiteRom, Tier, ValidationError,
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
