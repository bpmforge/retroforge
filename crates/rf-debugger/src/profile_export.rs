//! Annotation store → profile skeleton export (FR-DBG-005,
//! GAME_PROFILES.md §3 step 2: "Debugger exports annotations → profile
//! skeleton (`memory_map`/`rom_map` pre-filled with sources)"). **This is
//! the criterion W5-01 waits on** (ticket brief).
//!
//! ## Why this module has its own `Serialize` structs instead of
//! serializing `rf_profiles::schema::Profile` directly
//!
//! `rf_profiles::schema::Profile` (and its field types) derive only
//! `Deserialize` — schema v0's loader has never needed to write TOML back
//! out (verified against `crates/rf-profiles/src/schema.rs` at this
//! ticket's pre-flight), and `crates/rf-profiles/**` is not in this
//! ticket's `write_scope`, so adding a `Serialize` derive there is not
//! this ticket's to do. The structs below are typed 1:1 against that
//! schema's field names/nesting instead (only the subset GAME_PROFILES.md
//! §3 step 2 actually asks the debugger to pre-fill: `[meta]`,
//! `[[memory_map]]`, `[[rom_map]]` — `[camera]`/`[entities]`/`[decode]`
//! are step 3's "iterate decode rules", hand-authored later, not exported
//! here). To keep the two shapes from silently drifting apart, this
//! module's own test suite round-trips the *actual* emitted TOML text
//! through `rf_profiles::load_str` — the exact function
//! `retroforge-tool profile validate` calls (`tools/retroforge-tool/src/
//! main.rs::run_profile_validate`) — not a hand-rolled "does this look
//! like TOML" check (this ticket's vacuity trap (a)).
//!
//! ## Addresses serialize as decimal, not `0x...`
//!
//! Every hand-authored profile under `/profiles` writes `addr = 0x6029`
//! hex-literal style; `toml`'s `Serialize` for `u32` has no hex-literal
//! mode, so this module's output reads `addr = 24617` instead. Schema v0's
//! loader accepts either form (TOML integers are just integers by the
//! time `toml::Value` sees them — there is no separate "hex" type), so
//! this is a readability difference, not a validity one: a skeleton this
//! module emits still passes `retroforge-tool profile validate`
//! byte-for-byte the same as a hand-written hex one. A human iterating on
//! the exported skeleton (GAME_PROFILES.md §3 step 3) is free to rewrite
//! the literals as hex; this module does not attempt to.
//!
//! ## The `source`-refusal guarantee, at two layers
//!
//! [`crate::annotation::AnnotationStore::add`] already refuses to store an
//! annotation with an empty `source` (that module's doc), so an
//! `AnnotationStore`'s own [`AnnotationStore::entries`] can never contain
//! one. [`export_skeleton`] re-checks anyway, independently of the store
//! (vacuity trap (a): "assert the exporter refuses to emit a sourceless
//! entry") — belt-and-suspenders for any future caller that builds a
//! `&[Annotation]` some other way (e.g. straight from
//! [`crate::datacrystal::parse_tsv`]'s output without ever going through a
//! store).

use serde::Serialize;
use std::fmt;

use crate::annotation::{AddressSpace, Annotation};

/// Schema v0's own compatibility ceiling is major `0`
/// (`rf_profiles::schema::SUPPORTED_PROFILE_MAJOR`); every hand-authored
/// profile under `/profiles` writes `"0.1"`, so this module matches that
/// convention rather than inventing a new one.
const SKELETON_PROFILE_VERSION: &str = "0.1";

/// Console a skeleton targets (`[meta].console`, GAME_PROFILES.md §2) —
/// this module's own copy of `rf_profiles::schema::Console`'s two
/// variants, `Serialize` where that one is `Deserialize`-only (this
/// module's own doc explains why the two can't just share one type).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Console {
    Nes,
    Snes,
}

/// The `[meta]` fields a skeleton export needs from its caller — the
/// annotation store has no opinion on a profile's title/console/region,
/// so these come from whatever ROM/session the debugger currently has
/// open.
#[derive(Debug, Clone)]
pub struct ExportMeta {
    pub title: String,
    pub console: Console,
    pub region: String,
    pub authors: Vec<String>,
}

/// Everything [`export_skeleton`] can reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// An annotation with an empty `source` reached the exporter — see
    /// this module's doc for why this check exists independently of
    /// [`crate::annotation::AnnotationStore::add`]'s own.
    SourcelessEntry { addr: u32 },
    /// An annotation with an empty `label` reached the exporter (same
    /// belt-and-suspenders reasoning as `SourcelessEntry`).
    UnlabelledEntry { addr: u32 },
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::SourcelessEntry { addr } => {
                write!(f, "refusing to export {addr:#06x}: no `source`")
            }
            ExportError::UnlabelledEntry { addr } => {
                write!(f, "refusing to export {addr:#06x}: no `label`")
            }
        }
    }
}

impl std::error::Error for ExportError {}

#[derive(Debug, Clone, Serialize)]
struct MetaDoc {
    profile_version: String,
    title: String,
    console: Console,
    region: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    authors: Vec<String>,
}

/// Mirrors `rf_profiles::schema::MemoryMapEntry` field-for-field (this
/// module's own doc).
#[derive(Debug, Clone, Serialize)]
struct MemoryMapDoc {
    addr: u32,
    len: u32,
    #[serde(rename = "type")]
    ty: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    source: String,
}

/// Mirrors `rf_profiles::schema::RomMapEntry` field-for-field.
#[derive(Debug, Clone, Serialize)]
struct RomMapDoc {
    offset: u32,
    len: u32,
    label: String,
    #[serde(rename = "type")]
    ty: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<u32>,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SkeletonDoc {
    meta: MetaDoc,
    #[serde(rename = "memory_map", skip_serializing_if = "Vec::is_empty")]
    memory_map: Vec<MemoryMapDoc>,
    #[serde(rename = "rom_map", skip_serializing_if = "Vec::is_empty")]
    rom_map: Vec<RomMapDoc>,
}

/// Export `entries` (GAME_PROFILES.md §3 step 2's annotation → skeleton
/// path) into a schema-v0 profile skeleton's TOML text: `[meta]` from
/// `meta`, one `[[memory_map]]` row per `AddressSpace::Ram` entry, one
/// `[[rom_map]]` row per `AddressSpace::Rom` entry — in `entries`' own
/// order, partitioned by space, never reordered within a space.
///
/// # Errors
/// [`ExportError::SourcelessEntry`]/[`ExportError::UnlabelledEntry`] if
/// any entry has an empty `source`/`label` — see this module's doc for
/// why this is checked here even though
/// [`crate::annotation::AnnotationStore::add`] already guarantees it for
/// entries that came from a store.
pub fn export_skeleton(entries: &[Annotation], meta: &ExportMeta) -> Result<String, ExportError> {
    for e in entries {
        if e.source.trim().is_empty() {
            return Err(ExportError::SourcelessEntry { addr: e.addr });
        }
        if e.label.trim().is_empty() {
            return Err(ExportError::UnlabelledEntry { addr: e.addr });
        }
    }

    let memory_map = entries
        .iter()
        .filter(|e| e.space == AddressSpace::Ram)
        .map(|e| MemoryMapDoc {
            addr: e.addr,
            len: e.len,
            ty: e.ty.clone(),
            label: e.label.clone(),
            notes: e.notes.clone(),
            source: e.source.clone(),
        })
        .collect();

    let rom_map = entries
        .iter()
        .filter(|e| e.space == AddressSpace::Rom)
        .map(|e| RomMapDoc {
            offset: e.addr,
            len: e.len,
            label: e.label.clone(),
            ty: e.ty.clone(),
            count: e.count,
            source: e.source.clone(),
            notes: e.notes.clone(),
        })
        .collect();

    let doc = SkeletonDoc {
        meta: MetaDoc {
            profile_version: SKELETON_PROFILE_VERSION.to_string(),
            title: meta.title.clone(),
            console: meta.console,
            region: meta.region.clone(),
            authors: meta.authors.clone(),
        },
        memory_map,
        rom_map,
    };

    // `toml::to_string_pretty` on a well-formed `Serialize` struct built
    // entirely from plain owned `String`/numeric fields cannot itself fail
    // (no floats/maps/unsupported-type edge cases anywhere in `SkeletonDoc`)
    // — `layout::to_toml_string`'s own doc makes the identical claim for
    // the identical reason.
    Ok(toml::to_string_pretty(&doc).expect("SkeletonDoc always serializes to TOML"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::AnnotationStore;

    fn ram_entry(addr: u32, label: &str, source: &str) -> Annotation {
        Annotation {
            space: AddressSpace::Ram,
            addr,
            len: 2,
            ty: "u16".to_string(),
            label: label.to_string(),
            notes: Some("fresh note, not transcribed".to_string()),
            source: source.to_string(),
            count: None,
        }
    }

    fn rom_entry(offset: u32, label: &str, source: &str) -> Annotation {
        Annotation {
            space: AddressSpace::Rom,
            addr: offset,
            len: 4,
            ty: "ptr_table".to_string(),
            label: label.to_string(),
            notes: None,
            source: source.to_string(),
            count: Some(4),
        }
    }

    fn meta() -> ExportMeta {
        ExportMeta {
            title: "W4-06b export test".to_string(),
            console: Console::Nes,
            region: "ntsc".to_string(),
            authors: vec!["RetroForge W4-06b".to_string()],
        }
    }

    /// **Criterion 2, the round-trip through the real validator**
    /// (vacuity trap (a): "an export test that only checks the TOML
    /// parses proves nothing"). `rf_profiles::load_str` is the exact
    /// function `retroforge-tool profile validate` calls — see this
    /// module's doc — not a reimplementation of it.
    #[test]
    fn exported_skeleton_passes_the_real_rf_profiles_loader() {
        let entries = vec![
            ram_entry(0x6029, "player_x", "fixtures/nes/rf-scroller/FORMAT.md"),
            ram_entry(0x602B, "camera_x", "fixtures/nes/rf-scroller/FORMAT.md"),
            rom_entry(
                0x0000,
                "metatile_table",
                "fixtures/nes/rf-scroller/FORMAT.md#metatile-table",
            ),
        ];
        let text = export_skeleton(&entries, &meta()).expect("all entries are sourced");

        let outcome = rf_profiles::load_str(&text).expect("exported skeleton must validate");
        assert!(
            outcome.warnings.is_empty(),
            "exported skeleton must not trip the unknown-key warning: {:?}",
            outcome.warnings
        );
        assert_eq!(outcome.profile.memory_map.len(), 2);
        assert_eq!(outcome.profile.rom_map.len(), 1);

        // Sources must have actually survived the round trip, not just
        // "the file happens to parse" (a `MemoryMapEntry.source` typo like
        // `sourse` would still parse — `type` is `Option<String>` — and
        // only show up as a silently-`None` field plus an unknown-key
        // warning already asserted empty above).
        assert_eq!(
            outcome.profile.memory_map[0].source.as_deref(),
            Some("fixtures/nes/rf-scroller/FORMAT.md")
        );
        assert_eq!(
            outcome.profile.memory_map[1].source.as_deref(),
            Some("fixtures/nes/rf-scroller/FORMAT.md")
        );
        assert_eq!(
            outcome.profile.rom_map[0].source.as_deref(),
            Some("fixtures/nes/rf-scroller/FORMAT.md#metatile-table")
        );
        assert_eq!(outcome.profile.rom_map[0].count, Some(4));
    }

    #[test]
    fn memory_map_and_rom_map_entries_are_partitioned_by_space_not_interleaved() {
        let entries = vec![
            ram_entry(0x0010, "a", "src"),
            rom_entry(0x0020, "b", "src"),
            ram_entry(0x0030, "c", "src"),
        ];
        let text = export_skeleton(&entries, &meta()).unwrap();
        let outcome = rf_profiles::load_str(&text).unwrap();
        let mm_addrs: Vec<u32> = outcome.profile.memory_map.iter().map(|e| e.addr).collect();
        let rm_offsets: Vec<u32> = outcome.profile.rom_map.iter().map(|e| e.offset).collect();
        assert_eq!(mm_addrs, vec![0x0010, 0x0030]);
        assert_eq!(rm_offsets, vec![0x0020]);
    }

    /// Vacuity trap (a)'s exporter-level half: the exporter itself refuses
    /// a sourceless entry, independent of `AnnotationStore::add`'s own
    /// guarantee (this module's doc). Built by hand (not via
    /// `AnnotationStore::add`, which would itself refuse this) to prove
    /// `export_skeleton` does not merely rely on its caller.
    #[test]
    fn export_skeleton_refuses_a_sourceless_entry() {
        let mut bad = ram_entry(0x0042, "label", "will be cleared");
        bad.source = String::new();
        let err = export_skeleton(&[bad], &meta()).expect_err("must refuse");
        assert_eq!(err, ExportError::SourcelessEntry { addr: 0x0042 });
    }

    #[test]
    fn export_skeleton_refuses_an_unlabelled_entry() {
        let mut bad = ram_entry(0x0042, "will be cleared", "source");
        bad.label = String::new();
        let err = export_skeleton(&[bad], &meta()).expect_err("must refuse");
        assert_eq!(err, ExportError::UnlabelledEntry { addr: 0x0042 });
    }

    /// The same guarantee, proven end-to-end from a real `AnnotationStore`
    /// (rather than a hand-built slice): nothing that ever reached
    /// `store.entries()` can be sourceless, because `add` already refused
    /// it — this test documents that composition, not just each half in
    /// isolation.
    #[test]
    fn a_store_that_only_ever_accepted_sourced_annotations_always_exports_cleanly() {
        let mut store = AnnotationStore::new();
        store.add(ram_entry(0x0001, "x", "src-a")).unwrap();
        store.add(rom_entry(0x0002, "y", "src-b")).unwrap();
        assert!(export_skeleton(store.entries(), &meta()).is_ok());
    }

    #[test]
    fn an_empty_annotation_list_still_exports_a_loadable_skeleton() {
        let text = export_skeleton(&[], &meta()).expect("empty list is not an error");
        let outcome = rf_profiles::load_str(&text).expect("must still validate");
        assert!(outcome.profile.memory_map.is_empty());
        assert!(outcome.profile.rom_map.is_empty());
    }
}
