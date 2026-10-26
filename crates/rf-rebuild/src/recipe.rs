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
//! entry). A row that carries an entry `mode` (65816 recipes) only considers
//! functions entered in that mode. No candidate: the row is **unmatched**, the game keeps running on
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
    /// Optional: the decode mode the function is entered in
    /// ([`crate::Function::entry_mode`]; ticket W36-10). The 65816's
    /// instruction widths depend on `E`/`M`/`X`, and the shape hash ignores
    /// them, so the same bytes entered in two modes are two functions with one
    /// shape hash. A row with a mode matches only functions entered in it; a
    /// row without one (every 6502 row) matches any mode.
    pub mode: Option<u8>,
    pub hooks: HookPoints,
    /// Where the row came from (observation, never a disassembly). Machine
    /// proposals say `rule:<rule>` or `ai:<model>` (ticket W36-09).
    pub source: String,
    /// How sure the source is, 0-100; `None` for facts people established
    /// (ticket W36-09).
    pub confidence: Option<u8>,
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

/// One byte of a function to replace when its mod is on (ticket W36-06).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchRow {
    /// Short label for reports and the ledger (`"spawn-lead"`).
    pub name: String,
    /// The mod this row belongs to; rows of one mod apply or refuse together.
    pub mod_name: String,
    /// The recipe function (by name) the row edits. That function's row must
    /// carry an exact hash.
    pub function: String,
    /// Index into the function's instruction-byte stream (see the module
    /// doc).
    pub offset: u32,
    /// The new byte. The old byte is deliberately not stored.
    pub value: u8,
    /// Where the row came from (observation, never a disassembly).
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recipe {
    pub id: String,
    /// The game engine family this recipe describes (a label, not logic).
    pub engine: String,
    pub functions: Vec<RecipeFunction>,
    pub ram: Vec<RamMeaning>,
    pub patches: Vec<PatchRow>,
}

impl Recipe {
    /// The distinct mod names that have patch rows, in first-seen order.
    #[must_use]
    pub fn mods(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for p in &self.patches {
            if !out.contains(&p.mod_name.as_str()) {
                out.push(&p.mod_name);
            }
        }
        out
    }
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
            .filter(|(_, f)| row.mode.is_none_or(|m| f.entry_mode == m))
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

/// A patch row placed at a byte of the PRG image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPatch {
    pub name: String,
    pub mod_name: String,
    /// The recipe function the row edits.
    pub function: String,
    /// Byte offset into the PRG image (not the file: a header is the
    /// console shell's to add).
    pub prg_offset: u32,
    pub value: u8,
}

/// Why a mod's patch rows were refused. The whole mod is refused; nothing is
/// ever half-applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchRefusal {
    /// The recipe has no rows for this mod name.
    NoSuchMod(String),
    /// A row names a function the recipe does not list.
    UnknownFunction { row: String, function: String },
    /// No atlas function has the row's function's shape.
    Unmatched { row: String, function: String },
    /// The function matched on shape only: its exact hash is absent from the
    /// recipe, or the bytes differ from the ones the row was written for.
    NotExact { row: String, function: String },
    /// Several atlas functions have identical bytes, so which copy to edit
    /// is a guess.
    Ambiguous {
        row: String,
        function: String,
        copies: u32,
    },
    /// The offset is past the end of the function.
    OutOfFunction {
        row: String,
        function: String,
        offset: u32,
        len: u32,
    },
    /// Two rows edit the same byte.
    Overlap { first: String, second: String },
}

impl std::fmt::Display for PatchRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchMod(m) => write!(f, "the recipe has no mod called `{m}`"),
            Self::UnknownFunction { row, function } => write!(
                f,
                "patch `{row}` edits `{function}`, which the recipe does not list"
            ),
            Self::Unmatched { row, function } => write!(
                f,
                "patch `{row}`: no function in this ROM's code atlas looks like `{function}` \
                 (play more of the game with coverage recording on, then rebuild the atlas)"
            ),
            Self::NotExact { row, function } => write!(
                f,
                "patch `{row}`: `{function}` is not byte-for-byte the function the patch was \
                 written for, so it was not applied"
            ),
            Self::Ambiguous {
                row,
                function,
                copies,
            } => write!(
                f,
                "patch `{row}`: {copies} functions have the same bytes as `{function}`; \
                 refusing to guess which to edit"
            ),
            Self::OutOfFunction {
                row,
                function,
                offset,
                len,
            } => write!(
                f,
                "patch `{row}`: offset {offset} is past the end of `{function}` ({len} bytes)"
            ),
            Self::Overlap { first, second } => {
                write!(f, "patches `{first}` and `{second}` edit the same byte")
            }
        }
    }
}

impl std::error::Error for PatchRefusal {}

/// Place the rows of `mod_name` at PRG offsets, against the atlas of the
/// **unpatched** image. All or nothing: any row that does not resolve, or whose
/// function is not an exact, unambiguous match, refuses the whole mod.
///
/// # Errors
/// [`PatchRefusal`] naming the first row that could not be placed.
pub fn resolve_patches(
    atlas: &Atlas,
    recipe: &Recipe,
    mod_name: &str,
) -> Result<Vec<ResolvedPatch>, PatchRefusal> {
    let rows: Vec<&PatchRow> = recipe
        .patches
        .iter()
        .filter(|p| p.mod_name == mod_name)
        .collect();
    if rows.is_empty() {
        return Err(PatchRefusal::NoSuchMod(mod_name.to_string()));
    }
    // Only the functions the rows name need matching.
    let wanted = Recipe {
        functions: recipe
            .functions
            .iter()
            .filter(|f| rows.iter().any(|r| r.function == f.name))
            .cloned()
            .collect(),
        ..Recipe::default()
    };
    let report = match_recipe(atlas, &wanted);
    let mut out: Vec<ResolvedPatch> = Vec::with_capacity(rows.len());
    for row in rows {
        let (r, function) = (row.name.clone(), row.function.clone());
        let Some(func) = wanted.functions.iter().find(|f| f.name == row.function) else {
            return Err(PatchRefusal::UnknownFunction { row: r, function });
        };
        let Some(m) = report.matched.iter().find(|m| m.name == row.function) else {
            return Err(PatchRefusal::Unmatched { row: r, function });
        };
        let f = &atlas.functions[m.function as usize];
        let exact = match func.exact_hash {
            Some(h) if m.confidence == Confidence::Exact && f.byte_hash == h => h,
            _ => return Err(PatchRefusal::NotExact { row: r, function }),
        };
        let copies = atlas
            .functions
            .iter()
            .filter(|g| g.byte_hash == exact)
            .count() as u32;
        if copies != 1 {
            return Err(PatchRefusal::Ambiguous {
                row: row.name.clone(),
                function: row.function.clone(),
                copies,
            });
        }
        let Some(prg_offset) = stream_offset(atlas, f, row.offset) else {
            return Err(PatchRefusal::OutOfFunction {
                row: row.name.clone(),
                function: row.function.clone(),
                offset: row.offset,
                len: f.byte_len,
            });
        };
        if let Some(prev) = out.iter().find(|p| p.prg_offset == prg_offset) {
            return Err(PatchRefusal::Overlap {
                first: prev.name.clone(),
                second: row.name.clone(),
            });
        }
        out.push(ResolvedPatch {
            name: row.name.clone(),
            mod_name: row.mod_name.clone(),
            function: row.function.clone(),
            prg_offset,
            value: row.value,
        });
    }
    Ok(out)
}

/// The PRG offset of byte `index` of `f`'s instruction-byte stream: the
/// instructions in `(cpu, rom)` order, as [`crate::Function::byte_hash`]
/// walks them.
fn stream_offset(atlas: &Atlas, f: &crate::atlas::Function, index: u32) -> Option<u32> {
    let mut insns: Vec<(u32, u32, u8)> = f
        .blocks
        .iter()
        .filter_map(|&b| atlas.blocks.get(b as usize))
        .flat_map(|b| b.insns.iter())
        .map(|i| (i.cpu, i.rom, i.len))
        .collect();
    insns.sort_unstable();
    let mut left = index;
    for (_, rom, len) in insns {
        // `len` is at least one byte for any decoded instruction; a zero
        // would only make the walk see the same index twice, never loop.
        if left < u32::from(len) {
            return Some(rom + left);
        }
        left -= u32::from(len);
    }
    None
}
