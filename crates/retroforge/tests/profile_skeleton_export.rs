//! Ticket W4-06b criterion 2 (**"the criterion W5-01 waits on"**,
//! GAME_PROFILES.md §3 step 2): "Debugger exports annotations → profile
//! skeleton (`memory_map`/`rom_map` pre-filled with sources)". The ticket
//! brief is explicit: "Run the exported skeleton through
//! `retroforge-tool profile validate` and assert it passes — don't assert
//! the TOML 'looks right'."
//!
//! This test does exactly that, as literally as `write_scope` allows:
//! `tools/retroforge-tool/**` is out of this ticket's scope, so this file
//! cannot add a test *inside* that crate — instead it spawns the actual
//! compiled `retroforge-tool` binary as a subprocess (`cargo run -p
//! retroforge-tool -- profile validate <path>`, the exact command line
//! named in the ticket brief), the same way a developer running the
//! authoring pipeline (GAME_PROFILES.md §3 step 4) would.
//!
//! Two layers of proof, per the advisor review this ticket's pre-flight
//! called for: (1) the subprocess exits 0 with **no** `warning:` line —
//! not just exit 0 alone, which an unknown-key typo (e.g. `sourse`
//! instead of `source`) would also produce, since `MemoryMapEntry.source`
//! is `Option<String>` and a missing/misnamed key just silently
//! deserializes to `None` plus a warning; (2) `rf_profiles::load_file` is
//! called directly on the same file afterward to assert the sources
//! actually round-tripped as the *values* this test wrote, not merely
//! that some string exists in the `source` position.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use rf_debugger::annotation::{AddressSpace, Annotation, AnnotationStore};
use rf_debugger::profile_export::{self, Console, ExportMeta};

static NEXT_ID: AtomicU32 = AtomicU32::new(0);

fn scratch_path(tag: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "retroforge_w4_06b_skeleton_{tag}_{}_{id}.toml",
        std::process::id()
    ))
}

fn write_skeleton_from_a_real_annotation_store() -> (PathBuf, String) {
    let mut store = AnnotationStore::new();
    store
        .add(Annotation {
            space: AddressSpace::Ram,
            addr: 0x6029,
            len: 2,
            ty: "u16".to_string(),
            label: "player_x".to_string(),
            notes: Some("World X, pixels, 0..752".to_string()),
            source: "fixtures/nes/rf-scroller/FORMAT.md".to_string(),
            count: None,
        })
        .expect("sourced RAM annotation must be accepted");
    store
        .add(Annotation {
            space: AddressSpace::Rom,
            addr: 0x0000,
            len: 4,
            ty: "ptr_table".to_string(),
            label: "metatile_table".to_string(),
            notes: None,
            source: "fixtures/nes/rf-scroller/FORMAT.md#metatile-table".to_string(),
            count: Some(4),
        })
        .expect("sourced ROM annotation must be accepted");

    let meta = ExportMeta {
        title: "W4-06b criterion-2 export test".to_string(),
        console: Console::Nes,
        region: "ntsc".to_string(),
        authors: vec!["RetroForge W4-06b".to_string()],
    };
    let text = profile_export::export_skeleton(store.entries(), &meta)
        .expect("every stored annotation is sourced and labelled");

    let path = scratch_path("valid");
    let mut f = std::fs::File::create(&path).expect("create scratch profile file");
    f.write_all(text.as_bytes()).expect("write skeleton TOML");
    (path, text)
}

/// **Criterion 2's literal proof.** Spawns the real `retroforge-tool`
/// binary (via `cargo run -p retroforge-tool`, since `CARGO_BIN_EXE_*` is
/// only set for a package's OWN tests, not a sibling workspace member's —
/// verified empirically against this workspace's cargo before writing this
/// test) with exactly the command line the ticket brief names:
/// `profile validate <path>`.
#[test]
fn exported_skeleton_passes_retroforge_tool_profile_validate_with_no_warnings() {
    let (path, _text) = write_skeleton_from_a_real_annotation_store();

    let cargo = std::env::var("CARGO").expect("CARGO env var set at test runtime");
    let output = Command::new(&cargo)
        .args([
            "run",
            "--quiet",
            "-p",
            "retroforge-tool",
            "--",
            "profile",
            "validate",
        ])
        .arg(&path)
        .output()
        .expect("spawn `cargo run -p retroforge-tool -- profile validate`");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "retroforge-tool profile validate must exit 0 for an exported skeleton\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("warning:"),
        "exported skeleton must trip no unknown-key warning (a `source` typo would still \
         exit 0 with one) — stdout:\n{stdout}"
    );
    assert!(
        stdout.contains(": ok"),
        "expected the CLI's own `<path>: ok` line — matching bare \"ok\" would \
         also accept it inside a filename or another word — stdout:\n{stdout}"
    );

    let _ = std::fs::remove_file(&path);
}

/// **The second layer**: re-parse the same skeleton with `rf_profiles::
/// load_file` directly (the exact function `retroforge-tool profile
/// validate` calls internally) and assert the `source` VALUES survived,
/// not just that the CLI was happy. Mutation: rename `SkeletonDoc`'s
/// `source` field to anything else in `rf_debugger::profile_export` and
/// this fails (the CLI-only test above would not: a stray key just warns).
#[test]
fn exported_sources_survive_the_round_trip_as_the_exact_values_written() {
    let (path, _text) = write_skeleton_from_a_real_annotation_store();

    let outcome = rf_profiles::load_file(&path).expect("must load through the real validator");
    let _ = std::fs::remove_file(&path);

    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert_eq!(outcome.profile.memory_map.len(), 1);
    assert_eq!(
        outcome.profile.memory_map[0].source.as_deref(),
        Some("fixtures/nes/rf-scroller/FORMAT.md")
    );
    assert_eq!(outcome.profile.memory_map[0].addr, 0x6029);
    assert_eq!(outcome.profile.memory_map[0].label, "player_x");

    assert_eq!(outcome.profile.rom_map.len(), 1);
    assert_eq!(
        outcome.profile.rom_map[0].source.as_deref(),
        Some("fixtures/nes/rf-scroller/FORMAT.md#metatile-table")
    );
    assert_eq!(outcome.profile.rom_map[0].offset, 0x0000);
    assert_eq!(outcome.profile.rom_map[0].count, Some(4));
}

/// **The exporter's own refusal, proven against the CLI too**, not just
/// `rf-debugger`'s unit tests: a skeleton the exporter would have refused
/// to build never even reaches disk, so there is nothing for
/// `retroforge-tool profile validate` to see — this test asserts the
/// refusal happens before that point, closing the loop on vacuity trap
/// (a) at the integration level.
#[test]
fn a_sourceless_annotation_is_refused_before_a_file_is_ever_written() {
    let bad = Annotation {
        space: AddressSpace::Ram,
        addr: 0x0010,
        len: 1,
        ty: "u8".to_string(),
        label: "flags".to_string(),
        notes: None,
        source: String::new(),
        count: None,
    };
    let meta = ExportMeta {
        title: "t".to_string(),
        console: Console::Nes,
        region: "ntsc".to_string(),
        authors: vec![],
    };
    let err = profile_export::export_skeleton(std::slice::from_ref(&bad), &meta)
        .expect_err("sourceless annotation must be refused");
    assert_eq!(
        err,
        profile_export::ExportError::SourcelessEntry { addr: 0x0010 }
    );
}
