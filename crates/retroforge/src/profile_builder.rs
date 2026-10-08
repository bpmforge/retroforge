//! Making a profile in the app (Wave 24; design
//! https://claude.ai/artifact/6a89VhM41WY7Ryqaw5pt7k).
//!
//! W24-02: the player's own profiles folder, and a new profile stamped
//! with the exact dump being played so it matches the moment it is saved.

use std::path::{Path, PathBuf};

/// The player's own profiles: `<config>/retroforge/profiles`, searched
/// before the shipped ones.
#[must_use]
pub fn user_profiles_root(config_root: &Path) -> PathBuf {
    config_root
        .join(crate::bindings_store::APP_DIR)
        .join("profiles")
}

/// A folder-safe name for a title: lowercase letters and digits, runs of
/// anything else as one dash.
#[must_use]
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "game".to_owned()
    } else {
        out
    }
}

/// A new profile for one dump: the editor's own skeleton (so it loads with
/// no warnings) plus an `[[identity]]` carrying all four hashes.
#[must_use]
pub fn new_profile_text(title: &str, snes: bool, hashes: &rf_cart::RomHashes) -> String {
    let form = crate::profile_editor::NewProfileForm {
        title: title.to_owned(),
        console: if snes {
            rf_profiles::schema::Console::Snes
        } else {
            rf_profiles::schema::Console::Nes
        },
        region: "unknown".to_owned(),
        author: "made in RetroForge".to_owned(),
        source: "found in play with the RetroForge profile builder".to_owned(),
    };
    format!(
        "{}\n[[identity]]\nsha256 = \"{}\"\nsha1 = \"{}\"\nmd5 = \"{}\"\ncrc32 = \"{}\"\nrevision = \"the copy this profile was made from\"\n",
        form.to_toml(),
        hashes.sha256,
        hashes.sha1,
        hashes.md5,
        hashes.crc32
    )
}

/// Write a new profile for this dump under `user_root`, returning its
/// path. An existing profile for the same title is left alone and its
/// path returned.
///
/// # Errors
/// Returns the OS error text.
pub fn create(
    user_root: &Path,
    title: &str,
    snes: bool,
    hashes: &rf_cart::RomHashes,
) -> Result<PathBuf, String> {
    let dir = user_root
        .join(if snes { "snes" } else { "nes" })
        .join(slug(title));
    let path = dir.join("profile.toml");
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&path, new_profile_text(title, snes, hashes))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes() -> rf_cart::RomHashes {
        rf_cart::RomHashes {
            crc32: "0123abcd".into(),
            md5: "0".repeat(32),
            sha1: "1".repeat(40),
            sha256: "2".repeat(64),
        }
    }

    #[test]
    fn slugs_are_folder_safe() {
        assert_eq!(slug("Super Mario Bros. 3 (USA)"), "super-mario-bros-3-usa");
        assert_eq!(slug("!!!"), "game");
    }

    /// The new profile loads with the real loader, no warnings, and
    /// claims exactly this dump.
    #[test]
    fn a_new_profile_loads_cleanly_and_matches_its_dump() {
        let tmp = std::env::temp_dir().join(format!("rf_newprof_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let path = create(&tmp, "Test Game", true, &hashes()).unwrap();
        let outcome = rf_profiles::load_file(&path).expect("loads");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        assert!(outcome
            .profile
            .identity
            .iter()
            .any(|i| i.matches(&hashes())));
        let found = crate::level_view::find_matching_profile(&tmp, &hashes());
        assert!(found.is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
