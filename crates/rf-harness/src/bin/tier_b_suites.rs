//! Tier-B suite runner for the nightly gate (ticket W2-09;
//! `docs/TESTING.md` §4's "Tier B = nightly (slow or visual-manual-once
//! suites)").
//!
//! ## Why a separate binary from `local-gate-evidence`
//!
//! `local-gate-evidence` produces the committed evidence file for the
//! **Tier-A-local** suites, and its shape is fixed by
//! `scripts/validate-evidence.mjs`. Tier B is a different job with a
//! different cadence and no committed artifact: it runs a list of suites and
//! reports pass/fail. Folding it into the evidence binary would have meant
//! either polluting that file with results the validator does not expect, or
//! adding a mode flag that changes what an evidence file means.
//!
//! ## Reporting: every suite runs, then the run fails once
//!
//! A runner that stops at the first red suite tells you about one problem
//! per nightly. This runs them all and exits non-zero at the end with a
//! summary, so one night's log is one night's whole picture.
//!
//! ## Known-fails go through the existing waiver file, not a local list
//!
//! `crates/rf-harness/waivers.toml` is already the project's ENFORCEMENT
//! mechanism for known-fails (`TESTING.md` §5: "every known-fail carries a
//! justification AND an expiry date; an expired waiver reopens red"). This
//! runner consults it rather than inventing a second, weaker list, so a
//! Tier-B known-fail is subject to exactly the same discipline as a
//! Tier-A-local one: named ticket, stated reason, expiry date.
//!
//! A waived failure is reported as WAIVED and does not fail the run; an
//! EXPIRED waiver does. And a waiver that covers a suite which now PASSES
//! is itself an error — a stale waiver hides the next regression in that
//! suite, so the runner insists it be deleted.
//!
//! ## Suites with no fetched ROMs are a SKIP, not a pass
//!
//! `roms/` is gitignored (NFR-006). A missing ROM is reported as skipped and
//! counted separately — never folded into "passed", which is the failure
//! mode that makes a green nightly meaningless.

use std::path::PathBuf;
use std::process::ExitCode;

use rf_harness::blargg_evidence;
use rf_harness::blargg_evidence::ScreenOutcome;
use rf_harness::{BlarggStatus, Manifest, Protocol, Suite, Tier, Waiver, WaiverFile};

/// Tier-B suites come from the manifest's own `tier` field rather than a
/// list repeated here: `tests/rom-manifest.toml` already mirrors
/// `docs/TESTING.md` §4's table, and a second copy in this file is a second
/// thing that can silently disagree with it.
fn tier_b_suites(manifest: &Manifest) -> Vec<&Suite> {
    manifest
        .suites
        .iter()
        .filter(|suite| suite.tier == Tier::B)
        .collect()
}

struct Args {
    manifest: PathBuf,
    repo_root: PathBuf,
    waivers: PathBuf,
    /// `YYYY-MM-DD`, injected rather than read from the clock so a run is
    /// reproducible and a test can drive expiry — the same discipline
    /// `rf_harness::build_report` uses.
    today: String,
}

/// Does `waiver` cover `suite`/`rom`, and is it still valid today?
fn waiver_for<'a>(waivers: &'a [Waiver], suite: &str, rom: &str) -> Option<&'a Waiver> {
    waivers
        .iter()
        .find(|w| w.suite == suite && (w.rom == "*" || w.rom == rom))
}

fn parse_args() -> Args {
    let mut manifest = PathBuf::from("tests/rom-manifest.toml");
    let mut repo_root = PathBuf::from(".");
    let mut waivers = PathBuf::from("crates/rf-harness/waivers.toml");
    let mut today = String::new();
    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        match flag.as_str() {
            "--manifest" => manifest = PathBuf::from(argv.next().unwrap_or_default()),
            "--repo-root" => repo_root = PathBuf::from(argv.next().unwrap_or_default()),
            "--waivers" => waivers = PathBuf::from(argv.next().unwrap_or_default()),
            "--today" => today = argv.next().unwrap_or_default(),
            other => eprintln!("tier-b-suites: ignoring unknown argument {other}"),
        }
    }
    Args {
        manifest,
        repo_root,
        waivers,
        today,
    }
}

fn main() -> ExitCode {
    let args = parse_args();
    let text = match std::fs::read_to_string(&args.manifest) {
        Ok(text) => text,
        Err(e) => {
            eprintln!(
                "tier-b-suites: cannot read {}: {e}",
                args.manifest.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let manifest = match Manifest::parse(&text) {
        Ok(manifest) => manifest,
        Err(e) => {
            eprintln!("tier-b-suites: manifest does not parse: {e}");
            return ExitCode::FAILURE;
        }
    };

    let waivers = match std::fs::read_to_string(&args.waivers) {
        Ok(text) => match WaiverFile::parse(&text) {
            Ok(file) => file.waivers,
            Err(e) => {
                eprintln!("tier-b-suites: waiver file does not parse: {e}");
                return ExitCode::FAILURE;
            }
        },
        Err(_) => Vec::new(),
    };
    if args.today.is_empty() {
        eprintln!(
            "tier-b-suites: --today YYYY-MM-DD is required — waiver expiry must be evaluated \
             against an injected date, never a clock this binary reads itself"
        );
        return ExitCode::FAILURE;
    }

    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    let mut total_skip = 0usize;
    let mut total_waived = 0usize;
    let mut failing_suites: Vec<String> = Vec::new();
    let mut stale_waivers: Vec<String> = Vec::new();

    let suites = tier_b_suites(&manifest);
    if suites.is_empty() {
        eprintln!("tier-b-suites: the manifest declares no Tier-B suites at all — that is a manifest bug, not a clean run");
        return ExitCode::FAILURE;
    }

    for suite in suites {
        let suite_id = &suite.id;
        // `screen_text` joined `six_thousand` here in W2-19. Leaving it on
        // the skip path would have traded one silent gap for another: it
        // is the protocol `cpu_dummy_reads` actually uses, and that ROM
        // had been waived as a hang precisely because nothing scored it.
        if !matches!(suite.protocol, Protocol::SixThousand | Protocol::ScreenText) {
            eprintln!(
                "tier-b-suites: {suite_id}: SKIP (protocol {:?} has no runner here)",
                suite.protocol
            );
            total_skip += suite.roms.len();
            continue;
        }

        let mut pass = 0usize;
        let mut fail = 0usize;
        let mut skip = 0usize;
        let mut waived = 0usize;
        for rom in &suite.roms {
            let Some(artifact) = manifest.artifacts.iter().find(|a| a.id == rom.artifact) else {
                eprintln!(
                    "tier-b-suites: {suite_id}/{}: artifact {} missing from manifest",
                    rom.rom, rom.artifact
                );
                fail += 1;
                continue;
            };
            let path = args.repo_root.join(&artifact.dest);
            if !path.is_file() {
                skip += 1;
                continue;
            }
            let waiver = waiver_for(&waivers, suite_id, &rom.rom);
            let (failed, detail) = match suite.protocol {
                Protocol::ScreenText => {
                    match blargg_evidence::run_screen_text(&path, rom.frame_budget) {
                        Ok(ScreenOutcome::Passed(_)) => (false, String::new()),
                        // A ROM that printed no verdict has not given one —
                        // scored as a failure, never folded into "passed",
                        // which is the same rule the evidence binary applies.
                        Ok(outcome) => (true, format!("{outcome:?}").replace('\n', " | ")),
                        Err(e) => (true, format!("ERROR {e}")),
                    }
                }
                _ => match blargg_evidence::run(&path, rom.frame_budget) {
                    Ok(outcome) if outcome.status == BlarggStatus::Passed => (false, String::new()),
                    Ok(outcome) => (
                        true,
                        format!(
                            "{:?} — {}",
                            outcome.status,
                            outcome.message.trim().replace('\n', " | ")
                        ),
                    ),
                    Err(e) => (true, format!("ERROR {e}")),
                },
            };

            match (failed, waiver) {
                (false, Some(w)) => {
                    // A waiver over a passing ROM hides the next regression
                    // in it, so it is an error in its own right.
                    pass += 1;
                    stale_waivers.push(format!(
                        "{suite_id}/{} passes but is still waived (ticket {}) — delete the \
                         waiver rather than leaving it to mask the next failure",
                        rom.rom, w.ticket
                    ));
                }
                (false, None) => pass += 1,
                (true, Some(w)) if args.today.as_str() <= w.expiry.as_str() => {
                    waived += 1;
                    println!(
                        "tier-b-suites: {suite_id}/{}: WAIVED until {} ({}) — {detail}",
                        rom.rom, w.expiry, w.ticket
                    );
                }
                (true, Some(w)) => {
                    fail += 1;
                    eprintln!(
                        "tier-b-suites: {suite_id}/{}: WAIVER EXPIRED {} (ticket {}) — {detail}",
                        rom.rom, w.expiry, w.ticket
                    );
                }
                (true, None) => {
                    fail += 1;
                    eprintln!("tier-b-suites: {suite_id}/{}: FAIL {detail}", rom.rom);
                }
            }
        }

        if skip == suite.roms.len() {
            println!("tier-b-suites: {suite_id}: SKIP (no ROMs fetched)");
        } else {
            println!(
                "tier-b-suites: {suite_id}: {pass} passed, {fail} failed, {waived} waived, \
                 {skip} not fetched"
            );
        }
        if fail > 0 {
            failing_suites.push(suite_id.clone());
        }
        total_pass += pass;
        total_fail += fail;
        total_skip += skip;
        total_waived += waived;
    }

    println!(
        "tier-b-suites: TOTAL {total_pass} passed, {total_fail} failed, {total_waived} waived, \
         {total_skip} not fetched"
    );
    for stale in &stale_waivers {
        eprintln!("tier-b-suites: STALE WAIVER — {stale}");
    }
    if !stale_waivers.is_empty() {
        return ExitCode::FAILURE;
    }
    if total_fail > 0 {
        eprintln!(
            "tier-b-suites: failing suites: {}",
            failing_suites.join(", ")
        );
        return ExitCode::FAILURE;
    }
    if total_pass == 0 && total_waived == 0 {
        // A run where nothing was fetched is not a pass — it is a nightly
        // that measured nothing, and saying so is the whole point of
        // counting skips separately.
        eprintln!("tier-b-suites: nothing ran (no ROMs fetched); this is not a pass");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
