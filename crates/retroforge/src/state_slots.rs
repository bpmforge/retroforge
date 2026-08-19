//! The save-state manager's data model (ticket W4-11;
//! `docs/design/FRONTEND_UI.md` §3.2: "10 slots + auto-slots, each with
//! screenshot, timestamp, mode-at-save, 'contains mods' warning flag
//! (PROF chunk); load warns on version-migrated states").
//!
//! UI-free on purpose. The modal in `crate::app` draws what this
//! produces, and everything here is a function of files on disk — which
//! is what lets W4-09's headless harness assert the modal's contents
//! without a GPU, and what lets these rules be unit-tested at all.
//!
//! ## Mode-at-save and "contains mods" are DERIVED, not stored
//!
//! Both come from which optional chunks the container carries, and that
//! is deliberate rather than convenient. `rf_state::tags` already defines
//! `ENHC` (enhancement state) and `PROF` (the profile a session ran
//! under) as optional chunks that an Accuracy-mode save simply does not
//! have — its own doc contrasts "an Accuracy-mode save (no `ENHC`/`PROF`)
//! and an Enhanced-mode save of the same machine".
//!
//! So a sidecar field recording "this was saved in Enhanced mode" would
//! be a second source of truth that can disagree with the state itself,
//! and the failure mode is bad in the worst direction: a state whose
//! sidecar says Accuracy but which restores enhancement data would carry
//! a session out of the reference mode, which project law 6 exists to
//! prevent. Reading the chunk list cannot disagree with the chunk list.
//!
//! ## Thumbnails are a sidecar, and only thumbnails
//!
//! A screenshot is not machine state and has no business inside a
//! `.rfstate` container — SAVE_STATES.md's chunk list has no slot for it,
//! and adding one would change the format for a UI convenience. It is
//! written beside the state as a PNG instead, via `rf_renderer::png`
//! (which exists already, from W3-04's screenshot path — the pre-flight
//! note's "check whether a readback exists before building one").
//!
//! The frame it downscales is the shell's own RGBA framebuffer, never
//! core state: cores emit indexed pixels and the palette is applied
//! host-side (ARCHITECTURE §3), so the shell already holds the only RGB
//! that exists. Nothing here reaches into a core.

use std::path::{Path, PathBuf};

use rf_state::Container;
use rf_state::LoadWarning;

/// FRONTEND_UI §3.2's "10 slots".
pub const NUMBERED_SLOTS: u8 = 10;
/// How many rolling auto-save slots the manager keeps.
pub const AUTO_SLOTS: u8 = 3;

/// Thumbnail edge, in pixels. 96 wide keeps a 10-slot grid readable at
/// typical window sizes while staying a clean 8/3 downscale of the NES's
/// 256-pixel width, so the reduction is an exact box filter rather than a
/// resample with its own artefacts.
pub const THUMB_WIDTH: u32 = 96;

/// Which slot a state occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlotId {
    /// User slot 1..=10.
    Numbered(u8),
    /// Rolling auto-save 0..`AUTO_SLOTS`.
    Auto(u8),
}

impl SlotId {
    /// Every slot the manager shows, in display order.
    #[must_use]
    pub fn all() -> Vec<SlotId> {
        (1..=NUMBERED_SLOTS)
            .map(SlotId::Numbered)
            .chain((0..AUTO_SLOTS).map(SlotId::Auto))
            .collect()
    }

    /// Filename stem. Auto-slots are prefixed so a directory listing
    /// never confuses `auto0` with slot 0 — there is no slot 0.
    #[must_use]
    pub fn stem(self) -> String {
        match self {
            SlotId::Numbered(n) => format!("slot{n}"),
            SlotId::Auto(n) => format!("auto{n}"),
        }
    }

    #[must_use]
    pub fn label(self) -> String {
        match self {
            SlotId::Numbered(n) => format!("Slot {n}"),
            SlotId::Auto(n) => format!("Auto {n}"),
        }
    }

    /// Inverse of [`SlotId::stem`], for a command that had to cross a
    /// thread boundary as plain data.
    #[must_use]
    pub fn from_stem(stem: &str) -> Option<SlotId> {
        if let Some(n) = stem.strip_prefix("slot") {
            let n: u8 = n.parse().ok()?;
            (1..=NUMBERED_SLOTS)
                .contains(&n)
                .then_some(SlotId::Numbered(n))
        } else if let Some(n) = stem.strip_prefix("auto") {
            let n: u8 = n.parse().ok()?;
            (n < AUTO_SLOTS).then_some(SlotId::Auto(n))
        } else {
            None
        }
    }

    #[must_use]
    pub fn is_auto(self) -> bool {
        matches!(self, SlotId::Auto(_))
    }
}

/// What the modal shows for one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotInfo {
    pub id: SlotId,
    /// `None` for an empty slot — the modal shows all slots, occupied or
    /// not, because "which slots are free" is half of what the user
    /// opened it to find out.
    pub saved: Option<SavedState>,
}

/// The metadata of an occupied slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedState {
    pub path: PathBuf,
    /// Unix seconds, from the container header.
    pub timestamp: u64,
    /// FRONTEND_UI §3.2's "mode-at-save".
    pub mode: ModeAtSave,
    /// §3.2's "'contains mods' warning flag (PROF chunk)".
    pub contains_mods: bool,
    /// Sidecar PNG, if one was written.
    pub thumbnail: Option<PathBuf>,
}

/// Which mode a state was saved in, derived from its chunk list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeAtSave {
    Accuracy,
    Enhanced,
}

impl ModeAtSave {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ModeAtSave::Accuracy => "Accuracy",
            ModeAtSave::Enhanced => "Enhanced",
        }
    }
}

/// Where a game's states live: `<config>/states/<rom-sha256>/`.
///
/// Keyed by the normalized ROM hash for the same reason battery RAM is
/// (`crate::save_state::battery_ram_path`): the same cartridge dumped
/// twice shares one set of states, and two different games can never
/// collide.
#[must_use]
pub fn slots_dir(config_root: &Path, rom_sha256_hex: &str) -> PathBuf {
    config_root.join("states").join(rom_sha256_hex)
}

fn state_path(dir: &Path, slot: SlotId) -> PathBuf {
    dir.join(format!("{}.rfstate", slot.stem()))
}

fn thumb_path(dir: &Path, slot: SlotId) -> PathBuf {
    dir.join(format!("{}.png", slot.stem()))
}

/// Read the metadata of every slot, occupied or not.
///
/// A slot whose file is present but unreadable or malformed comes back
/// **empty rather than absent**, and never as an error: the manager's job
/// is to show the user what they have, and one corrupt file must not
/// blank the other twelve slots.
#[must_use]
pub fn scan(dir: &Path) -> Vec<SlotInfo> {
    SlotId::all()
        .into_iter()
        .map(|id| SlotInfo {
            id,
            saved: read_slot(dir, id),
        })
        .collect()
}

fn read_slot(dir: &Path, slot: SlotId) -> Option<SavedState> {
    let path = state_path(dir, slot);
    let bytes = std::fs::read(&path).ok()?;
    // `decode_default` registers no migrations, so a state at an older
    // chunk version is a hard error here. That is the right behaviour for
    // a LISTING: it must not silently migrate anything on disk. The
    // actual load path (`load`, below) is where migration and its warning
    // belong.
    let (container, _) = Container::decode_default(&bytes).ok()?;
    Some(describe(&container, path, thumb_path(dir, slot)))
}

fn describe(container: &Container, path: PathBuf, thumb: PathBuf) -> SavedState {
    let has = |tag: &[u8; 4]| container.chunk(*tag).is_some();
    SavedState {
        path,
        timestamp: container.header.timestamp,
        mode: if has(b"ENHC") {
            ModeAtSave::Enhanced
        } else {
            ModeAtSave::Accuracy
        },
        contains_mods: has(b"PROF"),
        thumbnail: thumb.exists().then_some(thumb),
    }
}

/// Write a state and its thumbnail into `slot`.
///
/// # Errors
/// Returns the OS error text. The state is written first and the
/// thumbnail second, deliberately: a thumbnail without a state is
/// invisible (nothing lists it), whereas a state without a thumbnail is
/// merely a slot with no picture. Failing the other way round would let a
/// disk-full error leave a listed slot the user cannot load.
pub fn save(
    dir: &Path,
    slot: SlotId,
    container_bytes: &[u8],
    thumbnail_rgba: Option<(&[u8], u32, u32)>,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = state_path(dir, slot);
    std::fs::write(&path, container_bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some((rgba, w, h)) = thumbnail_rgba {
        let (thumb, tw, th) = downscale(rgba, w, h, THUMB_WIDTH);
        let png = rf_renderer::png::encode_rgba(&thumb, tw, th);
        let tp = thumb_path(dir, slot);
        std::fs::write(&tp, png).map_err(|e| format!("{}: {e}", tp.display()))?;
    }
    Ok(())
}

/// Load a slot, returning its container and the warnings the UI must
/// surface.
///
/// # Errors
/// Returns the read or decode error text.
///
/// **`migrations` is taken rather than assumed** so the caller decides
/// what this build can migrate; §3.2's "load warns on version-migrated
/// states" is then just surfacing the `LoadWarning::Migrated` entries
/// `rf_state` already produces, rather than a second migration policy
/// living in the UI.
pub fn load(
    dir: &Path,
    slot: SlotId,
    migrations: &rf_state::MigrationRegistry,
) -> Result<(Container, Vec<LoadWarning>), String> {
    let path = state_path(dir, slot);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Container::decode(&bytes, migrations).map_err(|e| format!("{}: {e}", path.display()))
}

/// Human-readable text for the warnings a load produced.
///
/// Migration warnings are listed FIRST and worded as a change to the
/// user's data, because they are the one class §3.2 calls out by name: a
/// state that loaded after migration is not the state that was written,
/// and a user who is about to overwrite it deserves to know before they
/// do.
#[must_use]
pub fn warning_lines(warnings: &[LoadWarning]) -> Vec<String> {
    let mut migrated: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for w in warnings {
        match w {
            LoadWarning::Migrated { tag, from, to } => migrated.push(format!(
                "Migrated {} from v{from} to v{to} — this state was written by an older build \
                 and has been converted to load here.",
                String::from_utf8_lossy(tag)
            )),
            LoadWarning::UnknownChunk { tag } => other.push(format!(
                "Skipped {} — this session has nothing to restore it into.",
                String::from_utf8_lossy(tag)
            )),
        }
    }
    migrated.extend(other);
    migrated
}

/// Box-filter downscale to `target_width`, preserving aspect.
///
/// A plain nearest-neighbour pick would drop most of a 256-pixel frame on
/// the floor and make a one-pixel sprite vanish from its own thumbnail;
/// averaging every source pixel that lands in a destination cell keeps
/// the frame recognisable, which is the entire purpose of the picture.
#[must_use]
pub fn downscale(rgba: &[u8], width: u32, height: u32, target_width: u32) -> (Vec<u8>, u32, u32) {
    if width == 0 || height == 0 || target_width == 0 || rgba.len() < (width * height * 4) as usize
    {
        return (Vec::new(), 0, 0);
    }
    let target_width = target_width.min(width);
    let scale = width / target_width.max(1);
    let scale = scale.max(1);
    let (tw, th) = (width / scale, height / scale);
    let mut out = Vec::with_capacity((tw * th * 4) as usize);
    for ty in 0..th {
        for tx in 0..tw {
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in 0..scale {
                for sx in 0..scale {
                    let px = tx * scale + sx;
                    let py = ty * scale + sy;
                    if px >= width || py >= height {
                        continue;
                    }
                    let i = ((py * width + px) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += u32::from(rgba[i + c]);
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            for c in acc {
                out.push(u8::try_from(c / n).unwrap_or(u8::MAX));
            }
        }
    }
    (out, tw, th)
}
