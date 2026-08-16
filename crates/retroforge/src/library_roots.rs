//! The configured library folders, persisted (ticket W2-07).
//!
//! ## Why this is its own tiny store
//!
//! W2-08 owns app-wide settings screens and will own where a list like this
//! finally lives. The library needs *somewhere* to keep its roots now, so
//! this is deliberately the smallest thing that works: one file, one path
//! per line, in the same config directory as the bindings, with the same
//! "never leave the user with nothing" load policy. It is a handful of
//! lines for W2-08 to absorb, and absorbing it is easier than unpicking a
//! settings framework invented early.
//!
//! ## Roots are stored as written, resolved at scan time
//!
//! A root is saved exactly as the user chose it — not canonicalized —
//! because the canonical form of a path on a removable drive is not stable
//! across mounts, and a config that silently rewrote `~/roms` into
//! `/Volumes/…` would confuse anyone who read it. Containment
//! (NFR-010/D-006) resolves the root at scan time instead, which is where
//! it has to happen anyway: a symlink can be introduced after the folder
//! was configured.

use std::path::{Path, PathBuf};

/// File name inside the config directory.
pub const FILE_NAME: &str = "library.rflib";
const MAGIC: &str = "RFLIB 1";

/// Where the roots list lives under `root`.
#[must_use]
pub fn roots_path(config_root: &Path) -> PathBuf {
    config_root
        .join(crate::bindings_store::APP_DIR)
        .join(FILE_NAME)
}

/// Load the configured roots. An absent or unreadable file means "none
/// configured", which the library screen turns into its first-run call to
/// action rather than an error.
#[must_use]
pub fn load(config_root: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(roots_path(config_root)) else {
        return Vec::new();
    };
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        return Vec::new();
    }
    lines
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect()
}

/// Save the configured roots.
///
/// # Errors
/// Returns the I/O error message.
pub fn save(config_root: &Path, roots: &[PathBuf]) -> Result<PathBuf, String> {
    let path = roots_path(config_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut text = String::from(MAGIC);
    text.push('\n');
    for root in roots {
        text.push_str(&root.to_string_lossy());
        text.push('\n');
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rf-w2-07-roots-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn roots_round_trip_in_the_order_they_were_configured() {
        let config = temp_root("roundtrip");
        assert!(load(&config).is_empty(), "nothing configured yet");

        let roots = vec![
            PathBuf::from("/home/someone/roms"),
            PathBuf::from("/mnt/nas/nes"),
        ];
        save(&config, &roots).expect("save");
        assert_eq!(load(&config), roots, "order is the user's, so it is kept");

        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn a_file_that_is_not_ours_reads_as_no_roots_rather_than_as_paths() {
        let config = temp_root("foreign");
        let path = roots_path(&config);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "/etc/passwd\n/tmp\n").unwrap();
        assert!(
            load(&config).is_empty(),
            "without the magic line these are not our paths and must not be scanned"
        );
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn blank_lines_and_comments_are_ignored() {
        let config = temp_root("comments");
        let path = roots_path(&config);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "RFLIB 1\n\n# my roms\n/roms\n").unwrap();
        assert_eq!(load(&config), vec![PathBuf::from("/roms")]);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn saving_an_empty_list_clears_the_roots() {
        let config = temp_root("clear");
        save(&config, &[PathBuf::from("/roms")]).expect("save");
        assert_eq!(load(&config).len(), 1);
        save(&config, &[]).expect("save empty");
        assert!(load(&config).is_empty());
        let _ = std::fs::remove_dir_all(&config);
    }
}
