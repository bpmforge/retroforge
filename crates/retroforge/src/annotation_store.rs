//! Annotations on disk, keyed by normalized ROM hash (ticket W13-02f,
//! `docs/design/DEBUGGER.md` §4: "persisted per normalized ROM hash under
//! the user data dir ... import/export as JSON").
//!
//! ## One file per game, named by the hash — the same shape as settings
//!
//! `<config>/retroforge/annotations/<normalized-sha256>.rfannot`. This
//! mirrors [`crate::game_settings`] deliberately rather than inventing a
//! second layout: the hash is the same normalized identity
//! [`crate::library`] reports and [`crate::save_state`] verifies against,
//! so a renamed or re-dumped cartridge keeps the labels someone spent an
//! evening finding.
//!
//! ## The stored format IS the export format
//!
//! DEBUGGER.md §4 offers "SQLite or flat RON — decided at ticket time" for
//! persistence and separately names JSON for import/export. Choosing JSON
//! for both collapses two formats into one, and the consequence is the
//! useful part: **"export" is this file, and "import" is dropping someone
//! else's next to yours**. A second on-disk representation would have to
//! be kept in sync with the interchange one for no gain a user can see.
//!
//! ## A store that will not load is not silently an empty one
//!
//! [`load`] reports what happened. `game_settings::load` can fall back to
//! defaults because a game with unreadable settings must still be
//! playable; annotations are *authored work*, and quietly presenting an
//! empty list to someone whose file failed to parse invites them to
//! re-label everything and save over it. So the failure is returned and
//! the panel says so.

use std::path::{Path, PathBuf};

use rf_debugger::annotation::{AnnotationError, AnnotationStore};

/// Sub-directory under the config root.
pub const ANNOTATIONS_DIR: &str = "annotations";
/// Extension for one game's annotation file.
pub const EXTENSION: &str = "rfannot";

/// What [`load`] found.
#[derive(Debug, Default)]
pub struct Loaded {
    pub store: AnnotationStore,
    /// Rows the file held that [`AnnotationStore::add`]'s invariants
    /// refused — surfaced rather than dropped, per `from_json`'s contract.
    pub rejected: Vec<AnnotationError>,
    /// Why nothing loaded, when a file existed but could not be read or
    /// parsed. `None` both when the load succeeded and when there simply
    /// is no file yet — a new game is not an error.
    pub problem: Option<String>,
}

/// Where one game's annotations live under `root`.
#[must_use]
pub fn annotations_path(root: &Path, normalized_sha256: &str) -> PathBuf {
    root.join(crate::bindings_store::APP_DIR)
        .join(ANNOTATIONS_DIR)
        .join(format!("{normalized_sha256}.{EXTENSION}"))
}

/// Load a game's annotations. A missing file yields an empty store with no
/// problem reported; an unreadable or unparseable one yields an empty
/// store **and** a problem string (see this module's doc).
#[must_use]
pub fn load(root: &Path, normalized_sha256: &str) -> Loaded {
    let path = annotations_path(root, normalized_sha256);
    if !path.exists() {
        return Loaded::default();
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            return Loaded {
                problem: Some(format!("{}: {e}", path.display())),
                ..Loaded::default()
            }
        }
    };
    match AnnotationStore::from_json(&text) {
        Ok((store, rejected)) => Loaded {
            store,
            rejected,
            problem: None,
        },
        Err(e) => Loaded {
            problem: Some(format!("{}: {e}", path.display())),
            ..Loaded::default()
        },
    }
}

/// Save a game's annotations.
///
/// # Errors
/// Returns the error message so a caller can surface it — losing an
/// evening of labelling to a silent write failure is exactly the damage
/// this module exists to prevent.
pub fn save(
    root: &Path,
    normalized_sha256: &str,
    store: &AnnotationStore,
) -> Result<PathBuf, String> {
    let path = annotations_path(root, normalized_sha256);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = store.to_json().map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_debugger::annotation::{AddressSpace, Annotation};

    fn temp_root(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rf-w13-02f-annot-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn annotation(label: &str) -> Annotation {
        Annotation {
            space: AddressSpace::Ram,
            addr: 0x0086,
            len: 2,
            ty: "u16".to_string(),
            label: label.to_string(),
            notes: None,
            source: "https://datacrystal.tcrf.net/wiki/Example".to_string(),
            count: None,
        }
    }

    /// Bar B-7 in its smallest honest form: the labels survive a restart
    /// and do not bleed between games.
    #[test]
    fn annotations_persist_keyed_by_hash_and_do_not_bleed_between_games() {
        let root = temp_root("persist");
        let fresh = load(&root, HASH_A);
        assert!(fresh.store.is_empty());
        assert!(fresh.problem.is_none(), "a new game is not an error");

        let mut store = AnnotationStore::new();
        store.add(annotation("player_x_screen")).unwrap();
        save(&root, HASH_A, &store).unwrap();

        let back = load(&root, HASH_A);
        assert_eq!(back.store.entries(), store.entries());
        assert!(back.problem.is_none());
        assert!(load(&root, HASH_B).store.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A corrupt file must say so rather than presenting an empty list
    /// someone would save over.
    #[test]
    fn an_unparseable_file_reports_a_problem_instead_of_looking_empty() {
        let root = temp_root("corrupt");
        let path = annotations_path(&root, HASH_A);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ this is not annotation json").unwrap();

        let loaded = load(&root, HASH_A);
        assert!(loaded.store.is_empty());
        assert!(
            loaded.problem.is_some(),
            "silence here invites re-labelling over a recoverable file"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The store file and the interchange file are one format, so someone
    /// else's export drops in as-is.
    #[test]
    fn a_saved_file_is_itself_valid_import_json() {
        let root = temp_root("interchange");
        let mut store = AnnotationStore::new();
        store.add(annotation("player_x_screen")).unwrap();
        let path = save(&root, HASH_A, &store).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let (imported, rejected) = AnnotationStore::from_json(&text).unwrap();
        assert!(rejected.is_empty());
        assert_eq!(imported.entries(), store.entries());

        let _ = std::fs::remove_dir_all(&root);
    }
}
