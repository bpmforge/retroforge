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
    /// Human-facing name for the badge and the mode picker.
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

/// One game's settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    /// Keys this build does not know, kept verbatim (module doc).
    unknown: BTreeMap<String, String>,
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
        ] {
            if on {
                fields.insert(key.to_string(), "true".to_string());
            }
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
            ..GameSettings::default()
        };
        let restored = GameSettings::from_text(&s.to_text()).expect("parses");
        assert!(restored.deflicker);
        assert!(restored.full_level_view);
        assert!(restored.diorama);
        assert!(restored.mode7_ground);
        assert!(
            !restored.widescreen_decoded,
            "a toggle that was never set must stay off"
        );
    }
}
