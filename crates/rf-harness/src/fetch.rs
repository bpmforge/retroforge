//! Mirror-list fetch + hash verification (ticket W0-03; NFR-006,
//! FAILURE_MODES.md FM-14), plus pinned-commit git-subpath checkout
//! (ticket W0-07).
//!
//! [`crate::manifest::Artifact`] (fetched by [`fetch_artifact`])
//! deliberately has **no archive-unpack step**: every `Artifact` is one
//! file, and the fetcher writes exactly the bytes it verified to `dest`
//! and stops. **Resolved 2026-08-07 (ticket W2-11):** this paragraph used
//! to say extraction would need "a new `zip` crate dependency (a whole new
//! TECH_STACK decision, unverified API)" — that blocker no longer exists.
//! `zip 8.6.0` was adopted by ticket W2-13 (`docs/TECH_STACK.md` §2's
//! Archive row) for `crates/retroforge`'s ROM-open path
//! (`crates/retroforge/src/rom_open.rs::resolve_rom_bytes`), and this
//! crate's own `tests/alter_ego_replay.rs` (ticket W2-11) now uses it too,
//! as a `[dev-dependencies]` entry, to unpack the fetched `alter_ego.zip`
//! for the 5-minute replay regression. Despite that, [`fetch_artifact`]
//! staying unpack-free is still the right design, for a different reason
//! than "no viable dependency exists": keeping "verify these bytes hash to
//! `sha256`" and "interpret this archive's contents" as two separate steps
//! means a hash-verification bug can never be silently papered over by
//! extraction logic, and lets each unpack call site (the ROM-open dialog,
//! this crate's replay test, any future one) choose its own
//! untrusted-vs-already-pinned-content posture rather than baking one
//! policy into the fetcher for every future archive artifact.
//!
//! [`crate::manifest::GitArtifact`] (fetched by [`fetch_git_artifact`],
//! ticket W0-07) is the one deliberate exception to "every artifact is one
//! file": its `dest` is a directory — a `git sparse-checkout` of one
//! subpath at one pinned commit. This isn't the archive-unpack step the
//! paragraph above says this crate doesn't have; `git` itself is the
//! selection mechanism (`sparse-checkout set --cone <subpath>`), not a
//! zip/tar extractor this crate would need to implement or depend on, and
//! the result is verified by `git rev-parse HEAD` equaling the pinned
//! commit rather than a content hash this crate computes.
//!
//! The real network transport ([`CurlDownloader`]) shells out to the
//! system `curl` rather than pulling in an HTTP client crate — `curl` is
//! already required by project law (macOS ships it; every CI image used
//! here has it) and is not unit-testable IO, so it stays a thin,
//! deliberately-untested edge. All the logic that *is* unit tested
//! ([`fetch_artifact`]) is decoupled from it via the [`Downloader`] trait.
//! [`fetch_git_artifact`] follows the identical shape with [`GitRunner`]
//! standing in for `Downloader` and [`SystemGitRunner`] shelling out to
//! the system `git` (also already required by project law).

use crate::manifest::{Artifact, GitArtifact};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Abstracts "fetch these bytes from this URL" so [`fetch_artifact`] is
/// testable without real network access.
pub trait Downloader {
    /// Fetch `url`, returning the raw bytes or a human-readable error.
    ///
    /// # Errors
    /// Returns `Err` with a diagnostic message if the URL cannot be
    /// reached or the transport fails.
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Production [`Downloader`]: shells out to `curl -fsSL <url>` and
/// captures stdout directly (binary-safe — no temp file, no shasum
/// subprocess; hashing is done in Rust via [`hex_sha256`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct CurlDownloader;

impl Downloader for CurlDownloader {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let output = Command::new("curl")
            .args(["-fsSL", url])
            .output()
            .map_err(|e| format!("failed to spawn curl: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "curl exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(output.stdout)
    }
}

/// Why one mirror attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptFailure {
    /// The URL could not be fetched at all.
    Unreachable(String),
    /// The URL was fetched but its bytes don't hash to the artifact's
    /// declared `sha256` — see [`crate::manifest`] module doc for why this
    /// is treated as a distinct, nameable failure rather than lumped in
    /// with "unreachable".
    HashMismatch { expected: String, actual: String },
}

impl fmt::Display for AttemptFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttemptFailure::Unreachable(msg) => write!(f, "unreachable: {msg}"),
            AttemptFailure::HashMismatch { expected, actual } => {
                write!(f, "hash mismatch: expected {expected}, got {actual}")
            }
        }
    }
}

/// One failed mirror attempt, kept for the loud FM-14 diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchAttempt {
    pub url: String,
    pub failure: AttemptFailure,
}

/// [`fetch_artifact`] failure modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// Every mirror in [`Artifact::mirrors`] failed (unreachable or wrong
    /// hash). FM-14: "CI job fails loudly naming the artifact".
    AllMirrorsFailed {
        artifact_id: String,
        attempts: Vec<FetchAttempt>,
    },
    /// [`Artifact::sha256`] is a `TODO-` placeholder — there is nothing to
    /// verify against, so fetching is refused rather than silently
    /// trusting unverified bytes.
    PlaceholderHash {
        artifact_id: String,
        placeholder: String,
    },
    /// The verified bytes could not be written to `dest`.
    Write {
        artifact_id: String,
        message: String,
    },
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::AllMirrorsFailed {
                artifact_id,
                attempts,
            } => {
                writeln!(
                    f,
                    "artifact '{artifact_id}': all {} mirror(s) failed:",
                    attempts.len()
                )?;
                for a in attempts {
                    writeln!(f, "  - {}: {}", a.url, a.failure)?;
                }
                Ok(())
            }
            FetchError::PlaceholderHash {
                artifact_id,
                placeholder,
            } => write!(
                f,
                "artifact '{artifact_id}': sha256 is a placeholder ({placeholder}), refusing to fetch unverifiable bytes"
            ),
            FetchError::Write {
                artifact_id,
                message,
            } => write!(f, "artifact '{artifact_id}': failed to write dest: {message}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Hex-encode a SHA-256 digest, lowercase (matches `crates/rf-cart/src/hash.rs`'s
/// approach: RustCrypto 0.11's `finalize()` returns `Array<u8, N>`, which
/// does not implement `LowerHex`, so `format!("{:x}", ...)` does not
/// compile — hex-encode by hand instead).
#[must_use]
pub fn hex_sha256(data: &[u8]) -> String {
    use std::fmt::Write;
    let digest = Sha256::digest(data);
    digest.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Fetch `artifact`, trying [`Artifact::mirrors`] in order, verifying each
/// download's SHA-256 against [`Artifact::sha256`], and writing the first
/// match to `<repo_root>/<artifact.dest>`.
///
/// # Errors
/// Returns [`FetchError::PlaceholderHash`] immediately if the artifact has
/// no real hash to verify against. Returns
/// [`FetchError::AllMirrorsFailed`] naming every attempted URL and why it
/// failed (FM-14) if no mirror produces matching bytes. Returns
/// [`FetchError::Write`] if the verified bytes can't be written to disk.
pub fn fetch_artifact(
    artifact: &Artifact,
    downloader: &dyn Downloader,
    repo_root: &Path,
) -> Result<PathBuf, FetchError> {
    if artifact.is_hash_placeholder() {
        return Err(FetchError::PlaceholderHash {
            artifact_id: artifact.id.clone(),
            placeholder: artifact.sha256.clone(),
        });
    }

    let mut attempts = Vec::new();
    for url in &artifact.mirrors {
        match downloader.get(url) {
            Err(msg) => attempts.push(FetchAttempt {
                url: url.clone(),
                failure: AttemptFailure::Unreachable(msg),
            }),
            Ok(bytes) => {
                let actual = hex_sha256(&bytes);
                if actual == artifact.sha256 {
                    let dest_path = repo_root.join(&artifact.dest);
                    return write_verified(&artifact.id, &dest_path, &bytes).map(|()| dest_path);
                }
                attempts.push(FetchAttempt {
                    url: url.clone(),
                    failure: AttemptFailure::HashMismatch {
                        expected: artifact.sha256.clone(),
                        actual,
                    },
                });
            }
        }
    }

    Err(FetchError::AllMirrorsFailed {
        artifact_id: artifact.id.clone(),
        attempts,
    })
}

fn write_verified(artifact_id: &str, dest_path: &Path, bytes: &[u8]) -> Result<(), FetchError> {
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent).map_err(|e| FetchError::Write {
            artifact_id: artifact_id.to_string(),
            message: e.to_string(),
        })?;
    }
    fs::write(dest_path, bytes).map_err(|e| FetchError::Write {
        artifact_id: artifact_id.to_string(),
        message: e.to_string(),
    })
}

/// Abstracts "run `git` with these arguments", so [`fetch_git_artifact`]
/// (and the small `git_rev_parse_head`/`git_tree_is_clean` helpers below)
/// are testable without a real git binary or network access — the same
/// role [`Downloader`] plays for [`fetch_artifact`].
pub trait GitRunner {
    /// Run `git <args>`, returning trimmed stdout on success or a
    /// human-readable message (stderr, or the spawn error) on failure.
    ///
    /// # Errors
    /// Returns `Err` if the process can't be spawned or exits non-zero.
    fn run(&self, args: &[&str]) -> Result<String, String>;
}

/// Production [`GitRunner`]: shells out to the system `git` binary.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemGitRunner;

impl GitRunner for SystemGitRunner {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new("git")
            .args(args)
            .output()
            .map_err(|e| format!("failed to spawn git {}: {e}", args.join(" ")))?;
        if !output.status.success() {
            return Err(format!(
                "git {} exited with {}: {}",
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

/// [`fetch_git_artifact`] failure modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitFetchError {
    /// One of the `git` invocations that builds the checkout failed.
    CommandFailed {
        artifact_id: String,
        step: &'static str,
        message: String,
    },
    /// The fresh checkout's `git rev-parse HEAD` didn't equal
    /// [`GitArtifact::commit`] — the integrity check (module doc) failed.
    IntegrityCheckFailed {
        artifact_id: String,
        expected: String,
        actual: String,
    },
    /// `dest` already exists (module doc: pre-existing checkouts are
    /// never deleted or re-cloned) but is at the wrong commit, or isn't a
    /// git repository at all — either way this is a hard error, not
    /// something this function will silently "fix" by mutating `dest`.
    ExistingDestMismatch {
        artifact_id: String,
        dest: PathBuf,
        detail: String,
    },
}

impl fmt::Display for GitFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitFetchError::CommandFailed {
                artifact_id,
                step,
                message,
            } => write!(
                f,
                "git_artifact '{artifact_id}': {step} failed: {message}"
            ),
            GitFetchError::IntegrityCheckFailed {
                artifact_id,
                expected,
                actual,
            } => write!(
                f,
                "git_artifact '{artifact_id}': integrity check failed: rev-parse HEAD = {actual}, want {expected}"
            ),
            GitFetchError::ExistingDestMismatch {
                artifact_id,
                dest,
                detail,
            } => write!(
                f,
                "git_artifact '{artifact_id}': existing dest {} does not match the pinned commit and was left untouched: {detail}",
                dest.display()
            ),
        }
    }
}

impl std::error::Error for GitFetchError {}

/// Fetch `git_artifact`, writing (or verifying an already-correct)
/// checkout at `<repo_root>/<git_artifact.dest>`.
///
/// If `dest` already exists, it is **only ever read, never deleted or
/// re-cloned or otherwise mutated**: this function runs `git -C <dest>
/// rev-parse HEAD` and either returns `Ok` immediately (already at the
/// pinned commit — nothing to do) or [`GitFetchError::ExistingDestMismatch`]
/// (wrong commit, or not a git repository) without touching `dest` at
/// all. Only when `dest` doesn't exist yet does this perform the fresh
/// `init`/`remote add`/`sparse-checkout`/`fetch`/`checkout` sequence, each
/// step exactly as verified live against a real upstream repo (see
/// `plan.json` ticket W0-07 pre-flight notes), followed by the same
/// `rev-parse HEAD` integrity check.
///
/// # Errors
/// See [`GitFetchError`].
pub fn fetch_git_artifact(
    git_artifact: &GitArtifact,
    runner: &dyn GitRunner,
    repo_root: &Path,
) -> Result<PathBuf, GitFetchError> {
    let dest = repo_root.join(&git_artifact.dest);
    let dest_str = dest.to_string_lossy().into_owned();

    if dest.is_dir() {
        return match runner.run(&["-C", &dest_str, "rev-parse", "HEAD"]) {
            Ok(head) if head == git_artifact.commit => Ok(dest),
            Ok(head) => Err(GitFetchError::ExistingDestMismatch {
                artifact_id: git_artifact.id.clone(),
                dest,
                detail: format!(
                    "at commit {head}, want {} — refusing to delete/re-clone",
                    git_artifact.commit
                ),
            }),
            Err(message) => Err(GitFetchError::ExistingDestMismatch {
                artifact_id: git_artifact.id.clone(),
                dest,
                detail: format!(
                    "not a readable git repository ({message}) — refusing to delete/re-clone"
                ),
            }),
        };
    }

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| GitFetchError::CommandFailed {
            artifact_id: git_artifact.id.clone(),
            step: "mkdir parent",
            message: e.to_string(),
        })?;
    }

    let run = |step: &'static str, args: &[&str]| -> Result<String, GitFetchError> {
        runner
            .run(args)
            .map_err(|message| GitFetchError::CommandFailed {
                artifact_id: git_artifact.id.clone(),
                step,
                message,
            })
    };

    run("init", &["init", "-q", &dest_str])?;
    run(
        "remote add",
        &[
            "-C",
            &dest_str,
            "remote",
            "add",
            "origin",
            &git_artifact.repo,
        ],
    )?;
    run(
        "sparse-checkout set",
        &[
            "-C",
            &dest_str,
            "sparse-checkout",
            "set",
            "--cone",
            &git_artifact.subpath,
        ],
    )?;
    run(
        "fetch",
        &[
            "-C",
            &dest_str,
            "fetch",
            "--depth",
            "1",
            "--filter=blob:none",
            "origin",
            &git_artifact.commit,
        ],
    )?;
    run(
        "checkout",
        &["-C", &dest_str, "checkout", "-q", "FETCH_HEAD"],
    )?;
    let head = run("rev-parse HEAD", &["-C", &dest_str, "rev-parse", "HEAD"])?;

    if head != git_artifact.commit {
        return Err(GitFetchError::IntegrityCheckFailed {
            artifact_id: git_artifact.id.clone(),
            expected: git_artifact.commit.clone(),
            actual: head,
        });
    }

    Ok(dest)
}

/// `git -C <dir> rev-parse HEAD`, trimmed. Shared by [`fetch_git_artifact`]
/// and the local-gate evidence generator (ticket W0-07), which uses it to
/// record which commit of a directory (the repo root itself, or a fetched
/// vector-source checkout) was actually exercised.
///
/// # Errors
/// Returns `Err` if `dir` isn't inside a git repository or the process
/// can't be run.
pub fn git_rev_parse_head(runner: &dyn GitRunner, dir: &Path) -> Result<String, String> {
    runner.run(&["-C", &dir.to_string_lossy(), "rev-parse", "HEAD"])
}

/// `true` iff `git -C <dir> status --porcelain` prints nothing, i.e. the
/// working tree is clean. The local-gate evidence generator (ticket
/// W0-07) records this — never a wall-clock mtime — so evidence generated
/// against uncommitted changes can't silently claim authority for a
/// `retroforge_commit` value it doesn't actually reflect.
///
/// # Errors
/// Returns `Err` if `dir` isn't inside a git repository or the process
/// can't be run.
pub fn git_tree_is_clean(runner: &dyn GitRunner, dir: &Path) -> Result<bool, String> {
    let out = runner.run(&["-C", &dir.to_string_lossy(), "status", "--porcelain"])?;
    Ok(out.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::LicenseStatus;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Test double: maps URL -> canned response, and records every URL it
    /// was asked to fetch (in order) so tests can assert mirror ordering.
    struct FakeDownloader {
        responses: HashMap<String, Result<Vec<u8>, String>>,
        requested: RefCell<Vec<String>>,
    }

    impl FakeDownloader {
        fn new(responses: HashMap<String, Result<Vec<u8>, String>>) -> Self {
            FakeDownloader {
                responses,
                requested: RefCell::new(Vec::new()),
            }
        }
    }

    impl Downloader for FakeDownloader {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            self.requested.borrow_mut().push(url.to_string());
            self.responses
                .get(url)
                .cloned()
                .unwrap_or_else(|| Err(format!("no fake response configured for {url}")))
        }
    }

    fn artifact(mirrors: Vec<&str>, sha256: &str, dest: &str) -> Artifact {
        Artifact {
            id: "test-artifact".into(),
            license_status: LicenseStatus::PublicDomain,
            sha256: sha256.into(),
            mirrors: mirrors.into_iter().map(String::from).collect(),
            dest: dest.into(),
        }
    }

    #[test]
    fn fetches_and_writes_on_first_matching_mirror() {
        let bytes = b"hello world".to_vec();
        let expected_hash = hex_sha256(&bytes);
        let a = artifact(
            vec!["https://a.invalid/f"],
            &expected_hash,
            "roms/nes/f.bin",
        );
        let dl = FakeDownloader::new(HashMap::from([(
            "https://a.invalid/f".to_string(),
            Ok(bytes.clone()),
        )]));
        let tmp = tempdir();

        let dest = fetch_artifact(&a, &dl, &tmp).expect("fetch should succeed");
        assert_eq!(dest, tmp.join("roms/nes/f.bin"));
        assert_eq!(fs::read(&dest).unwrap(), bytes);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn falls_through_to_second_mirror_when_first_is_unreachable() {
        let bytes = b"payload".to_vec();
        let expected_hash = hex_sha256(&bytes);
        let a = artifact(
            vec!["https://down.invalid/f", "https://up.invalid/f"],
            &expected_hash,
            "roms/nes/f.bin",
        );
        let dl = FakeDownloader::new(HashMap::from([
            (
                "https://down.invalid/f".to_string(),
                Err("connection refused".to_string()),
            ),
            ("https://up.invalid/f".to_string(), Ok(bytes.clone())),
        ]));
        let tmp = tempdir();

        let dest = fetch_artifact(&a, &dl, &tmp).expect("second mirror should succeed");
        assert_eq!(fs::read(dest).unwrap(), bytes);
        assert_eq!(
            *dl.requested.borrow(),
            vec!["https://down.invalid/f", "https://up.invalid/f"]
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// "hash mismatch rejected" (METHOD step 4 test requirement): a mirror
    /// that answers but serves the wrong bytes must not be accepted, and
    /// the failure must say *why* (not just "unreachable").
    #[test]
    fn rejects_mirror_with_wrong_hash() {
        let bytes = b"wrong bytes".to_vec();
        let a = artifact(
            vec!["https://a.invalid/f"],
            "0000000000000000000000000000000000000000000000000000000000000000", // never matches
            "roms/nes/f.bin",
        );
        let dl = FakeDownloader::new(HashMap::from([(
            "https://a.invalid/f".to_string(),
            Ok(bytes),
        )]));
        let tmp = tempdir();

        let err = fetch_artifact(&a, &dl, &tmp).unwrap_err();
        match err {
            FetchError::AllMirrorsFailed {
                artifact_id,
                attempts,
            } => {
                assert_eq!(artifact_id, "test-artifact");
                assert_eq!(attempts.len(), 1);
                assert!(matches!(
                    attempts[0].failure,
                    AttemptFailure::HashMismatch { .. }
                ));
            }
            other => panic!("expected AllMirrorsFailed, got {other:?}"),
        }

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// "all-mirrors-fail is loud and names the artifact" (METHOD step 4).
    #[test]
    fn all_mirrors_failing_names_the_artifact_and_every_attempt() {
        let a = artifact(
            vec!["https://a.invalid/f", "https://b.invalid/f"],
            "f".repeat(64).as_str(),
            "roms/nes/f.bin",
        );
        let dl = FakeDownloader::new(HashMap::from([
            (
                "https://a.invalid/f".to_string(),
                Err("timed out".to_string()),
            ),
            (
                "https://b.invalid/f".to_string(),
                Err("dns failure".to_string()),
            ),
        ]));
        let tmp = tempdir();

        let err = fetch_artifact(&a, &dl, &tmp).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("test-artifact"), "message: {msg}");
        assert!(msg.contains("https://a.invalid/f"), "message: {msg}");
        assert!(msg.contains("https://b.invalid/f"), "message: {msg}");
        assert!(msg.contains("timed out"), "message: {msg}");
        assert!(msg.contains("dns failure"), "message: {msg}");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn refuses_to_fetch_placeholder_hash() {
        let a = artifact(
            vec!["https://a.invalid/f"],
            "TODO-network-unreachable",
            "roms/nes/f.bin",
        );
        let dl = FakeDownloader::new(HashMap::new());
        let tmp = tempdir();

        let err = fetch_artifact(&a, &dl, &tmp).unwrap_err();
        assert!(matches!(err, FetchError::PlaceholderHash { .. }));
        // No network call should have been attempted at all.
        assert!(dl.requested.borrow().is_empty());

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn hex_sha256_matches_known_vector() {
        // Same known-answer vector rf-cart's hash.rs uses for its hex
        // helper, so both crates' independent hex-encoding shims are
        // checked against the same ground truth.
        assert_eq!(
            hex_sha256(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    fn tempdir() -> PathBuf {
        // Tests in one binary run in PARALLEL THREADS sharing a process
        // id, and two threads can observe the same `SystemTime` tick, so
        // pid+nanos alone is NOT unique — a collision makes one test's
        // cleanup delete another's working directory. Observed twice as a
        // one-off flake during W1-03. The atomic counter closes the race.
        static TEMPDIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rf-harness-fetch-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            TEMPDIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Test double for [`GitRunner`]: a fixed queue of canned responses,
    /// consumed in call order (the real sequence is always linear), and a
    /// record of every `args` list it was asked to run so tests can assert
    /// both "did the right commands run" and — just as important for the
    /// "never disturb an existing checkout" invariant — "did NO further
    /// commands run".
    struct ScriptedGitRunner {
        responses: RefCell<std::collections::VecDeque<Result<String, String>>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl ScriptedGitRunner {
        fn new(responses: Vec<Result<&str, &str>>) -> Self {
            ScriptedGitRunner {
                responses: RefCell::new(
                    responses
                        .into_iter()
                        .map(|r| r.map(str::to_string).map_err(str::to_string))
                        .collect(),
                ),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    impl GitRunner for ScriptedGitRunner {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            self.responses
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Err("ScriptedGitRunner: no more canned responses".to_string()))
        }
    }

    fn git_artifact(dest: &str, commit: &str) -> GitArtifact {
        GitArtifact {
            id: "test-git-artifact".into(),
            license_status: LicenseStatus::NoLicenseGrantFetchOnly,
            repo: "https://example.invalid/repo.git".into(),
            commit: commit.into(),
            subpath: "sub/dir".into(),
            dest: dest.into(),
        }
    }

    #[test]
    fn fresh_clone_issues_the_documented_command_sequence_and_verifies_head() {
        let commit = "b".repeat(40);
        let runner = ScriptedGitRunner::new(vec![
            Ok(""),      // init
            Ok(""),      // remote add
            Ok(""),      // sparse-checkout set
            Ok(""),      // fetch
            Ok(""),      // checkout
            Ok(&commit), // rev-parse HEAD
        ]);
        let tmp = tempdir();
        let a = git_artifact("roms/nes/g1-src", &commit);

        let dest = fetch_git_artifact(&a, &runner, &tmp).expect("fresh clone should succeed");
        assert_eq!(dest, tmp.join("roms/nes/g1-src"));

        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[0][0], "init");
        assert_eq!(calls[1][2], "remote");
        assert_eq!(calls[2][2], "sparse-checkout");
        assert_eq!(calls[3][2], "fetch");
        assert_eq!(calls[4][2], "checkout");
        assert_eq!(calls[5][2], "rev-parse");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn integrity_check_failure_is_reported_when_head_does_not_match_pinned_commit() {
        let commit = "c".repeat(40);
        let wrong_head = "d".repeat(40);
        let runner = ScriptedGitRunner::new(vec![
            Ok(""),
            Ok(""),
            Ok(""),
            Ok(""),
            Ok(""),
            Ok(&wrong_head),
        ]);
        let tmp = tempdir();
        let a = git_artifact("roms/nes/g1-src", &commit);

        let err = fetch_git_artifact(&a, &runner, &tmp).unwrap_err();
        match err {
            GitFetchError::IntegrityCheckFailed {
                expected, actual, ..
            } => {
                assert_eq!(expected, commit);
                assert_eq!(actual, wrong_head);
            }
            other => panic!("expected IntegrityCheckFailed, got {other:?}"),
        }

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn command_failure_names_the_failing_step() {
        let commit = "e".repeat(40);
        let runner = ScriptedGitRunner::new(vec![
            Ok(""),                             // init
            Ok(""),                             // remote add
            Err("cone mode rejected the path"), // sparse-checkout set
        ]);
        let tmp = tempdir();
        let a = git_artifact("roms/nes/g1-src", &commit);

        let err = fetch_git_artifact(&a, &runner, &tmp).unwrap_err();
        match err {
            GitFetchError::CommandFailed { step, message, .. } => {
                assert_eq!(step, "sparse-checkout set");
                assert!(message.contains("cone mode rejected the path"));
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
        assert_eq!(runner.call_count(), 3);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn existing_dest_at_correct_commit_short_circuits_without_further_commands() {
        let commit = "f".repeat(40);
        let tmp = tempdir();
        let dest = tmp.join("roms/nes/g1-src");
        fs::create_dir_all(&dest).unwrap();
        let runner = ScriptedGitRunner::new(vec![Ok(&commit)]); // rev-parse HEAD only
        let a = git_artifact("roms/nes/g1-src", &commit);

        let result = fetch_git_artifact(&a, &runner, &tmp).expect("already-correct dest is Ok");
        assert_eq!(result, dest);
        // Exactly one command (the verifying rev-parse) — no init/remote/
        // fetch/checkout ever ran against a directory that already existed.
        assert_eq!(runner.call_count(), 1);
        assert_eq!(runner.calls.borrow()[0][2], "rev-parse");

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The "never disturb an existing checkout" invariant (module doc):
    /// a dest that exists but is at the WRONG commit must be reported as
    /// an error, and — just as important — must not trigger any
    /// mutating command (no delete, no re-init, no re-fetch).
    #[test]
    fn existing_dest_at_wrong_commit_errors_and_never_mutates() {
        let pinned = "1".repeat(40);
        let actual = "2".repeat(40);
        let tmp = tempdir();
        let dest = tmp.join("roms/nes/g1-src");
        fs::create_dir_all(&dest).unwrap();
        let runner = ScriptedGitRunner::new(vec![Ok(&actual)]);
        let a = git_artifact("roms/nes/g1-src", &pinned);

        let err = fetch_git_artifact(&a, &runner, &tmp).unwrap_err();
        assert!(matches!(err, GitFetchError::ExistingDestMismatch { .. }));
        assert_eq!(
            runner.call_count(),
            1,
            "must never issue a mutating command against a pre-existing wrong-commit dest"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn existing_dest_that_is_not_a_git_repo_errors_and_never_mutates() {
        let tmp = tempdir();
        let dest = tmp.join("roms/nes/g1-src");
        fs::create_dir_all(&dest).unwrap();
        let runner = ScriptedGitRunner::new(vec![Err("not a git repository")]);
        let a = git_artifact("roms/nes/g1-src", &"3".repeat(40));

        let err = fetch_git_artifact(&a, &runner, &tmp).unwrap_err();
        assert!(matches!(err, GitFetchError::ExistingDestMismatch { .. }));
        assert_eq!(runner.call_count(), 1);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn git_rev_parse_head_forwards_the_runners_output() {
        // The trim contract lives on `GitRunner::run` itself (SystemGitRunner
        // trims); this just checks git_rev_parse_head builds the right
        // argv and passes the result through unmodified.
        let runner = ScriptedGitRunner::new(vec![Ok("abc123")]);
        let dir = PathBuf::from("/some/dir");
        assert_eq!(git_rev_parse_head(&runner, &dir).unwrap(), "abc123");
        assert_eq!(runner.calls.borrow()[0][2], "rev-parse");
    }

    #[test]
    fn git_tree_is_clean_true_when_porcelain_output_empty() {
        let runner = ScriptedGitRunner::new(vec![Ok("")]);
        let dir = PathBuf::from("/some/dir");
        assert!(git_tree_is_clean(&runner, &dir).unwrap());
    }

    #[test]
    fn git_tree_is_clean_false_when_porcelain_output_nonempty() {
        let runner = ScriptedGitRunner::new(vec![Ok(" M some/file.rs\n")]);
        let dir = PathBuf::from("/some/dir");
        assert!(!git_tree_is_clean(&runner, &dir).unwrap());
    }
}
