//! Ticket W5-06 criteria 1-3: hot-reload on save, decode errors with ROM
//! offsets, and a preview that re-renders immediately.
//!
//! ## The vacuity trap: a watcher that always fires
//!
//! "Hot-reload works" is trivially satisfiable by reloading every poll,
//! and such an implementation passes any test that only checks a change
//! is noticed. So every reload test here is paired with a **no-change**
//! assertion, and the preview test asserts the preview's *content*
//! changed rather than that a reload happened.

use std::path::{Path, PathBuf};

use retroforge::authoring::{self, Watch};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rf_authoring_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/retroforge is two levels under the repo root")
        .to_path_buf()
}

/// The shipped RF-Scroller profile, as authoring text to edit.
fn shipped_profile_text() -> String {
    std::fs::read_to_string(repo_root().join("profiles/nes/rf-scroller/profile.toml"))
        .expect("the shipped profile is checked in")
}

/// The fixture ROM, header-stripped. Skips locally, fails on CI.
fn normalized_rom() -> Option<Vec<u8>> {
    let path = repo_root().join("fixtures/nes/rf-scroller/build/rf-scroller.nes");
    match std::fs::read(path) {
        Ok(raw) => Some(raw[16..].to_vec()),
        Err(_) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "fixture ROM missing on CI: its build step failed and this suite would pass \
                 having asserted nothing"
            );
            eprintln!("SKIP: fixture ROM not built — run fixtures/nes/rf-scroller/build.sh");
            None
        }
    }
}

/// **Criterion 1, with the half that matters.** A watch must notice a
/// save AND must stay quiet when nothing happened — a watcher that
/// always fires satisfies "hot-reload works" and reloads the preview
/// sixty times a second forever.
#[test]
fn the_watch_fires_on_save_and_stays_quiet_otherwise() {
    let dir = scratch("watch");
    let path = dir.join("profile.toml");
    std::fs::write(&path, "x = 1\n").unwrap();

    let mut watch = Watch::new(&path);
    assert!(
        !watch.poll(),
        "a freshly constructed watch must not report a change — otherwise every panel open looks \
         like an edit"
    );
    assert!(!watch.poll(), "still nothing has happened");

    // A save that changes the length is unambiguous regardless of the
    // filesystem's mtime granularity, which on some platforms is coarse
    // enough that two writes in the same test can share a timestamp.
    std::fs::write(&path, "x = 1\ny = 2\n").unwrap();
    assert!(watch.poll(), "a save must be noticed");
    assert!(!watch.poll(), "the same save must not be reported twice");

    // Deletion counts, once: the preview must stop claiming to show a
    // profile that is gone.
    std::fs::remove_file(&path).unwrap();
    assert!(watch.poll(), "a deleted profile is a change");
    assert!(!watch.poll());

    let _ = std::fs::remove_dir_all(&dir);
}

/// **Criterion 2.** A mistyped table address — the most common authoring
/// mistake — must come back naming the table and carrying the ROM offset
/// it pointed at.
#[test]
fn a_mistyped_table_address_reports_the_rom_offset_it_pointed_at() {
    let Some(rom) = normalized_rom() else { return };
    let dir = scratch("offsets");
    let path = dir.join("profile.toml");

    // Push level_rle_data past the end of the ROM, the way a fat finger
    // on a hex digit would.
    let text = shipped_profile_text().replace("offset = 0x1219", "offset = 0xF1219");
    std::fs::write(&path, &text).unwrap();

    let out = authoring::reload(&path, Some(&rom));
    assert!(
        !out.has_preview(),
        "a profile pointing outside the ROM must not produce a preview"
    );
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    let err = &out.errors[0];
    assert_eq!(
        err.rom_offset,
        Some(0xF_1219),
        "the error must carry the offset the author typed, so they can find it"
    );
    assert!(
        err.message.contains("level_rle_data"),
        "the error must name WHICH table: {}",
        err.message
    );
    assert!(
        err.line().contains("0xF1219"),
        "the inline line must render the offset in hex, like every other ROM address in this \
         project: {}",
        err.line()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A file-level problem must NOT invent a ROM offset — an offset the
/// author cannot act on is worse than none.
#[test]
fn a_toml_syntax_error_carries_no_rom_offset() {
    let dir = scratch("syntax");
    let path = dir.join("profile.toml");
    std::fs::write(&path, "[meta\nthis is not toml").unwrap();

    let out = authoring::reload(&path, None);
    assert!(out.profile.is_none());
    assert_eq!(out.errors.len(), 1);
    assert_eq!(
        out.errors[0].rom_offset, None,
        "a TOML syntax error is about the file, not the ROM"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **Criterion 3.** Edit the profile, reload, and the preview's CONTENT
/// must differ — not merely "a reload happened".
#[test]
fn editing_the_profile_changes_what_the_preview_shows() {
    let Some(rom) = normalized_rom() else { return };
    let dir = scratch("preview");
    let path = dir.join("profile.toml");

    std::fs::write(&path, shipped_profile_text()).unwrap();
    let before = authoring::reload(&path, Some(&rom));
    let before_level = before
        .level
        .expect("the shipped profile decodes the fixture");
    let before_text = authoring::preview_text(&before_level, 32, 14);
    assert!(
        before_text.lines().count() >= 14,
        "the preview must show the level's rows: {before_text}"
    );

    // Point the column table one byte off — still inside the ROM, still
    // structurally decodable, but a DIFFERENT level. This is the edit an
    // author makes while hunting for the right address, and the preview
    // has to answer it.
    let text = shipped_profile_text().replace(
        r#"offset = 0x11E9
len = 48
label = "level_column_offset""#,
        r#"offset = 0x11EA
len = 48
label = "level_column_offset""#,
    );
    std::fs::write(&path, &text).unwrap();
    let after = authoring::reload(&path, Some(&rom));

    match after.level {
        Some(level) => {
            let after_text = authoring::preview_text(&level, 32, 14);
            assert_ne!(
                before_text, after_text,
                "the preview did not change after the column table moved — it is not re-decoding"
            );
        }
        None => {
            // Equally acceptable and equally informative: the shifted
            // table breaks the format's invariant and the author is told
            // so. What must NOT happen is an unchanged preview.
            assert!(
                !after.errors.is_empty(),
                "a shifted table produced neither a different preview nor an error"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The shipped profile must round-trip through the authoring loop
/// cleanly — no errors, no warnings, a preview. If this ever fails, the
/// profile this project ships has drifted from the loader that reads it.
#[test]
fn the_shipped_profile_reloads_clean() {
    let Some(rom) = normalized_rom() else { return };
    let path = repo_root().join("profiles/nes/rf-scroller/profile.toml");
    let out = authoring::reload(&path, Some(&rom));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert!(out.has_preview());
}

/// Iterating without a ROM open is a normal authoring state, not an
/// error: an author shapes a profile's structure before they have the
/// game loaded.
#[test]
fn a_profile_with_no_rom_open_warns_rather_than_erroring() {
    let path = repo_root().join("profiles/nes/rf-scroller/profile.toml");
    let out = authoring::reload(&path, None);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(!out.has_preview());
    assert!(
        out.warnings.iter().any(|w| w.contains("No ROM open")),
        "the author must be told WHY there is no preview: {:?}",
        out.warnings
    );
}
