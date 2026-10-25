//! Turning Rebuild on for the current ROM (ticket W36-03): find or build the
//! code atlas, off the core thread.
//!
//! The atlas comes from the per-ROM cache (`code-atlas-v2`) when present,
//! otherwise from the coverage the session has recorded so far, and is then
//! stored for next time. If there is neither, the answer is
//! [`RebuildBuild::NeedCoverage`] and the panel says to record first.
//!
//! The cache key is built here from `rf_rebuild`'s constants. The harness's
//! `atlas::atlas_cache_key` builds the same four strings from the same
//! constants (it is a dev-dependency of this crate, so it cannot be called
//! from non-test code); a unit test asserts the two keys are equal.
//! The cache lives in its own directory: the canvas cache is open on the core
//! thread, and two handles on one index file could race.

use std::path::PathBuf;

use rf_cache::{Cache, CacheKey};
use rf_core_api::{CoverageLog, RebuildStats};
use rf_nes::rebuild::Atlas;
use rf_rebuild::{CACHE_ASSET, CACHE_PRODUCER, CACHE_SETTINGS};

const ATLAS_CACHE_CAP_BYTES: u64 = 64 * 1024 * 1024;

/// Where the Rebuild button stands, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RebuildStatus {
    #[default]
    Off,
    Building,
    /// No cached atlas and no recorded coverage to build one from.
    NeedCoverage,
    Failed(String),
    On(RebuildStats),
}

/// What the UI asks the core thread to load as function hooks (ticket
/// W36-05): a Lua script and the recipe its hook names resolve against.
/// Plain data, so it can cross the command channel; the Lua state itself is
/// created on the core thread and stays there (it is not `Send`).
pub struct HookSetup {
    pub manifest: rf_plugin_sdk::Manifest,
    /// The script that calls `rf.on_enter` / `rf.on_exit`.
    pub lua_source: String,
    pub recipe: rf_rebuild::Recipe,
}

/// Result of the background build.
pub enum RebuildBuild {
    Ready(Box<Atlas>),
    NeedCoverage,
    Failed(String),
}

#[cfg(not(test))]
fn cache_root() -> PathBuf {
    std::env::temp_dir().join("retroforge-atlas-cache")
}

/// Unit tests get a cache of their own per test process: the shared
/// directory is also written by the app and by concurrent test runs (the
/// gate, a second worktree), and a core-thread test that read another
/// process's half-written index flaked in the 2026-10-25 gate runs.
#[cfg(test)]
fn cache_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "retroforge-atlas-cache-test-{}",
        std::process::id()
    ))
}

fn key(rom_sha256: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: CACHE_ASSET.to_string(),
        producer: CACHE_PRODUCER.to_string(),
        settings_hash: CACHE_SETTINGS.to_string(),
    }
}

/// The cached atlas of the image with this hash, if one was stored (ticket
/// W36-06: a recipe mod resolves its patch rows against it when a game opens,
/// before any core exists). `None` on a miss, an unreadable cache or an entry
/// of another format. Blocking file I/O.
#[must_use]
pub fn cached_atlas(rom_sha256: &str) -> Option<Atlas> {
    cached_atlas_in(&cache_root(), rom_sha256)
}

/// Store `atlas` where [`cached_atlas`] looks, as the Rebuild panel does after
/// a build (for tests that stand in for that step).
#[doc(hidden)]
pub fn store_atlas_for_test(rom_sha256: &str, atlas: &Atlas) {
    if let Ok(mut cache) = Cache::open(cache_root(), ATLAS_CACHE_CAP_BYTES) {
        let _ = cache.put(&key(rom_sha256), &atlas.to_bytes());
    }
}

fn cached_atlas_in(root: &std::path::Path, rom_sha256: &str) -> Option<Atlas> {
    let mut cache = Cache::open(root, ATLAS_CACHE_CAP_BYTES).ok()?;
    let bytes = cache.get(&key(rom_sha256)).ok().flatten()?;
    Atlas::from_bytes(&bytes).ok()
}

/// Cached atlas, else one built from `coverage`, else `NeedCoverage`.
/// Blocking; run it on its own thread.
#[must_use]
pub fn find_or_build(
    ines: &[u8],
    rom_sha256: &str,
    coverage: Option<&CoverageLog>,
) -> RebuildBuild {
    find_or_build_in(&cache_root(), ines, rom_sha256, coverage)
}

fn find_or_build_in(
    root: &std::path::Path,
    ines: &[u8],
    rom_sha256: &str,
    coverage: Option<&CoverageLog>,
) -> RebuildBuild {
    let mut cache = Cache::open(root, ATLAS_CACHE_CAP_BYTES).ok();
    let cached = cache
        .as_mut()
        .and_then(|c| c.get(&key(rom_sha256)).ok().flatten())
        .and_then(|b| Atlas::from_bytes(&b).ok());
    if let Some(atlas) = cached {
        return RebuildBuild::Ready(Box::new(atlas));
    }
    let Some(log) = coverage.filter(|l| l.instructions_recorded > 0) else {
        return RebuildBuild::NeedCoverage;
    };
    let rom = match rf_nes::NesRom::from_ines_bytes(ines) {
        Ok(r) => r,
        Err(e) => return RebuildBuild::Failed(format!("not an NES image: {e:?}")),
    };
    let atlas = rf_nes::rebuild::build_atlas(rom.prg_rom(), log);
    if let Some(c) = cache.as_mut() {
        // A cache that cannot be written only costs the next build.
        let _ = c.put(&key(rom_sha256), &atlas.to_bytes());
    }
    RebuildBuild::Ready(Box::new(atlas))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::coverage::{WindowMap, OPCODE};

    fn tiny_ines() -> Vec<u8> {
        let mut d = vec![0u8; 16];
        d[0..4].copy_from_slice(b"NES\x1a");
        d[4] = 1;
        d.extend(vec![0xEAu8; 0x4000]);
        d
    }

    fn log() -> CoverageLog {
        let mut log = CoverageLog::new(0x4000);
        let mut m = WindowMap::EMPTY;
        for (p, s) in m.0.iter_mut().enumerate() {
            *s = (p as u32 % 4) * 0x1000;
        }
        log.note_windows(m);
        log.mark_prg(0, OPCODE);
        log.instructions_recorded = 1;
        log
    }

    /// The shell's key is the harness's key (one cache entry per ROM, written
    /// by either side, read by the other).
    #[test]
    fn the_cache_key_is_the_harness_key() {
        let sha = "ab".repeat(32);
        assert_eq!(key(&sha), rf_harness::atlas::atlas_cache_key(&sha));
    }

    #[test]
    fn needs_coverage_then_builds_then_hits_the_cache() {
        let dir = std::env::temp_dir().join(format!("rf-rebuild-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ines = tiny_ines();
        let sha = "ef".repeat(32);
        assert!(matches!(
            find_or_build_in(&dir, &ines, &sha, None),
            RebuildBuild::NeedCoverage
        ));
        let empty = CoverageLog::new(0x4000);
        assert!(matches!(
            find_or_build_in(&dir, &ines, &sha, Some(&empty)),
            RebuildBuild::NeedCoverage
        ));
        assert!(matches!(
            find_or_build_in(&dir, &ines, &sha, Some(&log())),
            RebuildBuild::Ready(_)
        ));
        // Stored: no coverage needed any more.
        assert!(matches!(
            find_or_build_in(&dir, &ines, &sha, None),
            RebuildBuild::Ready(_)
        ));
        // And a mod can fetch it without a build (W36-06); another ROM misses.
        assert!(cached_atlas_in(&dir, &sha).is_some());
        assert!(cached_atlas_in(&dir, &"cd".repeat(32)).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
