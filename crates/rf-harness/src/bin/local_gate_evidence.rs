//! `local-gate-evidence` — the binary `scripts/local-gate.sh` wraps
//! (ticket W0-07, extended by ticket W1-03 for the second Tier-A-local
//! suite).
//!
//! Runs the nes6502 SingleStepTests vector suite via
//! [`rf_harness::nes6502_evidence::run_all`] and the nestest golden-trace
//! diff via [`rf_harness::nestest_evidence::run`], builds the W0-03
//! accuracy report ([`rf_harness::build_report`]) covering both, and prints
//! the combined evidence as JSON on stdout — `docs/evidence/local-gate.json`'s
//! exact contents, one rolling file (git history is the audit trail).
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
use rf_harness::nes6502_evidence::run_all;
use rf_harness::nestest_evidence;
use rf_harness::{
    build_report, git_rev_parse_head, git_tree_is_clean, AccuracyRow, Json, Manifest, RowStatus,
    SystemGitRunner, WaiverFile,
};
use std::env;
use std::path::PathBuf;
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

    let open_tickets = args.open_tickets.clone();
    let report = match build_report(
        &[row, nestest_row],
        &waiver_file.waivers,
        &args.today,
        |t| open_tickets.iter().any(|o| o == t),
    ) {
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
