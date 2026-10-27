//! Cheat code parsing for the NES and SNES (ticket W41-01;
//! `docs/design/CHEATS_ACHIEVEMENTS.md` sections 1-2).
//!
//! Turns the text people paste (Game Genie, Pro Action Replay, raw
//! `addr:value`) into the two plain types `rf-core-api` defines: a
//! [`rf_core_api::RomOverride`] (the core answers CPU ROM reads with it, as
//! the real Game Genie did) or a [`rf_core_api::RamWrite`] (the app pokes it
//! each frame through the mod-tier door). Bit layouts and their sources are
//! in the [`nes`] and [`snes`] module docs. No title is ever named here and
//! no code database ships (law 5).
//!
//! Auto-detection: a `:` means a raw code. On the NES, 6 or 8 characters (an
//! optional `-` anywhere is ignored) are a Game Genie code if every
//! character is a Genie letter, else 6 hex digits are Action Replay (`AEAEAE`
//! is the one corner where hex and letters overlap; it reads as Game Genie,
//! write `AAAA:VV` to force the raw form). On the SNES a `-` means Game Genie
//! (`XXXX-XXXX`) and 8 hex digits without one mean Action Replay.
//!
//! Parsing never panics and never indexes by hand.

mod error;
pub mod nes;
pub mod snes;

pub use error::{CheatError, LineError};
pub use nes::encode_nes_game_genie;
pub use snes::encode_snes_game_genie;

use rf_core_api::{RamWrite, RomOverride};

/// Longest accepted code in characters (the longest real form is 18).
const MAX_CODE_CHARS: usize = 64;

/// Target console, which decides how a code is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Console {
    Nes,
    Snes,
}

/// What a code does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheatEffect {
    Rom(RomOverride),
    Ram(RamWrite),
}

/// Which notation the code was written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeFormat {
    NesGameGenie,
    NesActionReplay,
    SnesGameGenie,
    SnesActionReplay,
    Raw,
}

/// A parsed code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cheat {
    pub format: CodeFormat,
    pub effect: CheatEffect,
}

/// Parse one code. Trims, ignores case, detects the format.
///
/// # Errors
/// [`CheatError`] with a human reason; never panics.
pub fn parse(console: Console, code: &str) -> Result<Cheat, CheatError> {
    let s = code.trim();
    if s.is_empty() {
        return Err(CheatError::Empty);
    }
    if s.chars().take(MAX_CODE_CHARS + 1).count() > MAX_CODE_CHARS {
        return Err(CheatError::TooLong {
            max: MAX_CODE_CHARS,
        });
    }
    let up = s.to_ascii_uppercase();
    match console {
        Console::Nes => nes::parse(&up),
        Console::Snes => snes::parse(&up),
    }
}

/// Parse a text file of codes: one per line, `+` joins several codes on a
/// line, blank lines and lines starting with `#` are skipped. Results come
/// back in order; each error carries its 1-based line number.
#[must_use]
pub fn parse_list(console: Console, text: &str) -> Vec<Result<Cheat, LineError>> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .flat_map(|(i, l)| {
            l.split('+').map(move |part| {
                parse(console, part).map_err(|error| LineError { line: i + 1, error })
            })
        })
        .collect()
}

/// Parse exactly `digits` hex digits from `s`.
pub(crate) fn hex_field(field: &'static str, s: &str, digits: usize) -> Result<u32, CheatError> {
    let got = s.chars().count();
    if got != digits {
        return Err(CheatError::BadLength {
            field,
            expected: digits,
            got,
        });
    }
    s.chars().enumerate().try_fold(0u32, |acc, (index, ch)| {
        ch.to_digit(16)
            .map(|d| (acc << 4) | d)
            .ok_or(CheatError::BadChar { field, ch, index })
    })
}

/// Split a raw code `A..A:VV` or `A..A?CC:VV` into (address, compare, value).
pub(crate) fn split_raw(
    code: &str,
    addr_digits: usize,
) -> Result<(u32, Option<u8>, u8), CheatError> {
    let (lhs, rhs) = code.split_once(':').ok_or(CheatError::Malformed {
        reason: "missing ':'",
    })?;
    if rhs.contains(':') {
        return Err(CheatError::Malformed {
            reason: "more than one ':'",
        });
    }
    let value = hex_field("value", rhs, 2)?;
    let (addr_s, compare) = match lhs.split_once('?') {
        Some((a, c)) => (a, Some(hex_field("compare", c, 2)?)),
        None => (lhs, None),
    };
    let addr = hex_field("address", addr_s, addr_digits)?;
    Ok((
        addr,
        compare.map(|c| u8::try_from(c).unwrap_or(0)),
        u8::try_from(value).unwrap_or(0),
    ))
}
