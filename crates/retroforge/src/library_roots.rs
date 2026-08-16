//! The configured library folders (tickets W2-07, absorbed by W2-08).
//!
//! ## This is now a view onto `settings.toml`
//!
//! W2-07 needed somewhere to keep its roots before an app-wide settings
//! system existed and said in its own notes that W2-08 would absorb it.
//! This is that absorption: the roots live in `[paths] library_folders`
//! (`crate::settings`), and this module is the thin accessor the library
//! screen already calls, kept so the call sites did not have to move at the
//! same time as the storage.
//!
//! ## The legacy file is migrated, not abandoned
//!
//! W2-07's `library.rflib` is read once, folded into the settings file, and
//! then **deleted** — so a user who configured folders under the previous
//! build does not silently lose them and does not end up with two files
//! disagreeing about what their library is. Migration is idempotent: once
//! the legacy file is gone, [`load`] is a plain settings read.

use std::path::{Path, PathBuf};

/// The pre-W2-08 file name, still read once so its contents can be
/// migrated.
pub const LEGACY_FILE_NAME: &str = "library.rflib";
const LEGACY_MAGIC: &str = "RFLIB 1";

/// Where the pre-W2-08 roots file lived.
#[must_use]
pub fn legacy_roots_path(config_root: &Path) -> PathBuf {
    config_root
        .join(crate::bindings_store::APP_DIR)
        .join(LEGACY_FILE_NAME)
}

/// The configured library folders, migrating W2-07's file if it is still
/// there.
#[must_use]
pub fn load(config_root: &Path) -> Vec<PathBuf> {
    let (mut settings, _) = crate::settings::load(config_root);

    if let Some(legacy) = read_legacy(config_root) {
        // Union rather than replace: a user who configured folders in both
        // places keeps both, and the order stays theirs.
        for root in legacy {
            if !settings.paths.library_folders.contains(&root) {
                settings.paths.library_folders.push(root);
            }
        }
        if crate::settings::save(config_root, &settings).is_ok() {
            // Only after the new home is definitely written.
            let _ = std::fs::remove_file(legacy_roots_path(config_root));
        }
    }

    settings.paths.library_folders
}

/// Replace the configured library folders.
///
/// # Errors
/// Returns the error message from writing `settings.toml`.
pub fn save(config_root: &Path, roots: &[PathBuf]) -> Result<PathBuf, String> {
    let (mut settings, _) = crate::settings::load(config_root);
    settings.paths.library_folders = roots.to_vec();
    crate::settings::save(config_root, &settings)
}

/// Read W2-07's file if it exists and is ours.
fn read_legacy(config_root: &Path) -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string(legacy_roots_path(config_root)).ok()?;
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(LEGACY_MAGIC) {
        return None;
    }
    Some(
        lines
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(PathBuf::from)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rf-w2-08-roots-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write_legacy(config_root: &Path, body: &str) {
        let path = legacy_roots_path(config_root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn roots_round_trip_through_the_settings_file() {
        let config = temp_root("roundtrip");
        assert!(load(&config).is_empty());

        let roots = vec![PathBuf::from("/roms/nes"), PathBuf::from("/mnt/nas")];
        save(&config, &roots).expect("save");
        assert_eq!(load(&config), roots);

        // ...and they really are in settings.toml, not somewhere private.
        let (settings, _) = crate::settings::load(&config);
        assert_eq!(settings.paths.library_folders, roots);

        let _ = std::fs::remove_dir_all(&config);
    }

    /// The migration, and the property that matters about it: the user's
    /// folders survive, and the old file goes away so two files cannot
    /// disagree about what the library is.
    #[test]
    fn the_legacy_file_is_migrated_then_removed() {
        let config = temp_root("migrate");
        write_legacy(&config, "RFLIB 1\n/roms/old\n");

        let roots = load(&config);
        assert_eq!(roots, vec![PathBuf::from("/roms/old")]);
        assert!(
            !legacy_roots_path(&config).exists(),
            "the legacy file must be removed once its contents are safely in settings.toml"
        );
        let (settings, _) = crate::settings::load(&config);
        assert_eq!(settings.paths.library_folders, roots);

        // Idempotent: a second load is a plain settings read.
        assert_eq!(load(&config), roots);

        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn migration_unions_with_folders_already_in_settings_rather_than_replacing_them() {
        let config = temp_root("union");
        save(&config, &[PathBuf::from("/roms/new")]).expect("save");
        write_legacy(&config, "RFLIB 1\n/roms/old\n/roms/new\n");

        assert_eq!(
            load(&config),
            vec![PathBuf::from("/roms/new"), PathBuf::from("/roms/old")],
            "both survive, in a stable order, with no duplicate"
        );
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn a_legacy_file_that_is_not_ours_is_ignored_rather_than_read_as_paths() {
        let config = temp_root("foreign");
        write_legacy(&config, "/etc/passwd\n/tmp\n");
        assert!(load(&config).is_empty());
        let _ = std::fs::remove_dir_all(&config);
    }
}
