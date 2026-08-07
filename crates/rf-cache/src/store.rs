//! Content-addressed, LRU-bounded asset cache (`ARCHITECTURE.md` §7,
//! ticket W4-08).
//!
//! Generic over opaque payload bytes -- see the module doc on
//! [`crate::CanvasChunk`] for why this crate does not yet know how to
//! serialize a canvas chunk (that is ticket W4-03b's job).

use std::path::{Path, PathBuf};

use crate::entry::{read_entry, write_entry};
use crate::error::CacheError;
use crate::fsutil::{atomic_write, ensure_contained};
use crate::index::{Index, IndexEntry};
use crate::key::CacheKey;

const INDEX_FILE_NAME: &str = "index.bin";
const ENTRIES_DIR_NAME: &str = "entries";

/// A content-addressed cache rooted at a directory, bounded to a
/// configurable total-byte cap with least-recently-used eviction.
///
/// The cap is a constructor parameter (acceptance 2): `ARCHITECTURE.md`
/// §7 notes it will eventually be wired to a W2-08 Paths settings UI, but
/// that ticket is still todo, so there is no UI here -- only the
/// parameter it will end up setting.
pub struct Cache {
    /// Canonicalized cache root. Resolved once, here, so every later
    /// containment check (`fsutil::ensure_contained`) compares
    /// canonical-to-canonical (NFR-010).
    root: PathBuf,
    entries_dir: PathBuf,
    index_path: PathBuf,
    cap_bytes: u64,
    index: Index,
}

impl Cache {
    /// Open (creating if needed) a cache rooted at `root`, bounded to
    /// `cap_bytes` total payload bytes.
    ///
    /// `root` may itself be a symlink (W2-08 will let a user point the
    /// cache root anywhere) -- it is resolved to its real location once,
    /// here, and every subsequent path this store touches is validated
    /// against that resolved root.
    pub fn open(root: impl AsRef<Path>, cap_bytes: u64) -> Result<Self, CacheError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|source| CacheError::Io {
            context: "create cache root",
            message: source.to_string(),
        })?;
        let root = root.canonicalize().map_err(|source| CacheError::Io {
            context: "canonicalize cache root",
            message: source.to_string(),
        })?;

        let entries_dir = root.join(ENTRIES_DIR_NAME);
        std::fs::create_dir_all(&entries_dir).map_err(|source| CacheError::Io {
            context: "create entries directory",
            message: source.to_string(),
        })?;
        ensure_contained(&root, &entries_dir)?;

        let index_path = root.join(INDEX_FILE_NAME);
        ensure_contained(&root, &index_path)?;
        let index = load_or_rebuild_index(&root, &entries_dir, &index_path);

        let cache = Cache {
            root,
            entries_dir,
            index_path,
            cap_bytes,
            index,
        };
        // Self-heal: persist the (possibly just-rebuilt) index so a
        // missing/corrupt index.bin does not stay missing/corrupt.
        cache.persist_index()?;
        Ok(cache)
    }

    /// Look up `key`, returning its payload bytes if present. A hit
    /// counts as a touch: it becomes the most-recently-used entry.
    pub fn get(&mut self, key: &CacheKey) -> Result<Option<Vec<u8>>, CacheError> {
        let hash = key.content_hash();
        if !self.index.entries.contains_key(&hash) {
            return Ok(None);
        }

        let path = self.entries_dir.join(format!("{hash}.bin"));
        let entry = match read_entry(&self.root, &path) {
            Ok(entry) => entry,
            Err(e @ CacheError::Containment { .. }) => {
                // A containment violation means the on-disk entry was
                // tampered with (NFR-010), not merely absent -- surface
                // it rather than silently treating it as a miss.
                return Err(e);
            }
            Err(_) => {
                // The index says this entry is present but it is
                // genuinely missing/corrupt on disk -- heal the index and
                // report a miss rather than propagating an internal
                // inconsistency to the caller.
                self.index.entries.remove(&hash);
                self.persist_index()?;
                return Ok(None);
            }
        };
        if entry.key != *key {
            // A hash collision or a tampered header -- never return the
            // wrong asset for the key that was asked for.
            return Ok(None);
        }

        let counter = self.index.tick();
        if let Some(indexed) = self.index.entries.get_mut(&hash) {
            indexed.access_counter = counter;
        }
        self.persist_index()?;

        Ok(Some(entry.payload))
    }

    /// Insert or overwrite `key` with `payload`, then evict
    /// least-recently-used entries until total resident bytes are within
    /// `cap_bytes` (acceptance 2).
    pub fn put(&mut self, key: &CacheKey, payload: &[u8]) -> Result<(), CacheError> {
        let hash = key.content_hash();
        let path = self.entries_dir.join(format!("{hash}.bin"));
        write_entry(&self.root, &path, key, payload)?;

        let counter = self.index.tick();
        self.index.entries.insert(
            hash,
            IndexEntry {
                access_counter: counter,
                byte_len: payload.len() as u64,
            },
        );

        self.evict_to_cap();
        self.persist_index()
    }

    /// Whether `key` is currently resident.
    #[must_use]
    pub fn contains(&self, key: &CacheKey) -> bool {
        self.index.entries.contains_key(&key.content_hash())
    }

    /// Number of resident entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.entries.len()
    }

    /// Whether the cache currently holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.entries.is_empty()
    }

    /// Total payload bytes currently resident (sum of every entry's
    /// payload length; excludes header/index overhead).
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.index.total_bytes()
    }

    /// The configured size cap, in bytes.
    #[must_use]
    pub fn cap_bytes(&self) -> u64 {
        self.cap_bytes
    }

    fn evict_to_cap(&mut self) {
        while self.index.total_bytes() > self.cap_bytes && !self.index.entries.is_empty() {
            let Some(victim) = self.index.lru_hash() else {
                break;
            };
            self.index.entries.remove(&victim);
            let path = self.entries_dir.join(format!("{victim}.bin"));
            if ensure_contained(&self.root, &path).is_ok() {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    fn persist_index(&self) -> Result<(), CacheError> {
        let bytes = bincode::encode_to_vec(&self.index, bincode::config::standard())
            .map_err(|e| CacheError::Encode(e.to_string()))?;
        atomic_write(&self.index_path, &bytes)
    }
}

fn load_or_rebuild_index(root: &Path, entries_dir: &Path, index_path: &Path) -> Index {
    match std::fs::read(index_path) {
        Ok(bytes) => bincode::decode_from_slice::<Index, _>(&bytes, bincode::config::standard())
            .map(|(index, _)| index)
            .unwrap_or_else(|_| Index::rebuild_from_entries(root, entries_dir)),
        Err(_) => Index::rebuild_from_entries(root, entries_dir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn unique_dir(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rf-cache-store-test-{label}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a unique path under the system temp dir WITHOUT creating it
    /// -- used for the symlink-root test, where the path itself must not
    /// exist as a real directory before `Cache::open` sees it.
    fn unique_missing_path(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "rf-cache-store-test-{label}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn test_key(tag: &str) -> CacheKey {
        CacheKey {
            rom_sha256: "rom-abc".to_string(),
            asset_hash: format!("asset-{tag}"),
            producer: "test-producer".to_string(),
            settings_hash: "settings-1".to_string(),
        }
    }

    #[test]
    fn put_then_get_round_trips_payload() {
        let root = unique_dir("roundtrip");
        let mut cache = Cache::open(&root, 1_000_000).unwrap();
        let key = test_key("a");

        assert!(!cache.contains(&key));
        cache.put(&key, b"hello").unwrap();
        assert!(cache.contains(&key));
        assert_eq!(cache.get(&key).unwrap().unwrap(), b"hello");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn get_on_absent_key_is_none_not_error() {
        let root = unique_dir("absent");
        let mut cache = Cache::open(&root, 1_000_000).unwrap();
        assert_eq!(cache.get(&test_key("missing")).unwrap(), None);
        std::fs::remove_dir_all(&root).ok();
    }

    /// LRU-vs-FIFO vacuity trap: a test that only inserts cannot tell LRU
    /// and FIFO apart (both evict the same entry). This one inserts A and
    /// B, TOUCHES A via `get`, then inserts C to genuinely exceed the cap
    /// -- true LRU must evict B (least recently used), while a FIFO
    /// implementation evicts A (oldest inserted) instead. It also checks
    /// the evicted entry's file is genuinely gone from disk, and the
    /// retained entry still reads back correctly.
    #[test]
    fn lru_eviction_prefers_least_recently_touched_over_least_recently_inserted() {
        let root = unique_dir("lru");
        let payload = vec![0xABu8; 100];
        // Two entries fit (200 <= 250); a third genuinely exceeds the cap
        // (300 > 250), forcing exactly one eviction.
        let mut cache = Cache::open(&root, 250).unwrap();

        let key_a = test_key("a");
        let key_b = test_key("b");
        let key_c = test_key("c");

        cache.put(&key_a, &payload).unwrap();
        cache.put(&key_b, &payload).unwrap();
        // Touch A: it becomes most-recently-used, B becomes LRU.
        assert_eq!(cache.get(&key_a).unwrap().unwrap(), payload);

        cache.put(&key_c, &payload).unwrap();

        assert!(!cache.contains(&key_b), "B (LRU) must be evicted");
        assert!(cache.contains(&key_a), "A (recently touched) must survive");
        assert!(cache.contains(&key_c), "C (just inserted) must survive");

        // Genuinely gone from disk, not just absent from the index.
        let evicted_path = root
            .join(ENTRIES_DIR_NAME)
            .join(format!("{}.bin", key_b.content_hash()));
        assert!(!evicted_path.exists());

        // The retained entry still reads back correctly.
        assert_eq!(cache.get(&key_a).unwrap().unwrap(), payload);

        std::fs::remove_dir_all(&root).ok();
    }

    /// Persistence vacuity trap: "reopen and the value is still there" is
    /// the weak version. This asserts all three things a reopen must
    /// preserve: the value, the accumulated byte total, and the access
    /// ordering -- proving the third by evicting AFTER the restart and
    /// checking the entry that was LRU *before* the restart is the one
    /// that goes.
    #[test]
    fn persistence_across_restart_preserves_value_bytes_and_lru_order() {
        let root = unique_dir("persistence");
        let payload = vec![0xCDu8; 100];
        let key_a = test_key("a");
        let key_b = test_key("b");
        let key_c = test_key("c");

        {
            let mut cache = Cache::open(&root, 250).unwrap();
            cache.put(&key_a, &payload).unwrap();
            cache.put(&key_b, &payload).unwrap();
            // Touch A so B is LRU going into the restart.
            assert_eq!(cache.get(&key_a).unwrap().unwrap(), payload);
            assert_eq!(cache.total_bytes(), 200);
        } // `cache` dropped here -- simulates process restart.

        let mut cache = Cache::open(&root, 250).unwrap();

        // 1. Byte total survives -- checked before touching anything, so
        // this is purely a property of what was persisted at close time.
        assert_eq!(cache.total_bytes(), 200);

        // 2. Access ordering survives: NOTHING is read here before this
        // eviction. If ordering were not persisted (e.g. the pre-restart
        // touch of A only updated the in-memory counter and was never
        // written to disk), a freshly reloaded index would have A and B
        // tied/at whatever order the loader defaults to, and this `put`
        // could evict A instead of B. Only a genuinely persisted counter
        // makes B -- untouched since before the restart -- the one that
        // goes here.
        cache.put(&key_c, &payload).unwrap();

        assert!(
            !cache.contains(&key_b),
            "B was LRU before the restart and must still be the one evicted"
        );
        assert!(cache.contains(&key_a));
        assert!(cache.contains(&key_c));

        // 3. The value survives -- read only now, after the eviction
        // proof above, so this touch cannot contaminate it.
        assert_eq!(cache.get(&key_a).unwrap().unwrap(), payload);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn recovers_when_index_file_is_missing() {
        let root = unique_dir("recover-missing");
        {
            let mut cache = Cache::open(&root, 1_000_000).unwrap();
            cache.put(&test_key("a"), b"payload-a").unwrap();
            cache.put(&test_key("b"), b"payload-b").unwrap();
        }

        std::fs::remove_file(root.join(INDEX_FILE_NAME)).unwrap();

        let mut cache = Cache::open(&root, 1_000_000).unwrap();
        assert_eq!(cache.get(&test_key("a")).unwrap().unwrap(), b"payload-a");
        assert_eq!(cache.get(&test_key("b")).unwrap().unwrap(), b"payload-b");
        assert_eq!(
            cache.total_bytes(),
            (b"payload-a".len() + b"payload-b".len()) as u64
        );

        // Recovery healed the index file back into a valid, loadable state.
        let healed_bytes = std::fs::read(root.join(INDEX_FILE_NAME)).unwrap();
        let (healed_index, _): (Index, usize) =
            bincode::decode_from_slice(&healed_bytes, bincode::config::standard()).unwrap();
        assert_eq!(healed_index.entries.len(), 2);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn recovers_when_index_file_is_corrupt() {
        let root = unique_dir("recover-corrupt");
        {
            let mut cache = Cache::open(&root, 1_000_000).unwrap();
            cache.put(&test_key("a"), b"payload-a").unwrap();
            cache.put(&test_key("b"), b"payload-b").unwrap();
        }

        std::fs::write(root.join(INDEX_FILE_NAME), b"not a valid bincode index").unwrap();

        let mut cache = Cache::open(&root, 1_000_000).unwrap();
        assert_eq!(cache.get(&test_key("a")).unwrap().unwrap(), b"payload-a");
        assert_eq!(cache.get(&test_key("b")).unwrap().unwrap(), b"payload-b");
        assert_eq!(
            cache.total_bytes(),
            (b"payload-a".len() + b"payload-b".len()) as u64
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Containment vacuity trap: since every entry filename is a hash,
    /// `../..` inside key material is structurally inexpressible (see
    /// `key.rs`'s `traversal_shaped_producer_hashes_like_ordinary_text`),
    /// so feeding a traversal string into a key would pass trivially and
    /// prove nothing. The real vector is a symlink planted directly at an
    /// entry's on-disk path, pointing outside the cache root -- this
    /// plants exactly that and proves `put` refuses to follow it.
    #[test]
    fn refuses_symlink_planted_inside_cache_root_pointing_outside() {
        let root = unique_dir("containment-inside");
        let outside = unique_dir("containment-outside");
        let outside_file = outside.join("secret.bin");
        std::fs::write(&outside_file, b"must never be overwritten").unwrap();

        let key = test_key("evil");
        let entry_path = root
            .join(ENTRIES_DIR_NAME)
            .join(format!("{}.bin", key.content_hash()));

        // Cache::open creates root/entries/ first; plant the symlink there
        // before ever calling `put`.
        {
            let _cache = Cache::open(&root, 1_000_000).unwrap();
        }
        symlink(&outside_file, &entry_path).unwrap();

        let mut cache = Cache::open(&root, 1_000_000).unwrap();
        let err = cache.put(&key, b"payload").unwrap_err();
        assert!(matches!(err, CacheError::Containment { .. }));

        // The outside file was never touched.
        assert_eq!(
            std::fs::read(&outside_file).unwrap(),
            b"must never be overwritten"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// The other half of NFR-010 containment: the cache ROOT itself being
    /// a symlink must NOT be spuriously rejected (W2-08 lets a user point
    /// the cache root anywhere, including at a symlink). This is also the
    /// macOS `/var` -> `/private/var` trap: a naive implementation that
    /// compares a canonicalized child against an un-canonicalized root
    /// would fail this even though nothing is actually unsafe.
    #[test]
    fn allows_symlinked_cache_root() {
        let real_root = unique_dir("containment-real-root");
        let link_root = unique_missing_path("containment-link-root");
        symlink(&real_root, &link_root).unwrap();

        let mut cache = Cache::open(&link_root, 1_000_000).unwrap();
        let key = test_key("through-symlink");
        cache.put(&key, b"hello").unwrap();
        assert_eq!(cache.get(&key).unwrap().unwrap(), b"hello");

        // The entry actually lives under the REAL directory, proving the
        // root was resolved rather than blindly trusted as written.
        let real_entry_path = real_root
            .join(ENTRIES_DIR_NAME)
            .join(format!("{}.bin", key.content_hash()));
        assert!(real_entry_path.exists());

        std::fs::remove_file(&link_root).ok();
        std::fs::remove_dir_all(&real_root).ok();
    }

    #[test]
    fn eviction_never_exceeds_cap_after_multiple_puts() {
        let root = unique_dir("cap-bound");
        let mut cache = Cache::open(&root, 150).unwrap();
        let payload = vec![0u8; 100];

        for i in 0..5 {
            cache.put(&test_key(&i.to_string()), &payload).unwrap();
            assert!(cache.total_bytes() <= cache.cap_bytes());
        }

        std::fs::remove_dir_all(&root).ok();
    }
}
