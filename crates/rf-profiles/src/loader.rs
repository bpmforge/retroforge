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

    if let Some(scout) = profile.scout.as_ref() {
        validate_scout(scout, profile.meta.console)?;
    }
    if let Some(scene) = profile.scene.as_ref() {
        validate_scene(scene, profile.meta.console)?;
    }

    // `[overclock]` (ticket W30-01): the cap is a whole-hundred percent.
    if let Some(max) = profile.overclock.as_ref().and_then(|o| o.superfx_max) {
        if !(100..=800).contains(&max) || max % 100 != 0 {
            return Err(ProfileError::OverclockSuperfxMaxInvalid(max));
        }
    }
    // Ticket W30-08: the CPU/SA-1 ceilings, the NES line cap, the tick
    // counter's WRAM address and the target rate.
    if let Some(oc) = profile.overclock.as_ref() {
        let bad = |key, value: u32, valid| ProfileError::OverclockInvalid { key, value, valid };
        for (key, max) in [("cpu_max", oc.cpu_max), ("sa1_max", oc.sa1_max)] {
            if let Some(v) = max {
                if !(100..=400).contains(&v) || v % 100 != 0 {
                    return Err(bad(key, u32::from(v), "100..=400 in steps of 100"));
                }
            }
        }
        if let Some(v) = oc.nes_extra_lines_max.filter(|v| *v > 240) {
            return Err(bad("nes_extra_lines_max", u32::from(v), "0..=240"));
        }
        if let Some(v) = oc
            .tick_counter
            .filter(|v| !(0x7E_0000..=0x7F_FFFF).contains(v))
        {
            return Err(bad(
                "tick_counter",
                v,
                "a WRAM address, 0x7E0000..=0x7FFFFF",
            ));
        }
        if let Some(v) = oc.target_tick_hz.filter(|v| !(1..=60).contains(v)) {
            return Err(bad("target_tick_hz", v, "1..=60"));
        }
    }

    if let Some(sp) = profile.superfx_polygons.as_ref() {
        validate_superfx_polygons(sp)?;
    }

    if let Some(heights) = profile.heights.as_ref() {
        validate_heights(heights)?;
    }

    Ok(LoadOutcome { profile, warnings })
}

/// `[superfx_polygons]` (ticket W31-02): every error names the offending key.
fn validate_superfx_polygons(sp: &crate::schema::SuperfxPolygons) -> Result<(), ProfileError> {
    let bad =
        |key, value: String, valid| ProfileError::SuperfxPolygonsInvalid { key, value, valid };
    let scalar = |key, value: u32, max: u32, valid| {
        if value <= max {
            Ok(())
        } else {
            Err(bad(key, format!("{value} (0x{value:X})"), valid))
        }
    };
    scalar("pc_bank", sp.pc_bank, 0xFF, "0x00..=0xFF")?;
    scalar("count_read_pc", sp.count_read_pc, 0xFFFF, "0x0000..=0xFFFF")?;
    scalar("count_addr", sp.count_addr, 0xFFFF, "0x0000..=0xFFFF")?;
    // The observer reads up to 0x100 bytes from the buffer.
    scalar("buffer_addr", sp.buffer_addr, 0xFF00, "0x0000..=0xFF00")?;
    match sp.walker_pcs[..] {
        [lo, hi] if lo < hi && hi <= 0x1_0000 => {}
        _ => {
            return Err(bad(
                "walker_pcs",
                format!("{:?}", sp.walker_pcs),
                "[start, end) with start < end <= 0x10000",
            ))
        }
    }
    match sp.viewport[..] {
        [w, h] if (1..=256).contains(&w) && (1..=256).contains(&h) => {}
        _ => {
            return Err(bad(
                "viewport",
                format!("{:?}", sp.viewport),
                "[width, height], each 1..=256",
            ))
        }
    }
    Ok(())
}

/// `[heights]` (ticket W30-06): every error names the offending key.
fn validate_heights(heights: &crate::schema::Heights) -> Result<(), ProfileError> {
    use crate::schema::{Heights, HEIGHTS_MAX_EIGHTHS};
    let bad = |key: String, value: i64, valid| ProfileError::HeightsInvalid {
        key,
        value: value.to_string(),
        valid,
    };
    if !(1..=4).contains(&heights.layer) {
        return Err(bad("layer".into(), heights.layer, "1..=4"));
    }
    if ![8, 16].contains(&heights.tile_px) {
        return Err(bad("tile_px".into(), heights.tile_px, "8 or 16"));
    }
    let mut seen = std::collections::HashSet::new();
    for (key, value) in &heights.tiles {
        let named = format!("tiles.\"{key}\"");
        let Some(tile) = Heights::parse_tile_key(key) else {
            return Err(ProfileError::HeightsInvalid {
                key: named,
                value: value.to_string(),
                valid: "a hex tile number 0x000..=0x3FF as the key",
            });
        };
        if !seen.insert(tile) {
            return Err(ProfileError::HeightsInvalid {
                key: named,
                value: value.to_string(),
                valid: "each tile number listed once (this key repeats another)",
            });
        }
        if !(0..=HEIGHTS_MAX_EIGHTHS).contains(value) {
            return Err(bad(named, *value, "0..=64 eighths"));
        }
    }
    Ok(())
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

/// `[scout]` (ticket W32-07): strategy, ranges, and the address rows (each
/// cited, each inside the console's RAM, the roles the strategy needs).
fn validate_scout(scout: &crate::schema::Scout, console: Console) -> Result<(), ProfileError> {
    let bad = |why: String| ProfileError::ScoutInvalid(why);
    if !matches!(
        scout.strategy.as_str(),
        "player_move" | "camera_write" | "walk"
    ) {
        return Err(bad(format!(
            "strategy `{}` (valid: player_move, camera_write, walk)",
            scout.strategy
        )));
    }
    if scout.strategy == "walk" && console != Console::Nes {
        return Err(bad("strategy walk is for NES profiles".into()));
    }
    for (key, v, lo, hi) in [
        ("step_px", u32::from(scout.step_px), 1, 64),
        ("frame_budget", scout.frame_budget, 1, 20_000),
        ("stall_frames", scout.stall_frames.unwrap_or(120), 1, 3_000),
        ("settle_frames", scout.settle_frames.unwrap_or(90), 0, 3_600),
    ] {
        if !(lo..=hi).contains(&v) {
            return Err(bad(format!("{key} = {v} (valid: {lo}..={hi})")));
        }
    }
    if let Some(dirs) = scout.dirs.as_ref() {
        if dirs.is_empty()
            || dirs
                .iter()
                .any(|d| !matches!(d.as_str(), "right" | "left" | "up" | "down"))
        {
            return Err(bad(
                "dirs must be a non-empty list of \"right\" / \"left\" / \"up\" / \"down\"".into(),
            ));
        }
    }
    let (lo, hi): (u32, u32) = match console {
        Console::Snes => (0x7E_0000, 0x7F_FFFF),
        Console::Nes => (0, 0xFFFF),
    };
    for (i, row) in scout.addr.iter().enumerate() {
        if !matches!(
            row.role.as_str(),
            "camera_x"
                | "camera_y"
                | "player_x"
                | "player_x_hi"
                | "player_y"
                | "player_y_hi"
                | "hold"
                | "start_when"
        ) {
            return Err(bad(format!(
                "addr row {i}: role `{}` (valid: camera_x, camera_y, player_x, player_x_hi, player_y, player_y_hi, hold, start_when)",
                row.role
            )));
        }
        if !(lo..=hi).contains(&row.addr) {
            return Err(bad(format!(
                "addr row {i}: address 0x{:X} outside 0x{lo:X}..=0x{hi:X}",
                row.addr
            )));
        }
        if matches!(row.role.as_str(), "hold" | "start_when") && row.value.is_none() {
            return Err(bad(format!(
                "addr row {i}: a {} row needs a `value`",
                row.role
            )));
        }
        if row.source.as_ref().is_none_or(|s| s.trim().is_empty()) {
            return Err(bad(format!(
                "addr row {i} (0x{:X}) has no `source` (FR-PROF-003)",
                row.addr
            )));
        }
    }
    let has = |role: &str| scout.addr.iter().any(|r| r.role == role);
    // The SNES scout reads a 16-bit camera word; the NES one watches the
    // composite stitcher's own world x (W32-09), so a NES table may omit it.
    if console == Console::Snes && !has("camera_x") {
        return Err(bad("needs a camera_x row (the stop rules read it)".into()));
    }
    if scout.strategy == "player_move" && !has("player_x") {
        return Err(bad("player_move needs a player_x row".into()));
    }
    Ok(())
}

/// `[scene]` (ticket W33-09): each `level_id` row cited, inside the console's
/// RAM, 1..=4 bytes wide, its `idle` value representable; at most 8 rows and
/// 16 bytes of key; `sections` one of the known values.
fn validate_scene(scene: &crate::schema::Scene, console: Console) -> Result<(), ProfileError> {
    let bad = |why: String| ProfileError::SceneInvalid(why);
    if let Some(sections) = scene.sections.as_deref() {
        if sections != "axis" {
            return Err(bad(format!("sections `{sections}` (valid: \"axis\")")));
        }
    }
    if scene.level_id.len() > MAX_LEVEL_ID_ROWS {
        return Err(bad(format!(
            "{} level_id rows (at most {MAX_LEVEL_ID_ROWS})",
            scene.level_id.len()
        )));
    }
    let (lo, hi): (u32, u32) = match console {
        Console::Snes => (0x7E_0000, 0x7F_FFFF),
        Console::Nes => (0, 0xFFFF),
    };
    let mut total = 0u32;
    for (i, row) in scene.level_id.iter().enumerate() {
        let len = row.len.unwrap_or(1);
        if !(1..=4).contains(&len) {
            return Err(bad(format!("level_id row {i}: len = {len} (valid: 1..=4)")));
        }
        total += u32::from(len);
        if !(lo..=hi).contains(&row.addr) || row.addr + u32::from(len) - 1 > hi {
            return Err(bad(format!(
                "level_id row {i}: address 0x{:X} (+{len} bytes) outside 0x{lo:X}..=0x{hi:X}",
                row.addr
            )));
        }
        if len < 4 && row.idle.unwrap_or(0) >> (8 * u32::from(len)) != 0 {
            return Err(bad(format!(
                "level_id row {i}: idle 0x{:X} does not fit in {len} byte(s)",
                row.idle.unwrap_or(0)
            )));
        }
        if row.source.as_ref().is_none_or(|s| s.trim().is_empty()) {
            return Err(bad(format!(
                "level_id row {i} (0x{:X}) has no `source` (FR-PROF-003)",
                row.addr
            )));
        }
    }
    if total > MAX_LEVEL_ID_BYTES {
        return Err(bad(format!(
            "level_id rows read {total} bytes (at most {MAX_LEVEL_ID_BYTES})"
        )));
    }
    Ok(())
}

/// Most `[[scene.level_id]]` rows a profile may declare.
pub const MAX_LEVEL_ID_ROWS: usize = 8;
/// Most bytes all rows together may read.
pub const MAX_LEVEL_ID_BYTES: u32 = 16;

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

    const SUPERFX_POLYGONS: &str = "[superfx_polygons]\npc_bank = 0x01\ncount_read_pc = 0x8DE0\n\
        count_addr = 0x0134\nbuffer_addr = 0x0982\nwalker_pcs = [0xA6C0, 0xA740]\n\
        viewport = [224, 170]\n";

    /// Ticket W31-02: `[superfx_polygons]` loads when valid, with hex values.
    #[test]
    fn superfx_polygons_load() {
        let outcome = load_str(&format!("{VALID}\n{SUPERFX_POLYGONS}")).expect("valid");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        let sp = outcome.profile.superfx_polygons.expect("section");
        assert_eq!(
            (sp.pc_bank, sp.count_read_pc, sp.count_addr, sp.buffer_addr),
            (1, 0x8DE0, 0x134, 0x982)
        );
        assert_eq!(
            (sp.walker_pcs, sp.viewport),
            (vec![0xA6C0, 0xA740], vec![224, 170])
        );
    }

    /// Ticket W31-02: every bad value is rejected by name; a missing key is
    /// a parse error; an unknown key warns.
    #[test]
    fn superfx_polygons_are_validated() {
        let with =
            |from: &str, to: &str| format!("{VALID}\n{}", SUPERFX_POLYGONS.replace(from, to));
        for (from, to, key) in [
            ("pc_bank = 0x01", "pc_bank = 0x100", "pc_bank"),
            (
                "count_read_pc = 0x8DE0",
                "count_read_pc = 0x10000",
                "count_read_pc",
            ),
            ("count_addr = 0x0134", "count_addr = 0x10000", "count_addr"),
            (
                "buffer_addr = 0x0982",
                "buffer_addr = 0xFF01",
                "buffer_addr",
            ),
            ("[0xA6C0, 0xA740]", "[0xA740, 0xA6C0]", "walker_pcs"),
            ("[0xA6C0, 0xA740]", "[0xA6C0, 0xA6C0]", "walker_pcs"),
            ("[0xA6C0, 0xA740]", "[0xA6C0]", "walker_pcs"),
            ("[0xA6C0, 0xA740]", "[0xA6C0, 0x10001]", "walker_pcs"),
            ("[224, 170]", "[0, 170]", "viewport"),
            ("[224, 170]", "[224, 257]", "viewport"),
            ("[224, 170]", "[224, 170, 1]", "viewport"),
        ] {
            let err = load_str(&with(from, to)).expect_err(to);
            assert!(
                err.to_string()
                    .contains(&format!("[superfx_polygons].{key}")),
                "{to}: {err}"
            );
        }
        let edge = with("[0xA6C0, 0xA740]", "[0xA6C0, 0x10000]").replace("0x0982", "0xFF00");
        load_str(&edge).expect("the largest legal walker range and buffer");
        let missing = with("count_addr = 0x0134\n", "");
        assert!(load_str(&missing).is_err(), "all keys are required");
        let unknown = format!("{VALID}\n{SUPERFX_POLYGONS}bogus = 1\n");
        let warnings = load_str(&unknown).expect("unknown key warns").warnings;
        assert!(warnings.iter().any(|w| w.contains("bogus")), "{warnings:?}");
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
    // `[scout]` (ticket W32-07).
    // ---------------------------------------------------------------

    const SCOUT: &str = r#"
        [scout]
        strategy = "player_move"
        step_px = 8
        frame_budget = 1500
        dirs = ["right", "left"]

        [[scout.addr]]
        role = "camera_x"
        addr = 0x6B
        source = "observed: test"

        [[scout.addr]]
        role = "player_x"
        addr = 0x6D
        source = "observed: test"

        [[scout.addr]]
        role = "hold"
        addr = 0x70
        value = 3
        source = "observed: test"
    "#;

    #[test]
    fn a_valid_scout_table_loads_clean() {
        let outcome = load_str(&format!("{VALID}\n{SCOUT}")).expect("valid [scout] loads");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        let scout = outcome.profile.scout.expect("scout section");
        assert_eq!(scout.step_px, 8);
        assert_eq!(scout.addr.len(), 3);
        assert_eq!(scout.addr[2].value, Some(3));
    }

    #[test]
    fn a_scout_table_may_pin_its_strategy_and_by_default_does_not() {
        let plain = load_str(&format!("{VALID}\n{SCOUT}")).expect("loads");
        assert_eq!(plain.profile.scout.expect("scout").pin, None);
        let pinned = SCOUT.replace(
            "frame_budget = 1500",
            "frame_budget = 1500\n        pin = true",
        );
        let outcome = load_str(&format!("{VALID}\n{pinned}")).expect("a pinned table loads");
        assert_eq!(outcome.profile.scout.expect("scout").pin, Some(true));
    }

    #[test]
    fn a_scout_table_with_a_bad_key_or_row_is_rejected_and_says_which() {
        let bad = |from: &str, to: &str, needle: &str| {
            let text = format!("{VALID}\n{}", SCOUT.replacen(from, to, 1));
            let err = load_str(&text).expect_err("must be rejected");
            assert!(matches!(err, ProfileError::ScoutInvalid(_)), "{err}");
            assert!(err.to_string().contains(needle), "{err}");
        };
        bad("\"player_move\"", "\"teleport\"", "strategy");
        bad("step_px = 8", "step_px = 0", "step_px");
        bad(
            "frame_budget = 1500",
            "frame_budget = 99999",
            "frame_budget",
        );
        bad("[\"right\", \"left\"]", "[\"sideways\"]", "dirs");
        bad("role = \"camera_x\"", "role = \"camera_z\"", "role");
        bad("addr = 0x6B", "addr = 0x1_0000", "outside");
        bad(
            "source = \"observed: test\"\n\n        [[scout.addr]]\n        role = \"player_x\"",
            "\n\n        [[scout.addr]]\n        role = \"player_x\"",
            "source",
        );
        bad("value = 3\n", "\n", "value");
        bad(
            "role = \"player_x\"",
            "role = \"hold\"\n        value = 1",
            "player_move needs a player_x",
        );
    }

    #[test]
    fn a_scout_table_needs_a_camera_row() {
        // The SNES scout reads a camera word (the NES one watches the stitcher).
        let snes = VALID.replacen("console = \"nes\"", "console = \"snes\"", 1);
        let text = SCOUT
            .replacen("role = \"camera_x\"", "role = \"player_x\"", 1)
            .replace("addr = 0x6B", "addr = 0x7E006B")
            .replace("addr = 0x6D", "addr = 0x7E006D")
            .replace("addr = 0x70", "addr = 0x7E0070");
        let err = load_str(&format!("{snes}\n{text}")).expect_err("no camera_x");
        assert!(err.to_string().contains("camera_x"), "{err}");
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

    /// Ticket W30-01: `[overclock].superfx_max` loads when valid and is
    /// rejected (naming the key) when out of range or not a multiple of 100.
    #[test]
    fn overclock_superfx_max_is_validated() {
        let ok = format!("{VALID}\n[overclock]\nsuperfx_max = 300\n");
        let outcome = load_str(&ok).expect("300 is a valid cap");
        assert_eq!(outcome.profile.overclock.unwrap().superfx_max, Some(300));
        for bad in [0, 50, 150, 900, 1000] {
            let text = format!("{VALID}\n[overclock]\nsuperfx_max = {bad}\n");
            let err = load_str(&text).expect_err("invalid cap must be rejected");
            assert!(matches!(err, ProfileError::OverclockSuperfxMaxInvalid(v) if v == bad));
            assert!(err.to_string().contains("[overclock].superfx_max"));
        }
    }

    /// Ticket W30-08: the governor's keys load when valid and are rejected,
    /// naming the key, when out of range.
    #[test]
    fn overclock_governor_keys_are_validated() {
        let ok = format!(
            "{VALID}\n[overclock]\ncpu_max = 300\nsa1_max = 400\nnes_extra_lines_max = 240\n\
             tick_counter = 0x7E0012\ntarget_tick_hz = 30\n"
        );
        let oc = load_str(&ok).expect("valid").profile.overclock.unwrap();
        assert_eq!(
            (oc.cpu_max, oc.sa1_max, oc.nes_extra_lines_max),
            (Some(300), Some(400), Some(240))
        );
        assert_eq!(
            (oc.tick_counter, oc.target_tick_hz),
            (Some(0x7E0012), Some(30))
        );
        for (key, bad) in [
            ("cpu_max", "0"),
            ("cpu_max", "250"),
            ("cpu_max", "500"),
            ("sa1_max", "50"),
            ("sa1_max", "800"),
            ("nes_extra_lines_max", "241"),
            ("tick_counter", "0x7DFFFF"),
            ("tick_counter", "0x800000"),
            ("tick_counter", "0x0012"),
            ("target_tick_hz", "0"),
            ("target_tick_hz", "61"),
        ] {
            let text = format!("{VALID}\n[overclock]\n{key} = {bad}\n");
            let err = load_str(&text).expect_err("invalid value must be rejected");
            assert!(
                err.to_string().contains(&format!("[overclock].{key}")),
                "{key} = {bad}: {err}"
            );
        }
        let edge = format!("{VALID}\n[overclock]\ntick_counter = 0x7FFFFF\n");
        assert!(load_str(&edge).is_ok());
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

    // `[heights]` (ticket W30-06; GAME_PROFILES.md §2).

    fn heights_text(body: &str) -> String {
        format!("{VALID}\n[heights]\n{body}\n")
    }

    #[test]
    fn a_valid_heights_table_loads_and_resolves() {
        let text = heights_text(
            "layer = 2\ntile_px = 16\n[heights.tiles]\n\"0x0A4\" = 16\n\"3FF\" = 64\n\"0x001\" = 0",
        );
        let h = load_str(&text)
            .expect("valid [heights]")
            .profile
            .heights
            .unwrap();
        assert_eq!((h.layer, h.tile_px), (2, 16));
        let table = h.table();
        assert_eq!(table.get(&0x0A4), Some(&16));
        assert_eq!(table.get(&0x3FF), Some(&64));
        assert_eq!(table.get(&0x001), Some(&0));
        assert_eq!(table.len(), 3);
    }

    #[test]
    fn heights_defaults_to_layer_one_and_eight_pixel_cells_and_no_warnings() {
        let out = load_str(&heights_text("")).expect("empty [heights]");
        let h = out.profile.heights.unwrap();
        assert_eq!((h.layer, h.tile_px, h.tiles.len()), (1, 8, 0));
        let out = load_str(&heights_text("[heights.tiles]\n\"0x10\" = 8")).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn a_profile_without_heights_has_none() {
        assert!(load_str(VALID).unwrap().profile.heights.is_none());
    }

    #[test]
    fn a_misspelt_heights_key_warns() {
        let out = load_str(&heights_text("lyer = 1")).unwrap();
        assert!(
            out.warnings.contains(&"heights.lyer".to_string()),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn heights_layer_and_tile_px_are_range_checked_by_name() {
        for (body, key) in [
            ("layer = 0", "[heights].layer"),
            ("layer = 5", "[heights].layer"),
            ("tile_px = 12", "[heights].tile_px"),
            ("tile_px = 0", "[heights].tile_px"),
        ] {
            let err = load_str(&heights_text(body)).unwrap_err();
            assert!(err.to_string().contains(key), "{body}: {err}");
        }
    }

    #[test]
    fn heights_tile_keys_and_values_are_validated_and_the_error_names_the_key() {
        for (entry, needle) in [
            ("\"0x400\" = 8", "0x400"),
            ("\"zz\" = 8", "zz"),
            ("\"\" = 8", "tiles"),
            ("\"0x0A4\" = 65", "0x0A4"),
            ("\"0x0A4\" = -1", "0x0A4"),
        ] {
            let text = heights_text(&format!("[heights.tiles]\n{entry}"));
            let err = load_str(&text).unwrap_err();
            assert!(
                matches!(err, ProfileError::HeightsInvalid { .. }),
                "{entry}: {err}"
            );
            assert!(err.to_string().contains(needle), "{entry}: {err}");
        }
        // Two spellings of one tile are a mistake, not a silent override.
        let dup = heights_text("[heights.tiles]\n\"0x0A4\" = 8\n\"A4\" = 16");
        assert!(matches!(
            load_str(&dup).unwrap_err(),
            ProfileError::HeightsInvalid { .. }
        ));
    }

    #[test]
    fn a_nes_scout_table_may_omit_camera_x_and_use_walk_and_player_x_hi() {
        let text = format!("{VALID}\n{SCOUT}")
            .replacen("role = \"camera_x\"", "role = \"player_x_hi\"", 1)
            .replacen("strategy = \"player_move\"", "strategy = \"walk\"", 1);
        // VALID is a NES profile: no camera_x row, walk strategy, hi byte row.
        load_str(&text).expect("a NES walk table loads");
    }

    #[test]
    fn the_walk_strategy_is_rejected_on_the_snes() {
        let snes = VALID.replacen("console = \"nes\"", "console = \"snes\"", 1);
        let text = format!("{snes}\n{SCOUT}").replacen("player_move", "walk", 1);
        let err = load_str(&text).expect_err("walk is NES only");
        assert!(matches!(err, ProfileError::ScoutInvalid(_)), "{err}");
    }

    // `[scene]` (ticket W33-09).
    const SCENE: &str = r#"
        [scene]
        sections = "axis"

        [[scene.level_id]]
        addr = 0x2A
        source = "observed: test"

        [[scene.level_id]]
        addr = 0x61
        len = 2
        idle = 0
        decides = false
        notes = "pointer word"
        source = "observed: test"
    "#;

    #[test]
    fn a_valid_scene_table_loads_clean() {
        let outcome = load_str(&format!("{VALID}\n{SCENE}")).expect("valid [scene] loads");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        let scene = outcome.profile.scene.expect("scene section");
        assert_eq!(scene.level_id.len(), 2);
        assert_eq!(scene.level_id[1].len, Some(2));
        assert_eq!(scene.sections.as_deref(), Some("axis"));
    }

    #[test]
    fn a_scene_table_with_a_bad_row_is_rejected_and_says_which() {
        for (from, to) in [
            ("sections = \"axis\"", "sections = \"zigzag\""),
            (
                "addr = 0x61\n        len = 2",
                "addr = 0x61\n        len = 5",
            ),
            ("addr = 0x2A", "addr = 0x1_0000"),
            (
                "addr = 0x61\n        len = 2\n        idle = 0",
                "addr = 0x61\n        len = 1\n        idle = 300",
            ),
            (
                "source = \"observed: test\"\n\n        [[scene.level_id]]",
                "\n\n        [[scene.level_id]]",
            ),
        ] {
            let text = format!("{VALID}\n{SCENE}").replace(from, to);
            let err = load_str(&text).expect_err(to);
            assert!(matches!(err, ProfileError::SceneInvalid(_)), "{to}: {err}");
        }
    }

    #[test]
    fn a_snes_scene_row_must_sit_in_work_ram() {
        let snes = VALID.replace("nes", "snes");
        let text =
            format!("{snes}\n[[scene.level_id]]\naddr = 0x1000\nsource = \"observed: test\"\n");
        let err = load_str(&text).expect_err("outside 7E/7F");
        assert!(matches!(err, ProfileError::SceneInvalid(_)), "{err}");
    }
}
