//! Why a cheat code was rejected, in words a person can act on.

use std::fmt;

/// A code could not be parsed. Display gives a human reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheatError {
    /// Nothing but whitespace.
    Empty,
    /// More characters than any supported code has.
    TooLong { max: usize },
    /// A numeric field has the wrong number of hex digits.
    BadLength {
        field: &'static str,
        expected: usize,
        got: usize,
    },
    /// The whole code has a length no supported format uses.
    BadCodeLength { expected: &'static str, got: usize },
    /// A character that is not valid in this field.
    BadChar {
        field: &'static str,
        ch: char,
        index: usize,
    },
    /// The address is outside what this form of code may touch.
    AddressOutOfRange { addr: u32, reason: &'static str },
    /// Structure of a raw code is wrong (separators).
    Malformed { reason: &'static str },
}

impl fmt::Display for CheatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty code"),
            Self::TooLong { max } => write!(f, "code is longer than {max} characters"),
            Self::BadLength {
                field,
                expected,
                got,
            } => write!(f, "{field} needs {expected} hex digits, got {got}"),
            Self::BadCodeLength { expected, got } => {
                write!(
                    f,
                    "unrecognised code length: got {got} characters, expected {expected}"
                )
            }
            Self::BadChar { field, ch, index } => write!(
                f,
                "invalid character {ch:?} at position {} in {field}",
                index + 1
            ),
            Self::AddressOutOfRange { addr, reason } => {
                write!(f, "address ${addr:X} rejected: {reason}")
            }
            Self::Malformed { reason } => write!(f, "malformed code: {reason}"),
        }
    }
}

impl std::error::Error for CheatError {}

/// A [`CheatError`] tagged with the 1-based line it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
    /// 1-based line number in the text given to `parse_list`.
    pub line: usize,
    /// What was wrong.
    pub error: CheatError,
}

impl fmt::Display for LineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.error)
    }
}

impl std::error::Error for LineError {}
