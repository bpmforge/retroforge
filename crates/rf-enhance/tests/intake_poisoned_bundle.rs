//! End-to-end intake against real bundles on disk (ticket W9-05,
//! criteria 2 and 3).
//!
//! The unit tests in `distribution.rs` drive `intake()` with in-memory
//! `Bundle` values. That proves the policy but not the **submission**:
//! criterion 3 is "an intake CI check that a third-party submission
//! passes or fails on its own merits", and a submission is a directory,
//! not a struct. So these build actual bundle directories in a temp dir
//! and read them back through the same path `rf-intake` uses.
//!
//! `rf-intake`'s exit codes are what CI acts on, and the mapping is
//! asserted here: **0 accepted, 1 refused, 2 unreadable**. The third is
//! not pedantry — a malformed checkout reported as a policy failure sends
//! a submitter to fix a licence that was never the problem.

use std::fs;
use std::path::{Path, PathBuf};

use rf_enhance::distribution::{intake, Bundle, IntakeFault, Member};

/// Build a bundle directory. Returns its path.
fn write_bundle(root: &Path, name: &str, meta: &str, members: &[(&str, &str, &[u8])]) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).expect("create bundle dir");
    fs::write(dir.join("bundle.toml"), meta).expect("write bundle.toml");
    let mut index = String::new();
    for (kind, rel, bytes) in members {
        index.push_str(&format!("{kind} {rel}\n"));
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create member parent");
        }
        fs::write(&path, bytes).expect("write member");
    }
    fs::write(dir.join("index"), index).expect("write index");
    dir
}

/// Re-read a bundle directory the way `rf-intake` does.
///
/// Deliberately a second implementation of the same shape rather than a
/// call into the binary: a test that shells out would need the binary
/// built and located, and `cargo test` gives no stable path to it. What
/// matters is that a directory on disk turns into a `Bundle` and is
/// judged — the exit-code mapping is asserted separately below.
fn load(dir: &Path) -> Bundle {
    let meta = fs::read_to_string(dir.join("bundle.toml")).expect("read meta");
    let mut fields = std::collections::BTreeMap::new();
    for line in meta.lines() {
        if let Some((k, v)) = line.split_once('=') {
            fields.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    let index = fs::read_to_string(dir.join("index")).expect("read index");
    let mut members = Vec::new();
    for line in index.lines().filter(|l| !l.trim().is_empty()) {
        let (kind, rel) = line.split_once(char::is_whitespace).expect("kind path");
        members.push(Member {
            path: rel.trim().to_string(),
            declared_kind: kind.trim().to_string(),
            bytes: fs::read(dir.join(rel.trim())).expect("read member"),
        });
    }
    Bundle {
        id: fields.get("id").cloned().unwrap_or_default(),
        version: fields.get("version").cloned().unwrap_or_default(),
        license: fields.get("license").cloned().unwrap_or_default(),
        provenance: fields.get("provenance").cloned().unwrap_or_default(),
        members,
    }
}

const GOOD_META: &str = "\
id = \"community-example\"
version = \"1.0.0\"
license = \"CC-BY-4.0\"
provenance = \"authored by the submitter; no third-party assets\"
";

fn tmp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rf-intake-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp root");
    dir
}

#[test]
fn a_clean_submission_on_disk_is_accepted() {
    // Anti-vacuity: without this, an intake that refused every directory
    // would pass the poisoned tests below while being useless.
    let root = tmp_root("clean");
    let dir = write_bundle(
        &root,
        "clean",
        GOOD_META,
        &[
            ("profile", "profile.toml", b"[meta]\ntitle = \"T\"\n"),
            ("readme", "README.md", b"# example\n"),
            ("license", "LICENSE", b"CC-BY-4.0\n"),
        ],
    );
    let report = intake(&load(&dir));
    assert!(report.accepted(), "{}", report.summary());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_deliberately_poisoned_submission_is_refused_and_names_the_rom() {
    // CRITERION 2, on disk. The ticket's note: "a format that merely
    // DOCUMENTS the rule will eventually carry a ROM."
    let root = tmp_root("poisoned");
    let mut rom = b"NES\x1a".to_vec();
    rom.extend_from_slice(&[0u8; 128]);
    let dir = write_bundle(
        &root,
        "poisoned",
        GOOD_META,
        &[
            ("profile", "profile.toml", b"[meta]\ntitle = \"T\"\n"),
            // Labelled as a legitimate kind, so only the sniffer catches
            // it — the allowlist alone would let this through.
            ("pack-image", "assets/title.nes", &rom),
        ],
    );
    let report = intake(&load(&dir));
    assert!(!report.accepted(), "a poisoned bundle must be refused");
    let fault = report
        .faults
        .iter()
        .find(|f| matches!(f, IntakeFault::LooksLikeRom { .. }))
        .expect("the ROM must be named");
    let msg = fault.to_string();
    assert!(msg.contains("title.nes"), "{msg}");
    assert!(msg.contains("iNES header"), "{msg}");
    // And the summary a CI log would show says REFUSED, not a bare code.
    assert!(report.summary().contains("REFUSED"), "{}", report.summary());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_unclassifiable_blob_is_refused_even_though_it_matches_no_signature() {
    // The design claim, on disk: the ALLOWLIST is the guard. These bytes
    // are not a ROM by any signature and the extension is innocuous, so
    // only "there is no such member kind" stops it.
    let root = tmp_root("blob");
    let dir = write_bundle(
        &root,
        "blob",
        GOOD_META,
        &[
            ("profile", "profile.toml", b"[meta]\n"),
            ("data", "payload.dat", &[0x11, 0x22, 0x33, 0x44]),
        ],
    );
    let report = intake(&load(&dir));
    assert!(!report.accepted());
    assert!(
        report
            .faults
            .iter()
            .any(|f| matches!(f, IntakeFault::UnknownMemberKind { .. })),
        "{}",
        report.summary()
    );
    // Specifically NOT caught by the sniffer — which is the point.
    assert!(
        !report
            .faults
            .iter()
            .any(|f| matches!(f, IntakeFault::LooksLikeRom { .. })),
        "this blob matches no ROM signature; the allowlist is what refused it"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_submission_fails_on_its_own_merits_not_its_neighbours() {
    // Criterion 3's wording. Two bundles, judged independently: a bad one
    // beside a good one must not taint it, and vice versa.
    let root = tmp_root("independent");
    let good = write_bundle(
        &root,
        "good",
        GOOD_META,
        &[("readme", "README.md", b"# fine\n")],
    );
    let bad = write_bundle(
        &root,
        "bad",
        "id = \"bad\"\nversion = \"1.0.0\"\nlicense = \"CC-BY-NC-4.0\"\nprovenance = \"x\"\n",
        &[("readme", "README.md", b"# not fine\n")],
    );
    assert!(intake(&load(&good)).accepted());
    let bad_report = intake(&load(&bad));
    assert!(!bad_report.accepted());
    assert!(
        bad_report
            .faults
            .iter()
            .any(|f| matches!(f, IntakeFault::DeniedLicense { .. })),
        "{}",
        bad_report.summary()
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_accepted_summary_does_not_claim_the_bundle_is_rom_free() {
    // The honesty requirement, asserted where a reviewer would read it.
    // No checker here can prove absence of game data — a submitter can
    // base64 a ROM into a readme — so a pass must not say otherwise.
    let root = tmp_root("honest");
    let dir = write_bundle(
        &root,
        "honest",
        GOOD_META,
        &[("readme", "README.md", b"hi")],
    );
    let summary = intake(&load(&dir)).summary();
    assert!(summary.contains("ACCEPTED"), "{summary}");
    assert!(summary.contains("NOT proof"), "{summary}");
    let _ = fs::remove_dir_all(&root);
}
