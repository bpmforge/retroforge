//! `fetch-test-roms` — the binary `scripts/fetch-test-roms.sh` wraps.
//! Reads `tests/rom-manifest.toml` and fetches every artifact of either
//! kind into gitignored `roms/`: `[[artifact]]` entries with a real
//! (non-placeholder) `sha256`, verifying hashes and trying mirrors in
//! order (FM-14); `[[git_artifact]]` entries (ticket W0-07) via a pinned
//! commit + sparse checkout, verified by `git rev-parse HEAD` rather than
//! a hash. Placeholder-hash `[[artifact]]` entries are skipped with a
//! warning, not treated as failures — see `rf_harness::manifest` module
//! doc for why some entries are still `TODO-`.
//!
//! Usage: `fetch-test-roms [--manifest PATH] [--repo-root PATH] [ID...]`
//! With no `ID` arguments, fetches every real-hash artifact and every
//! git_artifact in the manifest. With one or more `ID` arguments, fetches
//! only those (ids may name either kind — they share one namespace, see
//! `Manifest::validate`); unknown ids are a loud error, not a silent no-op.

use rf_harness::{fetch_artifact, fetch_git_artifact, CurlDownloader, Manifest, SystemGitRunner};
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    let mut manifest_path = PathBuf::from("tests/rom-manifest.toml");
    let mut repo_root = PathBuf::from(".");
    let mut requested_ids: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--manifest" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("--manifest requires a path argument");
                    return ExitCode::FAILURE;
                };
                manifest_path = PathBuf::from(v);
            }
            "--repo-root" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("--repo-root requires a path argument");
                    return ExitCode::FAILURE;
                };
                repo_root = PathBuf::from(v);
            }
            other => requested_ids.push(other.to_string()),
        }
        i += 1;
    }

    run(&manifest_path, &repo_root, &requested_ids)
}

fn run(manifest_path: &Path, repo_root: &Path, requested_ids: &[String]) -> ExitCode {
    let text = match std::fs::read_to_string(manifest_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("failed to read manifest {}: {e}", manifest_path.display());
            return ExitCode::FAILURE;
        }
    };

    let manifest = match Manifest::parse(&text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("failed to parse manifest {}: {e}", manifest_path.display());
            return ExitCode::FAILURE;
        }
    };

    if let Err(errors) = manifest.validate() {
        eprintln!("manifest {} failed validation:", manifest_path.display());
        for e in &errors {
            eprintln!("  - {e}");
        }
        return ExitCode::FAILURE;
    }

    // Ids share one namespace across [[artifact]] and [[git_artifact]]
    // (Manifest::validate enforces uniqueness) — resolve each requested id
    // against whichever kind actually declares it.
    let (plain_targets, git_targets): (Vec<&rf_harness::Artifact>, Vec<&rf_harness::GitArtifact>) =
        if requested_ids.is_empty() {
            (
                manifest.artifacts.iter().collect(),
                manifest.git_artifacts.iter().collect(),
            )
        } else {
            let mut plain = Vec::new();
            let mut git = Vec::new();
            let mut unknown = Vec::new();
            for id in requested_ids {
                match (manifest.artifact(id), manifest.git_artifact(id)) {
                    (Some(a), None) => plain.push(a),
                    (None, Some(g)) => git.push(g),
                    (None, None) => unknown.push(id.clone()),
                    (Some(_), Some(_)) => unreachable!(
                        "Manifest::validate rejects duplicate ids across artifact kinds"
                    ),
                }
            }
            if !unknown.is_empty() {
                eprintln!("unknown artifact id(s): {}", unknown.join(", "));
                return ExitCode::FAILURE;
            }
            (plain, git)
        };

    let downloader = CurlDownloader;
    let git_runner = SystemGitRunner;
    let mut fetched = 0usize;
    let mut skipped_placeholder = 0usize;
    let mut failed = 0usize;

    for artifact in plain_targets {
        if artifact.is_hash_placeholder() {
            println!(
                "SKIP  {} (placeholder sha256: {})",
                artifact.id, artifact.sha256
            );
            skipped_placeholder += 1;
            continue;
        }
        match fetch_artifact(artifact, &downloader, repo_root) {
            Ok(path) => {
                println!("OK    {} -> {}", artifact.id, path.display());
                fetched += 1;
            }
            Err(e) => {
                eprintln!("FAIL  {e}");
                failed += 1;
            }
        }
    }

    for git_artifact in git_targets {
        match fetch_git_artifact(git_artifact, &git_runner, repo_root) {
            Ok(path) => {
                println!("OK    {} -> {}", git_artifact.id, path.display());
                fetched += 1;
            }
            Err(e) => {
                eprintln!("FAIL  {e}");
                failed += 1;
            }
        }
    }

    println!(
        "\n{fetched} fetched, {skipped_placeholder} skipped (placeholder hash), {failed} failed"
    );

    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
