//! Matching a profile to the open ROM, decoding its level once, and
//! building the enhanced scene from live machine state (ticket W5-03;
//! FR-ENH-006).
//!
//! The shell is the only layer that can do this. `rf-enhance` owns the
//! decoder and the scene construction but may not read a bus or a
//! filesystem; `rf-nes` may not know profiles exist. This module is the
//! mediator, exactly as `crate::debug_dock` is for `rf-debugger`'s
//! viewers and `crate::canvas_accum` is for the stitcher.
//!
//! ## Decoded once, not per frame
//!
//! [`LevelSession::open`] decodes at ROM-load time and keeps the result
//! for the session. That is not an optimisation, it is what the decode
//! being a **pure function of ROM bytes** buys: ROM bytes do not change,
//! so re-deriving the level every frame would produce the same answer at
//! 60 Hz forever. Only the camera and sprites are re-read per frame,
//! because only they change.
//!
//! ## Failing to find a profile is not an error
//!
//! Most ROMs have no profile, and that is the normal case rather than a
//! fault. Every failure here degrades to `None` and leaves the enhanced
//! view showing what it showed before — a missing profile must never be
//! a reason a game will not run (D-004's trust ladder applied one level
//! up: enhancements are opt-in overlays over an unmodified simulation,
//! project law 6).

use std::path::{Path, PathBuf};

use rf_enhance::decode::metatile_screens::{self, DecodedLevel, Spec};
use rf_enhance::level_view::{self, LevelGeometry};
use rf_enhance::scene_graph::{LevelId, SceneGraph};
use rf_profiles::schema::Profile;

/// Palette index the original-viewport outline is drawn in.
const OUTLINE_COLOR: u8 = 0x30;

/// A profile matched to the running game, plus its decoded level.
pub struct LevelSession {
    pub profile: Profile,
    pub level: DecodedLevel,
    pub geometry: LevelGeometry,
    pub spec: Spec,
    /// Where the profile came from, for the UI to show — a user seeing
    /// their game enhanced should be able to find out by what.
    pub source: PathBuf,
}

impl LevelSession {
    /// Find a profile whose `[[identity]]` matches `normalized_sha256`,
    /// decode its level out of `rom` (the raw file as opened — the header
    /// is stripped here, since `rom_map` offsets are into the normalized
    /// image).
    ///
    /// Returns `None` when no profile matches, the profile declares no
    /// decodable level, or the decode fails — see the module doc for why
    /// none of those is an error.
    #[must_use]
    pub fn open(profiles_root: &Path, rom: &[u8], hashes: &rf_cart::RomHashes) -> Option<Self> {
        let (profile, source) = find_matching_profile(profiles_root, hashes)?;
        let spec = metatile_screens::spec_from_profile(&profile).ok()?;
        // Offsets are into the NORMALIZED image. Handing the raw file
        // through would shift every table by the 16-byte iNES header and
        // decode a plausible-looking wrong level rather than failing —
        // `rf_enhance::decode`'s module doc is explicit about this.
        let normalized = rom.strip_prefix(b"NES\x1a").map_or(rom, |_| &rom[16..]);
        let level = metatile_screens::decode(normalized, &spec).ok()?;
        let geometry = LevelGeometry::from_level(&level, 8);
        Some(Self {
            profile,
            level,
            geometry,
            spec,
            source,
        })
    }

    /// Build the enhanced scene for this frame.
    ///
    /// `read` is a non-perturbing CPU peek and `entity_table` the bytes
    /// the profile's `[entities].table.addr` points at, both supplied by
    /// the caller because this crate's stepper is the only thing that can
    /// produce them without touching core state.
    #[must_use]
    pub fn scene(
        &self,
        full_map: bool,
        read: &dyn Fn(u32) -> u8,
        entity_table: &[u8],
        target_width: u32,
        target_height: u32,
    ) -> SceneGraph {
        let camera = level_view::live_camera(&self.profile, read);
        let sprites = level_view::sprites_in_world(&self.profile, entity_table, camera);
        if full_map {
            level_view::full_map_scene(
                LevelId(0),
                self.geometry,
                sprites,
                camera,
                target_width,
                target_height,
                OUTLINE_COLOR,
            )
        } else {
            level_view::ultrawide_scene_over_level(
                LevelId(0),
                sprites,
                camera,
                target_width,
                target_height,
                OUTLINE_COLOR,
            )
        }
    }

    /// Ticket W16-06: the Diorama "walls pop up" geometry layer for this
    /// session's decoded level, or `None` for exactly the same reasons
    /// [`Self::has_collision`] would say `false` — the two are kept in
    /// sync deliberately (this calls the same underlying `rf_enhance`
    /// function [`Self::has_collision`] checks the preconditions of), so
    /// a caller that already checked `has_collision()` cannot get `None`
    /// here for a different reason than that check already reported.
    #[must_use]
    pub fn diorama_geometry(&self) -> Option<rf_enhance::scene_graph::SceneLayer> {
        let bits = &self.profile.decode.as_ref()?.collision.as_ref()?.bits;
        level_view::diorama_geometry_layer(
            LevelId(0),
            &self.level,
            self.geometry,
            bits,
            "solid",
            (0, 0),
        )
    }

    /// Ticket W16-06: whether this session's decode actually produced a
    /// solidity mask — the narrower fact `crate::enhance_ui::feature_rows`
    /// needs for the Diorama row (that module's own doc: a matched profile
    /// with no collision table must still read as `NeedsProfile`, not
    /// `Available`). Checks both that a `[decode.collision]` table was
    /// declared AND that its `bits` names a `"solid"` bit — the same two
    /// preconditions `rf_enhance::scene_graph::geometry_layer` itself
    /// requires, checked here so the UI can know the answer before
    /// attempting the (identical) decode `diorama_geometry_layer` would
    /// do.
    #[must_use]
    pub fn has_collision(&self) -> bool {
        self.level.collision.is_some()
            && self
                .profile
                .decode
                .as_ref()
                .and_then(|d| d.collision.as_ref())
                .is_some_and(|c| c.bits.split(',').any(|b| b.trim() == "solid"))
    }

    /// Where in RAM the entity table starts, so the caller knows what to
    /// slice. `None` when the profile declares no entities.
    #[must_use]
    pub fn entity_table_range(&self) -> Option<(u32, usize)> {
        let e = self.profile.entities.as_ref()?;
        Some((
            e.table.addr,
            (e.table.count as usize) * (e.table.stride.max(1) as usize),
        ))
    }
}

/// Ticket W24-01: where the shipped profiles are — a `profiles` folder
/// holding `nes` or `snes`, beside `exe_dir` or in any folder above it,
/// else in `cwd`, else the bare relative path (which then finds nothing,
/// as before).
#[must_use]
pub fn locate_profiles_root(exe_dir: Option<&Path>, cwd: Option<&Path>) -> PathBuf {
    let is_root = |p: &Path| p.join("nes").is_dir() || p.join("snes").is_dir();
    exe_dir
        .into_iter()
        .flat_map(Path::ancestors)
        .chain(cwd)
        .map(|d| d.join("profiles"))
        .find(|p| is_root(p))
        .unwrap_or_else(|| PathBuf::from("profiles"))
}

/// Walk `profiles_root` for a `profile.toml` whose identity matches.
///
/// `pub` since ticket W5-06: the author workspace needs the PATH of the
/// profile that matched, so it knows which file to watch.
///
/// Matching is on the **normalized** sha256 and nothing else: that is
/// what `rf_cart::hash` says profiles are keyed on, and matching on a raw
/// file hash would fail for the same game in different packaging while
/// appearing to work for whichever copy the author happened to have.
#[must_use]
pub fn find_matching_profile(
    root: &Path,
    hashes: &rf_cart::RomHashes,
) -> Option<(Profile, PathBuf)> {
    let mut found = Vec::new();
    collect_profiles(root, &mut found);
    found.sort();
    for path in found {
        let Ok(outcome) = rf_profiles::load_file(&path) else {
            continue; // a broken profile must not stop a good one loading
        };
        // Ticket W11-09: `IdentityEntry::matches` — the schema's OWN
        // matcher, checking every hash family an entry declares.
        //
        // This used to be a second, weaker comparison written here:
        // `i.sha256.as_deref() == Some(normalized_sha256)`, ignoring
        // sha1, md5 and crc32 entirely. The schema has carried all four
        // since W4-02, so a profile identified by a published No-Intro
        // CRC32 — the only realistic way to identify a commercial title
        // nobody in this project holds a copy of — loaded cleanly and
        // matched nothing, for ever, silently. Two matchers for one
        // schema is how that happens; there is one now.
        if outcome.profile.identity.iter().any(|i| i.matches(hashes)) {
            return Some((outcome.profile, path));
        }
    }
    None
}

/// Every `sha256` any profile under `root` declares in its identity list
/// (ticket W15-05: the library grid's "profile matched" badge,
/// `UX_WAVE_15.md` §3.1).
///
/// Deliberately a WEAKER match than [`find_matching_profile`]: that
/// function checks every hash family an `IdentityEntry` declares
/// (`IdentityEntry::matches`, needing a full `rf_cart::RomHashes`), but a
/// library entry only ever carries its normalized sha256
/// (`crate::library::EntryIdentity::Recognized`) — there is no sha1/md5/
/// crc32 to check here. A profile identified ONLY by, say, a No-Intro
/// CRC32 will not light this badge even though it would match at load
/// time; that is a badge being conservative about a hash family it
/// cannot see, not a bug, and it costs nothing at the point that matters
/// (the real load-time match still uses every hash family).
#[must_use]
pub fn all_profile_sha256s(root: &Path) -> std::collections::HashSet<String> {
    let mut found = Vec::new();
    collect_profiles(root, &mut found);
    let mut out = std::collections::HashSet::new();
    for path in found {
        let Ok(outcome) = rf_profiles::load_file(&path) else {
            continue;
        };
        for identity in &outcome.profile.identity {
            if let Some(sha256) = &identity.sha256 {
                out.insert(sha256.clone());
            }
        }
    }
    out
}

/// Ticket W20-20 (`docs/design/UX_WAVE_20.md` §6): the enhancement chips
/// a library card shows for a game with this profile — only what this
/// build can actually DO for it, not what the profile claims:
///
/// * "Full level" needs a `[decode]` table (2 of 11 shipped profiles —
///   the audit's row A; `capabilities.full_level` alone decodes nothing);
/// * "3D walls" needs that decode to carry collision (diorama);
/// * "Widescreen" needs an SNES profile (the NES has no widescreen path).
///
/// Loading fast-forward is NOT offered as a chip: the one shipped profile
/// that declares it describes a loop its fixture never runs (W20-17's
/// finding), and a chip promising it would be untrue.
#[must_use]
pub fn profile_chips(profile: &rf_profiles::schema::Profile) -> Vec<&'static str> {
    let mut chips = Vec::new();
    if let Some(decode) = &profile.decode {
        chips.push("Full level");
        if decode.collision.is_some() {
            chips.push("3D walls");
        }
    }
    if profile.meta.console == rf_profiles::schema::Console::Snes
        && (profile.widescreen.is_some()
            || profile.capabilities.widescreen != rf_profiles::schema::WidescreenMode::default())
    {
        chips.push("Widescreen");
    }
    chips
}

/// Ticket W20-20: [`profile_chips`] for every profile under `root`, keyed
/// by each identity's normalized SHA-256.
#[must_use]
pub fn profile_chips_by_sha256(
    root: &Path,
) -> std::collections::HashMap<String, Vec<&'static str>> {
    let mut found = Vec::new();
    collect_profiles(root, &mut found);
    let mut out = std::collections::HashMap::new();
    for path in found {
        let Ok(outcome) = rf_profiles::load_file(&path) else {
            continue;
        };
        let chips = profile_chips(&outcome.profile);
        for identity in &outcome.profile.identity {
            if let Some(sha256) = &identity.sha256 {
                out.insert(sha256.clone(), chips.clone());
            }
        }
    }
    out
}

fn collect_profiles(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_profiles(&path, out);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("profile.toml") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod chip_tests {
    use super::*;

    /// Ticket W20-20: chips reflect what the app can do, profile by
    /// profile, on the shipped profiles.
    #[test]
    fn chips_follow_what_the_app_can_actually_do() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
        let load = |rel: &str| rf_profiles::load_file(&root.join(rel)).expect(rel).profile;
        assert_eq!(
            profile_chips(&load("nes/rf-scroller/profile.toml")),
            vec!["Full level", "3D walls"]
        );
        assert_eq!(
            profile_chips(&load("snes/rf-scroller-s/profile.toml")),
            vec!["Widescreen"]
        );
        assert!(
            profile_chips(&load("nes/metroid/profile.toml")).is_empty(),
            "a profile with no decode table promises nothing"
        );
    }

    /// Ticket W24-01: a binary in `target/release` finds the repo's
    /// profiles two folders up; a working directory still works; nothing
    /// found falls back to the relative path.
    #[test]
    fn profiles_root_is_found_above_the_executable() {
        let tmp = std::env::temp_dir().join(format!("rf_profroot_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("profiles/snes")).unwrap();
        std::fs::create_dir_all(tmp.join("target/release")).unwrap();
        let exe = tmp.join("target/release");
        assert_eq!(locate_profiles_root(Some(&exe), None), tmp.join("profiles"));
        assert_eq!(locate_profiles_root(None, Some(&tmp)), tmp.join("profiles"));
        let elsewhere =
            std::env::temp_dir().join(format!("rf_profroot_none_{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(
            locate_profiles_root(Some(&elsewhere), Some(&elsewhere)),
            PathBuf::from("profiles")
        );
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }
}
