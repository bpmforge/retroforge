//! The `[mods]` game-logic patch engine (ticket W7-12; FR-ENH-009).
//!
//! ## This is the sharpest test of law 6 in the project
//!
//! Every other enhancement is presentation: a wider canvas, a smoother
//! camera, a replaced tile. A `[mods]` patch rewrites the game's **code**,
//! so a machine running one is not simulating the cartridge any more. Law
//! 6 says Accuracy Mode is the reference and enhancements are opt-in
//! overlays over an unmodified simulation, and the only way to keep that
//! true here is mechanical:
//!
//! * **Nothing is enabled by default.** [`ModEngine::from_profile`] starts
//!   with an empty enabled set no matter what the profile declares. A
//!   profile can offer a patch; only a user can turn it on.
//! * **[`ModEngine::is_accuracy_mode`] is false the moment one is on.** It
//!   is derived from the enabled set rather than stored alongside it,
//!   because a stored flag can disagree with reality and this is the one
//!   flag that must not.
//! * **Every transition is ledgered** with the frame it happened on, so a
//!   session can always answer "was this run modified, and from when".
//!
//! ## Applying is reversible, and refuses rather than half-applies
//!
//! [`ModEngine::apply`] returns the original bytes it overwrote, so
//! [`revert`] restores the exact image. Both the range check and the
//! overlap check run over the whole set **before** the first byte is
//! written — a patch set that is half-applied and then rejected leaves a
//! ROM that is neither the cartridge nor the mod, which is the worst of
//! the three possible outcomes.
//!
//! ## Overlaps are an error, not a last-writer-wins
//!
//! Two enabled patches touching the same byte make the result depend on
//! application order, and make `revert` order-dependent too: reverting the
//! first would restore bytes the second had already replaced. Since the
//! enabled set is a `BTreeSet` the order would at least be stable, but
//! stable-and-arbitrary is not the same as correct, so overlapping patches
//! are refused with both ids named.

use std::collections::{BTreeMap, BTreeSet};

use rf_profiles::schema::{ModPatch, Mods};

/// One patch a profile declares. Mirrors [`ModPatch`] but owns its data,
/// so the engine outlives the borrowed profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredPatch {
    pub id: String,
    pub description: String,
    /// Offset into the **normalized** ROM image (header stripped), the
    /// same domain `[[rom_map]]` offsets live in.
    pub addr: u32,
    pub replace: Vec<u8>,
}

impl DeclaredPatch {
    fn end(&self) -> u64 {
        u64::from(self.addr) + self.replace.len() as u64
    }
}

/// Why a mod operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModError {
    /// No patch with this id is declared by the active profile. Naming an
    /// undeclared id is a typo or a stale UI, never a no-op.
    UnknownId(String),
    /// The patch would write past the end of the ROM image.
    OutOfRange {
        id: String,
        addr: u32,
        len: usize,
        rom_len: usize,
    },
    /// Two enabled patches write the same byte.
    Overlap {
        first: String,
        second: String,
        addr: u64,
    },
}

impl std::fmt::Display for ModError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModError::UnknownId(id) => {
                write!(f, "no mod `{id}` is declared by the active profile")
            }
            ModError::OutOfRange {
                id,
                addr,
                len,
                rom_len,
            } => write!(
                f,
                "mod `{id}` writes {len} bytes at {addr:#08X}, past the end of a \
                 {rom_len}-byte ROM"
            ),
            ModError::Overlap {
                first,
                second,
                addr,
            } => write!(
                f,
                "mods `{first}` and `{second}` both write {addr:#08X}; the result \
                 would depend on which was applied last"
            ),
        }
    }
}

impl std::error::Error for ModError {}

/// What happened to a mod, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModEvent {
    Enabled,
    Disabled,
    /// The patch's bytes actually reached the ROM image.
    Applied {
        addr: u32,
        len: usize,
    },
    /// The image was restored.
    Reverted {
        addr: u32,
        len: usize,
    },
}

/// One ledger line (FR-ENH-009's "ledger-logged").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub id: String,
    pub event: ModEvent,
    /// Frame number the transition happened on, so the ledger orders
    /// against everything else the session records.
    pub frame: u64,
}

/// The bytes a patch overwrote, kept so the image can be restored exactly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Applied {
    /// `id -> (addr, original bytes)`, ordered so a revert is
    /// deterministic.
    originals: BTreeMap<String, (u32, Vec<u8>)>,
}

impl Applied {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.originals.is_empty()
    }

    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.originals.keys().cloned().collect()
    }
}

/// The patch engine for one profile.
#[derive(Debug, Clone, Default)]
pub struct ModEngine {
    declared: Vec<DeclaredPatch>,
    enabled: BTreeSet<String>,
    ledger: Vec<LedgerEntry>,
}

impl ModEngine {
    /// Build from a profile's `[mods]` section — **with everything off**.
    ///
    /// The enabled set is empty regardless of what the profile says,
    /// because FR-ENH-009 makes that a property of the engine rather than
    /// of the data. There is deliberately no `enabled` field in the schema
    /// for a profile to set.
    #[must_use]
    pub fn from_profile(mods: Option<&Mods>) -> Self {
        let declared = mods
            .map(|m| {
                m.patch
                    .iter()
                    .map(|p: &ModPatch| DeclaredPatch {
                        id: p.id.clone(),
                        description: p.description.clone(),
                        addr: p.addr,
                        replace: p.replace.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            declared,
            enabled: BTreeSet::new(),
            ledger: Vec::new(),
        }
    }

    #[must_use]
    pub fn declared(&self) -> &[DeclaredPatch] {
        &self.declared
    }

    #[must_use]
    pub fn is_enabled(&self, id: &str) -> bool {
        self.enabled.contains(id)
    }

    /// Enabled ids, sorted. Sorted because this set is written into save
    /// states and compared across sessions: a set that serialised in
    /// insertion order would make two identical configurations compare
    /// unequal.
    #[must_use]
    pub fn enabled_ids(&self) -> Vec<String> {
        self.enabled.iter().cloned().collect()
    }

    #[must_use]
    pub fn ledger(&self) -> &[LedgerEntry] {
        &self.ledger
    }

    /// **Law 6, mechanised.** True while the simulation is unmodified.
    ///
    /// Derived from the enabled set rather than tracked separately: a
    /// stored flag is a second source of truth, and this is the one that
    /// decides whether the honesty badge is telling the truth.
    #[must_use]
    pub fn is_accuracy_mode(&self) -> bool {
        self.enabled.is_empty()
    }

    /// Turn one patch on.
    ///
    /// # Errors
    /// [`ModError::UnknownId`] if the profile declares no such patch.
    pub fn enable(&mut self, id: &str, frame: u64) -> Result<(), ModError> {
        if !self.declared.iter().any(|p| p.id == id) {
            return Err(ModError::UnknownId(id.to_string()));
        }
        if self.enabled.insert(id.to_string()) {
            self.ledger.push(LedgerEntry {
                id: id.to_string(),
                event: ModEvent::Enabled,
                frame,
            });
        }
        Ok(())
    }

    /// Turn one patch off.
    ///
    /// # Errors
    /// [`ModError::UnknownId`] if the profile declares no such patch.
    pub fn disable(&mut self, id: &str, frame: u64) -> Result<(), ModError> {
        if !self.declared.iter().any(|p| p.id == id) {
            return Err(ModError::UnknownId(id.to_string()));
        }
        if self.enabled.remove(id) {
            self.ledger.push(LedgerEntry {
                id: id.to_string(),
                event: ModEvent::Disabled,
                frame,
            });
        }
        Ok(())
    }

    /// Adopt a specific enabled set wholesale — the explicit half of
    /// [`reconcile`]'s answer.
    ///
    /// # Errors
    /// [`ModError::UnknownId`] naming the first id this profile does not
    /// declare. Checked for ALL ids before any is applied, so a partial
    /// adoption cannot happen.
    pub fn adopt(&mut self, ids: &[String], frame: u64) -> Result<(), ModError> {
        for id in ids {
            if !self.declared.iter().any(|p| &p.id == id) {
                return Err(ModError::UnknownId(id.clone()));
            }
        }
        let wanted: BTreeSet<String> = ids.iter().cloned().collect();
        for id in self.enabled_ids() {
            if !wanted.contains(&id) {
                self.disable(&id, frame)?;
            }
        }
        for id in &wanted {
            self.enable(id, frame)?;
        }
        Ok(())
    }

    /// Apply every enabled patch to a normalized ROM image.
    ///
    /// Validates the whole set first: a half-applied set that then fails
    /// leaves an image that is neither the cartridge nor the mod.
    ///
    /// # Errors
    /// [`ModError::OutOfRange`] or [`ModError::Overlap`], with nothing
    /// written in either case.
    pub fn apply(&mut self, rom: &mut [u8], frame: u64) -> Result<Applied, ModError> {
        let mut chosen: Vec<&DeclaredPatch> = self
            .declared
            .iter()
            .filter(|p| self.enabled.contains(&p.id))
            .collect();
        chosen.sort_by(|a, b| a.id.cmp(&b.id));

        for p in &chosen {
            if p.end() > rom.len() as u64 {
                return Err(ModError::OutOfRange {
                    id: p.id.clone(),
                    addr: p.addr,
                    len: p.replace.len(),
                    rom_len: rom.len(),
                });
            }
        }
        for (i, a) in chosen.iter().enumerate() {
            for b in &chosen[i + 1..] {
                let lo = u64::from(a.addr).max(u64::from(b.addr));
                let hi = a.end().min(b.end());
                if lo < hi {
                    return Err(ModError::Overlap {
                        first: a.id.clone(),
                        second: b.id.clone(),
                        addr: lo,
                    });
                }
            }
        }

        let mut applied = Applied::default();
        for p in chosen {
            let at = p.addr as usize;
            let original = rom[at..at + p.replace.len()].to_vec();
            rom[at..at + p.replace.len()].copy_from_slice(&p.replace);
            applied.originals.insert(p.id.clone(), (p.addr, original));
            self.ledger.push(LedgerEntry {
                id: p.id.clone(),
                event: ModEvent::Applied {
                    addr: p.addr,
                    len: p.replace.len(),
                },
                frame,
            });
        }
        Ok(applied)
    }

    /// Restore an image to exactly what it was before [`ModEngine::apply`].
    pub fn revert(&mut self, rom: &mut [u8], applied: &Applied, frame: u64) {
        for (id, (addr, original)) in &applied.originals {
            let at = *addr as usize;
            if at + original.len() <= rom.len() {
                rom[at..at + original.len()].copy_from_slice(original);
                self.ledger.push(LedgerEntry {
                    id: id.clone(),
                    event: ModEvent::Reverted {
                        addr: *addr,
                        len: original.len(),
                    },
                    frame,
                });
            }
        }
    }
}

/// The result of comparing a save state's mod set against the live one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The state was taken with exactly the mods now active.
    Match,
    /// They differ. Both directions are reported, because they mean
    /// different things: a mod in the state but not active means the
    /// state's machine ran code this one does not have, and a mod active
    /// but not in the state means the reverse.
    Mismatch {
        only_in_state: Vec<String>,
        only_active: Vec<String>,
    },
}

impl std::fmt::Display for Reconciliation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reconciliation::Match => write!(f, "mods match"),
            Reconciliation::Mismatch {
                only_in_state,
                only_active,
            } => {
                write!(f, "mod mismatch:")?;
                if !only_in_state.is_empty() {
                    write!(
                        f,
                        " state has [{}] which are off now",
                        only_in_state.join(", ")
                    )?;
                }
                if !only_active.is_empty() {
                    write!(
                        f,
                        " session has [{}] which the state lacks",
                        only_active.join(", ")
                    )?;
                }
                Ok(())
            }
        }
    }
}

/// Compare a state's recorded mod set with the session's active one
/// (FR-ENH-009's third criterion).
///
/// **The point is that there is no third answer.** A caller gets `Match`
/// or a `Mismatch` naming exactly what differs; it cannot get a state
/// loaded "mostly right". Loading a state whose machine ran patched code
/// into a session running unpatched code produces a machine whose RAM was
/// written by instructions that are no longer there — it will not crash
/// immediately, which is precisely why it must be refused loudly instead.
#[must_use]
pub fn reconcile(in_state: &[String], active: &[String]) -> Reconciliation {
    let s: BTreeSet<&String> = in_state.iter().collect();
    let a: BTreeSet<&String> = active.iter().collect();
    if s == a {
        return Reconciliation::Match;
    }
    Reconciliation::Mismatch {
        only_in_state: s.difference(&a).map(|v| (*v).clone()).collect(),
        only_active: a.difference(&s).map(|v| (*v).clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_mods() -> Mods {
        Mods {
            patch: vec![
                ModPatch {
                    id: "no-screen-shake".to_string(),
                    description: "NOP out the shake write".to_string(),
                    addr: 0x10,
                    replace: vec![0xEA, 0xEA],
                },
                ModPatch {
                    id: "fast-text".to_string(),
                    description: "one frame per glyph".to_string(),
                    addr: 0x20,
                    replace: vec![0x01],
                },
            ],
        }
    }

    fn rom() -> Vec<u8> {
        (0..64u8).collect()
    }

    /// **FR-ENH-009's first clause, and law 6's mechanical form.** A
    /// profile that declares patches must still produce a machine with
    /// none of them on.
    #[test]
    fn every_declared_mod_is_off_until_a_user_turns_it_on() {
        let e = ModEngine::from_profile(Some(&profile_mods()));
        assert_eq!(e.declared().len(), 2, "both patches are offered");
        assert!(e.enabled_ids().is_empty(), "and none is enabled");
        assert!(
            e.is_accuracy_mode(),
            "a session that has enabled nothing is still the reference"
        );
    }

    /// Enabling one leaves Accuracy Mode. This is the assertion that makes
    /// the honesty badge honest.
    #[test]
    fn enabling_a_patch_leaves_accuracy_mode_and_disabling_returns_to_it() {
        let mut e = ModEngine::from_profile(Some(&profile_mods()));
        e.enable("no-screen-shake", 10).unwrap();
        assert!(!e.is_accuracy_mode());
        assert_eq!(e.enabled_ids(), vec!["no-screen-shake".to_string()]);

        e.disable("no-screen-shake", 20).unwrap();
        assert!(
            e.is_accuracy_mode(),
            "turning the last patch off returns the session to the reference"
        );
    }

    /// An undeclared id is refused rather than ignored: a silent no-op
    /// makes a stale UI look like it worked.
    #[test]
    fn an_undeclared_mod_id_is_refused() {
        let mut e = ModEngine::from_profile(Some(&profile_mods()));
        let err = e.enable("does-not-exist", 0).unwrap_err();
        assert_eq!(err, ModError::UnknownId("does-not-exist".to_string()));
        assert!(e.is_accuracy_mode());
        assert!(
            e.ledger().is_empty(),
            "a refused enable is not a transition"
        );
    }

    /// Every transition is ledgered with its frame (FR-ENH-009's
    /// "ledger-logged"), including the apply itself.
    #[test]
    fn the_ledger_records_each_transition_with_its_frame() {
        let mut e = ModEngine::from_profile(Some(&profile_mods()));
        let mut image = rom();
        e.enable("fast-text", 7).unwrap();
        let applied = e.apply(&mut image, 8).unwrap();
        e.revert(&mut image, &applied, 9);
        e.disable("fast-text", 12).unwrap();

        let events: Vec<(&str, &ModEvent, u64)> = e
            .ledger()
            .iter()
            .map(|l| (l.id.as_str(), &l.event, l.frame))
            .collect();
        assert_eq!(
            events.len(),
            4,
            "enable, apply, revert, disable: {events:?}"
        );
        assert_eq!(events[0], ("fast-text", &ModEvent::Enabled, 7));
        assert!(matches!(events[1].1, ModEvent::Applied { .. }));
        assert_eq!(events[1].2, 8);
        assert!(matches!(events[2].1, ModEvent::Reverted { .. }));
        assert_eq!(events[3], ("fast-text", &ModEvent::Disabled, 12));

        // Enabling something already on is not a second transition.
        let before = e.ledger().len();
        e.enable("fast-text", 20).unwrap();
        e.enable("fast-text", 21).unwrap();
        assert_eq!(e.ledger().len(), before + 1, "only the real change ledgers");
    }

    /// Applying writes the declared bytes and nothing else, and reverting
    /// restores the image EXACTLY — byte-for-byte against a pristine copy.
    #[test]
    fn apply_then_revert_restores_the_original_image_byte_for_byte() {
        let mut e = ModEngine::from_profile(Some(&profile_mods()));
        let pristine = rom();
        let mut image = rom();
        e.enable("no-screen-shake", 0).unwrap();
        e.enable("fast-text", 0).unwrap();

        let applied = e.apply(&mut image, 1).unwrap();
        assert_eq!(&image[0x10..0x12], &[0xEA, 0xEA], "the patch landed");
        assert_eq!(image[0x20], 0x01);
        assert_ne!(image, pristine, "and the image really changed");
        // Untouched bytes stay untouched.
        assert_eq!(image[0x0F], pristine[0x0F]);
        assert_eq!(image[0x12], pristine[0x12]);

        e.revert(&mut image, &applied, 2);
        assert_eq!(image, pristine, "revert must be exact, not approximate");
    }

    /// **Nothing is written when the set is invalid.** A half-applied set
    /// leaves an image that is neither the cartridge nor the mod.
    #[test]
    fn an_out_of_range_patch_refuses_without_writing_anything() {
        let mut mods = profile_mods();
        mods.patch.push(ModPatch {
            id: "off-the-end".to_string(),
            description: "past the ROM".to_string(),
            addr: 0x3F,
            replace: vec![0xAA, 0xBB],
        });
        let mut e = ModEngine::from_profile(Some(&mods));
        e.enable("no-screen-shake", 0).unwrap();
        e.enable("off-the-end", 0).unwrap();

        let pristine = rom();
        let mut image = rom();
        let err = e.apply(&mut image, 1).unwrap_err();
        assert!(matches!(err, ModError::OutOfRange { .. }), "{err}");
        assert_eq!(
            image, pristine,
            "the valid patch in the same set must not have been applied either"
        );
    }

    /// Overlapping patches are an error, because otherwise the result — and
    /// the revert — depend on application order.
    #[test]
    fn two_patches_writing_the_same_byte_are_refused_naming_both() {
        let mut mods = profile_mods();
        mods.patch.push(ModPatch {
            id: "also-shake".to_string(),
            description: "overlaps no-screen-shake".to_string(),
            addr: 0x11,
            replace: vec![0x00],
        });
        let mut e = ModEngine::from_profile(Some(&mods));
        e.enable("no-screen-shake", 0).unwrap();
        e.enable("also-shake", 0).unwrap();

        let pristine = rom();
        let mut image = rom();
        let err = e.apply(&mut image, 1).unwrap_err();
        match &err {
            ModError::Overlap {
                first,
                second,
                addr,
            } => {
                assert_eq!(*addr, 0x11);
                let named = [first.as_str(), second.as_str()];
                assert!(named.contains(&"no-screen-shake") && named.contains(&"also-shake"));
            }
            other => panic!("expected an overlap error, got {other}"),
        }
        assert_eq!(image, pristine);
    }

    /// **Criterion 3.** A state's set and the session's set either match or
    /// the difference is named in both directions.
    #[test]
    fn reconcile_names_exactly_what_differs_in_both_directions() {
        let a = vec!["fast-text".to_string()];
        assert_eq!(reconcile(&a, &a), Reconciliation::Match);
        assert_eq!(reconcile(&[], &[]), Reconciliation::Match);

        let state = vec!["fast-text".to_string(), "no-screen-shake".to_string()];
        let active = vec!["no-screen-shake".to_string()];
        match reconcile(&state, &active) {
            Reconciliation::Mismatch {
                only_in_state,
                only_active,
            } => {
                assert_eq!(only_in_state, vec!["fast-text".to_string()]);
                assert!(only_active.is_empty());
            }
            Reconciliation::Match => panic!("these sets differ"),
        }

        // And the other direction, which means something different: this
        // session is running code the state's machine never had.
        match reconcile(&active, &state) {
            Reconciliation::Mismatch {
                only_in_state,
                only_active,
            } => {
                assert!(only_in_state.is_empty());
                assert_eq!(only_active, vec!["fast-text".to_string()]);
            }
            Reconciliation::Match => panic!("these sets differ"),
        }
    }

    /// Adopting a state's set is the explicit way to resolve a mismatch,
    /// and it is all-or-nothing.
    #[test]
    fn adopting_an_unknown_set_changes_nothing() {
        let mut e = ModEngine::from_profile(Some(&profile_mods()));
        e.enable("fast-text", 0).unwrap();
        let err = e
            .adopt(&["no-screen-shake".to_string(), "invented".to_string()], 1)
            .unwrap_err();
        assert_eq!(err, ModError::UnknownId("invented".to_string()));
        assert_eq!(
            e.enabled_ids(),
            vec!["fast-text".to_string()],
            "a refused adopt must not have applied the half it recognised"
        );

        e.adopt(&["no-screen-shake".to_string()], 2).unwrap();
        assert_eq!(e.enabled_ids(), vec!["no-screen-shake".to_string()]);
        assert_eq!(
            reconcile(&["no-screen-shake".to_string()], &e.enabled_ids()),
            Reconciliation::Match,
            "adopting is what turns a mismatch into a match"
        );
    }

    /// A profile with no `[mods]` at all is the common case and must be
    /// inert rather than special.
    #[test]
    fn a_profile_without_mods_offers_nothing_and_stays_accurate() {
        let mut e = ModEngine::from_profile(None);
        assert!(e.declared().is_empty());
        assert!(e.is_accuracy_mode());
        let mut image = rom();
        let pristine = rom();
        assert!(e.apply(&mut image, 0).unwrap().is_empty());
        assert_eq!(image, pristine);
    }
}
