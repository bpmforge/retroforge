//! `local-gate-evidence` — the binary `scripts/local-gate.sh` wraps
//! (ticket W0-07, extended by ticket W1-03 for the second Tier-A-local
//! suite, and by ticket W1-05b for the third and fourth).
//!
//! Runs the nes6502 SingleStepTests vector suite via
//! [`rf_harness::nes6502_evidence::run_all`], the nestest golden-trace
//! diff via [`rf_harness::nestest_evidence::run`], and (ticket W1-05b) the
//! `apu_test` (1 combined ROM, ticket W2-01a),
//! `ppu_vbl_nmi` (10 ROMs, blargg `$6000` protocol, via
//! [`rf_harness::blargg_evidence::run`]) and `sprite_hit_tests` (11 ROMs,
//! RAM-result-byte protocol, via
//! [`rf_harness::blargg_evidence::run_ram_result`] — see
//! `tests/rom-manifest.toml`'s comment on that suite for why it isn't the
//! `$6000` protocol too) suites — see [`SuiteResult`]'s doc for why those
//! two get per-ROM rows instead of nes6502/nestest's one-row-per-suite
//! shape. Builds the W0-03 accuracy report ([`rf_harness::build_report`])
//! covering all of the above, and prints the combined evidence as JSON on
//! stdout — `docs/evidence/local-gate.json`'s exact contents, one rolling
//! file (git history is the audit trail).
//!
//! This binary deliberately never reads `plan.json` — `today` and the
//! open-ticket-id list are supplied by the caller (`scripts/local-gate.sh`,
//! which reads `plan.json` in the shell/node layer), matching the
//! boundary `rf_harness::accuracy`'s module doc already establishes for
//! `build_report`'s `ticket_is_open` closure.
//!
//! Nothing is printed to stdout unless generation fully succeeds — a
//! partial or failed run must not let its caller mistake truncated output
//! for real evidence (mirrors `fetch_artifact`'s "verify before writing"
//! discipline, applied to this binary's own output instead of a
//! downloaded file).
use rf_harness::blargg_evidence::{self, RamResultOutcome};
use rf_harness::nes6502_evidence::run_all;
use rf_harness::nestest_evidence;
use rf_harness::{
    build_report, git_rev_parse_head, git_tree_is_clean, AccuracyRow, BlarggStatus, Json, Manifest,
    RowStatus, SystemGitRunner, WaiverFile,
};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

struct Args {
    manifest: PathBuf,
    waivers: PathBuf,
    repo_root: PathBuf,
    vectors_dir: PathBuf,
    nestest_rom: PathBuf,
    nestest_log: PathBuf,
    today: String,
    generated_at: String,
    open_tickets: Vec<String>,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut manifest = PathBuf::from("tests/rom-manifest.toml");
    let mut waivers = PathBuf::from("crates/rf-harness/waivers.toml");
    let mut repo_root = PathBuf::from(".");
    let mut vectors_dir = None;
    let mut nestest_rom = None;
    let mut nestest_log = None;
    let mut today = None;
    let mut generated_at = None;
    let mut open_tickets = Vec::new();

    let mut i = 0;
    while i < raw.len() {
        let flag = raw[i].as_str();
        let mut next = || -> Result<String, String> {
            i += 1;
            raw.get(i)
                .cloned()
                .ok_or_else(|| format!("{flag} requires a value argument"))
        };
        match flag {
            "--manifest" => manifest = PathBuf::from(next()?),
            "--waivers" => waivers = PathBuf::from(next()?),
            "--repo-root" => repo_root = PathBuf::from(next()?),
            "--vectors-dir" => vectors_dir = Some(PathBuf::from(next()?)),
            "--nestest-rom" => nestest_rom = Some(PathBuf::from(next()?)),
            "--nestest-log" => nestest_log = Some(PathBuf::from(next()?)),
            "--today" => today = Some(next()?),
            "--generated-at" => generated_at = Some(next()?),
            "--open-ticket" => open_tickets.push(next()?),
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }

    Ok(Args {
        manifest,
        waivers,
        repo_root,
        vectors_dir: vectors_dir.ok_or("--vectors-dir is required")?,
        nestest_rom: nestest_rom.ok_or("--nestest-rom is required")?,
        nestest_log: nestest_log.ok_or("--nestest-log is required")?,
        today: today.ok_or("--today is required")?,
        generated_at: generated_at.ok_or("--generated-at is required")?,
        open_tickets,
    })
}

fn main() -> ExitCode {
    let raw: Vec<String> = env::args().skip(1).collect();
    let args = match parse_args(&raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("local-gate-evidence: {e}");
            return ExitCode::FAILURE;
        }
    };
    run(&args)
}

/// Per-ROM outcome + accuracy row for one manifest suite driven against a
/// real `NesBus` (ticket W1-05b) — shared shape for both
/// `protocol = "six_thousand"` (`ppu_vbl_nmi`) and `protocol = "ram_result"`
/// (`sprite_hit_tests`, see `tests/rom-manifest.toml`'s comment on that
/// suite for why it isn't `six_thousand` too).
///
/// Unlike nes6502/nestest (one aggregate row each — "did every case pass"
/// is the only question those two suites' accuracy-table entry needs to
/// answer), `sprite_hit_tests`/`ppu_vbl_nmi` already have one
/// `[[suite.roms]]` entry per named sub-ROM in `tests/rom-manifest.toml`
/// (matching `docs/TESTING.md` §4's own per-suite rows), so this emits one
/// [`AccuracyRow`] per ROM — the finer grain the manifest schema doc
/// already calls for ("Suite rows in TESTING.md that bundle several named
/// blargg test programs into one FR/tier cell are split here into one
/// `[[suite]]` per named program"). A future waiver for exactly one failing
/// sub-ROM (not the whole suite) needs this grain to be expressible at all.
struct SuiteResult {
    rows: Vec<AccuracyRow>,
    roms_tested: usize,
    roms_passed: usize,
    roms_failed: usize,
    /// `(rom label, human-readable reason)` — printed by the caller and
    /// folded into this suite's JSON summary object, so a failure is named
    /// rather than only counted.
    failing: Vec<(String, String)>,
}

/// Drives every `[[suite.roms]]` row of manifest suite `suite_id` through
/// `run_rom` (`(rom_path, frame_budget) -> (status, frame, note)`),
/// refusing (an `Err`, not a partial result) if the suite is missing or its
/// row count doesn't match `expected_roms` — a silent manifest edit
/// shrinking this suite would otherwise under-report coverage exactly the
/// way `nes6502`'s `opcodes_tested != 256` guard and `nestest`'s
/// `lines_compared != 8991` guard both exist to catch for their own
/// suites. Protocol-specific (blargg `$6000` vs. RAM-result-byte) logic
/// lives in `run_rom`, supplied by [`run_six_thousand_suite`]/
/// [`run_ram_result_suite`] — this function only owns the shared
/// bookkeeping (row-count guard, per-ROM `AccuracyRow` construction,
/// pass/fail tallying, per-ROM `eprintln!`).
fn run_suite(
    manifest: &Manifest,
    suite_id: &str,
    expected_roms: usize,
    repo_root: &Path,
    mut run_rom: impl FnMut(&Path, u32) -> (RowStatus, u32, String),
) -> Result<SuiteResult, String> {
    let suite = manifest
        .suite(suite_id)
        .ok_or_else(|| format!("manifest has no suite named {suite_id:?}"))?;
    if suite.roms.len() != expected_roms {
        return Err(format!(
            "suite {suite_id:?} has {} rom row(s) in the manifest, expected {expected_roms} — \
             a partial/edited manifest would silently under-report coverage",
            suite.roms.len()
        ));
    }

    let mut rows = Vec::with_capacity(suite.roms.len());
    let mut roms_passed = 0usize;
    let mut roms_failed = 0usize;
    let mut failing = Vec::new();

    for suite_rom in &suite.roms {
        let artifact = manifest.artifact(&suite_rom.artifact).ok_or_else(|| {
            format!(
                "suite {suite_id:?} rom {:?}: unknown artifact id {:?}",
                suite_rom.rom, suite_rom.artifact
            )
        })?;
        let rom_path = repo_root.join(&artifact.dest);

        let (status, frame, note) = run_rom(&rom_path, suite_rom.frame_budget);

        eprintln!(
            "{suite_id}/{}: {} (frame {frame}) {note}",
            suite_rom.rom,
            if status == RowStatus::Pass {
                "PASS"
            } else {
                "FAIL"
            },
        );

        match status {
            RowStatus::Pass => roms_passed += 1,
            RowStatus::Fail => {
                roms_failed += 1;
                failing.push((suite_rom.rom.clone(), note));
            }
        }

        let row = AccuracyRow::from_manifest(manifest, suite_id, &suite_rom.rom, status, frame)
            .map_err(|e| {
                format!(
                    "failed to build accuracy row for {suite_id}/{}: {e}",
                    suite_rom.rom
                )
            })?;
        rows.push(row);
    }

    Ok(SuiteResult {
        rows,
        roms_tested: suite.roms.len(),
        roms_passed,
        roms_failed,
        failing,
    })
}

/// `protocol = "six_thousand"` suites (`ppu_vbl_nmi`) via
/// [`blargg_evidence::run`].
fn run_six_thousand_suite(
    manifest: &Manifest,
    suite_id: &str,
    expected_roms: usize,
    repo_root: &Path,
) -> Result<SuiteResult, String> {
    run_suite(
        manifest,
        suite_id,
        expected_roms,
        repo_root,
        |rom_path, frame_budget| match blargg_evidence::run(rom_path, frame_budget) {
            Ok(outcome) => match outcome.status {
                BlarggStatus::Passed => (RowStatus::Pass, outcome.frames_run, outcome.message),
                BlarggStatus::Failed(code) => (
                    RowStatus::Fail,
                    outcome.frames_run,
                    format!("failed with code ${code:02X}: {}", outcome.message),
                ),
                BlarggStatus::NeedsReset => (
                    RowStatus::Fail,
                    outcome.frames_run,
                    "protocol requested a reset button press, which this runner does not honor"
                        .to_string(),
                ),
            },
            Err(e) => (RowStatus::Fail, 0, e),
        },
    )
}

/// `protocol = "ram_result"` suites (`sprite_hit_tests`) via
/// [`blargg_evidence::run_ram_result`]. `result_addr` is the suite-specific
/// RAM location (e.g. `0x00F8` for `sprite_hit_tests`' `validation.a`
/// `result` symbol — see that function's doc). `frame` is always
/// `frame_budget` (the protocol has no earlier-completion signal by
/// design — see `run_ram_result`'s doc for why running the full budget is
/// deliberate, not a missed optimization).
fn run_ram_result_suite(
    manifest: &Manifest,
    suite_id: &str,
    expected_roms: usize,
    repo_root: &Path,
    result_addr: u16,
) -> Result<SuiteResult, String> {
    run_suite(
        manifest,
        suite_id,
        expected_roms,
        repo_root,
        |rom_path, frame_budget| match blargg_evidence::run_ram_result(
            rom_path,
            frame_budget,
            result_addr,
        ) {
            Ok(RamResultOutcome::Passed) => (RowStatus::Pass, frame_budget, "Passed".to_string()),
            Ok(RamResultOutcome::Failed(0)) => (
                RowStatus::Fail,
                frame_budget,
                format!(
                    "result byte at ${result_addr:04X} was still 0 after the full frame \
                     budget -- test likely never started or never finished"
                ),
            ),
            Ok(RamResultOutcome::Failed(code)) => (
                RowStatus::Fail,
                frame_budget,
                format!("result byte at ${result_addr:04X} = {code} (blargg error code)"),
            ),
            Err(e) => (RowStatus::Fail, 0, e),
        },
    )
}

fn suite_summary_json(suite_id: &str, r: &SuiteResult) -> Json {
    Json::object(vec![
        ("suite", Json::str(suite_id)),
        ("roms_tested", Json::Int(r.roms_tested as i64)),
        ("roms_passed", Json::Int(r.roms_passed as i64)),
        ("roms_failed", Json::Int(r.roms_failed as i64)),
        (
            "failing",
            Json::Array(
                r.failing
                    .iter()
                    .map(|(rom, reason)| {
                        Json::object(vec![
                            ("rom", Json::str(rom.clone())),
                            ("reason", Json::str(reason.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn run(args: &Args) -> ExitCode {
    let manifest_text = match std::fs::read_to_string(&args.manifest) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("failed to read manifest {}: {e}", args.manifest.display());
            return ExitCode::FAILURE;
        }
    };
    let manifest = match Manifest::parse(&manifest_text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("failed to parse manifest: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(errors) = manifest.validate() {
        eprintln!("manifest failed validation:");
        for e in &errors {
            eprintln!("  - {e}");
        }
        return ExitCode::FAILURE;
    }

    let waivers_text = match std::fs::read_to_string(&args.waivers) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("failed to read waivers {}: {e}", args.waivers.display());
            return ExitCode::FAILURE;
        }
    };
    let waiver_file = match WaiverFile::parse(&waivers_text) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("failed to parse waivers: {e}");
            return ExitCode::FAILURE;
        }
    };

    // --- nes6502 vector suite -------------------------------------------
    let summary = match run_all(&args.vectors_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nes6502 vector run failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "nes6502 vectors: {} opcode files, {} cases passed, {} cases failed",
        summary.opcodes_tested, summary.total_pass, summary.total_fail
    );
    if summary.opcodes_tested != 256 {
        eprintln!(
            "refusing to emit evidence: expected 256 opcode files, found {} — a partial vectors \
             checkout would silently under-report coverage",
            summary.opcodes_tested
        );
        return ExitCode::FAILURE;
    }
    for f in &summary.failing {
        eprintln!(
            "  ${:02X}: {} passed, {} failed; first failure: {}",
            f.opcode,
            f.pass,
            f.fail,
            f.first_failure.as_deref().unwrap_or("")
        );
    }

    let row_status = if summary.total_fail == 0 {
        RowStatus::Pass
    } else {
        RowStatus::Fail
    };
    let row =
        match AccuracyRow::from_manifest(&manifest, "nes6502", "nes6502-vectors", row_status, 0) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("failed to build accuracy row: {e}");
                return ExitCode::FAILURE;
            }
        };

    // --- nestest golden-trace diff ---------------------------------------
    let nestest_summary = match nestest_evidence::run(&args.nestest_rom, &args.nestest_log) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nestest run failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "nestest: {} lines compared, {} matched",
        nestest_summary.lines_compared, nestest_summary.lines_matched
    );
    if let Some(d) = &nestest_summary.first_divergence {
        eprintln!("  first divergence at line {}:", d.line_number);
        eprintln!("    expected: {}", d.expected);
        eprintln!("    got:      {}", d.got);
    }
    if nestest_summary.lines_compared != 8991 {
        eprintln!(
            "refusing to emit evidence: expected 8991 lines in nestest.log, found {} — a \
             truncated/wrong log would silently under-report coverage",
            nestest_summary.lines_compared
        );
        return ExitCode::FAILURE;
    }
    let nestest_row_status = if nestest_summary.lines_matched == nestest_summary.lines_compared {
        RowStatus::Pass
    } else {
        RowStatus::Fail
    };
    let nestest_row =
        match AccuracyRow::from_manifest(&manifest, "nestest", "nestest", nestest_row_status, 0) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("failed to build accuracy row: {e}");
                return ExitCode::FAILURE;
            }
        };

    // --- sprite_hit_tests (11 ROMs) + ppu_vbl_nmi (10 ROMs), ticket W1-05b
    // ------------------------------------------------------------------
    let sprite_hit =
        match run_ram_result_suite(&manifest, "sprite_hit_tests", 11, &args.repo_root, 0x00F8) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("sprite_hit_tests run failed: {e}");
                return ExitCode::FAILURE;
            }
        };
    eprintln!(
        "sprite_hit_tests: {}/{} ROMs passed",
        sprite_hit.roms_passed, sprite_hit.roms_tested
    );

    let ppu_vbl_nmi = match run_six_thousand_suite(&manifest, "ppu_vbl_nmi", 10, &args.repo_root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ppu_vbl_nmi run failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "ppu_vbl_nmi: {}/{} ROMs passed",
        ppu_vbl_nmi.roms_passed, ppu_vbl_nmi.roms_tested
    );

    // --- apu_test (1 combined ROM, 8 sub-tests), ticket W2-01a -----------
    // The combined `apu_test.nes` is one manifest ROM that runs all eight
    // sub-tests in sequence and reports the first failure's number, so it
    // is a single row here rather than the per-sub-ROM grain
    // `ppu_vbl_nmi`/`sprite_hit_tests` use (the manifest's `rom_singles`
    // are not fetched -- see `tests/rom-manifest.toml`'s `apu_test` suite).
    let apu_test = match run_six_thousand_suite(&manifest, "apu_test", 1, &args.repo_root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("apu_test run failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "apu_test: {}/{} ROMs passed",
        apu_test.roms_passed, apu_test.roms_tested
    );

    let open_tickets = args.open_tickets.clone();
    let mut all_rows = vec![row, nestest_row];
    all_rows.extend(sprite_hit.rows.iter().cloned());
    all_rows.extend(ppu_vbl_nmi.rows.iter().cloned());
    all_rows.extend(apu_test.rows.iter().cloned());
    let report = match build_report(&all_rows, &waiver_file.waivers, &args.today, |t| {
        open_tickets.iter().any(|o| o == t)
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("accuracy report refused to build: {e}");
            return ExitCode::FAILURE;
        }
    };

    // --- git provenance ---------------------------------------------------
    let git = SystemGitRunner;
    let retroforge_commit = match git_rev_parse_head(&git, &args.repo_root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("failed to determine repo commit: {e}");
            return ExitCode::FAILURE;
        }
    };
    let tree_clean = match git_tree_is_clean(&git, &args.repo_root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to check working tree state: {e}");
            return ExitCode::FAILURE;
        }
    };
    let vector_source_commit = match git_rev_parse_head(&git, &args.vectors_dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!(
                "failed to determine vector source commit for {}: {e}",
                args.vectors_dir.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let toolchain = rustc_version();

    let first_divergence_json = match &nestest_summary.first_divergence {
        None => Json::Null,
        Some(d) => Json::object(vec![
            ("line_number", Json::Int(d.line_number as i64)),
            ("expected", Json::str(d.expected.clone())),
            ("got", Json::str(d.got.clone())),
        ]),
    };

    let evidence = Json::object(vec![
        ("generated_at", Json::str(args.generated_at.clone())),
        ("retroforge_commit", Json::str(retroforge_commit)),
        ("tree_clean", Json::Bool(tree_clean)),
        ("toolchain", Json::str(toolchain)),
        (
            "vectors",
            Json::object(vec![
                ("suite", Json::str("nes6502")),
                ("source_commit", Json::str(vector_source_commit)),
                ("opcodes_tested", Json::Int(summary.opcodes_tested as i64)),
                ("total_pass", Json::Int(summary.total_pass as i64)),
                ("total_fail", Json::Int(summary.total_fail as i64)),
            ]),
        ),
        (
            "nestest",
            Json::object(vec![
                (
                    "lines_compared",
                    Json::Int(nestest_summary.lines_compared as i64),
                ),
                (
                    "lines_matched",
                    Json::Int(nestest_summary.lines_matched as i64),
                ),
                ("first_divergence", first_divergence_json),
            ]),
        ),
        (
            "sprite_hit_tests",
            suite_summary_json("sprite_hit_tests", &sprite_hit),
        ),
        (
            "ppu_vbl_nmi",
            suite_summary_json("ppu_vbl_nmi", &ppu_vbl_nmi),
        ),
        ("apu_test", suite_summary_json("apu_test", &apu_test)),
        ("accuracy_table", report.to_json()),
    ]);

    println!("{}", evidence.to_json_string());
    ExitCode::SUCCESS
}

fn rustc_version() -> String {
    Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
