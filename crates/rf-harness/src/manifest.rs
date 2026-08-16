//! `tests/rom-manifest.toml` schema and validation (ticket W0-03; NFR-006,
//! TESTING.md §3, FAILURE_MODES.md FM-14).
//!
//! See the header comment in `tests/rom-manifest.toml` itself for the full
//! schema description — this module is the executable form of that
//! contract. The load-bearing design decision (verified against a real
//! counterexample this ticket found: qmtpro.com's and christopherpow's
//! copies of `nestest.log` differ by line-ending normalization alone) is
//! that `sha256` lives on the [`Artifact`], not on each mirror URL: a
//! mirror is only a mirror if it serves the exact bytes the artifact's one
//! hash names. Splitting the hash per-mirror would let a stale or
//! re-encoded "mirror" silently verify against itself, defeating FM-14.
//!
//! ## `[[git_artifact]]` (ticket W0-07)
//!
//! A second, deliberately separate table array for artifacts that are a
//! pinned commit of a *sparse subpath* of a git repo rather than one
//! fetchable file — e.g. `SingleStepTests/ProcessorTests`' `nes6502/v1`
//! directory (256 JSON files), which replaces the old
//! `singlestep-nes6502` whole-repo-zip [`Artifact`] entry: codeload zips
//! stream without a `Content-Length` header, so a multi-GB transfer can
//! truncate silently into bytes that still unzip and still "look like" a
//! ROM-test archive — exactly the FM-14 shape [`Artifact::sha256`] exists
//! to catch, except an archive sha256 for a whole-repo snapshot is itself
//! fragile (GitHub's generated zips are not byte-stable across requests
//! for the same commit — verified empirically at W1-01a/b).
//!
//! [`GitArtifact`] has **no `sha256` field** — this is intentional, not an
//! oversight, and not a weakening of [`Artifact`]'s invariant (that
//! struct, and `fetch_artifact`'s [`crate::fetch::FetchError::PlaceholderHash`]
//! refusal, are both untouched). A git commit SHA already *is* a content
//! hash: `git -C <dest> rev-parse HEAD` equaling [`GitArtifact::commit`]
//! after a `--filter=blob:none` sparse fetch proves the checked-out tree
//! is bit-identical to what that commit names, for exactly the same
//! reason a Merkle-tree hash proves it — see
//! [`crate::fetch::fetch_git_artifact`]. It does **not** independently
//! attest the *subpath*'s content beyond what the commit itself already
//! guarantees; that's the point, not a gap — a future reader should not
//! "fix" this by bolting on a redundant sha256 of the checked-out tree.

use serde::Deserialize;
use std::collections::HashSet;
use std::fmt;

/// Top-level parsed form of `tests/rom-manifest.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// One entry per fetchable file (see module doc).
    #[serde(default, rename = "artifact")]
    pub artifacts: Vec<Artifact>,
    /// One entry per pinned-commit/sparse-subpath git checkout (module doc,
    /// ticket W0-07).
    #[serde(default, rename = "git_artifact")]
    pub git_artifacts: Vec<GitArtifact>,
    /// One entry per accuracy-table suite (see module doc).
    #[serde(default, rename = "suite")]
    pub suites: Vec<Suite>,
}

/// A single fetchable, byte-identical unit.
#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    /// Unique key referenced by [`SuiteRom::artifact`].
    pub id: String,
    /// No-vendor/no-rehost rule (TESTING.md §3, design-review G-43).
    pub license_status: LicenseStatus,
    /// Expected SHA-256, lowercase hex, or a `"TODO-<reason>"` placeholder
    /// (never a fabricated hash — see [`Artifact::is_hash_placeholder`]).
    pub sha256: String,
    /// Ordered list of alternate-origin URLs (FM-14). Must be non-empty and
    /// every URL must serve bytes matching `sha256` — see module doc.
    pub mirrors: Vec<String>,
    /// Path under `roms/` (gitignored, NFR-006) the verified bytes land at.
    pub dest: String,
}

impl Artifact {
    /// `true` if [`Artifact::sha256`] is an honest "not computed yet"
    /// placeholder rather than a real hash. [`crate::fetch::fetch_artifact`]
    /// refuses to fetch these — a placeholder can never be "verified".
    #[must_use]
    pub fn is_hash_placeholder(&self) -> bool {
        self.sha256.starts_with("TODO-")
    }
}

/// A single pinned-commit, sparse-subpath git checkout (module doc, ticket
/// W0-07). Unlike [`Artifact`], integrity comes from [`GitArtifact::commit`]
/// alone — see module doc for why there is no `sha256` field here.
#[derive(Debug, Clone, Deserialize)]
pub struct GitArtifact {
    /// Unique key referenced by [`SuiteRom::artifact`] — shares its
    /// namespace with [`Artifact::id`] (see [`Manifest::validate`]).
    pub id: String,
    /// Same posture/rule as [`Artifact::license_status`].
    pub license_status: LicenseStatus,
    /// Clone URL, e.g. `https://github.com/<org>/<repo>.git`.
    pub repo: String,
    /// 40 lowercase hex chars. The integrity check: after fetching, `git
    /// -C <dest> rev-parse HEAD` must equal this value exactly (see
    /// [`crate::fetch::fetch_git_artifact`]).
    pub commit: String,
    /// Path within the repo passed to `git sparse-checkout set --cone`.
    pub subpath: String,
    /// Path under `roms/` (gitignored, NFR-006) the checkout lands at —
    /// a directory, not a file (see module doc: this is the one
    /// deliberate exception to `fetch.rs`'s "every artifact is one file").
    pub dest: String,
}

/// License posture of an [`Artifact`] (no-vendor/no-rehost rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LicenseStatus {
    /// No license grant from upstream — fetch-from-origin only, never
    /// vendored/mirrored/re-hosted by us (TESTING.md §3, G-43).
    NoLicenseGrantFetchOnly,
    /// Public domain (e.g. Alter Ego).
    PublicDomain,
    /// Permissively licensed (MIT/zlib/similar) upstream.
    Permissive,
}

/// One accuracy-table suite: the waiver/report lookup key.
#[derive(Debug, Clone, Deserialize)]
pub struct Suite {
    /// Suite id — accuracy-table row key and waiver-file lookup key.
    pub id: String,
    /// Target console.
    pub console: Console,
    /// `FR-CORE-xxx`, copied verbatim from TESTING.md — DATA, not inferred.
    pub fr: String,
    /// CI cadence (TESTING.md §4/§5).
    pub tier: Tier,
    /// How this suite's ROMs are run/scored.
    pub protocol: Protocol,
    /// One row per accuracy-table ROM.
    #[serde(default, rename = "roms")]
    pub roms: Vec<SuiteRom>,
}

/// Target console for a [`Suite`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Console {
    Nes,
    Snes,
}

/// CI cadence (TESTING.md §4/§5: "Tier A = every PR; Tier B = nightly").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Tier {
    A,
    B,
}

/// How a [`Suite`]'s ROMs are exercised and scored (TESTING.md §1/§2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// blargg `$6000`/`$6004` protocol — see [`crate::blargg`].
    SixThousand,
    /// nestest-style byte-exact CPU trace diff against a golden log.
    TraceLog,
    /// SingleStepTests-style JSON per-opcode vectors; no ROM run.
    Vector,
    /// Framebuffer SHA-256 vs a checked-in golden — see
    /// [`crate::golden_frame`].
    GoldenFrame,
    /// RAM result block compared against a shipped expected-value file.
    RamResult,
    /// Per-channel RMS envelope compared against a known-good recording.
    AudioRms,
    /// The ROM's verdict is only on screen: it writes ASCII character codes
    /// into the nametable and never touches `$6000` or a RAM result byte
    /// (ticket W2-12). blargg's own readme is the scoring rule — "If a test
    /// prints 'passed', it passed" — so the runner reads the nametable and
    /// looks for PASSED / FAIL.
    ///
    /// This exists because `cpu_timing_test6` is scoreable no other way,
    /// and because `dmc_dma_during_read4` (W2-01b) is in the same family:
    /// screen-only ROMs were previously either unwired or mis-scored as
    /// hangs.
    ScreenText,
}

/// One accuracy-table row within a [`Suite`].
#[derive(Debug, Clone, Deserialize)]
pub struct SuiteRom {
    /// [`Artifact::id`] this row's ROM/data comes from.
    pub artifact: String,
    /// Accuracy-table row label (not necessarily the on-disk filename).
    pub rom: String,
    /// Declared timeout in frames (TESTING.md §2); `0` for protocols with
    /// no frame concept (e.g. [`Protocol::Vector`]).
    pub frame_budget: u32,
}

/// A validation failure. [`Manifest::validate`] collects every violation
/// rather than stopping at the first, so one run reports everything wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ValidationError {}

impl Manifest {
    /// Parse `text` as a `tests/rom-manifest.toml` document.
    ///
    /// # Errors
    /// Returns a human-readable message on malformed TOML or a field that
    /// doesn't match the schema (e.g. an unknown `protocol` string).
    pub fn parse(text: &str) -> Result<Manifest, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Structural self-consistency checks that TOML deserialization alone
    /// cannot express: unique ids (shared namespace across [`Artifact`]
    /// and [`GitArtifact`] — both are things a `[[suite.roms]]` row can
    /// name), every `dest` really lives under `roms/` (the testable form
    /// of NFR-006 — "the repo shall never contain ROMs"), every mirror
    /// list non-empty, every `sha256` either a real lowercase-hex digest
    /// or an honest `TODO-` placeholder, every [`GitArtifact::commit`] a
    /// real 40-lowercase-hex commit SHA, and every `[[suite.roms]]`
    /// reference resolves to a declared artifact of either kind.
    ///
    /// # Errors
    /// Returns every violation found, not just the first.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        let mut artifact_ids = HashSet::new();

        for a in &self.artifacts {
            if !artifact_ids.insert(a.id.as_str()) {
                errors.push(ValidationError(format!("duplicate artifact id: {}", a.id)));
            }
            if !a.dest.starts_with("roms/") {
                errors.push(ValidationError(format!(
                    "artifact {}: dest {:?} must start with \"roms/\" (NFR-006)",
                    a.id, a.dest
                )));
            }
            if a.mirrors.is_empty() {
                errors.push(ValidationError(format!(
                    "artifact {}: mirrors list is empty (FM-14 requires at least one origin)",
                    a.id
                )));
            }
            if !a.is_hash_placeholder() && !is_lowercase_sha256_hex(&a.sha256) {
                errors.push(ValidationError(format!(
                    "artifact {}: sha256 {:?} is neither 64 lowercase hex chars nor a TODO- placeholder",
                    a.id, a.sha256
                )));
            }
        }

        for g in &self.git_artifacts {
            if !artifact_ids.insert(g.id.as_str()) {
                errors.push(ValidationError(format!(
                    "duplicate artifact id: {} (shared namespace between [[artifact]] and [[git_artifact]])",
                    g.id
                )));
            }
            if !g.dest.starts_with("roms/") {
                errors.push(ValidationError(format!(
                    "git_artifact {}: dest {:?} must start with \"roms/\" (NFR-006)",
                    g.id, g.dest
                )));
            }
            if !is_lowercase_commit_hex(&g.commit) {
                errors.push(ValidationError(format!(
                    "git_artifact {}: commit {:?} is not 40 lowercase hex chars",
                    g.id, g.commit
                )));
            }
        }

        let mut suite_ids = HashSet::new();
        for s in &self.suites {
            if !suite_ids.insert(s.id.as_str()) {
                errors.push(ValidationError(format!("duplicate suite id: {}", s.id)));
            }
            if s.roms.is_empty() {
                errors.push(ValidationError(format!(
                    "suite {}: roms list is empty",
                    s.id
                )));
            }
            for r in &s.roms {
                if !artifact_ids.contains(r.artifact.as_str()) {
                    errors.push(ValidationError(format!(
                        "suite {} rom {:?}: references unknown artifact id {:?}",
                        s.id, r.rom, r.artifact
                    )));
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Look up an artifact by id.
    #[must_use]
    pub fn artifact(&self, id: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.id == id)
    }

    /// Look up a git artifact by id.
    #[must_use]
    pub fn git_artifact(&self, id: &str) -> Option<&GitArtifact> {
        self.git_artifacts.iter().find(|g| g.id == id)
    }

    /// Look up a suite by id.
    #[must_use]
    pub fn suite(&self, id: &str) -> Option<&Suite> {
        self.suites.iter().find(|s| s.id == id)
    }
}

impl Suite {
    /// Look up one of this suite's accuracy-table rows by ROM label.
    #[must_use]
    pub fn rom(&self, rom: &str) -> Option<&SuiteRom> {
        self.roms.iter().find(|r| r.rom == rom)
    }
}

fn is_lowercase_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn is_lowercase_commit_hex(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest this ticket actually populated (TESTING.md §3, ticket
    /// W0-03 acceptance criterion 4) parses and validates cleanly. Ties the
    /// crate to the real file so a future hand-edit that breaks the schema
    /// or introduces a dangling `[[suite.roms]].artifact` reference fails
    /// `cargo test`, not just code review.
    #[test]
    fn real_manifest_parses_and_validates() {
        let text = include_str!("../../../tests/rom-manifest.toml");
        let manifest = Manifest::parse(text).expect("real manifest must parse");
        manifest
            .validate()
            .expect("real manifest must pass validation");
        assert!(!manifest.artifacts.is_empty());
        assert!(!manifest.suites.is_empty());
    }

    #[test]
    fn parses_minimal_manifest() {
        let text = r#"
            [[artifact]]
            id = "a1"
            license_status = "public-domain"
            sha256 = "00000000000000000000000000000000000000000000000000000000000000ab"
            mirrors = ["https://example.invalid/a1"]
            dest = "roms/nes/a1.nes"

            [[suite]]
            id = "s1"
            console = "nes"
            fr = "FR-CORE-999"
            tier = "A"
            protocol = "six_thousand"
            [[suite.roms]]
            artifact = "a1"
            rom = "a1"
            frame_budget = 60
        "#;
        let manifest = Manifest::parse(text).unwrap();
        assert_eq!(manifest.artifacts.len(), 1);
        assert_eq!(manifest.suites.len(), 1);
        manifest.validate().unwrap();
    }

    #[test]
    fn rejects_dest_outside_roms() {
        let m = Manifest {
            artifacts: vec![Artifact {
                id: "a1".into(),
                license_status: LicenseStatus::PublicDomain,
                sha256: "TODO-test".into(),
                mirrors: vec!["https://example.invalid".into()],
                dest: "not-roms/a1.nes".into(),
            }],
            git_artifacts: vec![],
            suites: vec![],
        };
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.0.contains("must start with")));
    }

    #[test]
    fn rejects_empty_mirrors() {
        let m = Manifest {
            artifacts: vec![Artifact {
                id: "a1".into(),
                license_status: LicenseStatus::PublicDomain,
                sha256: "TODO-test".into(),
                mirrors: vec![],
                dest: "roms/nes/a1.nes".into(),
            }],
            git_artifacts: vec![],
            suites: vec![],
        };
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.0.contains("mirrors list is empty")));
    }

    #[test]
    fn rejects_suite_rom_referencing_unknown_artifact() {
        let m = Manifest {
            artifacts: vec![],
            git_artifacts: vec![],
            suites: vec![Suite {
                id: "s1".into(),
                console: Console::Nes,
                fr: "FR-CORE-999".into(),
                tier: Tier::A,
                protocol: Protocol::SixThousand,
                roms: vec![SuiteRom {
                    artifact: "does-not-exist".into(),
                    rom: "r1".into(),
                    frame_budget: 60,
                }],
            }],
        };
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.0.contains("unknown artifact id")));
    }

    #[test]
    fn accepts_todo_hash_placeholder() {
        let m = Manifest {
            artifacts: vec![Artifact {
                id: "a1".into(),
                license_status: LicenseStatus::PublicDomain,
                sha256: "TODO-network-unreachable".into(),
                mirrors: vec!["https://example.invalid".into()],
                dest: "roms/nes/a1.nes".into(),
            }],
            git_artifacts: vec![],
            suites: vec![],
        };
        m.validate().unwrap();
        assert!(m.artifacts[0].is_hash_placeholder());
    }

    #[test]
    fn rejects_malformed_sha256() {
        let m = Manifest {
            artifacts: vec![Artifact {
                id: "a1".into(),
                license_status: LicenseStatus::PublicDomain,
                sha256: "not-a-hash-and-not-a-todo".into(),
                mirrors: vec!["https://example.invalid".into()],
                dest: "roms/nes/a1.nes".into(),
            }],
            git_artifacts: vec![],
            suites: vec![],
        };
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.0.contains("sha256")));
    }

    fn sample_git_artifact() -> GitArtifact {
        GitArtifact {
            id: "g1".into(),
            license_status: LicenseStatus::NoLicenseGrantFetchOnly,
            repo: "https://example.invalid/repo.git".into(),
            commit: "b".repeat(40),
            subpath: "sub/dir".into(),
            dest: "roms/nes/g1-src".into(),
        }
    }

    #[test]
    fn git_artifact_parses_and_validates() {
        let text = r#"
            [[git_artifact]]
            id = "g1"
            license_status = "no-license-grant-fetch-only"
            repo = "https://example.invalid/repo.git"
            commit = "bb11756436da8fd16cce86aef63dc6725f48836f"
            subpath = "nes6502/v1"
            dest = "roms/nes/g1-src"
        "#;
        let manifest = Manifest::parse(text).unwrap();
        assert_eq!(manifest.git_artifacts.len(), 1);
        manifest.validate().unwrap();
        assert_eq!(
            manifest.git_artifact("g1").unwrap().commit,
            "bb11756436da8fd16cce86aef63dc6725f48836f"
        );
    }

    #[test]
    fn git_artifact_rejects_dest_outside_roms() {
        let m = Manifest {
            artifacts: vec![],
            git_artifacts: vec![GitArtifact {
                dest: "not-roms/g1-src".into(),
                ..sample_git_artifact()
            }],
            suites: vec![],
        };
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.0.contains("must start with")));
    }

    #[test]
    fn git_artifact_rejects_malformed_commit() {
        let m = Manifest {
            artifacts: vec![],
            git_artifacts: vec![GitArtifact {
                commit: "not-40-hex-chars".into(),
                ..sample_git_artifact()
            }],
            suites: vec![],
        };
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.0.contains("40 lowercase hex")));
    }

    #[test]
    fn git_artifact_rejects_uppercase_commit() {
        let m = Manifest {
            artifacts: vec![],
            git_artifacts: vec![GitArtifact {
                commit: "B".repeat(40),
                ..sample_git_artifact()
            }],
            suites: vec![],
        };
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.0.contains("40 lowercase hex")));
    }

    #[test]
    fn duplicate_id_across_artifact_and_git_artifact_kinds_is_rejected() {
        let m = Manifest {
            artifacts: vec![Artifact {
                id: "shared-id".into(),
                license_status: LicenseStatus::PublicDomain,
                sha256: "TODO-test".into(),
                mirrors: vec!["https://example.invalid".into()],
                dest: "roms/nes/a1.nes".into(),
            }],
            git_artifacts: vec![GitArtifact {
                id: "shared-id".into(),
                ..sample_git_artifact()
            }],
            suites: vec![],
        };
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.0.contains("duplicate artifact id")));
    }

    #[test]
    fn suite_rom_may_reference_a_git_artifact_id() {
        let m = Manifest {
            artifacts: vec![],
            git_artifacts: vec![sample_git_artifact()],
            suites: vec![Suite {
                id: "s1".into(),
                console: Console::Nes,
                fr: "FR-CORE-999".into(),
                tier: Tier::A,
                protocol: Protocol::Vector,
                roms: vec![SuiteRom {
                    artifact: "g1".into(),
                    rom: "r1".into(),
                    frame_budget: 0,
                }],
            }],
        };
        m.validate().unwrap();
    }
}
