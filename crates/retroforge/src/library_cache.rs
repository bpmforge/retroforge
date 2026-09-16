//! What the last library scan found, so the next one does not redo it
//! (ticket W14-02).
//!
//! # What is expensive, and what this actually saves
//!
//! Identity is the normalized hash of the cartridge
//! (`crate::library`'s module doc), so establishing it means **reading and
//! hashing every file** — and since W14-01 that includes decompressing
//! every archive, because a real collection is zipped. Measured on the
//! collection this was built against: **17 seconds in release for 2546
//! archives**, and a folder holding other consoles' games is worse, since
//! an archive must be opened before anyone can know it holds nothing this
//! build runs.
//!
//! None of that changes between launches unless a file does. So each
//! result is remembered against the file's **path, length and modification
//! time** — the same triple `make` has used to decide staleness for fifty
//! years — and a file matching all three is not opened at all.
//!
//! # This is a cache, and it never gets a vote
//!
//! A cache that can be wrong about what is on disk is a bug generator, so:
//!
//! - it is keyed on all three of path, length and mtime, never on path
//!   alone. A ROM replaced in place by a different dump of the same size
//!   still changes its mtime;
//! - a file that is **missing** from the cache is scanned, and a cache
//!   entry whose file no longer exists is simply never consulted and drops
//!   out when the cache is next written;
//! - **a corrupt or unreadable cache file is discarded**, not repaired and
//!   not fatal. `load` returns an empty cache and the scan does its full
//!   job, which is slow exactly once. A library that silently came back
//!   empty because a cache file lost a brace would be a far worse failure
//!   than a slow start.
//!
//! It deliberately remembers **refusals too** — a file this build could
//! not parse, and an archive holding no cartridge at all. Those cost the
//! same decompression to discover as a success does, and a collection
//! containing other consoles is mostly them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::library::{Console, EntryIdentity};

/// File name under the app's config directory.
const FILE_NAME: &str = "library-cache.toml";

/// What a previous scan concluded about one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CachedOutcome {
    /// A cartridge, with the identity everything else keys on.
    Recognized {
        console: Console,
        normalized_sha256: String,
    },
    /// A file this build could not parse, with the reason it gave.
    Unrecognized { reason: String },
    /// An archive holding no NES or SNES cartridge — skipped entirely by
    /// the scan, and remembered so it is not decompressed again. In a
    /// mixed collection this is the majority case.
    NoCartridge,
}

impl CachedOutcome {
    /// The scan's own representation, or `None` for a file the scan does
    /// not list at all.
    #[must_use]
    pub fn to_identity(&self) -> Option<EntryIdentity> {
        match self {
            CachedOutcome::Recognized {
                console,
                normalized_sha256,
            } => Some(EntryIdentity::Recognized {
                console: *console,
                normalized_sha256: normalized_sha256.clone(),
            }),
            CachedOutcome::Unrecognized { reason } => Some(EntryIdentity::Unrecognized {
                reason: reason.clone(),
            }),
            CachedOutcome::NoCartridge => None,
        }
    }

    /// Build from what the scan concluded. `None` means "no cartridge
    /// here", which is a fact worth remembering, not an absence.
    #[must_use]
    pub fn from_identity(identity: Option<&EntryIdentity>) -> Self {
        match identity {
            Some(EntryIdentity::Recognized {
                console,
                normalized_sha256,
            }) => CachedOutcome::Recognized {
                console: *console,
                normalized_sha256: normalized_sha256.clone(),
            },
            Some(EntryIdentity::Unrecognized { reason }) => CachedOutcome::Unrecognized {
                reason: reason.clone(),
            },
            None => CachedOutcome::NoCartridge,
        }
    }
}

/// One remembered file: what it was, and how to tell it has not changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    len: u64,
    /// Modification time as whole seconds since the Unix epoch. Seconds
    /// rather than nanoseconds because that is the resolution every
    /// filesystem in play agrees on, and a sub-second edit that keeps the
    /// exact byte length is not a case worth a finer clock.
    mtime_secs: u64,
    outcome: CachedOutcome,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OnDisk {
    #[serde(default)]
    entries: Vec<Entry>,
}

/// The remembered results of previous scans.
#[derive(Debug, Default, Clone)]
pub struct LibraryCache {
    by_path: HashMap<PathBuf, (u64, u64, CachedOutcome)>,
}

/// Where the cache file lives under `config_root`.
#[must_use]
pub fn cache_path(config_root: &Path) -> PathBuf {
    config_root
        .join(crate::bindings_store::APP_DIR)
        .join(FILE_NAME)
}

/// The length and mtime a file must still have for its cached result to
/// be used. `None` when the file cannot be stated, which makes it a miss.
#[must_use]
pub fn stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((meta.len(), mtime))
}

impl LibraryCache {
    /// Read the cache, or return an empty one.
    ///
    /// Never fails: a missing, unreadable or malformed file is an empty
    /// cache (module doc), because the cost of being wrong here is one
    /// slow scan and the cost of trusting a broken file is a wrong
    /// library.
    #[must_use]
    pub fn load(config_root: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(cache_path(config_root)) else {
            return Self::default();
        };
        let Ok(disk) = toml::from_str::<OnDisk>(&text) else {
            return Self::default();
        };
        Self {
            by_path: disk
                .entries
                .into_iter()
                .map(|e| (e.path, (e.len, e.mtime_secs, e.outcome)))
                .collect(),
        }
    }

    /// What a previous scan concluded about `path`, if the file is
    /// unchanged since. Three-part key: a different length or a different
    /// mtime is a miss.
    #[must_use]
    pub fn get(&self, path: &Path, len: u64, mtime_secs: u64) -> Option<&CachedOutcome> {
        let (cached_len, cached_mtime, outcome) = self.by_path.get(path)?;
        (*cached_len == len && *cached_mtime == mtime_secs).then_some(outcome)
    }

    /// Remember what a scan concluded.
    pub fn insert(&mut self, path: PathBuf, len: u64, mtime_secs: u64, outcome: CachedOutcome) {
        self.by_path.insert(path, (len, mtime_secs, outcome));
    }

    /// How many files are remembered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// Write the cache.
    ///
    /// # Errors
    /// Returns a message when the directory cannot be created or the file
    /// cannot be written. Callers treat that as cosmetic: failing to save
    /// a cache costs speed, never correctness.
    pub fn save(&self, config_root: &Path) -> Result<PathBuf, String> {
        let path = cache_path(config_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut entries: Vec<Entry> = self
            .by_path
            .iter()
            .map(|(path, (len, mtime_secs, outcome))| Entry {
                path: path.clone(),
                len: *len,
                mtime_secs: *mtime_secs,
                outcome: outcome.clone(),
            })
            .collect();
        // Sorted so the file is stable between runs: a cache that reorders
        // itself every launch is noise in a backup or a diff.
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let text = toml::to_string(&OnDisk { entries }).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rf-w14-02-cache-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn recognized(hash: &str) -> CachedOutcome {
        CachedOutcome::Recognized {
            console: Console::Nes,
            normalized_sha256: hash.to_string(),
        }
    }

    #[test]
    fn a_cache_round_trips_through_a_real_file() {
        let root = temp_root("roundtrip");
        let mut cache = LibraryCache::default();
        cache.insert(
            PathBuf::from("/roms/a.nes"),
            40,
            1_700_000_000,
            recognized("aa"),
        );
        cache.insert(
            PathBuf::from("/roms/b.zip"),
            41,
            1_700_000_001,
            CachedOutcome::NoCartridge,
        );
        cache.save(&root).expect("save");

        let loaded = LibraryCache::load(&root);
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            loaded.get(Path::new("/roms/a.nes"), 40, 1_700_000_000),
            Some(&recognized("aa"))
        );
        assert_eq!(
            loaded.get(Path::new("/roms/b.zip"), 41, 1_700_000_001),
            Some(&CachedOutcome::NoCartridge)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_changed_length_or_mtime_is_a_miss() {
        let mut cache = LibraryCache::default();
        cache.insert(
            PathBuf::from("/roms/a.nes"),
            40,
            1_700_000_000,
            recognized("aa"),
        );

        assert!(cache
            .get(Path::new("/roms/a.nes"), 40, 1_700_000_000)
            .is_some());
        assert!(
            cache
                .get(Path::new("/roms/a.nes"), 41, 1_700_000_000)
                .is_none(),
            "a different length must not reuse an identity"
        );
        assert!(
            cache
                .get(Path::new("/roms/a.nes"), 40, 1_700_000_001)
                .is_none(),
            "a ROM replaced in place keeps its length and changes its mtime — that is \
             exactly the case a path-only key would get wrong"
        );
        assert!(cache
            .get(Path::new("/roms/other.nes"), 40, 1_700_000_000)
            .is_none());
    }

    /// The cache never gets a vote: garbage on disk means an empty cache
    /// and a full scan, never a crash and never a wrong library.
    #[test]
    fn a_corrupt_cache_file_is_discarded_rather_than_trusted_or_fatal() {
        let root = temp_root("corrupt");
        let path = cache_path(&root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not toml {{{").unwrap();

        let loaded = LibraryCache::load(&root);
        assert!(
            loaded.is_empty(),
            "a corrupt cache must read as no cache at all"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_cache_file_is_an_empty_cache() {
        let root = temp_root("missing");
        assert!(LibraryCache::load(&root).is_empty());
    }

    #[test]
    fn an_outcome_round_trips_through_the_scan_representation() {
        let identity = EntryIdentity::Recognized {
            console: Console::Snes,
            normalized_sha256: "beef".to_string(),
        };
        let cached = CachedOutcome::from_identity(Some(&identity));
        assert_eq!(cached.to_identity(), Some(identity));

        assert_eq!(
            CachedOutcome::from_identity(None),
            CachedOutcome::NoCartridge
        );
        assert_eq!(CachedOutcome::NoCartridge.to_identity(), None);
    }
}
