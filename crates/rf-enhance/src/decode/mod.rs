//! Level-decoder families (ticket W5-02a; FR-ENH-005,
//! `docs/design/GAME_PROFILES.md` §2).
//!
//! §2: "Decoder families (`decode.kind`) are implemented once in
//! `rf-enhance` and parameterized by data: `metatile_screens`,
//! `room_grid`, `tilemap_direct`, plus `custom` (delegates to a named
//! plugin). New families are added when >= 2 games need the same shape —
//! resist one-off engine code in the runtime."
//!
//! ## Two properties this module is built around
//!
//! **Pure functions of ROM bytes.** Nothing here takes a core, a bus, a
//! `CoreSink` or a frame. A decoder gets a byte slice and a spec and
//! returns data, which is what makes the whole family testable offline
//! with no emulation running — ticket W5-02a's second acceptance
//! criterion, and the reason the level view can be built once at load
//! time rather than re-derived per frame.
//!
//! **Offsets are into the NORMALIZED image.** Every `[[rom_map]]` offset
//! in a profile is relative to the header-stripped ROM (SRS FR-CORE-011;
//! `rf_cart::hash`'s normalization is what profiles are keyed on), so
//! these functions take the same. Handing them a raw iNES file instead
//! shifts everything by 16 bytes and produces a plausible-looking wrong
//! level rather than an error — which is exactly why it is stated here,
//! in the signature's doc, and asserted in the tests.
//!
//! ## Family versioning
//!
//! §2: families are versioned, and "a behavioral change to a family bumps
//! its version; the loader refuses a newer major... profiles never
//! silently re-decode differently under an upgraded emulator." Schema v0
//! has no `decode.family_version` field yet (it is v0.2's), so this is
//! recorded rather than enforced: **`metatile_screens` as implemented
//! here is family version 1.** Any change to how it turns bytes into a
//! grid — including "fixing" the strictness decisions documented in
//! [`metatile_screens`] — is a version bump, not a patch.

pub mod metatile_screens;
/// The second family (ticket W9-08). See the module doc for why it is a
/// different SHAPE rather than a second stream decoder.
pub mod room_grid;

/// Why a decode could not be performed.
///
/// Every variant names the specific thing that was wrong and where.
/// Deliberately not a single `String`: a level view that fails should be
/// able to say "this profile's `metatile_table` runs past the end of the
/// ROM" rather than "decode failed", because the person reading it is
/// usually the person who just wrote the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The profile has no `[decode]` section at all.
    NoDecodeSection,
    /// `decode.kind` names a family this build does not implement.
    UnknownFamily(String),
    /// A required `[decode]` sub-table is missing.
    MissingSpec {
        family: &'static str,
        field: &'static str,
    },
    /// The family needs a `[[rom_map]]` entry with this label and there
    /// is none. See [`metatile_screens`]'s "What the family requires from
    /// `rom_map`" section.
    MissingRomMapEntry {
        family: &'static str,
        label: &'static str,
    },
    /// A table or slice named by the profile lies outside the ROM.
    OutOfRange {
        what: String,
        offset: u64,
        len: u64,
        rom_len: usize,
    },
    /// `decode.screens.order` names an encoding this family does not
    /// implement.
    UnsupportedOrder(String),
    /// The level data violates the format's own invariant.
    MalformedLevel(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDecodeSection => write!(f, "profile has no [decode] section"),
            Self::UnknownFamily(k) => write!(f, "unknown decoder family `{k}`"),
            Self::MissingSpec { family, field } => {
                write!(f, "{family}: [decode].{field} is required but missing")
            }
            Self::MissingRomMapEntry { family, label } => write!(
                f,
                "{family}: no [[rom_map]] entry labelled `{label}` — this family reads the level \
                 through that table, so a profile without it cannot be decoded"
            ),
            Self::OutOfRange {
                what,
                offset,
                len,
                rom_len,
            } => write!(
                f,
                "{what} at offset {offset:#X} length {len} runs past the end of a {rom_len}-byte \
                 ROM (offsets are into the NORMALIZED, header-stripped image)"
            ),
            Self::UnsupportedOrder(o) => write!(f, "unsupported [decode].screens.order `{o}`"),
            Self::MalformedLevel(m) => write!(f, "malformed level data: {m}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Take `len` bytes at `offset`, or report exactly what did not fit.
pub(crate) fn slice_at<'a>(
    rom: &'a [u8],
    offset: u32,
    len: u32,
    what: &str,
) -> Result<&'a [u8], DecodeError> {
    let start = offset as usize;
    let end = start.checked_add(len as usize);
    match end {
        Some(end) if end <= rom.len() => Ok(&rom[start..end]),
        _ => Err(DecodeError::OutOfRange {
            what: what.to_string(),
            offset: u64::from(offset),
            len: u64::from(len),
            rom_len: rom.len(),
        }),
    }
}
