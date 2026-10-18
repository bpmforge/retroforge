//! Schema v0 — versioned Rust structs for `docs/design/GAME_PROFILES.md` §2
//! (FR-PROF-001). Field names/nesting mirror that document's TOML example
//! section-for-section; where a comment below cites a line, it is citing
//! that document, not inventing shape.
//!
//! `SUPPORTED_PROFILE_MAJOR` is this loader's compatibility ceiling for
//! `[meta].profile_version`'s major component (§1: "loader rejects newer
//! majors, warns on unknown keys" — the version check itself lives in
//! `crate::loader`, not here; this module only carries the constant both
//! sides agree on).
//!
//! Everything here is data: no field executes anything (ARCHITECTURE.md §7,
//! "rf-profiles ... must not [contain] executable code (data only; code =
//! plugins)"). `[[mods.patch]]`'s `replace` bytes are a declarative
//! byte-replacement descriptor for some future, separately-ticketed patch
//! applier — this crate does not apply them.

use std::collections::BTreeMap;

use serde::Deserialize;

/// This loader's ceiling for `[meta].profile_version`'s major component.
/// A profile whose major exceeds this is rejected outright (§1).
pub const SUPPORTED_PROFILE_MAJOR: u64 = 0;

/// Console a profile targets (`[meta].console`, §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Console {
    Nes,
    Snes,
}

/// `[capabilities].widescreen` (§2): none | stitched | decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WidescreenMode {
    #[default]
    None,
    Stitched,
    Decoded,
}

/// A full profile, top to bottom matching GAME_PROFILES.md §2's sections.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Profile {
    pub meta: Meta,
    #[serde(default)]
    pub identity: Vec<IdentityEntry>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub memory_map: Vec<MemoryMapEntry>,
    pub camera: Option<Camera>,
    pub entities: Option<Entities>,
    #[serde(default)]
    pub rom_map: Vec<RomMapEntry>,
    pub decode: Option<Decode>,
    pub antiflicker: Option<AntiFlicker>,
    /// `[atmosphere]` — pins the atmosphere-layer heuristic's plane
    /// directly (ticket W16-10).
    pub atmosphere: Option<Atmosphere>,
    /// `[overclock]` — caps the player's Super FX overclock for this game
    /// (ticket W30-01).
    pub overclock: Option<Overclock>,
    /// `[superfx_polygons]` — where a Super FX game keeps its polygon stream,
    /// for the polygon observer (ticket W31-02).
    pub superfx_polygons: Option<SuperfxPolygons>,
    /// `[heights]` — a hand-painted per-tile height table the live-screen
    /// diorama raises its walls from (ticket W30-06).
    pub heights: Option<Heights>,
    pub loading: Option<Loading>,
    pub plugins: Option<Plugins>,
    pub mods: Option<Mods>,
    /// `[widescreen]` — per-BG-layer widescreen policies (ticket W8-05).
    pub widescreen: Option<Widescreen>,
    /// `[scout]` — how the level scout drives a second copy of the machine
    /// to draw the level ahead of the camera (ticket W32-07).
    pub scout: Option<Scout>,
    /// `[scene]` — the RAM bytes that name the level the game is in, so
    /// scenes are keyed by what the game says, not by what the picture looks
    /// like (ticket W33-09).
    pub scene: Option<Scene>,
    /// `[text]` — translation and accessibility overlay declarations
    /// (ticket W8-12).
    pub text: Option<Text>,
}

impl Profile {
    /// The `[[identity]]` entry that matches `identity`'s *normalized*
    /// hashes (FR-PROF-002) — never `.raw`; rf-cart's own doc comment on
    /// `RomIdentity` says normalized is what profiles key on, and a naive
    /// implementation that matched raw could pass a same-content-different-
    /// packaging test while being wrong for every other dump of the game.
    ///
    /// Returns the matched entry, not just whether one matched: FR-PROF-002
    /// ("per-revision overrides; mismatched revisions shall not partially
    /// apply") and GAME_PROFILES.md §1 ("one profile may list multiple
    /// revisions with per-revision address overrides") both require
    /// knowing WHICH revision matched, not just that some entry did — a
    /// bare `bool` would force every caller to re-run this search. Schema
    /// v0 has no per-revision override *fields* yet (that mechanism is not
    /// this ticket's to invent); this method only identifies the matching
    /// entry so a future consumer can build on it.
    pub fn matching_revision(&self, identity: &rf_cart::RomIdentity) -> Option<&IdentityEntry> {
        self.identity
            .iter()
            .find(|e| e.matches(&identity.normalized))
    }

    /// True iff any `[[identity]]` entry matches `identity`'s normalized
    /// hashes. Convenience wrapper over `matching_revision` for callers
    /// (the CLI's `profile hash`) that only need a yes/no answer.
    pub fn matches_rom(&self, identity: &rf_cart::RomIdentity) -> bool {
        self.matching_revision(identity).is_some()
    }
}

/// `[meta]` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Meta {
    pub profile_version: String,
    pub title: String,
    pub console: Console,
    pub region: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    /// SPDX identifier. Required at Phase-9 community-intake CI (§2, D-005)
    /// but not by this loader — schema v0 accepts a first-party profile
    /// without one.
    pub license: Option<String>,
    pub requires: Option<Requires>,
}

/// `[meta.requires]` (§2): optional assertions checked against rf-cart
/// detection at load — the checking itself is out of this ticket's scope
/// (loader/schema/validator only), so these fields are carried, not
/// enforced.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Requires {
    pub mapper: Option<String>,
    #[serde(default)]
    pub chips: Vec<String>,
}

/// One `[[identity]]` row (§2). At least one hash family must be present —
/// enforced by `crate::loader::load_str`, not here, so this type stays a
/// plain data carrier.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct IdentityEntry {
    pub sha256: Option<String>,
    pub sha1: Option<String>,
    pub md5: Option<String>,
    pub crc32: Option<String>,
    pub revision: Option<String>,
}

impl IdentityEntry {
    /// True iff every hash family *this entry specifies* matches `hashes`
    /// case-insensitively (profiles may author hex in any case). An entry
    /// that specifies zero families never matches — defensive independent
    /// of the loader's own "at least one hash" load-time check.
    pub fn matches(&self, hashes: &rf_cart::RomHashes) -> bool {
        let checks: [(&Option<String>, &str); 4] = [
            (&self.sha256, &hashes.sha256),
            (&self.sha1, &hashes.sha1),
            (&self.md5, &hashes.md5),
            (&self.crc32, &hashes.crc32),
        ];
        let mut specified_any = false;
        for (want, have) in checks {
            if let Some(want) = want {
                specified_any = true;
                if !want.eq_ignore_ascii_case(have) {
                    return false;
                }
            }
        }
        specified_any
    }
}

/// `[capabilities]` (§2). Absence of a key means "off" (`serde(default)`
/// on every field, matching the section's role as opt-in UI gates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub full_level: bool,
    #[serde(default)]
    pub hud_separation: bool,
    #[serde(default)]
    pub entity_overlay: bool,
    #[serde(default)]
    pub widescreen: WidescreenMode,
    #[serde(default)]
    pub fast_load: bool,
    #[serde(default)]
    pub smooth_camera: bool,
}

/// One `[[memory_map]]` row (§2), DataCrystal-shaped.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MemoryMapEntry {
    pub addr: u32,
    pub len: u32,
    #[serde(rename = "type")]
    pub ty: String,
    pub label: String,
    pub notes: Option<String>,
    /// FR-PROF-003's clean-room provenance citation. **Required** — see
    /// [`RomMapEntry::source`] for why it is typed `Option` and rejected
    /// by the loader rather than by serde (ticket W4-02a).
    pub source: Option<String>,
    /// Ticket W27-04: how the value reads on screen — added to the raw
    /// number (lives stored minus one: `show_add = 1`).
    #[serde(default)]
    pub show_add: i64,
    /// Ticket W27-04: `"digits"` for one decimal digit per byte, most
    /// significant first (a score kept as digits). Absent: a number.
    pub show: Option<String>,
    /// Ticket W27-04: the value as an index into names (a power-up kept
    /// as 0, 1, 2 ...).
    #[serde(default)]
    pub names: Vec<String>,
    /// Ticket W27-04: the `label` of the row holding this one's maximum
    /// (health against heart containers), drawn as a bar.
    pub max_label: Option<String>,
}

impl MemoryMapEntry {
    /// Ticket W27-04: the value as the game shows it, from this row's
    /// bytes (little-endian, `len` of them, at most four as a number).
    #[must_use]
    pub fn display(&self, bytes: &[u8]) -> String {
        if self.show.as_deref() == Some("digits") {
            return bytes.iter().map(|b| char::from(b'0' + (b % 10))).collect();
        }
        let raw = bytes
            .iter()
            .take(4)
            .enumerate()
            .fold(0i64, |v, (i, b)| v | (i64::from(*b) << (8 * i)));
        if let Some(name) = usize::try_from(raw).ok().and_then(|i| self.names.get(i)) {
            return name.clone();
        }
        (raw + self.show_add).to_string()
    }

    /// Ticket W27-04: the shown value as a number, for a bar.
    #[must_use]
    pub fn number(&self, bytes: &[u8]) -> i64 {
        bytes
            .iter()
            .take(4)
            .enumerate()
            .fold(0i64, |v, (i, b)| v | (i64::from(*b) << (8 * i)))
            + self.show_add
    }
}

/// `[camera]` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Camera {
    pub mode: String,
    pub x: Option<AxisSpec>,
    pub y: Option<AxisSpec>,
    pub hud: Option<HudSpec>,
}

/// `[camera].x` / `[camera].y` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AxisSpec {
    pub addr: u32,
    #[serde(rename = "type")]
    pub ty: String,
    pub scale: Option<i32>,
    pub page: Option<PageSpec>,
}

/// `[camera].x.page` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PageSpec {
    pub addr: u32,
}

/// `[camera].hud` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HudSpec {
    pub region: String,
    #[serde(default)]
    pub scanlines: Vec<u32>,
    pub detect: String,
}

/// `[entities]` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Entities {
    pub table: EntityTable,
    /// Per-game field names (`x`, `y`, `kind`, `active`, ...) are
    /// necessarily freeform — this is why `crate::shape`'s unknown-key
    /// walk treats `entities.fields` as `Shape::Any` rather than flagging
    /// arbitrary game-specific names as unknown keys.
    pub fields: BTreeMap<String, FieldSpec>,
    pub offscreen_valid: bool,
}

/// `[entities].table` (§2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EntityTable {
    pub addr: u32,
    pub stride: u32,
    pub count: u32,
}

/// One `[entities].fields` value: a bare byte offset, or an
/// offset+mask pair (§2: `active = { offset = 15, mask = 0x80 }`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum FieldSpec {
    Offset(u32),
    Masked { offset: u32, mask: u32 },
}

/// One `[[rom_map]]` row (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RomMapEntry {
    pub offset: u32,
    pub len: u32,
    pub label: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub count: Option<u32>,
    /// FR-PROF-003's clean-room provenance citation. **Required** —
    /// `Option` here is a wire-format detail, not permission to omit it:
    /// the loader rejects a profile whose `source` is absent, empty or
    /// whitespace (`ProfileError::MapEntryMissingSource`, ticket W4-02a).
    ///
    /// It is typed `Option<String>` rather than `String` so the failure
    /// is the loader's own named error, naming the table, index and
    /// label, rather than serde's generic "missing field" — which would
    /// point at a line number and leave someone editing a forty-row map
    /// to work out which entry it meant.
    ///
    /// W4-02 shipped this as genuinely optional (its acceptance criteria
    /// did not cite FR-PROF-003) and flagged the gap rather than assuming
    /// it away; W4-02a is that flag being acted on. GAME_PROFILES.md §2's
    /// example omitted the field entirely until then, so the spec's own
    /// example failed the spec's own requirement.
    pub source: Option<String>,
    pub notes: Option<String>,
}

/// `[decode]` (§2). Ships as data only — no decoder runs in this ticket
/// (W5-02a owns `metatile_screens` itself); this type exists to prove the
/// schema can *express* a decode rule, per `fixtures/nes/rf-scroller/
/// FORMAT.md`'s column-RLE metatile shape (`profiles/nes/rf-scroller-demo/
/// profile.toml` mirrors that fixture's real constants).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Decode {
    pub kind: String,
    pub metatile: Option<MetatileSpec>,
    pub screens: Option<ScreensSpec>,
    pub palettes: Option<PalettesSpec>,
    pub collision: Option<CollisionSpec>,
    /// `[decode].room_grid` (ticket W9-08).
    pub room_grid: Option<RoomGridSpec>,
    /// §2's own forward note: schema v0.2 adds `decode.family_version`,
    /// versioned the same way `profile_version` is. Carried here as an
    /// optional string so a v0 loader neither requires nor rejects it.
    pub family_version: Option<String>,
}

/// `[decode].room_grid` (ticket W9-08).
///
/// The second decoder family. Where `metatile_screens` streams
/// column-RLE data left to right, a room-grid game stores a **2-D grid of
/// fixed-size rooms** — the shape Zelda-likes and Metroid-likes use — so
/// the two exercise genuinely different parts of the format rather than
/// being two profiles of the same shape.
///
/// **`index` is the load-bearing optional field.** With it, the grid
/// holds room *numbers* and rooms may repeat; without it, cell N is room
/// N laid out in reading order. Real room games nearly always have the
/// indirection, because reusing one room in eight places is how they fit
/// on the cartridge.
///
/// GAME_PROFILES.md §2 names `room_grid` but never specified it — this
/// struct and `rf_enhance::decode::room_grid` are the specification. §2
/// itself is outside W9-08's write_scope, so documenting the table there
/// is recorded as follow-up rather than done here.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RoomGridSpec {
    /// Grid width, in rooms.
    pub rooms_across: u32,
    /// Grid height, in rooms.
    pub rooms_down: u32,
    /// Tiles across one room.
    pub room_width: u32,
    /// Tiles down one room.
    pub room_height: u32,
    /// `true` when the grid stores room numbers indirected through the
    /// `room_grid_index` table rather than laying rooms out in reading
    /// order.
    #[serde(default)]
    pub indexed: bool,
}

/// `[decode].metatile` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MetatileSpec {
    pub table: u32,
    pub size: u32,
    pub chr_bank_reg: Option<String>,
}

/// `[decode].screens` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScreensSpec {
    pub width: u32,
    pub height: u32,
    pub order: String,
}

/// `[decode].palettes` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PalettesSpec {
    pub table: u32,
    pub per_area: bool,
}

/// `[decode].collision` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CollisionSpec {
    pub table: u32,
    pub bits: String,
}

/// `[antiflicker]` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AntiFlicker {
    pub mode: String,
    #[serde(default)]
    pub exclude_oam: Vec<u32>,
    #[serde(default)]
    pub blink_periods: Vec<u32>,
}

/// The trust-ladder rung `[atmosphere].ladder` pins the heuristic to
/// (ticket W16-10; `ENHANCEMENT_RUNTIME.md` §2a: "Profile `[capabilities]`/
/// `[antiflicker]` entries may pin ladder states"). Mirrors
/// `rf_enhance::trust::TrustState`'s three rungs exactly, as its own
/// spelling (`shadow`/`advisory`/`active`) — this crate cannot depend on
/// `rf-enhance` (ARCHITECTURE.md §3/§6: cores/lower layers never import
/// upward), so the rung is re-declared here and mapped onto
/// `TrustState` by whichever `rf-enhance` function reads it
/// (`rf_enhance::atmosphere::apply_profile_pin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AtmosphereLadder {
    Shadow,
    Advisory,
    Active,
}

/// `[atmosphere]` (§2, ticket W16-10; `ENHANCEMENT_WAVE_16.md` §4): pins a
/// background plane directly as the atmosphere-layer heuristic's plane,
/// skipping `rf_enhance::atmosphere::AtmosphereDetector` the same way
/// `[antiflicker]` is documented (`ENHANCEMENT_RUNTIME.md` §2a) as able to
/// pin ladder state for its own heuristic.
///
/// `plane`'s valid range is per-console (checked in `crate::loader`, not
/// here, since it needs `[meta].console` to decide it): NES has one
/// background plane (0); SNES has up to four (0-3), per `PixelLayer::
/// Background(n)`'s own doc that BG mode decides which of 0-3 exist.
/// `tint`/`strength` are optional cosmetic hints for the fog pass (W16-04);
/// `ladder` defaults to [`AtmosphereLadder::Active`] when absent, since a
/// profile author who bothers to name a plane at all is declaring it a
/// known fact, not a shadow-mode guess -- the opposite default from the
/// heuristic's own fresh-install-shadow rule (D-004), which is why a
/// profile author who instead wants to merely *exercise the schema*
/// without asserting anything must say `ladder = "shadow"` explicitly
/// (see `profiles/snes/rf-scroller-s`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Atmosphere {
    pub plane: u8,
    /// `"#rrggbb"`, validated in `crate::loader`.
    pub tint: Option<String>,
    /// `0.0..=1.0`, validated in `crate::loader`.
    pub strength: Option<f32>,
    pub ladder: Option<AtmosphereLadder>,
}

/// `[overclock]` (§2, tickets W30-01 and W30-08): what the player may do to
/// the game's clocks, and what the speed governor needs to know about the
/// game. Every key is optional; omitted caps mean "uncapped" (the shell's
/// own maximum). Validated in `crate::loader`.
///
/// - `superfx_max` — highest Super FX overclock, 100..=800, step 100.
/// - `cpu_max` / `sa1_max` — highest S-CPU / SA-1 ceiling, 100..=400,
///   step 100 (100 = this game may not be overclocked).
/// - `nes_extra_lines_max` — most extra vblank lines, 0..=240 (0 = none).
/// - `tick_counter` — a WRAM address (`0x7E0000..=0x7FFFFF`) of a byte the
///   game increments once per logic tick; the governor's tick signal.
/// - `target_tick_hz` — the game's normal logic rate, 1..=60; otherwise the
///   governor learns it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Overclock {
    pub superfx_max: Option<u16>,
    pub cpu_max: Option<u16>,
    pub sa1_max: Option<u16>,
    pub nes_extra_lines_max: Option<u16>,
    pub tick_counter: Option<u32>,
    pub target_tick_hz: Option<u32>,
}

/// `[superfx_polygons]` (§2, ticket W31-02): the addresses at which a Super FX
/// game's rasteriser can be *observed* handing the GSU a polygon, found by
/// watching the emulator run the game (no disassembly, no source). All keys
/// are required; ranges are validated in `crate::loader`. See
/// `docs/research/W31-01-starfox-polygon-map.md` for how each was found.
///
/// - `pc_bank` — GSU program bank (`0..=0xFF`) of both code ranges below.
/// - `count_read_pc` — address of the instruction that reads the polygon's
///   vertex count; it starts a polygon.
/// - `count_addr` — GSU RAM address of that count.
/// - `buffer_addr` — GSU RAM address of the clipped-corner work buffer:
///   4-byte entries, x word then y word, only the low byte of each meaningful.
/// - `walker_pcs` — `[start, end)` instruction addresses of the edge walker,
///   whose reads of the buffer say how many corners the polygon has.
/// - `viewport` — `[width, height]` of the 3D view in pixels.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SuperfxPolygons {
    pub pc_bank: u32,
    pub count_read_pc: u32,
    pub count_addr: u32,
    pub buffer_addr: u32,
    pub walker_pcs: Vec<u32>,
    pub viewport: Vec<u32>,
}

/// `[heights]` (§2, ticket W30-06): a per-tile height table the author
/// paints by hand, from which the diorama is raised out of the tilemap that
/// is ON SCREEN right now — for a game with no level decoder, which has no
/// other way to know a wall from a floor. Nothing is inferred from game
/// code; a tile the table does not list is flat ground (height 0).
///
/// - `layer` — the SNES BG layer whose tilemap is read, `1..=4` (default 1).
///   Ignored for NES, which has a single nametable.
/// - `tile_px` — the grid cell a height applies to, `8` or `16` (default 8).
///   `16` groups 2x2 tiles and uses the top-left tile's number.
/// - `tiles` — tile number (the tilemap entry's 10-bit character number, as
///   a hex string key such as `"0x0A4"`) to height in eighths of a tile,
///   `0..=64`.
///
/// Fields are signed so the loader can reject an out-of-range value by
/// naming its key rather than failing with a generic type error.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Heights {
    #[serde(default = "default_heights_layer")]
    pub layer: i64,
    #[serde(default = "default_heights_tile_px")]
    pub tile_px: i64,
    #[serde(default)]
    pub tiles: std::collections::BTreeMap<String, i64>,
}

fn default_heights_layer() -> i64 {
    1
}

fn default_heights_tile_px() -> i64 {
    8
}

/// The largest tile number a tilemap entry can hold (10 bits).
pub const HEIGHTS_MAX_TILE: u32 = 0x3FF;
/// The tallest wall, in eighths of a tile.
pub const HEIGHTS_MAX_EIGHTHS: i64 = 64;

impl Heights {
    /// Parse a `[heights.tiles]` key: a hex number, `0x` prefix optional,
    /// `0..=0x3FF`. `None` for anything else.
    #[must_use]
    pub fn parse_tile_key(key: &str) -> Option<u16> {
        let digits = key
            .strip_prefix("0x")
            .or_else(|| key.strip_prefix("0X"))
            .unwrap_or(key);
        let n = u32::from_str_radix(digits, 16).ok()?;
        (n <= HEIGHTS_MAX_TILE).then_some(n as u16)
    }

    /// The validated table as tile number -> height in eighths. Entries that
    /// fail validation (the loader rejects them, so none reach here from a
    /// loaded profile) are skipped.
    #[must_use]
    pub fn table(&self) -> std::collections::HashMap<u16, u8> {
        self.tiles
            .iter()
            .filter_map(|(k, v)| {
                let tile = Self::parse_tile_key(k)?;
                let eighths = u8::try_from(*v)
                    .ok()
                    .filter(|h| i64::from(*h) <= HEIGHTS_MAX_EIGHTHS)?;
                Some((tile, eighths))
            })
            .collect()
    }
}

/// `[scout]` (ticket W32-07): the recipe for the level scout, a second copy
/// of the machine (restored from a snapshot) that is driven ahead of the
/// camera so the widescreen margins show real level. Every address row needs
/// a `source`, like `[[memory_map]]`; addresses are found BY OBSERVATION.
///
/// - `strategy` — `"walk"` (NES, W32-09: no X pokes, the scout's pad holds
///   the pass's direction so a player who must walk to move the camera does),
///   `"player_move"` (move the player's X words; the game's own
///   follow logic moves the camera and streams the level) or
///   `"camera_write"` (write the camera X words directly; most games snap it
///   back).
/// - `step_px` — pixels per scout frame, `1..=64`.
/// - `frame_budget` — most frames one pass may run, `1..=20000`.
/// - `stall_frames` — a pass ends after this many frames without new level,
///   `1..=3000` (default 120).
/// - `settle_frames` — live frames to wait after a scene is entered
///   (default 90).
/// - `dirs` — passes: `"right"` / `"left"` on a horizontal scene,
///   `"down"` / `"up"` on a vertical one (ticket W33-08; the scout follows
///   the scene's own axis and defaults to right + left, or down + up).
/// - `addr` — the rows: `role = "camera_x"` / `"player_x"` (16-bit
///   little-endian words; `"player_x_hi"` names the high byte when it is not
///   the byte after the `player_x` one, as on the NES games; the NES scout
///   needs no `camera_x`), `role = "camera_y"` / `"player_y"` (the same for a
///   vertical scene; `"player_y_hi"` names the high byte, and a `player_y`
///   with none is a single byte, as Mega Man 2's), `role = "hold"` (a byte
///   with a `value`, written every scout frame; a hold on a `player_y` byte is
///   released on a vertical pass) and `role = "start_when"` (a byte with a `value`
///   the LIVE game must hold before a scout starts, e.g. "player alive").
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Scout {
    pub strategy: String,
    pub step_px: u16,
    pub frame_budget: u32,
    pub stall_frames: Option<u32>,
    pub settle_frames: Option<u32>,
    pub dirs: Option<Vec<String>>,
    /// Ticket W33-06: `true` = try only `strategy`. Without it the scout
    /// starts there and falls back down the ladder (player_move, walk,
    /// camera_write) when the agreement check refuses a rung.
    pub pin: Option<bool>,
    #[serde(default)]
    pub addr: Vec<ScoutAddr>,
}

/// One `[[scout.addr]]` row.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScoutAddr {
    pub role: String,
    pub addr: u32,
    /// Required for `role = "hold"` and `"start_when"`.
    pub value: Option<u8>,
    pub notes: Option<String>,
    pub source: Option<String>,
}

/// `[scene]` (ticket W33-09): how the canvas model tells scenes apart.
///
/// - `[[scene.level_id]]` — rows naming the bytes that identify the level
///   (or room) the game is in. The scene key is the concatenation of every
///   row's bytes; the same bytes are the same scene on every visit, so a pipe
///   room and the level it came from stay two scenes and the level's canvas
///   is intact when the player returns. When EVERY row reads its `idle` value
///   the game is "not in a level" (a title, a map) and the picture decides
///   instead. Addresses are found by observation and each needs a `source`.
/// - `sections` — `"axis"`: a switch of the scroll axis (a horizontal section
///   into a vertical one) starts a new section scene joined to the old one by
///   a continuous edge. Default: off when `level_id` rows exist.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Scene {
    #[serde(default)]
    pub level_id: Vec<LevelId>,
    pub sections: Option<String>,
}

/// One `[[scene.level_id]]` row.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LevelId {
    /// NES `0..=0xFFFF`, SNES `0x7E0000..=0x7FFFFF`.
    pub addr: u32,
    /// Bytes read, little-endian, `1..=4` (default 1).
    pub len: Option<u8>,
    /// What the row reads when the game is not in a level (default 0).
    pub idle: Option<u32>,
    /// Whether this row reading something other than `idle` says a level is
    /// running (default true). `false`: the row's bytes join the key but do
    /// not decide on their own, for a word that holds stale values on the
    /// map but is level-specific inside a level.
    pub decides: Option<bool>,
    pub notes: Option<String>,
    /// Required: where the address was found (FR-PROF-003).
    pub source: Option<String>,
}

/// `[loading]` (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Loading {
    #[serde(default)]
    pub wait_loops: Vec<WaitLoop>,
}

/// One `[[loading.wait_loops]]` row (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WaitLoop {
    pub pc: u32,
    pub until: UntilSpec,
    pub label: String,
}

/// `[[loading.wait_loops]].until` (§2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UntilSpec {
    pub addr: u32,
    pub equals: u32,
}

/// `[plugins]` (§2). Names/version refs only — resolving or loading a
/// plugin is `rf-plugin-sdk`'s job, not this crate's (ARCHITECTURE.md §7).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Plugins {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
}

/// `[mods]` (§2). Declarative byte-replacement descriptors, off by
/// `[widescreen]` (ticket W8-05): per-BG-layer widescreen policies on
/// the bsnes-hd model.
///
/// Every field is optional, and an absent one means "use the built-in
/// default" — bsnes-hd's own `autoHor&Ver` for backgrounds and `safe` for
/// sprites, both of which decline to widen anything they cannot justify.
///
/// **There is deliberately no `enabled` field.** A profile describes what
/// widescreen should look like for this game; the user decides whether to
/// have it at all. That is the same split `[mods]` uses, and it is what
/// keeps law 6's "a fresh install boots in Accuracy Mode" true of a
/// machine running a profile that would rather it were not.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Widescreen {
    pub bg1: Option<String>,
    pub bg2: Option<String>,
    pub bg3: Option<String>,
    pub bg4: Option<String>,
    pub obj: Option<String>,
}

/// default, never applied by this crate.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Mods {
    #[serde(default)]
    pub patch: Vec<ModPatch>,
}

/// One `[[mods.patch]]` row (§2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModPatch {
    pub id: String,
    pub description: String,
    pub addr: u32,
    pub replace: Vec<u8>,
}

/// `[text]` (ticket W8-12): the text an overlay may replace, and what it
/// may be replaced with.
///
/// **There is deliberately no `enabled` field**, for the same reason
/// [`Widescreen`] has none and `[mods]` has none: a profile *describes*
/// what a translation would be, the user decides whether to have one at
/// all. A profile that could switch itself on would make law 6's "a fresh
/// install boots in Accuracy Mode" false on any machine that happened to
/// load it.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct Text {
    /// `[[text.region]]` — where readable text lives on screen.
    #[serde(default, rename = "region")]
    pub regions: Vec<TextRegion>,
    /// `[[text.entry]]` — one original string and its replacements.
    #[serde(default, rename = "entry")]
    pub entries: Vec<TextEntry>,
}

/// One `[[text.region]]` row: a named rectangle of screen text.
///
/// Declared rather than detected. §6 lists "HUD/text detection" as a
/// later AI capability; a profile author naming the box is what makes the
/// feature work today, and it is also what makes it auditable — a human
/// wrote down where the text is, so a wrong overlay is a wrong profile
/// line rather than an opaque misfire.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TextRegion {
    /// Stable name, used by the ledger so an entry says *where* it acted.
    pub id: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One `[[text.entry]]` row: an original string and what may replace it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TextEntry {
    /// The [`TextRegion::id`] this applies to.
    pub region: String,
    /// **The game's own text, verbatim.** Recorded in the profile rather
    /// than only hashed, because criterion 3 requires the replaced text
    /// to stay identifiable: a player must be able to read what the game
    /// said, not just be told that something was swapped.
    pub original: String,
    /// Translated replacement, keyed by language tag (e.g. `en`, `ja`).
    #[serde(default)]
    pub translations: BTreeMap<String, String>,
    /// Accessibility replacement — expanded abbreviations, plain-language
    /// rewording. Separate from a translation because they answer
    /// different needs and a user may want one without the other.
    pub accessible: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FR-PROF-002 alias coverage, family by family, plus the negative
    /// case the vacuity trap explicitly names — a test using one family
    /// would pass under a "match on sha256 only" mutation.
    fn hashes() -> rf_cart::RomHashes {
        rf_cart::RomHashes {
            crc32: "deadbeef".to_string(),
            md5: "0123456789abcdef0123456789abcdef".to_string(),
            sha1: "0123456789abcdef0123456789abcdef01234567".to_string(),
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd".to_string(),
        }
    }

    #[test]
    fn identity_entry_matches_on_sha256_alone() {
        let entry = IdentityEntry {
            sha256: Some(hashes().sha256),
            ..Default::default()
        };
        assert!(entry.matches(&hashes()));
    }

    #[test]
    fn identity_entry_matches_on_sha1_alone() {
        let entry = IdentityEntry {
            sha1: Some(hashes().sha1),
            ..Default::default()
        };
        assert!(entry.matches(&hashes()));
    }

    #[test]
    fn identity_entry_matches_on_md5_alone() {
        let entry = IdentityEntry {
            md5: Some(hashes().md5),
            ..Default::default()
        };
        assert!(entry.matches(&hashes()));
    }

    #[test]
    fn identity_entry_matches_on_crc32_alone() {
        let entry = IdentityEntry {
            crc32: Some(hashes().crc32),
            ..Default::default()
        };
        assert!(entry.matches(&hashes()));
    }

    #[test]
    fn identity_entry_matches_is_case_insensitive() {
        let entry = IdentityEntry {
            crc32: Some("DEADBEEF".to_string()),
            ..Default::default()
        };
        assert!(entry.matches(&hashes()));
    }

    #[test]
    fn identity_entry_negative_case_does_not_match_wrong_rom() {
        let entry = IdentityEntry {
            sha256: Some(hashes().sha256),
            ..Default::default()
        };
        let mut other = hashes();
        other.sha256 = "f".repeat(64);
        assert!(!entry.matches(&other));
    }

    #[test]
    fn identity_entry_with_multiple_hashes_requires_every_one_to_match() {
        let entry = IdentityEntry {
            sha256: Some(hashes().sha256),
            crc32: Some("ffffffff".to_string()), // deliberately wrong
            ..Default::default()
        };
        assert!(!entry.matches(&hashes()));
    }

    #[test]
    fn identity_entry_with_no_hashes_never_matches() {
        let entry = IdentityEntry::default();
        assert!(!entry.matches(&hashes()));
    }

    #[test]
    fn profile_matches_rom_true_when_any_identity_entry_matches() {
        let mut profile = minimal_profile();
        profile.identity = vec![
            IdentityEntry {
                sha256: Some("f".repeat(64)),
                ..Default::default()
            },
            IdentityEntry {
                crc32: Some(hashes().crc32),
                ..Default::default()
            },
        ];
        let identity = rf_cart::RomIdentity {
            normalized: hashes(),
            raw: hashes(),
        };
        assert!(profile.matches_rom(&identity));
    }

    /// FR-PROF-002's "per-revision overrides" clause needs to know WHICH
    /// `[[identity]]` entry matched, not just that one did — this asserts
    /// `matching_revision` returns the correct revision, not merely a
    /// non-`None` result, among several entries where only one matches.
    #[test]
    fn profile_matching_revision_returns_the_entry_that_actually_matched() {
        let mut profile = minimal_profile();
        profile.identity = vec![
            IdentityEntry {
                sha256: Some("f".repeat(64)),
                revision: Some("wrong".to_string()),
                ..Default::default()
            },
            IdentityEntry {
                crc32: Some(hashes().crc32),
                revision: Some("right".to_string()),
                ..Default::default()
            },
        ];
        let identity = rf_cart::RomIdentity {
            normalized: hashes(),
            raw: hashes(),
        };
        let matched = profile
            .matching_revision(&identity)
            .expect("one entry matches");
        assert_eq!(matched.revision.as_deref(), Some("right"));
    }

    #[test]
    fn profile_matches_rom_ignores_raw_and_checks_normalized_only() {
        let mut profile = minimal_profile();
        profile.identity = vec![IdentityEntry {
            sha256: Some(hashes().sha256),
            ..Default::default()
        }];
        let mut raw = hashes();
        raw.sha256 = "f".repeat(64); // raw deliberately wrong
        let identity = rf_cart::RomIdentity {
            normalized: hashes(),
            raw,
        };
        assert!(
            profile.matches_rom(&identity),
            "must match on normalized even when raw differs"
        );
    }

    fn minimal_profile() -> Profile {
        Profile {
            meta: Meta {
                profile_version: "0.1".to_string(),
                title: "t".to_string(),
                console: Console::Nes,
                region: "ntsc".to_string(),
                authors: vec![],
                sources: vec![],
                license: None,
                requires: None,
            },
            identity: vec![],
            capabilities: Capabilities::default(),
            memory_map: vec![],
            camera: None,
            entities: None,
            rom_map: vec![],
            decode: None,
            antiflicker: None,
            atmosphere: None,
            overclock: None,
            superfx_polygons: None,
            heights: None,
            loading: None,
            plugins: None,
            mods: None,
            widescreen: None,
            scout: None,
            scene: None,
            text: None,
        }
    }
}

#[cfg(test)]
mod display_tests {
    use super::MemoryMapEntry;

    fn row(show_add: i64, show: Option<&str>, names: &[&str]) -> MemoryMapEntry {
        MemoryMapEntry {
            addr: 0,
            len: 1,
            ty: "u8".into(),
            label: "x".into(),
            notes: None,
            source: Some("test".into()),
            show_add,
            show: show.map(str::to_owned),
            names: names.iter().map(|s| (*s).to_owned()).collect(),
            max_label: None,
        }
    }

    /// Ticket W27-04: lives stored minus one, a digit score, a power-up.
    #[test]
    fn values_read_the_way_the_game_shows_them() {
        assert_eq!(row(1, None, &[]).display(&[2]), "3");
        assert_eq!(
            row(0, Some("digits"), &[]).display(&[0, 0, 1, 2, 3, 4, 0]),
            "0012340"
        );
        assert_eq!(
            row(0, None, &["Small", "Super", "Fire", "Raccoon"]).display(&[3]),
            "Raccoon"
        );
        assert_eq!(
            row(0, None, &["Small"]).display(&[9]),
            "9",
            "out of range: the number"
        );
        assert_eq!(
            row(0, None, &[]).display(&[0x34, 0x12]),
            "4660",
            "little-endian"
        );
    }
}
