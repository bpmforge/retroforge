//! Walks `/profiles` (repo root) and loads every `profile.toml` found,
//! making the example profiles load-bearing rather than decorative
//! fixtures only unit tests reference. `profiles/nes` and `profiles/snes`
//! were EMPTY before this ticket (the pre-flight note's claim that
//! examples already existed there was wrong for this checkout) — this
//! test is what keeps the two profiles this ticket adds from silently
//! bit-rotting once something else changes the schema.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/rf-profiles is two levels under the repo root")
        .to_path_buf()
}

fn find_profile_tomls(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_profile_tomls(&path, out);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("profile.toml") {
            out.push(path);
        }
    }
}

#[test]
fn every_example_profile_under_profiles_dir_loads_cleanly() {
    let profiles_dir = repo_root().join("profiles");
    let mut found = Vec::new();
    find_profile_tomls(&profiles_dir.join("nes"), &mut found);
    find_profile_tomls(&profiles_dir.join("snes"), &mut found);

    assert!(
        !found.is_empty(),
        "expected at least one profile.toml under {}/{{nes,snes}} — none found",
        profiles_dir.display()
    );

    for path in &found {
        let outcome = rf_profiles::load_file(path)
            .unwrap_or_else(|e| panic!("{} failed to load: {e}", path.display()));
        assert!(
            outcome.warnings.is_empty(),
            "{} produced unexpected unknown-key warnings: {:?}",
            path.display(),
            outcome.warnings
        );
    }
}

#[test]
fn nes_example_targets_nes_console() {
    let path = repo_root().join("profiles/nes/rf-scroller-demo/profile.toml");
    let outcome = rf_profiles::load_file(&path).expect("nes example loads");
    assert_eq!(outcome.profile.meta.console, rf_profiles::Console::Nes);
    // Proves the schema can express fixtures/nes/rf-scroller/FORMAT.md's
    // column-RLE metatile shape (SCREEN_COLS=48, SCREEN_ROWS=14) without
    // this ticket decoding anything.
    let decode = outcome.profile.decode.expect("decode section present");
    let screens = decode.screens.expect("screens spec present");
    assert_eq!(screens.width, 48);
    assert_eq!(screens.height, 14);
    assert_eq!(screens.order, "column_rle");
}

#[test]
fn snes_example_targets_snes_console_and_matches_on_sha1_alone() {
    let path = repo_root().join("profiles/snes/example-mode7/profile.toml");
    let outcome = rf_profiles::load_file(&path).expect("snes example loads");
    assert_eq!(outcome.profile.meta.console, rf_profiles::Console::Snes);
    assert_eq!(outcome.profile.identity.len(), 1);
    assert!(outcome.profile.identity[0].sha256.is_none());
    assert!(outcome.profile.identity[0].sha1.is_some());
}
