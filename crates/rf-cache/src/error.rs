//! Cache errors, hand-rolled (`Display` + `std::error::Error`) per the
//! established local pattern (`crates/rf-cart/src/error.rs`,
//! `crates/rf-state/src/error.rs`) -- `thiserror` is not a workspace
//! dependency and this crate is too small to justify adding it.
//!
//! I/O failures are captured as `String` messages rather than a raw
//! `std::io::Error`, matching `rf-harness::fetch::FetchError::Write` --
//! `std::io::Error` implements neither `Clone` nor `PartialEq`, and
//! preserving those derives on this enum is worth the lost `source()`
//! chaining for a crate this size.

use std::fmt;
use std::path::PathBuf;

/// Everything that can go wrong opening or using a [`crate::Cache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    /// An I/O operation failed; `context` names what was being attempted.
    Io {
        /// What was being attempted when the error occurred.
        context: &'static str,
        /// `source.to_string()` of the underlying `std::io::Error`.
        message: String,
    },
    /// A candidate path resolved outside the cache root, or a path this
    /// crate is about to read/write is itself a symlink (NFR-010).
    Containment {
        /// What was being attempted when the violation was found.
        context: &'static str,
        /// The offending path.
        path: PathBuf,
    },
    /// bincode encode failure -- surfaced rather than panicking, even
    /// though the types this crate encodes should never fail to encode.
    Encode(String),
    /// bincode decode failure: an entry or index file was truncated,
    /// corrupted, or hand-edited.
    Decode(String),
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheError::Io { context, message } => write!(f, "{context}: {message}"),
            CacheError::Containment { context, path } => {
                write!(f, "{context}: unsafe cache path: {}", path.display())
            }
            CacheError::Encode(msg) => write!(f, "cache encode failed: {msg}"),
            CacheError::Decode(msg) => write!(f, "cache decode failed: {msg}"),
        }
    }
}

impl std::error::Error for CacheError {}
