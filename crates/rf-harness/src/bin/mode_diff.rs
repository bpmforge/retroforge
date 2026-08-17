//! Runs the accuracy-vs-compatibility diff (ticket W3-07; FR-MODE-004,
//! `docs/design/EMULATION_CORES.md` §5's "CI diffs both").
//!
//! See `rf_harness::mode_diff`'s module doc for what this enforces — in
//! particular why a run in which NOTHING diverges is a failure, not a
//! success.

use std::path::PathBuf;
use std::process::ExitCode;

use rf_harness::mode_diff::{self, DECLARED};
use rf_harness::Manifest;

fn main() -> ExitCode {
    let mut manifest_path = PathBuf::from("tests/rom-manifest.toml");
    let mut repo_root = PathBuf::from(".");
    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        match flag.as_str() {
            "--manifest" => manifest_path = PathBuf::from(argv.next().unwrap_or_default()),
            "--repo-root" => repo_root = PathBuf::from(argv.next().unwrap_or_default()),
            other => eprintln!("mode-diff: ignoring unknown argument {other}"),
        }
    }

    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("mode-diff: cannot read {}: {e}", manifest_path.display());
            return ExitCode::FAILURE;
        }
    };
    let manifest = match Manifest::parse(&text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("mode-diff: manifest does not parse: {e}");
            return ExitCode::FAILURE;
        }
    };

    let report = mode_diff::run(&manifest, &repo_root);

    for c in &report.compared {
        if c.diverged() {
            println!("mode-diff: DIVERGED {}/{}", c.suite, c.rom);
            println!("    accuracy:      {}", c.accuracy);
            println!("    compatibility: {}", c.compatibility);
        }
    }
    println!(
        "mode-diff: {} ROM(s) compared under both configs, {} diverged, {} not runnable/fetched",
        report.compared.len(),
        report.compared.iter().filter(|c| c.diverged()).count(),
        report.skipped
    );

    if report.compared.is_empty() {
        eprintln!(
            "mode-diff: nothing ran. A diff that compared no ROMs is not a clean diff — it is \
             the vacuous case this runner exists to refuse."
        );
        return ExitCode::FAILURE;
    }

    for c in &report.undeclared {
        eprintln!(
            "mode-diff: UNDECLARED DIVERGENCE {}/{} — compatibility may only differ where a \
             DeclaredDivergence says it may (rf_harness::mode_diff::DECLARED)",
            c.suite, c.rom
        );
    }
    for stale in &report.stale {
        eprintln!("mode-diff: STALE DECLARATION — {stale}");
    }

    if report.is_clean() {
        println!(
            "mode-diff OK — {} declared divergence(s), all still observable",
            DECLARED.len()
        );
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
