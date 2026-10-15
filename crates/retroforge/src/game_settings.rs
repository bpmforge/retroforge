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
    /// "core in cycle-accurate config, enhancement runtime not
    /// subscribed, renderer in original pipeline. Reference for all
    /// tests." The default, and law 6's "a fresh install boots in
    /// Accuracy Mode".
    #[default]
    Accuracy,
    /// "core may enable documented fast paths … that pass the
    /// compatibility test suite; still deterministic." The switch itself
    /// is W3-07's `CoreConfig::accuracy_mode`.
    Compatibility,
    /// "enhancement runtime subscribed; user-selected features on."
    Enhanced,
    /// "everything exposed: traces, viewers, breakpoints, frame stepping;
    /// enhancement optional."
    ResearchDebug,
    /// "Enhanced + a matched profile; unlocks profile-gated features
    /// (full-level view, HUD split, entity overlays)."
    ///
    /// Selectable without a profile — the mode is what the user *asked
    /// for*, and the UI explains what is unavailable rather than silently
    /// refusing the choice. [`Mode::profile_gated_features_unlocked`] is
    /// the question the feature list actually asks.
    GameAware,
}

impl Mode {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Accuracy => "accuracy",
            Mode::Compatibility => "compatibility",
            Mode::Enhanced => "enhanced",
            Mode::ResearchDebug => "research-debug",
            Mode::GameAware => "game-aware",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "accuracy" => Some(Mode::Accuracy),
            "compatibility" => Some(Mode::Compatibility),
            "enhanced" => Some(Mode::Enhanced),
            "research-debug" => Some(Mode::ResearchDebug),
            "game-aware" => Some(Mode::GameAware),
            // Deliberately no catch-all mapping to a *permissive* mode: an
            // unrecognised name (an older file, a typo, a newer build's
            // mode) resolves to `Accuracy` at the call site's
            // `unwrap_or_default()`, which is FR-MODE-003's direction —
            // enhancement is never on because a name failed to parse.
            _ => None,
        }
    }
}

impl Mode {
    /// Ticket W22-01 (`docs/design/UX_WAVE_22.md`, Brad 2026-10-07): the
    /// name a PLAYER sees — "Original" for Accuracy, which reads as what it
    /// is (the unmodified machine) rather than as a quality grade.
    /// [`Self::display_name`] stays the code, docs and Debug name.
    #[must_use]
    pub const fn player_name(self) -> &'static str {
        match self {
            Mode::Accuracy => "Original",
            Mode::Compatibility => "Compatibility",
            Mode::Enhanced => "Enhanced",
            Mode::ResearchDebug => "Research/Debug",
            Mode::GameAware => "Game-Aware",
        }
    }

    /// Ticket W22-01: the one line under each picker card.
    #[must_use]
    pub const fn player_blurb(self) -> &'static str {
        match self {
            Mode::Accuracy => "Exactly what the console drew. Nothing added.",
            Mode::Enhanced => "Fixes that work on any game, like sprite dropout and flicker.",
            Mode::GameAware => {
                "Uses a profile of this game to widen it, map it and build it in 3D."
            }
            Mode::Compatibility => "Documented fast paths; still deterministic.",
            Mode::ResearchDebug => "Everything on, for research.",
        }
    }

    /// Ticket W22-01: the modes the player's picker offers, in order.
    /// Compatibility and Research/Debug stay in Enhance › Game settings.
    pub const PICKER: [Mode; 3] = [Mode::Accuracy, Mode::Enhanced, Mode::GameAware];

    /// The code/docs/Debug name (Game settings window, Enhance workspace).
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Mode::Accuracy => "Accuracy",
            Mode::Compatibility => "Compatibility",
            Mode::Enhanced => "Enhanced",
            Mode::ResearchDebug => "Research/Debug",
            Mode::GameAware => "Game-Aware",
        }
    }

    /// Does this mode subscribe the enhancement runtime at all?
    ///
    /// This is the FR-MODE-003 question — "enhancement never on
    /// silently" — asked once, here, rather than re-derived by every
    /// caller that needs it. `Research/Debug` says "enhancement
    /// optional" (§4), which means *not on by itself*: a debugging mode
    /// that quietly changed the picture would defeat its own purpose.
    #[must_use]
    pub const fn enhancement_active(self) -> bool {
        matches!(self, Mode::Enhanced | Mode::GameAware)
    }

    /// Only `Game-Aware` unlocks profile-gated features, and only §4's
    /// "Enhanced **+ a matched profile**" — so the mode alone is not
    /// enough and the caller must pass whether one matched.
    #[must_use]
    pub const fn profile_gated_features_unlocked(self, profile_matched: bool) -> bool {
        matches!(self, Mode::GameAware) && profile_matched
    }

    /// All five, in ARCHITECTURE §4's order, for the mode picker.
    #[must_use]
    pub const fn all() -> [Mode; 5] {
        [
            Mode::Accuracy,
            Mode::Compatibility,
            Mode::Enhanced,
            Mode::ResearchDebug,
            Mode::GameAware,
        ]
    }
}

/// Ticket W30-08: how an overclocked chip's speed is chosen.
///
/// `Governed` (the default) only ever REMOVES SLOWDOWN: the speed governor
/// (`rf_enhance::speed_governor`) raises the clock while the game runs short
/// of its normal rate and lowers it again the moment it does not, so the game
/// never ticks faster than it was designed to; the slider is its ceiling.
/// `Fixed` always runs at the slider's speed, which makes some games run
/// faster than intended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverclockMode {
    #[default]
    Governed,
    Fixed,
}

impl OverclockMode {
    /// The settings-file spelling.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            OverclockMode::Governed => "governed",
            OverclockMode::Fixed => "fixed",
        }
    }

    /// Parse [`Self::key`]; anything else is `None` (the caller keeps the
    /// default, which is the safe one).
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "governed" => Some(OverclockMode::Governed),
            "fixed" => Some(OverclockMode::Fixed),
            _ => None,
        }
    }
}

/// One game's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameSettings {
    pub mode: Mode,
    /// The W3-05a sprite-limit-bypass overlay, per game.
    pub sprite_overlay: bool,
    /// Ticket W4-05 (FR-ENH-010): the remaining per-game enhancement
    /// toggles. All default to OFF — FR-MODE-003's "enhancement never on
    /// silently" is a property of the DEFAULT, not of a check somewhere.
    pub deflicker: bool,
    pub widescreen_decoded: bool,
    pub full_level_view: bool,
    /// Ticket W16-06 (`docs/design/ENHANCEMENT_WAVE_16.md` §5): Diorama
    /// tier two, "walls pop up" — off by default, same law-6 posture as
    /// every toggle above.
    pub diorama: bool,
    /// Ticket W16-14 (`docs/design/ENHANCEMENT_WAVE_16.md` §9): "Mode 7 as
    /// 3D" — off by default, same law-6 posture as `diorama` above. Gated
    /// on live BG-mode-7 game state rather than a matched profile (see
    /// `enhance_ui::feature_rows`'s own doc), so it is a separate flag
    /// from `diorama` even though both composite through the same live
    /// view slot.
    pub mode7_ground: bool,
    /// Ticket W20-17: profile-declared loading fast-forward (FR-ENH-008,
    /// `rf_enhance::loading`) — off by default (law 6).
    pub loading_fast_forward: bool,
    /// Ticket W30-01: run the Super FX chip faster than the cartridge's
    /// 21.4 MHz. This changes what the game does (a simulation change, like
    /// a `[mods]` patch), so it is off by default (law 6) and only takes
    /// effect in Enhanced-class modes.
    pub superfx_overclock: bool,
    /// Ticket W30-01: the overclock percentage used when
    /// [`Self::superfx_overclock`] is on. Valid 200..=800 in steps of 100;
    /// clamped on load. Defaults to 200.
    pub superfx_overclock_pct: u16,
    /// Ticket W30-01c: whether the speed governor chooses the Super FX
    /// clock (the default; [`Self::superfx_overclock_pct`] is then its
    /// ceiling) or it is fixed at that speed.
    pub superfx_overclock_mode: OverclockMode,
    /// Ticket W30-08: run the SNES main CPU (65816) faster to remove
    /// slowdown. A simulation change by design, so off by default (law 6)
    /// and Enhanced-only. [`Self::cpu_overclock_pct`] is the ceiling
    /// (200..=400, clamped on load, default 200) and
    /// [`Self::cpu_overclock_mode`] says whether the governor chooses the
    /// speed or it is fixed at the ceiling.
    pub cpu_overclock: bool,
    pub cpu_overclock_pct: u16,
    pub cpu_overclock_mode: OverclockMode,
    /// Ticket W30-08: the same trio for the SA-1 coprocessor.
    pub sa1_overclock: bool,
    pub sa1_overclock_pct: u16,
    pub sa1_overclock_mode: OverclockMode,
    /// Ticket W30-09: smoother SNES sound — interpolate the S-DSP's voices
    /// with [`Self::snes_interpolation_mode`] instead of the hardware
    /// Gaussian filter, for what you hear only. Presentation, not
    /// simulation (the echo and every register keep the hardware values),
    /// but the hardware filter is the reference, so it is off by default
    /// (law 6) and Enhanced-only.
    pub snes_interpolation: bool,
    /// Ticket W30-09: which smoother filter. Never
    /// [`rf_snes::apu::dsp::Interpolation::Gaussian`] (that is "off");
    /// defaults to cubic and is coerced to it on load.
    pub snes_interpolation_mode: rf_snes::apu::dsp::Interpolation,
    /// Ticket W30-08: extra CPU-only vblank lines for the NES, 0 (off)
    /// ..=240; clamped on load.
    pub nes_extra_lines: u16,
    /// Ticket W30-07: run-ahead, in frames (0 = off, 1 or 2). Hides the
    /// game's own input lag by showing a frame from slightly ahead; the real
    /// simulation is untouched (presentation only), but it costs extra CPU,
    /// so it is off by default (law 6). Clamped to 0..=2 on load.
    pub run_ahead: u8,
    /// Ticket W30-02: redraw Mode 7 floors at a finer step (presentation
    /// only; the simulation is untouched). Off by default (law 6).
    pub hd_mode7: bool,
    /// Ticket W30-02: output pixels per native pixel, 2..=4; clamped on load.
    pub hd_mode7_scale: u8,
    /// Ticket W30-02: average 2x2 sub-samples per output pixel (costs CPU).
    pub hd_mode7_supersample: bool,
    /// Ticket W30-02: blend the Mode 7 matrix between scanlines. On by
    /// default; only the OFF state is persisted.
    pub hd_mode7_perspective: bool,
    /// Ticket W30-04: the HD-2D lens and lighting effects (presentation
    /// only, applied to the finished picture). Each is off by default
    /// (law 6) and has a strength stored as an integer percent `0..=100`
    /// (`*_pct`; an `f32` would cost this struct its `Eq`).
    pub lens_bloom: bool,
    pub lens_bloom_pct: u8,
    pub lens_tiltshift: bool,
    pub lens_tiltshift_pct: u8,
    pub lens_shadows: bool,
    pub lens_shadows_pct: u8,
    pub lens_grade: bool,
    pub lens_grade_pct: u8,
    /// The weather overlay: `Weather::None` is off; the percent is its
    /// density.
    pub lens_weather: rf_renderer::lens::Weather,
    pub lens_weather_pct: u8,
    /// Time of day for the grade, percent of a day: 0 midnight, 50 noon.
    pub lens_time_of_day: u8,
    /// Take the time of day from the clock instead of
    /// [`Self::lens_time_of_day`].
    pub lens_follow_clock: bool,
    /// Ticket W30-05: the diorama / Mode 7 ground camera (presentation
    /// only). Pitch in degrees (20..=80), yaw in degrees (-30..=30), zoom
    /// as an integer percent (100..=400, so this struct keeps its `Eq`),
    /// and whether the view follows the game's own camera. Each is
    /// persisted only when it differs from the default.
    pub diorama_pitch: u8,
    pub diorama_yaw: i8,
    pub diorama_zoom: u16,
    pub diorama_follow: bool,
    /// Ticket W30-03: the screen shape decoded widescreen aims for. 16:9
    /// by default; only a non-default choice is persisted.
    pub widescreen_aspect: rf_enhance::widescreen::Aspect,
    /// Shader name, `None` for the default pipeline. A string rather than
    /// an enum because W3-02a owns the shader set and this must not have to
    /// change when that lands.
    pub shader: Option<String>,
    /// Ticket W3-05c (FR-ENH-011, D-004): this game's heuristic trust
    /// ladder, persisted here rather than in a second store of its own.
    ///
    /// The ticket's own note is why: "building a second, parallel
    /// persistence path inside rf-enhance would be exactly the kind of
    /// duplicate abstraction this board keeps deferring until a second
    /// consumer genuinely exists". This file is already the per-game
    /// store, already keyed by normalized ROM hash, and already preserves
    /// keys it does not understand.
    pub trust: rf_enhance::trust::TrustLadder,
    /// Ticket W15-02 (UX_WAVE_15 §3, §11): wall-clock seconds since the
    /// Unix epoch of this game's most recent launch, `None` for a game
    /// never opened. `SystemTime` rather than a monotonic clock because
    /// this is meant to survive a restart and be compared across
    /// processes — the library toolbar's "Recently played" chip and
    /// "Last played" sort both read it back cold from disk.
    pub last_played_epoch_secs: Option<u64>,
    /// How many times this game has been launched. Never written back
    /// down on its own — only [`GameSettings::record_launch`] advances
    /// it, alongside `last_played_epoch_secs`, so the two can never drift
    /// apart from each other.
    pub play_count: u32,
    /// The star toggle next to Play (UX_WAVE_15 §3): persisted here like
    /// every other per-game setting rather than in a second favourites
    /// list, so a renamed or re-dumped cartridge keeps its favourite
    /// status the same way it keeps its mode.
    pub favourite: bool,
    /// Ticket W27-05: the profile items pinned to the screen (their
    /// `label`s), in the order pinned, and the corner they show in.
    pub pinned_items: Vec<String>,
    pub info_corner: InfoCorner,
    /// Keys this build does not know, kept verbatim (module doc).
    unknown: BTreeMap<String, String>,
}

/// Ticket W30-01: the lowest/highest/step of the stored overclock percent.
const SUPERFX_PCT_MIN: u16 = 200;
const SUPERFX_PCT_MAX: u16 = 800;
/// Ticket W30-07: the most frames run-ahead may run ahead.
const RUN_AHEAD_MAX: u8 = crate::run_ahead::MAX_RUN_AHEAD;
const SUPERFX_PCT_DEFAULT: u16 = 200;
/// Ticket W30-08: the stored CPU / SA-1 overclock ceiling's range (whole
/// hundreds) and the most extra NES vblank lines.
pub const CLOCK_PCT_MIN: u16 = 200;
pub const CLOCK_PCT_MAX: u16 = 400;
pub const CLOCK_PCT_DEFAULT: u16 = 200;
pub const NES_EXTRA_LINES_MAX: u16 = 240;
/// What switching "Less slowdown" on starts at, before the slider moves it.
pub const NES_EXTRA_LINES_ON: u16 = 60;

/// Ticket W30-02: bounds and default of the stored HD Mode 7 scale.
const HD_MODE7_SCALE_MIN: u8 = 2;
const HD_MODE7_SCALE_MAX: u8 = 4;
const HD_MODE7_SCALE_DEFAULT: u8 = 2;

/// Ticket W30-04: every lens strength defaults to half; the time of day to
/// noon.
const LENS_PCT_DEFAULT: u8 = 50;
const LENS_PCT_MAX: u8 = 100;
const LENS_TIME_DEFAULT: u8 = 50;

/// Ticket W30-05: the diorama camera's defaults and ranges. The pitch
/// default is `rf_renderer::diorama::PITCH_DEG` (52), so a fresh game looks
/// exactly as it did before the camera was adjustable.
pub const DIORAMA_PITCH_DEFAULT: u8 = 52;
pub const DIORAMA_PITCH_MIN: u8 = 20;
pub const DIORAMA_PITCH_MAX: u8 = 80;
pub const DIORAMA_YAW_LIMIT: i8 = 30;
pub const DIORAMA_ZOOM_DEFAULT: u16 = 100;
pub const DIORAMA_ZOOM_MAX: u16 = 400;

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            mode: Mode::default(),
            sprite_overlay: false,
            deflicker: false,
            widescreen_decoded: false,
            full_level_view: false,
            diorama: false,
            mode7_ground: false,
            loading_fast_forward: false,
            superfx_overclock: false,
            superfx_overclock_pct: SUPERFX_PCT_DEFAULT,
            superfx_overclock_mode: OverclockMode::Governed,
            cpu_overclock: false,
            cpu_overclock_pct: CLOCK_PCT_DEFAULT,
            cpu_overclock_mode: OverclockMode::Governed,
            sa1_overclock: false,
            sa1_overclock_pct: CLOCK_PCT_DEFAULT,
            sa1_overclock_mode: OverclockMode::Governed,
            snes_interpolation: false,
            snes_interpolation_mode: rf_snes::apu::dsp::Interpolation::Cubic,
            nes_extra_lines: 0,
            run_ahead: 0,
            hd_mode7: false,
            hd_mode7_scale: HD_MODE7_SCALE_DEFAULT,
            hd_mode7_supersample: false,
            hd_mode7_perspective: true,
            lens_bloom: false,
            lens_bloom_pct: LENS_PCT_DEFAULT,
            lens_tiltshift: false,
            lens_tiltshift_pct: LENS_PCT_DEFAULT,
            lens_shadows: false,
            lens_shadows_pct: LENS_PCT_DEFAULT,
            lens_grade: false,
            lens_grade_pct: LENS_PCT_DEFAULT,
            lens_weather: rf_renderer::lens::Weather::None,
            lens_weather_pct: LENS_PCT_DEFAULT,
            lens_time_of_day: LENS_TIME_DEFAULT,
            lens_follow_clock: false,
            diorama_pitch: DIORAMA_PITCH_DEFAULT,
            diorama_yaw: 0,
            diorama_zoom: DIORAMA_ZOOM_DEFAULT,
            diorama_follow: false,
            widescreen_aspect: rf_enhance::widescreen::Aspect::default(),
            shader: None,
            trust: rf_enhance::trust::TrustLadder::default(),
            last_played_epoch_secs: None,
            play_count: 0,
            favourite: false,
            pinned_items: Vec::new(),
            info_corner: InfoCorner::default(),
            unknown: BTreeMap::new(),
        }
    }
}

impl GameSettings {
    /// Record one launch: bump the play count and stamp the launch time.
    /// A pure mutator — no I/O — so the caller decides when (and whether)
    /// to persist it, and this is testable without a filesystem.
    ///
    /// `when` is a parameter rather than `SystemTime::now()` called inside,
    /// so a test can assert an exact value instead of merely "some value
    /// close to now".
    pub fn record_launch(&mut self, when: std::time::SystemTime) {
        self.play_count = self.play_count.saturating_add(1);
        self.last_played_epoch_secs = when
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
    }
}

impl GameSettings {
    /// Ticket W30-04: whether any lens effect is switched on. The
    /// "HD-2D lens and lighting" row reads as on exactly when this is.
    #[must_use]
    pub fn lens_any(&self) -> bool {
        self.lens_bloom
            || self.lens_tiltshift
            || self.lens_shadows
            || self.lens_grade
            || self.lens_weather != rf_renderer::lens::Weather::None
    }

    /// Ticket W30-04: switch every lens effect off (the row's own toggle).
    pub fn lens_clear(&mut self) {
        self.lens_bloom = false;
        self.lens_tiltshift = false;
        self.lens_shadows = false;
        self.lens_grade = false;
        self.lens_weather = rf_renderer::lens::Weather::None;
    }

    /// Ticket W30-04: the row's toggle turned on with nothing chosen yet
    /// starts with the miniature look (glow and tilt-shift); a player who
    /// already picked effects keeps their picks.
    pub fn lens_start(&mut self) {
        if !self.lens_any() {
            self.lens_bloom = true;
            self.lens_tiltshift = true;
        }
    }

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
        // Only written when ON, so a game nobody has configured does not
        // grow three keys that restate the default (same reasoning as the
        // trust ladder's key, W3-05c).
        for (key, on) in [
            ("deflicker", self.deflicker),
            ("widescreen_decoded", self.widescreen_decoded),
            ("full_level_view", self.full_level_view),
            ("diorama", self.diorama),
            ("mode7_ground", self.mode7_ground),
            ("loading_fast_forward", self.loading_fast_forward),
            ("superfx_overclock", self.superfx_overclock),
            ("cpu_overclock", self.cpu_overclock),
            ("sa1_overclock", self.sa1_overclock),
            ("snes_interpolation", self.snes_interpolation),
            ("hd_mode7", self.hd_mode7),
            ("hd_mode7_supersample", self.hd_mode7_supersample),
        ] {
            if on {
                fields.insert(key.to_string(), "true".to_string());
            }
        }
        // Ticket W30-01: the percentage is only written when it differs
        // from the default, same "restate nothing" rule.
        if self.superfx_overclock_pct != SUPERFX_PCT_DEFAULT {
            fields.insert(
                "superfx_overclock_pct".to_string(),
                self.superfx_overclock_pct.to_string(),
            );
        }
        // Ticket W30-08: only what differs from the default (200%,
        // governed, no extra lines).
        for (key, pct, mode) in [
            // Ticket W30-01c: the Super FX's pct is written above.
            ("cpu", self.cpu_overclock_pct, self.cpu_overclock_mode),
            ("sa1", self.sa1_overclock_pct, self.sa1_overclock_mode),
        ] {
            if pct != CLOCK_PCT_DEFAULT {
                fields.insert(format!("{key}_overclock_pct"), pct.to_string());
            }
            if mode != OverclockMode::Governed {
                fields.insert(format!("{key}_overclock_mode"), mode.key().to_string());
            }
        }
        if self.superfx_overclock_mode != OverclockMode::Governed {
            fields.insert(
                "superfx_overclock_mode".to_string(),
                self.superfx_overclock_mode.key().to_string(),
            );
        }
        // Ticket W30-09: the filter is written only when it is not cubic.
        if self.snes_interpolation_mode != rf_snes::apu::dsp::Interpolation::Cubic {
            fields.insert(
                "snes_interpolation_mode".to_string(),
                self.snes_interpolation_mode.name().to_string(),
            );
        }
        if self.nes_extra_lines > 0 {
            fields.insert(
                "nes_extra_lines".to_string(),
                self.nes_extra_lines.to_string(),
            );
        }
        // Ticket W30-07: written only when on, like the other opt-ins.
        if self.run_ahead > 0 {
            fields.insert("run_ahead".to_string(), self.run_ahead.to_string());
        }
        // Ticket W30-02: the scale and perspective flag are written only
        // when they differ from their defaults (2, on).
        if self.hd_mode7_scale != HD_MODE7_SCALE_DEFAULT {
            fields.insert(
                "hd_mode7_scale".to_string(),
                self.hd_mode7_scale.to_string(),
            );
        }
        if !self.hd_mode7_perspective {
            fields.insert("hd_mode7_perspective".to_string(), "false".to_string());
        }
        // Ticket W30-04: same rule -- only what differs from the default.
        for (key, on) in [
            ("lens_bloom", self.lens_bloom),
            ("lens_tiltshift", self.lens_tiltshift),
            ("lens_shadows", self.lens_shadows),
            ("lens_grade", self.lens_grade),
            ("lens_follow_clock", self.lens_follow_clock),
        ] {
            if on {
                fields.insert(key.to_string(), "true".to_string());
            }
        }
        for (key, pct, default) in [
            ("lens_bloom_pct", self.lens_bloom_pct, LENS_PCT_DEFAULT),
            (
                "lens_tiltshift_pct",
                self.lens_tiltshift_pct,
                LENS_PCT_DEFAULT,
            ),
            ("lens_shadows_pct", self.lens_shadows_pct, LENS_PCT_DEFAULT),
            ("lens_grade_pct", self.lens_grade_pct, LENS_PCT_DEFAULT),
            ("lens_weather_pct", self.lens_weather_pct, LENS_PCT_DEFAULT),
            ("lens_time_of_day", self.lens_time_of_day, LENS_TIME_DEFAULT),
        ] {
            if pct != default {
                fields.insert(key.to_string(), pct.to_string());
            }
        }
        if self.lens_weather != rf_renderer::lens::Weather::None {
            fields.insert(
                "lens_weather".to_string(),
                self.lens_weather.key().to_string(),
            );
        }
        // Ticket W30-05: the diorama camera, only what differs.
        if self.diorama_pitch != DIORAMA_PITCH_DEFAULT {
            fields.insert("diorama_pitch".to_string(), self.diorama_pitch.to_string());
        }
        if self.diorama_yaw != 0 {
            fields.insert("diorama_yaw".to_string(), self.diorama_yaw.to_string());
        }
        if self.diorama_zoom != DIORAMA_ZOOM_DEFAULT {
            fields.insert("diorama_zoom".to_string(), self.diorama_zoom.to_string());
        }
        if self.diorama_follow {
            fields.insert("diorama_follow".to_string(), "true".to_string());
        }
        // Ticket W30-03: 16:9 is the default and is never written.
        if self.widescreen_aspect != rf_enhance::widescreen::Aspect::default() {
            fields.insert(
                "widescreen_aspect".to_string(),
                self.widescreen_aspect.key().to_string(),
            );
        }
        if let Some(shader) = &self.shader {
            fields.insert("shader".to_string(), shader.clone());
        }
        // Ticket W15-02: same "only write it when it says something" rule
        // as the toggles above — a game nobody has launched yet must not
        // grow `last_played`/`play_count` keys that only restate zero.
        if let Some(epoch) = self.last_played_epoch_secs {
            fields.insert("last_played".to_string(), epoch.to_string());
        }
        if self.play_count > 0 {
            fields.insert("play_count".to_string(), self.play_count.to_string());
        }
        if !self.pinned_items.is_empty() {
            fields.insert("pinned_items".to_string(), self.pinned_items.join(","));
        }
        if self.info_corner != InfoCorner::default() {
            fields.insert(
                "info_corner".to_string(),
                self.info_corner.name().to_string(),
            );
        }
        if self.favourite {
            fields.insert("favourite".to_string(), "true".to_string());
        }
        // Ticket W3-05c: omitted entirely when everything is at its
        // default, so an untouched game's file does not grow a key that
        // only says "all-shadow" — which is what `TrustLadder::default`
        // already means.
        let trust = self.trust.to_settings_value();
        if !trust.is_empty() {
            fields.insert("trust".to_string(), trust);
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
                "deflicker" => settings.deflicker = value == "true",
                "widescreen_decoded" => settings.widescreen_decoded = value == "true",
                "full_level_view" => settings.full_level_view = value == "true",
                "diorama" => settings.diorama = value == "true",
                "mode7_ground" => settings.mode7_ground = value == "true",
                "loading_fast_forward" => settings.loading_fast_forward = value == "true",
                "superfx_overclock" => settings.superfx_overclock = value == "true",
                "cpu_overclock" => settings.cpu_overclock = value == "true",
                "sa1_overclock" => settings.sa1_overclock = value == "true",
                "snes_interpolation" => settings.snes_interpolation = value == "true",
                // Ticket W30-09: an unknown name, or "gaussian" (which is
                // what OFF means), keeps the default smoother filter.
                "snes_interpolation_mode" => {
                    if let Some(mode) = rf_snes::apu::dsp::Interpolation::from_name(value) {
                        if mode != rf_snes::apu::dsp::Interpolation::Gaussian {
                            settings.snes_interpolation_mode = mode;
                        }
                    }
                }
                // Ticket W30-08: whole hundreds, clamped; an unknown mode
                // keeps the safe default (governed).
                "cpu_overclock_pct" | "sa1_overclock_pct" => {
                    if let Ok(pct) = value.parse::<u16>() {
                        let pct = (pct / 100 * 100).clamp(CLOCK_PCT_MIN, CLOCK_PCT_MAX);
                        if key == "cpu_overclock_pct" {
                            settings.cpu_overclock_pct = pct;
                        } else {
                            settings.sa1_overclock_pct = pct;
                        }
                    }
                }
                "cpu_overclock_mode" | "sa1_overclock_mode" | "superfx_overclock_mode" => {
                    if let Some(mode) = OverclockMode::from_key(value) {
                        match key {
                            "cpu_overclock_mode" => settings.cpu_overclock_mode = mode,
                            "sa1_overclock_mode" => settings.sa1_overclock_mode = mode,
                            _ => settings.superfx_overclock_mode = mode,
                        }
                    }
                }
                "nes_extra_lines" => {
                    if let Ok(lines) = value.parse::<u16>() {
                        settings.nes_extra_lines = lines.min(NES_EXTRA_LINES_MAX);
                    }
                }
                "run_ahead" => {
                    if let Ok(frames) = value.parse::<u8>() {
                        settings.run_ahead = frames.min(RUN_AHEAD_MAX);
                    }
                }
                "superfx_overclock_pct" => {
                    if let Ok(pct) = value.parse::<u16>() {
                        // Round down to the 100 step, then clamp (W30-01).
                        settings.superfx_overclock_pct =
                            (pct / 100 * 100).clamp(SUPERFX_PCT_MIN, SUPERFX_PCT_MAX);
                    }
                }
                // An unknown aspect keeps the default rather than failing.
                "widescreen_aspect" => {
                    if let Some(a) = rf_enhance::widescreen::Aspect::from_key(value) {
                        settings.widescreen_aspect = a;
                    }
                }
                "hd_mode7" => settings.hd_mode7 = value == "true",
                "hd_mode7_supersample" => settings.hd_mode7_supersample = value == "true",
                "hd_mode7_perspective" => settings.hd_mode7_perspective = value != "false",
                "hd_mode7_scale" => {
                    if let Ok(scale) = value.parse::<u8>() {
                        settings.hd_mode7_scale =
                            scale.clamp(HD_MODE7_SCALE_MIN, HD_MODE7_SCALE_MAX);
                    }
                }
                "lens_bloom" => settings.lens_bloom = value == "true",
                "lens_tiltshift" => settings.lens_tiltshift = value == "true",
                "lens_shadows" => settings.lens_shadows = value == "true",
                "lens_grade" => settings.lens_grade = value == "true",
                "lens_follow_clock" => settings.lens_follow_clock = value == "true",
                // An unknown sky keeps the default (clear).
                "lens_weather" => {
                    if let Some(w) = rf_renderer::lens::Weather::from_key(value) {
                        settings.lens_weather = w;
                    }
                }
                // Ticket W30-05: clamp into range; junk keeps the default.
                "diorama_pitch" => {
                    if let Ok(v) = value.parse::<i64>() {
                        settings.diorama_pitch = v
                            .clamp(i64::from(DIORAMA_PITCH_MIN), i64::from(DIORAMA_PITCH_MAX))
                            as u8;
                    }
                }
                "diorama_yaw" => {
                    if let Ok(v) = value.parse::<i64>() {
                        settings.diorama_yaw = v
                            .clamp(-i64::from(DIORAMA_YAW_LIMIT), i64::from(DIORAMA_YAW_LIMIT))
                            as i8;
                    }
                }
                "diorama_zoom" => {
                    if let Ok(v) = value.parse::<i64>() {
                        settings.diorama_zoom = v
                            .clamp(i64::from(DIORAMA_ZOOM_DEFAULT), i64::from(DIORAMA_ZOOM_MAX))
                            as u16;
                    }
                }
                "diorama_follow" => settings.diorama_follow = value == "true",
                "lens_bloom_pct" => parse_pct(value, &mut settings.lens_bloom_pct),
                "lens_tiltshift_pct" => parse_pct(value, &mut settings.lens_tiltshift_pct),
                "lens_shadows_pct" => parse_pct(value, &mut settings.lens_shadows_pct),
                "lens_grade_pct" => parse_pct(value, &mut settings.lens_grade_pct),
                "lens_weather_pct" => parse_pct(value, &mut settings.lens_weather_pct),
                "lens_time_of_day" => parse_pct(value, &mut settings.lens_time_of_day),
                "trust" => {
                    settings.trust = rf_enhance::trust::TrustLadder::from_settings_value(value);
                }
                "last_played" => {
                    // An unparseable value falls back to `None` rather than
                    // refusing the whole file, same rule as `mode` above.
                    settings.last_played_epoch_secs = value.parse().ok();
                }
                "play_count" => {
                    settings.play_count = value.parse().unwrap_or(0);
                }
                "favourite" => settings.favourite = value == "true",
                "pinned_items" => {
                    settings.pinned_items = value
                        .split(',')
                        .filter(|l| !l.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                "info_corner" => {
                    settings.info_corner = InfoCorner::from_name(value).unwrap_or_default()
                }
                other => {
                    settings
                        .unknown
                        .insert(other.to_string(), value.to_string());
                }
            }
        }
        Some(settings)
    }

    /// The camera these settings describe (ticket W30-05), without a follow
    /// point: the live camera is known only at render time, so the shell
    /// fills `follow` in when [`Self::diorama_follow`] is set.
    #[must_use]
    pub fn diorama_camera(&self) -> rf_renderer::diorama::DioramaCamera {
        rf_renderer::diorama::DioramaCamera {
            pitch_deg: f32::from(self.diorama_pitch),
            yaw_deg: f32::from(self.diorama_yaw),
            zoom: f32::from(self.diorama_zoom) / 100.0,
            follow: None,
        }
        .clamped()
    }

    /// Keys this build did not recognize — exposed so a test (and a curious
    /// developer) can see that they survived a round trip.
    #[must_use]
    pub fn unknown_keys(&self) -> &BTreeMap<String, String> {
        &self.unknown
    }
}

/// Ticket W30-04: read a `0..=100` percent, clamping a larger number and
/// leaving `target` alone for text that is not a number.
fn parse_pct(value: &str, target: &mut u8) {
    if let Ok(n) = value.parse::<u32>() {
        *target = u8::try_from(n.min(u32::from(LENS_PCT_MAX))).unwrap_or(LENS_PCT_MAX);
    }
}

/// Where one game's settings live under `root`.
#[must_use]
/// Ticket W27-05: where pinned Game info shows over the game.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InfoCorner {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl InfoCorner {
    pub const ALL: [InfoCorner; 4] = [
        InfoCorner::TopLeft,
        InfoCorner::TopRight,
        InfoCorner::BottomLeft,
        InfoCorner::BottomRight,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            InfoCorner::TopLeft => "top_left",
            InfoCorner::TopRight => "top_right",
            InfoCorner::BottomLeft => "bottom_left",
            InfoCorner::BottomRight => "bottom_right",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            InfoCorner::TopLeft => "Top left",
            InfoCorner::TopRight => "Top right",
            InfoCorner::BottomLeft => "Bottom left",
            InfoCorner::BottomRight => "Bottom right",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }
}

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

#[cfg(test)]
mod trust_persistence_tests {
    use super::*;
    use rf_enhance::trust::TrustState;

    /// Ticket W3-05c, acceptance criterion 1: per-game trust state is
    /// PERSISTED — through this store, keyed by normalized ROM hash like
    /// every other per-game setting, rather than through a second path of
    /// rf-enhance's own.
    #[test]
    fn the_trust_ladder_round_trips_through_the_per_game_file() {
        let mut settings = GameSettings::default();
        settings.trust.set_state("anti-flicker", TrustState::Active);
        settings
            .trust
            .suppress("anti-flicker", "deliberate strobe here", "scene-a")
            .expect("a real justification is accepted");

        let restored =
            GameSettings::from_text(&settings.to_text()).expect("self-written file parses");
        assert_eq!(restored.trust.state("anti-flicker"), TrustState::Active);
        assert!(restored.trust.is_suppressed("anti-flicker", "scene-a"));
        assert!(
            !restored.trust.is_suppressed("anti-flicker", "scene-b"),
            "the suppression's scene scope must survive persistence, or auto-reopen \
             silently stops working across a restart"
        );
    }

    /// D-004's "fresh install all-shadow" must survive a save/load with
    /// no key written at all — otherwise the default would be a thing the
    /// file asserts rather than a thing the code guarantees.
    #[test]
    fn an_untouched_game_writes_no_trust_key_and_still_loads_all_shadow() {
        let settings = GameSettings::default();
        let text = settings.to_text();
        assert!(
            !text.contains("trust"),
            "an untouched game must not grow a key that only restates the default:\n{text}"
        );
        let restored = GameSettings::from_text(&text).expect("self-written file parses");
        assert_eq!(restored.trust.state("anti-flicker"), TrustState::Shadow);
        assert!(!restored.trust.should_act("anti-flicker", "scene-a"));
    }

    /// The unknown-key preservation this file already guarantees must
    /// keep working alongside the new key — a build that did not know
    /// `trust` must not eat it, and this build must not eat theirs.
    #[test]
    fn trust_coexists_with_unknown_keys_from_another_build() {
        let text =
            format!("{MAGIC}\nmode=accuracy\ntrust=anti-flicker=active\nfuture_key=whatever\n");
        let settings = GameSettings::from_text(&text).expect("file parses");
        assert_eq!(settings.trust.state("anti-flicker"), TrustState::Active);
        let round_tripped = settings.to_text();
        assert!(
            round_tripped.contains("future_key=whatever"),
            "an unknown key must survive: {round_tripped}"
        );
        assert!(round_tripped.contains("trust=anti-flicker=active"));
    }
}

#[cfg(test)]
mod mode_and_feature_persistence_tests {
    use super::*;

    /// Ticket W15-02, acceptance criterion 4: play-count/last-played/favourite
    /// persistence round-trips through the per-game file.
    #[cfg(test)]
    mod recency_and_favourite_persistence_tests {
        use super::*;

        fn temp_root(label: &str) -> PathBuf {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "rf-w15-02-set-{label}-{}-{unique}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            dir
        }

        /// An untouched game must write none of the three new keys — the same
        /// "default is a property of the code, not a file assertion" rule as
        /// the trust ladder and the enhancement toggles.
        #[test]
        fn an_untouched_game_writes_none_of_the_new_keys() {
            let text = GameSettings::default().to_text();
            for key in ["last_played", "play_count", "favourite"] {
                assert!(!text.contains(key), "untouched game wrote `{key}`:\n{text}");
            }
        }

        #[test]
        fn record_launch_bumps_the_count_and_stamps_the_time_and_both_persist() {
            let mut settings = GameSettings::default();
            assert_eq!(settings.play_count, 0);
            assert_eq!(settings.last_played_epoch_secs, None);

            let first = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
            settings.record_launch(first);
            assert_eq!(settings.play_count, 1);
            assert_eq!(settings.last_played_epoch_secs, Some(1_000));

            let second = std::time::UNIX_EPOCH + std::time::Duration::from_secs(2_000);
            settings.record_launch(second);
            assert_eq!(
                settings.play_count, 2,
                "a second launch must not reset the count"
            );
            assert_eq!(
                settings.last_played_epoch_secs,
                Some(2_000),
                "the timestamp must advance to the newer launch"
            );

            let restored =
                GameSettings::from_text(&settings.to_text()).expect("self-written file parses");
            assert_eq!(restored.play_count, 2);
            assert_eq!(restored.last_played_epoch_secs, Some(2_000));
        }

        #[test]
        fn favourite_persists_independently_of_play_history() {
            let settings = GameSettings {
                favourite: true,
                ..GameSettings::default()
            };
            let restored =
                GameSettings::from_text(&settings.to_text()).expect("self-written file parses");
            assert!(restored.favourite);
            assert_eq!(
                restored.play_count, 0,
                "favouriting alone must not fake a play"
            );
            assert_eq!(restored.last_played_epoch_secs, None);
        }

        /// Full load/save round trip through the actual store, keyed by hash,
        /// like `settings_persist_keyed_by_hash_and_do_not_bleed_between_games`
        /// above — this is the same guarantee for the three new fields.
        #[test]
        fn play_history_round_trips_through_the_store_keyed_by_hash() {
            let root = temp_root("recency");
            const HASH: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

            let mut settings = load(&root, HASH);
            settings.record_launch(std::time::UNIX_EPOCH + std::time::Duration::from_secs(42));
            settings.favourite = true;
            save(&root, HASH, &settings).expect("save");

            let reloaded = load(&root, HASH);
            assert_eq!(reloaded.play_count, 1);
            assert_eq!(reloaded.last_played_epoch_secs, Some(42));
            assert!(reloaded.favourite);

            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// FR-MODE-001: all five ARCHITECTURE §4 modes round-trip by name.
    #[test]
    fn every_mode_round_trips_through_the_per_game_file() {
        for mode in Mode::all() {
            let s = GameSettings {
                mode,
                ..GameSettings::default()
            };
            let restored = GameSettings::from_text(&s.to_text()).expect("self-written file parses");
            assert_eq!(restored.mode, mode, "{} did not survive", mode.name());
        }
        assert_eq!(Mode::all().len(), 5, "ARCHITECTURE §4 defines five modes");
    }

    /// FR-MODE-003, at the persistence layer: an unrecognised mode name —
    /// an older file, a typo, a newer build's mode — resolves to
    /// Accuracy. Enhancement is never on because a name failed to parse.
    #[test]
    fn an_unknown_mode_name_falls_back_to_accuracy_not_to_enhanced() {
        let text = format!("{MAGIC}\nmode=super-turbo\n");
        let restored = GameSettings::from_text(&text).expect("file parses");
        assert_eq!(restored.mode, Mode::Accuracy);
        assert!(!restored.mode.enhancement_active());
    }

    /// FR-ENH-010: the per-game enhancement toggles persist, and an
    /// untouched game writes none of them — the default stays a property
    /// of the code rather than something a file has to assert.
    #[test]
    fn enhancement_toggles_persist_and_an_untouched_game_writes_none() {
        let untouched = GameSettings::default().to_text();
        for key in [
            "deflicker",
            "widescreen_decoded",
            "full_level_view",
            "diorama",
            "mode7_ground",
            "superfx_overclock",
            "superfx_overclock_pct",
            "superfx_overclock_mode",
            "cpu_overclock",
            "sa1_overclock",
            "snes_interpolation",
            "nes_extra_lines",
            "run_ahead",
            "hd_mode7",
            "lens_",
        ] {
            assert!(
                !untouched.contains(key),
                "an untouched game must not write `{key}`:\n{untouched}"
            );
        }

        let s = GameSettings {
            deflicker: true,
            full_level_view: true,
            diorama: true,
            mode7_ground: true,
            superfx_overclock: true,
            superfx_overclock_pct: 400,
            superfx_overclock_mode: OverclockMode::Fixed,
            cpu_overclock: true,
            cpu_overclock_pct: 300,
            cpu_overclock_mode: OverclockMode::Fixed,
            sa1_overclock: true,
            sa1_overclock_pct: 400,
            snes_interpolation: true,
            snes_interpolation_mode: rf_snes::apu::dsp::Interpolation::Linear,
            nes_extra_lines: 120,
            run_ahead: 2,
            hd_mode7: true,
            hd_mode7_scale: 4,
            hd_mode7_supersample: true,
            hd_mode7_perspective: false,
            widescreen_aspect: rf_enhance::widescreen::Aspect::Wide32x9,
            ..GameSettings::default()
        };
        let restored = GameSettings::from_text(&s.to_text()).expect("parses");
        // Ticket W30-03: the aspect round-trips; the default is never written.
        assert_eq!(
            restored.widescreen_aspect,
            rf_enhance::widescreen::Aspect::Wide32x9
        );
        assert!(!GameSettings::default()
            .to_text()
            .contains("widescreen_aspect"));
        let junk =
            GameSettings::from_text(&format!("{MAGIC}\nwidescreen_aspect=5:4\n")).expect("parses");
        assert_eq!(
            junk.widescreen_aspect,
            rf_enhance::widescreen::Aspect::Wide16x9
        );
        // Ticket W30-02: HD Mode 7 round-trips; only non-defaults are written.
        assert!(restored.hd_mode7 && restored.hd_mode7_supersample);
        assert_eq!(restored.hd_mode7_scale, 4);
        assert!(!restored.hd_mode7_perspective);
        let on_only = GameSettings {
            hd_mode7: true,
            ..GameSettings::default()
        }
        .to_text();
        assert!(on_only.contains("hd_mode7=true"));
        for key in [
            "hd_mode7_scale",
            "hd_mode7_supersample",
            "hd_mode7_perspective",
        ] {
            assert!(
                !on_only.contains(key),
                "default `{key}` must not be written"
            );
        }
        let back = GameSettings::from_text(&on_only).expect("parses");
        assert_eq!(back.hd_mode7_scale, 2);
        assert!(back.hd_mode7_perspective, "perspective defaults on");
        for (raw, want) in [("9", 4), ("1", 2), ("0", 2), ("3", 3)] {
            let t = GameSettings::from_text(&format!("{MAGIC}\nhd_mode7_scale={raw}\n"))
                .expect("parses");
            assert_eq!(t.hd_mode7_scale, want, "scale {raw}");
        }
        assert!(restored.superfx_overclock);
        assert_eq!(restored.superfx_overclock_pct, 400);
        // Ticket W30-01c: the Super FX mode round-trips; governed writes no key.
        assert_eq!(restored.superfx_overclock_mode, OverclockMode::Fixed);
        assert!(!GameSettings {
            superfx_overclock_mode: OverclockMode::Governed,
            ..s.clone()
        }
        .to_text()
        .contains("superfx_overclock_mode"));
        // Ticket W30-08: the trio and the line count round-trip; the
        // governed default writes no mode key; junk clamps or is ignored.
        assert!(restored.cpu_overclock && restored.sa1_overclock);
        assert_eq!(restored.cpu_overclock_pct, 300);
        assert_eq!(restored.cpu_overclock_mode, OverclockMode::Fixed);
        assert_eq!(restored.sa1_overclock_pct, 400);
        assert_eq!(restored.sa1_overclock_mode, OverclockMode::Governed);
        assert!(!s.to_text().contains("sa1_overclock_mode"));
        // Ticket W30-09: the toggle and the filter round-trip; the cubic
        // default writes no mode key; "gaussian"/junk keep the default.
        assert!(restored.snes_interpolation);
        assert_eq!(
            restored.snes_interpolation_mode,
            rf_snes::apu::dsp::Interpolation::Linear
        );
        let on = GameSettings {
            snes_interpolation: true,
            ..GameSettings::default()
        }
        .to_text();
        assert!(on.contains("snes_interpolation=true") && !on.contains("_mode"));
        for raw in ["gaussian", "bogus"] {
            let t = GameSettings::from_text(&format!("{MAGIC}\nsnes_interpolation_mode={raw}\n"))
                .expect("parses");
            assert_eq!(
                t.snes_interpolation_mode,
                rf_snes::apu::dsp::Interpolation::Cubic,
                "{raw}"
            );
        }
        assert_eq!(restored.nes_extra_lines, 120);
        for (raw, want) in [("9999", 400), ("50", 200), ("350", 300), ("x", 200)] {
            let t = GameSettings::from_text(&format!("{MAGIC}\ncpu_overclock_pct={raw}\n"))
                .expect("parses");
            assert_eq!(t.cpu_overclock_pct, want, "cpu pct {raw}");
            let t = GameSettings::from_text(&format!("{MAGIC}\nsa1_overclock_pct={raw}\n"))
                .expect("parses");
            assert_eq!(t.sa1_overclock_pct, want, "sa1 pct {raw}");
        }
        for (raw, want) in [("500", 240), ("240", 240), ("1", 1), ("x", 0)] {
            let t = GameSettings::from_text(&format!("{MAGIC}\nnes_extra_lines={raw}\n"))
                .expect("parses");
            assert_eq!(t.nes_extra_lines, want, "lines {raw}");
        }
        let t = GameSettings::from_text(&format!("{MAGIC}\ncpu_overclock_mode=turbo\n"))
            .expect("parses");
        assert_eq!(t.cpu_overclock_mode, OverclockMode::Governed);
        // Ticket W30-07: run-ahead round-trips, and junk is clamped to 2.
        assert_eq!(restored.run_ahead, 2);
        for (raw, want) in [("1", 1), ("9", 2), ("0", 0), ("x", 0)] {
            let t =
                GameSettings::from_text(&format!("{MAGIC}\nrun_ahead={raw}\n")).expect("parses");
            assert_eq!(t.run_ahead, want, "run_ahead {raw}");
        }
        let wild = GameSettings::from_text(&format!("{MAGIC}\nsuperfx_overclock_pct=9999\n"))
            .expect("parses");
        assert_eq!(wild.superfx_overclock_pct, 800);
        let low = GameSettings::from_text(&format!("{MAGIC}\nsuperfx_overclock_pct=50\n"))
            .expect("parses");
        assert_eq!(low.superfx_overclock_pct, 200);
        assert!(restored.deflicker);
        assert!(restored.full_level_view);
        assert!(restored.diorama);
        assert!(restored.mode7_ground);
        assert!(
            !restored.widescreen_decoded,
            "a toggle that was never set must stay off"
        );
    }

    /// Ticket W30-04: every lens setting round-trips, only non-defaults are
    /// written, strengths clamp to 0..=100, and junk falls back.
    #[test]
    fn lens_settings_persist_clamp_and_write_only_non_defaults() {
        use rf_renderer::lens::Weather;
        let untouched = GameSettings::default().to_text();
        assert!(!untouched.contains("lens_"), "{untouched}");
        let all = GameSettings {
            lens_bloom: true,
            lens_bloom_pct: 80,
            lens_tiltshift: true,
            lens_tiltshift_pct: 0,
            lens_shadows: true,
            lens_shadows_pct: 100,
            lens_grade: true,
            lens_grade_pct: 25,
            lens_weather: Weather::Snow,
            lens_weather_pct: 70,
            lens_time_of_day: 0,
            lens_follow_clock: true,
            ..GameSettings::default()
        };
        assert_eq!(
            GameSettings::from_text(&all.to_text()).expect("parses"),
            all,
            "every lens field survives a save"
        );
        // On with the default strength: just the flag is written.
        let on = GameSettings {
            lens_bloom: true,
            ..GameSettings::default()
        }
        .to_text();
        assert!(on.contains("lens_bloom=true"));
        assert!(!on.contains("lens_bloom_pct"), "default strength: {on}");
        assert!(!on.contains("lens_weather"), "clear sky: {on}");
        // Clamp wild values; ignore junk.
        let wild = GameSettings::from_text(&format!(
            "{MAGIC}\nlens_bloom_pct=250\nlens_grade_pct=abc\nlens_time_of_day=999\nlens_weather=hail\n"
        ))
        .expect("parses");
        assert_eq!(wild.lens_bloom_pct, 100);
        assert_eq!(wild.lens_grade_pct, 50, "junk keeps the default");
        assert_eq!(wild.lens_time_of_day, 100);
        assert_eq!(wild.lens_weather, Weather::None);
        // The row switch helpers.
        let mut s = GameSettings::default();
        assert!(!s.lens_any());
        s.lens_start();
        assert!(s.lens_bloom && s.lens_tiltshift && !s.lens_grade && s.lens_any());
        s.lens_grade = true;
        s.lens_bloom = false;
        s.lens_tiltshift = false;
        s.lens_start();
        assert!(!s.lens_bloom, "picks already made are kept");
        s.lens_clear();
        assert!(!s.lens_any());
    }

    /// Ticket W30-05: the diorama camera persists only when non-default,
    /// clamps wild values, and the default settings give the default camera.
    #[test]
    fn diorama_camera_settings_persist_clamp_and_write_only_non_defaults() {
        let untouched = GameSettings::default().to_text();
        assert!(!untouched.contains("diorama_"), "{untouched}");
        assert_eq!(
            GameSettings::default().diorama_camera(),
            rf_renderer::diorama::DioramaCamera::default(),
            "default settings must reproduce the pre-W30-05 camera"
        );
        let tuned = GameSettings {
            diorama_pitch: 30,
            diorama_yaw: -20,
            diorama_zoom: 250,
            diorama_follow: true,
            ..GameSettings::default()
        };
        let text = tuned.to_text();
        assert!(text.contains("diorama_pitch=30") && text.contains("diorama_yaw=-20"));
        assert!(text.contains("diorama_zoom=250") && text.contains("diorama_follow=true"));
        assert_eq!(GameSettings::from_text(&text).expect("parses"), tuned);
        // Only the changed field is written.
        let only_yaw = GameSettings {
            diorama_yaw: 5,
            ..GameSettings::default()
        }
        .to_text();
        assert!(only_yaw.contains("diorama_yaw=5"));
        assert!(!only_yaw.contains("diorama_pitch") && !only_yaw.contains("diorama_zoom"));
        assert!(!only_yaw.contains("diorama_follow"));
        // Wild values clamp, junk keeps the default.
        let wild = GameSettings::from_text(&format!(
            "{MAGIC}\ndiorama_pitch=200\ndiorama_yaw=-99\ndiorama_zoom=9000\n"
        ))
        .expect("parses");
        assert_eq!(wild.diorama_pitch, DIORAMA_PITCH_MAX);
        assert_eq!(wild.diorama_yaw, -DIORAMA_YAW_LIMIT);
        assert_eq!(wild.diorama_zoom, DIORAMA_ZOOM_MAX);
        let junk = GameSettings::from_text(&format!("{MAGIC}\ndiorama_pitch=abc\ndiorama_zoom=\n"))
            .expect("parses");
        assert_eq!(junk.diorama_pitch, DIORAMA_PITCH_DEFAULT);
        assert_eq!(junk.diorama_zoom, DIORAMA_ZOOM_DEFAULT);
        let low = GameSettings::from_text(&format!("{MAGIC}\ndiorama_pitch=0\ndiorama_zoom=10\n"))
            .expect("parses");
        assert_eq!(low.diorama_pitch, DIORAMA_PITCH_MIN);
        assert_eq!(low.diorama_zoom, DIORAMA_ZOOM_DEFAULT);
    }

    /// Ticket W27-05: pins and their corner survive a save.
    #[test]
    fn pinned_items_round_trip() {
        let mut s = GameSettings::default();
        s.pinned_items = vec!["lives".into(), "coins".into()];
        s.info_corner = InfoCorner::BottomRight;
        let back = GameSettings::from_text(&s.to_text()).expect("parses");
        assert_eq!(back.pinned_items, ["lives", "coins"]);
        assert_eq!(back.info_corner, InfoCorner::BottomRight);
        assert!(GameSettings::from_text(&GameSettings::default().to_text())
            .expect("parses")
            .pinned_items
            .is_empty());
    }
}
