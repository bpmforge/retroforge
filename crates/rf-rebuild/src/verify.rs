//! The per-ROM verification record (ticket W37-03): how much lockstep checking
//! a game's rebuilt path has survived, kept in the per-ROM cache next to the
//! atlas so the Rebuild panel can say so.
//!
//! **Writers** are the tools that run a rebuilt core against a plain one and
//! compare every frame: the census and lockstep harnesses
//! (`rf_harness::atlas::store_verify_record`) and, from W37-04, the shadow
//! core. **Readers** are the shell's first Rebuild job and the panel. This
//! crate sees neither the cache nor a clock; it defines the bytes, the
//! arithmetic and the one threshold.
//!
//! ## What the record means
//!
//! `frames` counts frames compared (registers, memories, picture and sound)
//! and `divergences` counts runs' frames that differed. Counts add across
//! runs: a record is evidence, never a stamp. Two runs over the same
//! first minute add up to two minutes of frames; the panel therefore also
//! needs [`PROVEN_RUNS`] separate runs before it says *proven*, so one long
//! loop of an idle title screen is not enough on its own.
//!
//! ## When a game is proven
//!
//! [`Verification::of`] says **Proven** only when the record holds at least
//! [`PROVEN_FRAMES`] compared frames (10 minutes at 60 fps), from at least
//! [`PROVEN_RUNS`] runs, and **zero** divergences. Any divergence ever
//! recorded makes the state **Diverged**: it can only be cleared by a new
//! atlas format (the cache key carries the format version, so a changed
//! executor starts from "not checked" instead of inheriting an old stamp).
//! W37-04 ends its shadow-core checking for a game once this state is
//! Proven.

use crate::atlas::Atlas;

/// Cache asset name of the record.
pub const VERIFY_ASSET: &str = "rebuild-verification";
/// Cache settings string: the record's format and the atlas format it vouches
/// for. A new atlas format is a clean miss, so a "proven" stamp never
/// outlives the executor it checked.
pub const VERIFY_SETTINGS: &str = "rebuild-verify-v1/code-atlas-v5";

/// Compared frames needed before a game can be called proven: ten minutes at
/// 60 fps.
pub const PROVEN_FRAMES: u64 = 36_000;
/// Separate runs needed before a game can be called proven.
pub const PROVEN_RUNS: u32 = 2;

/// Cache asset name of the demoted-function list (ticket W37-04).
pub const DEMOTED_ASSET: &str = "rebuild-demoted";
/// Cache settings string of the demoted-function list: same atlas-format
/// suffix as [`VERIFY_SETTINGS`], so a new executor starts with nothing
/// demoted.
pub const DEMOTED_SETTINGS: &str = "rebuild-demoted-v1/code-atlas-v5";

const MAGIC: [u8; 4] = *b"RFVR";
const VERSION: u8 = 1;
const LEN: usize = 4 + 1 + 8 + 8 + 4;

/// What the lockstep tools have recorded for one ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VerifyRecord {
    /// Frames compared in lockstep, over all runs.
    pub frames: u64,
    /// Frames on which the rebuilt and plain copies differed, over all runs.
    pub divergences: u64,
    /// Runs folded in.
    pub runs: u32,
}

impl VerifyRecord {
    /// Fold one finished lockstep run in. Saturating, so a damaged counter
    /// can never wrap into "zero divergences".
    pub fn add_run(&mut self, frames: u64, divergences: u64) {
        self.frames = self.frames.saturating_add(frames);
        self.divergences = self.divergences.saturating_add(divergences);
        self.runs = self.runs.saturating_add(1);
    }

    /// The record's bytes: magic, version, then the three counters.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(LEN);
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&self.divergences.to_le_bytes());
        out.extend_from_slice(&self.runs.to_le_bytes());
        out
    }

    /// `None` for anything but exactly the bytes [`Self::encode`] writes: a
    /// damaged or foreign entry reads as "not checked".
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != LEN || bytes[..4] != MAGIC || bytes[4] != VERSION {
            return None;
        }
        Some(Self {
            frames: u64::from_le_bytes(bytes[5..13].try_into().ok()?),
            divergences: u64::from_le_bytes(bytes[13..21].try_into().ok()?),
            runs: u32::from_le_bytes(bytes[21..25].try_into().ok()?),
        })
    }
}

/// A demoted function, named the way it survives a new atlas: function ids
/// are positions in one atlas and are renumbered by every rebuild, so the
/// list stores the function's entry (`entry_rom`, `entry_mode`), which
/// [`Atlas::functions`] keys on.
pub type FunctionKey = (u32, u8);

const DEMOTED_MAGIC: [u8; 4] = *b"RFDM";
const DEMOTED_VERSION: u8 = 1;
/// Most entries a list may hold when read back (a damaged count can not ask
/// for gigabytes).
const DEMOTED_MAX: usize = 1 << 16;

/// The key of function `id` in `atlas`, if it has one.
#[must_use]
pub fn function_key(atlas: &Atlas, id: u32) -> Option<FunctionKey> {
    atlas
        .functions
        .get(id as usize)
        .map(|f| (f.entry_rom, f.entry_mode))
}

/// The ids in `atlas` of the functions named by `keys` (keys the atlas does
/// not hold are dropped), ascending.
#[must_use]
pub fn demoted_ids(atlas: &Atlas, keys: &[FunctionKey]) -> Vec<u32> {
    let wanted: std::collections::HashSet<FunctionKey> = keys.iter().copied().collect();
    atlas
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| wanted.contains(&(f.entry_rom, f.entry_mode)))
        .map(|(i, _)| i as u32)
        .collect()
}

/// The demoted list's bytes: magic, version, a count, then 5 bytes a key.
#[must_use]
pub fn encode_demoted(keys: &[FunctionKey]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + keys.len() * 5);
    out.extend_from_slice(&DEMOTED_MAGIC);
    out.push(DEMOTED_VERSION);
    out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    for &(rom, mode) in keys {
        out.extend_from_slice(&rom.to_le_bytes());
        out.push(mode);
    }
    out
}

/// `None` for anything but exactly the bytes [`encode_demoted`] writes.
#[must_use]
pub fn decode_demoted(bytes: &[u8]) -> Option<Vec<FunctionKey>> {
    if bytes.len() < 9 || bytes[..4] != DEMOTED_MAGIC || bytes[4] != DEMOTED_VERSION {
        return None;
    }
    let n = u32::from_le_bytes(bytes[5..9].try_into().ok()?) as usize;
    if n > DEMOTED_MAX || bytes.len() != 9 + n * 5 {
        return None;
    }
    // A bounded `chunks_exact` walk: every chunk is consumed once.
    Some(
        bytes[9..]
            .chunks_exact(5)
            .map(|c| (u32::from_le_bytes([c[0], c[1], c[2], c[3]]), c[4]))
            .collect(),
    )
}

/// What the panel says about a game's rebuilt path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verification {
    /// No lockstep run on record.
    #[default]
    NotChecked,
    /// Frames compared with no difference, short of the proven threshold.
    Checked { frames: u64 },
    /// Frames compared, and some differed.
    Diverged { frames: u64, divergences: u64 },
    /// At least [`PROVEN_FRAMES`] frames over [`PROVEN_RUNS`] runs, no difference.
    Proven { frames: u64 },
}

impl Verification {
    /// The state a (possibly absent) record stands for.
    #[must_use]
    pub fn of(record: Option<&VerifyRecord>) -> Self {
        let Some(r) = record.filter(|r| r.frames > 0) else {
            return Self::NotChecked;
        };
        if r.divergences > 0 {
            Self::Diverged {
                frames: r.frames,
                divergences: r.divergences,
            }
        } else if r.frames >= PROVEN_FRAMES && r.runs >= PROVEN_RUNS {
            Self::Proven { frames: r.frames }
        } else {
            Self::Checked { frames: r.frames }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_demoted_list_round_trips_and_damage_reads_as_absent() {
        let keys = vec![(0x10, 0), (0x8000, 3), (7, 1)];
        let bytes = encode_demoted(&keys);
        assert_eq!(decode_demoted(&bytes), Some(keys));
        assert_eq!(decode_demoted(&encode_demoted(&[])), Some(Vec::new()));
        for cut in 0..bytes.len() {
            assert_eq!(decode_demoted(&bytes[..cut]), None, "cut at {cut}");
        }
        let mut huge = encode_demoted(&[]);
        huge[5..9].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode_demoted(&huge), None);
    }

    #[test]
    fn a_record_round_trips_and_damage_reads_as_absent() {
        let mut r = VerifyRecord::default();
        r.add_run(600, 0);
        r.add_run(1_000, 3);
        assert_eq!(
            r,
            VerifyRecord {
                frames: 1_600,
                divergences: 3,
                runs: 2
            }
        );
        assert_eq!(VerifyRecord::decode(&r.encode()), Some(r));
        let bytes = r.encode();
        for cut in 0..bytes.len() {
            assert_eq!(VerifyRecord::decode(&bytes[..cut]), None, "cut at {cut}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(VerifyRecord::decode(&longer), None);
        let mut other_version = bytes;
        other_version[4] = 9;
        assert_eq!(VerifyRecord::decode(&other_version), None);
    }

    #[test]
    fn counters_saturate_instead_of_wrapping() {
        let mut r = VerifyRecord {
            frames: u64::MAX - 1,
            divergences: u64::MAX,
            runs: u32::MAX,
        };
        r.add_run(10, 10);
        assert_eq!(
            (r.frames, r.divergences, r.runs),
            (u64::MAX, u64::MAX, u32::MAX)
        );
    }

    #[test]
    fn the_states_follow_the_threshold() {
        let rec = |frames, divergences, runs| VerifyRecord {
            frames,
            divergences,
            runs,
        };
        assert_eq!(Verification::of(None), Verification::NotChecked);
        assert_eq!(
            Verification::of(Some(&rec(0, 0, 1))),
            Verification::NotChecked
        );
        assert_eq!(
            Verification::of(Some(&rec(600, 0, 1))),
            Verification::Checked { frames: 600 }
        );
        // Long enough but a single run: not proven.
        assert_eq!(
            Verification::of(Some(&rec(PROVEN_FRAMES, 0, 1))),
            Verification::Checked {
                frames: PROVEN_FRAMES
            }
        );
        // Two runs but one frame short.
        assert_eq!(
            Verification::of(Some(&rec(PROVEN_FRAMES - 1, 0, 2))),
            Verification::Checked {
                frames: PROVEN_FRAMES - 1
            }
        );
        assert_eq!(
            Verification::of(Some(&rec(PROVEN_FRAMES, 0, 2))),
            Verification::Proven {
                frames: PROVEN_FRAMES
            }
        );
        // One difference anywhere spoils it, however much else passed.
        assert_eq!(
            Verification::of(Some(&rec(PROVEN_FRAMES * 10, 1, 9))),
            Verification::Diverged {
                frames: PROVEN_FRAMES * 10,
                divergences: 1
            }
        );
    }

    /// The cache key's settings string names the atlas format it vouches for.
    #[test]
    fn the_settings_string_carries_the_atlas_format() {
        assert!(VERIFY_SETTINGS.ends_with(crate::CACHE_SETTINGS));
    }
}
