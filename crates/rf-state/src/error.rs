//! Container errors and non-fatal load warnings.
//!
//! `thiserror` is not currently a workspace dependency; per the established
//! pattern in `rf-cart` (see `crates/rf-cart/src/error.rs`), this enum is
//! hand-rolled (`Display` + `std::error::Error`) rather than adding a new
//! dependency for one small crate.
//!
//! Every variant here corresponds to "fail with a diagnostic, never panic
//! on untrusted input": a `.rfstate` file is untrusted data (it may be
//! hand-edited, truncated, corrupted in transit, or crafted by a hostile
//! actor), so parsing code must return one of these instead of indexing
//! out of bounds, unwrapping, or allocating an attacker-declared size.

use std::fmt;

/// Render a 4-byte chunk tag as text for error messages ("naming the
/// chunk" per FR-STATE-004) without requiring the bytes to be valid UTF-8
/// (a corrupt/hostile file may not honor the ASCII-tag convention).
fn tag_str(tag: [u8; 4]) -> String {
    String::from_utf8_lossy(&tag).into_owned()
}

/// Everything that can go wrong building or parsing a `.rfstate` container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerError {
    /// The first 4 bytes were not `"RFST"`.
    BadMagic {
        /// The bytes actually found.
        found: [u8; 4],
    },
    /// `container_version` is not one this build knows how to read.
    UnsupportedContainerVersion {
        /// Version found in the stream.
        found: u16,
        /// The version this build supports.
        supported: u16,
    },
    /// `flags` was nonzero. No bits are defined in container v1; a byte
    /// this build does not understand set in `flags` means the file may
    /// depend on semantics we do not implement, so it is refused rather
    /// than silently ignored (relaxing this later is backward-compatible;
    /// tightening it later would not be).
    NonZeroFlags {
        /// The flags value actually found.
        found: u16,
    },
    /// A fixed-size field ran past the end of the input.
    Truncated {
        /// What we were trying to read when the shortfall was found.
        context: &'static str,
        /// Minimum byte count required.
        needed: usize,
        /// Actual byte count available.
        got: usize,
    },
    /// A length-prefixed header field declared a length larger than this
    /// format allows (independent of how many bytes are actually present).
    OversizedLength {
        /// What field this was.
        context: &'static str,
        /// The declared length.
        declared: usize,
        /// The maximum allowed.
        max: usize,
    },
    /// A field required to be UTF-8 was not.
    InvalidUtf8 {
        /// What field this was.
        context: &'static str,
    },
    /// The zstd stream was malformed (bad frame, bad checksum, ...).
    Decompression(String),
    /// The zstd stream decompressed past the safety cap without ending.
    /// Guards against a decompression bomb: a small compressed input
    /// claiming an unbounded decompressed size.
    DecompressedTooLarge {
        /// The cap that was exceeded.
        max: usize,
    },
    /// A chunk's declared `len` exceeds the bytes actually remaining in
    /// the decompressed body — rejected before any payload allocation.
    ChunkTruncated {
        /// The chunk whose length field was bad.
        tag: [u8; 4],
        /// Declared payload length.
        needed: usize,
        /// Bytes actually remaining.
        got: usize,
    },
    /// A payload was too large to encode (its length does not fit the
    /// wire format's `u32` length field).
    PayloadTooLarge {
        /// The chunk in question.
        tag: [u8; 4],
        /// The payload length that did not fit.
        len: usize,
    },
    /// A required (core-set) chunk was absent (FR-STATE-004).
    MissingRequiredChunk {
        /// The missing chunk's tag.
        tag: [u8; 4],
    },
    /// A chunk's `version` did not match the version this build expects,
    /// and no migration function was registered for it (FR-STATE-004).
    CannotMigrate {
        /// The chunk whose version could not be reconciled.
        tag: [u8; 4],
        /// Version found in the stream.
        found: u16,
        /// Version this build expects.
        expected: u16,
    },
    /// A registered migration function itself failed.
    MigrationFailed {
        /// The chunk being migrated.
        tag: [u8; 4],
        /// The migration function's own error message.
        message: String,
    },
    /// `rom_sha256` did not match the ROM the caller is trying to load
    /// this state against.
    RomMismatch {
        /// Hash the caller expected (their currently-loaded ROM).
        expected: [u8; 32],
        /// Hash actually stored in the state.
        found: [u8; 32],
    },
    /// A bincode payload failed to encode.
    Encode(String),
    /// A bincode payload failed to decode.
    Decode(String),
}

impl fmt::Display for ContainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContainerError::BadMagic { found } => {
                write!(f, "bad .rfstate magic: found {found:02x?}, expected \"RFST\"")
            }
            ContainerError::UnsupportedContainerVersion { found, supported } => {
                write!(
                    f,
                    "unsupported .rfstate container_version {found} (this build supports {supported})"
                )
            }
            ContainerError::NonZeroFlags { found } => {
                write!(f, "unsupported .rfstate flags 0x{found:04x} (no bits defined in v1)")
            }
            ContainerError::Truncated {
                context,
                needed,
                got,
            } => write!(
                f,
                "truncated .rfstate ({context}): needed at least {needed} bytes, got {got}"
            ),
            ContainerError::OversizedLength {
                context,
                declared,
                max,
            } => write!(
                f,
                "oversized {context} length: declared {declared} bytes, max allowed {max}"
            ),
            ContainerError::InvalidUtf8 { context } => {
                write!(f, "{context} is not valid UTF-8")
            }
            ContainerError::Decompression(msg) => write!(f, "zstd decompression failed: {msg}"),
            ContainerError::DecompressedTooLarge { max } => {
                write!(f, "decompressed .rfstate body exceeds safety cap of {max} bytes")
            }
            ContainerError::ChunkTruncated { tag, needed, got } => write!(
                f,
                "truncated chunk '{}': declared payload of {needed} bytes, only {got} remain",
                tag_str(*tag)
            ),
            ContainerError::PayloadTooLarge { tag, len } => write!(
                f,
                "chunk '{}' payload of {len} bytes does not fit the u32 length field",
                tag_str(*tag)
            ),
            ContainerError::MissingRequiredChunk { tag } => {
                write!(f, "missing required chunk '{}'", tag_str(*tag))
            }
            ContainerError::CannotMigrate {
                tag,
                found,
                expected,
            } => write!(
                f,
                "chunk '{}' is at version {found}, expected {expected}, and no migration is registered",
                tag_str(*tag)
            ),
            ContainerError::MigrationFailed { tag, message } => {
                write!(f, "migration for chunk '{}' failed: {message}", tag_str(*tag))
            }
            ContainerError::RomMismatch { expected, found } => write!(
                f,
                "state ROM hash mismatch: expected {}, found {}",
                hex32(expected),
                hex32(found)
            ),
            ContainerError::Encode(msg) => write!(f, "payload encode failed: {msg}"),
            ContainerError::Decode(msg) => write!(f, "payload decode failed: {msg}"),
        }
    }
}

impl std::error::Error for ContainerError {}

fn hex32(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A non-fatal condition surfaced while loading a container (FR-STATE-004:
/// unknown optional chunks are skipped "with a warning" — returned here as
/// data rather than a log line, since this crate depends on no logging
/// facade and a returned `Vec` is directly assertable in tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadWarning {
    /// A chunk tag not in this build's registry was present. Its bytes
    /// are retained in the decoded [`crate::Container`] (skipping means
    /// "not interpreted", not "discarded") for forward compatibility.
    UnknownChunk {
        /// The unrecognized tag.
        tag: [u8; 4],
    },
    /// A known chunk was found at an older version and successfully
    /// migrated to the version this build expects.
    Migrated {
        /// The migrated chunk's tag.
        tag: [u8; 4],
        /// Version found in the stream.
        from: u16,
        /// Version migrated to.
        to: u16,
    },
}
