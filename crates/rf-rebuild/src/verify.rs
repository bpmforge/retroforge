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
