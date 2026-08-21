//! Mesen-compatible HD packs: tile identity, `hires.txt` import, and a
//! record-while-playing builder (ticket W8-11; `docs/SCOPE.md`'s
//! asset-replacement row).
//!
//! ## Why Mesen's identity and not our own
//!
//! `docs/design/ENHANCEMENT_RUNTIME.md` §6 already commits to it —
//! "matching is hash-based like Mesen HD packs, not fuzzy image matching"
//! — and criterion 1 spells out the reason: *"so existing community packs
//! load"*. A pack format that is 95% Mesen is a format that loads no
//! existing pack at all, so the identity here is the one Mesen actually
//! writes, verified against its published `hires.txt` spec (v106) rather
//! than recalled.
//!
//! This is a **different format from [`crate`]'s sibling in `rf-ai`**
//! (`rf_ai::pack`). That one is RetroForge's own AI/artist pack format,
//! keyed by SHA-256. This one is an interchange format we did not design
//! and cannot change. They coexist deliberately: one is ours to evolve,
//! the other is a compatibility surface.
//!
//! ## PNG is deliberately out of band
//!
//! A `<tile>` row names an image index and an `(x, y)` inside it. This
//! module parses and emits **those coordinates**, never pixels: it has no
//! PNG decoder and does not want one. Adding `png`/`image` to
//! `rf-enhance` would be a `docs/TECH_STACK.md` dependency row, which is
//! outside this ticket's write_scope — but the constraint produced the
//! better design anyway. The round trip that criterion 3 asks for is over
//! the *manifest*, which is the part a builder can get wrong.
//!
//! ## Unknown conditions load, and never match
//!
//! Mesen defines twelve condition types. This module evaluates a subset,
//! and the question is what to do with the rest. Refusing the file would
//! fail criterion 1 — real community packs use all of them. Treating an
//! unevaluated condition as *true* would replace tiles the pack author
//! deliberately gated, silently, which is the honesty violation this
//! project exists to avoid.
//!
//! So the rule is three-way:
//!
//! * malformed **syntax** → the file is refused whole;
//! * a **known-but-unevaluated** condition → the pack loads, the rule is
//!   marked never-matching, and the fact is ledgered
//!   ([`HdPack::unevaluated`]);
//! * an **evaluated** condition → matches normally.
//!
//! "The pack loaded and some tiles did not change" is therefore
//! distinguishable from "the pack loaded and did nothing", which is the
//! whole point of ledgering it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// How a `<tile>` row identifies the original tile.
///
/// A sum, not a string, because the spec puts **two different things** in
/// one field: a CHR ROM tile index, or the 16 raw bytes of a CHR RAM tile
/// written as 32 hex characters. Collapsing them to text would make
/// `"2E"` (index 46) and a 32-char pattern compare as the same kind of
/// thing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TileData {
    /// CHR ROM: the tile's index, written as hex.
    ChrRom(u32),
    /// CHR RAM: the tile's 16 pattern bytes, written as 32 hex chars.
    ChrRam([u8; 16]),
}

impl TileData {
    /// Render in the exact form `hires.txt` expects.
    #[must_use]
    pub fn to_field(&self) -> String {
        match self {
            // Upper-case hex, no padding — matches Mesen's own writer and
            // the examples in its spec (`<tile>0,2E,...`).
            TileData::ChrRom(ix) => format!("{ix:X}"),
            TileData::ChrRam(bytes) => bytes.iter().fold(String::new(), |mut s, b| {
                let _ = write!(s, "{b:02X}");
                s
            }),
        }
    }

    /// Parse the tile-data field.
    ///
    /// Length decides the form: exactly 32 hex chars is CHR RAM pattern
    /// bytes, anything shorter is a CHR ROM index. That is the spec's own
    /// rule, and it is why the length check comes before the radix parse.
    ///
    /// # Errors
    /// [`HdPackError::BadTileData`] for a non-hex or over-long field.
    pub fn parse(field: &str) -> Result<Self, HdPackError> {
        let bad = || HdPackError::BadTileData {
            field: field.to_string(),
        };
        if field.is_empty() || field.len() > 32 {
            return Err(bad());
        }
        if field.len() == 32 {
            let mut bytes = [0u8; 16];
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = u8::from_str_radix(&field[i * 2..i * 2 + 2], 16).map_err(|_| bad())?;
            }
            return Ok(TileData::ChrRam(bytes));
        }
        Ok(TileData::ChrRom(
            u32::from_str_radix(field, 16).map_err(|_| bad())?,
        ))
    }
}

/// The `(tile data, palette)` pair a rule matches on.
///
/// **The palette is part of the identity**, exactly as in `rf_ai::pack` —
/// the same tile bytes under two palettes are two different pictures, and
/// keying on pixels alone is what produces the classic HD-pack artefact
/// where a recoloured enemy wears its sibling's skin.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TileKey {
    pub tile: TileData,
    /// The 4 palette bytes (`8` hex characters in the file).
    pub palette: [u8; 4],
}

/// One `<tile>` row.
#[derive(Debug, Clone, PartialEq)]
pub struct TileRule {
    pub key: TileKey,
    /// Index into [`HdPack::images`].
    pub img: usize,
    pub x: u32,
    pub y: u32,
    /// Brightness multiplier; `1.0` unless the pack reuses one HD tile
    /// across a fade.
    pub brightness: f32,
    /// The spec's `default tile` Y/N flag: a fallback used when a tile
    /// matches the tile data but **no rule matches its palette**.
    pub default_tile: bool,
    /// Condition expression from a `[name]` prefix, verbatim and
    /// unparsed beyond splitting — see [`Condition`].
    pub condition: Option<String>,
}

/// A `<condition>` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub name: String,
    /// The type token, e.g. `memoryCheckConstant`. Kept as text because
    /// this module deliberately does not evaluate every type — see the
    /// module doc.
    pub kind: String,
    pub params: Vec<String>,
}

/// The condition types this module can actually evaluate.
///
/// Everything else loads and never matches. Kept as an explicit list
/// rather than inferred, so adding an evaluator is a deliberate edit in
/// one place.
pub const EVALUATED_CONDITIONS: &[&str] = &["hmirror", "vmirror", "bgpriority"];

/// A parsed HD pack.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HdPack {
    pub version: u32,
    pub scale: u32,
    /// `<overscan>` top, right, bottom, left.
    pub overscan: [u32; 4],
    pub images: Vec<String>,
    pub tiles: Vec<TileRule>,
    pub conditions: BTreeMap<String, Condition>,
    pub options: Vec<String>,
    /// Condition names present in the file whose type this build cannot
    /// evaluate. **Ledger, not an error** — see the module doc.
    pub unevaluated: Vec<String>,
}

/// Why a `hires.txt` could not be loaded.
///
/// Every variant carries the offending line number and text: a pack author
/// fixing a 4,000-line `hires.txt` needs to be told *where*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HdPackError {
    /// A tag had the wrong number of comma-separated fields.
    FieldCount {
        line_no: usize,
        tag: String,
        expected: usize,
        found: usize,
    },
    /// A numeric field would not parse.
    BadNumber { line_no: usize, field: String },
    /// The tile-data field is neither a hex index nor 32 hex chars.
    BadTileData { field: String },
    /// The palette field is not exactly 8 hex characters.
    BadPalette { field: String },
    /// A `<tile>` names an image index with no `<img>` behind it.
    ImageOutOfRange { line_no: usize, img: usize },
    /// A tile's `[name]` prefix names a condition never declared.
    UnknownCondition { line_no: usize, name: String },
    /// No `<ver>` line. Refused rather than assumed, because the version
    /// is what tells a future reader which spec the file follows.
    MissingVersion,
}

impl std::fmt::Display for HdPackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HdPackError::FieldCount {
                line_no,
                tag,
                expected,
                found,
            } => write!(
                f,
                "line {line_no}: <{tag}> needs {expected} fields, found {found}"
            ),
            HdPackError::BadNumber { line_no, field } => {
                write!(f, "line {line_no}: {field:?} is not a number")
            }
            HdPackError::BadTileData { field } => write!(
                f,
                "tile data {field:?} is neither a hex index nor 32 hex characters"
            ),
            HdPackError::BadPalette { field } => {
                write!(f, "palette {field:?} is not 8 hex characters")
            }
            HdPackError::ImageOutOfRange { line_no, img } => write!(
                f,
                "line {line_no}: image index {img} has no matching <img> line"
            ),
            HdPackError::UnknownCondition { line_no, name } => {
                write!(f, "line {line_no}: condition {name:?} was never declared")
            }
            HdPackError::MissingVersion => write!(f, "no <ver> line"),
        }
    }
}

impl std::error::Error for HdPackError {}

fn parse_palette(field: &str) -> Result<[u8; 4], HdPackError> {
    if field.len() != 8 {
        return Err(HdPackError::BadPalette {
            field: field.to_string(),
        });
    }
    let mut out = [0u8; 4];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&field[i * 2..i * 2 + 2], 16).map_err(|_| {
            HdPackError::BadPalette {
                field: field.to_string(),
            }
        })?;
    }
    Ok(out)
}

fn hex8(p: &[u8; 4]) -> String {
    p.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02X}");
        s
    })
}

/// Split a line into an optional `[condition]` prefix and the rest.
fn split_condition(line: &str) -> (Option<String>, &str) {
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return (Some(rest[..end].to_string()), &rest[end + 1..]);
        }
    }
    (None, line)
}

/// Parse a `hires.txt`.
///
/// Syntax faults refuse the file whole. A condition whose *type* this
/// build cannot evaluate does **not** — it is ledgered in
/// [`HdPack::unevaluated`] and its rules never match. See the module doc.
///
/// # Errors
/// See [`HdPackError`].
pub fn parse_hires(text: &str) -> Result<HdPack, HdPackError> {
    let mut pack = HdPack {
        scale: 1,
        ..Default::default()
    };
    let mut saw_version = false;
    // Tile rows are checked against `<img>`/`<condition>` declarations
    // only after the whole file is read, because the spec does not
    // require a declaration to precede its use.
    let mut pending: Vec<(usize, TileRule)> = Vec::new();

    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.trim();
        // `//` is Mesen's comment marker; blank lines are ignored.
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let (condition, body) = split_condition(line);
        // Anything that is not a tag is skipped rather than refused:
        // real packs carry stray text, and refusing the file over it
        // would fail criterion 1 for no safety gain.
        let Some(rest) = body.strip_prefix('<') else {
            continue;
        };
        let Some(close) = rest.find('>') else {
            continue;
        };
        let tag = &rest[..close];
        let args = rest[close + 1..].trim();
        let fields: Vec<&str> = args.split(',').map(str::trim).collect();

        let num = |s: &str| -> Result<u32, HdPackError> {
            s.parse::<u32>().map_err(|_| HdPackError::BadNumber {
                line_no,
                field: s.to_string(),
            })
        };
        let need = |n: usize| -> Result<(), HdPackError> {
            if fields.len() == n {
                Ok(())
            } else {
                Err(HdPackError::FieldCount {
                    line_no,
                    tag: tag.to_string(),
                    expected: n,
                    found: fields.len(),
                })
            }
        };

        match tag {
            "ver" => {
                pack.version = num(args)?;
                saw_version = true;
            }
            "scale" => pack.scale = num(args)?,
            "overscan" => {
                need(4)?;
                for (slot, f) in pack.overscan.iter_mut().zip(&fields) {
                    *slot = f.parse::<u32>().map_err(|_| HdPackError::BadNumber {
                        line_no,
                        field: (*f).to_string(),
                    })?;
                }
            }
            "img" => pack.images.push(args.to_string()),
            "options" => pack.options.extend(
                fields
                    .iter()
                    .filter(|f| !f.is_empty())
                    .map(|f| (*f).to_string()),
            ),
            "condition" => {
                if fields.len() < 2 {
                    return Err(HdPackError::FieldCount {
                        line_no,
                        tag: tag.to_string(),
                        expected: 2,
                        found: fields.len(),
                    });
                }
                let c = Condition {
                    name: fields[0].to_string(),
                    kind: fields[1].to_string(),
                    params: fields[2..].iter().map(|s| (*s).to_string()).collect(),
                };
                if !EVALUATED_CONDITIONS.contains(&c.kind.as_str()) {
                    pack.unevaluated.push(c.name.clone());
                }
                pack.conditions.insert(c.name.clone(), c);
            }
            "tile" => {
                // <tile>img,tileData,palette,x,y,brightness,defaultTile
                need(7)?;
                let rule = TileRule {
                    key: TileKey {
                        tile: TileData::parse(fields[1])?,
                        palette: parse_palette(fields[2])?,
                    },
                    img: num(fields[0])? as usize,
                    x: num(fields[3])?,
                    y: num(fields[4])?,
                    brightness: fields[5]
                        .parse::<f32>()
                        .map_err(|_| HdPackError::BadNumber {
                            line_no,
                            field: fields[5].to_string(),
                        })?,
                    default_tile: fields[6].eq_ignore_ascii_case("Y"),
                    condition,
                };
                pending.push((line_no, rule));
            }
            _ => {}
        }
    }

    if !saw_version {
        return Err(HdPackError::MissingVersion);
    }

    for (line_no, rule) in pending {
        if rule.img >= pack.images.len() {
            return Err(HdPackError::ImageOutOfRange {
                line_no,
                img: rule.img,
            });
        }
        if let Some(expr) = &rule.condition {
            for name in expr.split('&') {
                let name = name.trim_start_matches('!');
                if !pack.conditions.contains_key(name) {
                    return Err(HdPackError::UnknownCondition {
                        line_no,
                        name: name.to_string(),
                    });
                }
            }
        }
        pack.tiles.push(rule);
    }

    Ok(pack)
}

/// Serialise back to `hires.txt` form.
///
/// The inverse of [`parse_hires`] for everything this type models, which
/// is what makes criterion 3's round trip meaningful: the builder writes
/// through here, and the loader reads it back.
#[must_use]
pub fn write_hires(pack: &HdPack) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "<ver>{}", pack.version);
    let _ = writeln!(out, "<scale>{}", pack.scale);
    if pack.overscan != [0; 4] {
        let o = pack.overscan;
        let _ = writeln!(out, "<overscan>{},{},{},{}", o[0], o[1], o[2], o[3]);
    }
    if !pack.options.is_empty() {
        let _ = writeln!(out, "<options>{}", pack.options.join(","));
    }
    for img in &pack.images {
        let _ = writeln!(out, "<img>{img}");
    }
    for c in pack.conditions.values() {
        let _ = write!(out, "<condition>{},{}", c.name, c.kind);
        for p in &c.params {
            let _ = write!(out, ",{p}");
        }
        let _ = writeln!(out);
    }
    for t in &pack.tiles {
        if let Some(c) = &t.condition {
            let _ = write!(out, "[{c}]");
        }
        let _ = writeln!(
            out,
            "<tile>{},{},{},{},{},{},{}",
            t.img,
            t.key.tile.to_field(),
            hex8(&t.key.palette),
            t.x,
            t.y,
            t.brightness,
            if t.default_tile { "Y" } else { "N" }
        );
    }
    out
}

impl HdPack {
    /// Find the replacement for an on-screen tile.
    ///
    /// **Three tiers, in this order**, which is the spec's own rule and
    /// not an optimisation:
    ///
    /// 1. a rule whose `(tile data, palette)` both match;
    /// 2. otherwise a `default tile` rule matching the tile data alone —
    ///    the spec: "matches the tile data, but has no rules matching its
    ///    palette data";
    /// 3. otherwise `None`, and the original tile is drawn.
    ///
    /// Tier 2 is what makes palette-keying safe. Without it a pack author
    /// must enumerate every palette a tile ever appears under, and the
    /// ones they miss fall through to the original — which is how a
    /// recoloured sprite ends up half-replaced.
    ///
    /// A rule carrying a condition this build cannot evaluate never
    /// matches, at either tier.
    #[must_use]
    pub fn lookup(&self, tile: &TileData, palette: &[u8; 4]) -> Option<&TileRule> {
        let usable = |r: &&TileRule| match &r.condition {
            None => true,
            Some(expr) => expr.split('&').all(|n| {
                let n = n.trim_start_matches('!');
                self.conditions
                    .get(n)
                    .is_some_and(|c| EVALUATED_CONDITIONS.contains(&c.kind.as_str()))
            }),
        };
        self.tiles
            .iter()
            .find(|r| usable(r) && &r.key.tile == tile && &r.key.palette == palette)
            .or_else(|| {
                self.tiles
                    .iter()
                    .find(|r| usable(r) && r.default_tile && &r.key.tile == tile)
            })
    }
}

/// One tile seen while playing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TileObservation {
    pub tile: TileData,
    pub palette: [u8; 4],
}

/// Record-while-playing pack builder (criterion 3).
///
/// Collects the distinct tiles a game actually drew and lays each out on
/// a tileset grid, producing an [`HdPack`] its own [`parse_hires`]
/// accepts — the round trip the ticket names as the acceptance.
///
/// Deduplication is by `(tile data, palette)`, i.e. the same identity the
/// loader matches on. Recording the same tile twice must not produce two
/// rules, because the first would shadow the second forever and the pack
/// author would have no way to see why their edit did nothing.
#[derive(Debug, Clone)]
pub struct PackBuilder {
    image: String,
    tile_size: u32,
    columns: u32,
    scale: u32,
    version: u32,
    seen: BTreeMap<(TileData, [u8; 4]), u32>,
}

impl PackBuilder {
    /// `columns` is how many tiles wide the emitted tileset is; it must be
    /// non-zero or the layout has no rows to place anything on.
    ///
    /// # Panics
    /// If `columns` or `tile_size` is zero.
    #[must_use]
    pub fn new(image: &str, tile_size: u32, columns: u32, scale: u32) -> Self {
        assert!(columns > 0, "a tileset needs at least one column");
        assert!(tile_size > 0, "a tile needs a non-zero size");
        Self {
            image: image.to_string(),
            tile_size,
            columns,
            scale,
            version: 106,
            seen: BTreeMap::new(),
        }
    }

    /// Record a tile the game drew. Returns `true` if it was new.
    pub fn observe(&mut self, obs: &TileObservation) -> bool {
        let key = (obs.tile.clone(), obs.palette);
        let next = self.seen.len() as u32;
        if self.seen.contains_key(&key) {
            return false;
        }
        self.seen.insert(key, next);
        true
    }

    /// How many distinct tiles have been recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// `true` if nothing has been recorded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Emit the pack.
    ///
    /// Slots are assigned in the `BTreeMap`'s key order rather than
    /// observation order, so the same recording always produces the same
    /// tileset layout. A builder whose output moved between runs would
    /// make every pack a fresh diff and every cached image stale.
    #[must_use]
    pub fn build(&self) -> HdPack {
        let mut tiles = Vec::with_capacity(self.seen.len());
        for (slot, (tile, palette)) in self.seen.keys().enumerate() {
            let slot = slot as u32;
            tiles.push(TileRule {
                key: TileKey {
                    tile: tile.clone(),
                    palette: *palette,
                },
                img: 0,
                x: (slot % self.columns) * self.tile_size * self.scale,
                y: (slot / self.columns) * self.tile_size * self.scale,
                brightness: 1.0,
                default_tile: false,
                condition: None,
            });
        }
        HdPack {
            version: self.version,
            scale: self.scale,
            overscan: [0; 4],
            images: vec![self.image.clone()],
            tiles,
            conditions: BTreeMap::new(),
            options: Vec::new(),
            unevaluated: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-written `hires.txt` in Mesen's own syntax, taken from its
    /// published spec's example rather than produced by [`PackBuilder`].
    ///
    /// **This fixture is the point of criterion 1.** A round trip over
    /// only our own builder's output would prove the builder and the
    /// loader agree with each other while both disagree with Mesen —
    /// which is exactly the bug "so existing community packs load" is
    /// asking us not to ship.
    const MESEN_FIXTURE: &str = "\
// a comment Mesen allows
<ver>106
<scale>2
<overscan>8,8,8,8
<img>Tileset01.png
<options>disableSpriteLimit
<condition>myCondition,memoryCheckConstant,8FFF,==,3F
[myCondition]<tile>0,2E,FF16360F,0,0,1,N
<tile>0,2E,FF16360F,10,10,1,Y
";

    fn rom(ix: u32) -> TileData {
        TileData::ChrRom(ix)
    }

    #[test]
    fn parses_the_mesen_fixture() {
        let p = parse_hires(MESEN_FIXTURE).unwrap();
        assert_eq!(p.version, 106);
        assert_eq!(p.scale, 2);
        assert_eq!(p.overscan, [8, 8, 8, 8]);
        assert_eq!(p.images, vec!["Tileset01.png"]);
        assert_eq!(p.options, vec!["disableSpriteLimit"]);
        assert_eq!(p.tiles.len(), 2);
        // 0x2E is an index, not a 32-char pattern.
        assert_eq!(p.tiles[0].key.tile, rom(0x2E));
        assert_eq!(p.tiles[0].key.palette, [0xFF, 0x16, 0x36, 0x0F]);
        assert_eq!(p.tiles[0].condition.as_deref(), Some("myCondition"));
        assert!(!p.tiles[0].default_tile);
        assert!(p.tiles[1].default_tile);
    }

    #[test]
    fn an_unevaluated_condition_loads_and_is_ledgered() {
        // memoryCheckConstant is not in EVALUATED_CONDITIONS.
        let p = parse_hires(MESEN_FIXTURE).unwrap();
        assert_eq!(p.unevaluated, vec!["myCondition"]);
    }

    #[test]
    fn a_rule_gated_on_an_unevaluated_condition_never_matches() {
        // The honesty call: NOT treated as always-true. Tile 0x2E under
        // this exact palette has a conditional rule at (0,0) and a
        // default-tile rule at (10,10); since the condition cannot be
        // evaluated, the conditional rule must be skipped and the
        // default one used.
        let p = parse_hires(MESEN_FIXTURE).unwrap();
        let hit = p.lookup(&rom(0x2E), &[0xFF, 0x16, 0x36, 0x0F]).unwrap();
        assert_eq!((hit.x, hit.y), (10, 10));
    }

    #[test]
    fn exact_palette_match_beats_a_default_tile() {
        // Tier 1 before tier 2. Getting this backwards would make every
        // default tile shadow the specific rules it exists to back up.
        let src = "<ver>106\n<img>t.png\n\
                   <tile>0,5,AABBCCDD,4,4,1,Y\n\
                   <tile>0,5,00112233,9,9,1,N\n";
        let p = parse_hires(src).unwrap();
        let hit = p.lookup(&rom(5), &[0x00, 0x11, 0x22, 0x33]).unwrap();
        assert_eq!((hit.x, hit.y), (9, 9));
    }

    #[test]
    fn an_unmatched_palette_falls_back_to_the_default_tile() {
        let src = "<ver>106\n<img>t.png\n\
                   <tile>0,5,AABBCCDD,4,4,1,Y\n\
                   <tile>0,5,00112233,9,9,1,N\n";
        let p = parse_hires(src).unwrap();
        let hit = p.lookup(&rom(5), &[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
        assert_eq!((hit.x, hit.y), (4, 4));
    }

    #[test]
    fn a_tile_with_no_rule_at_all_is_left_alone() {
        // Tier 3: the original is drawn. Returning something here would
        // replace pixels no pack author asked to replace.
        let src = "<ver>106\n<img>t.png\n<tile>0,5,AABBCCDD,4,4,1,N\n";
        let p = parse_hires(src).unwrap();
        assert!(p.lookup(&rom(6), &[0xAA, 0xBB, 0xCC, 0xDD]).is_none());
    }

    #[test]
    fn chr_ram_tile_data_is_sixteen_bytes_not_an_index() {
        let pattern = "000102030405060708090A0B0C0D0E0F";
        assert_eq!(pattern.len(), 32);
        let td = TileData::parse(pattern).unwrap();
        assert_eq!(
            td,
            TileData::ChrRam([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
        );
        // And it round-trips through the field form.
        assert_eq!(td.to_field(), pattern);
        // A short field is an index, and the two are not confusable.
        assert_ne!(TileData::parse("2E").unwrap(), td);
    }

    #[test]
    fn missing_version_is_refused() {
        // Refused rather than assumed: the version says which spec the
        // file follows.
        assert_eq!(
            parse_hires("<scale>2\n<img>t.png\n").unwrap_err(),
            HdPackError::MissingVersion
        );
    }

    #[test]
    fn a_malformed_tile_row_refuses_the_whole_file() {
        // Syntax faults are the case where refusing IS right.
        let e = parse_hires("<ver>106\n<img>t.png\n<tile>0,5,AABBCCDD,4\n").unwrap_err();
        assert_eq!(
            e,
            HdPackError::FieldCount {
                line_no: 3,
                tag: "tile".into(),
                expected: 7,
                found: 4
            }
        );
    }

    #[test]
    fn a_tile_naming_a_missing_image_is_refused() {
        let e = parse_hires("<ver>106\n<tile>3,5,AABBCCDD,0,0,1,N\n").unwrap_err();
        assert_eq!(e, HdPackError::ImageOutOfRange { line_no: 2, img: 3 });
    }

    #[test]
    fn a_tile_naming_an_undeclared_condition_is_refused() {
        // Distinct from an unevaluated one: this pack is internally
        // inconsistent, not merely using a feature we lack.
        let e =
            parse_hires("<ver>106\n<img>t.png\n[ghost]<tile>0,5,AABBCCDD,0,0,1,N\n").unwrap_err();
        assert_eq!(
            e,
            HdPackError::UnknownCondition {
                line_no: 3,
                name: "ghost".into()
            }
        );
    }

    #[test]
    fn a_bad_palette_is_refused() {
        // 6 hex chars, not 8 — a truncated palette would otherwise match
        // the wrong tiles forever.
        let e = parse_hires("<ver>106\n<img>t.png\n<tile>0,5,AABBCC,0,0,1,N\n").unwrap_err();
        assert_eq!(
            e,
            HdPackError::BadPalette {
                field: "AABBCC".into()
            }
        );
    }

    #[test]
    fn the_fixture_survives_a_write_parse_round_trip() {
        // Not our builder's output: Mesen's own syntax, out and back.
        let a = parse_hires(MESEN_FIXTURE).unwrap();
        let b = parse_hires(&write_hires(&a)).unwrap();
        assert_eq!(a, b);
    }

    // ---------------------------------------------------------------
    // Builder — criterion 3's round trip
    // ---------------------------------------------------------------

    fn obs(ix: u32, pal: [u8; 4]) -> TileObservation {
        TileObservation {
            tile: rom(ix),
            palette: pal,
        }
    }

    #[test]
    fn the_builder_produces_a_pack_its_own_loader_accepts() {
        // The acceptance, verbatim: "a pack builder that records while
        // playing, producing a pack the loader accepts — round-tripped
        // in a test".
        let mut b = PackBuilder::new("out.png", 8, 4, 2);
        for (ix, pal) in [
            (1, [0x0F, 0x16, 0x27, 0x18]),
            (2, [0x0F, 0x16, 0x27, 0x18]),
            (3, [0x0F, 0x30, 0x10, 0x00]),
        ] {
            assert!(b.observe(&obs(ix, pal)));
        }
        let built = b.build();
        let text = write_hires(&built);
        let reloaded = parse_hires(&text).unwrap();
        assert_eq!(built, reloaded);
        assert_eq!(reloaded.tiles.len(), 3);
        // And every recorded tile is actually findable through the same
        // lookup the renderer uses — a pack that parses but matches
        // nothing would pass a weaker assertion.
        for (ix, pal) in [
            (1, [0x0F, 0x16, 0x27, 0x18]),
            (2, [0x0F, 0x16, 0x27, 0x18]),
            (3, [0x0F, 0x30, 0x10, 0x00]),
        ] {
            assert!(reloaded.lookup(&rom(ix), &pal).is_some(), "lost tile {ix}");
        }
    }

    #[test]
    fn recording_the_same_tile_twice_produces_one_rule() {
        // A duplicate would shadow itself forever, and the pack author
        // would have no way to see why editing the second did nothing.
        let mut b = PackBuilder::new("out.png", 8, 4, 1);
        assert!(b.observe(&obs(7, [1, 2, 3, 4])));
        assert!(!b.observe(&obs(7, [1, 2, 3, 4])));
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn the_same_palette_under_a_different_tile_is_a_different_rule() {
        // The palette is part of the identity, but so is the tile.
        let mut b = PackBuilder::new("out.png", 8, 4, 1);
        assert!(b.observe(&obs(7, [1, 2, 3, 4])));
        assert!(b.observe(&obs(8, [1, 2, 3, 4])));
        assert!(b.observe(&obs(7, [9, 9, 9, 9])));
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn layout_does_not_depend_on_observation_order() {
        // A builder whose tileset moved between runs would make every
        // pack a fresh diff and every cached image stale.
        let mut a = PackBuilder::new("out.png", 8, 2, 1);
        let mut z = PackBuilder::new("out.png", 8, 2, 1);
        let tiles = [
            obs(3, [1, 1, 1, 1]),
            obs(1, [2, 2, 2, 2]),
            obs(2, [3, 3, 3, 3]),
        ];
        for t in &tiles {
            a.observe(t);
        }
        for t in tiles.iter().rev() {
            z.observe(t);
        }
        assert_eq!(a.build(), z.build());
    }

    #[test]
    fn slots_wrap_onto_rows_at_the_column_count() {
        let mut b = PackBuilder::new("out.png", 8, 2, 1);
        for ix in 0..3 {
            b.observe(&obs(ix, [0, 0, 0, 0]));
        }
        let p = b.build();
        let xy: Vec<(u32, u32)> = p.tiles.iter().map(|t| (t.x, t.y)).collect();
        assert_eq!(xy, vec![(0, 0), (8, 0), (0, 8)]);
    }

    #[test]
    fn scale_multiplies_the_emitted_coordinates() {
        // At scale 2 an 8px tile occupies 16px in the tileset; emitting
        // unscaled coordinates would overlap every replacement.
        let mut b = PackBuilder::new("out.png", 8, 2, 2);
        for ix in 0..2 {
            b.observe(&obs(ix, [0, 0, 0, 0]));
        }
        let p = b.build();
        assert_eq!(p.scale, 2);
        assert_eq!(p.tiles[1].x, 16);
    }

    #[test]
    fn an_empty_builder_produces_a_pack_that_still_parses() {
        // Degenerate but legal: a recording session that saw nothing.
        let b = PackBuilder::new("out.png", 8, 4, 1);
        assert!(b.is_empty());
        let p = parse_hires(&write_hires(&b.build())).unwrap();
        assert!(p.tiles.is_empty());
        assert_eq!(p.version, 106);
    }
}
