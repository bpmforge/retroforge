//! RAM search by observation, the "cheat finder" (ticket W38-10).
//!
//! Take a snapshot of work RAM, play, take another, and keep the addresses
//! whose value did what you saw: stayed equal, changed, went up, went down,
//! or equals a number you know. Repeat until few remain. A survivor becomes a
//! candidate recipe RAM row whose source is `observed: cheat search`
//! ([`hit_row`]).
//!
//! Pure logic over byte slices: the caller passes the RAM it read (NES
//! `$0000-$07FF`, SNES WRAM as one 128 KiB slice) and the bus address of the
//! slice's first byte. Values are little-endian, 1 to 4 bytes wide. This is a
//! fact-finding tool, not a cheat database: nothing here knows a game.

use rf_rebuild::{Grade, RamMeaning};

use crate::ram_exchange::{RamRow, SOURCE_SEARCH};

/// Widest value the search compares.
pub const MAX_WIDTH: u8 = 4;

/// How a value must compare for an address to stay a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Same as at the previous snapshot.
    Equal,
    /// Different from the previous snapshot.
    Changed,
    /// Greater than at the previous snapshot.
    Increased,
    /// Less than at the previous snapshot.
    Decreased,
    /// Equal to this number now (the previous snapshot is ignored).
    EqualTo(u32),
}

/// Why a search could not start or continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    /// Width outside `1..=4`.
    BadWidth(u8),
    /// The snapshot is shorter than one value.
    TooShort,
    /// A later snapshot differs in length from the first.
    LengthMismatch { expected: usize, got: usize },
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadWidth(w) => write!(f, "width {w} is not 1 to {MAX_WIDTH} bytes"),
            Self::TooShort => write!(f, "the RAM snapshot is shorter than one value"),
            Self::LengthMismatch { expected, got } => {
                write!(
                    f,
                    "snapshot is {got} bytes, the search started with {expected}"
                )
            }
        }
    }
}

impl std::error::Error for SearchError {}

/// One surviving address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Bus address of the first byte.
    pub addr: u32,
    pub width: u8,
    /// Value at the snapshot before the last refinement.
    pub previous: u32,
    /// Value at the latest snapshot.
    pub current: u32,
}

/// A search in progress: the latest snapshot and the offsets still alive.
#[derive(Debug, Clone)]
pub struct CheatSearch {
    base: u32,
    width: u8,
    /// Snapshot before the last refinement (equals `now` until one happens).
    before: Vec<u8>,
    now: Vec<u8>,
    alive: Vec<u32>,
    rounds: u32,
}

fn read_le(ram: &[u8], off: usize, width: u8) -> u32 {
    ram.get(off..off + usize::from(width)).map_or(0, |b| {
        b.iter().rev().fold(0, |acc, &x| (acc << 8) | u32::from(x))
    })
}

impl CheatSearch {
    /// Start a search over `ram` (bus address `base` for its first byte):
    /// every offset where a `width`-byte value fits is a candidate.
    ///
    /// # Errors
    /// [`SearchError::BadWidth`] or [`SearchError::TooShort`].
    pub fn new(base: u32, width: u8, ram: &[u8]) -> Result<Self, SearchError> {
        if !(1..=MAX_WIDTH).contains(&width) {
            return Err(SearchError::BadWidth(width));
        }
        let w = usize::from(width);
        if ram.len() < w {
            return Err(SearchError::TooShort);
        }
        let last = u32::try_from(ram.len() - w).unwrap_or(u32::MAX);
        Ok(Self {
            base,
            width,
            before: ram.to_vec(),
            now: ram.to_vec(),
            alive: (0..=last).collect(),
            rounds: 0,
        })
    }

    /// Take the next snapshot and keep the candidates that satisfy `filter`;
    /// returns how many remain. The new snapshot becomes the one the next
    /// refinement compares against.
    ///
    /// # Errors
    /// [`SearchError::LengthMismatch`] when `ram` is not the size the search
    /// started with (the search is left unchanged).
    pub fn refine(&mut self, ram: &[u8], filter: Filter) -> Result<usize, SearchError> {
        if ram.len() != self.now.len() {
            return Err(SearchError::LengthMismatch {
                expected: self.now.len(),
                got: ram.len(),
            });
        }
        let (old, w) = (&self.now, self.width);
        self.alive.retain(|&o| {
            let off = o as usize;
            let (p, c) = (read_le(old, off, w), read_le(ram, off, w));
            match filter {
                Filter::Equal => c == p,
                Filter::Changed => c != p,
                Filter::Increased => c > p,
                Filter::Decreased => c < p,
                Filter::EqualTo(v) => c == v,
            }
        });
        self.before = std::mem::replace(&mut self.now, ram.to_vec());
        self.rounds += 1;
        Ok(self.alive.len())
    }

    /// Addresses still alive.
    #[must_use]
    pub fn len(&self) -> usize {
        self.alive.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.alive.is_empty()
    }

    /// Refinements applied so far.
    #[must_use]
    pub fn rounds(&self) -> u32 {
        self.rounds
    }

    /// Up to `limit` survivors in address order.
    #[must_use]
    pub fn hits(&self, limit: usize) -> Vec<Hit> {
        self.alive
            .iter()
            .take(limit)
            .map(|&o| Hit {
                addr: self.base.wrapping_add(o),
                width: self.width,
                previous: read_le(&self.before, o as usize, self.width),
                current: read_le(&self.now, o as usize, self.width),
            })
            .collect()
    }
}

/// A hit as a candidate recipe RAM row, source `observed: cheat search`,
/// value = the latest reading. An empty `name` becomes `found_<addr>`.
#[must_use]
pub fn hit_row(hit: &Hit, name: &str) -> RamRow {
    let name = if name.trim().is_empty() {
        format!("found_{:X}", hit.addr)
    } else {
        name.trim().to_string()
    };
    RamRow {
        meaning: RamMeaning {
            addr: hit.addr,
            width: hit.width,
            name,
            source: SOURCE_SEARCH.to_string(),
            grade: Grade::Candidate,
        },
        value: Some(hit.current),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ram(f: impl Fn(usize) -> u8) -> Vec<u8> {
        (0..64).map(f).collect()
    }

    #[test]
    fn filters_narrow_by_observation() {
        let a = ram(|i| i as u8);
        let mut s = CheatSearch::new(0x7E_0000, 1, &a).unwrap();
        assert_eq!(s.len(), 64);
        // Only offsets 5 and 9 change (5 up, 9 down).
        let mut b = a.clone();
        b[5] += 3;
        b[9] -= 1;
        assert_eq!(s.refine(&b, Filter::Changed).unwrap(), 2);
        let hits = s.hits(10);
        assert_eq!(
            hits[0],
            Hit {
                addr: 0x7E_0005,
                width: 1,
                previous: 5,
                current: 8
            }
        );
        assert_eq!(hits[1].addr, 0x7E_0009);
        // Next snapshot: 5 goes up again, 9 stays.
        let mut c = b.clone();
        c[5] += 1;
        let mut up = s.clone();
        assert_eq!(up.refine(&c, Filter::Increased).unwrap(), 1);
        assert_eq!(up.hits(5)[0].addr, 0x7E_0005);
        let mut eq = s.clone();
        assert_eq!(eq.refine(&c, Filter::Equal).unwrap(), 1);
        assert_eq!(eq.hits(5)[0].addr, 0x7E_0009);
        let mut down = s.clone();
        let mut d = b.clone();
        d[9] -= 2;
        d[5] -= 1;
        assert_eq!(down.refine(&d, Filter::Decreased).unwrap(), 2);
        let mut v = s.clone();
        assert_eq!(v.refine(&c, Filter::EqualTo(9)).unwrap(), 1);
        assert_eq!(v.rounds(), 2);
    }

    #[test]
    fn equal_to_works_from_the_first_snapshot() {
        let a = ram(|i| if i == 17 { 99 } else { 0 });
        let mut s = CheatSearch::new(0, 1, &a).unwrap();
        assert_eq!(s.refine(&a, Filter::EqualTo(99)).unwrap(), 1);
        assert_eq!(s.hits(1)[0].addr, 17);
    }

    #[test]
    fn wide_values_are_little_endian_and_unaligned() {
        let mut a = ram(|_| 0);
        a[10] = 0xFF;
        a[11] = 0x00;
        let mut s = CheatSearch::new(0x100, 2, &a).unwrap();
        a[11] = 0x01; // 0x00FF -> 0x01FF
                      // Offset 10 (0x01FF vs 0x00FF) and offset 11 (0x0001 vs 0x0000).
        assert_eq!(s.refine(&a, Filter::Increased).unwrap(), 2);
        let h = s.hits(8);
        assert_eq!(
            (h[0].addr, h[0].previous, h[0].current),
            (0x10A, 0x00FF, 0x01FF)
        );
        assert_eq!(h[1].addr, 0x10B);
    }

    #[test]
    fn errors_leave_the_search_unchanged() {
        assert_eq!(
            CheatSearch::new(0, 0, &[1]).unwrap_err(),
            SearchError::BadWidth(0)
        );
        assert_eq!(
            CheatSearch::new(0, 5, &[1; 9]).unwrap_err(),
            SearchError::BadWidth(5)
        );
        assert_eq!(
            CheatSearch::new(0, 2, &[1]).unwrap_err(),
            SearchError::TooShort
        );
        let mut s = CheatSearch::new(0, 1, &[1, 2, 3]).unwrap();
        assert!(matches!(
            s.refine(&[1, 2], Filter::Equal),
            Err(SearchError::LengthMismatch {
                expected: 3,
                got: 2
            })
        ));
        assert_eq!(s.len(), 3);
        assert_eq!(s.rounds(), 0);
    }

    #[test]
    fn hit_becomes_a_candidate_row_with_search_provenance() {
        let hit = Hit {
            addr: 0x7E_1234,
            width: 2,
            previous: 1,
            current: 300,
        };
        let row = hit_row(&hit, "");
        assert_eq!(row.meaning.name, "found_7E1234");
        assert_eq!(row.meaning.grade, Grade::Candidate);
        assert_eq!(row.meaning.source, "observed: cheat search");
        assert_eq!(row.value, Some(300));
        assert!(row.meaning.is_hub_eligible()); // observed by our own tooling
        assert_eq!(hit_row(&hit, " lives ").meaning.name, "lives");
    }
}
