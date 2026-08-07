//! Cache key: the 4-tuple ARCHITECTURE.md §7 keys `rf-cache` on --
//! `(rom_sha256, asset_hash, producer, settings_hash)` -- plus hashing it
//! into a filesystem-safe, fixed-length entry identifier.
//!
//! Entry filenames are the SHA-256 of the *whole* 4-tuple, never of any
//! individual field. `producer` is the free-text component
//! (`docs/design/ENHANCEMENT_RUNTIME.md` §6 calls it the "model" -- an AI
//! enhancer's own identity string), so it is the only field a caller (or
//! an attacker who controls a settings/profile source) could shape into a
//! path-traversal payload (`../../etc/passwd`) if it were used directly as
//! a directory/file name. Hashing the whole tuple into a fixed 64-char
//! lowercase-hex string removes that vector by construction: no
//! separator, no `/`, nothing but `[0-9a-f]` ever reaches the filesystem
//! from key material (NFR-010).
//!
//! The tradeoff is that a filename alone no longer reconstructs its key,
//! so every entry file carries its own key tuple in a header (see
//! `entry.rs`) -- which is also what lets the index be rebuilt from the
//! entries directory alone if `index.bin` is missing or corrupt (see
//! `index.rs`).

use bincode::{Decode, Encode};
use sha2::{Digest, Sha256};

/// The cache's content-addressing key (`ARCHITECTURE.md` §7).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct CacheKey {
    /// Normalized ROM SHA-256, hex (matches `rf_cart`'s
    /// `RomHashes::sha256`).
    pub rom_sha256: String,
    /// Hash of the source asset (tile/sprite/layer/canvas region) this
    /// entry was derived from.
    pub asset_hash: String,
    /// Free-text producer identity: the enhancer/model that produced this
    /// entry (`ENHANCEMENT_RUNTIME.md` §6's "model"), or a fixed name for
    /// a deterministic (non-AI) producer such as the map stitcher.
    pub producer: String,
    /// Hash of the settings/parameters that shaped this entry, so the
    /// same asset run through different settings does not collide.
    pub settings_hash: String,
}

impl CacheKey {
    /// Fixed-length, filesystem-safe identifier for this key: the
    /// lowercase-hex SHA-256 of the 4-tuple, with each field
    /// length-prefixed so distinct tuples never collide via
    /// field-boundary concatenation (e.g. `("ab", "c")` vs `("a", "bc")`).
    ///
    /// This is also the entry's filename stem (`entries/<hash>.bin`) --
    /// see the module doc above for why hashing the whole tuple (rather
    /// than using any field verbatim as a path component) is what makes
    /// path traversal structurally inexpressible.
    #[must_use]
    pub fn content_hash(&self) -> String {
        let mut hasher = Sha256::new();
        for field in [
            &self.rom_sha256,
            &self.asset_hash,
            &self.producer,
            &self.settings_hash,
        ] {
            hasher.update((field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        hex(&hasher.finalize())
    }
}

/// Hex-encode lowercase, manually: RustCrypto 0.11's `Digest::finalize()`
/// returns a fixed-size `Array<u8, N>`, which does not implement
/// `LowerHex` (unlike 0.10) -- the `format!("{:x}", ...)` idiom does not
/// compile. Same shim as `crates/rf-cart/src/hash.rs`'s `hex()` helper.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(rom: &str, asset: &str, producer: &str, settings: &str) -> CacheKey {
        CacheKey {
            rom_sha256: rom.to_string(),
            asset_hash: asset.to_string(),
            producer: producer.to_string(),
            settings_hash: settings.to_string(),
        }
    }

    #[test]
    fn content_hash_is_64_lowercase_hex_chars() {
        let hash = key("rom", "asset", "producer", "settings").content_hash();
        assert_eq!(hash.len(), 64);
        assert!(hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn content_hash_differs_when_any_field_differs() {
        let base = key("rom", "asset", "producer", "settings");
        let variants = [
            key("ROM", "asset", "producer", "settings"),
            key("rom", "ASSET", "producer", "settings"),
            key("rom", "asset", "PRODUCER", "settings"),
            key("rom", "asset", "producer", "SETTINGS"),
        ];
        for v in variants {
            assert_ne!(base.content_hash(), v.content_hash());
        }
    }

    /// Traversal-shaped `producer` strings must not produce anything
    /// resembling a path -- the hash absorbs them like any other bytes.
    #[test]
    fn traversal_shaped_producer_hashes_like_ordinary_text() {
        let evil = key("rom", "asset", "../../../../etc/passwd", "settings");
        let hash = evil.content_hash();
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains('/'));
        assert!(!hash.contains('.'));
    }

    /// Field-boundary concatenation must not collide two distinct tuples.
    #[test]
    fn field_boundary_shifting_does_not_collide() {
        let a = key("ab", "c", "producer", "settings");
        let b = key("a", "bc", "producer", "settings");
        assert_ne!(a.content_hash(), b.content_hash());
    }
}
