//! Error types shared by [`crate::EmulatorCore`] methods (FR-CORE-001).
use std::fmt;

/// Failure loading a [`crate::CartImage`] into a core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// The image failed a basic sanity check (empty, truncated, bad magic).
    InvalidImage(String),
    /// The image's mapper/board is recognized but not implemented by this
    /// core.
    UnsupportedMapper(String),
    /// Any other load failure; message is core-defined.
    Other(String),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::InvalidImage(msg) => write!(f, "invalid cartridge image: {msg}"),
            CoreError::UnsupportedMapper(msg) => write!(f, "unsupported mapper: {msg}"),
            CoreError::Other(msg) => write!(f, "core load error: {msg}"),
        }
    }
}

impl std::error::Error for CoreError {}

/// Failure saving/restoring machine state via [`crate::StateWriter`] /
/// [`crate::StateReader`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// The underlying byte sink/source failed (short write, EOF, etc.).
    /// `rf-state` owns the actual container format; this variant just
    /// carries its message across the trait boundary.
    Io(String),
    /// A version tag in the state stream did not match what the core
    /// expects.
    VersionMismatch {
        /// Version the core expects.
        expected: u32,
        /// Version actually found in the stream.
        found: u32,
    },
    /// The stream was structurally well-formed but its contents were not
    /// valid for this core (wrong ROM, corrupt chunk, etc.).
    Corrupt(String),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::Io(msg) => write!(f, "state I/O error: {msg}"),
            StateError::VersionMismatch { expected, found } => {
                write!(
                    f,
                    "state version mismatch: expected {expected}, found {found}"
                )
            }
            StateError::Corrupt(msg) => write!(f, "corrupt state: {msg}"),
        }
    }
}

impl std::error::Error for StateError {}
