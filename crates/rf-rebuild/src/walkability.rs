//! Tile solidity by observed walking (ticket W39-03b, D-021).
//!
//! The collision-table finder ([`crate::collision`]) needs a routine that reads
//! a tile-class table. A top-down game moves by free position tests, so the
//! finder calls every tile solid (the W38-03 Zelda run). The other way in is to
//! ask the game itself: stand the player next to a cell, push towards it, and
//! see whether he advances. This module is the pure half: it tallies those
//! pushes per tile id and turns the verdicts into [`TileSemantics`] rows. The
//! driving (RAM pokes on a scratch machine) lives in the harness.
//!
//! A tile id is judged only on enough evidence ([`MIN_PROBES`]) and a clear
//! majority ([`MIN_PRECISION_PCT`]); an id that is blocked in some cells and
//! walkable in others (a shared edge tile, an enemy's knock-back) is left out
//! of the rows and listed as ambiguous, never guessed.
//!
//! Only ids and counts are held (law 5): no pixels, no ROM bytes.

use std::collections::BTreeMap;

use crate::recipe::{Grade, TileClass, TileSemantics};

/// Fewest pushes an id needs before it is judged.
pub const MIN_PROBES: u32 = 4;
/// The share (percent) of one outcome an id needs to be judged by it.
pub const MIN_PRECISION_PCT: u32 = 90;

/// What the pushes say about one tile id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Solid,
    Walkable,
    /// Enough pushes, no clear majority.
    Ambiguous,
    /// Fewer than [`MIN_PROBES`] pushes.
    Unseen,
}

/// Blocked and free push counts per tile id.
#[derive(Debug, Clone, Default)]
pub struct WalkTally {
    counts: BTreeMap<u32, (u32, u32)>,
}

impl WalkTally {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One push at a cell showing `id`: `blocked` is whether the player failed
    /// to advance.
    pub fn record(&mut self, id: u32, blocked: bool) {
        let e = self.counts.entry(id).or_default();
        if blocked {
            e.0 = e.0.saturating_add(1);
        } else {
            e.1 = e.1.saturating_add(1);
        }
    }

    /// `(blocked, free)` for `id`.
    #[must_use]
    pub fn counts(&self, id: u32) -> (u32, u32) {
        self.counts.get(&id).copied().unwrap_or_default()
    }

    /// Distinct ids with at least one push.
    #[must_use]
    pub fn ids(&self) -> usize {
        self.counts.len()
    }

    #[must_use]
    pub fn verdict(&self, id: u32) -> Verdict {
        let (b, f) = self.counts(id);
        let n = b + f;
        if n < MIN_PROBES {
            Verdict::Unseen
        } else if b * 100 >= n * MIN_PRECISION_PCT {
            Verdict::Solid
        } else if f * 100 >= n * MIN_PRECISION_PCT {
            Verdict::Walkable
        } else {
            Verdict::Ambiguous
        }
    }

    /// Ids with `verdict`, ascending.
    #[must_use]
    pub fn ids_with(&self, verdict: Verdict) -> Vec<u32> {
        self.counts
            .keys()
            .copied()
            .filter(|&id| self.verdict(id) == verdict)
            .collect()
    }

    /// The solid ids merged into consecutive ranges, as recipe rows. Walkable
    /// ids need no row (an id with no row is open).
    #[must_use]
    pub fn to_rows(&self, experiment: &str, grade: Grade) -> Vec<TileSemantics> {
        let mut rows: Vec<TileSemantics> = Vec::new();
        for id in self.ids_with(Verdict::Solid) {
            match rows.last_mut() {
                Some(r) if r.last + 1 == id => r.last = id,
                _ => rows.push(TileSemantics {
                    class: TileClass::Solid,
                    first: id,
                    last: id,
                    layer: None,
                    tileset: None,
                    source: format!("walk probe W39-03b: {experiment}"),
                    grade,
                }),
            }
        }
        rows
    }
}

/// The rows as the `[[tile]]` tables a recipe file holds
/// (`docs/design/GAME_PROFILES.md`).
#[must_use]
pub fn rows_toml(rows: &[TileSemantics]) -> String {
    let mut out = String::new();
    for r in rows {
        out.push_str(&format!(
            "[[tile]]\nclass = \"{}\"\nfirst = {:#04x}\nlast = {:#04x}\ngrade = \"{}\"\nsource = \"{}\"\n\n",
            r.class.as_str(),
            r.first,
            r.last,
            r.grade.as_str(),
            r.source.replace('"', "'"),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally(rows: &[(u32, u32, u32)]) -> WalkTally {
        let mut t = WalkTally::new();
        for &(id, blocked, free) in rows {
            (0..blocked).for_each(|_| t.record(id, true));
            (0..free).for_each(|_| t.record(id, false));
        }
        t
    }

    #[test]
    fn verdicts_need_evidence_and_a_clear_majority() {
        let t = tally(&[(1, 9, 0), (2, 0, 9), (3, 5, 5), (4, 2, 0), (5, 19, 1)]);
        assert_eq!(t.verdict(1), Verdict::Solid);
        assert_eq!(t.verdict(2), Verdict::Walkable);
        assert_eq!(t.verdict(3), Verdict::Ambiguous);
        assert_eq!(t.verdict(4), Verdict::Unseen);
        assert_eq!(t.verdict(5), Verdict::Solid, "95% is enough");
        assert_eq!(t.verdict(99), Verdict::Unseen);
    }

    #[test]
    fn solid_ids_merge_into_ranges_and_others_stay_out() {
        let t = tally(&[(10, 6, 0), (11, 6, 0), (12, 0, 6), (13, 6, 0), (20, 4, 4)]);
        let rows = t.to_rows("test", Grade::Candidate);
        let spans: Vec<(u32, u32)> = rows.iter().map(|r| (r.first, r.last)).collect();
        assert_eq!(spans, vec![(10, 11), (13, 13)]);
        assert!(rows.iter().all(|r| r.class == TileClass::Solid));
        assert_eq!(t.ids_with(Verdict::Ambiguous), vec![20]);
    }

    #[test]
    fn rows_print_as_recipe_tables() {
        let t = tally(&[(0x24, 5, 0)]);
        let s = rows_toml(&t.to_rows("x", Grade::Candidate));
        assert!(s.contains("[[tile]]\nclass = \"solid\"\nfirst = 0x24\nlast = 0x24"));
        assert!(s.contains("grade = \"candidate\""));
    }
}
