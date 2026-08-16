//! Where the binding file lives, and the load/save policy around it
//! (ticket W2-06's "remap UI persists per-user config").
//!
//! ## Path policy, and what is deliberately not decided here
//!
//! `<config-dir>/retroforge/bindings.rfbind`, where `<config-dir>` is the
//! platform's own config location (`$XDG_CONFIG_HOME` or `~/.config` on
//! Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on
//! Windows), resolved from environment variables rather than by adding a
//! `directories`-style dependency for one path.
//!
//! W2-08 owns app-wide settings and will own the app-data-directory policy
//! with it; this module deliberately does not invent that policy, it just
//! needs one file. [`bindings_path`] takes an explicit root so tests never
//! touch a real user's config, and so W2-08 can hand it a different one
//! without a rewrite.
//!
//! ## Load policy: never leave the user with nothing
//!
//! - No file yet ⇒ defaults, and no error (first run).
//! - File present but unparseable as a binding file ⇒ defaults, with the
//!   reason reported, and **the bad file is left on disk** rather than
//!   overwritten, so a user who hand-edited it can fix their typo instead
//!   of losing the whole thing.
//! - File present with some bad lines ⇒ everything else loads, warnings
//!   reported (`rf_input::Bindings::from_text`'s own rule).

use std::path::{Path, PathBuf};

use rf_input::{BindingWarning, Bindings};

/// File name inside the config directory.
pub const FILE_NAME: &str = "bindings.rfbind";
/// Sub-directory under the platform config root.
pub const APP_DIR: &str = "retroforge";

/// The platform config root, or `None` when the environment says nothing.
///
/// Reads the environment and hands the values to [`config_root_from`],
/// which holds the actual policy. Split that way because `std::env::set_var`
/// is `unsafe` (Rust 2024) and this workspace forbids `unsafe` outright, so
/// a test that mutated the environment could not exist here — and a policy
/// nobody can test is a policy nobody can trust.
#[must_use]
pub fn config_root() -> Option<PathBuf> {
    config_root_from(
        std::env::var_os("RETROFORGE_CONFIG_DIR").map(PathBuf::from),
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("APPDATA").map(PathBuf::from),
    )
}

/// The path policy itself, as a pure function of the four environment
/// values that can decide it.
///
/// `RETROFORGE_CONFIG_DIR` wins everywhere — it is what a test, a portable
/// install or a user with an opinion sets. After that the platform's own
/// convention applies, and `None` means "this environment has nowhere to
/// put a config", which the caller degrades to session-only bindings rather
/// than a panic.
#[must_use]
pub fn config_root_from(
    explicit: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
    appdata: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        return Some(explicit);
    }
    if cfg!(target_os = "windows") {
        return appdata;
    }
    if cfg!(target_os = "macos") {
        return home.map(|home| home.join("Library/Application Support"));
    }
    xdg_config_home.or_else(|| home.map(|home| home.join(".config")))
}

/// The binding file's path under `root`.
#[must_use]
pub fn bindings_path(root: &Path) -> PathBuf {
    root.join(APP_DIR).join(FILE_NAME)
}

/// What a load did — reported so the UI can say something rather than
/// silently substituting defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    /// No file yet: first run.
    Defaulted,
    /// Loaded, possibly with skipped lines.
    Loaded(Vec<BindingWarning>),
    /// A file exists but could not be read or is not a binding file; the
    /// defaults are in force and the file was left untouched.
    Rejected(String),
}

/// Load bindings from `root`, never failing.
#[must_use]
pub fn load(root: &Path) -> (Bindings, LoadOutcome) {
    let path = bindings_path(root);
    if !path.is_file() {
        return (Bindings::default(), LoadOutcome::Defaulted);
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => return (Bindings::default(), LoadOutcome::Rejected(e.to_string())),
    };
    match Bindings::from_text(&text) {
        Ok((bindings, warnings)) => (bindings, LoadOutcome::Loaded(warnings)),
        Err(e) => (Bindings::default(), LoadOutcome::Rejected(e.to_string())),
    }
}

/// Save bindings under `root`, creating the directory if needed.
///
/// # Errors
/// Returns the I/O error message; a caller should surface it rather than
/// assume the remap stuck.
pub fn save(root: &Path, bindings: &Bindings) -> Result<PathBuf, String> {
    let path = bindings_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, bindings.to_text()).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_input::{Key, NesButton, PadButton};

    fn temp_root(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // pid + sequence: libtest runs a binary's tests in parallel threads
        // that share one pid, so pid alone races (the flake W1-03 fixed in
        // rf-harness).
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("rf-w2-06-{label}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The acceptance criterion, end to end through a real file: a remap
    /// made now is the binding on the next run.
    #[test]
    fn a_remap_persists_across_a_save_and_reload() {
        let root = temp_root("persist");
        let (mut bindings, outcome) = load(&root);
        assert_eq!(outcome, LoadOutcome::Defaulted, "first run has no file");

        bindings.keys.bind(Key::Q, 0, NesButton::A);
        bindings.pads.bind(PadButton::West, NesButton::B);
        let path = save(&root, &bindings).expect("save");
        assert!(path.is_file());

        let (reloaded, outcome) = load(&root);
        assert_eq!(outcome, LoadOutcome::Loaded(Vec::new()));
        assert_eq!(reloaded.keys.lookup(Key::Q), Some((0, NesButton::A)));
        assert_eq!(reloaded.pads.lookup(PadButton::West), Some(NesButton::B));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A corrupt config must not cost the user their bindings *file* — they
    /// get working defaults now and can still fix the typo later.
    #[test]
    fn a_corrupt_file_falls_back_to_defaults_and_is_left_on_disk() {
        let root = temp_root("corrupt");
        std::fs::create_dir_all(bindings_path(&root).parent().unwrap()).unwrap();
        std::fs::write(bindings_path(&root), "not a binding file at all\n").unwrap();

        let (bindings, outcome) = load(&root);
        assert!(matches!(outcome, LoadOutcome::Rejected(_)));
        assert_eq!(
            bindings.keys.lookup(Key::ArrowUp),
            Some((0, NesButton::Up)),
            "defaults must be in force"
        );
        assert!(
            bindings_path(&root).is_file(),
            "the user's file must be left alone, not overwritten"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn saving_creates_the_directory_when_it_does_not_exist() {
        let root = temp_root("mkdir");
        assert!(!root.exists());
        save(&root, &Bindings::default()).expect("save creates its directory");
        assert!(bindings_path(&root).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The override exists so nothing in the test suite — and no portable
    /// install — is stuck with the platform default. It must win over
    /// every other source.
    #[test]
    fn the_explicit_override_beats_every_platform_default() {
        let picked = config_root_from(
            Some(PathBuf::from("/explicit")),
            Some(PathBuf::from("/xdg")),
            Some(PathBuf::from("/home")),
            Some(PathBuf::from("/appdata")),
        );
        assert_eq!(picked, Some(PathBuf::from("/explicit")));
    }

    #[test]
    fn the_platform_default_is_used_when_there_is_no_override() {
        let picked = config_root_from(
            None,
            Some(PathBuf::from("/xdg")),
            Some(PathBuf::from("/home")),
            Some(PathBuf::from("/appdata")),
        );
        let expected = if cfg!(target_os = "windows") {
            PathBuf::from("/appdata")
        } else if cfg!(target_os = "macos") {
            PathBuf::from("/home/Library/Application Support")
        } else {
            PathBuf::from("/xdg")
        };
        assert_eq!(picked, Some(expected));
    }

    /// An environment with nothing set at all means "no persistence", which
    /// the caller turns into session-only bindings — never a panic and
    /// never a write to some guessed path.
    #[test]
    fn an_empty_environment_yields_no_config_root() {
        assert_eq!(config_root_from(None, None, None, None), None);
    }
}
