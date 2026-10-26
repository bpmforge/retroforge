//! Savestate fuzzing for NES code nobody's script reached (ticket W37-05;
//! `docs/design/REBUILD_LAB.md` section 9).
//!
//! The atlas only knows code that ran. Scripted drives and input movies
//! cover the happy path; deaths, game over, pause, menus and level-end
//! sequences need inputs nobody wrote. This driver takes **seed savestates**
//! (captured during an ordinary drive), restores one, plays a short episode
//! of seeded-random input with structured bursts, and records what ran.
//!
//! ## Method
//!
//! * **Episodes** are `(corpus entry, episode seed)` pairs. The inputs are a
//!   pure function of the episode seed ([`gen_inputs`]), and the emulator is
//!   deterministic, so any find replays from its record ([`replay`]).
//! * **Novelty guidance.** An episode runs in segments of
//!   [`Config::segment`] frames. After each segment the segment's coverage is
//!   compared with everything seen so far; a segment that executed a PRG byte
//!   nobody had executed adds the savestate taken at its *start* to the
//!   corpus, so later episodes start right before the new code and can push
//!   on from there. Every segment's log is merged into the running union
//!   either way (indirect targets and bank windows feed the atlas too).
//! * **Crash safety.** Unseen code can jump anywhere. A `JAM`/`KIL` opcode
//!   (the core completes frames after one, so the flag is polled), a frame
//!   that never reaches a boundary (the core's own cycle budget reports it),
//!   a frozen machine and a panic inside the core each end the episode and
//!   are counted ([`HaltCounts`]); the run goes on from the next restore.
//! * **Law 8.** Every loop is a `for` over a range or slice, or advances an
//!   explicit counter by at least one on every path (noted where used).
//!
//! The driver knows the NES, not any game: game facts enter only through the
//! `label` hook, which returns a bit mask of whatever the caller wants to
//! attribute new code to (a test reads RAM for that).

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore as _, PpuPixel, ResetKind, StateError, StateReader, Step,
};
use rf_nes::core::NesCore;
use rf_nes::StateRegion;

use crate::lockstep::PayloadBuf;

/// Standard NES pad bits (the core's own layout, port 0 in the low byte).
pub mod pad {
    pub const A: u16 = 1;
    pub const B: u16 = 2;
    pub const SELECT: u16 = 4;
    pub const START: u16 = 8;
    pub const UP: u16 = 16;
    pub const DOWN: u16 = 32;
    pub const LEFT: u16 = 64;
    pub const RIGHT: u16 = 128;
}

// ------------------------------------------------------------------ rng

/// SplitMix64: a fixed, local, seedable generator (no host RNG, no clock).
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi` (`lo <= hi`; modulo bias is irrelevant here).
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        debug_assert!(lo <= hi);
        lo + (self.next_u64() % (u64::from(hi - lo) + 1)) as u32
    }

    fn percent(&mut self) -> u32 {
        self.range(0, 99)
    }
}

// ----------------------------------------------------------- savestates

struct SliceReader<'a> {
    data: &'a [u8],
}

impl StateReader for SliceReader<'_> {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), StateError> {
        let Some((head, tail)) = self.data.split_at_checked(buf.len()) else {
            return Err(StateError::Io("savestate region too short".to_string()));
        };
        buf.copy_from_slice(head);
        self.data = tail;
        Ok(())
    }
}

/// An in-process snapshot of a NES core: every [`StateRegion`] payload, the
/// same bytes the shipped `.rfstate` container carries, without the envelope.
/// Only valid at a frame boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Savestate(Vec<(StateRegion, Vec<u8>)>);

impl Savestate {
    /// # Errors
    /// A region refuses to save (mid-frame queues are not empty).
    pub fn capture(core: &NesCore) -> Result<Self, String> {
        let mut regions = Vec::with_capacity(StateRegion::ALL.len());
        for region in StateRegion::ALL {
            let mut buf = PayloadBuf::default();
            core.bus()
                .save_region(core.cpu(), region, &mut buf)
                .map_err(|e| format!("{e:?}"))?;
            regions.push((region, buf.0));
        }
        Ok(Savestate(regions))
    }

    /// # Errors
    /// A region does not load (a state of another cartridge).
    pub fn restore(&self, core: &mut NesCore) -> Result<(), String> {
        let (cpu, bus) = core.parts_mut();
        for (region, bytes) in &self.0 {
            let mut r = SliceReader { data: bytes };
            bus.load_region(cpu, *region, &mut r)
                .map_err(|e| format!("{e:?}"))?;
        }
        core.reset_hook_frames();
        Ok(())
    }

    /// Bytes held, for the budget line.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.iter().map(|r| r.1.len()).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// --------------------------------------------------------------- inputs

/// The pad words (port 0 low byte, port 1 high byte) of an episode of
/// `frames` frames, a pure function of `seed`: bursts of idle, held
/// directions, run-and-jump, mashing, Start taps (pause), Select/Start
/// combinations and arbitrary held masks. Impossible d-pad pairs (Up+Down)
/// appear on purpose: the hardware cannot produce them, a game's input code
/// may still meet them from a bad adapter, and they reach odd branches.
#[must_use]
pub fn gen_inputs(seed: u64, frames: u32) -> Vec<u16> {
    let n = frames as usize;
    let mut rng = Rng::new(seed);
    let mut out: Vec<u16> = Vec::with_capacity(n);
    let dirs = [
        pad::UP,
        pad::DOWN,
        pad::LEFT,
        pad::RIGHT,
        pad::UP | pad::RIGHT,
        pad::DOWN | pad::LEFT,
        pad::LEFT | pad::UP,
        pad::RIGHT | pad::DOWN,
    ];
    // Terminates: every pass of the loop pushes at least one word (`len` is
    // clamped to `1..=room` and each arm fills exactly `len`), so
    // `out.len()` strictly grows toward `n`.
    while out.len() < n {
        let room = n - out.len();
        let kind = rng.percent();
        let before = out.len();
        match kind {
            // Idle: long enough for attract modes and timeouts.
            0..=11 => {
                let long = rng.percent() < 12;
                let len = if long {
                    rng.range(300, 900)
                } else {
                    rng.range(30, 300)
                };
                fill(&mut out, len, room, |_| 0);
            }
            // Hold a direction, maybe with B and/or A.
            12..=31 => {
                let d = dirs[rng.range(0, dirs.len() as u32 - 1) as usize];
                let extra = (rng.next_u64() as u16) & (pad::A | pad::B);
                let len = rng.range(10, 120);
                fill(&mut out, len, room, |_| d | extra);
            }
            // Run right and jump on a period.
            32..=45 => {
                let period = rng.range(20, 90);
                let high = rng.range(4, period.max(5) - 1);
                let dir = if rng.percent() < 80 {
                    pad::RIGHT
                } else {
                    pad::LEFT
                };
                let len = rng.range(60, 300);
                fill(&mut out, len, room, |i| {
                    dir | pad::B | if i % period < high { pad::A } else { 0 }
                });
            }
            // Mash: a fresh random mask every 1..=4 frames.
            46..=63 => {
                let len = rng.range(20, 200);
                let hold = rng.range(1, 4);
                let two_pads = rng.percent() < 10;
                let mut mask = 0u16;
                fill(&mut out, len, room, |i| {
                    if i % hold == 0 {
                        mask = (rng.next_u64() & 0xFF) as u16;
                        if two_pads {
                            mask |= ((rng.next_u64() & 0xFF) as u16) << 8;
                        }
                    }
                    mask
                });
            }
            // Pause taps: Start for two frames, released for a while.
            64..=71 => {
                let taps = rng.range(1, 6);
                for _ in 0..taps {
                    let gap = rng.range(8, 40);
                    push_run(&mut out, n, 2, pad::START);
                    push_run(&mut out, n, gap, 0);
                }
            }
            // Select / Start combinations.
            72..=81 => {
                let combo = [
                    pad::SELECT,
                    pad::START,
                    pad::SELECT | pad::START,
                    pad::A | pad::B | pad::SELECT | pad::START,
                    pad::SELECT | pad::A,
                    pad::START | pad::B,
                ][rng.range(0, 5) as usize];
                let len = rng.range(2, 40);
                fill(&mut out, len, room, |_| combo);
                push_run(&mut out, n, rng.range(1, 30), 0);
            }
            // An arbitrary mask held.
            82..=89 => {
                let mask = (rng.next_u64() & 0xFF) as u16;
                let len = rng.range(10, 150);
                fill(&mut out, len, room, |_| mask);
            }
            // Alternate left and right quickly.
            90..=95 => {
                let flip = rng.range(2, 12);
                let len = rng.range(40, 200);
                fill(&mut out, len, room, |i| {
                    if (i / flip).is_multiple_of(2) {
                        pad::LEFT
                    } else {
                        pad::RIGHT | pad::B
                    }
                });
            }
            // Hold A (a long jump / confirm) with a direction.
            _ => {
                let d = dirs[rng.range(0, 3) as usize];
                let len = rng.range(20, 120);
                fill(&mut out, len, room, |_| pad::A | d);
            }
        }
        if out.len() == before {
            // A pause-tap pass can leave nothing only when `room` was
            // exhausted by an earlier push, which ends the loop; make the
            // progress rule hold on this path too.
            out.push(0);
        }
    }
    out.truncate(n);
    out
}

fn fill(out: &mut Vec<u16>, len: u32, room: usize, mut f: impl FnMut(u32) -> u16) {
    let len = (len as usize).clamp(1, room);
    out.extend((0..len as u32).map(&mut f));
}

fn push_run(out: &mut Vec<u16>, cap: usize, len: u32, word: u16) {
    let room = cap.saturating_sub(out.len());
    out.extend(std::iter::repeat_n(word, (len as usize).min(room)));
}

// --------------------------------------------------------------- driver

/// Why an episode ended early.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Halt {
    /// A `JAM`/`KIL` opcode ran.
    Jam,
    /// A frame never reached its boundary inside the core's cycle budget.
    NoFrame,
    /// Work RAM, video memory and registers were identical for
    /// [`FROZEN_FRAMES`] frames while the pads kept changing.
    Frozen,
    /// The core panicked (caught; a bug in the core to report).
    Panic,
    /// The core refused to save a savestate at a frame boundary.
    Capture,
}

/// Identical-state frames before an episode is called frozen.
pub const FROZEN_FRAMES: u32 = 600;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HaltCounts {
    pub jam: u32,
    pub no_frame: u32,
    pub frozen: u32,
    pub panic: u32,
    pub capture: u32,
    /// Episodes that executed code from RAM that no earlier run had (an
    /// informational count: some games run a routine from RAM on purpose).
    pub new_ram_exec_episodes: u32,
}

impl HaltCounts {
    fn note(&mut self, h: Halt) {
        match h {
            Halt::Jam => self.jam += 1,
            Halt::NoFrame => self.no_frame += 1,
            Halt::Frozen => self.frozen += 1,
            Halt::Panic => self.panic += 1,
            Halt::Capture => self.capture += 1,
        }
    }
}

/// One corpus member: a savestate to start episodes from.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub state: Savestate,
    /// `(corpus index, episode)` it was cut from; `None` for a caller seed.
    pub parent: Option<(usize, u32)>,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Master seed: same seed, seeds and budget give the same run (when no
    /// wall-clock stop fires).
    pub seed: u64,
    pub max_episodes: u32,
    pub max_frames: u64,
    pub min_episode_frames: u32,
    pub max_episode_frames: u32,
    /// Frames between novelty checks (and savestates).
    pub segment: u32,
    /// Derived (novelty) corpus entries kept at most.
    pub max_derived: usize,
    /// Percent of episodes started from a derived entry once any exists.
    pub derived_percent: u32,
    pub progress_every: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            seed: 0x5EED_0037_0005,
            max_episodes: 1500,
            max_frames: 1_500_000,
            min_episode_frames: 300,
            max_episode_frames: 1200,
            segment: 20,
            max_derived: 256,
            derived_percent: 50,
            progress_every: 100,
        }
    }
}

/// Caller hooks. `label` runs after every frame and returns a bit mask of
/// the game states then true (game facts, supplied by a test); `stop` is
/// polled between episodes (a wall-clock budget lives there, not in this
/// crate); `progress` gets one line now and then.
pub struct Hooks<'a> {
    pub label: &'a mut dyn FnMut(&NesCore) -> u64,
    pub stop: &'a mut dyn FnMut() -> bool,
    pub progress: &'a mut dyn FnMut(&str),
}

/// A segment (or episode) that ran PRG bytes nobody had run.
#[derive(Debug, Clone)]
pub struct Find {
    pub episode: u32,
    /// Corpus index the episode started from.
    pub entry: usize,
    pub ep_seed: u64,
    /// Frames the generator was asked for / frames actually run.
    pub planned: u32,
    pub frames_run: u32,
    /// PRG bytes first executed by this episode.
    pub new_bytes: usize,
    /// Up to [`SAMPLE`] of their offsets, ascending, for replay checks.
    pub sample: Vec<u32>,
    /// Corpus indices cut from this episode.
    pub derived: Vec<usize>,
}

/// Cap on offsets kept per find.
pub const SAMPLE: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct HaltRecord {
    pub episode: u32,
    pub entry: usize,
    pub ep_seed: u64,
    pub planned: u32,
    pub frames_run: u32,
    pub kind: Halt,
}

/// Per label bit: segments that ran new code while the bit was set, and the
/// new bytes in them (a segment counts for every bit it saw, so bits overlap).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LabelGain {
    pub segments: u32,
    pub bytes: usize,
}

pub struct Report {
    pub episodes: u32,
    pub frames: u64,
    pub seeds: usize,
    pub corpus: Vec<Entry>,
    pub finds: Vec<Find>,
    pub halts: HaltCounts,
    /// Up to 64 halting episodes, for replay.
    pub halt_records: Vec<HaltRecord>,
    pub label_gains: BTreeMap<u32, LabelGain>,
    /// Baseline plus everything the fuzz ran.
    pub coverage: CoverageLog,
    pub baseline_prg_bytes: usize,
    pub new_prg_bytes: usize,
    pub stopped_by: &'static str,
}

struct Null;

impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h = (*h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
}

/// Fingerprint of what a frozen machine would not change.
fn print(core: &NesCore) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    fnv(&mut h, core.bus().ram());
    fnv(&mut h, core.bus().vram());
    let c = core.cpu();
    fnv(&mut h, &[c.a, c.x, c.y, c.s]);
    fnv(&mut h, &c.pc.to_le_bytes());
    h
}

struct Segment {
    frames: u32,
    halt: Option<Halt>,
    labels: u64,
}

/// Run `pads` (a slice, so the loop ends) on `core`. `frozen` carries the
/// frozen-detector across segments.
fn run_pads(
    core: &mut NesCore,
    pads: &[u16],
    label: &mut dyn FnMut(&NesCore) -> u64,
    frozen: &mut (u64, u32),
) -> Segment {
    let mut seg = Segment {
        frames: 0,
        halt: None,
        labels: 0,
    };
    for &p in pads {
        core.bus_mut().set_controller_buttons(0, p as u8);
        core.bus_mut().set_controller_buttons(1, (p >> 8) as u8);
        let r = core.step(Step::Frame, &mut Null);
        seg.frames += 1;
        if !r.frame_complete {
            seg.halt = Some(Halt::NoFrame);
            break;
        }
        if core.cpu().jammed {
            seg.halt = Some(Halt::Jam);
            break;
        }
        seg.labels |= label(core);
        let fp = print(core);
        if fp == frozen.0 {
            frozen.1 += 1;
            if frozen.1 >= FROZEN_FRAMES {
                seg.halt = Some(Halt::Frozen);
                break;
            }
        } else {
            *frozen = (fp, 0);
        }
    }
    seg
}

fn new_core(raw: &[u8]) -> Result<NesCore, String> {
    let mut core = NesCore::from_ines_bytes(raw).map_err(|e| format!("{e:?}"))?;
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    Ok(core)
}

/// Replay an episode from its record: restore `start`, run the first
/// `frames_run` frames of `gen_inputs(ep_seed, planned)`, return the
/// coverage of just that run and why it ended (if early).
///
/// # Errors
/// The image does not load or the state is not its own.
pub fn replay(
    raw: &[u8],
    start: &Savestate,
    ep_seed: u64,
    planned: u32,
    frames_run: u32,
) -> Result<(CoverageLog, Option<Halt>), String> {
    let mut core = new_core(raw)?;
    start.restore(&mut core)?;
    let _ = core.take_coverage();
    let pads = gen_inputs(ep_seed, planned);
    let take = (frames_run as usize).min(pads.len());
    let mut frozen = (0u64, 0u32);
    let seg = run_pads(&mut core, &pads[..take], &mut |_| 0, &mut frozen);
    Ok((core.take_coverage().ok_or("coverage not armed")?, seg.halt))
}

/// PRG bytes `log` ran that `seen` has not: count and the first [`SAMPLE`]
/// offsets.
fn novelty(log: &CoverageLog, seen: &CoverageLog) -> (usize, Vec<u32>) {
    let mut count = 0usize;
    let mut sample = Vec::new();
    for (i, (l, s)) in log.prg_flags.iter().zip(&seen.prg_flags).enumerate() {
        if *l != 0 && *s == 0 {
            count += 1;
            if sample.len() < SAMPLE {
                sample.push(i as u32);
            }
        }
    }
    (count, sample)
}

fn new_ram_exec(log: &CoverageLog, seen: &CoverageLog) -> bool {
    log.ram_exec
        .iter()
        .zip(&seen.ram_exec)
        .any(|(l, s)| *l != 0 && *s == 0)
}

/// Fuzz `raw` from `seeds`. `baseline` is the coverage the caller already
/// has (it must be for this cartridge); the report's `coverage` is that plus
/// everything the fuzz ran.
///
/// # Errors
/// The image or a seed state does not load, `seeds` is empty, or `baseline`
/// is for another cartridge.
pub fn fuzz(
    raw: &[u8],
    seeds: Vec<Entry>,
    baseline: CoverageLog,
    cfg: &Config,
    hooks: &mut Hooks<'_>,
) -> Result<Report, String> {
    if seeds.is_empty() {
        return Err("no seed savestates".to_string());
    }
    let mut core = new_core(raw)?;
    let seed_count = seeds.len();
    let mut corpus = seeds;
    let mut union = baseline;
    if union.prg_flags.len() != core.bus().prg_rom_len() {
        return Err("baseline coverage is for another cartridge".to_string());
    }
    let baseline_prg_bytes = union.executed_prg_bytes();
    let mut rng = Rng::new(cfg.seed);
    let mut report = Report {
        episodes: 0,
        frames: 0,
        seeds: seed_count,
        corpus: Vec::new(),
        finds: Vec::new(),
        halts: HaltCounts::default(),
        halt_records: Vec::new(),
        label_gains: BTreeMap::new(),
        coverage: union.clone(),
        baseline_prg_bytes,
        new_prg_bytes: 0,
        stopped_by: "episodes",
    };
    let segment = cfg.segment.max(1) as usize;
    // Terminates: `ep` counts up to `max_episodes`, and the other exits
    // (`break`) only end the loop.
    for ep in 0..cfg.max_episodes {
        if report.frames >= cfg.max_frames {
            report.stopped_by = "frames";
            break;
        }
        if (hooks.stop)() {
            report.stopped_by = "wall clock";
            break;
        }
        // Visit every caller seed once first, then mix in derived entries.
        let derived = corpus.len() - seed_count;
        let entry = if (ep as usize) < seed_count {
            ep as usize
        } else if derived > 0 && rng.percent() < cfg.derived_percent {
            seed_count + rng.range(0, derived as u32 - 1) as usize
        } else {
            rng.range(0, seed_count as u32 - 1) as usize
        };
        let ep_seed = rng.next_u64();
        let planned = rng.range(
            cfg.min_episode_frames,
            cfg.max_episode_frames.max(cfg.min_episode_frames),
        );
        let pads = gen_inputs(ep_seed, planned);

        corpus[entry].state.restore(&mut core)?;
        let _ = core.take_coverage();
        let mut frozen = (print(&core), 0u32);
        let mut frames_run = 0u32;
        let mut halt = None;
        let mut new_bytes = 0usize;
        let mut sample: Vec<u32> = Vec::new();
        let mut cut: Vec<usize> = Vec::new();
        let mut ram_new = false;

        for chunk in pads.chunks(segment) {
            // The state at the start of the segment, for the corpus.
            let Ok(start) = Savestate::capture(&core) else {
                // A core that cannot save at a frame boundary is a finding.
                halt = Some(Halt::Capture);
                break;
            };
            let run = catch_unwind(AssertUnwindSafe(|| {
                run_pads(&mut core, chunk, &mut *hooks.label, &mut frozen)
            }));
            let seg = match run {
                Ok(s) => s,
                Err(_) => Segment {
                    frames: 0,
                    halt: Some(Halt::Panic),
                    labels: 0,
                },
            };
            let at = frames_run;
            frames_run += seg.frames;
            report.frames += u64::from(seg.frames);
            if seg.halt == Some(Halt::Panic) {
                halt = seg.halt;
                break;
            }
            if let Some(log) = core.take_coverage() {
                let (n, s) = novelty(&log, &union);
                ram_new |= new_ram_exec(&log, &union);
                if n > 0 {
                    new_bytes += n;
                    let room = SAMPLE.saturating_sub(sample.len());
                    sample.extend(s.into_iter().take(room));
                    for bit in 0..64u32 {
                        if seg.labels >> bit & 1 == 1 {
                            let g = report.label_gains.entry(bit).or_default();
                            g.segments += 1;
                            g.bytes += n;
                        }
                    }
                    if corpus.len() - seed_count < cfg.max_derived {
                        cut.push(corpus.len());
                        corpus.push(Entry {
                            name: format!("ep{ep}@{at}"),
                            state: start,
                            parent: Some((entry, ep)),
                        });
                    }
                }
                union.merge(&log);
            }
            if seg.halt.is_some() {
                halt = seg.halt;
                break;
            }
        }

        report.episodes += 1;
        if ram_new {
            report.halts.new_ram_exec_episodes += 1;
        }
        if let Some(h) = halt {
            report.halts.note(h);
            if report.halt_records.len() < 64 {
                report.halt_records.push(HaltRecord {
                    episode: ep,
                    entry,
                    ep_seed,
                    planned,
                    frames_run,
                    kind: h,
                });
            }
        }
        if new_bytes > 0 {
            report.new_prg_bytes += new_bytes;
            report.finds.push(Find {
                episode: ep,
                entry,
                ep_seed,
                planned,
                frames_run,
                new_bytes,
                sample,
                derived: cut,
            });
        }
        if cfg.progress_every > 0 && report.episodes.is_multiple_of(cfg.progress_every) {
            (hooks.progress)(&format!(
                "fuzz: {} episodes, {} frames, +{} PRG bytes ({} finds), corpus {}, halts jam {} / no-frame {} / frozen {} / panic {}",
                report.episodes,
                report.frames,
                report.new_prg_bytes,
                report.finds.len(),
                corpus.len(),
                report.halts.jam,
                report.halts.no_frame,
                report.halts.frozen,
                report.halts.panic,
            ));
        }
    }
    report.corpus = corpus;
    report.coverage = union;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NROM-128 whose program reads pad 1's A button forever: A up loops
    /// through a NOP path, A down runs into a `KIL` (`$02`).
    fn jam_on_a_rom() -> Vec<u8> {
        let mut d = vec![0u8; 16];
        d[0..4].copy_from_slice(b"NES\x1a");
        d[4] = 1;
        d[5] = 0; // CHR RAM
        let mut prg = vec![0xEAu8; 0x4000];
        let code: &[u8] = &[
            0xA9, 0x01, // LDA #1
            0x8D, 0x16, 0x40, // STA $4016
            0xA9, 0x00, // LDA #0
            0x8D, 0x16, 0x40, // STA $4016
            0xAD, 0x16, 0x40, // LDA $4016
            0x29, 0x01, // AND #1
            0xF0, 0x03, // BEQ +3 (A up)
            0x02, // KIL (A down)
            0xEA, 0xEA, // padding
            0x4C, 0x00, 0x80, // JMP $8000
        ];
        prg[..code.len()].copy_from_slice(code);
        prg[0x3FFC] = 0x00;
        prg[0x3FFD] = 0x80;
        d.extend(prg);
        d
    }

    fn quiet<'a>(
        label: &'a mut dyn FnMut(&NesCore) -> u64,
        stop: &'a mut dyn FnMut() -> bool,
        progress: &'a mut dyn FnMut(&str),
    ) -> Hooks<'a> {
        Hooks {
            label,
            stop,
            progress,
        }
    }

    #[test]
    fn inputs_are_a_pure_function_of_the_seed_and_exactly_as_long_as_asked() {
        for seed in 0..40u64 {
            for n in [0u32, 1, 7, 300, 1200] {
                let a = gen_inputs(seed, n);
                assert_eq!(a.len(), n as usize);
                assert_eq!(a, gen_inputs(seed, n));
            }
        }
        assert_ne!(gen_inputs(1, 600), gen_inputs(2, 600));
        let all: u16 = gen_inputs(9, 5000).iter().fold(0, |m, w| m | w);
        assert_eq!(all & 0xFF, 0xFF, "every button appears");
    }

    #[test]
    fn savestate_restore_resumes_the_same_machine() {
        let rom = jam_on_a_rom();
        let mut a = new_core(&rom).unwrap();
        let mut frozen = (0, 0);
        run_pads(&mut a, &[0; 5], &mut |_| 0, &mut frozen);
        let s = Savestate::capture(&a).unwrap();
        run_pads(&mut a, &[0; 5], &mut |_| 0, &mut frozen);
        let after = print(&a);
        let mut b = new_core(&rom).unwrap();
        s.restore(&mut b).unwrap();
        run_pads(&mut b, &[0; 5], &mut |_| 0, &mut frozen);
        assert_eq!(print(&b), after);
        assert_eq!(
            Savestate::capture(&b).unwrap(),
            Savestate::capture(&a).unwrap()
        );
    }

    fn run_jam(seed: u64) -> Report {
        let rom = jam_on_a_rom();
        let mut core = new_core(&rom).unwrap();
        let mut f = (0, 0);
        run_pads(&mut core, &[0; 3], &mut |_| 0, &mut f);
        let base = {
            let _ = core.take_coverage();
            run_pads(&mut core, &[0; 3], &mut |_| 0, &mut f);
            core.take_coverage().unwrap()
        };
        let seed_entry = Entry {
            name: "boot".to_string(),
            state: Savestate::capture(&core).unwrap(),
            parent: None,
        };
        let cfg = Config {
            seed,
            max_episodes: 24,
            min_episode_frames: 30,
            max_episode_frames: 60,
            segment: 10,
            progress_every: 0,
            ..Config::default()
        };
        let (mut l, mut s, mut p) = (|_: &NesCore| 0u64, || false, |_: &str| {});
        fuzz(
            &rom,
            vec![seed_entry],
            base,
            &cfg,
            &mut quiet(&mut l, &mut s, &mut p),
        )
        .unwrap()
    }

    #[test]
    fn finds_the_jam_counts_it_keeps_going_and_replays() {
        let r = run_jam(11);
        assert!(
            r.halts.jam > 0,
            "A pressed runs into the KIL: {:?}",
            r.halts
        );
        assert!(r.new_prg_bytes > 0, "the A-down path is new code");
        assert!(!r.finds.is_empty() && r.episodes == 24);
        assert!(r.corpus.len() > r.seeds, "novelty cut a derived entry");
        let f = &r.finds[0];
        let (log, _) = replay(
            &jam_on_a_rom(),
            &r.corpus[f.entry].state,
            f.ep_seed,
            f.planned,
            f.frames_run,
        )
        .unwrap();
        for &o in &f.sample {
            assert_ne!(log.prg_flags[o as usize], 0, "offset {o} replays");
        }
        let h = r.halt_records[0];
        let (_, halt) = replay(
            &jam_on_a_rom(),
            &r.corpus[h.entry].state,
            h.ep_seed,
            h.planned,
            h.frames_run,
        )
        .unwrap();
        assert_eq!(halt, Some(h.kind));
    }

    #[test]
    fn same_seed_same_run_different_seed_different_run() {
        let (a, b, c) = (run_jam(5), run_jam(5), run_jam(6));
        let key = |r: &Report| {
            (
                r.frames,
                r.new_prg_bytes,
                r.halts,
                r.finds.iter().map(|f| f.ep_seed).collect::<Vec<_>>(),
            )
        };
        assert_eq!(key(&a), key(&b));
        assert_ne!(key(&a).3, key(&c).3);
    }

    #[test]
    fn a_panic_in_a_hook_is_caught_counted_recorded_and_the_run_goes_on() {
        let rom = jam_on_a_rom();
        let core = new_core(&rom).unwrap();
        let seed = Entry {
            name: "s".into(),
            state: Savestate::capture(&core).unwrap(),
            parent: None,
        };
        let base = CoverageLog::new(core.bus().prg_rom_len());
        let cfg = Config {
            max_episodes: 4,
            min_episode_frames: 20,
            max_episode_frames: 30,
            progress_every: 0,
            ..Config::default()
        };
        let mut calls = 0u32;
        let mut l = |_: &NesCore| -> u64 {
            calls += 1;
            assert!(!calls.is_multiple_of(25), "injected");
            0
        };
        let (mut s, mut p) = (|| false, |_: &str| {});
        let r = fuzz(
            &rom,
            vec![seed],
            base,
            &cfg,
            &mut quiet(&mut l, &mut s, &mut p),
        )
        .unwrap();
        assert_eq!(r.episodes, 4, "the run continued after the panic");
        assert!(r.halts.panic >= 1);
        assert!(r.halt_records.iter().any(|h| h.kind == Halt::Panic));
    }

    #[test]
    fn budgets_stop_the_run() {
        let rom = jam_on_a_rom();
        let core = new_core(&rom).unwrap();
        let seed = Entry {
            name: "s".into(),
            state: Savestate::capture(&core).unwrap(),
            parent: None,
        };
        let base = CoverageLog::new(core.bus().prg_rom_len());
        let cfg = Config {
            progress_every: 0,
            min_episode_frames: 20,
            max_episode_frames: 30,
            ..Config::default()
        };
        let mut n = 0;
        let (mut l, mut p) = (|_: &NesCore| 0u64, |_: &str| {});
        let mut s = || {
            n += 1;
            n > 3
        };
        let r = fuzz(
            &rom,
            vec![seed],
            base,
            &cfg,
            &mut quiet(&mut l, &mut s, &mut p),
        )
        .unwrap();
        assert_eq!((r.episodes, r.stopped_by), (3, "wall clock"));
        assert!(fuzz(
            &rom,
            vec![],
            CoverageLog::new(1),
            &cfg,
            &mut quiet(&mut l, &mut || false, &mut p)
        )
        .is_err());
    }
}
