//! Fact plugins: adding to and refining a recipe without changing play
//! (ticket W39-00; `docs/design/REBUILD_LAB.md` section 9, "fact plugins
//! (names, RAM maps, tile semantics) change nothing at run time").
//!
//! A fact plugin ships a recipe-format file of rows. [`merge_facts`] folds it
//! into the base recipe under three rules that keep it read-only in effect:
//!
//! 1. **No patches.** A `[[patch]]` row edits the game's code, which is not a
//!    fact; a file that carries one is refused whole ([`FactRefusal`]).
//! 2. **A plugin cannot vouch.** Every row that comes in is clamped to
//!    [`Grade::Candidate`]. Confirmed rows are what patches and write hooks
//!    trust, and only the lockstep checker or a person confirms.
//! 3. **No demotion.** A row the recipe already holds at `Observed` or
//!    `Confirmed` is kept; a conflicting incoming row is reported in
//!    [`FactMerge::conflicts`], never applied. Only a `Candidate` row can be
//!    refined (replaced) by a differing incoming row.
//!
//! Rows are matched by their key: function by `name`, RAM by `addr`, object
//! table and data region by `name`, tile range by `(class, first, last)`.

use crate::recipe::{Grade, Recipe};

/// Why a fact file was refused whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactRefusal {
    /// The file carries `[[patch]]` rows: code edits are not facts.
    CarriesPatches { count: usize },
}

impl std::fmt::Display for FactRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CarriesPatches { count } => write!(
                f,
                "fact plugin carries {count} [[patch]] row(s) — patches change the game and \
                 belong in a recipe mod, not a fact plugin"
            ),
        }
    }
}

impl std::error::Error for FactRefusal {}

/// What a merge did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FactMerge {
    pub recipe: Recipe,
    /// Rows with a key the recipe did not have.
    pub added: usize,
    /// Candidate rows replaced by a differing incoming row.
    pub refined: usize,
    /// Incoming rows identical to what the recipe held.
    pub unchanged: usize,
    /// `"<table> <key>"` for each incoming row that disagreed with a trusted
    /// (Observed or Confirmed) row and was dropped.
    pub conflicts: Vec<String>,
}

/// Fold `facts` into `base`. See the module doc for the rules.
///
/// # Errors
/// [`FactRefusal::CarriesPatches`] when `facts` has patch rows.
pub fn merge_facts(base: &Recipe, facts: &Recipe) -> Result<FactMerge, FactRefusal> {
    if !facts.patches.is_empty() {
        return Err(FactRefusal::CarriesPatches {
            count: facts.patches.len(),
        });
    }
    let mut out = FactMerge {
        recipe: base.clone(),
        ..FactMerge::default()
    };
    let r = &mut out.recipe;
    let (mut added, mut refined, mut unchanged) = (0, 0, 0);
    let mut conflicts = Vec::new();
    let mut tally = |t: Tally, label: &str, key: String| match t {
        Tally::Added => added += 1,
        Tally::Refined => refined += 1,
        Tally::Unchanged => unchanged += 1,
        Tally::Conflict => conflicts.push(format!("{label} {key}")),
    };
    for row in &facts.functions {
        let t = merge_row(
            &mut r.functions,
            row,
            |a, b| a.name == b.name,
            |x| x.grade,
            |x, g| x.grade = g,
        );
        tally(t, "function", row.name.clone());
    }
    for row in &facts.ram {
        let t = merge_row(
            &mut r.ram,
            row,
            |a, b| a.addr == b.addr,
            |x| x.grade,
            |x, g| x.grade = g,
        );
        tally(t, "ram", format!("{:#06x}", row.addr));
    }
    for row in &facts.object_tables {
        let t = merge_row(
            &mut r.object_tables,
            row,
            |a, b| a.name == b.name,
            |x| x.grade,
            |x, g| x.grade = g,
        );
        tally(t, "object_table", row.name.clone());
    }
    for row in &facts.tiles {
        let t = merge_row(
            &mut r.tiles,
            row,
            |a, b| a.class == b.class && a.first == b.first && a.last == b.last,
            |x| x.grade,
            |x, g| x.grade = g,
        );
        tally(t, "tile", format!("{:#x}..={:#x}", row.first, row.last));
    }
    for row in &facts.data_regions {
        let t = merge_row(
            &mut r.data_regions,
            row,
            |a, b| a.name == b.name,
            |x| x.grade,
            |x, g| x.grade = g,
        );
        tally(t, "data_region", row.name.clone());
    }
    out.added = added;
    out.refined = refined;
    out.unchanged = unchanged;
    out.conflicts = conflicts;
    Ok(out)
}

enum Tally {
    Added,
    Refined,
    Unchanged,
    Conflict,
}

/// One keyed row. The incoming copy is clamped to Candidate first, so an
/// identical row compares equal to a Candidate one already held.
fn merge_row<T: Clone + PartialEq>(
    rows: &mut Vec<T>,
    incoming: &T,
    same_key: impl Fn(&T, &T) -> bool,
    grade: impl Fn(&T) -> Grade,
    set_grade: impl Fn(&mut T, Grade),
) -> Tally {
    let mut row = incoming.clone();
    set_grade(&mut row, Grade::Candidate);
    match rows.iter().position(|have| same_key(have, &row)) {
        None => {
            rows.push(row);
            Tally::Added
        }
        Some(i) => {
            let mut have_as_candidate = rows[i].clone();
            set_grade(&mut have_as_candidate, Grade::Candidate);
            if have_as_candidate == row {
                Tally::Unchanged
            } else if grade(&rows[i]) == Grade::Candidate {
                rows[i] = row;
                Tally::Refined
            } else {
                Tally::Conflict
            }
        }
    }
}

// A new recipe table is a compile error here, not a silent omission from the
// merge above.
const _: fn(&Recipe) = |r| {
    let Recipe {
        id: _,
        engine: _,
        functions: _,
        ram: _,
        patches: _,
        object_tables: _,
        tiles: _,
        data_regions: _,
    } = r;
};
#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{PatchRow, RamMeaning};

    fn ram(addr: u32, name: &str, grade: Grade) -> RamMeaning {
        RamMeaning {
            addr,
            width: 1,
            name: name.into(),
            source: "test".into(),
            grade,
        }
    }

    fn base() -> Recipe {
        Recipe {
            id: "t".into(),
            ram: vec![
                ram(0x10, "lives", Grade::Confirmed),
                ram(0x20, "maybe", Grade::Candidate),
            ],
            ..Recipe::default()
        }
    }

    #[test]
    fn new_rows_are_added_as_candidates_never_confirmed() {
        let facts = Recipe {
            ram: vec![ram(0x30, "score", Grade::Confirmed)],
            ..Recipe::default()
        };
        let m = merge_facts(&base(), &facts).unwrap();
        assert_eq!(m.added, 1);
        let row = m.recipe.ram.iter().find(|r| r.addr == 0x30).unwrap();
        assert_eq!(row.grade, Grade::Candidate, "a plugin cannot vouch");
    }

    #[test]
    fn trusted_rows_are_kept_and_the_conflict_is_reported() {
        let facts = Recipe {
            ram: vec![ram(0x10, "not_lives", Grade::Observed)],
            ..Recipe::default()
        };
        let m = merge_facts(&base(), &facts).unwrap();
        assert_eq!(m.conflicts, vec!["ram 0x0010".to_string()]);
        assert_eq!(m.recipe.ram[0].name, "lives");
        assert_eq!(m.recipe.ram[0].grade, Grade::Confirmed);
    }

    #[test]
    fn a_candidate_row_can_be_refined_and_identical_rows_are_noops() {
        let facts = Recipe {
            ram: vec![
                ram(0x20, "better_name", Grade::Observed),
                ram(0x10, "lives", Grade::Confirmed),
            ],
            ..Recipe::default()
        };
        let m = merge_facts(&base(), &facts).unwrap();
        assert_eq!((m.refined, m.unchanged, m.added), (1, 1, 0));
        assert_eq!(m.recipe.ram[1].name, "better_name");
        assert!(m.conflicts.is_empty());
    }

    #[test]
    fn patch_rows_refuse_the_whole_file() {
        let facts = Recipe {
            patches: vec![PatchRow::default()],
            ram: vec![ram(0x30, "score", Grade::Observed)],
            ..Recipe::default()
        };
        assert_eq!(
            merge_facts(&base(), &facts),
            Err(FactRefusal::CarriesPatches { count: 1 })
        );
    }
}
