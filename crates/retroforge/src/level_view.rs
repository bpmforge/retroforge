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
