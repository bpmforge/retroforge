//! Cartridge-parsing errors.
//!
//! `thiserror` is not currently a workspace dependency; this enum is
//! hand-rolled (`Display` + `std::error::Error`) per the ticket note that
//! doing so is preferable to adding a new dependency for one small crate.
//!
//! Every variant here corresponds to "fail with a diagnostic, never panic
//! on untrusted input" (FR-CORE-013): parsing code must return one of these
//! instead of indexing out of bounds, unwrapping, or panicking on a
//! malformed/hostile ROM image.

use std::fmt;

/// Everything that can go wrong parsing or identifying a cartridge image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CartError {
    /// The header is structurally invalid (bad magic, no plausible SNES
    /// header found, an unrepresentable size field, ...).
    InvalidHeader(String),
    /// The file is shorter than the header declares it should be.
    Truncated {
        /// What we were trying to locate/read when the shortfall was found.
        context: &'static str,
        /// Minimum byte count required.
        needed: usize,
        /// Actual byte count available.
        got: usize,
    },
    /// A well-formed NES mapper number that rf-nes does not implement yet.
    UnsupportedMapper {
        /// The raw mapper number read from the header.
        id: u16,
        /// A common name for the mapper, when we happen to know it.
        name: Option<&'static str>,
    },
    /// A well-formed SNES coprocessor / map-mode extension that rf-snes
    /// does not implement yet (SA-1, Super FX, DSP-*, ExHiROM, ...).
    UnsupportedChip {
        /// Human-readable chip/mode name plus the raw header byte, for
        /// diagnostics.
        name: String,
    },
}

impl fmt::Display for CartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CartError::InvalidHeader(msg) => write!(f, "invalid cartridge header: {msg}"),
            CartError::Truncated {
                context,
                needed,
                got,
            } => {
                write!(
                    f,
                    "truncated ROM data ({context}): needed at least {needed} bytes, got {got}"
                )
            }
            CartError::UnsupportedMapper { id, name: None } => {
                write!(f, "unsupported mapper: {id}")
            }
            CartError::UnsupportedMapper {
                id,
                name: Some(name),
            } => {
                write!(f, "unsupported mapper: {id} ({name})")
            }
            CartError::UnsupportedChip { name } => write!(f, "unsupported chip: {name}"),
        }
    }
}

impl std::error::Error for CartError {}
