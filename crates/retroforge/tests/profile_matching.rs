//! **A profile matches on any hash family it declares** (ticket W11-09).
//!
//! `rf_profiles::schema::IdentityEntry` has carried four families —
//! sha256, sha1, md5, crc32 — since W4-02, and `IdentityEntry::matches`
//! correctly checks every one an entry specifies.
//! `level_view::find_matching_profile`, the only matcher the app calls,
//! compared normalized sha256 and ignored the other three. The schema's
//! own matcher was dead code as far as the product went, and a profile
//! identified by a published No-Intro CRC32 — the realistic way to name a
//! commercial title nobody in this project holds a copy of — loaded
//! cleanly and matched nothing, silently, for ever.
//!
//! Two matchers for one schema is how that happens. There is one now,
//! and this is what holds it there.

use std::path::PathBuf;

use retroforge::level_view::find_matching_profile;

/// A minimal, valid profile whose `[[identity]]` names exactly the given
/// hash family — the shape a real authored profile has, not a stub.
fn write_profile(dir: &std::path::Path, name: &str, identity_line: &str) -> PathBuf {
    let sub = dir.join(name);
    std::fs::create_dir_all(&sub).expect("profile dir");
    let path = sub.join("profile.toml");
    std::fs::write(
        &path,
        format!(
            r#"
[meta]
profile_version = "0.1"
title = "{name}"
console = "nes"
region = "ntsc"
authors = ["W11-09 test"]
sources = ["docs/design/GAME_PROFILES.md"]
license = "MIT"

[[identity]]
{identity_line}
revision = "1.0"

[capabilities]
"#
        ),
    )
    .expect("write profile");
    path
}

fn hashes(crc32: &str, md5: &str, sha1: &str, sha256: &str) -> rf_cart::RomHashes {
    rf_cart::RomHashes {
        crc32: crc32.to_string(),
        md5: md5.to_string(),
        sha1: sha1.to_string(),
        sha256: sha256.to_string(),
    }
}

const CRC: &str = "a1b2c3d4";
const MD5: &str = "0123456789abcdef0123456789abcdef";
const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn every_declared_hash_family_can_identify_a_rom() {
    let root = std::env::temp_dir().join(format!("rf_match_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("root");

    let by = hashes(CRC, MD5, SHA1, SHA256);
    for (name, line) in [
        ("by-crc32", format!("crc32 = \"{CRC}\"")),
        ("by-md5", format!("md5 = \"{MD5}\"")),
        ("by-sha1", format!("sha1 = \"{SHA1}\"")),
        ("by-sha256", format!("sha256 = \"{SHA256}\"")),
    ] {
        let one = root.join(name);
        std::fs::create_dir_all(&one).expect("dir");
        write_profile(&one, "p", &line);
        let found = find_matching_profile(&one, &by);
        assert!(
            found.is_some(),
            "a profile identified by {name} did not match a ROM with that exact hash. Before \
             W11-09 only sha256 was consulted, so three of these four failed silently."
        );
    }
}

/// **The negative half, which is the half that matters.** A matcher that
/// returned `Some` unconditionally would pass every assertion above.
#[test]
fn a_wrong_hash_does_not_match() {
    let root = std::env::temp_dir().join(format!("rf_match_neg_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("root");
    write_profile(&root, "p", &format!("crc32 = \"{CRC}\""));

    let other = hashes("ffffffff", MD5, SHA1, SHA256);
    assert!(
        find_matching_profile(&root, &other).is_none(),
        "a profile declaring crc32 = {CRC} matched a ROM whose crc32 is ffffffff — the \
         matcher is not comparing, it is agreeing"
    );
}

/// Case-insensitivity is the schema's documented behaviour ("profiles may
/// author hex in any case"), and it is worth pinning at the app's matcher
/// too: a profile authored from a datfile that prints uppercase hex is
/// the ordinary case, not an edge one.
#[test]
fn hex_case_does_not_decide_a_match() {
    let root = std::env::temp_dir().join(format!("rf_match_case_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("root");
    write_profile(&root, "p", &format!("crc32 = \"{}\"", CRC.to_uppercase()));
    assert!(
        find_matching_profile(&root, &hashes(CRC, MD5, SHA1, SHA256)).is_some(),
        "an uppercase crc32 in a profile must match a lowercase one from rf-cart"
    );
}
