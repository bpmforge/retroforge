//! `rf-intake` — run a community bundle through licence-gated intake
//! (ticket W9-05, criterion 3; FR-PROF-007).
//!
//! ```text
//! rf-intake <bundle-dir>
//! ```
//!
//! Exit status is the whole point, because that is what a CI job acts on:
//! **0 = accepted, 1 = refused, 2 = the bundle could not be read.**
//! Refusal and unreadable are distinct so a broken checkout does not read
//! as a policy failure — a submitter told "your licence is wrong" when the
//! directory was simply missing would go looking in the wrong place.
//!
//! ## The bundle on disk
//!
//! ```text
//! <bundle-dir>/
//!   bundle.toml     flat `key = value`: id, version, license, provenance
//!   index           one `<kind> <path>` line per member
//!   ...members...
//! ```
//!
//! Both metadata files are **hand-parsed**, deliberately, exactly as
//! `rf_plugin_sdk::manifest` is: the format is flat, this binary must not
//! drag a parser dependency into `rf-enhance`, and an intake checker that
//! cannot itself fail in interesting ways is worth more than one that
//! supports nesting nobody uses.
//!
//! A member listed in `index` but missing on disk is a **read** failure
//! (exit 2), not a refusal: the submission is malformed rather than
//! non-compliant.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use rf_enhance::distribution::{intake, Bundle, Member};

fn flat_pairs(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    out
}

fn read_bundle(dir: &Path) -> Result<Bundle, String> {
    let meta_path = dir.join("bundle.toml");
    let meta_text =
        std::fs::read_to_string(&meta_path).map_err(|e| format!("{}: {e}", meta_path.display()))?;
    let meta = flat_pairs(&meta_text);

    let index_path = dir.join("index");
    let index_text = std::fs::read_to_string(&index_path)
        .map_err(|e| format!("{}: {e}", index_path.display()))?;

    let mut members = Vec::new();
    for (i, raw) in index_text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((kind, rel)) = line.split_once(char::is_whitespace) else {
            return Err(format!(
                "{}:{}: expected `<kind> <path>`, got {line:?}",
                index_path.display(),
                i + 1
            ));
        };
        let rel = rel.trim();
        let path = dir.join(rel);
        // A listed-but-absent member is MALFORMED, not non-compliant.
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        members.push(Member {
            path: rel.to_string(),
            declared_kind: kind.trim().to_string(),
            bytes,
        });
    }

    // Missing metadata keys become empty strings rather than a read
    // error, so `intake` reports them as the POLICY faults they are
    // (MissingLicense / MissingProvenance) instead of this binary
    // reporting a parse problem for something that is really a
    // compliance problem.
    Ok(Bundle {
        id: meta.get("id").cloned().unwrap_or_default(),
        version: meta.get("version").cloned().unwrap_or_default(),
        license: meta.get("license").cloned().unwrap_or_default(),
        provenance: meta.get("provenance").cloned().unwrap_or_default(),
        members,
    })
}

fn main() -> ExitCode {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: rf-intake <bundle-dir>");
        return ExitCode::from(2);
    };
    match read_bundle(Path::new(&dir)) {
        Err(e) => {
            eprintln!("rf-intake: cannot read bundle: {e}");
            ExitCode::from(2)
        }
        Ok(bundle) => {
            let report = intake(&bundle);
            println!("{}", report.summary());
            if report.accepted() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
    }
}
