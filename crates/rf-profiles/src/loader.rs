//! Profile loading: TOML text/file → `LoadOutcome` (FR-PROF-001,
//! FR-PROF-004, NFR-008).
//!
//! Order matters and is deliberate (see module tests): the document is
//! parsed generically first, `meta.profile_version`'s major is checked
//! *before* anything else, then unknown keys are collected, and only then
//! is the typed `schema::Profile` deserialize attempted. A profile that is
//! both newer-major *and* structurally broken must fail with the
//! newer-major error, not a confusing type error — checking version first
//! is what guarantees that.
//!
//! `warns on unknown keys` (§1) is structural, not incidental:
//! `LoadOutcome` carries `warnings` out of the loader rather than logging
//! and discarding them, so a caller (the CLI, a test) cannot merely fail to
//! print them without also failing to see them at all.

use std::path::Path;

use serde::Deserialize;

use crate::error::ProfileError;
use crate::schema::{Console, Profile, SUPPORTED_PROFILE_MAJOR};
use crate::shape;

/// Highest legal `[atmosphere].plane` index for a console (ticket W16-10):
/// NES has one background plane (index 0); SNES has up to four (0-3),
/// per `rf_core_api::PixelLayer::Background(n)`'s own doc that BG mode
/// decides which of 0-3 exist.
fn max_atmosphere_plane(console: Console) -> u8 {
    match console {
        Console::Nes => 0,
        Console::Snes => 3,
    }
}

/// A successfully-loaded profile plus any unknown-key warnings collected
/// along the way (FR-PROF-004: "warns on unknown keys" — this is the type
/// that keeps the warning from being swallowed).
#[derive(Debug, Clone, PartialEq)]
pub struct LoadOutcome {
    pub profile: Profile,
    /// Dotted key paths not part of schema v0, e.g. `"decode.metatlie"`.
    pub warnings: Vec<String>,
}

/// Load and validate a profile from its TOML text.
pub fn load_str(text: &str) -> Result<LoadOutcome, ProfileError> {
    let value: toml::Value =
        toml::from_str(text).map_err(|e| ProfileError::Parse(e.to_string()))?;

    let version_str = value
        .get("meta")
        .and_then(|m| m.get("profile_version"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProfileError::MissingField("meta.profile_version".to_string()))?;
    let (major, _minor) = parse_major_minor(version_str)?;
    if major > SUPPORTED_PROFILE_MAJOR {
        return Err(ProfileError::NewerMajor {
            found: major,
            supported: SUPPORTED_PROFILE_MAJOR,
        });
    }

    let warnings = shape::unknown_keys(&value);

    let profile =
        Profile::deserialize(value).map_err(|e| ProfileError::Deserialize(e.to_string()))?;

    for (idx, entry) in profile.identity.iter().enumerate() {
        let has_hash = entry.sha256.is_some()
            || entry.sha1.is_some()
            || entry.md5.is_some()
            || entry.crc32.is_some();
        if !has_hash {
            return Err(ProfileError::IdentityMissingHash(idx));
        }
    }

    // FR-PROF-003, enforced (ticket W4-02a). W4-02 added `source` as an
    // OPTIONAL field and deliberately did not implement the
    // fail-without-one half, since its own acceptance criteria did not
    // cite this requirement — a scope decision, recorded rather than
    // assumed away, which is why this exists as its own ticket.
    //
    // Both tables, checked in declaration order so the FIRST offending
    // row is the one reported: someone fixing a profile wants the top of
    // the list, not an arbitrary member of it.
    for (idx, entry) in profile.memory_map.iter().enumerate() {
        if entry.source.as_ref().is_none_or(|s| s.trim().is_empty()) {
            return Err(ProfileError::MapEntryMissingSource {
                table: "memory_map",
                index: idx,
                label: entry.label.clone(),
            });
        }
    }
    for (idx, entry) in profile.rom_map.iter().enumerate() {
        if entry.source.as_ref().is_none_or(|s| s.trim().is_empty()) {
            return Err(ProfileError::MapEntryMissingSource {
                table: "rom_map",
                index: idx,
                label: entry.label.clone(),
            });
        }
    }

    // `[atmosphere]` (ticket W16-10; GAME_PROFILES.md §2): plane range is
    // per-console, so it is checked here rather than at the type level —
    // the same reason `[[identity]]`'s hash check and the `source`
    // citation checks above live in the loader and not in `schema.rs`.
    if let Some(atm) = profile.atmosphere.as_ref() {
        let max = max_atmosphere_plane(profile.meta.console);
        if atm.plane > max {
            let console = match profile.meta.console {
                Console::Nes => "nes",
                Console::Snes => "snes",
            };
            return Err(ProfileError::AtmospherePlaneOutOfRange {
                console,
                plane: atm.plane,
                max,
            });
        }
        if let Some(strength) = atm.strength {
            if !(0.0..=1.0).contains(&strength) {
                return Err(ProfileError::AtmosphereStrengthOutOfRange(strength));
            }
        }
        if let Some(tint) = atm.tint.as_ref() {
            let valid = tint.len() == 7
                && tint.starts_with('#')
                && tint[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !valid {
                return Err(ProfileError::AtmosphereInvalidTint(tint.clone()));
            }
        }
    }

    Ok(LoadOutcome { profile, warnings })
}

/// Load and validate a profile from a file path.
pub fn load_file(path: &Path) -> Result<LoadOutcome, ProfileError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ProfileError::Io(format!("{}: {e}", path.display())))?;
    load_str(&text)
}

/// Split `"MAJOR.MINOR"` (or bare `"MAJOR"`) into numeric components.
fn parse_major_minor(s: &str) -> Result<(u64, u64), ProfileError> {
    let mut parts = s.splitn(2, '.');
    let major = parts
        .next()
        .unwrap_or("")
        .parse::<u64>()
        .map_err(|_| ProfileError::InvalidVersion(s.to_string()))?;
    let minor = parts
        .next()
        .and_then(|m| m.parse::<u64>().ok())
        .unwrap_or(0);
    Ok((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
        [meta]
        profile_version = "0.1"
        title = "Example Platformer"
        console = "nes"
        region = "ntsc"

        [[identity]]
        sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd"
    "#;

    /// Vacuity trap (a), first half: a test that only parses a VALID
    /// profile cannot detect a loader that accepts anything. This one
    /// parses successfully — the *next* tests are the ones that would
    /// actually catch a broken loader.
    #[test]
    fn parses_a_valid_profile() {
        let outcome = load_str(VALID).expect("valid profile parses");
        assert_eq!(outcome.profile.meta.title, "Example Platformer");
        assert!(outcome.warnings.is_empty());
    }

    /// Vacuity trap (a), second half: a newer-major profile MUST be
    /// rejected. Mutation: bump `SUPPORTED_PROFILE_MAJOR` to 1 and this
    /// fails.
    #[test]
    fn rejects_a_newer_major_profile_version() {
        let text = VALID.replace("profile_version = \"0.1\"", "profile_version = \"1.0\"");
        let err = load_str(&text).expect_err("newer major must be rejected");
        assert!(matches!(
            err,
            ProfileError::NewerMajor {
                found: 1,
                supported: 0
            }
        ));
    }

    /// A newer *minor*, same major, is schema v0's own documented forward
    /// path (GAME_PROFILES.md §2: "schema v0.2 adds decode.family_version")
    /// and must be accepted, not rejected by an accidental string
    /// comparison instead of a numeric major comparison.
    #[test]
    fn accepts_a_newer_minor_with_same_major() {
        let text = VALID.replace("profile_version = \"0.1\"", "profile_version = \"0.99\"");
        load_str(&text).expect("newer minor, same major, must load");
    }

    /// A profile that is BOTH newer-major AND structurally broken
    /// elsewhere must fail with the newer-major error specifically —
    /// proving the version check runs before the typed deserialize would
    /// otherwise surface a confusing, unreachable type error instead.
    #[test]
    fn newer_major_error_wins_over_other_structural_problems() {
        let text = VALID
            .replace("profile_version = \"0.1\"", "profile_version = \"7.0\"")
            .replace("title = \"Example Platformer\"", ""); // also drop a required field
        let err = load_str(&text).expect_err("must still fail");
        assert!(matches!(err, ProfileError::NewerMajor { found: 7, .. }));
    }

    // ---------------------------------------------------------------
    // FR-PROF-003 (ticket W4-02a): "Every memory_map/rom_map entry shall
    // carry a `source` citation (clean-room provenance); validation shall
    // fail without one."
    // ---------------------------------------------------------------

    const MEMORY_ROW: &str = r#"
        [[memory_map]]
        addr = 0x0300
        len = 2
        type = "u16le"
        label = "player_x"
    "#;

    const ROM_ROW: &str = r#"
        [[rom_map]]
        offset = 0x8000
        len = 16
        type = "metatile_table"
        label = "metatiles"
    "#;

    #[test]
    fn a_memory_map_entry_without_a_source_fails_and_names_the_entry() {
        let err = load_str(&format!("{VALID}{MEMORY_ROW}"))
            .expect_err("FR-PROF-003: validation shall fail without a source");
        match &err {
            ProfileError::MapEntryMissingSource {
                table,
                index,
                label,
            } => {
                assert_eq!(*table, "memory_map");
                assert_eq!(*index, 0);
                assert_eq!(
                    label, "player_x",
                    "the LABEL is what someone can search for"
                );
            }
            other => panic!("wrong error: {other:?}"),
        }
        // The message must actually name it — an error type carrying the
        // label is no use if `Display` drops it.
        let rendered = err.to_string();
        assert!(rendered.contains("memory_map"), "{rendered}");
        assert!(rendered.contains("player_x"), "{rendered}");
    }

    #[test]
    fn a_rom_map_entry_without_a_source_fails_too() {
        let err = load_str(&format!("{VALID}{ROM_ROW}"))
            .expect_err("FR-PROF-003 names rom_map as well as memory_map");
        assert!(matches!(
            err,
            ProfileError::MapEntryMissingSource {
                table: "rom_map",
                index: 0,
                ..
            }
        ));
    }

    /// A present-but-blank `source` is not a citation. Without this, the
    /// rule is trivially satisfiable by `source = ""`, which would make
    /// the whole provenance requirement decorative.
    #[test]
    fn a_blank_source_does_not_count_as_a_citation() {
        for blank in ["\"\"", "\"   \""] {
            let text = format!("{VALID}{MEMORY_ROW}\n        source = {blank}\n");
            assert!(
                matches!(
                    load_str(&text),
                    Err(ProfileError::MapEntryMissingSource { .. })
                ),
                "source = {blank} must not satisfy FR-PROF-003"
            );
        }
    }

    /// The positive half: with a real citation the same profile loads
    /// clean. Without this, every test above would pass on a loader that
    /// rejected all map entries outright.
    #[test]
    fn a_cited_entry_loads_clean() {
        let text =
            format!("{VALID}{MEMORY_ROW}\n        source = \"docs/research/nes-ram-map.md\"\n");
        let outcome = load_str(&text).expect("a cited entry must load");
        assert_eq!(outcome.profile.memory_map.len(), 1);
        assert_eq!(
            outcome.profile.memory_map[0].source.as_deref(),
            Some("docs/research/nes-ram-map.md")
        );
    }

    /// Vacuity trap (a), unknown-key half: an unknown key must produce a
    /// warning that is actually returned, not swallowed. Mutation: make
    /// `shape::unknown_keys` return an empty vec unconditionally and this
    /// fails.
    #[test]
    fn warns_on_an_unknown_key_and_surfaces_it_in_the_outcome() {
        let text = format!("{VALID}\n[bogus_section]\nx = 1\n");
        let outcome = load_str(&text).expect("unknown key must warn, not fail");
        assert!(
            outcome.warnings.contains(&"bogus_section".to_string()),
            "{:?}",
            outcome.warnings
        );
    }

    /// Vacuity trap (b): an `[[identity]]` entry with zero hash families
    /// identifies nothing and must be rejected at load time, with its own
    /// test distinct from the alias-matching tests in `schema`.
    #[test]
    fn rejects_an_identity_entry_with_no_hash_family() {
        let text = VALID.replace(
            "sha256 = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd\"",
            "revision = \"1.0\"",
        );
        let err = load_str(&text).expect_err("hashless identity entry must be rejected");
        assert!(matches!(err, ProfileError::IdentityMissingHash(0)));
    }

    // ---------------------------------------------------------------
    // `[atmosphere]` (ticket W16-10; GAME_PROFILES.md §2).
    // ---------------------------------------------------------------

    /// A valid `[atmosphere]` table on an NES profile (plane 0 is the
    /// only legal NES plane) loads clean, with `ladder` present.
    #[test]
    fn a_valid_atmosphere_table_loads_clean() {
        let text = format!(
            "{VALID}\n[atmosphere]\nplane = 0\ntint = \"#336699\"\nstrength = 0.5\nladder = \"shadow\"\n"
        );
        let outcome = load_str(&text).expect("valid [atmosphere] must load");
        let atm = outcome
            .profile
            .atmosphere
            .expect("atmosphere section must be present");
        assert_eq!(atm.plane, 0);
        assert_eq!(atm.tint.as_deref(), Some("#336699"));
        assert_eq!(atm.strength, Some(0.5));
        assert_eq!(atm.ladder, Some(crate::schema::AtmosphereLadder::Shadow));
    }

    /// A minimal `[atmosphere]` table (`plane` only) also loads clean —
    /// `tint`/`strength`/`ladder` are all optional.
    #[test]
    fn a_minimal_atmosphere_table_loads_clean() {
        let text = format!("{VALID}\n[atmosphere]\nplane = 0\n");
        let outcome = load_str(&text).expect("a plane-only [atmosphere] must load");
        let atm = outcome.profile.atmosphere.expect("must be present");
        assert_eq!(atm.plane, 0);
        assert!(atm.tint.is_none());
        assert!(atm.strength.is_none());
        assert!(atm.ladder.is_none());
    }

    /// An NES profile naming plane 1 must be rejected — NES has exactly
    /// one background plane (index 0).
    #[test]
    fn an_atmosphere_plane_out_of_range_for_nes_is_rejected() {
        let text = format!("{VALID}\n[atmosphere]\nplane = 1\n");
        let err = load_str(&text).expect_err("plane 1 is out of range for NES");
        assert!(matches!(
            err,
            ProfileError::AtmospherePlaneOutOfRange {
                console: "nes",
                plane: 1,
                max: 0,
            }
        ));
    }

    /// A SNES profile allows planes 0-3 but rejects plane 4.
    #[test]
    fn an_atmosphere_plane_out_of_range_for_snes_is_rejected() {
        let snes = VALID.replace("console = \"nes\"", "console = \"snes\"");
        let text = format!("{snes}\n[atmosphere]\nplane = 4\n");
        let err = load_str(&text).expect_err("plane 4 is out of range for SNES");
        assert!(matches!(
            err,
            ProfileError::AtmospherePlaneOutOfRange {
                console: "snes",
                plane: 4,
                max: 3,
            }
        ));
        // The boundary itself (plane 3) must load clean.
        let ok_text = format!("{snes}\n[atmosphere]\nplane = 3\n");
        load_str(&ok_text).expect("plane 3 is the top of the SNES range");
    }

    /// `strength` outside `0.0..=1.0` is rejected.
    #[test]
    fn an_atmosphere_strength_out_of_range_is_rejected() {
        let text = format!("{VALID}\n[atmosphere]\nplane = 0\nstrength = 1.5\n");
        let err = load_str(&text).expect_err("strength 1.5 is out of range");
        assert!(matches!(
            err,
            ProfileError::AtmosphereStrengthOutOfRange(v) if v == 1.5
        ));
    }

    /// A `tint` that is not `#rrggbb` is rejected.
    #[test]
    fn an_atmosphere_tint_that_is_not_hex_is_rejected() {
        for bad in ["blue", "#zzzzzz", "336699", "#3366"] {
            let text = format!("{VALID}\n[atmosphere]\nplane = 0\ntint = \"{bad}\"\n");
            let err = load_str(&text).expect_err(&format!("`{bad}` must be rejected"));
            assert!(
                matches!(err, ProfileError::AtmosphereInvalidTint(ref v) if v == bad),
                "{bad}: {err}"
            );
        }
    }

    /// An unrecognised `ladder` value is a schema error (the enum
    /// deserialize rejects it), not silently accepted.
    #[test]
    fn an_unrecognised_atmosphere_ladder_value_is_rejected() {
        let text = format!("{VALID}\n[atmosphere]\nplane = 0\nladder = \"bogus\"\n");
        let err = load_str(&text).expect_err("an unknown ladder rung must be rejected");
        assert!(matches!(err, ProfileError::Deserialize(_)), "{err}");
    }

    #[test]
    fn missing_profile_version_names_the_field() {
        let text = r#"
            [meta]
            title = "t"
            console = "nes"
            region = "ntsc"
        "#;
        let err = load_str(text).expect_err("missing profile_version must error");
        match err {
            ProfileError::MissingField(path) => assert_eq!(path, "meta.profile_version"),
            other => panic!("expected MissingField, got {other:?}"),
        }
    }
}
