//! Collision-table finder (ticket W38-03): the collision routine and which
//! tile ids it treats as solid, one-way or water. Offsets only (law 5, D-021).
//!
//! Console-agnostic like [`crate::dataregion`]: a [`StepObserver`]
//! ([`CollisionReader`]) collects facts while the game runs, then pure
//! functions turn them into [`TileSemantics`] rows.
//!
//! ## What is observed
//!
//! * Every RAM *load* (an `LDA`/`LDX`/`LDY` or a compare against memory) is a
//!   candidate "tile read": the instruction's ROM offset (its *site*), the RAM
//!   address it read and the value. A level buffer is a RAM region a site walks
//!   across; a site that only ever reads one address is a variable, not a map.
//! * Whether the loaded value is *consumed as a tile id*: a following
//!   `CMP`/`CPX`/`CPY #imm` ([`Consumer::Compare`]) or a ROM table read indexed
//!   by it ([`Consumer::Lookup`]). Consumption narrows the candidates; it is
//!   not what decides a class.
//! * Once per frame, the caller attaches what the *player did* that frame
//!   ([`FrameRecord::y`], `x`, `pushing`): two RAM values it was told to watch,
//!   passed as experiment parameters (the recipe's `player_y`, or an address
//!   the test names), never read from engine code.
//!
//! ## How a class is decided: by outcome
//!
//! The game's own collision code is not read or decompiled. Each frame is
//! labelled from the player's motion alone ([`Motion`]): moving down, moving
//! up, **blocked** (stopped after having been falling, or pushing sideways
//! without advancing), or unknown. A site's tile id `v` is then
//!
//! * [`TileClass::Solid`] when it is read mostly on blocked frames and not on
//!   free-moving ones (the floor under a resting player, a wall he pushes on);
//! * [`TileClass::OneWay`] when it blocks a fall but the same id is read while
//!   rising through it (stand on it from above, pass up through it);
//! * [`TileClass::Water`] when it is read through a run of slow vertical
//!   motion far below the game's free-fall speed (swimming).
//!
//! Only ids that clear the evidence thresholds ([`MIN_EVIDENCE`],
//! [`MIN_PRECISION_PCT`]) become rows; everything else stays out.
//!
//! ## The comparison
//!
//! The hand-painted height map (`[heights.tiles]`, W30-06) is keyed by the
//! tilemap's *character number*, while a collision routine branches on the
//! *level-buffer block id*; those are different id spaces, so the two are
//! compared as per-screen **grids** ([`Grid`], [`compare`]), not as id lists.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rf_core_api::{BusAccess, StepFacts, StepObserver};

use crate::dataregion::Resolver;
use crate::hw::{classify, Hit, Platform};
use crate::recipe::{Grade, TileClass, TileSemantics};

/// Frames with fewer reads than this at a site do not make it a candidate.
const MIN_ACTIVE_PCT: usize = 50;
/// Blocked (or rising) frames a tile id needs before it earns a class.
pub const MIN_EVIDENCE: u32 = 3;
/// Share of an id's reads (percent) that must fall in the deciding motion.
pub const MIN_PRECISION_PCT: u32 = 85;
/// Rising frames right after leaving the ground that are not counted as
/// "rising through" (the feet probe is still over the floor).
const RISE_SKIP: usize = 3;
/// A frame-to-frame jump of the player's y bigger than this is a teleport
/// (a level load, a death), not motion.
const TELEPORT: i32 = 64;
/// A swim run is at least this many frames of slow vertical motion.
const SWIM_RUN: usize = 8;
/// Recent loads kept for value matching, and how many steps old a match may be.
const RING: usize = 8;
const WINDOW: u64 = 6;
/// Reads kept per site per frame, and sites kept per frame.
const READS_PER_SITE: usize = 64;
const SITES_PER_FRAME: usize = 256;

/// How a loaded value is used right after the load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Consumer {
    /// Compared against an immediate.
    Compare,
    /// Used as the index of a ROM table read.
    Lookup,
}

/// One tile read in a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRead {
    /// ROM offset of the loading instruction.
    pub site: u32,
    /// RAM address read (the lowest byte, for a 16-bit read).
    pub ram: u32,
    pub value: u32,
}

/// Everything seen in one observed frame plus what the player did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameRecord {
    pub reads: Vec<TileRead>,
    /// The watched vertical position after the frame (down is larger).
    pub y: i32,
    /// The watched horizontal position after the frame.
    pub x: i32,
    /// The pad was pushing the player sideways this frame.
    pub pushing: bool,
    /// The sample is valid (the caller attached one).
    pub sampled: bool,
}

/// What the finder knows about one loading instruction over the whole run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SiteFacts {
    /// Atlas function the instruction belongs to, when known.
    pub function: Option<u32>,
    /// Distinct RAM addresses read, with a read count each.
    pub addrs: BTreeMap<u32, u32>,
    pub consumers: BTreeSet<Consumer>,
}

/// All facts of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollisionFacts {
    pub platform: Option<Platform>,
    pub sites: BTreeMap<u32, SiteFacts>,
    pub frames: Vec<FrameRecord>,
}

impl CollisionFacts {
    /// Append another run's frames and site facts.
    pub fn merge(&mut self, other: &Self) {
        for (site, s) in &other.sites {
            let e = self.sites.entry(*site).or_default();
            e.function = e.function.or(s.function);
            for (a, n) in &s.addrs {
                *e.addrs.entry(*a).or_default() += n;
            }
            e.consumers.extend(s.consumers.iter().copied());
        }
        self.frames.extend(other.frames.iter().cloned());
    }
}

#[derive(Debug, Clone, Copy)]
struct Load {
    step: u64,
    site: u32,
    value: u32,
}

/// Opcodes that load a register from memory (6502 and 65816).
const LOADS: &[u8] = &[
    0xA5, 0xB5, 0xAD, 0xBD, 0xB9, 0xA1, 0xB1, 0xB2, 0xA7, 0xB7, 0xAF, 0xBF, 0xA3, 0xB3, // LDA
    0xA6, 0xB6, 0xAE, 0xBE, // LDX
    0xA4, 0xB4, 0xAC, 0xBC, // LDY
    0xC5, 0xD5, 0xCD, 0xDD, 0xD9, 0xC1, 0xD1, 0xD2, 0xC7, 0xD7, 0xCF, 0xDF, 0xC3, 0xD3, // CMP
    0xE4, 0xEC, 0xC4, 0xCC, // CPX, CPY
];
/// Compare-immediate opcodes.
const CMP_IMM: &[u8] = &[0xC9, 0xE0, 0xC0];

/// The observer: hand it to `run_frame_observed` with the resolver set for the
/// frame, then call [`Self::attach_player`] once the frame returns.
pub struct CollisionReader {
    platform: Platform,
    resolver: Resolver,
    step: u64,
    ring: VecDeque<Load>,
    current: BTreeMap<u32, Vec<(u32, u32)>>,
    facts: CollisionFacts,
}

impl CollisionReader {
    #[must_use]
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            resolver: Box::new(|_| None),
            step: 0,
            ring: VecDeque::new(),
            current: BTreeMap::new(),
            facts: CollisionFacts {
                platform: Some(platform),
                ..CollisionFacts::default()
            },
        }
    }

    /// Install the CPU-address to ROM-offset map for the frames that follow.
    pub fn set_resolver(&mut self, resolver: Resolver) {
        self.resolver = resolver;
    }

    /// Attach the watched player values to the frame that just ended.
    pub fn attach_player(&mut self, y: i32, x: i32, pushing: bool) {
        if let Some(f) = self.facts.frames.last_mut() {
            f.y = y;
            f.x = x;
            f.pushing = pushing;
            f.sampled = true;
        }
    }

    /// What has been learned so far; observing continues into a fresh set.
    pub fn take_facts(&mut self) -> CollisionFacts {
        std::mem::replace(
            &mut self.facts,
            CollisionFacts {
                platform: Some(self.platform),
                ..CollisionFacts::default()
            },
        )
    }

    fn ram(&self, addr: u32) -> Option<u32> {
        match classify(self.platform, addr, false) {
            Hit::Ram { addr } => Some(addr),
            _ => None,
        }
    }

    /// A ROM table read indexed by a register: `(index low byte)` for the
    /// absolute-indexed shape (`op lo hi` with the read at `base + index`).
    fn lookup_index(&self, acc: &[BusAccess]) -> Option<u8> {
        let first = acc.first()?;
        let pc = first.addr;
        let prefix = acc
            .iter()
            .enumerate()
            .take_while(|(i, a)| !a.write && a.addr == pc.wrapping_add(*i as u32))
            .count();
        if prefix != 3 {
            return None;
        }
        let operand = u32::from(acc[1].value) | (u32::from(acc[2].value) << 8);
        let data = acc[3..]
            .iter()
            .rev()
            .find(|a| !a.write && (self.resolver)(a.addr).is_some())?;
        let index = (data.addr & 0xFFFF).wrapping_sub(operand) & 0xFFFF;
        (index < 0x100).then_some(index as u8)
    }
}

impl StepObserver for CollisionReader {
    fn step(&mut self, facts: StepFacts, acc: &[BusAccess]) {
        self.step += 1;
        if facts.interrupt || acc.is_empty() {
            return;
        }
        let Some(site) = (self.resolver)(acc[0].addr) else {
            return;
        };
        let opcode = acc[0].value;
        let step = self.step;
        // A value loaded a moment ago is being used as an index or compared.
        let consumed = if CMP_IMM.contains(&opcode) && acc.len() >= 2 {
            Some(Consumer::Compare)
        } else {
            self.lookup_index(acc).map(|_| Consumer::Lookup)
        };
        if let Some(kind) = consumed {
            let index = self.lookup_index(acc);
            let hits: Vec<u32> = self
                .ring
                .iter()
                .filter(|l| {
                    step - l.step <= WINDOW
                        && match kind {
                            Consumer::Compare => true,
                            Consumer::Lookup => index.is_some_and(|i| (l.value & 0xFF) as u8 == i),
                        }
                })
                .map(|l| l.site)
                .collect();
            for s in hits {
                if let Some(f) = self.facts.sites.get_mut(&s) {
                    f.consumers.insert(kind);
                }
            }
        }
        if !LOADS.contains(&opcode) {
            return;
        }
        // The data read is the last RAM read; a 16-bit load reads two bytes.
        let reads: Vec<(usize, u32)> = acc
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, a)| !a.write)
            .filter_map(|(i, a)| self.ram(a.addr).map(|r| (i, r)))
            .collect();
        let Some(&(li, ladr)) = reads.last() else {
            return;
        };
        let (addr, value) = match reads.len().checked_sub(2).map(|k| reads[k]) {
            Some((pi, padr)) if padr + 1 == ladr && self.platform == Platform::Snes && pi < li => (
                padr,
                u32::from(acc[pi].value) | (u32::from(acc[li].value) << 8),
            ),
            _ => (ladr, u32::from(acc[li].value)),
        };
        let function = facts.function;
        let s = self.facts.sites.entry(site).or_default();
        s.function = s.function.or(function);
        *s.addrs.entry(addr).or_default() += 1;
        if self.ring.len() == RING {
            self.ring.pop_front();
        }
        self.ring.push_back(Load { step, site, value });
        if self.current.len() < SITES_PER_FRAME || self.current.contains_key(&site) {
            let v = self.current.entry(site).or_default();
            if v.len() < READS_PER_SITE {
                v.push((addr, value));
            }
        }
    }

    fn frame_end(&mut self) {
        let reads = std::mem::take(&mut self.current)
            .into_iter()
            .flat_map(|(site, v)| {
                v.into_iter()
                    .map(move |(ram, value)| TileRead { site, ram, value })
            })
            .collect();
        self.facts.frames.push(FrameRecord {
            reads,
            ..FrameRecord::default()
        });
    }
}

/// What the player's motion says about a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// y grew (falling).
    Falling,
    /// y shrank; `since` frames since it left the ground.
    Rising { since: usize },
    /// Stopped after falling (resting on something).
    BlockedDown,
    /// Pushing sideways and not advancing.
    BlockedSide,
    /// Pushing sideways and advancing.
    FreeSide,
    /// Apex, start of the run, a teleport: no evidence either way.
    Unknown,
}

/// Label every frame from the watched values. Walks the frames once, each
/// iteration advancing by exactly one (law 8).
#[must_use]
pub fn label_frames(frames: &[FrameRecord]) -> Vec<Motion> {
    let mut out = Vec::with_capacity(frames.len());
    let mut last_dir = 0i32;
    let mut rising = 0usize;
    for (i, f) in frames.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| &frames[p]);
        let motion = match prev {
            Some(p) if f.sampled && p.sampled => {
                let dy = f.y - p.y;
                let dx = f.x - p.x;
                if dy.abs() > TELEPORT || dx.abs() > TELEPORT {
                    last_dir = 0;
                    rising = 0;
                    Motion::Unknown
                } else if dy > 0 {
                    last_dir = 1;
                    rising = 0;
                    Motion::Falling
                } else if dy < 0 {
                    last_dir = -1;
                    rising += 1;
                    Motion::Rising { since: rising }
                } else if last_dir > 0 {
                    rising = 0;
                    Motion::BlockedDown
                } else if f.pushing && dx == 0 && last_dir == 0 {
                    Motion::BlockedSide
                } else {
                    rising = 0;
                    Motion::Unknown
                }
            }
            _ => Motion::Unknown,
        };
        // Sideways evidence stands on its own: a vertical state does not hide
        // a wall the player is pushing on.
        let motion = match (motion, prev) {
            (Motion::Unknown | Motion::BlockedDown, Some(p)) if f.sampled && p.sampled => {
                let dx = f.x - p.x;
                if f.pushing && dx == 0 && f.y == p.y {
                    Motion::BlockedSide
                } else if f.pushing && dx != 0 && dx.abs() <= TELEPORT && motion == Motion::Unknown
                {
                    Motion::FreeSide
                } else {
                    motion
                }
            }
            _ => motion,
        };
        out.push(motion);
    }
    out
}

/// Per tile id counts at one site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub blocked_down: u32,
    pub blocked_side: u32,
    pub falling: u32,
    pub rising: u32,
    pub free_side: u32,
    pub slow: u32,
    pub normal: u32,
}

impl Tally {
    fn blocked(&self) -> u32 {
        self.blocked_down + self.blocked_side
    }
    fn free(&self) -> u32 {
        self.falling + self.free_side
    }
    fn pct(part: u32, whole: u32) -> u32 {
        if whole == 0 {
            0
        } else {
            part * 100 / whole
        }
    }
}

/// One tile id's verdict at one site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub id: u32,
    pub class: TileClass,
    pub tally: Tally,
}

/// One collision-routine candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionSite {
    pub site: u32,
    pub function: Option<u32>,
    pub consumers: BTreeSet<Consumer>,
    /// Distinct RAM addresses it read (the level buffer span).
    pub addrs: BTreeSet<u32>,
    /// Frames in which it read anything.
    pub active_frames: usize,
    pub verdicts: Vec<Verdict>,
}

impl CollisionSite {
    /// Ids classified as `class`.
    #[must_use]
    pub fn ids(&self, class: TileClass) -> Vec<u32> {
        self.verdicts
            .iter()
            .filter(|v| v.class == class)
            .map(|v| v.id)
            .collect()
    }
}

/// Frames that are slow vertical runs of at least [`SWIM_RUN`] frames.
fn slow_frames(frames: &[FrameRecord], motion: &[Motion]) -> Vec<bool> {
    let mut fast = 0i32;
    let mut dys: Vec<i32> = Vec::new();
    for (i, f) in frames.iter().enumerate().skip(1) {
        if f.sampled && frames[i - 1].sampled && matches!(motion[i], Motion::Falling) {
            dys.push(f.y - frames[i - 1].y);
        }
    }
    dys.sort_unstable();
    if let Some(&top) = dys.get(dys.len().saturating_mul(9) / 10) {
        fast = top;
    }
    let limit = (fast / 3).max(1);
    let is_slow = |i: usize| -> bool {
        let Some(p) = i.checked_sub(1) else {
            return false;
        };
        let (a, b) = (&frames[p], &frames[i]);
        a.sampled && b.sampled && (b.y - a.y).abs() <= limit && b.y != a.y
    };
    let mut out = vec![false; frames.len()];
    // Runs of consecutive slow frames; `i` advances every iteration.
    let mut i = 0usize;
    while i < frames.len() {
        if !is_slow(i) {
            i += 1;
            continue;
        }
        let start = i;
        while i < frames.len() && is_slow(i) {
            i += 1;
        }
        if i - start >= SWIM_RUN && fast > 3 * limit.min(fast) / 3 {
            for slot in &mut out[start..i] {
                *slot = true;
            }
        }
    }
    out
}

/// Decide which sites are collision routines and what each tile id is.
#[must_use]
pub fn infer(facts: &CollisionFacts) -> Vec<CollisionSite> {
    let motion = label_frames(&facts.frames);
    let slow = slow_frames(&facts.frames, &motion);
    // site -> id -> tally, and the frames each site was active in.
    let mut tallies: BTreeMap<u32, BTreeMap<u32, Tally>> = BTreeMap::new();
    let mut active: BTreeMap<u32, usize> = BTreeMap::new();
    for (i, f) in facts.frames.iter().enumerate() {
        let mut seen: BTreeSet<u32> = BTreeSet::new();
        for r in &f.reads {
            seen.insert(r.site);
            let t = tallies
                .entry(r.site)
                .or_default()
                .entry(r.value)
                .or_default();
            let m = motion[i];
            if slow[i] {
                t.slow += 1;
            } else {
                t.normal += 1;
            }
            match m {
                Motion::BlockedDown => t.blocked_down += 1,
                Motion::BlockedSide => t.blocked_side += 1,
                Motion::Falling => t.falling += 1,
                Motion::Rising { since } if since > RISE_SKIP => t.rising += 1,
                Motion::FreeSide => t.free_side += 1,
                _ => {}
            }
        }
        for s in seen {
            *active.entry(s).or_default() += 1;
        }
    }
    let sampled = facts.frames.iter().filter(|f| f.sampled).count().max(1);
    let mut out = Vec::new();
    for (site, by_id) in &tallies {
        let Some(sf) = facts.sites.get(site) else {
            continue;
        };
        let act = active.get(site).copied().unwrap_or(0);
        // A map walker reads many addresses and reads on most frames.
        if sf.addrs.len() < 2 || act * 100 < sampled * MIN_ACTIVE_PCT {
            continue;
        }
        let verdicts: Vec<Verdict> = by_id
            .iter()
            .filter_map(|(&id, t)| {
                decide(t).map(|class| Verdict {
                    id,
                    class,
                    tally: *t,
                })
            })
            .collect();
        if verdicts.is_empty() {
            continue;
        }
        out.push(CollisionSite {
            site: *site,
            function: sf.function,
            consumers: sf.consumers.clone(),
            addrs: sf.addrs.keys().copied().collect(),
            active_frames: act,
            verdicts,
        });
    }
    // Strongest evidence first: the routine that decides most ids.
    out.sort_by_key(|s| {
        (
            std::cmp::Reverse(s.verdicts.len()),
            std::cmp::Reverse(s.consumers.len()),
            s.site,
        )
    });
    out
}

/// One id's class from its tally, or `None` when the evidence is short.
#[must_use]
pub fn decide(t: &Tally) -> Option<TileClass> {
    // Swimming: read through slow vertical runs and rarely through normal play.
    if t.slow >= MIN_EVIDENCE && Tally::pct(t.slow, t.slow + t.normal) >= MIN_PRECISION_PCT {
        return Some(TileClass::Water);
    }
    let blocked = t.blocked();
    if blocked < MIN_EVIDENCE {
        return None;
    }
    // Rarely read while moving freely: solid.
    let free = t.free();
    let against = free + t.rising;
    if Tally::pct(blocked, blocked + against) >= MIN_PRECISION_PCT {
        return Some(TileClass::Solid);
    }
    // Blocks a fall but is read while rising through it: one-way.
    if t.blocked_down >= MIN_EVIDENCE
        && t.rising >= MIN_EVIDENCE
        && Tally::pct(t.blocked_down, t.blocked_down + t.falling) >= MIN_PRECISION_PCT
    {
        return Some(TileClass::OneWay);
    }
    None
}

/// Merge consecutive ids of one class into recipe rows. `ids_are` says which
/// id space the ids are in (it goes in the row's `source`).
#[must_use]
pub fn to_rows(
    site: &CollisionSite,
    layer: Option<u8>,
    tileset: Option<&str>,
    ids_are: &str,
    experiment: &str,
    grade: Grade,
) -> Vec<TileSemantics> {
    let mut rows: Vec<TileSemantics> = Vec::new();
    for class in [
        TileClass::Solid,
        TileClass::OneWay,
        TileClass::Water,
        TileClass::Hazard,
    ] {
        let ids = site.ids(class);
        let mut it = ids.into_iter();
        let Some(mut first) = it.next() else {
            continue;
        };
        let mut last = first;
        let mut push = |first: u32, last: u32| {
            rows.push(TileSemantics {
                class,
                first,
                last,
                layer,
                tileset: tileset.map(str::to_string),
                source: format!(
                    "collision-table finder W38-03: routine at rom {:#x} reads {ids_are}; {experiment}",
                    site.site
                ),
                grade,
            });
        };
        for id in it {
            if id == last + 1 {
                last = id;
            } else {
                push(first, last);
                first = id;
                last = id;
            }
        }
        push(first, last);
    }
    rows
}

/// The span of RAM a site walked: `(lowest, highest, distinct addresses)`.
#[must_use]
pub fn buffer_span(site: &CollisionSite) -> Option<(u32, u32, usize)> {
    let lo = *site.addrs.first()?;
    let hi = *site.addrs.last()?;
    Some((lo, hi, site.addrs.len()))
}

/// A per-screen grid of "solid or not" (`None`: unknown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    pub w: usize,
    pub h: usize,
    pub cells: Vec<Option<bool>>,
}

impl Grid {
    #[must_use]
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            cells: vec![None; w * h],
        }
    }
}

/// The grid the found rows give for a screen of block ids (`None`: not
/// observed). Solid and one-way count as solid; everything else as open.
#[must_use]
pub fn found_grid(ids: &[Option<u32>], w: usize, rows: &[TileSemantics]) -> Grid {
    let h = ids.len().checked_div(w).unwrap_or(0);
    let mut g = Grid::new(w, h);
    for (cell, id) in g.cells.iter_mut().zip(ids) {
        *cell = id.map(|v| {
            rows.iter().any(|r| {
                matches!(r.class, TileClass::Solid | TileClass::OneWay)
                    && (r.first..=r.last).contains(&v)
            })
        });
    }
    g
}

/// The grid a painted height table gives for a screen of tilemap character
/// numbers: a cell with a height above zero is solid.
#[must_use]
pub fn painted_grid(chars: &[Option<u32>], w: usize, heights: &BTreeMap<u32, u32>) -> Grid {
    let h = chars.len().checked_div(w).unwrap_or(0);
    let mut g = Grid::new(w, h);
    for (cell, c) in g.cells.iter_mut().zip(chars) {
        *cell = c.map(|c| heights.get(&c).copied().unwrap_or(0) > 0);
    }
    g
}

/// Cell-by-cell agreement of two grids.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Agreement {
    pub both_solid: u32,
    pub both_open: u32,
    pub found_only: u32,
    pub painted_only: u32,
    /// Cells where either side had no value.
    pub unknown: u32,
}

impl Agreement {
    #[must_use]
    pub fn compared(&self) -> u32 {
        self.both_solid + self.both_open + self.found_only + self.painted_only
    }

    /// Agreement in percent of the compared cells (`None`: nothing compared).
    #[must_use]
    pub fn pct(&self) -> Option<f64> {
        let n = self.compared();
        (n > 0).then(|| f64::from(self.both_solid + self.both_open) * 100.0 / f64::from(n))
    }

    /// Agreement is at least `min_pct` percent.
    #[must_use]
    pub fn within(&self, min_pct: f64) -> bool {
        self.pct().is_some_and(|p| p >= min_pct)
    }
}

/// Compare a found grid with a painted one (same size; extra cells of the
/// longer one count as unknown).
#[must_use]
pub fn compare(found: &Grid, painted: &Grid) -> Agreement {
    let mut a = Agreement::default();
    let n = found.cells.len().max(painted.cells.len());
    for i in 0..n {
        let f = found.cells.get(i).copied().flatten();
        let p = painted.cells.get(i).copied().flatten();
        match (f, p) {
            (Some(true), Some(true)) => a.both_solid += 1,
            (Some(false), Some(false)) => a.both_open += 1,
            (Some(true), Some(false)) => a.found_only += 1,
            (Some(false), Some(true)) => a.painted_only += 1,
            _ => a.unknown += 1,
        }
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(y: i32, reads: &[(u32, u32, u32)]) -> FrameRecord {
        FrameRecord {
            reads: reads
                .iter()
                .map(|&(site, ram, value)| TileRead { site, ram, value })
                .collect(),
            y,
            x: 0,
            pushing: false,
            sampled: true,
        }
    }

    /// Fall from y=0 to 40, rest on id 7 (read at two addresses so the site
    /// walks a buffer), jump and land again.
    fn drop_and_stand() -> Vec<FrameRecord> {
        let mut v = Vec::new();
        // standing at the start is unknown (no fall seen yet)
        v.push(frame(0, &[(0x100, 0x500, 0), (0x100, 0x501, 0)]));
        for y in [2, 6, 12, 20, 30, 40] {
            v.push(frame(y, &[(0x100, 0x510, 0), (0x100, 0x511, 0)]));
        }
        for _ in 0..8 {
            v.push(frame(40, &[(0x100, 0x520, 7), (0x100, 0x521, 0)]));
        }
        // rise through the air, fall back, stand again
        for y in [34, 30, 28, 28] {
            v.push(frame(y, &[(0x100, 0x530, 0), (0x100, 0x531, 0)]));
        }
        for y in [30, 34, 40] {
            v.push(frame(y, &[(0x100, 0x540, 0), (0x100, 0x541, 0)]));
        }
        for _ in 0..6 {
            v.push(frame(40, &[(0x100, 0x550, 7), (0x100, 0x551, 0)]));
        }
        v
    }

    fn facts_of(frames: Vec<FrameRecord>) -> CollisionFacts {
        let mut f = CollisionFacts {
            platform: Some(Platform::Nes),
            frames,
            ..CollisionFacts::default()
        };
        let site = f.sites.entry(0x100).or_default();
        for a in 0x500..0x560 {
            site.addrs.insert(a, 1);
        }
        site.consumers.insert(Consumer::Compare);
        f
    }

    #[test]
    fn labels_follow_the_motion() {
        let m = label_frames(&drop_and_stand());
        assert_eq!(m[0], Motion::Unknown);
        assert_eq!(m[1], Motion::Falling);
        assert_eq!(m[7], Motion::BlockedDown);
        assert_eq!(m[14], Motion::BlockedDown);
        assert!(matches!(m[15], Motion::Rising { since: 1 }));
        // the apex repeat of y=28 after rising is unknown, not blocked
        assert_eq!(m[18], Motion::Unknown);
    }

    #[test]
    fn solid_id_is_the_one_read_only_while_resting() {
        let sites = infer(&facts_of(drop_and_stand()));
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].ids(TileClass::Solid), vec![7]);
        // id 0 is read on every kind of frame: no class
        assert!(sites[0].verdicts.iter().all(|v| v.id != 0));
    }

    #[test]
    fn a_site_that_reads_one_cell_is_a_variable() {
        let mut f = facts_of(drop_and_stand());
        f.sites
            .get_mut(&0x100)
            .unwrap()
            .addrs
            .retain(|a, _| *a == 0x500);
        assert!(infer(&f).is_empty());
    }

    #[test]
    fn too_little_evidence_gives_no_class() {
        let mut frames = Vec::new();
        for y in [0, 5, 10] {
            frames.push(frame(y, &[(0x100, 0x500, 0), (0x100, 0x501, 0)]));
        }
        for _ in 0..2 {
            frames.push(frame(10, &[(0x100, 0x502, 9), (0x100, 0x503, 0)]));
        }
        for _ in 0..4 {
            frames.push(frame(10, &[(0x100, 0x502, 0), (0x100, 0x503, 0)]));
        }
        let sites = infer(&facts_of(frames));
        assert!(sites.iter().all(|s| !s.ids(TileClass::Solid).contains(&9)));
    }

    #[test]
    fn a_wall_the_player_pushes_on_is_solid() {
        let mut frames = Vec::new();
        for x in [0, 2, 4, 6] {
            let mut f = frame(40, &[(0x100, 0x500, 0), (0x100, 0x501, 0)]);
            f.x = x;
            f.pushing = true;
            frames.push(f);
        }
        for _ in 0..6 {
            let mut f = frame(40, &[(0x100, 0x502, 12), (0x100, 0x501, 0)]);
            f.x = 6;
            f.pushing = true;
            frames.push(f);
        }
        let sites = infer(&facts_of(frames));
        assert_eq!(sites[0].ids(TileClass::Solid), vec![12]);
    }

    #[test]
    fn a_one_way_platform_blocks_a_fall_but_is_read_while_rising() {
        let mut frames = Vec::new();
        // fall onto it (id 5 under the feet), rest, rise through it twice
        for y in [0, 4, 9, 15] {
            frames.push(frame(y, &[(0x100, 0x500, 0), (0x100, 0x501, 0)]));
        }
        for _ in 0..6 {
            frames.push(frame(15, &[(0x100, 0x502, 5), (0x100, 0x501, 0)]));
        }
        // rising: the feet probe is inside the platform for several frames
        for y in [12, 9, 6, 3, 0, -3, -6] {
            frames.push(frame(y, &[(0x100, 0x502, 5), (0x100, 0x501, 0)]));
        }
        let sites = infer(&facts_of(frames));
        assert_eq!(sites[0].ids(TileClass::OneWay), vec![5]);
        assert!(sites[0].ids(TileClass::Solid).is_empty());
    }

    #[test]
    fn water_is_read_through_a_slow_run_far_below_free_fall() {
        let mut frames = Vec::new();
        // a fast fall sets the free-fall speed
        for y in [0, 3, 9, 18, 30, 45] {
            frames.push(frame(y, &[(0x100, 0x500, 0), (0x100, 0x501, 0)]));
        }
        // then ten frames of 1-pixel bobbing under id 9
        let mut y = 45;
        for k in 0..10 {
            y += if k % 2 == 0 { 1 } else { -1 };
            frames.push(frame(y, &[(0x100, 0x502, 9), (0x100, 0x501, 0)]));
        }
        let sites = infer(&facts_of(frames));
        assert_eq!(sites[0].ids(TileClass::Water), vec![9]);
    }

    #[test]
    fn consecutive_ids_merge_into_one_row() {
        let mk = |id, class| Verdict {
            id,
            class,
            tally: Tally::default(),
        };
        let site = CollisionSite {
            site: 0x1234,
            function: None,
            consumers: BTreeSet::new(),
            addrs: BTreeSet::new(),
            active_frames: 1,
            verdicts: vec![
                mk(3, TileClass::Solid),
                mk(4, TileClass::Solid),
                mk(5, TileClass::Solid),
                mk(9, TileClass::Solid),
                mk(20, TileClass::OneWay),
            ],
        };
        let rows = to_rows(
            &site,
            Some(1),
            None,
            "the RAM level buffer",
            "test",
            Grade::Candidate,
        );
        let spans: Vec<_> = rows.iter().map(|r| (r.class, r.first, r.last)).collect();
        assert_eq!(
            spans,
            vec![
                (TileClass::Solid, 3, 5),
                (TileClass::Solid, 9, 9),
                (TileClass::OneWay, 20, 20)
            ]
        );
        assert!(rows[0].source.contains("0x1234"));
        assert!(rows[0].source.contains("level buffer"));
    }

    #[test]
    fn grids_compare_cell_by_cell() {
        let rows = vec![TileSemantics {
            class: TileClass::Solid,
            first: 1,
            last: 2,
            layer: None,
            tileset: None,
            source: String::new(),
            grade: Grade::Candidate,
        }];
        // block ids on a 4x2 screen
        let ids = [
            Some(1),
            Some(0),
            Some(2),
            None,
            Some(0),
            Some(0),
            Some(1),
            Some(3),
        ];
        let found = found_grid(&ids, 4, &rows);
        assert_eq!(found.cells[0], Some(true));
        assert_eq!(found.cells[3], None);
        // painted: character numbers with heights, tile 30 tall, 31 flat
        let heights = BTreeMap::from([(30, 16), (31, 0)]);
        let chars = [
            Some(30),
            Some(31),
            Some(30),
            Some(31),
            Some(31),
            Some(30),
            Some(30),
            Some(31),
        ];
        let painted = painted_grid(&chars, 4, &heights);
        let a = compare(&found, &painted);
        assert_eq!(a.both_solid, 3);
        assert_eq!(a.both_open, 3);
        assert_eq!(a.painted_only, 1);
        assert_eq!(a.found_only, 0);
        assert_eq!(a.unknown, 1);
        assert_eq!(a.compared(), 7);
        let pct = a.pct().unwrap();
        assert!((pct - 6.0 / 7.0 * 100.0).abs() < 1e-9);
        assert!(a.within(80.0));
        assert!(!a.within(90.0));
        assert_eq!(compare(&Grid::new(2, 2), &Grid::new(2, 2)).pct(), None);
    }

    /// Drive the observer with hand-made bus traffic: a 6502 `LDA ($10),Y`
    /// loading from the level buffer, then `CMP #$05`.
    #[test]
    fn the_observer_records_a_tile_read_and_its_compare() {
        let mut r = CollisionReader::new(Platform::Nes);
        r.set_resolver(Box::new(|a| (a >= 0x8000).then_some(a - 0x8000)));
        let acc = |addr, value| BusAccess {
            addr,
            value,
            write: false,
        };
        // LDA ($10),Y at $8100: opcode, operand, pointer lo/hi, data
        r.step(
            StepFacts::default(),
            &[
                acc(0x8100, 0xB1),
                acc(0x8101, 0x10),
                acc(0x0010, 0x00),
                acc(0x0011, 0x05),
                acc(0x0503, 0x07),
            ],
        );
        // CMP #$05 at $8102
        r.step(
            StepFacts::default(),
            &[acc(0x8102, 0xC9), acc(0x8103, 0x05)],
        );
        r.frame_end();
        r.attach_player(40, 3, false);
        let f = r.take_facts();
        assert_eq!(f.frames.len(), 1);
        assert_eq!(
            f.frames[0].reads,
            vec![TileRead {
                site: 0x100,
                ram: 0x503,
                value: 7
            }]
        );
        assert!(f.frames[0].sampled);
        assert!(f.sites[&0x100].consumers.contains(&Consumer::Compare));
    }
}
