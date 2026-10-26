//! Rebuild recipes: TOML text <-> [`rf_rebuild::Recipe`] (ticket W36-05;
//! `docs/design/GAME_PROFILES.md` "Recipes").
//!
//! A recipe is a data file next to a profile (`profiles/<console>/<game>/
//! recipe.toml`): facts about a game's functions found by observation, never
//! bytes (law 5) and never copied from a disassembly (D-021). The types and
//! the matcher live in `rf-rebuild`; this module only reads and writes the
//! text. Hashes are quoted hex strings (`"0x00ab..."`) because TOML integers
//! are signed 64-bit and a 64-bit FNV hash does not fit.
//!
//! ```toml
//! [recipe]
//! format = 1
//! id = "super-mario-bros"
//! engine = "smb1"
//!
//! [[function]]
//! name = "nmi"
//! shape_hash = "0x0123456789abcdef"    # required
//! exact_hash = "0xfedcba9876543210"    # optional: raises confidence
//! address_hint = 0x8E19                # optional: CPU entry on the observed dump
//! entry = "nmi"                        # optional (W38-00): nmi|irq|reset|brk|call|indirect
//! insn_count = 41                      # optional: bounds the unmatched-reason scan
//! mode = 3                             # optional: 65816 entry mode (W36-10), 0..=4
//! hooks = ["entry", "exit"]            # which sides a hook may attach to
//! confidence = 90                      # optional, percent: machine-made names
//! grade = "observed"                   # observed | candidate | confirmed
//! source = "observed: NMI vector target of a 1-1 recording"   # required
//! # Names proposed by the understand pass (ticket W36-09) say where they came
//! # from: source = "rule:read_controllers" or source = "ai:<model>", with a
//! # confidence. Names people wrote (any other source) win over them.
//!
//! [[ram]]
//! addr = 0x075A
//! width = 1
//! name = "lives"
//! grade = "confirmed"
//! source = "profile.toml memory_map: lives"                   # required
//!
//! # Object tables and tile semantics (ticket W38-00): facts, no consumers yet.
//! # Field `f` of slot `i` is at base + f + i * stride.
//! [[object_table]]
//! name = "enemies"
//! base = 0x0F
//! slots = 5
//! stride = 1
//! x_lo = 0x3F2                         # optional offsets: x_lo x_hi y y_hi type state
//! state = 0
//! grade = "observed"
//! source = "observed: ..."
//!
//! [[tile]]
//! class = "solid"                      # solid | one_way | water | hazard
//! first = 0x10
//! last = 0x1f                          # inclusive
//! layer = 1                            # optional
//! tileset = "grass"                    # optional
//! grade = "candidate"
//! source = "ai:..."
//!
//! # A mod: bytes of a listed function to replace (ticket W36-06). Only the
//! # NEW value is stored; the function's `exact_hash` vouches for the old one.
//! [[patch]]
//! name = "spawn-lead"                 # label for reports
//! mod = "live-enemies"                # rows of one mod apply together
//! function = "tick"                   # a [[function]] with an exact_hash
//! offset = 41                         # index into the function's byte stream
//! value = 0xF0
//! grade = "confirmed"                 # a candidate patch is never applied
//! source = "observed: ..."            # required
//! ```
//!
//! A row without `grade` is a pre-W38-00 row: it reads as `observed` (`ai:`
//! sources as `candidate`) and is written back with the grade spelled out.
//!
//! Unknown keys are an error (the format is new and small; a typo that
//! silently dropped a hash would leave a hook inactive with no explanation).

use std::fmt::Write as _;
use std::path::Path;

use rf_rebuild::{
    EntryKind, Grade, HookPoints, ObjectTable, PatchRow, RamMeaning, Recipe, RecipeFunction,
    TileClass, TileSemantics,
};
use serde::Deserialize;

/// The recipe format this build reads and writes.
pub const RECIPE_FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeError {
    Io(String),
    Parse(String),
    /// A newer (or unknown) `recipe.format`.
    Format {
        found: u32,
        supported: u32,
    },
    /// A hash that is not a `0x` plus 1-16 hex digits string.
    BadHash {
        function: String,
        field: &'static str,
        value: String,
    },
    /// A row without the `source` note facts must carry.
    MissingSource {
        table: &'static str,
        name: String,
    },
    /// `hooks` names something other than `entry` / `exit`.
    BadHookPoint {
        function: String,
        value: String,
    },
    DuplicateFunction(String),
    /// A `[[patch]]` that names no listed function, or one without an
    /// `exact_hash` (the hash is what vouches for the byte being replaced).
    BadPatch {
        patch: String,
        why: &'static str,
    },
    DuplicatePatch(String),
    /// A `mode` above 4 (the 65816 has five decode modes).
    BadMode {
        function: String,
        value: u8,
    },
    /// A `grade` other than observed / candidate / confirmed.
    BadGrade {
        row: String,
        value: String,
    },
    /// An `entry` other than nmi / irq / reset / brk / call / indirect.
    BadEntry {
        function: String,
        value: String,
    },
    /// A `[[object_table]]` or `[[tile]]` that cannot be right.
    BadFact {
        table: &'static str,
        name: String,
        why: &'static str,
    },
    /// A `confidence` above 100 (it is a percentage).
    BadConfidence {
        function: String,
        value: u8,
    },
}

impl std::fmt::Display for RecipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read recipe: {e}"),
            Self::Parse(e) => write!(f, "recipe parse error: {e}"),
            Self::Format { found, supported } => write!(
                f,
                "recipe format {found} is newer than this build reads ({supported})"
            ),
            Self::BadHash {
                function,
                field,
                value,
            } => write!(
                f,
                "function `{function}`: {field} `{value}` is not a quoted hex hash like \"0x00ff...\""
            ),
            Self::MissingSource { table, name } => write!(
                f,
                "[[{table}]] `{name}` has no `source` — recipe rows are facts and must say where they came from"
            ),
            Self::BadHookPoint { function, value } => write!(
                f,
                "function `{function}`: hook point `{value}` is neither \"entry\" nor \"exit\""
            ),
            Self::DuplicateFunction(n) => write!(f, "function `{n}` appears twice"),
            Self::BadPatch { patch, why } => write!(f, "[[patch]] `{patch}`: {why}"),
            Self::DuplicatePatch(n) => write!(f, "patch `{n}` appears twice"),
            Self::BadMode { function, value } => write!(
                f,
                "function `{function}`: mode {value} is not a decode mode (0..=4)"
            ),
            Self::BadGrade { row, value } => write!(
                f,
                "`{row}`: grade `{value}` is not \"observed\", \"candidate\" or \"confirmed\""
            ),
            Self::BadEntry { function, value } => write!(
                f,
                "function `{function}`: entry `{value}` is not nmi, irq, reset, brk, call or indirect"
            ),
            Self::BadFact { table, name, why } => write!(f, "[[{table}]] `{name}`: {why}"),
            Self::BadConfidence { function, value } => write!(
                f,
                "function `{function}`: confidence {value} is not a percentage (0-100)"
            ),
        }
    }
}

impl std::error::Error for RecipeError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipeDoc {
    recipe: Head,
    #[serde(default)]
    function: Vec<FunctionRow>,
    #[serde(default)]
    ram: Vec<RamRow>,
    #[serde(default)]
    patch: Vec<PatchRowDoc>,
    #[serde(default)]
    object_table: Vec<ObjectTableDoc>,
    #[serde(default)]
    tile: Vec<TileDoc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    format: u32,
    id: String,
    #[serde(default)]
    engine: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FunctionRow {
    name: String,
    shape_hash: String,
    exact_hash: Option<String>,
    address_hint: Option<u32>,
    mode: Option<u8>,
    entry: Option<String>,
    insn_count: Option<u32>,
    #[serde(default)]
    hooks: Vec<String>,
    source: Option<String>,
    confidence: Option<u8>,
    grade: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RamRow {
    addr: u32,
    #[serde(default = "one")]
    width: u8,
    name: String,
    source: Option<String>,
    grade: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectTableDoc {
    name: String,
    base: u32,
    slots: u32,
    #[serde(default = "one32")]
    stride: u32,
    x_lo: Option<u32>,
    x_hi: Option<u32>,
    y: Option<u32>,
    y_hi: Option<u32>,
    #[serde(rename = "type")]
    kind: Option<u32>,
    state: Option<u32>,
    source: Option<String>,
    grade: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TileDoc {
    class: String,
    first: u32,
    last: u32,
    layer: Option<u8>,
    tileset: Option<String>,
    source: Option<String>,
    grade: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchRowDoc {
    name: String,
    #[serde(rename = "mod")]
    mod_name: String,
    function: String,
    offset: u32,
    value: u8,
    source: Option<String>,
    grade: Option<String>,
}

fn one() -> u8 {
    1
}

fn one32() -> u32 {
    1
}

/// The grade a row states, or the one a pre-W38-00 row (no `grade` key) gets:
/// `candidate` for a machine guess from an AI, else `observed`. `legacy`
/// counts the rows that had to be defaulted.
fn grade_of(
    row: &str,
    grade: Option<&str>,
    source: &str,
    legacy: &mut usize,
) -> Result<Grade, RecipeError> {
    match grade {
        Some(g) => Grade::parse(g).ok_or_else(|| RecipeError::BadGrade {
            row: row.to_string(),
            value: g.to_string(),
        }),
        None => {
            *legacy += 1;
            Ok(if source.starts_with("ai:") {
                Grade::Candidate
            } else {
                Grade::Observed
            })
        }
    }
}

fn parse_hash(function: &str, field: &'static str, s: &str) -> Result<u64, RecipeError> {
    let bad = || RecipeError::BadHash {
        function: function.to_string(),
        field,
        value: s.to_string(),
    };
    let digits = s.strip_prefix("0x").ok_or_else(bad)?;
    if digits.is_empty() || digits.len() > 16 {
        return Err(bad());
    }
    u64::from_str_radix(digits, 16).map_err(|_| bad())
}

fn source_of(table: &'static str, name: &str, s: Option<String>) -> Result<String, RecipeError> {
    s.filter(|s| !s.trim().is_empty())
        .ok_or_else(|| RecipeError::MissingSource {
            table,
            name: name.to_string(),
        })
}

/// Parse recipe text.
///
/// # Errors
/// [`RecipeError`] for malformed TOML, a newer format, a bad hash, hook
/// point, grade or entry kind, a row with no `source`, or a function named
/// twice.
pub fn load_recipe_str(text: &str) -> Result<Recipe, RecipeError> {
    load_recipe_noted(text).map(|(r, _)| r)
}

/// [`load_recipe_str`] plus how many rows had no `grade` and were defaulted
/// (a recipe written before ticket W38-00); `0` means the text is already
/// current. [`recipe_to_toml`] of the result is the migrated text.
///
/// # Errors
/// As [`load_recipe_str`].
pub fn load_recipe_noted(text: &str) -> Result<(Recipe, usize), RecipeError> {
    let doc: RecipeDoc = toml::from_str(text).map_err(|e| RecipeError::Parse(e.to_string()))?;
    if doc.recipe.format > RECIPE_FORMAT || doc.recipe.format == 0 {
        return Err(RecipeError::Format {
            found: doc.recipe.format,
            supported: RECIPE_FORMAT,
        });
    }
    let mut legacy = 0usize;
    let mut functions: Vec<RecipeFunction> = Vec::new();
    for row in doc.function {
        if functions.iter().any(|f| f.name == row.name) {
            return Err(RecipeError::DuplicateFunction(row.name));
        }
        let mut hooks = HookPoints::default();
        // No `hooks` key means both sides are allowed.
        if row.hooks.is_empty() {
            hooks = HookPoints {
                entry: true,
                exit: true,
            };
        }
        for h in &row.hooks {
            match h.as_str() {
                "entry" => hooks.entry = true,
                "exit" => hooks.exit = true,
                other => {
                    return Err(RecipeError::BadHookPoint {
                        function: row.name,
                        value: other.to_string(),
                    })
                }
            }
        }
        if let Some(m) = row.mode.filter(|&m| m > 4) {
            return Err(RecipeError::BadMode {
                function: row.name,
                value: m,
            });
        }
        if let Some(c) = row.confidence.filter(|&c| c > 100) {
            return Err(RecipeError::BadConfidence {
                function: row.name,
                value: c,
            });
        }
        let entry = match row.entry.as_deref() {
            None => None,
            Some(e) => Some(EntryKind::parse(e).ok_or_else(|| RecipeError::BadEntry {
                function: row.name.clone(),
                value: e.to_string(),
            })?),
        };
        let source = source_of("function", &row.name, row.source)?;
        functions.push(RecipeFunction {
            mode: row.mode,
            confidence: row.confidence,
            shape_hash: parse_hash(&row.name, "shape_hash", &row.shape_hash)?,
            exact_hash: row
                .exact_hash
                .as_deref()
                .map(|h| parse_hash(&row.name, "exact_hash", h))
                .transpose()?,
            address_hint: row.address_hint,
            entry,
            insn_count: row.insn_count,
            hooks,
            grade: grade_of(&row.name, row.grade.as_deref(), &source, &mut legacy)?,
            source,
            name: row.name,
        });
    }
    let mut ram = Vec::new();
    for row in doc.ram {
        let source = source_of("ram", &row.name, row.source)?;
        ram.push(RamMeaning {
            addr: row.addr,
            width: row.width,
            grade: grade_of(&row.name, row.grade.as_deref(), &source, &mut legacy)?,
            source,
            name: row.name,
        });
    }
    let mut patches: Vec<PatchRow> = Vec::new();
    for row in doc.patch {
        if patches.iter().any(|p| p.name == row.name) {
            return Err(RecipeError::DuplicatePatch(row.name));
        }
        let bad = |why| RecipeError::BadPatch {
            patch: row.name.clone(),
            why,
        };
        match functions.iter().find(|f| f.name == row.function) {
            None => return Err(bad("its function is not listed in this recipe")),
            Some(f) if f.exact_hash.is_none() => {
                return Err(bad(
                    "its function has no exact_hash to vouch for the old byte",
                ))
            }
            Some(_) => {}
        }
        let source = source_of("patch", &row.name, row.source)?;
        patches.push(PatchRow {
            grade: grade_of(&row.name, row.grade.as_deref(), &source, &mut legacy)?,
            source,
            name: row.name,
            mod_name: row.mod_name,
            function: row.function,
            offset: row.offset,
            value: row.value,
        });
    }
    let mut object_tables: Vec<ObjectTable> = Vec::new();
    for row in doc.object_table {
        let bad = |why| RecipeError::BadFact {
            table: "object_table",
            name: row.name.clone(),
            why,
        };
        if object_tables.iter().any(|t| t.name == row.name) {
            return Err(bad("appears twice"));
        }
        if row.slots == 0 || row.stride == 0 {
            return Err(bad("needs at least one slot and a stride of at least 1"));
        }
        let source = source_of("object_table", &row.name, row.source.clone())?;
        object_tables.push(ObjectTable {
            grade: grade_of(&row.name, row.grade.as_deref(), &source, &mut legacy)?,
            source,
            name: row.name,
            base: row.base,
            slots: row.slots,
            stride: row.stride,
            x_lo: row.x_lo,
            x_hi: row.x_hi,
            y: row.y,
            y_hi: row.y_hi,
            kind: row.kind,
            state: row.state,
        });
    }
    let mut tiles: Vec<TileSemantics> = Vec::new();
    for row in doc.tile {
        let label = format!("{} {:#x}..={:#x}", row.class, row.first, row.last);
        let class = TileClass::parse(&row.class).ok_or_else(|| RecipeError::BadFact {
            table: "tile",
            name: label.clone(),
            why: "class is not solid, one_way, water or hazard",
        })?;
        if row.first > row.last {
            return Err(RecipeError::BadFact {
                table: "tile",
                name: label,
                why: "first is above last",
            });
        }
        let source = source_of("tile", &label, row.source)?;
        tiles.push(TileSemantics {
            grade: grade_of(&label, row.grade.as_deref(), &source, &mut legacy)?,
            class,
            first: row.first,
            last: row.last,
            layer: row.layer,
            tileset: row.tileset,
            source,
        });
    }
    Ok((
        Recipe {
            id: doc.recipe.id,
            engine: doc.recipe.engine,
            functions,
            ram,
            patches,
            object_tables,
            tiles,
        },
        legacy,
    ))
}

/// Read a recipe file.
///
/// # Errors
/// [`RecipeError::Io`] plus everything [`load_recipe_str`] reports.
pub fn load_recipe_file(path: &Path) -> Result<Recipe, RecipeError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| RecipeError::Io(format!("{}: {e}", path.display())))?;
    load_recipe_str(&text)
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write_function(out: &mut String, f: &RecipeFunction) {
    let _ = writeln!(out, "\n[[function]]\nname = {}", quote(&f.name));
    let _ = writeln!(out, "shape_hash = \"{:#018x}\"", f.shape_hash);
    if let Some(h) = f.exact_hash {
        let _ = writeln!(out, "exact_hash = \"{h:#018x}\"");
    }
    if let Some(a) = f.address_hint {
        let _ = writeln!(out, "address_hint = {a:#06x}");
    }
    if let Some(m) = f.mode {
        let _ = writeln!(out, "mode = {m}");
    }
    if let Some(e) = f.entry {
        let _ = writeln!(out, "entry = \"{}\"", e.as_str());
    }
    if let Some(n) = f.insn_count {
        let _ = writeln!(out, "insn_count = {n}");
    }
    let mut hooks = Vec::new();
    if f.hooks.entry {
        hooks.push("\"entry\"");
    }
    if f.hooks.exit {
        hooks.push("\"exit\"");
    }
    let _ = writeln!(out, "hooks = [{}]", hooks.join(", "));
    if let Some(c) = f.confidence {
        let _ = writeln!(out, "confidence = {c}");
    }
    let _ = writeln!(out, "grade = \"{}\"", f.grade.as_str());
    let _ = writeln!(out, "source = {}", quote(&f.source));
}

/// First line of the block [`name_rows_block`] writes.
pub const NAME_BLOCK_BEGIN: &str =
    "# ---- understand pass: machine-proposed names (rule:/ai: sources) ----";
/// Last line of that block.
pub const NAME_BLOCK_END: &str = "# ---- end understand pass ----";

/// `rows` as `[[function]]` tables between marker comments, for appending to
/// a recipe file without rewriting (and losing the comments of) the rest of
/// it (ticket W36-09). Empty `rows` give an empty string.
#[must_use]
pub fn name_rows_block(rows: &[RecipeFunction]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut out = format!("\n{NAME_BLOCK_BEGIN}\n");
    for f in rows {
        write_function(&mut out, f);
    }
    let _ = writeln!(out, "\n{NAME_BLOCK_END}");
    out
}

/// `text` with any earlier marked block removed and `block` (from
/// [`name_rows_block`]) appended: re-running the understand pass replaces its
/// own rows and leaves every other row, comment and table as it was.
#[must_use]
pub fn splice_name_block(text: &str, block: &str) -> String {
    let mut kept = String::with_capacity(text.len() + block.len());
    let mut skipping = false;
    for line in text.lines() {
        if line.trim_end() == NAME_BLOCK_BEGIN {
            skipping = true;
        }
        if !skipping {
            kept.push_str(line);
            kept.push('\n');
        }
        if skipping && line.trim_end() == NAME_BLOCK_END {
            skipping = false;
        }
    }
    let trimmed = kept.trim_end_matches('\n');
    format!("{trimmed}\n{block}")
}

/// Write `recipe` as TOML that [`load_recipe_str`] reads back to an equal
/// recipe. Meant for tools that build a recipe by observation; comments are
/// the author's to add.
#[must_use]
pub fn recipe_to_toml(recipe: &Recipe) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "[recipe]\nformat = {RECIPE_FORMAT}\nid = {}\nengine = {}",
        quote(&recipe.id),
        quote(&recipe.engine)
    );
    for f in &recipe.functions {
        write_function(&mut out, f);
    }
    for r in &recipe.ram {
        let _ = writeln!(
            out,
            "\n[[ram]]\naddr = {:#06x}\nwidth = {}\nname = {}\ngrade = \"{}\"\nsource = {}",
            r.addr,
            r.width,
            quote(&r.name),
            r.grade.as_str(),
            quote(&r.source)
        );
    }
    for p in &recipe.patches {
        let _ = writeln!(
            out,
            "\n[[patch]]\nname = {}\nmod = {}\nfunction = {}\noffset = {}\nvalue = {:#04x}\ngrade = \"{}\"\nsource = {}",
            quote(&p.name),
            quote(&p.mod_name),
            quote(&p.function),
            p.offset,
            p.value,
            p.grade.as_str(),
            quote(&p.source)
        );
    }
    for t in &recipe.object_tables {
        let _ = writeln!(
            out,
            "\n[[object_table]]\nname = {}\nbase = {:#06x}\nslots = {}\nstride = {}",
            quote(&t.name),
            t.base,
            t.slots,
            t.stride
        );
        for (key, v) in [
            ("x_lo", t.x_lo),
            ("x_hi", t.x_hi),
            ("y", t.y),
            ("y_hi", t.y_hi),
            ("type", t.kind),
            ("state", t.state),
        ] {
            if let Some(v) = v {
                let _ = writeln!(out, "{key} = {v:#x}");
            }
        }
        let _ = writeln!(
            out,
            "grade = \"{}\"\nsource = {}",
            t.grade.as_str(),
            quote(&t.source)
        );
    }
    for t in &recipe.tiles {
        let _ = writeln!(
            out,
            "\n[[tile]]\nclass = \"{}\"\nfirst = {:#x}\nlast = {:#x}",
            t.class.as_str(),
            t.first,
            t.last
        );
        if let Some(l) = t.layer {
            let _ = writeln!(out, "layer = {l}");
        }
        if let Some(ts) = &t.tileset {
            let _ = writeln!(out, "tileset = {}", quote(ts));
        }
        let _ = writeln!(
            out,
            "grade = \"{}\"\nsource = {}",
            t.grade.as_str(),
            quote(&t.source)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
        [recipe]
        format = 1
        id = "demo"
        engine = "demo-engine"

        [[function]]
        name = "tick"
        shape_hash = "0xffffffffffffffff"
        exact_hash = "0x0000000000000001"
        address_hint = 0x8E19
        hooks = ["entry"]
        source = "observed"

        [[function]]
        name = "draw"
        shape_hash = "0x12"
        source = "observed"

        [[function]]
        name = "read_controllers"
        shape_hash = "0x34"
        exact_hash = "0x56"
        address_hint = 0x8E80
        confidence = 92
        source = "rule:read_controllers"

        [[function]]
        name = "vram_upload_c3a1"
        shape_hash = "0x78"
        confidence = 70
        source = "ai:claude-opus-5-5"

        [[ram]]
        addr = 0x075A
        name = "lives"
        source = "profile memory_map"

        [[patch]]
        name = "lead"
        mod = "wide"
        function = "tick"
        offset = 41
        value = 0xF0
        source = "observed: test"
    "#;

    #[test]
    fn a_recipe_parses_with_full_width_hashes_and_defaults() {
        let r = load_recipe_str(GOOD).expect("parses");
        assert_eq!((r.id.as_str(), r.engine.as_str()), ("demo", "demo-engine"));
        assert_eq!(r.functions[0].shape_hash, u64::MAX, "above i64::MAX");
        assert_eq!(r.functions[0].exact_hash, Some(1));
        assert_eq!(r.functions[0].address_hint, Some(0x8E19));
        assert!(r.functions[0].hooks.entry && !r.functions[0].hooks.exit);
        assert_eq!(r.functions[1].exact_hash, None);
        assert_eq!(r.functions[1].confidence, None);
        assert_eq!(r.functions[2].confidence, Some(92));
        assert_eq!(r.functions[2].source, "rule:read_controllers");
        assert_eq!(r.functions[3].source, "ai:claude-opus-5-5");
        assert!(r.functions[1].hooks.entry && r.functions[1].hooks.exit);
        assert_eq!((r.ram[0].addr, r.ram[0].width), (0x75A, 1));
        assert_eq!(r.patches.len(), 1);
        let p = &r.patches[0];
        assert_eq!(
            (p.mod_name.as_str(), p.function.as_str(), p.offset, p.value),
            ("wide", "tick", 41, 0xF0)
        );
        assert_eq!(r.mods(), ["wide"]);
    }

    #[test]
    fn an_entry_mode_reads_writes_and_is_range_checked() {
        let with_mode = GOOD.replace(
            "address_hint = 0x8E19\n",
            "address_hint = 0x8E19\n        mode = 3\n",
        );
        let r = load_recipe_str(&with_mode).expect("parses");
        assert_eq!(r.functions[0].mode, Some(3));
        assert_eq!(r.functions[1].mode, None);
        let text = recipe_to_toml(&r);
        assert_eq!(
            text.matches("mode = 3").count(),
            1,
            "only the row that has one"
        );
        assert_eq!(load_recipe_str(&text), Ok(r));
        assert!(matches!(
            load_recipe_str(&with_mode.replace("mode = 3", "mode = 5")),
            Err(RecipeError::BadMode { value: 5, .. })
        ));
    }

    #[test]
    fn writing_then_reading_is_the_identity() {
        let r = load_recipe_str(GOOD).unwrap();
        assert_eq!(load_recipe_str(&recipe_to_toml(&r)), Ok(r));
    }

    #[test]
    fn a_spliced_block_replaces_its_own_rows_and_nothing_else() {
        let r = load_recipe_str(GOOD).unwrap();
        let rows: Vec<RecipeFunction> = r
            .functions
            .iter()
            .filter(|f| f.source.starts_with("rule:") || f.source.starts_with("ai:"))
            .cloned()
            .collect();
        assert_eq!(rows.len(), 2);
        let human = "# comment kept\n[recipe]\nformat = 1\nid = \"demo\"\n\n[[function]]\nname = \"tick\"\nshape_hash = \"0x1\"\nsource = \"observed\"\n";
        let once = splice_name_block(human, &name_rows_block(&rows));
        let parsed = load_recipe_str(&once).expect("spliced recipe parses");
        assert_eq!(parsed.functions.len(), 3);
        assert_eq!(parsed.functions[1], rows[0]);
        assert!(once.starts_with("# comment kept"));
        // A second run replaces the block, it does not stack another.
        let twice = splice_name_block(&once, &name_rows_block(&rows[..1]));
        let parsed2 = load_recipe_str(&twice).expect("parses");
        assert_eq!(parsed2.functions.len(), 2);
        assert_eq!(twice.matches(NAME_BLOCK_BEGIN).count(), 1);
        // No rows: the block is gone, the human part is untouched.
        let none = splice_name_block(&twice, &name_rows_block(&[]));
        assert_eq!(load_recipe_str(&none).unwrap().functions.len(), 1);
        assert!(none.contains("# comment kept"));
    }

    const V1: &str = r#"
        [recipe]
        format = 1
        id = "demo"
        engine = "demo-engine"

        [[function]]
        name = "nmi"
        shape_hash = "0x1"
        entry = "nmi"
        insn_count = 41
        grade = "confirmed"
        source = "rule:nmi"

        [[function]]
        name = "guess"
        shape_hash = "0x2"
        grade = "candidate"
        source = "ai:some-model"

        [[ram]]
        addr = 0x0F
        name = "enemy_flag"
        grade = "observed"
        source = "observed"

        [[object_table]]
        name = "enemies"
        base = 0x0F
        slots = 5
        stride = 1
        x_lo = 0x3F2
        y = 0x2E
        type = 0x5
        state = 0
        grade = "observed"
        source = "observed: slot flag bytes in 1-1 play"

        [[tile]]
        class = "one_way"
        first = 0x10
        last = 0x1f
        layer = 1
        tileset = "grass"
        grade = "candidate"
        source = "ai:some-model"

        [[tile]]
        class = "water"
        first = 0x40
        last = 0x40
        grade = "confirmed"
        source = "profile"
    "#;

    #[test]
    fn a_v1_recipe_reads_its_grades_entry_kinds_and_fact_tables_and_round_trips() {
        let (r, legacy) = load_recipe_noted(V1).expect("parses");
        assert_eq!(legacy, 0, "every row states its grade");
        assert_eq!(r.functions[0].entry, Some(EntryKind::Nmi));
        assert_eq!(r.functions[0].insn_count, Some(41));
        assert_eq!(r.functions[0].grade, Grade::Confirmed);
        assert_eq!(r.functions[1].grade, Grade::Candidate);
        let t = &r.object_tables[0];
        assert_eq!((t.base, t.slots, t.stride), (0x0F, 5, 1));
        assert_eq!(
            (t.x_lo, t.x_hi, t.y, t.kind, t.state),
            (Some(0x3F2), None, Some(0x2E), Some(5), Some(0))
        );
        assert_eq!(r.tiles[0].class, TileClass::OneWay);
        assert_eq!(
            (r.tiles[0].first, r.tiles[0].last, r.tiles[0].layer),
            (0x10, 0x1f, Some(1))
        );
        assert_eq!(r.tiles[0].tileset.as_deref(), Some("grass"));
        assert_eq!(r.tiles[1].grade, Grade::Confirmed);
        let text = recipe_to_toml(&r);
        assert_eq!(load_recipe_noted(&text), Ok((r, 0)));
    }

    #[test]
    fn a_pre_v1_recipe_migrates_by_loading_and_writing() {
        // `GOOD` has no grades (the shape of every file before W38-00).
        let (r, legacy) = load_recipe_noted(GOOD).expect("parses");
        assert_eq!(legacy, 6, "4 functions + 1 ram + 1 patch defaulted");
        assert_eq!(r.functions[0].grade, Grade::Observed);
        assert_eq!(
            r.functions[2].grade,
            Grade::Observed,
            "rule: rows were measured"
        );
        assert_eq!(
            r.functions[3].grade,
            Grade::Candidate,
            "ai: rows are guesses"
        );
        let migrated = recipe_to_toml(&r);
        assert_eq!(migrated.matches("grade = ").count(), 6);
        assert_eq!(load_recipe_noted(&migrated), Ok((r, 0)));
    }

    #[test]
    fn bad_v1_input_is_refused_with_a_reason() {
        let with = |from: &str, to: &str| load_recipe_str(&V1.replace(from, to));
        assert!(matches!(
            with(
                "grade = \"confirmed\"\n        source = \"rule:nmi\"",
                "grade = \"sure\"\n        source = \"rule:nmi\""
            ),
            Err(RecipeError::BadGrade { .. })
        ));
        assert!(matches!(
            with("entry = \"nmi\"", "entry = \"vblank\""),
            Err(RecipeError::BadEntry { .. })
        ));
        assert!(matches!(
            with("slots = 5", "slots = 0"),
            Err(RecipeError::BadFact { .. })
        ));
        assert!(matches!(
            with("class = \"one_way\"", "class = \"lava\""),
            Err(RecipeError::BadFact { .. })
        ));
        assert!(matches!(
            with("last = 0x1f", "last = 0x01"),
            Err(RecipeError::BadFact { .. })
        ));
        assert!(matches!(
            with("source = \"observed: slot flag bytes in 1-1 play\"", ""),
            Err(RecipeError::MissingSource {
                table: "object_table",
                ..
            })
        ));
        assert!(matches!(
            with(
                "tileset = \"grass\"",
                "tileset = \"grass\"\n        speed = 3"
            ),
            Err(RecipeError::Parse(_))
        ));
    }

    #[test]
    fn bad_input_is_refused_with_a_reason() {
        let with = |from: &str, to: &str| load_recipe_str(&GOOD.replace(from, to));
        assert!(matches!(
            with("format = 1", "format = 2"),
            Err(RecipeError::Format { found: 2, .. })
        ));
        assert!(matches!(
            with("confidence = 92", "confidence = 101"),
            Err(RecipeError::BadConfidence { value: 101, .. })
        ));
        assert!(matches!(
            with("confidence = 92", "confidence = -1"),
            Err(RecipeError::Parse(_))
        ));
        assert!(matches!(
            with("\"0x12\"", "\"12\""),
            Err(RecipeError::BadHash { .. })
        ));
        assert!(matches!(
            with("\"0x12\"", "\"0x112233445566778899\""),
            Err(RecipeError::BadHash { .. })
        ));
        assert!(matches!(
            with("[\"entry\"]", "[\"middle\"]"),
            Err(RecipeError::BadHookPoint { .. })
        ));
        assert!(matches!(
            with("source = \"profile memory_map\"", ""),
            Err(RecipeError::MissingSource { table: "ram", .. })
        ));
        assert!(matches!(
            with("name = \"draw\"", "name = \"tick\""),
            Err(RecipeError::DuplicateFunction(_))
        ));
        assert!(matches!(
            with("name = \"lives\"", "name = \"lives\"\nbytes = 3"),
            Err(RecipeError::Parse(_))
        ));
        // Patch rows: a function that is not listed, one with no exact hash
        // (the draw row has none), a value that is not a byte, no source.
        assert!(matches!(
            with(
                "function = \"tick\"\n        offset",
                "function = \"nope\"\n        offset"
            ),
            Err(RecipeError::BadPatch { .. })
        ));
        assert!(matches!(
            with(
                "function = \"tick\"\n        offset",
                "function = \"draw\"\n        offset"
            ),
            Err(RecipeError::BadPatch { .. })
        ));
        assert!(matches!(
            with("value = 0xF0", "value = 256"),
            Err(RecipeError::Parse(_))
        ));
        assert!(matches!(
            with("source = \"observed: test\"", ""),
            Err(RecipeError::MissingSource { table: "patch", .. })
        ));
        assert!(matches!(
            with("name = \"lead\"", "name = \"lead\"\n        old = 1"),
            Err(RecipeError::Parse(_))
        ));
    }
}
