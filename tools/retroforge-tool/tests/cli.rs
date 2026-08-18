//! Integration tests for the `retroforge-tool` binary, driven as an
//! actual subprocess (`env!("CARGO_BIN_EXE_retroforge-tool")`) so these
//! only run when `retroforge-tool` is part of `cargo test --workspace` —
//! which is exactly why `tools/*` had to join the workspace's `members`
//! list rather than staying a standalone manifest (see root Cargo.toml's
//! comment).
//!
//! Vacuity trap (c): "a `profile validate` test that only checks exit
//! code 0 on a good file is worthless" — every non-zero-exit test here
//! also asserts the printed message names the offending key/reason, and
//! the warning test asserts the warning text is actually printed, not
//! merely that the exit code stayed 0 (which a loader that silently
//! dropped warnings would also produce).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_retroforge-tool")
}

/// A fresh scratch directory per test, under the OS temp dir, cleaned up
/// on drop. No `tempfile` dependency (not on TECH_STACK.md's sanctioned
/// list) — a PID+name-qualified path under `std::env::temp_dir()` would be
/// enough for uniqueness alone, but the constructor also `remove_dir_all`s
/// any pre-existing directory at that path first; two tests sharing a
/// `tag` (a future test added to this file, say) would then have one wipe
/// the other's directory mid-run, and the resulting failure would read as
/// a confusing missing-file rather than a name clash. A per-process
/// atomic counter makes every path unique regardless of `tag` collisions.
static NEXT_SCRATCH_ID: AtomicU32 = AtomicU32::new(0);

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let id = NEXT_SCRATCH_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "retroforge_tool_cli_test_{tag}_{}_{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch dir");
        ScratchDir(path)
    }

    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const VALID_PROFILE: &str = r#"
[meta]
profile_version = "0.1"
title = "CLI test profile"
console = "nes"
region = "ntsc"
"#;

#[test]
fn profile_validate_exits_zero_on_a_good_file() {
    let dir = ScratchDir::new("good");
    let path = dir.write("good.toml", VALID_PROFILE);

    let output = Command::new(bin())
        .args(["profile", "validate"])
        .arg(&path)
        .output()
        .expect("run retroforge-tool");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ok"), "{stdout}");
}

#[test]
fn profile_validate_exits_nonzero_and_names_the_offending_key_on_a_bad_file() {
    let dir = ScratchDir::new("bad");
    // Missing `title`, a required field.
    let bad = r#"
        [meta]
        profile_version = "0.1"
        console = "nes"
        region = "ntsc"
    "#;
    let path = dir.write("bad.toml", bad);

    let output = Command::new(bin())
        .args(["profile", "validate"])
        .arg(&path)
        .output()
        .expect("run retroforge-tool");

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("title"),
        "expected the offending key `title` to be named in: {stderr}"
    );
}

#[test]
fn profile_validate_rejects_a_newer_major_and_says_so() {
    let dir = ScratchDir::new("newer_major");
    let newer = VALID_PROFILE.replace("profile_version = \"0.1\"", "profile_version = \"9.0\"");
    let path = dir.write("newer.toml", &newer);

    let output = Command::new(bin())
        .args(["profile", "validate"])
        .arg(&path)
        .output()
        .expect("run retroforge-tool");

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("newer"), "{stderr}");
    assert!(stderr.contains('9'), "{stderr}");
}

/// Mutation target: if `main.rs`'s validate path stopped printing
/// `outcome.warnings`, this test (not just the library-level one in
/// `rf-profiles`) would fail — it is the one that proves the CLI itself
/// doesn't swallow a warning the loader correctly produced.
#[test]
fn profile_validate_prints_unknown_key_warning_for_a_file_that_still_exits_zero() {
    let dir = ScratchDir::new("warn");
    let warn = format!("{VALID_PROFILE}\n[bogus_section]\nx = 1\n");
    let path = dir.write("warn.toml", &warn);

    let output = Command::new(bin())
        .args(["profile", "validate"])
        .arg(&path)
        .output()
        .expect("run retroforge-tool");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("warning") && stdout.contains("bogus_section"),
        "{stdout}"
    );
}

#[test]
fn profile_validate_walks_a_directory_and_finds_nested_toml_files() {
    let dir = ScratchDir::new("dirwalk");
    dir.write("a/one.toml", VALID_PROFILE);
    dir.write("a/b/two.toml", VALID_PROFILE);

    let output = Command::new(bin())
        .args(["profile", "validate"])
        .arg(dir.path())
        .output()
        .expect("run retroforge-tool");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("one.toml"), "{stdout}");
    assert!(stdout.contains("two.toml"), "{stdout}");
}

/// Minimal valid NROM image (1x16KB PRG, 1x8KB CHR), same shape
/// `rf-cart`'s own tests build, with a distinguishing payload byte so its
/// hash isn't the all-zero-ROM hash.
fn synthetic_nrom(payload_byte: u8) -> Vec<u8> {
    let mut rom = Vec::new();
    rom.extend_from_slice(&[0x4E, 0x45, 0x53, 0x1A]); // "NES\x1A"
    rom.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    rom.extend(vec![payload_byte; 16 * 1024 + 8 * 1024]);
    rom
}

#[test]
fn profile_hash_reports_match_for_the_rom_it_names() {
    let dir = ScratchDir::new("hash_match");
    let rom_bytes = synthetic_nrom(0xAB);
    let identity = rf_cart::hash::identity_nes(&rom_bytes);

    let rom_path = dir.path().join("game.nes");
    std::fs::write(&rom_path, &rom_bytes).unwrap();

    let profile = format!(
        "{VALID_PROFILE}\n[[identity]]\nsha256 = \"{}\"\n",
        identity.normalized.sha256
    );
    let profile_path = dir.write("game.toml", &profile);

    let output = Command::new(bin())
        .args(["profile", "hash"])
        .arg(&rom_path)
        .arg(&profile_path)
        .output()
        .expect("run retroforge-tool");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("MATCH") && !stdout.contains("NO MATCH"),
        "{stdout}"
    );
}

#[test]
fn profile_hash_reports_no_match_and_exits_nonzero_for_the_wrong_rom() {
    let dir = ScratchDir::new("hash_mismatch");
    let rom_bytes = synthetic_nrom(0xCD);

    let rom_path = dir.path().join("other.nes");
    std::fs::write(&rom_path, &rom_bytes).unwrap();

    // A well-formed but wrong hash — this profile identifies a different
    // ROM than the one on disk.
    let profile = format!(
        "{VALID_PROFILE}\n[[identity]]\nsha256 = \"{}\"\n",
        "f".repeat(64)
    );
    let profile_path = dir.write("other.toml", &profile);

    let output = Command::new(bin())
        .args(["profile", "hash"])
        .arg(&rom_path)
        .arg(&profile_path)
        .output()
        .expect("run retroforge-tool");

    assert!(!output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("NO MATCH"), "{stdout}");
}

/// W4-02 reserved the `rom` group and this test asserted the placeholder
/// message; ticket W4-07 implemented it, so what is worth asserting here
/// is what the placeholder was standing in for — that `rom` still
/// dispatches to its own group rather than being swallowed by `profile`,
/// and that its subcommands are the four W4-07 shipped. The content of
/// those subcommands is tested in `rom_cli.rs`.
#[test]
fn rom_group_dispatches_separately_from_profile() {
    let output = Command::new(bin()).args(["rom", "hash"]).output().unwrap();
    assert!(!output.status.success(), "a missing path is an error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rom hash") && stderr.contains("<rom-path>"),
        "the error must come from the rom group, not the profile group: {stderr}"
    );

    let usage = Command::new(bin()).output().unwrap();
    let usage_text = String::from_utf8_lossy(&usage.stderr);
    for sub in ["rom hash", "rom inspect", "rom dump", "rom trace"] {
        assert!(
            usage_text.contains(sub),
            "usage must list `{sub}`: {usage_text}"
        );
    }
}
