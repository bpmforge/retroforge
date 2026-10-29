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
//! entry). Two rows of one recipe that share a shape (and could share a mode)
//! cannot be told apart by their bytes; each then matches only the function at
//! its own address hint. A row that carries an entry `mode` (65816 recipes) only considers
//! functions entered in that mode. No candidate: the row is **unmatched**, the game keeps running on
//! the generic rebuild and the interpreter, and the row's hooks are inactive.

use crate::atlas::{entry, Atlas};
use crate::dataregion::{Part, RegionKind};

/// How far a row can be trusted (ticket W38-00).
///
/// Only [`Grade::Candidate`] is restricted: a candidate row never drives a
/// patch and never drives a hook on a host that may write memory. It may
/// still drive read-only hooks and labels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Grade {
    /// Measured by RetroForge tooling on a real run.
    #[default]
    Observed,
    /// Proposed by a finder or an AI and not verified yet.
    Candidate,
    /// Verified: a lockstep test passed, or a person checked it.
    Confirmed,
}

impl Grade {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Candidate => "candidate",
            Self::Confirmed => "confirmed",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "observed" => Some(Self::Observed),
            "candidate" => Some(Self::Candidate),
            "confirmed" => Some(Self::Confirmed),
            _ => None,
        }
    }

    /// Whether a row of this grade may drive a patch or a write-capable
    /// hook.
    #[must_use]
    pub const fn may_write(self) -> bool {
        !matches!(self, Self::Candidate)
    }
}

/// How a function was entered, in the atlas's terms ([`crate::entry`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Nmi,
    Irq,
    Reset,
    Brk,
    Call,
    Indirect,
}

impl EntryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nmi => "nmi",
            Self::Irq => "irq",
            Self::Reset => "reset",
            Self::Brk => "brk",
            Self::Call => "call",
            Self::Indirect => "indirect",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "nmi" => Self::Nmi,
            "irq" => Self::Irq,
            "reset" => Self::Reset,
            "brk" => Self::Brk,
            "call" => Self::Call,
            "indirect" => Self::Indirect,
            _ => return None,
        })
    }

    /// The [`crate::entry`] bit the atlas sets for this kind.
    #[must_use]
    pub const fn bit(self) -> u8 {
        match self {
            Self::Nmi => entry::NMI,
            Self::Irq => entry::IRQ,
            Self::Reset => entry::RESET,
            Self::Brk => entry::BRK,
            Self::Call => entry::CALL,
            Self::Indirect => entry::INDIRECT,
        }
    }

    /// The single interrupt kind among `kinds`, `None` when there is none or
    /// several (one handler behind two vectors tells nothing apart).
    #[must_use]
    pub fn sole_interrupt(kinds: u8) -> Option<Self> {
        let hit: Vec<Self> = [Self::Nmi, Self::Irq, Self::Reset, Self::Brk]
            .into_iter()
            .filter(|k| kinds & k.bit() != 0)
            .collect();
        match hit.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }
}

/// Which side of a function a hook may attach to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HookPoints {
    pub entry: bool,
    pub exit: bool,
}

/// One function the recipe knows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    /// Optional: how the function is entered (ticket W38-00). When present,
    /// only atlas functions entered that way can match, which separates twin
    /// stubs (an NMI and an IRQ handler with identical bytes) wherever a
    /// hack moved them.
    pub entry: Option<EntryKind>,
    /// Optional: instructions in the function on the observed dump. A count,
    /// not bytes; it bounds the scan that tells a removed function from one
    /// the drive did not reach ([`classify_unmatched`]).
    pub insn_count: Option<u32>,
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
    pub grade: Grade,
}

/// What a RAM byte (or run) means.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RamMeaning {
    pub addr: u32,
    /// Width in bytes.
    pub width: u8,
    pub name: String,
    pub source: String,
    pub grade: Grade,
}

/// Source prefixes the recipe hub accepts (RECIPE_HUB.md "Moderation rule" 1;
/// D-031).
const HUB_SOURCE_PREFIXES: [&str; 4] = ["observed:", "rule:", "finder:", "ai:"];

/// Words that mark a source as a community map or a disassembly project;
/// the hub refuses them whatever else the source says (D-021, D-031).
const HUB_BANNED_SOURCE_WORDS: [&str; 7] = [
    "community",
    "wiki",
    "forum",
    "disassembl",
    "decompil",
    "imported",
    "retroachievements",
];

/// Whether a row's `source` note is one the hub may take (D-031): it starts
/// with an allowed prefix and names no banned provenance. Imported rows
/// ("imported: user cheat", "imported: RetroAchievements") fail both tests.
#[must_use]
pub fn source_is_hub_eligible(source: &str) -> bool {
    let s = source.trim().to_ascii_lowercase();
    HUB_SOURCE_PREFIXES.iter().any(|p| s.starts_with(p))
        && !HUB_BANNED_SOURCE_WORDS.iter().any(|w| s.contains(w))
}

impl RamMeaning {
    /// Whether this row may be submitted to or promoted on the recipe hub.
    /// Only the source decides: an imported row stays local however its grade
    /// is edited (D-031).
    #[must_use]
    pub fn is_hub_eligible(&self) -> bool {
        source_is_hub_eligible(&self.source)
    }
}

/// A table of game objects in RAM (ticket W38-00): `slots` entries, field
/// `f` of slot `i` at `base + f.offset + i * stride`. That covers both a
/// record per slot (stride = record size, small offsets) and a column per
/// field (stride 1, offsets = distance between columns).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObjectTable {
    pub name: String,
    pub base: u32,
    pub slots: u32,
    pub stride: u32,
    pub x_lo: Option<u32>,
    pub x_hi: Option<u32>,
    pub y: Option<u32>,
    pub y_hi: Option<u32>,
    /// Offset of the object type id.
    pub kind: Option<u32>,
    /// Offset of the byte that says the slot is in use or what it is doing.
    pub state: Option<u32>,
    pub source: String,
    pub grade: Grade,
}

/// What a tile id range means to the game's physics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileClass {
    Solid,
    OneWay,
    Water,
    Hazard,
}

impl TileClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Solid => "solid",
            Self::OneWay => "one_way",
            Self::Water => "water",
            Self::Hazard => "hazard",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "solid" => Self::Solid,
            "one_way" => Self::OneWay,
            "water" => Self::Water,
            "hazard" => Self::Hazard,
            _ => return None,
        })
    }
}

/// Tile ids `first..=last` behave as `class`, on one layer and/or tileset
/// when those are given (ticket W38-00).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileSemantics {
    pub class: TileClass,
    pub first: u32,
    pub last: u32,
    pub layer: Option<u8>,
    pub tileset: Option<String>,
    pub source: String,
    pub grade: Grade,
}

/// A region of the ROM image and what the game uses it for (ticket W38-01).
/// Offsets and sizes only: the bytes stay in the cartridge (law 5). `rom` is
/// an offset into the image the atlas indexes (the PRG image on the NES, the
/// header-stripped ROM on the SNES).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRegion {
    pub name: String,
    pub kind: RegionKind,
    pub rom: u32,
    pub size: u32,
    /// Bytes between elements (1 for a byte table or a stream).
    pub stride: u32,
    /// For a split pointer table, which byte of each pointer this holds.
    pub part: Option<Part>,
    /// A pointer table: pointer width in bytes.
    pub pointer_bytes: Option<u8>,
    /// RAM address the index is read from.
    pub index_ram: Option<u32>,
    /// RAM address of the pointer a stream is read through.
    pub pointer_ram: Option<u32>,
    /// A stream: the pointer table whose entries point into it.
    pub via: Option<String>,
    pub confidence: Option<u8>,
    pub source: String,
    /// Finder output is [`Grade::Candidate`] until a level-switch experiment
    /// confirms it.
    pub grade: Grade,
}

/// One byte of a function to replace when its mod is on (ticket W36-06).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
    /// A [`Grade::Candidate`] row is never applied.
    pub grade: Grade,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recipe {
    pub id: String,
    /// The game engine family this recipe describes (a label, not logic).
    pub engine: String,
    pub functions: Vec<RecipeFunction>,
    pub ram: Vec<RamMeaning>,
    pub patches: Vec<PatchRow>,
    pub object_tables: Vec<ObjectTable>,
    pub tiles: Vec<TileSemantics>,
    pub data_regions: Vec<DataRegion>,
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
    /// The grade of the row that matched.
    pub grade: Grade,
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

    /// Like [`Self::function_of`] but for a hook that may write memory: a
    /// [`Grade::Candidate`] row resolves to nothing (ticket W38-00).
    #[must_use]
    pub fn function_of_writable(&self, name: &str) -> Option<u32> {
        self.matched
            .iter()
            .find(|m| m.name == name && m.grade.may_write())
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
            // A row that says how its function is entered only takes
            // functions entered that way (what separates twin stubs).
            .filter(|(_, f)| row.entry.is_none_or(|k| f.entry_kinds & k.bit() != 0))
            // Rows nothing but an address tells apart (see `twins`) match
            // only the function at their hint: never a look-alike elsewhere.
            .filter(|(_, f)| !twins(recipe, row) || row.address_hint == Some(f.entry_cpu))
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
                grade: row.grade,
            }),
            None => report.unmatched.push(row.name.clone()),
        }
    }
    report
}

/// Whether another row of `recipe` has this row's shape hash, could be
/// entered in the same mode and is not told apart by its `entry` kind. Their
/// bytes agree (a pair of interrupt stubs, say), so a ROM where one of them
/// changed would hand its name to the other; such rows are only trusted at the
/// address they were observed at (ticket W36-10). Rows with different entry
/// kinds are not twins: the atlas knows which function the NMI vector reached
/// (ticket W38-00).
fn twins(recipe: &Recipe, row: &RecipeFunction) -> bool {
    recipe.functions.iter().any(|o| {
        o.name != row.name
            && o.shape_hash == row.shape_hash
            && (o.mode.is_none() || row.mode.is_none() || o.mode == row.mode)
            && (o.entry.is_none() || row.entry.is_none() || o.entry == row.entry)
    })
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
    /// The patch row, or the function row it edits, is a
    /// [`Grade::Candidate`]: not verified, so never applied.
    Candidate { row: String, function: String },
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
            Self::Candidate { row, function } => write!(
                f,
                "patch `{row}`: it or `{function}` is only a candidate (not verified yet), \
                 so it was not applied"
            ),
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
        if !row.grade.may_write() || !func.grade.may_write() {
            return Err(PatchRefusal::Candidate { row: r, function });
        }
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

/// Why a recipe row found no function in an atlas (ticket W38-00).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmatchedReason {
    /// Nothing in the image has the row's shape at the row's decode mode: the
    /// function is gone, or edited so far its hash moved.
    Removed,
    /// Code of that shape sits in bytes the recording never executed: the
    /// drive did not reach it.
    NotReached,
    /// Code of that shape exists and ran, yet the row did not match it: the
    /// entry kind or decode mode the row insists on is not how it was entered.
    Changed,
}

impl UnmatchedReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::NotReached => "not_reached",
            Self::Changed => "changed",
        }
    }
}

/// Tell why `row` is unmatched by a superset scan: decode `row.insn_count`
/// instructions at **every** offset of `prg` (linearly, following the decode
/// mode through `REP`/`SEP`) and compare the shape hash, which is blind to
/// where the code sits. `executed(offset)` says whether the recording ran the
/// byte. A hit on an unexecuted byte wins ([`UnmatchedReason::NotReached`]),
/// then a hit on an executed one ([`UnmatchedReason::Changed`]), else
/// [`UnmatchedReason::Removed`].
///
/// This is a linear-sweep approximation: a function laid out in pieces (its
/// extent is the blocks control flow reaches, not a contiguous run) hashes
/// differently here and reads as removed. `None` when the row has no
/// `insn_count` to scan with.
///
/// Terminates by construction: one pass over the offsets, each decoding at
/// most `insn_count` instructions, every one of which consumes at least one
/// byte.
#[must_use]
pub fn classify_unmatched(
    isa: &dyn crate::Isa,
    prg: &[u8],
    executed: &dyn Fn(usize) -> bool,
    row: &RecipeFunction,
) -> Option<UnmatchedReason> {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let want = row.insn_count.filter(|&n| n > 0)?;
    let modes: Vec<u8> = match row.mode {
        Some(m) => vec![m],
        None => (0..isa.mode_count().max(1) as u8).collect(),
    };
    let cpu = |i: usize| {
        if isa.fixed_mapping() {
            isa.cpu_for_rom(i as u32)
        } else {
            0
        }
    };
    let mut seen_executed = false;
    for start in 0..prg.len() {
        for &m0 in &modes {
            let (mut at, mut mode, mut shape) = (start, m0, FNV_OFFSET);
            let mut ok = true;
            for _ in 0..want {
                let end = (at + isa.max_len().max(1)).min(prg.len());
                let Some(d) = isa.decode(&prg[at..end], cpu(at), crate::DecodeMode(mode)) else {
                    ok = false;
                    break;
                };
                let len = usize::from(d.len).max(1);
                let insn = &prg[at..(at + len).min(prg.len())];
                let masked = isa.masked_operand(insn, crate::DecodeMode(mode));
                for (i, &b) in insn.iter().enumerate() {
                    let kept = if masked.contains(&i) { 0 } else { b };
                    shape = (shape ^ u64::from(kept)).wrapping_mul(FNV_PRIME);
                }
                at += len;
                match d.after {
                    crate::ModeAfter::Same => {}
                    crate::ModeAfter::Set(m) => mode = m.0,
                    crate::ModeAfter::Unknown => {}
                }
            }
            if ok && shape == row.shape_hash {
                if !executed(start) {
                    return Some(UnmatchedReason::NotReached);
                }
                seen_executed = true;
            }
        }
    }
    Some(if seen_executed {
        UnmatchedReason::Changed
    } else {
        UnmatchedReason::Removed
    })
}

#[cfg(test)]
mod hub_tests {
    use super::*;

    #[test]
    fn hub_eligibility_follows_provenance() {
        let row = |src: &str| RamMeaning {
            source: src.into(),
            grade: Grade::Candidate,
            ..RamMeaning::default()
        };
        assert!(row("observed: cheat search").is_hub_eligible());
        assert!(row("finder: camera").is_hub_eligible());
        assert!(!row("imported: user cheat").is_hub_eligible());
        assert!(!row("imported: RetroAchievements").is_hub_eligible());
        assert!(!row("observed: from a community map").is_hub_eligible());
        assert!(!row("").is_hub_eligible());
        assert!(!row("test").is_hub_eligible());
    }
}
