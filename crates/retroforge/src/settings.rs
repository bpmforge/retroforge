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

use std::collections::BTreeMap;
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

    /// Ticket W21-04: the segmented control's one-word label.
    #[must_use]
    pub const fn short_label(self) -> &'static str {
        match self {
            ScaleMode::Integer => "Integer",
            ScaleMode::Fit => "Fit",
            ScaleMode::Stretch => "Stretch",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ScaleMode::Integer => "Integer (sharp)",
            ScaleMode::Fit => "Fit window",
            ScaleMode::Stretch => "Stretch",
        }
    }
}

/// Shape of one emulated pixel on screen (ticket W20-01;
/// `crate::play_view`). `Tv` is the NTSC 8:7 pixel aspect RENDERER.md §2
/// names as the default; `Square` is the raw 1:1 grid many players prefer
/// for pixel art.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelAspect {
    #[default]
    Tv,
    Square,
}

impl PixelAspect {
    pub const ALL: [PixelAspect; 2] = [PixelAspect::Tv, PixelAspect::Square];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            PixelAspect::Tv => "TV (8:7)",
            PixelAspect::Square => "Square pixels",
        }
    }
}

/// Which MetalFX mode (if any) Settings > Video should use (ticket W16-08;
/// `docs/design/ENHANCEMENT_WAVE_16.md` §7 Path B). A **scaler** choice,
/// not an enhancement-ladder toggle (CLAUDE.md law 6: it upscales the
/// pixels the core already produced, so Accuracy Mode may use it) — it
/// lives on `VideoSettings` next to `shader`/`scale_mode`, not in
/// `rf-enhance`'s trust ladder. `Off` is the only variant a fresh install
/// can boot with matches [`ScaleMode`]'s own "safest default" precedent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetalFxSetting {
    /// No MetalFX pass; the plain shader-chain scaler
    /// (`scale_mode`/`shader`) runs instead. Default — a fresh install
    /// boots with this off regardless of hardware (CLAUDE.md law 6,
    /// NON_GOALS #8), and it is also what a build/platform/device that
    /// fails [`rf_renderer::MetalFxAvailability::is_available`] is pinned
    /// to (the UI shows the reason from that type rather than letting the
    /// setting persist to a value the current run cannot honor).
    #[default]
    Off,
    /// `MTLFXSpatialScaler`, macOS + `metalfx` feature + supported device
    /// only.
    Spatial,
    /// Not offered by this build (ticket W16-08's temporal attempt did not
    /// reach a shippable, under-budget steady-state measurement — see
    /// `rf_renderer::metalfx`'s module doc and
    /// `crates/rf-renderer/tests/metalfx_bench.rs`'s
    /// `metalfx_temporal_upscale_attempt`). Kept as a variant (rather than
    /// deleted) so a settings file written by a future build that *does*
    /// ship it round-trips; this build's UI never lets a user select it.
    Temporal,
}

/// Video settings (FRONTEND_UI §2: "scaling, shaders, vsync, display mode").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoSettings {
    pub scale_mode: ScaleMode,
    /// Pixel aspect (ticket W20-01). `#[serde(default)]` on the struct
    /// means a settings file written before this field existed loads as
    /// `Tv`.
    pub pixel_aspect: PixelAspect,
    /// Shader by manifest id (`crate::shader_select::kind_from_id`),
    /// `None` for the plain picture. Still a string so a settings file
    /// naming a shader this build lacks loads (and shows as none) instead
    /// of failing to parse.
    pub shader: Option<String>,
    pub vsync: bool,
    /// Ticket W20-15: the corner performance overlay (FPS, frame-time
    /// sparkline, audio buffer). Off by default.
    pub perf_overlay: bool,
    /// Ticket W20-15: show the controller state sent to the core. Off by
    /// default.
    pub input_display: bool,
    /// Ticket W20-19: fill the letterbox with a darkened average of the
    /// picture's edges. Off by default.
    pub ambient_glow: bool,
    /// Ticket W21-04: a procedural console bezel around the picture. Off
    /// by default; mutually exclusive with `ambient_glow` through
    /// [`VideoSettings::set_surround`].
    pub bezel: bool,
    /// MetalFX scaler choice (ticket W16-08). `Off` by default; see
    /// [`MetalFxSetting`].
    pub metalfx: MetalFxSetting,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            scale_mode: ScaleMode::default(),
            pixel_aspect: PixelAspect::default(),
            shader: None,
            // On by default: tearing is the more objectionable artifact,
            // and W2-05's audio clock (not vsync) is what paces the
            // emulator, so leaving it on costs no timing accuracy.
            vsync: true,
            perf_overlay: false,
            input_display: false,
            ambient_glow: false,
            bezel: false,
            metalfx: MetalFxSetting::default(),
        }
    }
}

/// Ticket W21-04: what fills the space around the picture — the review's
/// "Around the picture: Black / Ambient glow / Bezel".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surround {
    Black,
    Glow,
    Bezel,
}

impl Surround {
    pub const ALL: [Surround; 3] = [Surround::Black, Surround::Glow, Surround::Bezel];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Surround::Black => "Black",
            Surround::Glow => "Ambient glow",
            Surround::Bezel => "Bezel",
        }
    }
}

impl VideoSettings {
    /// Ticket W21-04: the surround these flags describe. Bezel wins if a
    /// hand-edited file sets both.
    #[must_use]
    pub fn surround(&self) -> Surround {
        if self.bezel {
            Surround::Bezel
        } else if self.ambient_glow {
            Surround::Glow
        } else {
            Surround::Black
        }
    }

    pub fn set_surround(&mut self, surround: Surround) {
        self.ambient_glow = surround == Surround::Glow;
        self.bezel = surround == Surround::Bezel;
    }
}

/// Ticket W21-04: which layer a video change is written to — the review's
/// "Apply to: This game / All NES / Everything". Read back in the order
/// game, console, everything: the narrowest layer that exists wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoScope {
    Game,
    Console,
    Everything,
}

impl VideoScope {
    pub const ALL: [VideoScope; 3] = [
        VideoScope::Game,
        VideoScope::Console,
        VideoScope::Everything,
    ];

    /// `console` is the running game's console name ("NES"), if known.
    #[must_use]
    pub fn label(self, console: Option<&str>) -> String {
        match self {
            VideoScope::Game => "This game".to_owned(),
            VideoScope::Console => format!("All {}", console.unwrap_or("of this console")),
            VideoScope::Everything => "Everything".to_owned(),
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
    /// Ticket W15-05, `UX_WAVE_15.md` §4.3: a folder of user-supplied box
    /// art, matched by normalized title (`crate::thumbnail::find_user_art`)
    /// when a game has no save-state screenshot or first-frame capture
    /// yet. `None` (the default) means that source contributes nothing —
    /// never an error, since most players will never set this.
    pub art_folder: Option<PathBuf>,
    /// Ticket W15-09, ruling D-011: "Fetch box art from the internet",
    /// off by default. Gates ALL network use in the art pipeline — with
    /// this `false` (a fresh install's state, NON_GOALS #6), `crate::art`
    /// never builds a request, never spawns the fetch worker, and the
    /// fourth thumbnail source (`ThumbnailSource::Fetched`) never wins.
    pub fetch_art: bool,
    /// Ticket W15-09: the fetched-art cache's own size cap in megabytes,
    /// independent of `cache_cap_mb` (`UX_WAVE_15.md` §4: "a cache cap...
    /// alongside the existing `rf-cache` size cap") — a user who fills
    /// their stitched-canvas cache should not thereby evict cover art, and
    /// vice versa.
    pub art_cache_cap_mb: u64,
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
            art_folder: None,
            // Off by default (D-011, NON_GOALS #6): a fresh install makes
            // zero network requests until a player opts in explicitly.
            fetch_art: false,
            // 256 MB (plan.json W15-09 acceptance's stated default):
            // enough for a few thousand small boxart PNGs, small enough
            // that opting in is not itself a disk-space surprise.
            art_cache_cap_mb: 256,
        }
    }
}

/// Ticket W15-05, `UX_WAVE_15.md` §3: the library screen's own settings —
/// currently just the Grid/List toggle, kept as its own `[library]`
/// section rather than folded into `paths` since it is a view preference,
/// not a filesystem location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LibrarySettings {
    pub view: crate::library::LibraryView,
}

/// Ticket W20-04: the windowed size to reopen at. `None` = the built-in
/// [`crate::app::WINDOW_SIZE`]. Not written while fullscreen, so leaving
/// fullscreen and quitting does not make the next launch screen-sized.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WindowSettings {
    pub inner_size: Option<[f32; 2]>,
}

impl WindowSettings {
    /// The size to open at: the saved one, clamped to at least
    /// [`crate::app::MIN_WINDOW_SIZE`] and to something finite, or the
    /// default.
    #[must_use]
    pub fn startup_size(&self) -> [f32; 2] {
        let [mw, mh] = crate::app::MIN_WINDOW_SIZE;
        match self.inner_size {
            Some([w, h]) if w.is_finite() && h.is_finite() => [w.max(mw), h.max(mh)],
            _ => crate::app::WINDOW_SIZE,
        }
    }
}

/// Ticket W20-13: play-session features with a cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PlaySettings {
    /// Keep a rewind history (memory cost shown in Settings). Off by
    /// default — SAVE_STATES.md §4 is explicit that the cost is real.
    pub rewind: bool,
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
    /// Ticket W15-05: the Grid/List toggle, persisted (acceptance 2).
    pub library: LibrarySettings,
    /// Ticket W20-04.
    pub window: WindowSettings,
    /// Ticket W20-02: per-shader parameter values.
    pub shaders: crate::shader_select::ShaderSettings,
    /// Ticket W20-13.
    pub play: PlaySettings,
    /// Ticket W21-04: video settings for one console ("NES", "SNES"),
    /// overriding `video` for its games.
    pub video_by_console: BTreeMap<String, VideoSettings>,
    /// Ticket W21-04: video settings for one game, keyed by its normalized
    /// ROM SHA-256 (principle 5: everything keyed by ROM hash), overriding
    /// both of the above.
    pub video_by_game: BTreeMap<String, VideoSettings>,
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
    library: LibrarySettings,
    window: WindowSettings,
    shaders: crate::shader_select::ShaderSettings,
    play: PlaySettings,
    video_by_console: BTreeMap<String, VideoSettings>,
    video_by_game: BTreeMap<String, VideoSettings>,
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
            library: self.library,
            window: self.window,
            shaders: self.shaders.clone(),
            play: self.play,
            video_by_console: self.video_by_console.clone(),
            video_by_game: self.video_by_game.clone(),
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
            if !matches!(
                key.as_str(),
                "video"
                    | "audio"
                    | "paths"
                    | "accessibility"
                    | "library"
                    | "window"
                    | "shaders"
                    | "play"
                    | "video_by_console"
                    | "video_by_game"
            ) {
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
            library: section("library").try_into().unwrap_or_default(),
            window: section("window").try_into().unwrap_or_default(),
            shaders: section("shaders").try_into().unwrap_or_default(),
            play: section("play").try_into().unwrap_or_default(),
            video_by_console: section("video_by_console").try_into().unwrap_or_default(),
            video_by_game: section("video_by_game").try_into().unwrap_or_default(),
            unknown,
        })
    }

    /// Ticket W21-04: the video settings in force for a game, and the
    /// layer they came from — the game's own, else its console's, else
    /// everything's.
    #[must_use]
    pub fn resolve_video(
        &self,
        console: Option<&str>,
        game: Option<&str>,
    ) -> (VideoSettings, VideoScope) {
        if let Some(v) = game.and_then(|g| self.video_by_game.get(g)) {
            return (v.clone(), VideoScope::Game);
        }
        if let Some(v) = console.and_then(|c| self.video_by_console.get(c)) {
            return (v.clone(), VideoScope::Console);
        }
        (self.video.clone(), VideoScope::Everything)
    }

    /// Ticket W21-04: write `video` into `scope`'s layer. Choosing a wider
    /// scope drops this game's narrower overrides, so what was chosen is
    /// what applies.
    pub fn store_video(
        &mut self,
        scope: VideoScope,
        video: &VideoSettings,
        console: Option<&str>,
        game: Option<&str>,
    ) {
        match (scope, console, game) {
            (VideoScope::Game, _, Some(g)) => {
                self.video_by_game.insert(g.to_owned(), video.clone());
            }
            (VideoScope::Console, Some(c), _) => {
                if let Some(g) = game {
                    self.video_by_game.remove(g);
                }
                self.video_by_console.insert(c.to_owned(), video.clone());
            }
            _ => {
                if let Some(g) = game {
                    self.video_by_game.remove(g);
                }
                if let Some(c) = console {
                    self.video_by_console.remove(c);
                }
                self.video = video.clone();
            }
        }
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

    /// Ticket W20-04: the window size round-trips, and a nonsense or tiny
    /// one opens at something usable.
    #[test]
    fn window_size_round_trips_and_is_clamped_on_startup() {
        let mut settings = AppSettings::default();
        settings.window.inner_size = Some([1280.0, 900.0]);
        let back = AppSettings::from_toml(&settings.to_toml().unwrap()).unwrap();
        assert_eq!(back.window.startup_size(), [1280.0, 900.0]);
        let tiny = WindowSettings {
            inner_size: Some([10.0, 10.0]),
        };
        assert_eq!(tiny.startup_size(), crate::app::MIN_WINDOW_SIZE);
        assert_eq!(
            WindowSettings::default().startup_size(),
            crate::app::WINDOW_SIZE
        );
        let nan = WindowSettings {
            inner_size: Some([f32::NAN, 600.0]),
        };
        assert_eq!(nan.startup_size(), crate::app::WINDOW_SIZE);
    }

    /// Ticket W21-04: game beats console beats everything; storing at a
    /// wider scope removes this game's narrower overrides; both maps
    /// round-trip through the file.
    #[test]
    fn video_scopes_resolve_narrowest_first_and_round_trip() {
        let mut s = AppSettings::default();
        let crt = VideoSettings {
            shader: Some("crt".into()),
            ..Default::default()
        };
        let fit = VideoSettings {
            scale_mode: ScaleMode::Fit,
            ..Default::default()
        };
        assert_eq!(
            s.resolve_video(Some("NES"), Some("aa")).1,
            VideoScope::Everything
        );
        s.store_video(VideoScope::Console, &crt, Some("NES"), Some("aa"));
        assert_eq!(
            s.resolve_video(Some("NES"), Some("bb")),
            (crt.clone(), VideoScope::Console)
        );
        assert_eq!(
            s.resolve_video(Some("SNES"), None).1,
            VideoScope::Everything
        );
        s.store_video(VideoScope::Game, &fit, Some("NES"), Some("aa"));
        assert_eq!(
            s.resolve_video(Some("NES"), Some("aa")),
            (fit.clone(), VideoScope::Game)
        );

        let back = AppSettings::from_toml(&s.to_toml().unwrap()).unwrap();
        assert_eq!(
            back.resolve_video(Some("NES"), Some("aa")),
            (fit, VideoScope::Game)
        );
        assert_eq!(back.resolve_video(Some("NES"), Some("bb")).0, crt);

        s.store_video(VideoScope::Everything, &crt, Some("NES"), Some("aa"));
        assert!(s.video_by_game.is_empty() && s.video_by_console.is_empty());
        assert_eq!(
            s.resolve_video(Some("NES"), Some("aa")),
            (crt, VideoScope::Everything)
        );
    }

    #[test]
    fn surround_flags_are_exclusive() {
        let mut v = VideoSettings::default();
        assert_eq!(v.surround(), Surround::Black);
        for s in Surround::ALL {
            v.set_surround(s);
            assert_eq!(v.surround(), s);
        }
        v.ambient_glow = true;
        assert_eq!(
            v.surround(),
            Surround::Bezel,
            "bezel wins a hand-edited clash"
        );
    }
}
