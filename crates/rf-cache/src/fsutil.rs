//! Filesystem helpers shared by entry and index persistence: an atomic
//! same-directory write, and the NFR-010 containment check.
//!
//! There is no prior art for realpath containment anywhere else in this
//! repo (zero `canonicalize`/`realpath` hits outside tests as of this
//! ticket) -- this module is what sets the pattern.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::CacheError;

/// Write `bytes` to `path` atomically: encode to a uniquely-named temp
/// file in the same directory, `fsync` it, then rename over `path`.
/// Same-directory temp + rename keeps the replace atomic on one
/// filesystem and never leaves a half-written file visible at `path`.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CacheError> {
    let dir = path
        .parent()
        .expect("cache paths are always joined under a directory");

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp_path = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));

    {
        let mut f = std::fs::File::create(&tmp_path).map_err(|source| CacheError::Io {
            context: "create temp file for atomic write",
            message: source.to_string(),
        })?;
        f.write_all(bytes).map_err(|source| CacheError::Io {
            context: "write temp file for atomic write",
            message: source.to_string(),
        })?;
        f.sync_all().map_err(|source| CacheError::Io {
            context: "fsync temp file for atomic write",
            message: source.to_string(),
        })?;
    }

    std::fs::rename(&tmp_path, path).map_err(|source| CacheError::Io {
        context: "rename temp file into place",
        message: source.to_string(),
    })?;
    Ok(())
}

/// NFR-010: refuse `candidate` unless (a) it is not itself a symlink, and
/// (b) its deepest *existing* ancestor canonicalizes to `root` or a
/// descendant of `root`.
///
/// Two traps this exists to avoid:
///
/// - `std::fs::canonicalize` fails on a path that does not exist yet, and
///   a cache exists specifically to create files that do not exist yet --
///   so canonicalizing `candidate` directly is not an option. Instead we
///   canonicalize its deepest existing ancestor (for a fresh entry file,
///   that is the `entries/` directory itself).
/// - `root` must already be canonical (resolved once, at
///   [`crate::Cache::open`]) so every comparison here is
///   canonical-to-canonical. Comparing a canonicalized child against an
///   un-canonicalized root would make a *correct* implementation look
///   broken on macOS, where `std::env::temp_dir()` sits under `/var` ->
///   `/private/var`.
///
/// The leaf-symlink check is what catches a symlink planted *at* an
/// entry's own path (whether or not its target exists, and regardless of
/// where it points) -- ancestor canonicalization alone would miss a
/// dangling symlink, since `Path::exists()` follows symlinks and reports
/// `false` for a dangling one, which would make the ancestor walk skip
/// straight past it to the (legitimate) parent directory.
pub(crate) fn ensure_contained(root: &Path, candidate: &Path) -> Result<(), CacheError> {
    if let Ok(meta) = std::fs::symlink_metadata(candidate) {
        if meta.file_type().is_symlink() {
            return Err(CacheError::Containment {
                context: "cache path is a symlink",
                path: candidate.to_path_buf(),
            });
        }
    }

    let mut probe: PathBuf = candidate.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            return Err(CacheError::Containment {
                context: "no existing ancestor found for containment check",
                path: candidate.to_path_buf(),
            });
        }
    }

    let canonical_ancestor = probe.canonicalize().map_err(|source| CacheError::Io {
        context: "canonicalize ancestor for containment check",
        message: source.to_string(),
    })?;
    if canonical_ancestor != root && !canonical_ancestor.starts_with(root) {
        return Err(CacheError::Containment {
            context: "path resolves outside cache root",
            path: candidate.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn unique_dir(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rf-cache-fsutil-test-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn allows_a_plain_nested_candidate() {
        let root = unique_dir("ok-root");
        let sub = root.join("entries");
        std::fs::create_dir_all(&sub).unwrap();

        ensure_contained(&root, &sub.join("does-not-exist-yet.bin")).unwrap();

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_when_the_leaf_itself_is_a_symlink() {
        let root = unique_dir("leaf-root");
        let outside = unique_dir("leaf-outside");
        let outside_file = outside.join("target.bin");
        std::fs::write(&outside_file, b"x").unwrap();
        let sub = root.join("entries");
        std::fs::create_dir_all(&sub).unwrap();
        let leaf = sub.join("poisoned.bin");
        symlink(&outside_file, &leaf).unwrap();

        let err = ensure_contained(&root, &leaf).unwrap_err();
        assert!(matches!(err, CacheError::Containment { .. }));

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// Isolates the ANCESTOR-canonicalization branch specifically, as
    /// distinct from the leaf-symlink check above: here the candidate
    /// itself (`entries/not-yet-written.bin`) does not exist at all, so
    /// `symlink_metadata` on it fails and the leaf check never fires --
    /// only the *parent* (`entries/`) is a symlink pointing outside root.
    /// A build that dropped the ancestor-walk/canonicalize logic but kept
    /// the leaf check would pass `refuses_when_the_leaf_itself_is_a_symlink`
    /// above yet wrongly accept this one.
    #[test]
    fn refuses_when_an_ancestor_directory_is_a_symlink_pointing_outside() {
        let root = unique_dir("ancestor-root");
        let outside = unique_dir("ancestor-outside");
        let poisoned_entries_dir = root.join("entries");
        symlink(&outside, &poisoned_entries_dir).unwrap();

        let candidate = poisoned_entries_dir.join("not-yet-written.bin");
        assert!(!candidate.exists(), "candidate must not exist yet");

        let err = ensure_contained(&root, &candidate).unwrap_err();
        assert!(matches!(err, CacheError::Containment { .. }));

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// Isolates the LEAF-symlink branch, which
    /// `refuses_when_the_leaf_itself_is_a_symlink` above does *not*: that
    /// test points its symlink at an **existing** file, so
    /// `Path::exists()` follows the link, the ancestor walk canonicalizes
    /// straight to the outside target and refuses it anyway -- meaning
    /// deleting the leaf check entirely still leaves that test green
    /// (verified by mutation).
    ///
    /// A **dangling** symlink is the case the leaf check actually exists
    /// for, and it is the doc comment's stated justification: `exists()`
    /// reports `false` for a dangling link, so the ancestor walk pops
    /// straight past it to the legitimate `entries/` parent and the path
    /// is wrongly accepted. Nothing escapes today only because every
    /// write goes through [`atomic_write`], and `rename(2)` replaces the
    /// symlink rather than following it -- so this is defense in depth
    /// against a future caller reaching for `File::create`, which *would*
    /// follow the link and create a file outside the root (NFR-010).
    #[test]
    fn refuses_a_dangling_leaf_symlink_pointing_outside() {
        let root = unique_dir("dangling-root");
        let outside = unique_dir("dangling-outside");
        let sub = root.join("entries");
        std::fs::create_dir_all(&sub).unwrap();

        // Target deliberately never created.
        let never_created = outside.join("never-created.bin");
        let leaf = sub.join("poisoned.bin");
        symlink(&never_created, &leaf).unwrap();

        assert!(
            !leaf.exists(),
            "exists() follows symlinks and must report false for a dangling one -- \
             that is precisely why the ancestor walk alone cannot catch this"
        );
        assert!(
            std::fs::symlink_metadata(&leaf)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself must still be present"
        );

        let err = ensure_contained(&root, &leaf).unwrap_err();
        assert!(matches!(err, CacheError::Containment { .. }));

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
