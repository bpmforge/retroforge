//! bsnes-hd settings files (`Game.bso`, same base name as the ROM): a short
//! string of letters each followed by an integer, e.g.
//! `w1s1W48S2i0o1p1b1B1c1`. Per the wide-snes documentation `W<n>` is the
//! extra dots per side (48 -> 352 wide, 64 -> 384, 96 -> 448, 112 -> 480),
//! `o` is overscan on/off and `p` is pixel-aspect correction on/off. The
//! other letters are bsnes-hd mode / sprite / window / background options;
//! their meaning is not guessed -- they are kept as raw key/values.
use std::collections::BTreeMap;
use std::fmt;

/// A parsed `.bso`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BsoSettings {
    /// `W<n>`: extra dots per side (so output width is `256 + 2n`).
    pub extra_per_side: Option<u16>,
    /// `o<n>`: overscan on (non-zero) / off.
    pub overscan: Option<bool>,
    /// `p<n>`: pixel-aspect-ratio correction on / off.
    pub par: Option<bool>,
    /// Every letter/value pair, including the three above.
    pub raw: BTreeMap<char, i64>,
}

/// Why a `.bso` string was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BsoError {
    Empty,
    /// Expected a letter at this character index.
    NotALetter(usize),
    /// A letter with no number after it, at this character index.
    MissingNumber(usize),
    /// A number too large to hold.
    NumberTooLarge(usize),
    /// `W` larger than any real width could use.
    ExtraTooLarge,
}

impl fmt::Display for BsoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "the .bso file is empty"),
            Self::NotALetter(i) => write!(f, "the .bso file has an unexpected character at {i}"),
            Self::MissingNumber(i) => write!(f, "the .bso setting at {i} has no number"),
            Self::NumberTooLarge(i) => write!(f, "the .bso number at {i} is too large"),
            Self::ExtraTooLarge => write!(f, "the .bso widescreen width is too large"),
        }
    }
}

impl std::error::Error for BsoError {}

/// Largest `W` accepted (a 2048-dot-wide picture is already absurd).
const MAX_EXTRA: i64 = 1024;

impl BsoSettings {
    /// Parse the settings string (surrounding whitespace ignored).
    ///
    /// # Errors
    /// [`BsoError`] for an empty or malformed string.
    pub fn parse(text: &str) -> Result<Self, BsoError> {
        let chars: Vec<char> = text.trim().chars().collect();
        if chars.is_empty() {
            return Err(BsoError::Empty);
        }
        let mut raw = BTreeMap::new();
        let mut i = 0;
        // Every iteration consumes the letter at `i` (and any digits), so
        // `i` strictly increases or the loop returns an error.
        while i < chars.len() {
            let letter = chars[i];
            if !letter.is_ascii_alphabetic() {
                return Err(BsoError::NotALetter(i));
            }
            i += 1;
            let at = i;
            let neg = chars.get(i) == Some(&'-');
            if neg {
                i += 1;
            }
            let digits_from = i;
            let mut value: i64 = 0;
            while let Some(d) = chars.get(i).and_then(|c| c.to_digit(10)) {
                value = value
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(i64::from(d)))
                    .filter(|&v| v <= i64::from(u32::MAX))
                    .ok_or(BsoError::NumberTooLarge(at))?;
                i += 1;
            }
            if i == digits_from {
                return Err(BsoError::MissingNumber(at));
            }
            raw.insert(letter, if neg { -value } else { value });
        }
        let extra_per_side = match raw.get(&'W') {
            None => None,
            Some(&v) if (0..=MAX_EXTRA).contains(&v) => Some(v as u16),
            Some(_) => return Err(BsoError::ExtraTooLarge),
        };
        Ok(Self {
            extra_per_side,
            overscan: raw.get(&'o').map(|&v| v != 0),
            par: raw.get(&'p').map(|&v| v != 0),
            raw,
        })
    }
}

/// What a session reports about widescreen conventions a patch relies on.
/// W32-03 acts on it; nothing in the PPU reads it yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WidescreenConvention {
    #[default]
    None,
    /// The bsnes-hd convention with this many extra dots per side.
    BsnesHd { extra_per_side: u16 },
}

impl BsoSettings {
    /// The convention this file declares: only a `W` setting does.
    #[must_use]
    pub fn convention(&self) -> WidescreenConvention {
        match self.extra_per_side {
            Some(n) if n > 0 => WidescreenConvention::BsnesHd { extra_per_side: n },
            _ => WidescreenConvention::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_snes_strings_parse() {
        let b = BsoSettings::parse("w1s1W48S2i0o1p1b1B1c1\n").unwrap();
        assert_eq!(b.extra_per_side, Some(48));
        assert_eq!(b.overscan, Some(true));
        assert_eq!(b.par, Some(true));
        assert_eq!(b.raw.get(&'w'), Some(&1));
        assert_eq!(b.raw.get(&'S'), Some(&2));
        assert_eq!(b.raw.get(&'i'), Some(&0));
        assert_eq!(b.raw.len(), 10);
        assert_eq!(
            b.convention(),
            WidescreenConvention::BsnesHd { extra_per_side: 48 }
        );
        for n in [64u16, 96, 112] {
            let b = BsoSettings::parse(&format!("w1s1W{n}S2i0o0p0b1B1c1")).unwrap();
            assert_eq!(b.extra_per_side, Some(n));
            assert_eq!(b.overscan, Some(false));
            assert_eq!(b.par, Some(false));
        }
    }

    #[test]
    fn no_w_means_no_convention_and_garbage_is_refused() {
        let b = BsoSettings::parse("w1s1o1").unwrap();
        assert_eq!(b.extra_per_side, None);
        assert_eq!(b.convention(), WidescreenConvention::None);
        assert_eq!(
            BsoSettings::parse("W0").unwrap().convention(),
            WidescreenConvention::None
        );
        assert_eq!(BsoSettings::parse("  "), Err(BsoError::Empty));
        assert_eq!(BsoSettings::parse("W"), Err(BsoError::MissingNumber(1)));
        assert_eq!(BsoSettings::parse("1W5"), Err(BsoError::NotALetter(0)));
        assert!(BsoSettings::parse("W99999999999999999999").is_err());
        assert_eq!(BsoSettings::parse("W5000"), Err(BsoError::ExtraTooLarge));
        assert!(BsoSettings::parse("W-3").is_err());
    }
}
