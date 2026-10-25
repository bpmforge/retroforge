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
//! hooks = ["entry", "exit"]            # which sides a hook may attach to
//! source = "observed: NMI vector target of a 1-1 recording"   # required
//!
//! [[ram]]
//! addr = 0x075A
//! width = 1
//! name = "lives"
//! source = "profile.toml memory_map: lives"                   # required
//! ```
//!
//! Unknown keys are an error (the format is new and small; a typo that
//! silently dropped a hash would leave a hook inactive with no explanation).

use std::fmt::Write as _;
use std::path::Path;

use rf_rebuild::{HookPoints, RamMeaning, Recipe, RecipeFunction};
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
    #[serde(default)]
    hooks: Vec<String>,
    source: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RamRow {
    addr: u32,
    #[serde(default = "one")]
    width: u8,
    name: String,
    source: Option<String>,
}

fn one() -> u8 {
    1
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
/// [`RecipeError`] for malformed TOML, a newer format, a bad hash or hook
/// point, a row with no `source`, or a function named twice.
pub fn load_recipe_str(text: &str) -> Result<Recipe, RecipeError> {
    let doc: RecipeDoc = toml::from_str(text).map_err(|e| RecipeError::Parse(e.to_string()))?;
    if doc.recipe.format > RECIPE_FORMAT || doc.recipe.format == 0 {
        return Err(RecipeError::Format {
            found: doc.recipe.format,
            supported: RECIPE_FORMAT,
        });
    }
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
        functions.push(RecipeFunction {
            shape_hash: parse_hash(&row.name, "shape_hash", &row.shape_hash)?,
            exact_hash: row
                .exact_hash
                .as_deref()
                .map(|h| parse_hash(&row.name, "exact_hash", h))
                .transpose()?,
            address_hint: row.address_hint,
            hooks,
            source: source_of("function", &row.name, row.source)?,
            name: row.name,
        });
    }
    let mut ram = Vec::new();
    for row in doc.ram {
        ram.push(RamMeaning {
            addr: row.addr,
            width: row.width,
            source: source_of("ram", &row.name, row.source)?,
            name: row.name,
        });
    }
    Ok(Recipe {
        id: doc.recipe.id,
        engine: doc.recipe.engine,
        functions,
        ram,
    })
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
        let _ = writeln!(out, "\n[[function]]\nname = {}", quote(&f.name));
        let _ = writeln!(out, "shape_hash = \"{:#018x}\"", f.shape_hash);
        if let Some(h) = f.exact_hash {
            let _ = writeln!(out, "exact_hash = \"{h:#018x}\"");
        }
        if let Some(a) = f.address_hint {
            let _ = writeln!(out, "address_hint = {a:#06x}");
        }
        let mut hooks = Vec::new();
        if f.hooks.entry {
            hooks.push("\"entry\"");
        }
        if f.hooks.exit {
            hooks.push("\"exit\"");
        }
        let _ = writeln!(out, "hooks = [{}]", hooks.join(", "));
        let _ = writeln!(out, "source = {}", quote(&f.source));
    }
    for r in &recipe.ram {
        let _ = writeln!(
            out,
            "\n[[ram]]\naddr = {:#06x}\nwidth = {}\nname = {}\nsource = {}",
            r.addr,
            r.width,
            quote(&r.name),
            quote(&r.source)
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

        [[ram]]
        addr = 0x075A
        name = "lives"
        source = "profile memory_map"
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
        assert!(r.functions[1].hooks.entry && r.functions[1].hooks.exit);
        assert_eq!((r.ram[0].addr, r.ram[0].width), (0x75A, 1));
    }

    #[test]
    fn writing_then_reading_is_the_identity() {
        let r = load_recipe_str(GOOD).unwrap();
        assert_eq!(load_recipe_str(&recipe_to_toml(&r)), Ok(r));
    }

    #[test]
    fn bad_input_is_refused_with_a_reason() {
        let with = |from: &str, to: &str| load_recipe_str(&GOOD.replace(from, to));
        assert!(matches!(
            with("format = 1", "format = 2"),
            Err(RecipeError::Format { found: 2, .. })
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
    }
}
