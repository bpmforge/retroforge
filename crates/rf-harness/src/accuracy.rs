//! Machine-readable accuracy table + waiver enforcement (ticket W0-03;
//! TESTING.md §5, R-C1/R-D4).
//!
//! TESTING.md §5, verbatim: "Raw and effective counts are reported
//! separately: known-fails live in an explicit waiver file carrying
//! justification + expiry date; an expired waiver reopens red; a red row
//! with no open ticket fails the report step (suite→FR→ticket mapping is a
//! lookup from the tables above, never a judgment call)."
//!
//! Reading that as an enforceable spec:
//!  - **raw** counts are the literal suite/ROM pass-fail results, untouched.
//!  - **effective** counts exclude, from the fail bucket, any fail row
//!    covered by a currently-valid (non-expired) waiver.
//!  - A fail row with no waiver, or only an *expired* one, is "red" — and
//!    [`build_report`] itself returns `Err`, not a report with a red count
//!    in it. The report step is the enforcement mechanism, not a
//!    dashboard.
//!  - A waiver's `ticket` field must both look like a real ticket id and
//!    be reported open by the caller-supplied `ticket_is_open` closure —
//!    rf-harness never reads `plan.json` itself (conductor-owned; forcing
//!    a plan.json fixture into this crate's tests would be backwards).
//!
//! No wall clock anywhere in this module: `today` is a caller-supplied ISO
//! `YYYY-MM-DD` string (plain lexicographic comparison sorts correctly),
//! so expiry tests are deterministic without a date crate or
//! `SystemTime`.

use crate::json::Json;
use crate::manifest::{Console, Manifest, Tier, ValidationError};
use serde::Deserialize;
use std::fmt;

/// One accuracy-table result row (suite × ROM × pass/fail/frame).
///
/// Build these via [`AccuracyRow::from_manifest`], not by hand, whenever a
/// row corresponds to a real manifest suite: `console`/`fr`/`tier` are
/// TESTING.md §5 lookup data ("a lookup from the tables above, never a
/// judgment call"), and `from_manifest` is what actually performs that
/// lookup instead of letting a caller supply a free-standing `fr`/`tier`
/// pair that could silently drift from `tests/rom-manifest.toml`. The
/// struct literal stays available (and is what this module's own waiver
/// tests use) for constructing rows that deliberately aren't tied to a
/// real manifest — waiver/report logic doesn't care where a row's FR/tier
/// came from, only [`from_manifest`](AccuracyRow::from_manifest) does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccuracyRow {
    pub suite: String,
    pub rom: String,
    pub console: Console,
    pub fr: String,
    pub tier: Tier,
    pub status: RowStatus,
    /// Frame at which the result was determined (0 for frame-less
    /// protocols such as vector suites).
    pub frame: u32,
}

impl AccuracyRow {
    /// Build a row by resolving `suite_id`/`rom`'s `console`/`fr`/`tier`
    /// from `manifest`'s `[[suite]]` table — the enforced form of the
    /// suite→FR→tier mapping (TESTING.md §5; see struct doc).
    ///
    /// # Errors
    /// Returns [`ValidationError`] if `suite_id` isn't a suite in
    /// `manifest`, or `rom` isn't one of that suite's `[[suite.roms]]`
    /// labels.
    pub fn from_manifest(
        manifest: &Manifest,
        suite_id: &str,
        rom: &str,
        status: RowStatus,
        frame: u32,
    ) -> Result<AccuracyRow, ValidationError> {
        let suite = manifest.suite(suite_id).ok_or_else(|| {
            ValidationError(format!("no suite named {suite_id:?} in the manifest"))
        })?;
        if suite.rom(rom).is_none() {
            return Err(ValidationError(format!(
                "suite {suite_id:?} has no rom labeled {rom:?}"
            )));
        }
        Ok(AccuracyRow {
            suite: suite.id.clone(),
            rom: rom.to_string(),
            console: suite.console,
            fr: suite.fr.clone(),
            tier: suite.tier,
            status,
            frame,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Pass,
    Fail,
}

/// A waiver-file entry: `rom = "*"` waives every ROM in `suite`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Waiver {
    pub suite: String,
    pub rom: String,
    pub justification: String,
    /// ISO `YYYY-MM-DD`. The waiver is valid through and including this
    /// date; `today > expiry` (lexicographic) means expired.
    pub expiry: String,
    /// Ticket id, expected to match `^W\d+-\d{2}[a-z]?$` (same pattern
    /// `scripts/validate-plan.mjs` enforces on `plan.json`).
    pub ticket: String,
}

/// Parsed form of `crates/rf-harness/waivers.toml` (the canonical waiver
/// file — kept inside this crate's write scope rather than under
/// `tests/`, see that file's header comment).
#[derive(Debug, Clone, Deserialize)]
pub struct WaiverFile {
    #[serde(default, rename = "waiver")]
    pub waivers: Vec<Waiver>,
}

impl WaiverFile {
    /// Parse a `waivers.toml` document.
    ///
    /// # Errors
    /// Returns a human-readable message on malformed TOML.
    pub fn parse(text: &str) -> Result<WaiverFile, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }
}

impl Waiver {
    fn covers(&self, row: &AccuracyRow) -> bool {
        self.suite == row.suite && (self.rom == "*" || self.rom == row.rom)
    }
}

const TICKET_PATTERN_HINT: &str = "W<phase>-<NN>[a-z], e.g. W1-02 or W1-02a";

fn ticket_id_looks_valid(ticket: &str) -> bool {
    // ^W\d+-\d{2}[a-z]?$
    let Some(rest) = ticket.strip_prefix('W') else {
        return false;
    };
    let Some(dash) = rest.find('-') else {
        return false;
    };
    let (phase, suffix) = (&rest[..dash], &rest[dash + 1..]);
    if phase.is_empty() || !phase.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let digits = suffix.get(..2).unwrap_or("");
    let tail = suffix.get(2..).unwrap_or("");
    digits.len() == 2
        && digits.bytes().all(|b| b.is_ascii_digit())
        && matches!(tail.as_bytes(), [] | [b'a'..=b'z'])
}

/// Why [`build_report`] refused to produce a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    /// A failing row has no waiver at all, or only an expired one.
    UnwaivedRedRow { suite: String, rom: String },
    /// A waiver covers a failing row but its `ticket` field doesn't look
    /// like a real ticket id.
    MalformedTicket {
        suite: String,
        rom: String,
        ticket: String,
    },
    /// A waiver covers a failing row with a well-formed, non-expired
    /// waiver, but the ticket it names isn't reported open.
    TicketNotOpen {
        suite: String,
        rom: String,
        ticket: String,
    },
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReportError::UnwaivedRedRow { suite, rom } => write!(
                f,
                "red row {suite}/{rom} has no valid (non-expired) waiver — report step refuses to pass silently"
            ),
            ReportError::MalformedTicket {
                suite,
                rom,
                ticket,
            } => write!(
                f,
                "waiver for {suite}/{rom} names ticket {ticket:?}, which doesn't match {TICKET_PATTERN_HINT}"
            ),
            ReportError::TicketNotOpen {
                suite,
                rom,
                ticket,
            } => write!(
                f,
                "waiver for {suite}/{rom} names ticket {ticket}, which is not open"
            ),
        }
    }
}

impl std::error::Error for ReportError {}

/// Counts for either the raw or effective bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub total: u32,
    pub pass: u32,
    pub fail: u32,
}

/// A fully built, enforcement-clean accuracy report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub rows: Vec<ReportRow>,
    pub raw: Counts,
    pub effective: Counts,
}

/// A row as it appears in the emitted report: the original result plus
/// whether a waiver covered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    pub row: AccuracyRow,
    pub waived: bool,
}

/// Build the accuracy report, enforcing TESTING.md §5's waiver rules (see
/// module doc). `today` is ISO `YYYY-MM-DD`. `ticket_is_open` reports
/// whether a ticket id is currently open — injected so this crate never
/// reads `plan.json` itself.
///
/// # Errors
/// See [`ReportError`]. Returns the *first* offending row found (rows are
/// walked in input order) — this is a hard gate, not an accumulate-and-report
/// pass, matching "fails the report step" rather than "reports one more red row".
pub fn build_report(
    rows: &[AccuracyRow],
    waivers: &[Waiver],
    today: &str,
    ticket_is_open: impl Fn(&str) -> bool,
) -> Result<Report, ReportError> {
    let mut report_rows = Vec::with_capacity(rows.len());
    let mut raw = Counts::default();
    let mut effective = Counts::default();

    for row in rows {
        raw.total += 1;
        effective.total += 1;
        match row.status {
            RowStatus::Pass => {
                raw.pass += 1;
                effective.pass += 1;
                report_rows.push(ReportRow {
                    row: row.clone(),
                    waived: false,
                });
            }
            RowStatus::Fail => {
                raw.fail += 1;
                let valid_waiver = waivers
                    .iter()
                    .find(|w| w.covers(row) && w.expiry.as_str() >= today);

                match valid_waiver {
                    None => {
                        return Err(ReportError::UnwaivedRedRow {
                            suite: row.suite.clone(),
                            rom: row.rom.clone(),
                        });
                    }
                    Some(w) => {
                        if !ticket_id_looks_valid(&w.ticket) {
                            return Err(ReportError::MalformedTicket {
                                suite: row.suite.clone(),
                                rom: row.rom.clone(),
                                ticket: w.ticket.clone(),
                            });
                        }
                        if !ticket_is_open(&w.ticket) {
                            return Err(ReportError::TicketNotOpen {
                                suite: row.suite.clone(),
                                rom: row.rom.clone(),
                                ticket: w.ticket.clone(),
                            });
                        }
                        // Valid, ticketed, open waiver: counted in raw
                        // (above) but excluded from effective's fail
                        // bucket per TESTING.md §5.
                        report_rows.push(ReportRow {
                            row: row.clone(),
                            waived: true,
                        });
                    }
                }
            }
        }
    }

    Ok(Report {
        rows: report_rows,
        raw,
        effective,
    })
}

impl Report {
    /// Render as the machine-readable JSON shape (ticket W0-03 acceptance
    /// criterion 5: "suite × ROM × pass/fail/frame JSON").
    #[must_use]
    pub fn to_json(&self) -> Json {
        let rows = self
            .rows
            .iter()
            .map(|r| {
                Json::object(vec![
                    ("suite", Json::str(r.row.suite.clone())),
                    ("rom", Json::str(r.row.rom.clone())),
                    (
                        "console",
                        Json::str(match r.row.console {
                            Console::Nes => "nes",
                            Console::Snes => "snes",
                        }),
                    ),
                    ("fr", Json::str(r.row.fr.clone())),
                    (
                        "tier",
                        Json::str(match r.row.tier {
                            Tier::A => "A",
                            Tier::B => "B",
                        }),
                    ),
                    (
                        "status",
                        Json::str(match (r.row.status, r.waived) {
                            (RowStatus::Pass, _) => "pass",
                            (RowStatus::Fail, true) => "waived",
                            (RowStatus::Fail, false) => "fail",
                        }),
                    ),
                    ("frame", Json::Int(i64::from(r.row.frame))),
                ])
            })
            .collect();
        Json::object(vec![
            ("rows", Json::Array(rows)),
            ("raw", counts_json(self.raw)),
            ("effective", counts_json(self.effective)),
        ])
    }
}

fn counts_json(c: Counts) -> Json {
    Json::object(vec![
        ("total", Json::Int(i64::from(c.total))),
        ("pass", Json::Int(i64::from(c.pass))),
        ("fail", Json::Int(i64::from(c.fail))),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(suite: &str, rom: &str, status: RowStatus) -> AccuracyRow {
        AccuracyRow {
            suite: suite.into(),
            rom: rom.into(),
            console: Console::Nes,
            fr: "FR-CORE-020".into(),
            tier: Tier::A,
            status,
            frame: 100,
        }
    }

    fn waiver(suite: &str, rom: &str, expiry: &str, ticket: &str) -> Waiver {
        Waiver {
            suite: suite.into(),
            rom: rom.into(),
            justification: "known upstream quirk, tracked".into(),
            expiry: expiry.into(),
            ticket: ticket.into(),
        }
    }

    #[test]
    fn all_pass_report_has_zero_fail_and_no_waivers_needed() {
        let rows = vec![
            row("s1", "r1", RowStatus::Pass),
            row("s1", "r2", RowStatus::Pass),
        ];
        let report = build_report(&rows, &[], "2026-08-02", |_| false).unwrap();
        assert_eq!(
            report.raw,
            Counts {
                total: 2,
                pass: 2,
                fail: 0
            }
        );
        assert_eq!(
            report.effective,
            Counts {
                total: 2,
                pass: 2,
                fail: 0
            }
        );
        assert!(report.rows.iter().all(|r| !r.waived));
    }

    /// TESTING.md §5: "a red row with no open ticket fails the report
    /// step" — the no-waiver-at-all case.
    #[test]
    fn unwaived_fail_row_fails_the_report_step() {
        let rows = vec![row("s1", "r1", RowStatus::Fail)];
        let err = build_report(&rows, &[], "2026-08-02", |_| true).unwrap_err();
        assert_eq!(
            err,
            ReportError::UnwaivedRedRow {
                suite: "s1".into(),
                rom: "r1".into(),
            }
        );
    }

    /// TESTING.md §5: "an expired waiver reopens red".
    #[test]
    fn expired_waiver_reopens_red() {
        let rows = vec![row("s1", "r1", RowStatus::Fail)];
        let waivers = vec![waiver("s1", "r1", "2026-01-01", "W1-02")]; // expired
        let err = build_report(&rows, &waivers, "2026-08-02", |_| true).unwrap_err();
        assert_eq!(
            err,
            ReportError::UnwaivedRedRow {
                suite: "s1".into(),
                rom: "r1".into(),
            }
        );
    }

    #[test]
    fn waiver_valid_through_and_including_expiry_date() {
        let rows = vec![row("s1", "r1", RowStatus::Fail)];
        let waivers = vec![waiver("s1", "r1", "2026-08-02", "W1-02")];
        // today == expiry: still valid (inclusive).
        let report = build_report(&rows, &waivers, "2026-08-02", |_| true).unwrap();
        assert_eq!(report.raw.fail, 1);
        assert_eq!(report.effective.fail, 0);
        assert!(report.rows[0].waived);
    }

    #[test]
    fn day_after_expiry_is_expired() {
        let rows = vec![row("s1", "r1", RowStatus::Fail)];
        let waivers = vec![waiver("s1", "r1", "2026-08-02", "W1-02")];
        let err = build_report(&rows, &waivers, "2026-08-03", |_| true).unwrap_err();
        assert!(matches!(err, ReportError::UnwaivedRedRow { .. }));
    }

    #[test]
    fn wildcard_rom_waives_whole_suite() {
        let rows = vec![
            row("s1", "r1", RowStatus::Fail),
            row("s1", "r2", RowStatus::Fail),
        ];
        let waivers = vec![waiver("s1", "*", "2099-01-01", "W1-02")];
        let report = build_report(&rows, &waivers, "2026-08-02", |_| true).unwrap();
        assert_eq!(report.raw.fail, 2);
        assert_eq!(report.effective.fail, 0);
    }

    #[test]
    fn malformed_ticket_id_fails_the_report_step() {
        let rows = vec![row("s1", "r1", RowStatus::Fail)];
        let waivers = vec![waiver("s1", "r1", "2099-01-01", "not-a-ticket")];
        let err = build_report(&rows, &waivers, "2026-08-02", |_| true).unwrap_err();
        assert!(matches!(err, ReportError::MalformedTicket { .. }));
    }

    #[test]
    fn ticket_must_be_reported_open() {
        let rows = vec![row("s1", "r1", RowStatus::Fail)];
        let waivers = vec![waiver("s1", "r1", "2099-01-01", "W1-02")];
        let err = build_report(&rows, &waivers, "2026-08-02", |_| false).unwrap_err();
        assert_eq!(
            err,
            ReportError::TicketNotOpen {
                suite: "s1".into(),
                rom: "r1".into(),
                ticket: "W1-02".into(),
            }
        );
    }

    /// `W1-02d` moved from REJECTED to ACCEPTED on 2026-08-05, deliberately.
    /// The suffix class was `[a-c]`, which capped a ticket family at three
    /// splits for no stated reason; W1-05c's close needed a fourth
    /// (`W1-05d`, owning `10-even_odd_timing` after that ROM was proven a
    /// separate defect), so the class was widened to `[a-z]` here and in
    /// `scripts/validate-plan.mjs`'s matching `ID_RE`. Everything the
    /// pattern was actually protecting against still fails below: a
    /// lowercase `W`, a one-digit number, a multi-character suffix, an
    /// uppercase suffix, and a non-ticket string.
    #[test]
    fn ticket_id_pattern_accepts_documented_shapes() {
        assert!(ticket_id_looks_valid("W1-02"));
        assert!(ticket_id_looks_valid("W0-03a"));
        assert!(ticket_id_looks_valid("W12-99c"));
        assert!(ticket_id_looks_valid("W1-02d"));
        assert!(ticket_id_looks_valid("W1-05z"));
        assert!(!ticket_id_looks_valid("w1-02"));
        assert!(!ticket_id_looks_valid("W1-2"));
        assert!(!ticket_id_looks_valid("W1-02ab"));
        assert!(!ticket_id_looks_valid("W1-02A"));
        assert!(!ticket_id_looks_valid("ticket-42"));
    }

    #[test]
    fn raw_and_effective_counts_separated_with_mixed_rows() {
        let rows = vec![
            row("s1", "r1", RowStatus::Pass),
            row("s1", "r2", RowStatus::Fail),
            row("s2", "r1", RowStatus::Fail),
        ];
        let waivers = vec![
            waiver("s1", "r2", "2099-01-01", "W1-02"),
            waiver("s2", "r1", "2099-01-01", "W1-03"),
        ];
        let report = build_report(&rows, &waivers, "2026-08-02", |_| true).unwrap();
        assert_eq!(
            report.raw,
            Counts {
                total: 3,
                pass: 1,
                fail: 2
            }
        );
        assert_eq!(
            report.effective,
            Counts {
                total: 3,
                pass: 1,
                fail: 0
            }
        );
    }

    #[test]
    fn json_shape_has_expected_top_level_keys() {
        let rows = vec![row("instr_test-v5", "all_instrs", RowStatus::Pass)];
        let report = build_report(&rows, &[], "2026-08-02", |_| false).unwrap();
        let json = report.to_json().to_json_string();
        assert!(json.contains("\"rows\":["));
        assert!(json.contains("\"raw\":{"));
        assert!(json.contains("\"effective\":{"));
        assert!(json.contains("\"suite\":\"instr_test-v5\""));
        assert!(json.contains("\"status\":\"pass\""));
    }

    #[test]
    fn json_marks_waived_rows_distinctly_from_pass_and_fail() {
        let rows = vec![row("s1", "r1", RowStatus::Fail)];
        let waivers = vec![waiver("s1", "r1", "2099-01-01", "W1-02")];
        let report = build_report(&rows, &waivers, "2026-08-02", |_| true).unwrap();
        let json = report.to_json().to_json_string();
        assert!(json.contains("\"status\":\"waived\""));
    }

    #[test]
    fn waiver_file_parses_empty_document() {
        let wf = WaiverFile::parse("").unwrap();
        assert!(wf.waivers.is_empty());
    }

    #[test]
    fn waiver_file_parses_populated_document() {
        let text = r#"
            [[waiver]]
            suite = "apu_mixer"
            rom = "*"
            justification = "RMS envelope automation lands with rf-audio (later ticket)"
            expiry = "2026-12-31"
            ticket = "W4-02"
        "#;
        let wf = WaiverFile::parse(text).unwrap();
        assert_eq!(wf.waivers.len(), 1);
        assert_eq!(wf.waivers[0].suite, "apu_mixer");
        assert_eq!(wf.waivers[0].ticket, "W4-02");
    }

    /// Ties the crate to the real, checked-in waiver file the same way
    /// `manifest::tests::real_manifest_parses_and_validates` ties it to
    /// the real ROM manifest.
    #[test]
    fn real_waiver_file_parses() {
        let text = include_str!("../waivers.toml");
        WaiverFile::parse(text).expect("real waivers.toml must parse");
    }

    fn sample_manifest() -> crate::manifest::Manifest {
        crate::manifest::Manifest::parse(
            r#"
            [[artifact]]
            id = "a1"
            license_status = "public-domain"
            sha256 = "TODO-test"
            mirrors = ["https://example.invalid/a1"]
            dest = "roms/nes/a1.nes"

            [[suite]]
            id = "mmc3_test_2"
            console = "nes"
            fr = "FR-CORE-025"
            tier = "A"
            protocol = "six_thousand"
            [[suite.roms]]
            artifact = "a1"
            rom = "1-clocking"
            frame_budget = 600
        "#,
        )
        .unwrap()
    }

    /// TESTING.md §5: the suite→FR→tier mapping is "a lookup from the
    /// tables above, never a judgment call" — `from_manifest` performs
    /// that lookup rather than accepting `fr`/`tier` as free-standing
    /// caller-supplied fields that could drift from the manifest.
    #[test]
    fn from_manifest_resolves_fr_tier_console_from_the_manifest() {
        let manifest = sample_manifest();
        let row = AccuracyRow::from_manifest(
            &manifest,
            "mmc3_test_2",
            "1-clocking",
            RowStatus::Pass,
            120,
        )
        .unwrap();
        assert_eq!(row.console, Console::Nes);
        assert_eq!(row.fr, "FR-CORE-025");
        assert_eq!(row.tier, Tier::A);
        assert_eq!(row.frame, 120);
    }

    #[test]
    fn from_manifest_errors_on_unknown_suite_rather_than_defaulting() {
        let manifest = sample_manifest();
        let err = AccuracyRow::from_manifest(
            &manifest,
            "does-not-exist",
            "1-clocking",
            RowStatus::Pass,
            0,
        )
        .unwrap_err();
        assert!(err.0.contains("does-not-exist"));
    }

    #[test]
    fn from_manifest_errors_on_unknown_rom_rather_than_defaulting() {
        let manifest = sample_manifest();
        let err =
            AccuracyRow::from_manifest(&manifest, "mmc3_test_2", "not-a-rom", RowStatus::Pass, 0)
                .unwrap_err();
        assert!(err.0.contains("not-a-rom"));
    }

    #[test]
    fn manifest_frame_budget_looks_up_the_declared_timeout() {
        let manifest = sample_manifest();
        assert_eq!(
            manifest
                .suite("mmc3_test_2")
                .unwrap()
                .rom("1-clocking")
                .unwrap()
                .frame_budget,
            600
        );
        assert!(manifest.suite("mmc3_test_2").unwrap().rom("nope").is_none());
    }
}
