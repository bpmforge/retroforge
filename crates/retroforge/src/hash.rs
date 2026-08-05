//! SHA-256 hex helper (ticket W1-07), shared by [`crate::stepper`]'s
//! reachable-state hash and the determinism/replay test suite's
//! framebuffer digest.
//!
//! Copies the 4-line `hex()` pattern from `crates/rf-cart/src/hash.rs:44`
//! rather than reusing `rf_cart::hash::hash_all` (which also computes
//! CRC32/MD5/SHA-1 — wasted work for a per-frame hash, and that function's
//! own doc scopes it to ROM-identity hashing specifically): RustCrypto
//! 0.11's `Digest::finalize()` returns a fixed-size `Array<u8, N>`, which
//! does not implement `LowerHex` (unlike 0.10) — hex encoding goes through
//! this `hex()` helper instead of `format!("{:x}", ..)`, which is a hard
//! `E0277` compile error against 0.11.
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-answer vector, same one `rf-cart`'s own hash test uses —
    /// proves the hex-encoding shim independent of anything state-hash
    /// specific.
    #[test]
    fn known_vector_for_hello() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
