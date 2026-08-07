//! On-disk entry format: `[bincode-encoded CacheKey header][raw payload
//! bytes]`, written atomically (temp file + rename, see `fsutil.rs`).
//!
//! The header carries this entry's own key tuple so the index
//! (`index.rs`) is a rebuildable *cache* of information every entry file
//! already carries, not its source of truth: if `index.bin` is lost or
//! corrupted, every entry can still name itself.
//!
//! `bincode = "2"`: `Encode`/`Decode` derives, `encode_to_vec` /
//! `decode_from_slice` with `bincode::config::standard()` -- the 1.x
//! `serialize`/`deserialize` free functions do not exist on this major
//! (R-11). Same helpers `crates/rf-state/src/payload.rs` uses for chunk
//! payloads.

use std::path::Path;

use crate::error::CacheError;
use crate::fsutil::{atomic_write, ensure_contained};
use crate::key::CacheKey;

/// A fully decoded entry file: its key header plus payload bytes.
#[derive(Debug)]
pub(crate) struct Entry {
    pub key: CacheKey,
    pub payload: Vec<u8>,
}

/// Encode `key` as the header, append `payload`, and write the result to
/// `path` atomically. Containment (NFR-010) is checked here, not left to
/// the caller, so every write path -- including any future one -- gets it
/// for free.
pub(crate) fn write_entry(
    root: &Path,
    path: &Path,
    key: &CacheKey,
    payload: &[u8],
) -> Result<(), CacheError> {
    ensure_contained(root, path)?;

    let mut bytes = bincode::encode_to_vec(key, bincode::config::standard())
        .map_err(|e| CacheError::Encode(e.to_string()))?;
    bytes.extend_from_slice(payload);
    atomic_write(path, &bytes)
}

/// Read and decode the entry file at `path`. Containment (NFR-010) is
/// checked here, not left to the caller -- this is also what protects the
/// index-rebuild directory scan (`index.rs`) from following a symlink
/// planted directly in the entries directory.
pub(crate) fn read_entry(root: &Path, path: &Path) -> Result<Entry, CacheError> {
    ensure_contained(root, path)?;

    let bytes = std::fs::read(path).map_err(|source| CacheError::Io {
        context: "read entry file",
        message: source.to_string(),
    })?;
    let (key, consumed): (CacheKey, usize) =
        bincode::decode_from_slice(&bytes, bincode::config::standard())
            .map_err(|e| CacheError::Decode(e.to_string()))?;
    let payload = bytes[consumed..].to_vec();
    Ok(Entry { key, payload })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> CacheKey {
        CacheKey {
            rom_sha256: "rom".to_string(),
            asset_hash: "asset".to_string(),
            producer: "producer".to_string(),
            settings_hash: "settings".to_string(),
        }
    }

    /// `ensure_contained` requires `root` to already be canonical (see its
    /// doc comment); `Cache::open` normally does that canonicalization,
    /// but these are entry.rs-level tests calling `write_entry`/
    /// `read_entry` directly, so the helper does it here instead. Skipping
    /// this would fail these tests on macOS, where `std::env::temp_dir()`
    /// sits under `/var` -> `/private/var` -- a real trap, not a
    /// theoretical one (hit while writing this test).
    fn unique_dir(label: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rf-cache-entry-test-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn write_then_read_round_trips_key_and_payload() {
        let dir = unique_dir("roundtrip");
        let path = dir.join("entry.bin");
        let payload = b"hello cache".to_vec();

        write_entry(&dir, &path, &key(), &payload).unwrap();
        let entry = read_entry(&dir, &path).unwrap();

        assert_eq!(entry.key, key());
        assert_eq!(entry.payload, payload);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_entry_rejects_garbage_without_panicking() {
        let dir = unique_dir("garbage");
        let path = dir.join("garbage.bin");
        std::fs::write(&path, [0xFFu8; 8]).unwrap();

        let err = read_entry(&dir, &path).unwrap_err();
        assert!(matches!(err, CacheError::Decode(_)));

        std::fs::remove_dir_all(&dir).ok();
    }
}
