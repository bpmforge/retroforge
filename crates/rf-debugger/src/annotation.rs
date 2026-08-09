//! Annotation store (FR-DBG-005, DEBUGGER.md §4: "Store: addr-space:addr
//! → { label, type, notes, source_url, ... }") — labelled RAM/ROM
//! addresses, the raw material [`crate::profile_export`] turns into a
//! profile skeleton and [`crate::datacrystal`] imports from a TSV
//! transcription.
//!
//! **Narrowed scope note** (this ticket's pre-flight, W4-06b): DEBUGGER.md
//! §4 also describes per-normalized-hash persistence (SQLite or RON) and a
//! `confidence` field. Neither is part of this ticket's four acceptance
//! criteria (persistence/confidence aren't cited by any of them) — this
//! module is the in-memory store + the one structural guarantee the
//! criteria DO require (a required `source`), not the full §4 design.
//! Persistence is a future ticket's to add.
//!
//! Pure data, like every other module in this crate ([`crate`]'s own
//! module doc): no `egui`, no `rf-nes`.

use std::fmt;

/// Which profile table an annotation exports into (GAME_PROFILES.md §2):
/// a RAM-space fact becomes a `[[memory_map]]` row, a ROM-file-offset fact
/// becomes a `[[rom_map]]` row. The two tables' shapes differ slightly
/// (`rom_map` also carries `count`; `memory_map` does not) — see
/// [`crate::profile_export`] for the mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressSpace {
    /// CPU-bus address (WRAM, PRG-RAM, MMIO, ...) — `GAME_PROFILES.md`
    /// §2's `[[memory_map]]`.
    Ram,
    /// ROM file offset — `GAME_PROFILES.md` §2's `[[rom_map]]`.
    Rom,
}

/// One labelled address — the store's unit of record.
///
/// `source` is a plain `String`, not `Option<String>`: FR-DBG-005 ("labelled
/// addresses with a REQUIRED source field") and CONSTRAINTS §2's facts-only
/// transcription policy ("the `source` URL is provenance ... verify facts
/// against the running game where practical") both treat an unsourced
/// annotation as a defect, not a legal-but-incomplete one.
/// [`AnnotationStore::add`] is the enforcement point — see its doc for why
/// the type alone (a `String` that could still be `""`) is not sufficient
/// enforcement on its own.
///
/// `notes` is deliberately the one FREE-PROSE field here, and it is never
/// populated by [`crate::datacrystal`]'s TSV importer (see that module's
/// doc) — CONSTRAINTS §2 requires prose to be written fresh by a human,
/// never transcribed from a wiki table, so the one channel this store has
/// for prose is UI-authored only, not import-populated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub space: AddressSpace,
    /// RAM address (`space == Ram`) or ROM file offset (`space == Rom`).
    pub addr: u32,
    pub len: u32,
    /// Freeform type tag (`GAME_PROFILES.md` §2's `memory_map`/`rom_map`
    /// examples: `"u8"`/`"u16"`/`"ptr_table"`/...) — schema v0 itself
    /// types this as a bare string (`rf_profiles::schema::MemoryMapEntry::
    /// ty`), so this store matches rather than inventing a closed enum the
    /// schema doesn't have.
    pub ty: String,
    pub label: String,
    /// Fresh prose only — see this struct's doc.
    pub notes: Option<String>,
    /// Provenance citation (a URL, a debugger session note, a disassembly
    /// reference — CONSTRAINTS §2). Required; see this struct's doc.
    pub source: String,
    /// `rom_map`-only (`GAME_PROFILES.md` §2's `[[rom_map]].count`) —
    /// meaningless for a `Ram` annotation; callers building one leave this
    /// `None`.
    pub count: Option<u32>,
}

/// Everything [`AnnotationStore::add`] can reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationError {
    /// `source` was empty (or all whitespace) — FR-DBG-005's "required
    /// source field", enforced here rather than left for the exporter to
    /// discover later.
    EmptySource { addr: u32 },
    /// `label` was empty (or all whitespace) — an unlabelled address
    /// annotates nothing.
    EmptyLabel { addr: u32 },
}

impl fmt::Display for AnnotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnnotationError::EmptySource { addr } => {
                write!(
                    f,
                    "annotation at {addr:#06x} has no `source` (required, FR-DBG-005)"
                )
            }
            AnnotationError::EmptyLabel { addr } => {
                write!(f, "annotation at {addr:#06x} has no `label`")
            }
        }
    }
}

impl std::error::Error for AnnotationError {}

/// In-memory annotation store. Insertion order is preserved (matches
/// authoring order, which is what a profile-skeleton export should read
/// back as — GAME_PROFILES.md §3 step 2 doesn't ask for any particular
/// sort).
#[derive(Debug, Clone, Default)]
pub struct AnnotationStore {
    entries: Vec<Annotation>,
}

impl AnnotationStore {
    #[must_use]
    pub fn new() -> Self {
        AnnotationStore::default()
    }

    /// Insert `annotation`, rejecting one with an empty (or
    /// whitespace-only) `source` or `label` — the one place every
    /// annotation enters the store, so this is the structural
    /// enforcement point for FR-DBG-005's "required source field": there
    /// is no other public way to grow [`Self::entries`] (no `pub` field,
    /// no bypass constructor), so once an annotation is inside a store its
    /// `source` is guaranteed non-empty.
    ///
    /// # Errors
    /// [`AnnotationError::EmptySource`]/[`AnnotationError::EmptyLabel`] if
    /// either field is empty or all-whitespace; `annotation` is dropped,
    /// not stored, in that case.
    pub fn add(&mut self, annotation: Annotation) -> Result<(), AnnotationError> {
        if annotation.source.trim().is_empty() {
            return Err(AnnotationError::EmptySource {
                addr: annotation.addr,
            });
        }
        if annotation.label.trim().is_empty() {
            return Err(AnnotationError::EmptyLabel {
                addr: annotation.addr,
            });
        }
        self.entries.push(annotation);
        Ok(())
    }

    /// Every stored annotation, in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[Annotation] {
        &self.entries
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(space: AddressSpace, addr: u32) -> Annotation {
        Annotation {
            space,
            addr,
            len: 2,
            ty: "u16".to_string(),
            label: "player_x".to_string(),
            notes: None,
            source: "https://example.test/ram-map".to_string(),
            count: None,
        }
    }

    #[test]
    fn add_accepts_a_fully_sourced_annotation() {
        let mut store = AnnotationStore::new();
        store.add(valid(AddressSpace::Ram, 0x6029)).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.entries()[0].addr, 0x6029);
    }

    /// Vacuity trap (a)'s store-level half: FR-DBG-005's "required source
    /// field" must actually be enforced, not merely documented. Mutation:
    /// delete the `source.trim().is_empty()` check in `add` and this
    /// fails (the empty-source annotation would be accepted).
    #[test]
    fn add_rejects_an_empty_source() {
        let mut store = AnnotationStore::new();
        let mut a = valid(AddressSpace::Ram, 0x0300);
        a.source = String::new();
        let err = store.add(a).expect_err("empty source must be rejected");
        assert_eq!(err, AnnotationError::EmptySource { addr: 0x0300 });
        assert!(
            store.is_empty(),
            "the rejected annotation must not be stored"
        );
    }

    #[test]
    fn add_rejects_a_whitespace_only_source() {
        let mut store = AnnotationStore::new();
        let mut a = valid(AddressSpace::Ram, 0x0301);
        a.source = "   \t  ".to_string();
        assert!(store.add(a).is_err());
    }

    #[test]
    fn add_rejects_an_empty_label() {
        let mut store = AnnotationStore::new();
        let mut a = valid(AddressSpace::Rom, 0x1000);
        a.label = String::new();
        let err = store.add(a).expect_err("empty label must be rejected");
        assert_eq!(err, AnnotationError::EmptyLabel { addr: 0x1000 });
        assert!(store.is_empty());
    }

    #[test]
    fn entries_preserve_insertion_order() {
        let mut store = AnnotationStore::new();
        store.add(valid(AddressSpace::Ram, 0x0010)).unwrap();
        store.add(valid(AddressSpace::Ram, 0x0002)).unwrap();
        store.add(valid(AddressSpace::Rom, 0x0100)).unwrap();
        let addrs: Vec<u32> = store.entries().iter().map(|e| e.addr).collect();
        assert_eq!(addrs, vec![0x0010, 0x0002, 0x0100]);
    }

    #[test]
    fn new_store_is_empty() {
        assert!(AnnotationStore::new().is_empty());
        assert_eq!(AnnotationStore::new().len(), 0);
    }
}
