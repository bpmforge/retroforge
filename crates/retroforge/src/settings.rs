//! App-wide settings: Video, Audio and Paths (ticket W2-08; FRONTEND_UI
//! §2's Settings tree and §7's persistence layout).
//!
//! ## `settings.toml`, and what "human-inspectable" costs
//!
//! FRONTEND_UI §7 is specific: `settings.toml` under the platform config
//! dir, "all human-inspectable". TOML is already sanctioned
//! (`docs/TECH_STACK.md` §2's Config/profiles row) and already used by
//! `rf-profiles` and `rf-harness`, so this adds no new dependency class.
//!
//! **Unknown keys are preserved**, the same commitment
//! `crate::game_settings` makes and for the same reason: this file will
//! grow (W3-02a's shaders, W4-04's plugins, W4-05's toggles), builds will
//! differ, and an older build that silently deleted a newer build's
//! settings would be doing quiet damage. The implementation keeps the
//! parsed `toml::Table` and overwrites only the keys it understands, so a
//! key this build has never heard of survives a load/save round trip
//! untouched.
//!
//! ## What this ticket absorbed
//!
//! W2-06 and W2-07 each needed one small piece of persistence before an
//! app-wide settings system existed, and each said in its own notes that
//! W2-08 would absorb it. This is that: `[paths] library_folders` is now
//! the home of the library roots W2-07 kept in `library.rflib`, and
//! `crate::library_roots` reads through to here. Bindings stay in their own
//! file, deliberately — they are a different shape (a table of
//! key-to-button rows a user hand-edits) and a different lifetime.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name under the config directory (FRONTEND_UI §7).
pub const FILE_NAME: &str = "settings.toml";

/// How the emulated picture is fitted to the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleMode {
    /// Whole-number scaling only, letterboxed — every emulated pixel the
    /// same size, which is the only mode that cannot introduce shimmer on
    /// a 2D grid. The default for that reason.
    #[default]
    Integer,
    /// Fill the window, preserving aspect ratio.
    Fit,
    /// Fill the window, ignoring aspect ratio.
    Stretch,
}

impl ScaleMode {
    pub const ALL: [ScaleMode; 3] = [ScaleMode::Integer, ScaleMode::Fit, ScaleMode::Stretch];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ScaleMode::Integer => "Integer (sharp)",
            ScaleMode::Fit => "Fit window",
            ScaleMode::Stretch => "Stretch",
        }
    }
}

/// Video settings (FRONTEND_UI §2: "scaling, shaders, vsync, display mode").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoSettings {
    pub scale_mode: ScaleMode,
    /// Shader name, `None` for the plain pipeline. A string because W3-02a
    /// owns the shader set; this must not need changing when that lands.
    pub shader: Option<String>,
    pub vsync: bool,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            scale_mode: ScaleMode::default(),
            shader: None,
            // On by default: tearing is the more objectionable artifact,
            // and W2-05's audio clock (not vsync) is what paces the
            // emulator, so leaving it on costs no timing accuracy.
            vsync: true,
        }
    }
}

/// Audio settings (FRONTEND_UI §2: "device, latency, volume").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Output device by name; `None` means the system default, which is
    /// what almost everyone wants and what survives a device being
    /// unplugged.
    pub device: Option<String>,
    /// Target buffer latency. Defaults to `crate::audio_out::LATENCY_MS`,
    /// whose own doc explains the 40 ms choice.
    pub latency_ms: u32,
    /// Linear gain, 0.0..=1.0.
    pub volume: f32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            device: None,
            latency_ms: crate::audio_out::LATENCY_MS,
            volume: 1.0,
        }
    }
}

/// Paths settings (FRONTEND_UI §2: "library folders, cache location & size
/// cap").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PathSettings {
    /// The library roots W2-07 scans. Stored as the user chose them, never
    /// canonicalized — containment resolves at scan time, where it must,
    /// since a symlink can appear after a folder was configured
    /// (`crate::library`'s module doc).
    ///
    /// Ticket W14-01: each root may declare which console it holds.
    /// [`crate::library::LibraryRoot`] is `#[serde(untagged)]`, so a
    /// settings file written before that ticket — a plain array of path
    /// strings — still deserializes, and one that never sets a hint still
    /// round-trips as plain strings.
    pub library_folders: Vec<crate::library::LibraryRoot>,
    /// Cache location; `None` means the default under the config dir.
    pub cache_dir: Option<PathBuf>,
    /// LRU cache cap in megabytes.
    pub cache_cap_mb: u64,
}

impl Default for PathSettings {
    fn default() -> Self {
        Self {
            library_folders: Vec::new(),
            cache_dir: None,
            // 2 GiB: big enough for stitched canvases of a decent-sized
            // library, small enough that nobody discovers it by running out
            // of disk.
            cache_cap_mb: 2048,
        }
    }
}

/// Everything in `settings.toml`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppSettings {
    pub video: VideoSettings,
    pub audio: AudioSettings,
    pub paths: PathSettings,
    /// Ticket W10-01. `crate::accessibility` existed since W8-04 with no
    /// home in the settings file and no consumer anywhere, so its UI
    /// scale and high-contrast palette were unreachable by any user and
    /// its WCAG tests measured a palette egui never saw. This is the
    /// home; `crate::app` reads it into `ctx.set_visuals`.
    pub accessibility: crate::accessibility::AccessibilitySettings,
    /// Tables and keys this build does not know, kept verbatim so a newer
    /// build's settings survive an older build touching the file (module
    /// doc).
    unknown: toml::Table,
}

/// The typed half, used for (de)serialization; the untyped half lives in
/// [`AppSettings::unknown`].
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
struct KnownSettings {
    video: VideoSettings,
    audio: AudioSettings,
    paths: PathSettings,
    accessibility: crate::accessibility::AccessibilitySettings,
}

impl AppSettings {
    /// Serialize to TOML, re-emitting every unknown table alongside the
    /// known ones.
    ///
    /// # Errors
    /// Returns a message if the settings cannot be encoded, which for these
    /// types means a bug rather than user input.
    pub fn to_toml(&self) -> Result<String, String> {
        let known = KnownSettings {
            accessibility: self.accessibility.clone(),
            video: self.video.clone(),
            audio: self.audio.clone(),
            paths: self.paths.clone(),
        };
        let mut table = toml::Table::try_from(known).map_err(|e| e.to_string())?;
        for (key, value) in &self.unknown {
            table.entry(key.clone()).or_insert_with(|| value.clone());
        }
        toml::to_string_pretty(&table).map_err(|e| e.to_string())
    }

    /// Parse, keeping anything this build does not recognize.
    ///
    /// # Errors
    /// Returns a message for TOML that does not parse at all. A file that
    /// parses but has a wrong-typed known field falls back to that field's
    /// default rather than refusing the whole file — one bad line must not
    /// cost every other setting.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;

        let mut unknown = toml::Table::new();
        for (key, value) in &table {
            if !matches!(key.as_str(), "video" | "audio" | "paths" | "accessibility") {
                unknown.insert(key.clone(), value.clone());
            }
        }

        // Per-section fallback: a malformed [audio] must not cost the user
        // their [video] settings.
        let section = |name: &str| -> toml::Value {
            table
                .get(name)
                .cloned()
                .unwrap_or_else(|| toml::Value::Table(toml::Table::new()))
        };
        Ok(Self {
            // `normalized` on the way IN, not only at the widget: the
            // module doc's point is that a settings file with
            // `ui_scale = nan` or `= 40` bricks the window, and a user
            // who cannot read the UI cannot open Settings to undo it.
            accessibility: {
                let a: crate::accessibility::AccessibilitySettings =
                    section("accessibility").try_into().unwrap_or_default();
                a.normalized()
            },
            video: section("video").try_into().unwrap_or_default(),
            audio: section("audio").try_into().unwrap_or_default(),
            paths: section("paths").try_into().unwrap_or_default(),
            unknown,
        })
    }

    /// Keys this build did not recognize — exposed so a test can prove they
    /// survived.
    #[must_use]
    pub fn unknown_keys(&self) -> &toml::Table {
        &self.unknown
    }
}

/// Where `settings.toml` lives under `config_root`.
#[must_use]
pub fn settings_path(config_root: &Path) -> PathBuf {
    config_root
        .join(crate::bindings_store::APP_DIR)
        .join(FILE_NAME)
}

/// Load app settings, falling back to defaults for anything unreadable.
/// Never fails: an app that refuses to start because its settings file has
/// a typo is worse than one that starts with defaults and says so.
#[must_use]
pub fn load(config_root: &Path) -> (AppSettings, Option<String>) {
    let path = settings_path(config_root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (AppSettings::default(), None);
    };
    match AppSettings::from_toml(&text) {
        Ok(settings) => (settings, None),
        Err(e) => (
            AppSettings::default(),
            Some(format!(
                "{} could not be parsed ({e}); using defaults. Your file was left untouched.",
                path.display()
            )),
        ),
    }
}

/// Save app settings.
///
/// # Errors
/// Returns the I/O or encoding error message.
pub fn save(config_root: &Path, settings: &AppSettings) -> Result<PathBuf, String> {
    let path = settings_path(config_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, settings.to_toml()?).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("rf-w2-08-{label}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn settings_round_trip_through_a_real_file() {
        let root = temp_root("roundtrip");
        let (loaded, problem) = load(&root);
        assert_eq!(loaded, AppSettings::default(), "no file yet");
        assert!(problem.is_none());

        let mut settings = AppSettings::default();
        settings.video.scale_mode = ScaleMode::Fit;
        settings.video.shader = Some("crt-aperture".to_string());
        settings.video.vsync = false;
        settings.audio.latency_ms = 25;
        settings.audio.volume = 0.6;
        settings.audio.device = Some("Speakers".to_string());
        settings.paths.library_folders = vec![crate::library::LibraryRoot::Bare(PathBuf::from(
            "/roms/nes",
        ))];
        settings.paths.cache_cap_mb = 512;

        save(&root, &settings).expect("save");
        let (reloaded, problem) = load(&root);
        assert!(problem.is_none());
        assert_eq!(reloaded, settings);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The module doc's headline commitment: a newer build's settings must
    /// survive an older build opening the file.
    #[test]
    fn unknown_tables_and_keys_survive_a_load_and_save() {
        let text = "\
[video]
scale_mode = \"fit\"

[plugins]
enabled = [\"future-thing\"]

[experimental]
nested = { deeper = 1 }
";
        let settings = AppSettings::from_toml(text).expect("parses");
        assert_eq!(settings.video.scale_mode, ScaleMode::Fit);
        assert_eq!(settings.unknown_keys().len(), 2);

        let written = settings.to_toml().expect("serializes");
        assert!(written.contains("future-thing"), "{written}");
        assert!(written.contains("deeper"), "{written}");

        let reparsed = AppSettings::from_toml(&written).expect("round trips");
        assert_eq!(reparsed, settings);

        let _ = std::fs::remove_dir_all(temp_root("noop"));
    }

    /// One malformed section must not cost the user the others.
    #[test]
    fn a_wrong_typed_section_falls_back_without_taking_the_rest_with_it() {
        let text = "\
[video]
scale_mode = \"stretch\"

[audio]
latency_ms = \"not a number\"
";
        let settings = AppSettings::from_toml(text).expect("still parses");
        assert_eq!(
            settings.video.scale_mode,
            ScaleMode::Stretch,
            "video survived"
        );
        assert_eq!(
            settings.audio,
            AudioSettings::default(),
            "audio fell back to defaults rather than refusing the file"
        );
    }

    #[test]
    fn a_file_that_is_not_toml_at_all_is_reported_and_defaults_are_used() {
        let root = temp_root("garbage");
        let path = settings_path(&root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not = = toml").unwrap();

        let (settings, problem) = load(&root);
        assert_eq!(settings, AppSettings::default());
        let problem = problem.expect("the user must be told");
        assert!(problem.contains("left untouched"), "{problem}");
        assert!(path.is_file(), "and the file really is left alone");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The defaults are choices, not accidents — each is argued in its
    /// type's doc, and pinned here so a change is deliberate.
    #[test]
    fn the_defaults_are_the_documented_ones() {
        let settings = AppSettings::default();
        assert_eq!(settings.video.scale_mode, ScaleMode::Integer);
        assert!(settings.video.vsync);
        assert_eq!(settings.video.shader, None);
        assert_eq!(settings.audio.latency_ms, crate::audio_out::LATENCY_MS);
        assert_eq!(settings.audio.volume, 1.0);
        assert_eq!(settings.audio.device, None);
        assert!(settings.paths.library_folders.is_empty());
        assert_eq!(settings.paths.cache_cap_mb, 2048);
    }

    /// W2-07's library roots now live here (module doc): the settings file
    /// is the one place a folder list is stored.
    #[test]
    fn library_folders_persist_through_the_settings_file() {
        let root = temp_root("folders");
        let mut settings = AppSettings::default();
        settings.paths.library_folders = vec![
            crate::library::LibraryRoot::Bare(PathBuf::from("/roms/nes")),
            crate::library::LibraryRoot::Bare(PathBuf::from("/mnt/nas/snes")),
        ];
        save(&root, &settings).expect("save");

        let (reloaded, _) = load(&root);
        assert_eq!(
            reloaded.paths.library_folders,
            settings.paths.library_folders
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
