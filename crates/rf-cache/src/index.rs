//! In-memory + persisted LRU index.
//!
//! Unlike `.rfstate` (`docs/design/CONTRACTS.md` §3), this index is **not**
//! a public/versioned contract -- it is a fully rebuildable cache of
//! information every entry file already carries in its own header
//! (`entry.rs`). Losing or corrupting `index.bin` is recoverable, never
//! data loss: see [`Index::rebuild_from_entries`], which every `Cache`
//! falls back to when the persisted index is absent or fails to decode.
//!
//! LRU order is tracked by a monotonic access counter, not wall-clock
//! time. `SystemTime`/mtime/atime are all unusable here: mtime never
//! moves on a read, atime is commonly disabled at the OS/mount level, and
//! wall-clock values are not reproducible across runs. A `u64` counter
//! that advances on every touch is what makes the persisted order
//! reproducible, and is what lets a post-restart eviction test assert a
//! *specific* entry rather than "some entry after `SystemTime::now()`".
//! (`scripts/validate-arch.sh` rule 4's wall-clock ban does not cover this
//! crate -- this is a design requirement, not something the linter
//! enforces.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bincode::{Decode, Encode};

/// One entry's residency bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub(crate) struct IndexEntry {
    pub access_counter: u64,
    pub byte_len: u64,
}

/// The whole index: per-entry bookkeeping keyed by content hash (the
/// entry's filename stem), plus the counter needed to keep assigning
/// fresh values after a reload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Encode, Decode)]
pub(crate) struct Index {
    pub next_counter: u64,
    pub entries: HashMap<String, IndexEntry>,
}

impl Index {
    pub(crate) fn total_bytes(&self) -> u64 {
        self.entries.values().map(|e| e.byte_len).sum()
    }

    /// Next access-counter value, advancing the clock. Used for both a
    /// fresh `put` and a `get`-triggered touch -- the same counter serves
    /// both, which is what makes "least recently used" mean exactly that
    /// (an entry inserted and never read again ages just like one that
    /// was read once and never again).
    pub(crate) fn tick(&mut self) -> u64 {
        let v = self.next_counter;
        self.next_counter += 1;
        v
    }

    /// Content hash of the least-recently-used entry, if any. Ties break
    /// on the hash string itself so eviction is fully deterministic given
    /// the same on-disk state -- real counters are never actually tied
    /// (each `tick()` call is unique), so this only matters as a
    /// reproducibility guarantee, never in the correct-behavior path.
    pub(crate) fn lru_hash(&self) -> Option<String> {
        self.entries
            .iter()
            .min_by(|(h1, e1), (h2, e2)| {
                e1.access_counter
                    .cmp(&e2.access_counter)
                    .then_with(|| h1.cmp(h2))
            })
            .map(|(h, _)| h.clone())
    }

    /// Rebuild the index from the entries directory alone, ignoring
    /// `index.bin` entirely. Called when the persisted index is missing
    /// or fails to decode.
    ///
    /// Recovered order is a deterministic function of filename (sorted
    /// ascending, oldest counter first) rather than the entries' true
    /// historical access order -- that information lives only in
    /// `index.bin`, so losing it also loses true LRU order. This is a
    /// documented, accepted degradation (recovery restores *availability*
    /// and *byte accounting*, not history), not a silent one: every
    /// recovered entry still gets a real, monotonically increasing
    /// counter, so eviction after a recovery is deterministic and
    /// resumes normal LRU behavior for anything touched from then on.
    ///
    /// Any entry file that fails to decode, or whose filename does not
    /// match the content hash of its own header (tampering, or a
    /// collision this crate did not create), is skipped rather than
    /// aborting the whole rebuild -- one damaged entry must not make the
    /// rest of the cache unrecoverable. This is also what keeps a
    /// symlink planted directly in the entries directory from being
    /// followed during recovery: `entry::read_entry` enforces containment
    /// on every path it reads, and a refusal here is just another decode
    /// failure to skip.
    pub(crate) fn rebuild_from_entries(root: &Path, entries_dir: &Path) -> Self {
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Ok(read_dir) = std::fs::read_dir(entries_dir) {
            for dir_entry in read_dir.flatten() {
                let path = dir_entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("bin") {
                    paths.push(path);
                }
            }
        }
        paths.sort();

        let mut index = Index::default();
        for path in paths {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(entry) = crate::entry::read_entry(root, &path) else {
                continue;
            };
            if entry.key.content_hash() != stem {
                continue;
            }
            let counter = index.tick();
            index.entries.insert(
                stem.to_string(),
                IndexEntry {
                    access_counter: counter,
                    byte_len: entry.payload.len() as u64,
                },
            );
        }
        index
    }
}
