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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    /// One stored annotation by index, or `None` if the index is stale.
    ///
    /// Indices are how the UI addresses a row, and a panel can hold one
    /// across a repaint in which the store shrank — so this returns
    /// `Option` rather than indexing, and every mutator below does the
    /// same. A debugger that panics because a list got shorter is worse
    /// than one that does nothing.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Annotation> {
        self.entries.get(index)
    }

    /// Replace the annotation at `index`, re-checking the same invariants
    /// [`Self::add`] enforces.
    ///
    /// Editing goes through the same gate as insertion deliberately:
    /// otherwise an edit could blank a `source` that `add` would have
    /// refused, and FR-DBG-005's guarantee would hold only for annotations
    /// nobody had touched since.
    ///
    /// # Errors
    /// [`AnnotationError::EmptySource`]/[`AnnotationError::EmptyLabel`] as
    /// [`Self::add`]; the stored annotation is left unchanged in that case.
    pub fn replace(&mut self, index: usize, annotation: Annotation) -> Result<(), AnnotationError> {
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
        if let Some(slot) = self.entries.get_mut(index) {
            *slot = annotation;
        }
        Ok(())
    }

    /// Remove the annotation at `index`, returning it. `None` if the index
    /// is out of range.
    pub fn remove(&mut self, index: usize) -> Option<Annotation> {
        (index < self.entries.len()).then(|| self.entries.remove(index))
    }

    /// Serialize the store as JSON — DEBUGGER.md §4's "import/export as
    /// JSON", and also the on-disk persistence format (see
    /// [`Self::from_json`] for why those are deliberately one format and
    /// not two).
    ///
    /// # Errors
    /// Propagates a `serde_json` failure. In practice this cannot fail for
    /// these types, but returning the error beats an `unwrap` in a path a
    /// user's data travels through.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.entries)
    }

    /// Parse a store from JSON, **through [`Self::add`]**.
    ///
    /// ## Why import re-validates rather than deserializing straight in
    ///
    /// `#[derive(Deserialize)]` onto `Vec<Annotation>` would happily
    /// accept `"source": ""` — the invariant FR-DBG-005 asks for lives in
    /// [`Self::add`], not in the type. A file edited by hand, or written
    /// by an older build, could then put an unsourced annotation into a
    /// store that every other code path is entitled to assume cannot hold
    /// one, and the exporter would be the thing that discovered it.
    /// Import is a public entry point, so it uses the same gate.
    ///
    /// Rejected rows are **reported, not silently dropped**: the count
    /// comes back with the store so a caller can say "14 imported, 2
    /// rejected" rather than quietly losing two.
    ///
    /// # Errors
    /// Propagates a `serde_json` parse failure (a file that is not
    /// annotation JSON at all).
    pub fn from_json(text: &str) -> Result<(Self, Vec<AnnotationError>), serde_json::Error> {
        let raw: Vec<Annotation> = serde_json::from_str(text)?;
        let mut store = AnnotationStore::new();
        let mut rejected = Vec::new();
        for annotation in raw {
            if let Err(e) = store.add(annotation) {
                rejected.push(e);
            }
        }
        Ok((store, rejected))
    }

    /// The `addr -> label` lookup [`label_operands`] needs, RAM-space only.
    ///
    /// ROM annotations are file offsets, not bus addresses, so labelling a
    /// `$8000` operand with a `rom_map` row whose offset happens to be
    /// `0x8000` would be a coincidence rendered as a fact.
    #[must_use]
    pub fn ram_labels(&self) -> Vec<RamLabel<'_>> {
        self.entries
            .iter()
            .filter(|a| a.space == AddressSpace::Ram)
            .map(|a| RamLabel {
                addr: a.addr,
                len: a.len.max(1),
                label: a.label.as_str(),
            })
            .collect()
    }
}

/// Promote a watchpoint to an annotation — DEBUGGER.md §1's last bullet,
/// "a watchpoint can be 'promoted' to a `memory_map` annotation with one
/// click (address, size, label)" (ticket W13-02e).
///
/// The watch supplies address and size; the caller supplies the two things
/// a watch cannot know — what the address *means* and where that claim
/// came from. `source` is still required, and this function does not
/// bypass [`AnnotationStore::add`]'s check: it builds the value, and `add`
/// remains the one gate.
///
/// ## Only a CPU-space watch promotes
///
/// A profile's `[[memory_map]]` is addressed on the **CPU** bus
/// (GAME_PROFILES.md §2), so a PPU-space watch has no row to become.
/// Returning `None` says so, rather than silently emitting a row whose
/// address means something else entirely — the same reason
/// [`AnnotationStore::ram_labels`] refuses to label a bus operand with a
/// ROM offset.
#[must_use]
pub fn promote_watch(
    watch: rf_core_api::MemWatch,
    label: &str,
    ty: &str,
    source: &str,
) -> Option<Annotation> {
    if watch.space != rf_core_api::WatchSpace::Cpu {
        return None;
    }
    Some(Annotation {
        space: AddressSpace::Ram,
        addr: watch.start,
        // The watch's range is inclusive at both ends, so a one-byte watch
        // has start == end and this is 1 — not 0.
        len: watch.end.saturating_sub(watch.start).saturating_add(1),
        ty: ty.to_string(),
        label: label.to_string(),
        notes: None,
        source: source.to_string(),
        count: None,
    })
}

/// One RAM annotation reduced to what [`label_operands`] needs.
///
/// `len` is clamped to at least 1 by [`AnnotationStore::ram_labels`]: a
/// zero-length annotation covers nothing, and silently labelling nothing
/// is harder to notice than labelling the one byte the author meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamLabel<'a> {
    pub addr: u32,
    pub len: u32,
    pub label: &'a str,
}

/// Append `{label}` after every `$`-prefixed operand in `text` that a RAM
/// annotation covers — DEBUGGER.md §4's cross-link, in its own example's
/// shape: `LDA $0086` becomes `LDA $0086 {player_x_screen}`.
///
/// UI-free and pure so it can be unit-tested without a trace, a core or an
/// egui context, like everything else in this crate.
///
/// ## Coverage, not equality
///
/// An annotation with `len > 1` covers a range: a 2-byte `player_x` at
/// `$0086` labels a read of `$0087` too, because that read *is* touching
/// `player_x`. Matching only the base address would leave the high byte of
/// every 16-bit quantity unlabelled, which is precisely the case a ROM
/// hacker is trying to see.
///
/// ## Why the operand is parsed rather than the address matched
///
/// A trace line is text by the time it reaches the viewer
/// ([`crate::trace::TraceEntry::text`]), and `TraceEntry::addr` holds the
/// *PC*, not the operand. Scanning for `$hex` is therefore the only way to
/// reach the operand, and it is exactly what the doc's example needs.
#[must_use]
pub fn label_operands(text: &str, labels: &[RamLabel<'_>]) -> String {
    // Cheap reject first: most lines have no `$` at all.
    if !text.contains('$') || labels.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // ADVANCE FIRST, unconditionally (project law 8): every path
            // through this loop moves `i`, so it cannot spin.
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
            j += 1;
        }
        let digits = &text[start + 1..j];
        out.push_str(&text[start..j]);
        i = j.max(start + 1);
        if digits.is_empty() {
            continue;
        }
        let Ok(addr) = u32::from_str_radix(digits, 16) else {
            continue;
        };
        // First covering annotation wins. Two annotations covering one
        // address is an authoring mistake; rendering both would make the
        // line unreadable and rendering neither would hide the mistake.
        if let Some(hit) = labels
            .iter()
            .find(|l| addr >= l.addr && addr < l.addr.saturating_add(l.len))
        {
            out.push_str(" {");
            out.push_str(hit.label);
            out.push('}');
        }
    }
    out
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

    /// W13-02f criterion 2's round trip, and the reason import goes
    /// through `add`: a hand-edited file must not be able to smuggle in
    /// what `add` refuses.
    #[test]
    fn json_round_trips_and_import_re_enforces_the_source_rule() {
        let mut store = AnnotationStore::new();
        store.add(valid(AddressSpace::Ram, 0x0086)).unwrap();
        store.add(valid(AddressSpace::Rom, 0x1234)).unwrap();

        let json = store.to_json().unwrap();
        let (back, rejected) = AnnotationStore::from_json(&json).unwrap();
        assert!(rejected.is_empty());
        assert_eq!(back.entries(), store.entries());

        // A file someone edited by hand, blanking a source.
        let tampered = json.replace("https://example.test/ram-map", "   ");
        let (loaded, rejected) = AnnotationStore::from_json(&tampered).unwrap();
        assert_eq!(loaded.len(), 0, "unsourced rows must not enter the store");
        assert_eq!(rejected.len(), 2, "and they must be REPORTED, not dropped");
        assert!(matches!(rejected[0], AnnotationError::EmptySource { .. }));
    }

    #[test]
    fn edit_and_delete_address_rows_by_index_and_survive_a_stale_one() {
        let mut store = AnnotationStore::new();
        store.add(valid(AddressSpace::Ram, 0x0086)).unwrap();
        store.add(valid(AddressSpace::Ram, 0x0090)).unwrap();

        let mut edited = valid(AddressSpace::Ram, 0x0086);
        edited.label = "player_x_screen".to_string();
        store.replace(0, edited).unwrap();
        assert_eq!(store.get(0).unwrap().label, "player_x_screen");

        // An edit may not blank what `add` would have refused.
        let mut blanked = valid(AddressSpace::Ram, 0x0086);
        blanked.source = "  ".to_string();
        assert!(matches!(
            store.replace(0, blanked),
            Err(AnnotationError::EmptySource { .. })
        ));
        assert_eq!(store.get(0).unwrap().source, "https://example.test/ram-map");

        assert_eq!(store.remove(0).unwrap().addr, 0x0086);
        assert_eq!(store.len(), 1);
        // A panel holding a stale index across a repaint must not panic.
        assert!(store.remove(7).is_none());
        assert!(store.get(7).is_none());
        assert!(store.replace(7, valid(AddressSpace::Ram, 1)).is_ok());
    }

    /// DEBUGGER.md §4's own example, verbatim.
    #[test]
    fn trace_operands_are_labelled_in_the_docs_own_shape() {
        let mut store = AnnotationStore::new();
        let mut a = valid(AddressSpace::Ram, 0x0086);
        a.label = "player_x_screen".to_string();
        a.len = 2;
        store.add(a).unwrap();
        let labels = store.ram_labels();

        assert_eq!(
            label_operands("LDA $0086", &labels),
            "LDA $0086 {player_x_screen}"
        );
        // len 2 covers the high byte: reading $0087 IS reading player_x.
        assert_eq!(
            label_operands("LDA $0087", &labels),
            "LDA $0087 {player_x_screen}"
        );
        assert_eq!(label_operands("LDA $0088", &labels), "LDA $0088");
        // Untouched when there is nothing to say, including for text with
        // no operand at all.
        assert_eq!(label_operands("CLC", &labels), "CLC");
        assert_eq!(label_operands("LDA $0086", &[]), "LDA $0086");
    }

    /// A ROM annotation is a FILE OFFSET. Labelling a bus operand with it
    /// would render a coincidence as a fact.
    #[test]
    fn rom_offsets_never_label_a_bus_operand() {
        let mut store = AnnotationStore::new();
        store.add(valid(AddressSpace::Rom, 0x8000)).unwrap();
        assert_eq!(
            label_operands("LDA $8000", &store.ram_labels()),
            "LDA $8000"
        );
    }

    /// Law 8: the hand-rolled walk in `label_operands` must advance on
    /// every path. A lone `$`, a `$` at end-of-string and a run of them are
    /// the shapes that would spin an index that only moves on a match.
    #[test]
    fn the_operand_scanner_terminates_on_degenerate_input() {
        let labels: Vec<RamLabel<'_>> = vec![RamLabel {
            addr: 0,
            len: 1,
            label: "zero",
        }];
        assert_eq!(label_operands("$", &labels), "$");
        assert_eq!(label_operands("$$$", &labels), "$$$");
        assert_eq!(label_operands("LDA $", &labels), "LDA $");
        assert_eq!(label_operands("$0 $0", &labels), "$0 {zero} $0 {zero}");
    }

    /// DEBUGGER.md §1's promotion bullet, and the two things it must not
    /// get wrong: the inclusive range becomes a length of 1, and a
    /// PPU-space watch has no `[[memory_map]]` row to become.
    #[test]
    fn a_watchpoint_promotes_to_a_memory_map_annotation() {
        use rf_core_api::{MemWatch, WatchAccess, WatchSpace};

        let one_byte = MemWatch::unconditional(3, WatchSpace::Cpu, 0x0086, WatchAccess::Write);
        let a = promote_watch(one_byte, "player_x", "u8", "https://example.test/session")
            .expect("a CPU-space watch promotes");
        assert_eq!(a.space, AddressSpace::Ram);
        assert_eq!(a.addr, 0x0086);
        assert_eq!(a.len, 1, "start == end is ONE byte, not zero");

        let two_byte = MemWatch {
            end: 0x0087,
            ..one_byte
        };
        let a = promote_watch(two_byte, "player_x", "u16", "https://example.test/session").unwrap();
        assert_eq!(a.len, 2);

        // And it still has to pass the store's gate.
        let mut store = AnnotationStore::new();
        assert!(store.add(a).is_ok());
        let sourceless = promote_watch(one_byte, "player_x", "u8", "   ").unwrap();
        assert!(matches!(
            store.add(sourceless),
            Err(AnnotationError::EmptySource { .. })
        ));

        // A PPU-space watch has no memory_map row to become.
        let ppu = MemWatch::unconditional(4, WatchSpace::Ppu, 0x2000, WatchAccess::Any);
        assert!(promote_watch(ppu, "nametable", "u8", "https://example.test/s").is_none());
    }
}
