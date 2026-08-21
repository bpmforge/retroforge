//! Community distribution format for packs, profiles and mods
//! (ticket W9-05; FR-PROF-005/006/007, ROADMAP P9 "community-safe: no ROM
//! data, license metadata required", CLAUDE.md law 5).
//!
//! ## The allowlist is the mechanism; the ROM sniffer is the diagnostic
//!
//! This is the design decision the whole module rests on, and getting it
//! backwards is the obvious mistake.
//!
//! A **denylist** of ROM signatures cannot work. Renaming `game.nes` to
//! `data.bin` defeats an extension check; a headerless ROM — which is
//! most SNES dumps — defeats a magic-byte check. Any sniffer is a filter
//! on things that *look* like ROMs, and "looks like" is not a property
//! anyone can enumerate.
//!
//! So the format is **deny-by-default over member KINDS**: a bundle may
//! carry a profile, a pack manifest, a replacement image, a mod patch, a
//! licence file or a readme, and **nothing else**. An arbitrary blob has
//! no kind it can claim, so it is refused for being unclassifiable rather
//! than for resembling a ROM. That is a property the format enforces
//! structurally instead of a pattern it hopes to recognise.
//!
//! [`sniff_rom`] then exists purely to make the *diagnostic* good: an
//! author who accidentally zipped their ROM in should be told "this looks
//! like an iNES ROM", not "unknown member kind". It is a courtesy, not
//! the guard, and [`IntakeReport`] says so.
//!
//! **What this cannot do, stated plainly:** it cannot prove a bundle
//! contains no ROM data. A determined submitter can base64 a ROM into a
//! readme. The claim this module actually supports is narrower and worth
//! stating exactly — see [`IntakeReport::summary`].
//!
//! ## Licence metadata is required, and unknown is not permissive
//!
//! FR-PROF-007: "SPDX license + provenance metadata required;
//! missing/unknown licenses and denylisted content (NC assets, GFDL text,
//! GPL-derived shader code) rejected; nothing activates without passing."
//!
//! Both halves matter. A **missing** licence is refused, and so is an
//! **unrecognised** one — treating "I don't know this string" as
//! acceptable is how a non-commercial asset gets shipped by a project
//! whose whole licence policy is allowlist-only (NFR-011).

use std::collections::BTreeSet;

/// SPDX identifiers a community bundle may declare.
///
/// Mirrors `deny.toml`'s allowlist (NFR-011) plus the asset licences that
/// make sense for artwork a code allowlist has no reason to carry.
/// **Allowlist, not denylist** — the same reason `deny.toml` is
/// allow-only: every licence nobody has considered is refused by default.
pub const ALLOWED_LICENSES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "Zlib",
    "ISC",
    "Unlicense",
    "CC0-1.0",
    "CC-BY-4.0",
    "CC-BY-SA-4.0",
];

/// What a bundle member is.
///
/// A closed set, and that is the point — see the module doc. There is
/// deliberately no `Other`/`Data` variant, because one would reopen
/// exactly the hole the allowlist closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemberKind {
    /// A `profile.toml` (FR-PROF-005/006).
    Profile,
    /// A replacement-pack manifest (`hires.txt` or our own).
    PackManifest,
    /// A replacement image referenced by a pack manifest.
    PackImage,
    /// A `[mods.patch]` declaration. Facts, never ROM bytes.
    ModPatch,
    /// The bundle's licence text.
    License,
    /// Human-readable documentation.
    Readme,
}

impl MemberKind {
    /// The name used in the bundle index and in every diagnostic.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            MemberKind::Profile => "profile",
            MemberKind::PackManifest => "pack-manifest",
            MemberKind::PackImage => "pack-image",
            MemberKind::ModPatch => "mod-patch",
            MemberKind::License => "license",
            MemberKind::Readme => "readme",
        }
    }

    /// Parse a declared kind. Unknown strings are `None` and get refused.
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        [
            MemberKind::Profile,
            MemberKind::PackManifest,
            MemberKind::PackImage,
            MemberKind::ModPatch,
            MemberKind::License,
            MemberKind::Readme,
        ]
        .into_iter()
        .find(|k| k.name() == s)
    }
}

/// One file in a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Path inside the bundle.
    pub path: String,
    /// What the submitter says it is.
    pub declared_kind: String,
    /// The bytes. Held in memory because a bundle is small by
    /// construction — anything large enough to matter is a ROM or an
    /// image, and images have a size cap.
    pub bytes: Vec<u8>,
}

/// A community bundle: metadata plus members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub id: String,
    pub version: String,
    /// SPDX identifier. **Required** — see the module doc.
    pub license: String,
    /// Where this came from (D-005). **Required.**
    pub provenance: String,
    pub members: Vec<Member>,
}

/// Why a bundle was refused at intake.
///
/// Every variant names the offending member and what was wrong, because a
/// submitter fixing a rejection needs both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntakeFault {
    /// No licence declared.
    MissingLicense,
    /// No provenance declared.
    MissingProvenance,
    /// A licence nobody has cleared. **Unknown is refused, not assumed
    /// permissive** (FR-PROF-007).
    UnknownLicense { declared: String },
    /// A licence that is known and not acceptable — NC/ND assets, GFDL
    /// text, GPL-derived code.
    DeniedLicense { declared: String, reason: String },
    /// The member declares a kind this format does not carry. **This is
    /// the deny-by-default guard**, and it is what actually stops an
    /// arbitrary blob.
    UnknownMemberKind { path: String, declared: String },
    /// The member's bytes look like a ROM. A *diagnostic*, not the guard
    /// — see the module doc.
    LooksLikeRom { path: String, signature: String },
    /// A member declared as one thing whose bytes are obviously another.
    ContentMismatch { path: String, detail: String },
    /// A bundle with no members. Refused rather than accepted as a no-op:
    /// an empty bundle that "passes intake" would be listed as installed
    /// while doing nothing.
    Empty,
}

impl std::fmt::Display for IntakeFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntakeFault::MissingLicense => write!(
                f,
                "no `license` declared — FR-PROF-007 requires an SPDX identifier, \
                 and intake is deny-by-default"
            ),
            IntakeFault::MissingProvenance => {
                write!(f, "no `provenance` declared — required by D-005")
            }
            IntakeFault::UnknownLicense { declared } => write!(
                f,
                "licence {declared:?} is not on the allowlist; unknown is REFUSED rather than \
                 assumed permissive. Allowed: {}",
                ALLOWED_LICENSES.join(", ")
            ),
            IntakeFault::DeniedLicense { declared, reason } => {
                write!(f, "licence {declared:?} is not acceptable: {reason}")
            }
            IntakeFault::UnknownMemberKind { path, declared } => write!(
                f,
                "{path}: {declared:?} is not a kind this format carries — a bundle may hold \
                 only profile, pack-manifest, pack-image, mod-patch, license or readme"
            ),
            IntakeFault::LooksLikeRom { path, signature } => write!(
                f,
                "{path} looks like ROM data ({signature}) — bundles carry facts, never game \
                 bytes (ROADMAP P9, CLAUDE.md law 5)"
            ),
            IntakeFault::ContentMismatch { path, detail } => {
                write!(f, "{path}: {detail}")
            }
            IntakeFault::Empty => write!(
                f,
                "bundle has no members — refused rather than installed as a no-op"
            ),
        }
    }
}

impl std::error::Error for IntakeFault {}

/// Licences that are recognised and refused, with the reason FR-PROF-007
/// gives.
///
/// Separate from "unknown" on purpose: a submitter who declared `CC-BY-NC-4.0`
/// gets told *why* it fails rather than being told nobody has heard of it.
fn denied_reason(spdx: &str) -> Option<&'static str> {
    let up = spdx.to_ascii_uppercase();
    if up.contains("-NC") || up.contains("NONCOMMERCIAL") {
        return Some("non-commercial Creative Commons licences are denied (FR-PROF-007)");
    }
    if up.contains("-ND") {
        return Some("no-derivatives Creative Commons licences are denied (FR-PROF-007)");
    }
    if up.starts_with("GFDL") {
        return Some("GFDL text is denied (FR-PROF-007)");
    }
    if up.starts_with("GPL") || up.starts_with("AGPL") || up.starts_with("LGPL") {
        return Some("copyleft is denied by NFR-011 and FR-PROF-007");
    }
    if up.starts_with("SSPL") || up.starts_with("BUSL") {
        return Some("source-available-but-restricted licences are denied");
    }
    None
}

/// Does this look like a ROM?
///
/// **A diagnostic, not the guard.** See the module doc: the allowlist is
/// what actually stops arbitrary data, and no sniffer can recognise a
/// headerless dump. This exists so an author who accidentally zipped
/// their ROM in gets told what happened.
///
/// Returns the signature name when something matches.
#[must_use]
pub fn sniff_rom(path: &str, bytes: &[u8]) -> Option<String> {
    // Magic bytes first — these are positive identifications.
    const MAGICS: &[(&[u8], &str)] = &[
        (b"NES\x1a", "iNES header"),
        (b"FDS\x1a", "FDS disk image"),
        (b"UNIF", "UNIF cartridge"),
        (b"\x7fELF", "ELF executable"),
    ];
    for (magic, name) in MAGICS {
        if bytes.starts_with(magic) {
            return Some((*name).to_string());
        }
    }
    // Then extensions, which catch a headerless dump the magic check
    // cannot — most SNES ROMs have no header at all.
    const ROM_EXTS: &[(&str, &str)] = &[
        (".nes", "NES ROM extension"),
        (".sfc", "SNES ROM extension"),
        (".smc", "SNES ROM extension"),
        (".fds", "FDS image extension"),
        (".unf", "UNIF extension"),
        (".gb", "Game Boy ROM extension"),
        (".gbc", "Game Boy Color ROM extension"),
        (".gba", "Game Boy Advance ROM extension"),
        (".z64", "N64 ROM extension"),
        (".bin", "raw binary extension"),
        (".rom", "ROM extension"),
    ];
    let lower = path.to_ascii_lowercase();
    for (ext, name) in ROM_EXTS {
        if lower.ends_with(ext) {
            return Some((*name).to_string());
        }
    }
    None
}

/// The result of running a bundle through intake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeReport {
    pub bundle_id: String,
    /// Every fault, collected rather than stopping at the first — a
    /// submitter fixes them in one pass instead of one per CI run. Same
    /// rule the pack validator follows.
    pub faults: Vec<IntakeFault>,
    /// Kinds actually present, for the reviewer's summary.
    pub kinds: BTreeSet<MemberKind>,
}

impl IntakeReport {
    /// **Nothing activates without passing** (FR-PROF-007).
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.faults.is_empty()
    }

    /// A line a CI job can print, and an honest statement of what passing
    /// does and does not mean.
    ///
    /// The wording is deliberate: passing intake means **no member
    /// declared a kind this format does not carry, and nothing tripped
    /// the ROM sniffer**. It does *not* mean the bundle provably contains
    /// no game data — a submitter can base64 a ROM into a readme, and no
    /// checker in this file would notice. Claiming otherwise would be the
    /// same overclaim the project refuses everywhere else.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.accepted() {
            let kinds: Vec<&str> = self.kinds.iter().map(|k| k.name()).collect();
            format!(
                "{}: ACCEPTED — carries [{}]; licence and provenance present. \
                 This means no member declared an uncarried kind and nothing matched a ROM \
                 signature; it is NOT proof the bundle contains no game data.",
                self.bundle_id,
                kinds.join(", ")
            )
        } else {
            let mut s = format!(
                "{}: REFUSED — {} fault(s)",
                self.bundle_id,
                self.faults.len()
            );
            for fault in &self.faults {
                s.push_str(&format!("\n  - {fault}"));
            }
            s
        }
    }
}

/// Run a bundle through intake.
///
/// Collects **every** fault rather than returning the first.
#[must_use]
pub fn intake(bundle: &Bundle) -> IntakeReport {
    let mut faults = Vec::new();
    let mut kinds = BTreeSet::new();

    if bundle.license.trim().is_empty() {
        faults.push(IntakeFault::MissingLicense);
    } else if let Some(reason) = denied_reason(&bundle.license) {
        faults.push(IntakeFault::DeniedLicense {
            declared: bundle.license.clone(),
            reason: reason.to_string(),
        });
    } else if !ALLOWED_LICENSES.contains(&bundle.license.trim()) {
        faults.push(IntakeFault::UnknownLicense {
            declared: bundle.license.clone(),
        });
    }

    if bundle.provenance.trim().is_empty() {
        faults.push(IntakeFault::MissingProvenance);
    }

    if bundle.members.is_empty() {
        faults.push(IntakeFault::Empty);
    }

    for m in &bundle.members {
        // THE GUARD: an unclassifiable member is refused for having no
        // kind, not for resembling anything.
        let Some(kind) = MemberKind::from_name(&m.declared_kind) else {
            faults.push(IntakeFault::UnknownMemberKind {
                path: m.path.clone(),
                declared: m.declared_kind.clone(),
            });
            // Still sniff, so the diagnostic can be specific about WHY
            // this blob is suspicious rather than only that it is
            // unclassified.
            if let Some(sig) = sniff_rom(&m.path, &m.bytes) {
                faults.push(IntakeFault::LooksLikeRom {
                    path: m.path.clone(),
                    signature: sig,
                });
            }
            continue;
        };
        kinds.insert(kind);

        // A member with a legitimate kind is still sniffed: the guard
        // stops the unclassifiable case, and this stops the case where
        // someone labels a ROM as a `pack-image`.
        if let Some(sig) = sniff_rom(&m.path, &m.bytes) {
            faults.push(IntakeFault::LooksLikeRom {
                path: m.path.clone(),
                signature: sig,
            });
        }

        // Cheap per-kind sanity: a text kind holding non-UTF-8 is either
        // corrupt or is binary wearing a text label.
        if matches!(
            kind,
            MemberKind::Profile
                | MemberKind::PackManifest
                | MemberKind::License
                | MemberKind::Readme
        ) && std::str::from_utf8(&m.bytes).is_err()
        {
            faults.push(IntakeFault::ContentMismatch {
                path: m.path.clone(),
                detail: format!("declared {} but the bytes are not UTF-8", kind.name()),
            });
        }
    }

    IntakeReport {
        bundle_id: bundle.id.clone(),
        faults,
        kinds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(path: &str, kind: &str, bytes: &[u8]) -> Member {
        Member {
            path: path.to_string(),
            declared_kind: kind.to_string(),
            bytes: bytes.to_vec(),
        }
    }

    fn good_bundle() -> Bundle {
        Bundle {
            id: "example-pack".into(),
            version: "0.1.0".into(),
            license: "CC-BY-4.0".into(),
            provenance: "authored by the submitter, no third-party assets".into(),
            members: vec![
                member("profile.toml", "profile", b"[meta]\ntitle = \"T\"\n"),
                member("LICENSE", "license", b"CC-BY-4.0"),
                member("README.md", "readme", b"# hello"),
            ],
        }
    }

    #[test]
    fn a_well_formed_bundle_is_accepted() {
        // Anti-vacuity for every refusal test below: an intake that
        // refused everything would pass them all while being useless.
        let r = intake(&good_bundle());
        assert!(r.accepted(), "{}", r.summary());
        assert!(r.kinds.contains(&MemberKind::Profile));
    }

    // -----------------------------------------------------------------
    // Criterion 2: the poisoned archive
    // -----------------------------------------------------------------

    #[test]
    fn a_bundle_carrying_an_ines_rom_is_refused_with_a_diagnostic() {
        // THE acceptance the ticket names: "a format that merely
        // DOCUMENTS the rule will eventually carry a ROM".
        let mut b = good_bundle();
        let mut rom = b"NES\x1a".to_vec();
        rom.extend_from_slice(&[0u8; 64]);
        b.members.push(member("smb.nes", "pack-image", &rom));

        let r = intake(&b);
        assert!(!r.accepted(), "a bundle with a ROM must be refused");
        let rom_fault = r
            .faults
            .iter()
            .find(|f| matches!(f, IntakeFault::LooksLikeRom { .. }))
            .expect("the diagnostic must name the ROM");
        let msg = rom_fault.to_string();
        assert!(msg.contains("smb.nes"), "{msg}");
        assert!(msg.contains("iNES header"), "{msg}");
    }

    #[test]
    fn a_headerless_rom_renamed_to_dat_is_still_refused() {
        // The evasion that defeats a magic-byte check. It is caught by
        // the ALLOWLIST — `data` is not a kind this format carries — not
        // by the sniffer, which is exactly the module's design claim.
        let mut b = good_bundle();
        b.members
            .push(member("level.dat", "data", &[0x78, 0x9c, 0x00, 0x11]));
        let r = intake(&b);
        assert!(!r.accepted());
        let f = r
            .faults
            .iter()
            .find(|f| matches!(f, IntakeFault::UnknownMemberKind { .. }))
            .expect("an unclassifiable member is refused for having no kind");
        assert!(f.to_string().contains("level.dat"));
    }

    #[test]
    fn a_rom_labelled_as_a_pack_image_is_still_refused() {
        // Labelling it correctly-shaped does not launder it: a legit kind
        // is still sniffed.
        let mut b = good_bundle();
        b.members
            .push(member("tiles.sfc", "pack-image", &[0u8; 32]));
        let r = intake(&b);
        assert!(!r.accepted());
        assert!(r
            .faults
            .iter()
            .any(|f| matches!(f, IntakeFault::LooksLikeRom { .. })));
    }

    #[test]
    fn the_sniffer_recognises_the_signatures_it_claims_to() {
        assert!(sniff_rom("x.png", b"NES\x1a\x01").is_some());
        assert!(sniff_rom("x.png", b"FDS\x1a").is_some());
        assert!(sniff_rom("rom.smc", &[]).is_some());
        assert!(sniff_rom("art.png", b"\x89PNG").is_none());
    }

    // -----------------------------------------------------------------
    // Criterion 1: required licence metadata
    // -----------------------------------------------------------------

    #[test]
    fn a_missing_licence_is_refused() {
        let mut b = good_bundle();
        b.license = String::new();
        let r = intake(&b);
        assert!(r.faults.contains(&IntakeFault::MissingLicense));
    }

    #[test]
    fn a_missing_provenance_is_refused() {
        let mut b = good_bundle();
        b.provenance = "   ".into();
        let r = intake(&b);
        assert!(r.faults.contains(&IntakeFault::MissingProvenance));
    }

    #[test]
    fn an_unknown_licence_is_refused_rather_than_assumed_permissive() {
        // The half of FR-PROF-007 that is easy to get wrong.
        let mut b = good_bundle();
        b.license = "WTFPL".into();
        let r = intake(&b);
        assert!(!r.accepted());
        let msg = r.faults[0].to_string();
        assert!(msg.contains("not on the allowlist"), "{msg}");
        assert!(
            msg.contains("REFUSED rather than assumed permissive"),
            "{msg}"
        );
    }

    #[test]
    fn a_non_commercial_licence_is_refused_and_says_why() {
        // Distinct from "unknown": the submitter is told the reason.
        let mut b = good_bundle();
        b.license = "CC-BY-NC-4.0".into();
        let r = intake(&b);
        let msg = r.faults[0].to_string();
        assert!(msg.contains("non-commercial"), "{msg}");
        assert!(
            !msg.contains("not on the allowlist"),
            "should be DENIED, not UNKNOWN: {msg}"
        );
    }

    #[test]
    fn copyleft_and_gfdl_are_refused() {
        for (spdx, needle) in [
            ("GPL-3.0-only", "copyleft"),
            ("AGPL-3.0", "copyleft"),
            ("GFDL-1.3", "GFDL"),
            ("CC-BY-ND-4.0", "no-derivatives"),
        ] {
            let mut b = good_bundle();
            b.license = spdx.into();
            let r = intake(&b);
            let msg = r.faults[0].to_string();
            assert!(msg.contains(needle), "{spdx}: {msg}");
        }
    }

    // -----------------------------------------------------------------
    // Criterion 3: an intake check that passes/fails on its own merits
    // -----------------------------------------------------------------

    #[test]
    fn every_fault_is_collected_not_just_the_first() {
        // So a submitter fixes them in one CI run rather than one per run.
        let b = Bundle {
            id: "bad".into(),
            version: "0.1.0".into(),
            license: String::new(),
            provenance: String::new(),
            members: vec![
                member("a.dat", "data", b"x"),
                member("b.nes", "profile", b"NES\x1a"),
            ],
        };
        let r = intake(&b);
        assert!(r.faults.len() >= 4, "{}", r.summary());
        assert!(r.faults.contains(&IntakeFault::MissingLicense));
        assert!(r.faults.contains(&IntakeFault::MissingProvenance));
    }

    #[test]
    fn an_empty_bundle_is_refused_rather_than_installed_as_a_noop() {
        let mut b = good_bundle();
        b.members.clear();
        let r = intake(&b);
        assert!(r.faults.contains(&IntakeFault::Empty));
    }

    #[test]
    fn the_summary_states_what_passing_does_not_prove() {
        // The honesty requirement: "accepted" must not read as "contains
        // no game data", because no checker here can establish that.
        let r = intake(&good_bundle());
        let s = r.summary();
        assert!(s.contains("ACCEPTED"), "{s}");
        assert!(s.contains("NOT proof"), "a pass must not overclaim: {s}");
    }

    #[test]
    fn a_refused_summary_lists_the_faults() {
        let mut b = good_bundle();
        b.license = String::new();
        let s = intake(&b).summary();
        assert!(s.contains("REFUSED"), "{s}");
        assert!(s.contains("license"), "{s}");
    }

    #[test]
    fn a_text_kind_holding_binary_is_a_content_mismatch() {
        let mut b = good_bundle();
        b.members
            .push(member("notes.txt", "readme", &[0xff, 0xfe, 0x00]));
        let r = intake(&b);
        assert!(r
            .faults
            .iter()
            .any(|f| matches!(f, IntakeFault::ContentMismatch { .. })));
    }

    #[test]
    fn member_kinds_round_trip_through_their_names() {
        for k in [
            MemberKind::Profile,
            MemberKind::PackManifest,
            MemberKind::PackImage,
            MemberKind::ModPatch,
            MemberKind::License,
            MemberKind::Readme,
        ] {
            assert_eq!(MemberKind::from_name(k.name()), Some(k));
        }
        // And there is no escape hatch.
        assert_eq!(MemberKind::from_name("data"), None);
        assert_eq!(MemberKind::from_name("other"), None);
    }
}
