//! Per-game recipes: facts about a game's functions and RAM, matched against
//! a code atlas (ticket W36-05; `docs/design/REBUILD_LAB.md` section 3).
//!
//! A recipe is **data, never code and never bytes** (law 5, D-021): for each
//! function a name, the shape hash and (optionally) the exact hash found by
//! observation, an address hint, which of entry/exit a hook may attach to,
//! and the meaning of RAM bytes with a note on where the fact came from. This
//! crate holds the plain types and the matcher; `rf-profiles` parses the
//! TOML and the shell pairs a recipe with the atlas it installed.
//!
//! ## Matching
//!
//! By [`crate::Function::shape_hash`], which ignores absolute-address
//! operands, so a ROM hack or a rebuilt copy that moved the function still
//! matches. When the row also carries an exact hash, candidates with an equal
//! [`crate::Function::byte_hash`] are preferred and the match is
//! [`Confidence::Exact`]. Several candidates left: the one whose entry
//! address is nearest the row's address hint wins (no hint: the lowest
//! entry). No candidate: the row is **unmatched**, the game keeps running on
//! the generic rebuild and the interpreter, and the row's hooks are inactive.

use crate::atlas::Atlas;

/// Which side of a function a hook may attach to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HookPoints {
    pub entry: bool,
    pub exit: bool,
}

/// One function the recipe knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeFunction {
    /// The name hooks register by (`rf.on_enter("nmi", ...)`).
    pub name: String,
    /// Required: [`crate::Function::shape_hash`] of the function.
    pub shape_hash: u64,
    /// Optional: [`crate::Function::byte_hash`], raises confidence.
    pub exact_hash: Option<u64>,
    /// Optional: CPU address of the entry in the dump the row was observed
    /// on; breaks ties between equal shapes.
    pub address_hint: Option<u32>,
    pub hooks: HookPoints,
    /// Where the row came from (observation, never a disassembly).
    pub source: String,
}

/// What a RAM byte (or run) means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamMeaning {
    pub addr: u32,
    /// Width in bytes.
    pub width: u8,
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recipe {
    pub id: String,
    /// The game engine family this recipe describes (a label, not logic).
    pub engine: String,
    pub functions: Vec<RecipeFunction>,
    pub ram: Vec<RamMeaning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Shape and exact hash both agree.
    Exact,
    /// Shape agrees; the exact hash was absent or differs (the function was
    /// moved or re-linked).
    Shape,
}

/// A recipe row paired with an atlas function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub name: String,
    /// Index into [`Atlas::functions`].
    pub function: u32,
    pub confidence: Confidence,
    /// How many atlas functions had this shape (1 = unambiguous).
    pub candidates: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatchReport {
    pub matched: Vec<Match>,
    /// Names of rows no atlas function matched.
    pub unmatched: Vec<String>,
}

impl MatchReport {
    /// The atlas function a recipe name resolved to.
    #[must_use]
    pub fn function_of(&self, name: &str) -> Option<u32> {
        self.matched
            .iter()
            .find(|m| m.name == name)
            .map(|m| m.function)
    }
}

/// Pair every row of `recipe` with at most one function of `atlas`.
#[must_use]
pub fn match_recipe(atlas: &Atlas, recipe: &Recipe) -> MatchReport {
    let mut report = MatchReport::default();
    for row in &recipe.functions {
        let by_shape: Vec<(u32, &crate::atlas::Function)> = atlas
            .functions
            .iter()
            .enumerate()
            .filter(|(_, f)| f.shape_hash == row.shape_hash)
            .map(|(i, f)| (i as u32, f))
            .collect();
        let exact: Vec<(u32, &crate::atlas::Function)> = row
            .exact_hash
            .map(|h| {
                by_shape
                    .iter()
                    .copied()
                    .filter(|(_, f)| f.byte_hash == h)
                    .collect()
            })
            .unwrap_or_default();
        let (pool, confidence) = if exact.is_empty() {
            (&by_shape, Confidence::Shape)
        } else {
            (&exact, Confidence::Exact)
        };
        // Nearest entry to the hint; ties (and no hint) go to the lowest
        // entry so the answer never depends on iteration order.
        let best = pool.iter().min_by_key(|(_, f)| {
            let dist = row.address_hint.map_or(0, |h| h.abs_diff(f.entry_cpu));
            (dist, f.entry_cpu, f.entry_rom)
        });
        match best {
            Some(&(function, _)) => report.matched.push(Match {
                name: row.name.clone(),
                function,
                confidence,
                candidates: by_shape.len() as u32,
            }),
            None => report.unmatched.push(row.name.clone()),
        }
    }
    report
}
