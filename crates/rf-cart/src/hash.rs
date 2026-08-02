//! Normalized ROM hashing, RetroAchievements/No-Intro compatible
//! (FR-CORE-011, `docs/research/game-identity-and-re-data.md`).
//!
//! Per-console normalization strips the packaging that varies between
//! otherwise-identical dumps (an iNES header, a copier header) *before*
//! hashing, so the same game hashes identically regardless of how it was
//! dumped. Profiles key on the normalized SHA-256; MD5 and CRC32 are kept
//! alongside for cross-referencing RetroAchievements and No-Intro
//! databases. The raw-file hash is kept too, but only for diagnostics —
//! never for identity.

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// One CRC32/MD5/SHA-1/SHA-256 bundle, hex-encoded lowercase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomHashes {
    /// No-Intro-compatible, 8 lowercase hex chars.
    pub crc32: String,
    /// RetroAchievements-compatible, 32 lowercase hex chars.
    pub md5: String,
    /// 40 lowercase hex chars.
    pub sha1: String,
    /// Profile primary key, 64 lowercase hex chars.
    pub sha256: String,
}

/// Raw + normalized hash bundle for a loaded ROM image (FR-CORE-011).
///
/// `normalized` is computed over the header-stripped image and is what
/// profiles key on. `raw` is the hash of the file exactly as read from
/// disk (before any stripping), kept for diagnostics only — two
/// differently-packaged dumps of the same game share `normalized` but not
/// `raw`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomIdentity {
    /// Hashes of the header-stripped image. Use this for profile matching.
    pub normalized: RomHashes,
    /// Hashes of the file exactly as read from disk. Diagnostics only.
    pub raw: RomHashes,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Compute CRC32/MD5/SHA-1/SHA-256 over `data`, hex-encoded lowercase.
///
/// RustCrypto 0.11's `Digest::finalize()` returns a fixed-size `Array<u8,
/// N>`, which does not implement `LowerHex` (unlike 0.10) — hex encoding
/// goes through the `hex()` helper above instead of `format!("{:x}", ...)`.
pub fn hash_all(data: &[u8]) -> RomHashes {
    let mut sha256 = Sha256::new();
    sha256.update(data);
    let sha256_digest = sha256.finalize();

    let mut crc = crc32fast::Hasher::new();
    crc.update(data);
    let crc32_value: u32 = crc.finalize();

    RomHashes {
        crc32: format!("{crc32_value:08x}"),
        md5: hex(&Md5::digest(data)),
        sha1: hex(&Sha1::digest(data)),
        sha256: hex(&sha256_digest),
    }
}

/// NES normalization (RA convention): if the file starts with the iNES/
/// NES 2.0 magic `"NES\x1A"`, skip the first 16 bytes and hash the rest;
/// otherwise hash the whole file. This intentionally does not also skip a
/// present trainer — the RA convention normalizes only the fixed header.
pub fn normalize_nes(data: &[u8]) -> &[u8] {
    if data.len() >= 4 && data[0..4] == crate::nes::INES_MAGIC {
        &data[16.min(data.len())..]
    } else {
        data
    }
}

/// SNES normalization (RA convention): if `filesize % 8192 == 512`, skip
/// the first 512 bytes (copier header) and hash the rest; otherwise hash
/// the whole file.
pub fn normalize_snes(data: &[u8]) -> &[u8] {
    if data.len() % 8192 == 512 {
        &data[512..]
    } else {
        data
    }
}

/// Raw + normalized hash bundle for an NES ROM image.
pub fn identity_nes(raw: &[u8]) -> RomIdentity {
    RomIdentity {
        normalized: hash_all(normalize_nes(raw)),
        raw: hash_all(raw),
    }
}

/// Raw + normalized hash bundle for an SNES ROM image.
pub fn identity_snes(raw: &[u8]) -> RomIdentity {
    RomIdentity {
        normalized: hash_all(normalize_snes(raw)),
        raw: hash_all(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-CORE-011: known-answer vectors for b"hello", verified with
    /// `shasum -a 256|1`, `md5`, and a reference CRC32 implementation —
    /// proves the hashing layer (including the 0.11 hex-encoding shim) is
    /// wired correctly, independent of any cartridge parsing.
    #[test]
    fn known_vectors_for_hello() {
        let h = hash_all(b"hello");
        assert_eq!(
            h.sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(h.sha1, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
        assert_eq!(h.md5, "5d41402abc4b2a76b9719d911017c592");
        assert_eq!(h.crc32, "3610a686");
    }

    #[test]
    fn normalize_nes_strips_16_byte_header_when_magic_present() {
        let mut data = b"NES\x1a".to_vec();
        data.extend_from_slice(&[0u8; 12]); // rest of 16-byte header
        data.extend_from_slice(b"payload");
        assert_eq!(normalize_nes(&data), b"payload");
    }

    #[test]
    fn normalize_nes_hashes_whole_file_without_magic() {
        let data = b"not an ines file at all".to_vec();
        assert_eq!(normalize_nes(&data), data.as_slice());
    }

    #[test]
    fn normalize_snes_strips_512_byte_copier_header() {
        let mut data = vec![0xAAu8; 512];
        data.extend_from_slice(&[0xBBu8; 8192]); // total len % 8192 == 512
        assert_eq!(normalize_snes(&data), &[0xBBu8; 8192][..]);
    }

    #[test]
    fn normalize_snes_no_strip_when_size_does_not_match() {
        let data = vec![0xCCu8; 8192]; // exact multiple, no copier header
        assert_eq!(normalize_snes(&data), data.as_slice());
    }

    #[test]
    fn identity_nes_normalized_differs_from_raw_and_matches_stripped_hash() {
        let mut data = b"NES\x1a".to_vec();
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(b"payload-bytes");
        let identity = identity_nes(&data);
        assert_eq!(identity.raw, hash_all(&data));
        assert_eq!(identity.normalized, hash_all(b"payload-bytes"));
        assert_ne!(identity.raw.sha256, identity.normalized.sha256);
    }

    #[test]
    fn identity_snes_normalized_differs_from_raw_when_copier_header_present() {
        let mut data = vec![0x11u8; 512];
        data.extend_from_slice(&[0x22u8; 8192]);
        let identity = identity_snes(&data);
        assert_eq!(identity.raw, hash_all(&data));
        assert_eq!(identity.normalized, hash_all(&[0x22u8; 8192]));
        assert_ne!(identity.raw.sha256, identity.normalized.sha256);
    }
}
