//! Mirror-list fetch + hash verification (ticket W0-03; NFR-006,
//! FAILURE_MODES.md FM-14).
//!
//! Deliberately has **no archive-unpack step**. Every [`crate::manifest::Artifact`]
//! is one file; the fetcher writes exactly the bytes it verified to `dest`
//! and stops. Where an upstream source is naturally a whole-repo archive
//! (e.g. SingleStepTests' hundreds of per-opcode JSON files), the manifest
//! records the archive itself as the artifact — this crate does not
//! extract it. Reasons: (1) extraction needs either a new `zip` crate
//! dependency (a whole new TECH_STACK decision, unverified API) or
//! shelling out to `unzip`, which is not guaranteed present on minimal CI
//! images; (2) nothing in this ticket's scope consumes the extracted form
//! — that's a future ticket (rf-nes/rf-snes's own vector-test runner).
//!
//! The real network transport ([`CurlDownloader`]) shells out to the
//! system `curl` rather than pulling in an HTTP client crate — `curl` is
//! already required by project law (macOS ships it; every CI image used
//! here has it) and is not unit-testable IO, so it stays a thin,
//! deliberately-untested edge. All the logic that *is* unit tested
//! ([`fetch_artifact`]) is decoupled from it via the [`Downloader`] trait.

use crate::manifest::Artifact;
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
        let dir = std::env::temp_dir().join(format!(
            "rf-harness-fetch-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
