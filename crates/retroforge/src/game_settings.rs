//! Per-game settings, keyed by normalized ROM hash (ticket W2-07,
//! FR-FE-002).
//!
//! ## One file per game, named by the hash
//!
//! `<config>/retroforge/games/<normalized-sha256>.rfgame`. One file rather
//! than a single index because the two failure modes differ sharply: a
//! corrupted index costs the user every game's settings, while a corrupted
//! per-game file costs one game's. It also makes the store append-only in
//! practice — two processes settling different games never touch the same
//! file — and makes "reset this game" a delete.
//!
//! The hash is the same normalized identity `crate::library` reports and
//! `crate::save_state` verifies against, so a renamed or re-dumped
//! cartridge keeps its settings.
//!
//! ## Unknown keys are PRESERVED, not dropped
//!
//! This is the design decision worth stating. The settings this build knows
//! (mode, sprite overlay, shader) are a fraction of what FR-FE-002 will
//! eventually hold — W3-02a adds shaders, W4-05 adds per-game enhancement
//! toggles — and versions of this app will differ. A load keeps every
//! key it does not recognize and a save writes them back, so an older build
//! opening a newer build's config does not silently delete the settings it
//! could not read. That is a property a `#[derive(Deserialize)]` struct
//! would not have given for free, and losing a user's settings by opening
//! them is exactly the kind of quiet damage that erodes trust in a config
//! system.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Sub-directory under the config root.
pub const GAMES_DIR: &str = "games";
/// Extension for a per-game settings file.
pub const EXTENSION: &str = "rfgame";
/// Magic + version, same shape as `rf_input`'s binding file and with the
/// same status: a config format, not a `CONTRACTS` §3 public commitment.
const MAGIC: &str = "RFGAME 1";

/// Which simulation mode a game opens in (law 6: a fresh install boots in
/// Accuracy Mode, so that is also the default here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Accuracy,
    Enhanced,
}

impl Mode {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Accuracy => "accuracy",
            Mode::Enhanced => "enhanced",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "accuracy" => Some(Mode::Accuracy),
            "enhanced" => Some(Mode::Enhanced),
            _ => None,
        }
    }
}

/// One game's settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GameSettings {
    pub mode: Mode,
    /// The W3-05a sprite-limit-bypass overlay, per game.
    pub sprite_overlay: bool,
    /// Shader name, `None` for the default pipeline. A string rather than
    /// an enum because W3-02a owns the shader set and this must not have to
    /// change when that lands.
    pub shader: Option<String>,
    /// Keys this build does not know, kept verbatim (module doc).
    unknown: BTreeMap<String, String>,
}

impl GameSettings {
    /// Serialize. Keys are sorted so a settings file does not churn between
    /// saves, which matters for anyone keeping their config in git.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut fields: BTreeMap<String, String> = self.unknown.clone();
        fields.insert("mode".to_string(), self.mode.name().to_string());
        fields.insert(
            "sprite_overlay".to_string(),
            self.sprite_overlay.to_string(),
        );
        if let Some(shader) = &self.shader {
            fields.insert("shader".to_string(), shader.clone());
        }

        let mut out = String::from(MAGIC);
        out.push('\n');
        for (key, value) in fields {
            out.push_str(&key);
            out.push('=');
            out.push_str(&value);
            out.push('\n');
        }
        out
    }

    /// Parse. Unrecognized keys are kept; an unrecognized *value* for a
    /// known key falls back to the default for that key rather than
    /// refusing the file, because one bad line should not cost a game its
    /// other settings.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(MAGIC) {
            return None;
        }
        let mut settings = GameSettings::default();
        for line in lines {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "mode" => settings.mode = Mode::from_name(value).unwrap_or_default(),
                "sprite_overlay" => settings.sprite_overlay = value == "true",
                "shader" => settings.shader = Some(value.to_string()),
                other => {
                    settings
                        .unknown
                        .insert(other.to_string(), value.to_string());
                }
            }
        }
        Some(settings)
    }

    /// Keys this build did not recognize — exposed so a test (and a curious
    /// developer) can see that they survived a round trip.
    #[must_use]
    pub fn unknown_keys(&self) -> &BTreeMap<String, String> {
        &self.unknown
    }
}

/// Where one game's settings live under `root`.
#[must_use]
pub fn settings_path(root: &Path, normalized_sha256: &str) -> PathBuf {
    root.join(crate::bindings_store::APP_DIR)
        .join(GAMES_DIR)
        .join(format!("{normalized_sha256}.{EXTENSION}"))
}

/// Load a game's settings, or the defaults when there is no file (or the
/// file is not a settings file). Never fails: a game with unreadable
/// settings must still be playable.
#[must_use]
pub fn load(root: &Path, normalized_sha256: &str) -> GameSettings {
    let path = settings_path(root, normalized_sha256);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| GameSettings::from_text(&text))
        .unwrap_or_default()
}

/// Save a game's settings.
///
/// # Errors
/// Returns the I/O error message so a caller can surface it; silently
/// failing to persist a setting the user just changed is worse than saying
/// so.
pub fn save(
    root: &Path,
    normalized_sha256: &str,
    settings: &GameSettings,
) -> Result<PathBuf, String> {
    let path = settings_path(root, normalized_sha256);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, settings.to_text()).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rf-w2-07-set-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// FR-FE-002 in its smallest honest form.
    #[test]
    fn settings_persist_keyed_by_hash_and_do_not_bleed_between_games() {
        let root = temp_root("persist");
        assert_eq!(load(&root, HASH_A), GameSettings::default());

        let settings = GameSettings {
            mode: Mode::Enhanced,
            sprite_overlay: true,
            shader: Some("crt-aperture".to_string()),
            ..GameSettings::default()
        };
        save(&root, HASH_A, &settings).expect("save");

        assert_eq!(load(&root, HASH_A), settings);
        assert_eq!(
            load(&root, HASH_B),
            GameSettings::default(),
            "another game must be unaffected"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The module doc's headline decision: an older build must not delete
    /// settings it cannot read.
    #[test]
    fn keys_this_build_does_not_know_survive_a_load_and_save() {
        let text = "RFGAME 1\n\
                    mode=enhanced\n\
                    future_toggle=42\n\
                    another_unknown=hello world\n";
        let settings = GameSettings::from_text(text).expect("parses");
        assert_eq!(settings.mode, Mode::Enhanced);
        assert_eq!(settings.unknown_keys().len(), 2);

        let written = settings.to_text();
        assert!(
            written.contains("future_toggle=42"),
            "an unknown key must be written back, not dropped: {written}"
        );
        assert!(written.contains("another_unknown=hello world"));

        let reparsed = GameSettings::from_text(&written).expect("round trips");
        assert_eq!(reparsed, settings);
    }

    #[test]
    fn a_bad_value_falls_back_to_the_default_without_costing_the_other_settings() {
        let text = "RFGAME 1\nmode=nonsense\nsprite_overlay=true\n";
        let settings = GameSettings::from_text(text).expect("parses");
        assert_eq!(settings.mode, Mode::Accuracy, "unknown mode -> default");
        assert!(settings.sprite_overlay, "the other setting still loaded");
    }

    #[test]
    fn a_file_that_is_not_a_settings_file_is_refused_and_load_gives_defaults() {
        assert_eq!(
            GameSettings::from_text("something else\nmode=enhanced\n"),
            None
        );

        let root = temp_root("notours");
        let path = settings_path(&root, HASH_A);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(load(&root, HASH_A), GameSettings::default());
        assert!(path.is_file(), "the user's file is left alone");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Law 6: a fresh install boots in Accuracy Mode, so the default for a
    /// game nobody has configured must be Accuracy — not whatever the last
    /// game used.
    #[test]
    fn the_default_mode_is_accuracy() {
        assert_eq!(GameSettings::default().mode, Mode::Accuracy);
        assert!(!GameSettings::default().sprite_overlay);
        assert_eq!(GameSettings::default().shader, None);
    }

    #[test]
    fn serialization_is_stable_across_saves() {
        let settings = GameSettings {
            mode: Mode::Enhanced,
            sprite_overlay: true,
            shader: Some("lcd-grid".to_string()),
            ..GameSettings::default()
        };
        assert_eq!(settings.to_text(), settings.to_text());
        assert!(!settings.to_text().contains('\r'));
    }
}
