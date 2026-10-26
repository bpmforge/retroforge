//! Turning Rebuild on for the current ROM (tickets W36-03, W37-01): the
//! generic tier for any NES or SNES cartridge, with no recording first.
//!
//! ## Flow
//!
//! 1. Turning Rebuild on starts a **first job** on its own thread and keeps
//!    the game running on the plain interpreter meanwhile. The job returns the
//!    cached atlas for the ROM's hash if one was stored, else an atlas built
//!    from the cached coverage plus anything the player recorded, else the
//!    **vectors-only atlas**: RESET/NMI/IRQ (and the SNES native vectors)
//!    decoded statically, every instruction `Inferred`
//!    (`rf_nes::rebuild::add_vector_seeds`, `rf_snes::rebuild::add_vector_seeds`).
//! 2. The coverage recorder is armed underneath for as long as Rebuild is on
//!    (unless the player already armed it). Every [`GROW_CHECK_FRAMES`] frames,
//!    if the recorded coverage grew and no job is running, a **grow job**
//!    rebuilds from `cached log + this session's log`, stores the atlas and
//!    the merged log under the ROM's hash, and hands the new atlas back.
//! 3. The core thread installs each finished atlas at a frame boundary
//!    ([`RebuildCtl::advance`]): the executor is the interpreter with a map,
//!    so a swap cannot change what the machine does (the lockstep tests swap
//!    mid-run and compare every frame). Atlas building never runs on the core
//!    thread (law 4: only the install does).
//!
//! The cache keeps two entries per ROM: the atlas (`code-atlas-v5`) and the
//! merged recording (`code-coverage-v1`). The atlas cannot absorb new coverage
//! by itself (its blocks and functions are derived, and the NES window maps the
//! builder resolves targets by are not recoverable from it), so a rebuild is
//! always from the recording. An atlas stored without a recording (the
//! harness, or a pre-W37 session) is used as it is and replaced only by a
//! grown atlas with at least as many instructions.
//!
//! The cache key is built here from `rf_rebuild`'s constants. The harness's
//! `atlas::atlas_cache_key` builds the same four strings from the same
//! constants (it is a dev-dependency of this crate, so it cannot be called
//! from non-test code); a unit test asserts the two keys are equal.
//! The cache lives in its own directory: the canvas cache is open on the core
//! thread, and two handles on one index file could race.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rf_cache::{Cache, CacheKey};
use rf_core_api::{CoverageLog, RebuildStats};
use rf_nes::rebuild::Atlas;
use rf_rebuild::{
    decode_coverage, encode_coverage, Provenance, Verification, VerifyRecord, CACHE_ASSET,
    CACHE_PRODUCER, CACHE_SETTINGS, COVERAGE_ASSET, COVERAGE_SETTINGS, VERIFY_ASSET,
    VERIFY_SETTINGS,
};

use crate::stepper::EmuStepper;

const ATLAS_CACHE_CAP_BYTES: u64 = 64 * 1024 * 1024;

/// Frames between checks for "did coverage grow, rebuild?" (5 s at 60 fps).
/// The wait also stretches to four times the last build's wall time, so a
/// slow build on a big ROM never runs back to back.
pub const GROW_CHECK_FRAMES: u64 = 300;

/// Where the atlas now installed came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AtlasSource {
    /// The vectors alone: nothing recorded yet.
    #[default]
    Vectors,
    /// The per-ROM cache, as stored.
    Cached,
    /// Built from recorded coverage (cached, this session's, or both).
    Coverage,
}

/// What the installed atlas holds, worked out where it is built (never on the
/// core thread): the panel's coverage and size lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AtlasFacts {
    pub functions: u32,
    /// Instructions the atlas holds, observed and inferred.
    pub instructions: u32,
    /// Bytes of instructions the recording had executed.
    pub observed_bytes: u64,
    /// Size of the ROM image's code space the atlas indexes.
    pub prg_len: u64,
    /// The atlas as stored in the cache.
    pub stored_bytes: u64,
}

impl AtlasFacts {
    /// Share of the ROM's code space the recording ran, `0.0..=1.0`.
    #[must_use]
    pub fn coverage(&self) -> f64 {
        match self.prg_len {
            0 => 0.0,
            n => (self.observed_bytes as f64 / n as f64).min(1.0),
        }
    }
}

/// The facts of `atlas`. Walks every instruction and serialises once: call it
/// on the build thread.
#[must_use]
pub fn atlas_facts(atlas: &Atlas) -> AtlasFacts {
    let observed_bytes = atlas
        .insns()
        .filter(|i| i.prov == Provenance::Observed)
        .map(|i| u64::from(i.len))
        .sum();
    AtlasFacts {
        functions: u32::try_from(atlas.functions.len()).unwrap_or(u32::MAX),
        instructions: atlas
            .stats
            .observed_insns
            .saturating_add(atlas.stats.inferred_insns),
        observed_bytes,
        prg_len: u64::from(atlas.prg_len),
        stored_bytes: atlas.to_bytes().len() as u64,
    }
}

/// What the Rebuild panel shows while on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RebuildInfo {
    /// What the installed atlas holds (coverage, size).
    pub facts: AtlasFacts,
    /// What the lockstep tools have recorded for this ROM (per-ROM cache).
    pub verification: Verification,
    /// The executor's counters since the current atlas was installed, so the
    /// fallback rate is the current atlas's, not an average over the session.
    pub stats: RebuildStats,
    /// Atlases swapped in after the first.
    pub swaps: u32,
    /// A grow job is running now.
    pub rebuilding: bool,
    pub source: AtlasSource,
    /// Wall time of the build that produced the installed atlas.
    pub last_build: Duration,
}

/// Where the Rebuild button stands, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RebuildStatus {
    #[default]
    Off,
    /// On, and the first atlas is still being built; the game runs on the
    /// interpreter meanwhile.
    Building,
    Failed(String),
    On(RebuildInfo),
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

/// A finished background build.
pub struct BuildOutcome {
    pub atlas: Box<Atlas>,
    pub facts: AtlasFacts,
    /// The ROM's verification record as the job found it.
    pub verification: Verification,
    pub source: AtlasSource,
    /// Wall time of the whole job (cache reads, build, cache writes).
    pub elapsed: Duration,
    /// The first job's stored recording, which grow jobs build on.
    pub base: Option<Box<CoverageLog>>,
}

/// Result of a background build.
pub enum RebuildBuild {
    Ready(Box<BuildOutcome>),
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

fn verify_key(rom_sha256: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: VERIFY_ASSET.to_string(),
        producer: CACHE_PRODUCER.to_string(),
        settings_hash: VERIFY_SETTINGS.to_string(),
    }
}

/// What the lockstep tools recorded for this ROM; "not checked" on a miss, a
/// damaged entry or no cache.
fn load_verification(cache: Option<&mut Cache>, sha: &str) -> Verification {
    let record = cache
        .and_then(|c| c.get(&verify_key(sha)).ok().flatten())
        .and_then(|b| VerifyRecord::decode(&b));
    Verification::of(record.as_ref())
}

/// The verification state stored for the ROM with this hash (a test or tool
/// asking the way the first Rebuild job does). Blocking file I/O.
#[must_use]
pub fn stored_verification(rom_sha256: &str) -> Verification {
    let mut cache = Cache::open(cache_root(), ATLAS_CACHE_CAP_BYTES).ok();
    load_verification(cache.as_mut(), rom_sha256)
}

fn coverage_key(rom_sha256: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: COVERAGE_ASSET.to_string(),
        producer: CACHE_PRODUCER.to_string(),
        settings_hash: COVERAGE_SETTINGS.to_string(),
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

/// How much recorded code an atlas stands on. Never shrinks as coverage is
/// added (the executed set only grows), unlike the total: a newly executed
/// byte can displace inferred decodes that overlapped it.
fn observed_of(a: &Atlas) -> u64 {
    u64::from(a.stats.observed_insns)
}

/// The atlas of `image` (an iNES file, or a SNES cartridge with or without a
/// copier header) from `log`, with the hardware vectors added as seeds. An
/// empty `log` gives the vectors-only atlas. Blocking and CPU-bound (seconds
/// for a 4 MiB ROM): never call it on the core thread.
///
/// # Errors
/// The image is not a cartridge this build can read.
pub fn build_for(image: &[u8], log: &CoverageLog) -> Result<Atlas, String> {
    if image.starts_with(b"NES\x1a") {
        let rom = rf_nes::NesRom::from_ines_bytes(image)
            .map_err(|e| format!("not an NES image: {e:?}"))?;
        Ok(rf_nes::rebuild::build_seeded_atlas(rom.prg_rom(), log))
    } else {
        rf_snes::rebuild::build_seeded_atlas_from_image(image, log)
            .map_err(|e| format!("not a SNES image: {e}"))
    }
}

fn load_atlas(cache: &mut Cache, sha: &str) -> Option<Atlas> {
    let bytes = cache.get(&key(sha)).ok().flatten()?;
    Atlas::from_bytes(&bytes).ok()
}

fn load_coverage(cache: &mut Cache, sha: &str) -> Option<CoverageLog> {
    let bytes = cache.get(&coverage_key(sha)).ok().flatten()?;
    decode_coverage(&bytes)
}

/// Did anything get recorded?
fn recorded(log: &CoverageLog) -> bool {
    log.instructions_recorded > 0 || log.prg_flags.iter().any(|&f| f != 0)
}

/// The first job (see the module doc): cached atlas, else atlas from the
/// cached recording plus `user` (what the player recorded before turning
/// Rebuild on), else the vectors-only atlas. Blocking; run it on its own
/// thread.
#[must_use]
pub fn first_atlas(image: &[u8], rom_sha256: &str, user: Option<&CoverageLog>) -> RebuildBuild {
    first_atlas_in(&cache_root(), image, rom_sha256, user)
}

fn first_atlas_in(
    root: &Path,
    image: &[u8],
    sha: &str,
    user: Option<&CoverageLog>,
) -> RebuildBuild {
    let t = Instant::now();
    let mut cache = Cache::open(root, ATLAS_CACHE_CAP_BYTES).ok();
    let cached_atlas = cache.as_mut().and_then(|c| load_atlas(c, sha));
    let mut log = cache.as_mut().and_then(|c| load_coverage(c, sha));
    let user = user.filter(|l| recorded(l));
    if let Some(u) = user {
        // No stored recording, or one for another size: the player's wins.
        let merged = log.as_mut().is_some_and(|l| l.merge(u));
        if !merged {
            log = Some(u.clone());
        }
    }
    let verification = load_verification(cache.as_mut(), sha);
    let ready = |atlas: Atlas, source, log: Option<CoverageLog>| {
        RebuildBuild::Ready(Box::new(BuildOutcome {
            facts: atlas_facts(&atlas),
            verification,
            atlas: Box::new(atlas),
            source,
            elapsed: t.elapsed(),
            base: log.map(Box::new),
        }))
    };
    if let (Some(atlas), None) = (cached_atlas.clone(), user) {
        return ready(atlas, AtlasSource::Cached, log);
    }
    if let Some(log) = log.filter(recorded) {
        return match build_for(image, &log) {
            Ok(atlas) => {
                store(cache.as_mut(), sha, &atlas, Some(&log), 0);
                ready(atlas, AtlasSource::Coverage, Some(log))
            }
            Err(e) => RebuildBuild::Failed(e),
        };
    }
    match build_for(image, &CoverageLog::new(0)) {
        Ok(atlas) => ready(atlas, AtlasSource::Vectors, None),
        Err(e) => RebuildBuild::Failed(e),
    }
}

/// A grow job: rebuild from `log` (the whole recording: cached, this
/// session's and the player's) and store the result. The atlas is stored only
/// if it rests on at least `keep_observed` recorded instructions, so a thin
/// session never replaces a richer atlas that had no recording to grow from; the recording
/// is always stored. Blocking; run it on its own thread.
#[must_use]
pub fn grow_atlas(
    image: &[u8],
    rom_sha256: &str,
    log: &CoverageLog,
    keep_observed: u64,
) -> RebuildBuild {
    grow_atlas_in(&cache_root(), image, rom_sha256, log, keep_observed)
}

fn grow_atlas_in(
    root: &Path,
    image: &[u8],
    sha: &str,
    log: &CoverageLog,
    keep_observed: u64,
) -> RebuildBuild {
    let t = Instant::now();
    match build_for(image, log) {
        Ok(atlas) => {
            let mut cache = Cache::open(root, ATLAS_CACHE_CAP_BYTES).ok();
            store(cache.as_mut(), sha, &atlas, Some(log), keep_observed);
            RebuildBuild::Ready(Box::new(BuildOutcome {
                facts: atlas_facts(&atlas),
                verification: load_verification(cache.as_mut(), sha),
                atlas: Box::new(atlas),
                source: AtlasSource::Coverage,
                elapsed: t.elapsed(),
                base: None,
            }))
        }
        Err(e) => RebuildBuild::Failed(e),
    }
}

/// Write the atlas (if big enough) and the recording. A cache that cannot be
/// written only costs the next build.
fn store(
    cache: Option<&mut Cache>,
    sha: &str,
    atlas: &Atlas,
    log: Option<&CoverageLog>,
    keep_observed: u64,
) {
    let Some(c) = cache else { return };
    if observed_of(atlas) >= keep_observed {
        let _ = c.put(&key(sha), &atlas.to_bytes());
    }
    if let Some(l) = log {
        let _ = c.put(&coverage_key(sha), &encode_coverage(l));
    }
}

/// What a recording had grown to when a build started: the next build is
/// worth running only if this changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Growth {
    code_bytes: usize,
    edges: usize,
    windows: usize,
    interrupts: usize,
}

impl Growth {
    fn of(logs: &[Option<&CoverageLog>]) -> Self {
        logs.iter().flatten().fold(Self::default(), |g, l| Self {
            code_bytes: g.code_bytes + l.opcode_prg_bytes(),
            edges: g.edges + l.indirect.len(),
            windows: g.windows + l.windows.len(),
            interrupts: g.interrupts + l.interrupts.len(),
        })
    }
}

struct Session {
    image: Arc<Vec<u8>>,
    sha: String,
    /// The stored recording the first job found (grow jobs build on it).
    base: Option<CoverageLog>,
    /// Everything recorded since Rebuild went on, taken from the recorder.
    live: Option<CoverageLog>,
    /// The recorder was armed by Rebuild, not by the player.
    armed_here: bool,
    first_installed: bool,
    swaps: u32,
    installed_observed: u64,
    facts: AtlasFacts,
    verification: Verification,
    source: AtlasSource,
    last_build: Duration,
    last_growth: Growth,
    next_check: u64,
}

/// Rebuild's state on the core thread: status, the running job and the
/// session. Everything that touches the machine happens here, on that thread.
#[derive(Default)]
pub struct RebuildCtl {
    pub status: RebuildStatus,
    dirty: bool,
    job: Option<Receiver<RebuildBuild>>,
    session: Option<Session>,
}

fn merge_into(acc: &mut Option<CoverageLog>, taken: CoverageLog) {
    match acc {
        Some(total) => {
            total.merge(&taken);
        }
        None => *acc = Some(taken),
    }
}

impl RebuildCtl {
    /// Rebuild is on (building, running or failed to start).
    #[must_use]
    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    /// Turn Rebuild on: arm the recorder (if the player has not) and start the
    /// first job. `user` is what the player recorded so far.
    pub fn start(
        &mut self,
        stepper: &mut EmuStepper,
        image: &[u8],
        sha: &str,
        coverage_on: bool,
        user: Option<&CoverageLog>,
    ) {
        self.stop(stepper, coverage_on);
        if !coverage_on {
            stepper.set_coverage_recording(true);
        }
        let image = Arc::new(image.to_vec());
        let (tx, rx) = mpsc::channel();
        let (img, sha_owned, user) = (Arc::clone(&image), sha.to_string(), user.cloned());
        std::thread::spawn(move || {
            let _ = tx.send(first_atlas(&img, &sha_owned, user.as_ref()));
        });
        self.job = Some(rx);
        self.session = Some(Session {
            image,
            sha: sha.to_string(),
            base: None,
            live: None,
            armed_here: !coverage_on,
            first_installed: false,
            swaps: 0,
            installed_observed: 0,
            facts: AtlasFacts::default(),
            verification: Verification::NotChecked,
            source: AtlasSource::Vectors,
            last_build: Duration::ZERO,
            last_growth: Growth::default(),
            next_check: 0,
        });
        self.status = RebuildStatus::Building;
        self.dirty = true;
    }

    /// Turn Rebuild off: drop the atlas and, if Rebuild armed it, the recorder.
    pub fn stop(&mut self, stepper: &mut EmuStepper, coverage_on: bool) {
        self.job = None;
        let _ = stepper.set_rebuild_atlas(None);
        if let Some(s) = self.session.take() {
            if s.armed_here && !coverage_on {
                stepper.set_coverage_recording(false);
            }
        }
        if self.status != RebuildStatus::Off {
            self.dirty = true;
        }
        self.status = RebuildStatus::Off;
    }

    /// Before the player arms or disarms the recorder: keep what Rebuild has
    /// recorded, because arming starts a fresh log and disarming drops it.
    pub fn before_coverage_toggle(&mut self, stepper: &mut EmuStepper, coverage_on: bool) {
        if let (Some(s), false) = (self.session.as_mut(), coverage_on) {
            if let Some(t) = stepper.take_coverage() {
                merge_into(&mut s.live, t);
            }
        }
    }

    /// After the player armed or disarmed the recorder: Rebuild needs it
    /// armed, so disarming hands it back to Rebuild.
    pub fn after_coverage_toggle(&mut self, stepper: &mut EmuStepper, now_on: bool) {
        if let Some(s) = self.session.as_mut() {
            if now_on {
                s.armed_here = false;
            } else {
                stepper.set_coverage_recording(true);
                s.armed_here = true;
            }
        }
    }

    /// Frame-boundary step: install a finished atlas, start a grow job when
    /// coverage has grown. Returns `true` when a new atlas was installed (the
    /// caller re-resolves function hooks against it: ids are per atlas).
    pub fn advance(
        &mut self,
        stepper: &mut EmuStepper,
        coverage_on: bool,
        user: &mut Option<CoverageLog>,
    ) -> bool {
        let installed = self.collect(stepper);
        self.maybe_grow(stepper, coverage_on, user);
        installed
    }

    fn collect(&mut self, stepper: &mut EmuStepper) -> bool {
        let Some(rx) = self.job.as_ref() else {
            return false;
        };
        let done = match rx.try_recv() {
            Ok(done) => done,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                RebuildBuild::Failed("atlas build thread ended".to_string())
            }
        };
        self.job = None;
        self.dirty = true;
        let Some(s) = self.session.as_mut() else {
            return false;
        };
        let out = match done {
            RebuildBuild::Ready(out) => out,
            RebuildBuild::Failed(e) => {
                // A failed grow keeps the atlas running; a failed first job
                // leaves nothing to run.
                if !s.first_installed {
                    self.status = RebuildStatus::Failed(e);
                }
                return false;
            }
        };
        let insns = observed_of(&out.atlas);
        let first = !s.first_installed;
        // The record may have grown since (a lockstep run finished): always
        // take the newest reading, installed or not.
        s.verification = out.verification;
        // A grown atlas that rests on less recorded code than the one running
        // (a thin session over a richer cache) is not an improvement.
        if !first && insns < s.installed_observed {
            s.last_build = out.elapsed;
            return false;
        }
        match stepper.set_rebuild_atlas(Some(out.atlas)) {
            Ok(()) => {
                if first {
                    s.first_installed = true;
                    s.base = out.base.map(|b| *b);
                } else {
                    s.swaps += 1;
                }
                s.installed_observed = insns;
                s.facts = out.facts;
                s.source = out.source;
                s.last_build = out.elapsed;
                // Grow checks start from the first install.
                s.next_check = stepper.frame_count() + GROW_CHECK_FRAMES;
                self.status = RebuildStatus::On(info(stepper, s, false));
                true
            }
            Err(e) => {
                self.status = RebuildStatus::Failed(e.to_string());
                false
            }
        }
    }

    fn maybe_grow(
        &mut self,
        stepper: &mut EmuStepper,
        coverage_on: bool,
        user: &mut Option<CoverageLog>,
    ) {
        let frame = stepper.frame_count();
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if self.job.is_some() || !s.first_installed || frame < s.next_check {
            return;
        }
        // The recorder's content goes where its owner reads it: the player's
        // accumulator while the player records, Rebuild's own otherwise.
        if let Some(t) = stepper.take_coverage() {
            if coverage_on {
                merge_into(user, t);
            } else {
                merge_into(&mut s.live, t);
            }
        }
        let wait = (s.last_build.as_secs_f64() * 4.0 * 60.0) as u64;
        s.next_check = frame + GROW_CHECK_FRAMES.max(wait);
        let growth = Growth::of(&[s.live.as_ref(), user.as_ref()]);
        if growth == s.last_growth {
            return;
        }
        s.last_growth = growth;
        let mut log = s.base.clone();
        for part in [s.live.as_ref(), user.as_ref()].into_iter().flatten() {
            match log.as_mut() {
                Some(l) => {
                    l.merge(part);
                }
                None => log = Some(part.clone()),
            }
        }
        let Some(log) = log else { return };
        let (tx, rx) = mpsc::channel();
        let (img, sha, keep) = (Arc::clone(&s.image), s.sha.clone(), s.installed_observed);
        std::thread::spawn(move || {
            let _ = tx.send(grow_atlas(&img, &sha, &log, keep));
        });
        self.job = Some(rx);
        self.dirty = true;
        if let RebuildStatus::On(i) = &mut self.status {
            i.rebuilding = true;
        }
    }

    /// What the panel should hear this frame: a change once, and the counters
    /// on the refresh cadence (`tick`) while on.
    pub fn report(&mut self, stepper: &EmuStepper, tick: bool) -> Option<RebuildStatus> {
        if let (RebuildStatus::On(_), Some(s)) = (&self.status, self.session.as_ref()) {
            if tick || self.dirty {
                self.status = RebuildStatus::On(info(stepper, s, self.job.is_some()));
                self.dirty = true;
            }
        }
        std::mem::take(&mut self.dirty).then(|| self.status.clone())
    }
}

fn info(stepper: &EmuStepper, s: &Session, rebuilding: bool) -> RebuildInfo {
    RebuildInfo {
        facts: s.facts,
        verification: s.verification,
        stats: stepper.rebuild_stats().unwrap_or_default(),
        swaps: s.swaps,
        rebuilding,
        source: s.source,
        last_build: s.last_build,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::coverage::{WindowMap, OPCODE};

    /// 32 KiB NROM: a reset at `$8000` (`JSR $8010 ; JMP *`), the callee at
    /// `$8010` (`RTS`) and an NMI at `$8020` (`RTI`).
    fn tiny_ines() -> Vec<u8> {
        let mut d = vec![0u8; 16];
        d[0..4].copy_from_slice(b"NES\x1a");
        d[4] = 2;
        let mut prg = vec![0xEAu8; 0x8000];
        prg[0..6].copy_from_slice(&[0x20, 0x10, 0x80, 0x4C, 0x03, 0x80]);
        prg[0x10] = 0x60;
        prg[0x20] = 0x40;
        prg[0x7FFA..0x8000].copy_from_slice(&[0x20, 0x80, 0x00, 0x80, 0x00, 0x00]);
        d.extend(prg);
        d
    }

    fn lorom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0] = 0x80; // BRA *
        rom[1] = 0xFE;
        rom[0x7FD5] = 0x20;
        rom[0x7FD7] = 0x08;
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        rom
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rf-rebuild-shell-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn ready(b: RebuildBuild) -> Box<BuildOutcome> {
        match b {
            RebuildBuild::Ready(o) => o,
            RebuildBuild::Failed(e) => panic!("build failed: {e}"),
        }
    }

    fn nes_log() -> CoverageLog {
        let mut log = CoverageLog::new(0x8000);
        let mut m = WindowMap::EMPTY;
        for (p, s) in m.0.iter_mut().enumerate() {
            *s = p as u32 * 0x1000;
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
    fn the_verification_key_is_the_harness_key() {
        let sha = "ab".repeat(32);
        assert_eq!(verify_key(&sha), rf_harness::atlas::verify_cache_key(&sha));
    }

    /// Ticket W37-03: the first job reports what the lockstep tools stored
    /// (written through the harness's own writer), and the atlas's facts.
    #[test]
    fn the_first_job_reads_the_verification_record_and_the_atlas_facts() {
        let root = dir("verify");
        let (ines, sha) = (tiny_ines(), "9a".repeat(32));
        let first = ready(first_atlas_in(&root, &ines, &sha, Some(&nes_log())));
        assert_eq!(first.verification, Verification::NotChecked);
        assert_eq!(first.facts.functions as usize, first.atlas.functions.len());
        assert!(first.facts.observed_bytes >= 1);
        assert_eq!(first.facts.prg_len, u64::from(first.atlas.prg_len));
        assert!(first.facts.stored_bytes > 0);
        assert!(first.facts.coverage() > 0.0 && first.facts.coverage() <= 1.0);
        {
            let mut cache = Cache::open(&root, ATLAS_CACHE_CAP_BYTES).expect("open");
            rf_harness::atlas::record_lockstep_run(&mut cache, &sha, 600, 0).expect("record");
        }
        let again = ready(first_atlas_in(&root, &ines, &sha, None));
        assert_eq!(again.verification, Verification::Checked { frames: 600 });
        {
            let mut cache = Cache::open(&root, ATLAS_CACHE_CAP_BYTES).expect("open");
            rf_harness::atlas::record_lockstep_run(&mut cache, &sha, 10, 1).expect("record");
        }
        let grown = ready(grow_atlas_in(&root, &ines, &sha, &nes_log(), 0));
        assert_eq!(
            grown.verification,
            Verification::Diverged {
                frames: 610,
                divergences: 1
            }
        );
        // Another ROM was never checked.
        let other = ready(first_atlas_in(&root, &ines, &"9b".repeat(32), None));
        assert_eq!(other.verification, Verification::NotChecked);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Ticket W37-01: no cache, no recording: the first atlas is the vectors
    /// alone, all `Inferred`, for either console, and nothing is stored (a
    /// mod must not resolve its patches against a vectors-only atlas).
    #[test]
    fn with_nothing_recorded_the_first_atlas_is_the_vectors_alone() {
        for (tag, image, nodes) in [("nes", tiny_ines(), 3), ("snes", lorom(), 1)] {
            let root = dir(tag);
            let sha = "ef".repeat(32);
            let out = ready(first_atlas_in(&root, &image, &sha, None));
            assert_eq!(out.source, AtlasSource::Vectors, "{tag}");
            assert_eq!(out.atlas.stats.observed_insns, 0, "{tag}");
            assert!(
                out.atlas.stats.inferred_insns >= nodes,
                "{tag}: {:?}",
                out.atlas.stats
            );
            assert!(!out.atlas.functions.is_empty(), "{tag}");
            assert!(out.base.is_none());
            assert!(cached_atlas_in(&root, &sha).is_none(), "{tag}: not stored");
            // An empty recording (armed, nothing run) is the same thing.
            let empty = CoverageLog::new(0x8000);
            let again = ready(first_atlas_in(&root, &image, &sha, Some(&empty)));
            assert_eq!(again.source, AtlasSource::Vectors, "{tag}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    #[should_panic(expected = "build failed")]
    fn a_non_cartridge_is_a_failed_build_not_a_panic_in_the_thread() {
        let _ = ready(first_atlas_in(&dir("junk2"), b"not a rom", "00", None));
    }

    /// Recorded coverage builds an atlas that is stored with its recording; a
    /// later first job finds both, and the stored recording is what grow jobs
    /// build on.
    #[test]
    fn coverage_builds_stores_both_entries_and_the_next_session_reuses_them() {
        let root = dir("store");
        let (ines, sha) = (tiny_ines(), "12".repeat(32));
        let out = ready(first_atlas_in(&root, &ines, &sha, Some(&nes_log())));
        assert_eq!(out.source, AtlasSource::Coverage);
        assert!(out.atlas.stats.observed_insns >= 1);
        assert!(cached_atlas_in(&root, &sha).is_some());
        let again = ready(first_atlas_in(&root, &ines, &sha, None));
        assert_eq!(again.source, AtlasSource::Cached);
        assert_eq!(*again.atlas, *out.atlas);
        let base = again.base.expect("the recording came back with it");
        assert_eq!(base.instructions_recorded, 1);
        assert_ne!(base.prg_flags[0] & OPCODE, 0);
        // Another ROM misses.
        let other = ready(first_atlas_in(&root, &ines, &"cd".repeat(32), None));
        assert_eq!(other.source, AtlasSource::Vectors);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A grow job over the stored recording plus new coverage keeps both.
    #[test]
    fn a_grow_job_merges_new_coverage_into_the_cached_recording() {
        let root = dir("grow");
        let (ines, sha) = (tiny_ines(), "34".repeat(32));
        let first = ready(first_atlas_in(&root, &ines, &sha, Some(&nes_log())));
        let mut base = *ready(first_atlas_in(&root, &ines, &sha, None))
            .base
            .unwrap();
        let mut more = CoverageLog::new(0x8000);
        more.mark_prg(0x10, OPCODE); // the RTS
        more.instructions_recorded = 5;
        assert!(base.merge(&more));
        let grown = ready(grow_atlas_in(&root, &ines, &sha, &base, 0));
        assert!(grown.atlas.stats.observed_insns > first.atlas.stats.observed_insns);
        let stored = ready(first_atlas_in(&root, &ines, &sha, None));
        assert_eq!(*stored.atlas, *grown.atlas, "the grown atlas replaced it");
        let log = *stored.base.expect("recording stored");
        assert_eq!(log.instructions_recorded, 6);
        assert_ne!(log.prg_flags[0x10] & OPCODE, 0);
        assert_ne!(log.prg_flags[0] & OPCODE, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A session that saw less than the stored atlas holds never replaces it.
    #[test]
    fn a_thin_grow_does_not_replace_a_richer_stored_atlas() {
        let root = dir("thin");
        let (ines, sha) = (tiny_ines(), "56".repeat(32));
        let rich = ready(first_atlas_in(&root, &ines, &sha, Some(&nes_log())));
        let keep = observed_of(&rich.atlas) + 1;
        let thin = ready(grow_atlas_in(
            &root,
            &ines,
            &sha,
            &CoverageLog::new(0x8000),
            keep,
        ));
        assert!(observed_of(&thin.atlas) < keep);
        assert_eq!(cached_atlas_in(&root, &sha).as_ref(), Some(&*rich.atlas));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A SNES image through the same flow, with the 65816 ISA.
    #[test]
    fn a_snes_image_builds_with_the_65816_isa_and_hits_the_cache() {
        let rom = lorom();
        let root = dir("snes-store");
        let sha = "78".repeat(32);
        let mut log = CoverageLog::with_volatile_len(rom.len(), 128 * 1024);
        log.mark_prg(0, OPCODE | rf_core_api::coverage::mode_bit(0));
        log.mark_prg(1, rf_core_api::coverage::OPERAND);
        log.instructions_recorded = 1;
        let out = ready(first_atlas_in(&root, &rom, &sha, Some(&log)));
        assert_eq!(out.atlas.mode_count, 5, "built with the 65816 ISA");
        assert!(out.atlas.insns().any(|i| i.rom == 0 && i.mode == 0));
        let again = ready(first_atlas_in(&root, &rom, &sha, None));
        assert_eq!(again.source, AtlasSource::Cached);
        let _ = std::fs::remove_dir_all(&root);
    }
}
