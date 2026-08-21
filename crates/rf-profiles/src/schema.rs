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
    pub loading: Option<Loading>,
    pub plugins: Option<Plugins>,
    pub mods: Option<Mods>,
    /// `[widescreen]` — per-BG-layer widescreen policies (ticket W8-05).
    pub widescreen: Option<Widescreen>,
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
    /// §2's own forward note: schema v0.2 adds `decode.family_version`,
    /// versioned the same way `profile_version` is. Carried here as an
    /// optional string so a v0 loader neither requires nor rejects it.
    pub family_version: Option<String>,
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
            loading: None,
            plugins: None,
            mods: None,
            widescreen: None,
            text: None,
        }
    }
}
