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
use crate::schema::{Profile, SUPPORTED_PROFILE_MAJOR};
use crate::shape;

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
