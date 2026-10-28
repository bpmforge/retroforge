//! Widescreen finders (ticket W38-04): the places a game assumes the picture is
//! 256 pixels wide, listed as candidates with a confidence, and the pure half
//! of the lockstep trial that decides which candidates are keepers.
//! Offsets and counts only (law 5, D-021); no game is read or decompiled.
//!
//! ## Which of the W32-07 checklist points a finder can reach
//!
//! `docs/research/W32-07-level-sources.md` lists 18 screen-width assumptions.
//! Most are not constants in code (streaming granularity, wrap-around maps,
//! level width, OAM counts); the ones that are a *number the CPU compares or
//! adds to a screen-relative position* are:
//!
//! | checklist point | [`Kind`] | how it is seen |
//! |---|---|---|
//! | 4 spawn window | [`Kind::Activation`] | an immediate op near object-x and camera reads, and an object-table state byte going 0 to non-zero soon after it |
//! | 5 off-screen culling | [`Kind::Despawn`] | the same, state going non-zero to 0 |
//! | 6 draw-visible tests, 10 transition thresholds | [`Kind::EdgeTest`] | object-x and camera both read just before an immediate compare/add/sub, no state flip seen |
//! | 8 camera clamps | [`Kind::CameraClamp`] | camera read before the immediate op and the camera written just after |
//! | 10 screen-position triggers | [`Kind::PositionTrigger`] | object-x (the player is a slot) read before the immediate op, no camera |
//! | HUD anchors | [`Kind::HudAnchor`] | a sprite that stands still on screen while the camera moves, and the ROM byte (an immediate or a table entry) its x/y was stored from |
//! | 11 split lines, screen-size constants | [`Kind::ScreenConst`] | an immediate op whose operand is a screen edge (`$F0`, `$F8`, `$FF`, `$E0`, 16-bit `$0100`) with no better evidence; always low confidence |
//!
//! ## What is observed
//!
//! [`WideReader`] is a [`StepObserver`] like [`crate::collision::CollisionReader`].
//! Per instruction it sees the bus accesses; the opcode byte is `acc[0]` (the
//! same practice [`crate::dataregion`] uses; no disassembler). An **immediate
//! op** is `ADC #`, `SBC #`, `CMP #`, `CPX #` or `CPY #` whose accesses are just
//! its own fetches (two on the 6502; two or three on the 65816, whose width
//! follows the M/X flags, so the fetch count is the width). SMB's own widescreen
//! constants (`docs/research/W36-06-live-enemies.md`) are *add* and *subtract*
//! immediates feeding a compare, not compare operands, which is why all five
//! are watched.
//!
//! For each site the reader keeps the RAM bytes read in the few steps before
//! it (are they object-x bytes? camera bytes?), how the branch right after it
//! went, whether the camera was written right after, and whether an object
//! state byte flipped soon after. The caller names the object-x, state and
//! camera addresses (the W38-02 table and [`find_camera`]); the reader has no
//! game knowledge.
//!
//! ## Trial
//!
//! [`widen_values`] proposes a wider constant. The harness replays the same
//! input on a copy of the game with that byte changed and hands both runs'
//! per-frame [`Snap`]s to [`compare_trial`], which names the first frame they
//! differ and classifies every differing byte: a field of a slot that is
//! outside the original 256-px view in either run is **off-screen state**; OAM,
//! the camera and a slot on screen are **visible state**; the rest is
//! **bookkeeping** (the object list's read position, say). [`Trial::keeper`]
//! is the rule the ticket states: keep only when the first divergence is
//! off-screen state (plus a few bookkeeping bytes) *and* the constant
//! visibly widened something (live objects farther outside the view than the
//! unmodified game ever kept them).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rf_core_api::{BusAccess, StepFacts, StepObserver};

use crate::atlas::Atlas;
use crate::dataregion::Resolver;
use crate::hw::{classify, Hit, Platform};
use crate::objtable::{Frame, Trace};
use crate::recipe::{Grade, ObjectTable};

/// Steps a RAM read stays attached to an immediate op that follows it.
const WINDOW: u64 = 32;
/// Steps after a site in which a state flip or a camera write is credited to it.
const AFTER: u64 = 40;
/// Steps after a site in which a camera write is credited to it.
const CAM_AFTER: u64 = 8;
/// Recent reads kept.
const READS: usize = 64;
/// Recent site hits kept.
const RECENT: usize = 24;
/// Steps after a site in which its branch is looked for.
const BRANCH_AFTER: u64 = 4;
/// Steps in which a stored byte is matched to the byte it was loaded from.
const SOURCE_AFTER: u64 = 6;
/// The original picture width the finders measure margins against.
pub const VIEW_W: i32 = 256;
/// A sprite this far left of the picture is still partly visible.
pub const VIEW_PAD: i32 = 16;
/// Bookkeeping bytes tolerated in the first divergence of a keeper.
pub const BOOKKEEPING_MAX: usize = 8;
/// Pixels beyond the unmodified game's reach a trial must keep an object alive
/// for the constant to count as having widened anything.
pub const MIN_GAIN: i32 = 16;

/// The immediate ops watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImmOp {
    Adc,
    Sbc,
    Cmp,
    Cpx,
    Cpy,
}

impl ImmOp {
    fn of(opcode: u8) -> Option<Self> {
        match opcode {
            0x69 => Some(Self::Adc),
            0xE9 => Some(Self::Sbc),
            0xC9 => Some(Self::Cmp),
            0xE0 => Some(Self::Cpx),
            0xC0 => Some(Self::Cpy),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Adc => "adc",
            Self::Sbc => "sbc",
            Self::Cmp => "cmp",
            Self::Cpx => "cpx",
            Self::Cpy => "cpy",
        }
    }
}

/// Addresses the caller wants watched, in the form [`classify`] gives
/// (`0x7E_xxxx` on the SNES).
#[derive(Debug, Clone, Default)]
pub struct Watch {
    /// Object x bytes (low and high columns of the W38-02 table).
    pub obj_x: BTreeSet<u32>,
    /// Object state bytes (non-zero means in use).
    pub obj_state: BTreeSet<u32>,
    /// Camera bytes ([`Camera`]).
    pub cam: BTreeSet<u32>,
    /// RAM bytes holding the x/y of sprites that stand still on screen.
    pub hud: BTreeSet<u32>,
}

/// Where a stored HUD coordinate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HudSource {
    /// An `LDA #imm`; the ROM offset is the opcode.
    Immediate,
    /// A byte read from ROM (a table entry); the ROM offset is that byte.
    Table,
}

/// What was seen at one immediate-op site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteFacts {
    pub function: Option<u32>,
    pub cpu: u32,
    pub op: ImmOp,
    /// Operand bytes: 1, or 2 on the 65816 in 16-bit mode.
    pub width: u8,
    /// First operand seen.
    pub imm: u16,
    /// A later hit had another operand (self-modifying code, or the site is
    /// reached in both widths).
    pub imm_varies: bool,
    pub hits: u64,
    /// Frames the site ran in.
    pub frames: u32,
    /// Hits with an object-x byte read in the window / a camera byte / both.
    pub obj_x: u64,
    pub cam: u64,
    pub both: u64,
    /// The branch right after: taken / not taken.
    pub taken: u32,
    pub not_taken: u32,
    /// Camera writes within [`CAM_AFTER`] steps.
    pub cam_writes: u32,
    /// Object state bytes going 0 to non-zero / non-zero to 0 within [`AFTER`]
    /// steps, credited to hits that had an object-x byte in their window.
    pub flip_up: u32,
    pub flip_down: u32,
}

/// Everything one observed run saw.
#[derive(Debug, Clone, Default)]
pub struct WideFacts {
    pub platform: Option<Platform>,
    /// By ROM offset of the opcode.
    pub sites: BTreeMap<u32, SiteFacts>,
    /// Watched HUD byte to the sources its stores came from and how often.
    pub hud: BTreeMap<u32, BTreeMap<(u32, HudSource), u32>>,
    pub frames: u32,
    /// State flips seen in all (the recall denominator).
    pub flips_up: u64,
    pub flips_down: u64,
    pub cam_writes: u64,
}

#[derive(Debug, Clone, Copy)]
struct Recent {
    step: u64,
    site: u32,
    obj_x: bool,
    cam: bool,
}

/// The observer: hand it to `run_frame_observed` with the resolver set for the
/// frame, then [`Self::take_facts`].
pub struct WideReader {
    platform: Platform,
    resolver: Resolver,
    watch: Watch,
    step: u64,
    reads: VecDeque<(u64, u32)>,
    last: BTreeMap<u32, u8>,
    recent: VecDeque<Recent>,
    pending_site: Option<(u64, u32)>,
    pending_branch: Option<(u32, u32)>,
    /// Recent byte sources: (step, rom offset, kind, value).
    sources: VecDeque<(u64, u32, HudSource, u8)>,
    this_frame: BTreeSet<u32>,
    facts: WideFacts,
}

impl WideReader {
    #[must_use]
    pub fn new(platform: Platform, watch: Watch) -> Self {
        Self {
            platform,
            resolver: Box::new(|_| None),
            watch,
            step: 0,
            reads: VecDeque::new(),
            last: BTreeMap::new(),
            recent: VecDeque::new(),
            pending_site: None,
            pending_branch: None,
            sources: VecDeque::new(),
            this_frame: BTreeSet::new(),
            facts: WideFacts {
                platform: Some(platform),
                ..WideFacts::default()
            },
        }
    }

    /// Install the CPU-address to ROM-offset map for the frames that follow.
    pub fn set_resolver(&mut self, resolver: Resolver) {
        self.resolver = resolver;
    }

    /// What has been seen so far; the reader goes on into a fresh set.
    pub fn take_facts(&mut self) -> WideFacts {
        std::mem::replace(
            &mut self.facts,
            WideFacts {
                platform: Some(self.platform),
                ..WideFacts::default()
            },
        )
    }

    fn ram(&self, addr: u32, write: bool) -> Option<u32> {
        match classify(self.platform, addr, write) {
            Hit::Ram { addr } => Some(addr),
            _ => None,
        }
    }

    /// The fall-through address of a 2-byte instruction at `pc`.
    fn fall(&self, pc: u32) -> u32 {
        match self.platform {
            Platform::Nes => pc.wrapping_add(2) & 0xFFFF,
            Platform::Snes => (pc & 0xFF_0000) | (pc.wrapping_add(2) & 0xFFFF),
        }
    }

    fn is_branch(opcode: u8) -> bool {
        opcode & 0x1F == 0x10
    }

    fn push_recent(&mut self, r: Recent) {
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(r);
    }

    fn credit(&mut self, f: impl Fn(&Recent) -> bool, apply: impl Fn(&mut SiteFacts)) {
        let step = self.step;
        let sites: Vec<u32> = self
            .recent
            .iter()
            .filter(|r| step - r.step <= AFTER && f(r))
            .map(|r| r.site)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for s in sites {
            if let Some(site) = self.facts.sites.get_mut(&s) {
                apply(site);
            }
        }
    }

    /// The ROM data bytes an instruction read (not its own fetches).
    fn rom_reads(&self, acc: &[BusAccess]) -> Vec<(u32, u8)> {
        let pc = acc[0].addr;
        let cap = match self.platform {
            Platform::Nes => 3,
            Platform::Snes => 4,
        };
        let mut prefix = 1;
        while prefix < cap
            && acc
                .get(prefix)
                .is_some_and(|a| !a.write && a.addr == pc.wrapping_add(prefix as u32))
        {
            prefix += 1;
        }
        acc.iter()
            .skip(prefix)
            .filter(|a| !a.write)
            .filter_map(|a| (self.resolver)(a.addr).map(|r| (r, a.value)))
            .collect()
    }

    fn observe_writes(&mut self, acc: &[BusAccess]) {
        for a in acc.iter().filter(|a| a.write) {
            let Some(ram) = self.ram(a.addr, true) else {
                continue;
            };
            let mut prev = self.last.insert(ram, a.value);
            // State bytes start at zero on power-on; a first sight is a 0.
            if prev.is_none() && self.watch.obj_state.contains(&ram) {
                prev = Some(0);
            }
            if self.watch.cam.contains(&ram) {
                self.facts.cam_writes += 1;
                let step = self.step;
                let cam_recent: Vec<u32> = self
                    .recent
                    .iter()
                    .filter(|r| step - r.step <= CAM_AFTER && r.cam)
                    .map(|r| r.site)
                    .collect();
                for s in cam_recent {
                    if let Some(site) = self.facts.sites.get_mut(&s) {
                        site.cam_writes += 1;
                    }
                }
            }
            if self.watch.obj_state.contains(&ram) {
                match (prev, a.value) {
                    (Some(0), v) if v != 0 => {
                        self.facts.flips_up += 1;
                        self.credit(|r| r.obj_x, |s| s.flip_up += 1);
                    }
                    (Some(p), 0) if p != 0 => {
                        self.facts.flips_down += 1;
                        self.credit(|r| r.obj_x, |s| s.flip_down += 1);
                    }
                    _ => {}
                }
            }
            if self.watch.hud.contains(&ram) {
                let step = self.step;
                let src = self
                    .sources
                    .iter()
                    .rev()
                    .find(|s| step - s.0 <= SOURCE_AFTER && s.3 == a.value)
                    .map(|s| (s.1, s.2));
                if let Some(key) = src {
                    *self
                        .facts
                        .hud
                        .entry(ram)
                        .or_default()
                        .entry(key)
                        .or_default() += 1;
                }
            }
        }
    }
}

impl StepObserver for WideReader {
    fn step(&mut self, facts: StepFacts, acc: &[BusAccess]) {
        self.step += 1;
        let step = self.step;
        if facts.interrupt || acc.is_empty() {
            self.pending_site = None;
            self.pending_branch = None;
            return;
        }
        let pc = acc[0].addr;
        let opcode = acc[0].value;
        // The branch after a site: taken when the next instruction is not the
        // fall-through.
        if let Some((site, bpc)) = self.pending_branch.take() {
            let fell = pc == self.fall(bpc);
            if let Some(s) = self.facts.sites.get_mut(&site) {
                if fell {
                    s.not_taken += 1;
                } else {
                    s.taken += 1;
                }
            }
        }
        if let Some((at, site)) = self.pending_site {
            if step - at > BRANCH_AFTER {
                self.pending_site = None;
            } else if Self::is_branch(opcode) {
                self.pending_branch = Some((site, pc));
                self.pending_site = None;
            }
        }
        // RAM reads feed the window; ROM reads feed the HUD source match.
        for a in acc.iter().skip(1).filter(|a| !a.write) {
            if let Some(ram) = self.ram(a.addr, false) {
                self.last.insert(ram, a.value);
                if self.reads.len() == READS {
                    self.reads.pop_front();
                }
                self.reads.push_back((step, ram));
            }
        }
        if !self.watch.hud.is_empty() {
            let site = (self.resolver)(pc);
            if opcode == 0xA9 && acc.len() >= 2 {
                if let Some(rom) = site {
                    self.push_source((step, rom, HudSource::Immediate, acc[1].value));
                }
            } else {
                let reads = self.rom_reads(acc);
                for (rom, v) in reads {
                    self.push_source((step, rom, HudSource::Table, v));
                }
            }
        }
        self.observe_writes(acc);
        // The immediate ops.
        let Some(op) = ImmOp::of(opcode) else {
            return;
        };
        let operand_ok = (2..=3).contains(&acc.len())
            && acc
                .iter()
                .enumerate()
                .all(|(i, a)| !a.write && a.addr == pc.wrapping_add(i as u32));
        let width_ok = self.platform == Platform::Snes || acc.len() == 2;
        if !operand_ok || !width_ok {
            return;
        }
        let Some(rom) = (self.resolver)(pc) else {
            return;
        };
        let width = (acc.len() - 1) as u8;
        let imm = if width == 2 {
            u16::from(acc[1].value) | (u16::from(acc[2].value) << 8)
        } else {
            u16::from(acc[1].value)
        };
        let (mut ox, mut cm) = (false, false);
        for (s, addr) in &self.reads {
            if step - s <= WINDOW {
                ox |= self.watch.obj_x.contains(addr);
                cm |= self.watch.cam.contains(addr);
            }
        }
        let s = self.facts.sites.entry(rom).or_insert_with(|| SiteFacts {
            function: facts.function,
            cpu: pc,
            op,
            width,
            imm,
            imm_varies: false,
            hits: 0,
            frames: 0,
            obj_x: 0,
            cam: 0,
            both: 0,
            taken: 0,
            not_taken: 0,
            cam_writes: 0,
            flip_up: 0,
            flip_down: 0,
        });
        s.function = s.function.or(facts.function);
        s.imm_varies |= s.imm != imm || s.width != width;
        s.hits += 1;
        s.obj_x += u64::from(ox);
        s.cam += u64::from(cm);
        s.both += u64::from(ox && cm);
        self.this_frame.insert(rom);
        self.pending_site = Some((step, rom));
        self.push_recent(Recent {
            step,
            site: rom,
            obj_x: ox,
            cam: cm,
        });
    }

    fn frame_end(&mut self) {
        self.facts.frames += 1;
        for rom in std::mem::take(&mut self.this_frame) {
            if let Some(s) = self.facts.sites.get_mut(&rom) {
                s.frames += 1;
            }
        }
    }
}

impl WideReader {
    fn push_source(&mut self, s: (u64, u32, HudSource, u8)) {
        if self.sources.len() == 16 {
            self.sources.pop_front();
        }
        self.sources.push_back(s);
    }
}

/// What a candidate is a candidate for (see the module table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Activation,
    Despawn,
    EdgeTest,
    CameraClamp,
    PositionTrigger,
    HudAnchor,
    ScreenConst,
}

impl Kind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Activation => "activation",
            Self::Despawn => "despawn",
            Self::EdgeTest => "edge-test",
            Self::CameraClamp => "camera-clamp",
            Self::PositionTrigger => "position-trigger",
            Self::HudAnchor => "hud-anchor",
            Self::ScreenConst => "screen-const",
        }
    }
}

/// Confidence bands over the 0-100 score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    Low,
    Medium,
    High,
}

impl Band {
    #[must_use]
    pub const fn of(score: u8) -> Self {
        if score >= 70 {
            Self::High
        } else if score >= 45 {
            Self::Medium
        } else {
            Self::Low
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// One place that may assume a 256-px picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: Kind,
    /// ROM offset of the opcode (or of the table byte, for a HUD table source).
    pub site: u32,
    /// ROM offset of the operand's low byte (`site + 1`), the byte a trial edits.
    pub operand: u32,
    pub function: Option<u32>,
    pub op: Option<ImmOp>,
    pub width: u8,
    pub imm: u16,
    pub score: u8,
    pub band: Band,
    /// One line of counts that justify the score (offsets and numbers only).
    pub evidence: String,
}

/// 8-bit screen edges and 16-bit widths that mark a bare compare as a screen
/// constant.
fn screen_const(imm: u16, width: u8) -> bool {
    match width {
        1 => matches!(imm, 0xE0 | 0xE8 | 0xF0 | 0xF8 | 0xFF),
        _ => matches!(imm, 0x0100 | 0x0110 | 0x0120 | 0x01F0 | 0x0200),
    }
}

/// Per function, the state flips credited to its sites: a function that
/// spawns or removes objects is where a bare camera-relative constant (checked
/// against the level data, not a slot) belongs too.
fn flip_functions(f: &WideFacts) -> BTreeMap<u32, (u32, u32)> {
    let mut m: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    for s in f.sites.values() {
        if let Some(func) = s.function {
            let e = m.entry(func).or_default();
            e.0 += s.flip_up;
            e.1 += s.flip_down;
        }
    }
    m.retain(|_, v| v.0 + v.1 > 0);
    m
}

fn kind_of(s: &SiteFacts, f: &WideFacts, flips: &BTreeMap<u32, (u32, u32)>) -> Option<Kind> {
    let up_down = (s.flip_up, s.flip_down);
    if s.obj_x > 0 && (up_down.0 > 0 || up_down.1 > 0) {
        return Some(if up_down.0 >= up_down.1 {
            Kind::Activation
        } else {
            Kind::Despawn
        });
    }
    // Operands 0 to 3 are carries and unit steps, not distances.
    if s.imm < 4 && s.width == 1 {
        return screen_const(s.imm, s.width).then_some(Kind::ScreenConst);
    }
    if s.cam > 0 && s.cam_writes > 0 && s.cam_writes as u64 * 4 >= s.cam {
        return Some(Kind::CameraClamp);
    }
    if s.cam > 0 && s.obj_x == 0 {
        if let Some(&(up, down)) = s.function.and_then(|func| flips.get(&func)) {
            return Some(if up >= down {
                Kind::Activation
            } else {
                Kind::Despawn
            });
        }
    }
    if s.both > 0 {
        return Some(Kind::EdgeTest);
    }
    if s.obj_x > 0 {
        return Some(Kind::PositionTrigger);
    }
    if s.cam > 0 {
        return Some(Kind::CameraClamp);
    }
    let _ = f;
    screen_const(s.imm, s.width).then_some(Kind::ScreenConst)
}

/// The 0-100 score of a site, with the line that explains it.
fn score(s: &SiteFacts, kind: Kind, f: &WideFacts) -> (u8, String) {
    let mut sc = 0u32;
    let flips = (s.flip_up + s.flip_down) as u64;
    let total = f.flips_up + f.flips_down;
    // Recall: the share of state flips this site sat in front of.
    let recall = if total == 0 {
        0.0
    } else {
        flips as f64 / total as f64
    };
    match kind {
        Kind::Activation | Kind::Despawn => {
            sc += 40;
            if recall >= 0.5 {
                sc += 15;
            }
            // A site that runs thousands of times per flip is a bystander.
            if s.hits <= 200 * flips.max(1) {
                sc += 10;
            }
        }
        Kind::EdgeTest => sc += 25,
        Kind::CameraClamp => {
            sc += 15;
            if s.cam_writes > 0 {
                sc += 20;
            }
        }
        Kind::PositionTrigger => sc += 15,
        Kind::HudAnchor => sc += 40,
        Kind::ScreenConst => sc += 10,
    }
    if s.obj_x > 0 && s.cam > 0 {
        sc += 15;
    }
    // The decision goes both ways in the run: it is live, not dead code.
    if s.taken >= 2 && s.not_taken >= 2 {
        sc += 15;
    }
    if s.hits >= 100 {
        sc += 5;
    }
    if !screen_const(s.imm, s.width) && s.imm >= 8 {
        sc += 5;
    }
    let line = format!(
        "hits {} frames {} objx {} cam {} both {} taken {}/{} camw {} up {} down {} recall {:.2}",
        s.hits,
        s.frames,
        s.obj_x,
        s.cam,
        s.both,
        s.taken,
        s.not_taken,
        s.cam_writes,
        s.flip_up,
        s.flip_down,
        recall
    );
    (sc.min(100) as u8, line)
}

/// How many candidates [`infer`] keeps.
pub const MAX_CANDIDATES: usize = 60;

/// Rank what a run saw into candidates, best first.
#[must_use]
pub fn infer(f: &WideFacts) -> Vec<Candidate> {
    let flips = flip_functions(f);
    let mut out: Vec<Candidate> = Vec::new();
    for (&rom, s) in &f.sites {
        let Some(kind) = kind_of(s, f, &flips) else {
            continue;
        };
        let (mut sc, evidence) = score(s, kind, f);
        // Sharing a function with the state flips is evidence in itself.
        if s.flip_up + s.flip_down == 0
            && s.function.is_some_and(|func| flips.contains_key(&func))
            && matches!(kind, Kind::Activation | Kind::Despawn)
        {
            sc = sc.saturating_add(25).min(100);
        }
        out.push(Candidate {
            kind,
            site: rom,
            operand: rom + 1,
            function: s.function,
            op: Some(s.op),
            width: s.width,
            imm: s.imm,
            score: sc,
            band: Band::of(sc),
            evidence,
        });
    }
    for (addr, srcs) in &f.hud {
        for (&(rom, src), &n) in srcs {
            let (site, operand) = match src {
                HudSource::Immediate => (rom, rom + 1),
                HudSource::Table => (rom, rom),
            };
            let sc = 40 + u8::try_from(n.min(20)).unwrap_or(20);
            out.push(Candidate {
                kind: Kind::HudAnchor,
                site,
                operand,
                function: None,
                op: None,
                width: 1,
                imm: 0,
                score: sc,
                band: Band::of(sc),
                evidence: format!(
                    "stored to ram {addr:#x} {n} times from a {} byte",
                    match src {
                        HudSource::Immediate => "immediate",
                        HudSource::Table => "table",
                    }
                ),
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.site.cmp(&b.site))
            .then(a.kind.cmp(&b.kind))
    });
    out.truncate(MAX_CANDIDATES);
    out
}

/// The wider constants a trial tries for `c` (operand bytes, low first). Empty
/// when "wider" has no byte representation: an 8-bit operand already at its
/// maximum.
#[must_use]
pub fn widen_values(c: &Candidate) -> Vec<u16> {
    if c.op.is_none() {
        return Vec::new();
    }
    let mut v: Vec<u16> = if c.width == 1 {
        let mid = (c.imm + 0x60).min(0xFF);
        vec![mid, 0xFF]
    } else {
        vec![c.imm.saturating_add(0x80), c.imm.saturating_add(0x180)]
    };
    v.retain(|&x| x > c.imm);
    v.dedup();
    v
}

/// The bytes to write at [`Candidate::operand`] for operand value `value`.
#[must_use]
pub fn operand_bytes(width: u8, value: u16) -> Vec<u8> {
    if width == 1 {
        vec![value as u8]
    } else {
        vec![value as u8, (value >> 8) as u8]
    }
}

// ---------------------------------------------------------------------------
// The camera and the HUD, found from a trace.
// ---------------------------------------------------------------------------

/// The camera bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Camera {
    /// RAM index of the low byte (the one that equals the scroll's low eight
    /// bits).
    pub lo: usize,
    /// RAM index of the page byte: `hi * 256 + lo` moves smoothly with the
    /// scroll. `None` when none does.
    pub hi: Option<usize>,
}

/// Find the camera in a trace: the RAM byte equal to the scroll's low eight
/// bits in nine frames of ten, and the byte that makes a smooth 16-bit
/// position with it. Names no address from game knowledge.
#[must_use]
pub fn find_camera(t: &Trace) -> Option<Camera> {
    let n = t.frames.len();
    let r = t.frames.iter().map(|f| f.ram.len()).min()?;
    if n < 60 || r == 0 {
        return None;
    }
    let moving = t
        .frames
        .windows(2)
        .filter(|w| w[0].cam_x != w[1].cam_x)
        .count();
    if moving < 20 {
        return None;
    }
    // The scroll the PPU used and the byte the game keeps can differ by a
    // frame of travel, so "equal" means within a few pixels (mod 256); the
    // exact matches break ties.
    // The game's byte is the one the PPU scroll is loaded from, so it is level
    // with the scroll or ahead of it; a byte a few pixels *behind* is another
    // edge (a right edge one pixel short of the picture's width, say). Only
    // when nothing is level or ahead is a byte behind accepted.
    let ahead = |ram: u8, cam: u16| ram.wrapping_sub((cam & 0xFF) as u8) <= 4;
    let behind = |ram: u8, cam: u16| {
        let d = ram.wrapping_sub((cam & 0xFF) as u8);
        d <= 4 || d >= 252
    };
    let pick = |near: &dyn Fn(u8, u16) -> bool| {
        let mut best: Option<(usize, usize)> = None;
        for a in 0..r {
            let hits = t.frames.iter().filter(|f| near(f.ram[a], f.cam_x)).count();
            let exact = t
                .frames
                .iter()
                .filter(|f| f.ram[a] == (f.cam_x & 0xFF) as u8)
                .count();
            if hits * 10 >= n * 9 && best.is_none_or(|(_, e)| exact > e) {
                best = Some((a, exact));
            }
        }
        best
    };
    let best = pick(&ahead).or_else(|| pick(&behind));
    let (lo, _) = best?;
    // The page byte: smoothest (hi * 256 + lo) among the bytes that change.
    let mut hi: Option<(usize, usize)> = None;
    for a in (0..r).filter(|&a| a != lo) {
        if t.frames.windows(2).all(|w| w[0].ram[a] == w[1].ram[a]) {
            continue;
        }
        let smooth = t
            .frames
            .windows(2)
            .filter(|w| {
                let p = i32::from(w[0].ram[a]) * 256 + i32::from(w[0].ram[lo]);
                let q = i32::from(w[1].ram[a]) * 256 + i32::from(w[1].ram[lo]);
                (q - p).abs() <= 32
            })
            .count();
        if smooth * 100 >= (n - 1) * 99 && hi.is_none_or(|(_, s)| smooth > s) {
            hi = Some((a, smooth));
        }
    }
    Some(Camera {
        lo,
        hi: hi.map(|(a, _)| a),
    })
}

/// RAM bytes that keep a fixed distance from the camera's low (or page) byte
/// in nine frames of ten and are not constant: the right edge, the copy the
/// PPU is loaded from. Code that compares against the screen's other edge
/// reads these, not the camera itself. Excludes the camera bytes.
#[must_use]
pub fn camera_aliases(t: &Trace, cam: &Camera) -> Vec<usize> {
    let n = t.frames.len();
    let r = t.frames.iter().map(|f| f.ram.len()).min().unwrap_or(0);
    let mut out = Vec::new();
    for (base, is_hi) in [(Some(cam.lo), false), (cam.hi, true)] {
        let Some(base) = base else { continue };
        for a in (0..r).filter(|&a| a != cam.lo && Some(a) != cam.hi) {
            if t.frames.windows(2).all(|w| w[0].ram[a] == w[1].ram[a]) {
                continue;
            }
            let mut votes = [0usize; 256];
            for f in &t.frames {
                votes[usize::from(f.ram[a].wrapping_sub(f.ram[base]))] += 1;
            }
            // A page byte that merely counts up is not tied to the camera.
            let top = votes.iter().copied().max().unwrap_or(0);
            if top * 100 >= n * 95 && (!is_hi || top * 100 >= n * 98) && !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// What the x page of a table is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XPage {
    /// A column at this offset from the table's base.
    Column(u32),
    /// No column; a page of zero places the slots as the game does (every
    /// slot stayed within the first 256 px of the level in the run).
    Zero,
}

/// Find the page column of a table that has none. A correct page makes a live
/// slot's world x move smoothly from one frame to the next (a slot's x low byte
/// wraps at 256; a wrong page then jumps by 256). So: of the pairs of
/// consecutive frames in which a slot is live in both, the share whose world x
/// moved by at most 40; a column that scores 97% and clearly beats a zero page
/// with the most slots near the camera (within -300..600 px of it) and clearly
/// more than a zero page has is the page; else a zero page that scores 97% is
/// [`XPage::Zero`]; else `None`. Returns `None` when the table already has a
/// page column or too little is live.
#[must_use]
pub fn find_x_page(table: &ObjectTable, cam: &Camera, t: &Trace) -> Option<XPage> {
    let x_lo = table.x_lo?;
    if table.x_hi.is_some() || table.stride == 0 {
        return None;
    }
    let r = t.frames.iter().map(|f| f.ram.len()).min()?;
    let at = |off: u32, i: u32| {
        // Wrapping: `off` ranges over every RAM byte, some of them below the base.
        table
            .base
            .wrapping_add(off)
            .wrapping_add(i.wrapping_mul(table.stride))
            .wrapping_sub(t.ram_base) as usize
    };
    let alive = |f: &Frame, i: u32| match table.state {
        Some(st) => at(st, i) < r && f.ram[at(st, i)] != 0,
        None => true,
    };
    let world = |f: &Frame, i: u32, page: Option<u32>| -> Option<i32> {
        let lo = at(x_lo, i);
        if lo >= r {
            return None;
        }
        let hi = match page {
            Some(o) => {
                let a = at(o, i);
                if a >= r {
                    return None;
                }
                i32::from(f.ram[a])
            }
            None => 0,
        };
        Some(hi * 256 + i32::from(f.ram[lo]))
    };
    // The camera's world x in a frame (0 without a page byte: then the range
    // test below passes nothing and smoothness decides alone).
    let cam_x = |f: &Frame| {
        cam.hi
            .map(|h| i32::from(f.ram[h]) * 256 + i32::from(f.ram[cam.lo]))
    };
    // (pairs, smooth pairs, pairs near the camera) for a page choice.
    let score = |page: Option<u32>| -> Option<(u32, u32, u32)> {
        let (mut pairs, mut smooth, mut near) = (0u32, 0u32, 0u32);
        for w in t.frames.windows(2) {
            for i in 0..table.slots {
                if !(alive(&w[0], i) && alive(&w[1], i)) {
                    continue;
                }
                let (a, b) = (world(&w[0], i, page)?, world(&w[1], i, page)?);
                pairs += 1;
                smooth += u32::from((b - a).abs() <= 40);
                // Live objects sit within a screen or so of the camera.
                near += u32::from(cam_x(&w[1]).is_none_or(|c| (-300..600).contains(&(b - c))));
            }
        }
        Some((pairs, smooth, near))
    };
    let (pairs, zero, zero_near) = score(None)?;
    if pairs < 50 {
        return None;
    }
    let margin = (pairs / 200).max(2);
    // Most pairs near the camera, then the smoothest, then the lowest offset.
    let mut best: Option<(u32, u32, u32)> = None;
    for a in 0..r as u32 {
        let o = t.ram_base.wrapping_add(a).wrapping_sub(table.base);
        if o == x_lo {
            continue;
        }
        if let Some((n, sm, near)) = score(Some(o)) {
            if n == pairs
                && sm * 100 >= pairs * 97
                && best.is_none_or(|(_, b, c)| (near, sm) > (b, c))
            {
                best = Some((o, near, sm));
            }
        }
    }
    match best {
        Some((o, near, _)) if near >= zero_near + margin => Some(XPage::Column(o)),
        _ if zero * 100 >= pairs * 97 => Some(XPage::Zero),
        Some((o, _, _)) => Some(XPage::Column(o)),
        None => None,
    }
}

/// A sprite that stands still on screen while the camera moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticSprite {
    pub index: u16,
    pub x: u8,
    pub y: u8,
    pub frames: u32,
}

/// Sprites at the same (x, y) in nine frames of ten, with the camera having
/// moved at least [`VIEW_W`] pixels in all: HUD sprites.
#[must_use]
pub fn static_sprites(t: &Trace) -> Vec<StaticSprite> {
    let n = t.frames.len();
    let travelled: u32 = t
        .frames
        .windows(2)
        .map(|w| u32::from((i32::from(w[1].cam_x) - i32::from(w[0].cam_x)).unsigned_abs() as u16))
        .sum();
    if n < 60 || travelled < VIEW_W as u32 {
        return Vec::new();
    }
    let mut seen: BTreeMap<(u16, u8, u8), u32> = BTreeMap::new();
    for f in &t.frames {
        for s in &f.sprites {
            *seen.entry((s.index, s.x, s.y)).or_default() += 1;
        }
    }
    seen.into_iter()
        .filter(|&(_, c)| c as usize * 10 >= n * 9)
        .map(|((index, x, y), frames)| StaticSprite {
            index,
            x,
            y,
            frames,
        })
        .collect()
}

/// RAM index of the OAM shadow: where the OAM the PPU holds is mirrored in
/// work RAM (the buffer a DMA copies from). `None` when no place matches in
/// nine frames of ten.
#[must_use]
pub fn oam_shadow(t: &Trace) -> Option<usize> {
    let f0 = t.frames.first()?;
    let n = t.frames.len();
    let len = f0.oam.len().min(512);
    let r = t.frames.iter().map(|f| f.ram.len()).min()?;
    if len < 64 || r < len || n < 20 {
        return None;
    }
    (0..=r - len).find(|&o| {
        t.frames
            .iter()
            .filter(|f| f.oam.len() >= len && f.ram[o..o + len] == f.oam[..len])
            .count()
            * 10
            >= n * 9
    })
}

/// The shadow-RAM bytes (x and y) of the static sprites, as watch addresses.
#[must_use]
pub fn hud_watch(t: &Trace, shadow: usize, sprites: &[StaticSprite]) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for s in sprites {
        let base = shadow + usize::from(s.index) * 4;
        // NES layout: y, tile, attr, x. SNES: x, y, tile, attr.
        let (a, b) = if t.ram_base == 0 {
            (base, base + 3)
        } else {
            (base, base + 1)
        };
        out.insert(t.ram_base + a as u32);
        out.insert(t.ram_base + b as u32);
    }
    out
}

// ---------------------------------------------------------------------------
// The trial.
// ---------------------------------------------------------------------------

/// One frame of a run, as the trial compares it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snap {
    /// Work RAM from its start (the window the harness chose).
    pub ram: Vec<u8>,
    pub oam: Vec<u8>,
    pub cam_x: u16,
}

/// How to read a [`Snap`]: the object table and the camera.
#[derive(Debug, Clone)]
pub struct View {
    /// Added to a RAM index to make an address (`0` NES, `0x7E_0000` SNES).
    pub ram_base: u32,
    pub table: ObjectTable,
    pub cam_lo: usize,
    pub cam_hi: Option<usize>,
    /// The table has no page column and a page of zero is right
    /// ([`XPage::Zero`]).
    pub x_page_zero: bool,
}

impl View {
    fn idx(&self, addr: u32) -> usize {
        addr.wrapping_sub(self.ram_base) as usize
    }

    fn field(&self, off: Option<u32>, slot: u32) -> Option<usize> {
        off.map(|o| {
            self.idx(
                self.table
                    .base
                    .wrapping_add(o)
                    .wrapping_add(slot.wrapping_mul(self.table.stride)),
            )
        })
    }

    /// The camera position, `None` without a page byte.
    fn cam(&self, s: &Snap) -> Option<i32> {
        let hi = *s.ram.get(self.cam_hi?)?;
        let lo = *s.ram.get(self.cam_lo)?;
        Some(i32::from(hi) * 256 + i32::from(lo))
    }

    /// Slot `i`'s state: in use? `true` when the table has no state byte.
    fn live(&self, s: &Snap, i: u32) -> bool {
        match self.field(self.table.state, i) {
            Some(a) => s.ram.get(a).is_some_and(|&v| v != 0),
            None => true,
        }
    }

    /// Slot `i`'s x relative to the camera, `None` when either has no page.
    pub fn rel_x(&self, s: &Snap, i: u32) -> Option<i32> {
        let lo = *s.ram.get(self.field(self.table.x_lo, i)?)?;
        let hi = match self.field(self.table.x_hi, i) {
            Some(a) => *s.ram.get(a)?,
            None if self.x_page_zero => 0,
            None => return None,
        };
        Some(i32::from(hi) * 256 + i32::from(lo) - self.cam(s)?)
    }

    /// Whether slot `i` shows in the original picture of `s` (conservatively
    /// `true` when its position cannot be told).
    fn on_screen(&self, s: &Snap, i: u32) -> bool {
        match self.rel_x(s, i) {
            Some(r) => self.live(s, i) && (-VIEW_PAD..VIEW_W).contains(&r),
            None => true,
        }
    }

    /// Every RAM index that belongs to slot `i`.
    fn slot_indices(&self, i: u32) -> Vec<usize> {
        let t = &self.table;
        [t.x_lo, t.x_hi, t.y, t.y_hi, t.kind, t.state]
            .into_iter()
            .filter_map(|o| self.field(o, i))
            .collect()
    }
}

/// Where live objects reached relative to the camera over a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reach {
    pub max_right: i32,
    pub min_left: i32,
}

/// Frames an object must stay out at a distance for it to count as reach: an
/// object spawned and removed again in the next frame has not been kept.
pub const PERSIST: usize = 8;

/// The farthest right and left any live slot stayed from the camera for
/// [`PERSIST`] frames running (the nearer of its extremes over the window).
#[must_use]
pub fn reach(v: &View, run: &[Snap]) -> Reach {
    let mut r = Reach {
        max_right: VIEW_W,
        min_left: 0,
    };
    for i in 0..v.table.slots {
        let pos: Vec<Option<i32>> = run
            .iter()
            .map(|s| {
                if !v.live(s, i) {
                    return None;
                }
                // Wrapped garbage (a dead-looking slot) is not reach.
                v.rel_x(s, i).filter(|x| (-2048..4096).contains(x))
            })
            .collect();
        // `windows` yields nothing for a run shorter than the window.
        for w in pos.windows(PERSIST) {
            if w.iter().all(Option::is_some) {
                let (lo, hi) = w
                    .iter()
                    .flatten()
                    .fold((i32::MAX, i32::MIN), |(l, h), &x| (l.min(x), h.max(x)));
                r.max_right = r.max_right.max(lo);
                r.min_left = r.min_left.min(hi);
            }
        }
    }
    r
}

/// Widen a table to the slots the recipe's RAM rows say exist. The finder only
/// claims slots it bound to a sprite, and objects that never show have slots
/// too; a `[[ram]]` row whose address falls in the table's state (or x) column
/// says how far that column runs (a row must start inside the slots already
/// claimed or right after them, so a different array is not mistaken for it).
/// Never shrinks the table.
#[must_use]
pub fn slots_from_rows(table: &ObjectTable, rows: &[crate::recipe::RamMeaning]) -> ObjectTable {
    let mut out = table.clone();
    for off in [table.state, table.x_lo].into_iter().flatten() {
        let start = table.base + off;
        for r in rows {
            let end = r.addr.saturating_add(u32::from(r.width));
            if r.addr >= start && r.addr - start <= table.slots && table.stride > 0 {
                out.slots = out.slots.max((end - start).div_ceil(table.stride));
            }
        }
    }
    out
}

/// The first divergence, classified.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Divergence {
    pub frame: u32,
    /// RAM indices of slot fields of slots outside the picture in both runs.
    pub off_screen: Vec<usize>,
    /// RAM indices of fields of slots on screen in either run.
    pub on_screen: Vec<usize>,
    /// Other RAM indices.
    pub bookkeeping: Vec<usize>,
    pub oam: bool,
    pub camera: bool,
}

/// The outcome of one trial.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trial {
    pub frames: u32,
    /// Object slots the comparison used (the table, grown by [`extend_slots`]).
    pub slots: u32,
    pub first: Option<Divergence>,
    /// First frame the picture differs (OAM, camera or an on-screen slot), if
    /// ever.
    pub visible_at: Option<u32>,
    /// Pixels past the unmodified game's reach a live object was kept to.
    pub gain_right: i32,
    pub gain_left: i32,
}

impl Trial {
    /// The ticket's rule: keep a constant only when the first divergence
    /// touches only off-screen state (and at most [`BOOKKEEPING_MAX`]
    /// bookkeeping bytes) and live objects were kept visibly farther out than
    /// the unmodified game ever kept them.
    #[must_use]
    pub fn keeper(&self) -> bool {
        let Some(d) = &self.first else {
            return false;
        };
        d.on_screen.is_empty()
            && !d.oam
            && !d.camera
            && (!d.off_screen.is_empty() || !d.bookkeeping.is_empty())
            && d.bookkeeping.len() <= BOOKKEEPING_MAX
            && self.widened()
    }

    /// Whether live objects were kept at least [`MIN_GAIN`] farther out.
    #[must_use]
    pub fn widened(&self) -> bool {
        self.gain_right >= MIN_GAIN || self.gain_left >= MIN_GAIN
    }

    /// What the widening did, when it did: objects kept farther right is an
    /// activation window, farther left a despawn window.
    #[must_use]
    pub fn reach_kind(&self) -> Option<Kind> {
        if !self.widened() {
            None
        } else if self.gain_right >= self.gain_left {
            Some(Kind::Activation)
        } else {
            Some(Kind::Despawn)
        }
    }

    /// Why this trial is not a keeper (empty for a keeper).
    #[must_use]
    pub fn why_not(&self) -> &'static str {
        match &self.first {
            None => "never diverged (the constant did not matter in this run)",
            Some(d) if d.oam || d.camera => "first divergence changed the picture or camera",
            Some(d) if !d.on_screen.is_empty() => "first divergence touched an on-screen object",
            Some(d) if d.off_screen.is_empty() && d.bookkeeping.is_empty() => {
                "first divergence changed no state"
            }
            Some(d) if d.bookkeeping.len() > BOOKKEEPING_MAX => {
                "first divergence touched too much other state"
            }
            _ if !self.widened() => "nothing was kept farther out than the unmodified game does",
            _ => "",
        }
    }
}

/// Compare an unmodified run with a run of a modified copy, frame by frame.
/// Law 8: both walks are bounded `for` loops over the shorter run.
#[must_use]
pub fn compare_trial(v: &View, plain: &[Snap], patched: &[Snap]) -> Trial {
    let n = plain.len().min(patched.len());
    let mut t = Trial {
        frames: n as u32,
        slots: v.table.slots,
        ..Trial::default()
    };
    let base = reach(v, &plain[..n]);
    let wide = reach(v, &patched[..n]);
    t.gain_right = wide.max_right - base.max_right;
    t.gain_left = base.min_left - wide.min_left;
    for f in 0..n {
        let (a, b) = (&plain[f], &patched[f]);
        let diff: Vec<usize> = a
            .ram
            .iter()
            .zip(&b.ram)
            .enumerate()
            .filter(|(_, (x, y))| x != y)
            .map(|(i, _)| i)
            .collect();
        let oam = a.oam != b.oam;
        let camera = a.cam_x != b.cam_x;
        let slot_on: Vec<bool> = (0..v.table.slots)
            .map(|i| v.on_screen(a, i) || v.on_screen(b, i))
            .collect();
        let mut owner: BTreeMap<usize, u32> = BTreeMap::new();
        for i in 0..v.table.slots {
            for ix in v.slot_indices(i) {
                owner.entry(ix).or_insert(i);
            }
        }
        let mut d = Divergence {
            frame: f as u32,
            oam,
            camera,
            ..Divergence::default()
        };
        for &ix in &diff {
            match owner.get(&ix) {
                Some(&i) if slot_on[i as usize] => d.on_screen.push(ix),
                Some(_) => d.off_screen.push(ix),
                None => d.bookkeeping.push(ix),
            }
        }
        // The camera bytes are visible state.
        let cam_idx = [Some(v.cam_lo), v.cam_hi];
        if diff.iter().any(|ix| cam_idx.contains(&Some(*ix))) {
            d.camera = true;
        }
        let visible = oam || d.camera || !d.on_screen.is_empty();
        if visible && t.visible_at.is_none() {
            t.visible_at = Some(f as u32);
        }
        if t.first.is_none() && (!diff.is_empty() || oam || camera) {
            t.first = Some(d);
        }
        if t.first.is_some() && t.visible_at.is_some() {
            break;
        }
    }
    t
}

// ---------------------------------------------------------------------------
// Rows.
// ---------------------------------------------------------------------------

/// Where a ROM offset sits in the atlas: the function that owns the
/// instruction and the index of the byte in that function's instruction-byte
/// stream (the `offset` a patch row carries; [`crate::resolve_patches`] walks
/// it the other way).
#[must_use]
pub fn locate(atlas: &Atlas, rom: u32) -> Option<(u32, u32)> {
    for (fi, f) in atlas.functions.iter().enumerate() {
        let mut insns: Vec<(u32, u32, u8)> = f
            .blocks
            .iter()
            .filter_map(|&b| atlas.blocks.get(b as usize))
            .flat_map(|b| b.insns.iter())
            .map(|i| (i.cpu, i.rom, i.len))
            .collect();
        insns.sort_unstable();
        let mut before = 0u32;
        for (_, r, len) in insns {
            if (r..r + u32::from(len)).contains(&rom) {
                return Some((fi as u32, before + (rom - r)));
            }
            before += u32::from(len);
        }
    }
    None
}

/// A finished candidate: what was found, what the trial said, and the grade a
/// recipe row should carry. Only a lockstep keeper is [`Grade::Confirmed`];
/// everything else is a [`Grade::Candidate`], which no patch ever applies.
#[must_use]
pub fn grade_of(trial: Option<&Trial>) -> Grade {
    match trial {
        Some(t) if t.keeper() => Grade::Confirmed,
        _ => Grade::Candidate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objtable::{Frame, Sprite};

    fn fetch(pc: u32, bytes: &[u8]) -> Vec<BusAccess> {
        bytes
            .iter()
            .enumerate()
            .map(|(i, &value)| BusAccess {
                addr: pc + i as u32,
                value,
                write: false,
            })
            .collect()
    }

    fn read(addr: u32, value: u8) -> BusAccess {
        BusAccess {
            addr,
            value,
            write: false,
        }
    }

    fn write(addr: u32, value: u8) -> BusAccess {
        BusAccess {
            addr,
            value,
            write: true,
        }
    }

    fn nes_reader(watch: Watch) -> WideReader {
        let mut r = WideReader::new(Platform::Nes, watch);
        r.set_resolver(Box::new(|a| a.checked_sub(0x8000)));
        r
    }

    fn feed(r: &mut WideReader, acc: &[BusAccess]) {
        r.step(StepFacts::default(), acc);
    }

    fn watch() -> Watch {
        Watch {
            obj_x: BTreeSet::from([0x87]),
            obj_state: BTreeSet::from([0x0F]),
            cam: BTreeSet::from([0x71C]),
            hud: BTreeSet::new(),
        }
    }

    /// `LDA $87 / SBC #$48 / BCC` then the object is cleared: a despawn site.
    fn despawn_hit(r: &mut WideReader, taken: bool) {
        let mut a = fetch(0x9000, &[0xA5, 0x87]);
        a.push(read(0x87, 0x40));
        feed(r, &a);
        let mut a = fetch(0x9002, &[0xA5, 0x1C]);
        a.push(read(0x071C, 0x10));
        feed(r, &a);
        feed(r, &fetch(0x9004, &[0xE9, 0x48]));
        feed(r, &fetch(0x9006, &[0x90, 0x04]));
        feed(r, &fetch(if taken { 0x900C } else { 0x9008 }, &[0xEA]));
        // The slot is cleared.
        let mut a = fetch(0x9010, &[0x85, 0x0F]);
        a.push(write(0x0F, 0));
        feed(r, &a);
    }

    #[test]
    fn an_immediate_op_after_object_x_and_camera_reads_becomes_a_despawn_candidate() {
        let mut r = nes_reader(watch());
        // The slot is first seen live (a read), then cleared in each hit.
        for k in 0..6 {
            let mut a = fetch(0x9100, &[0xA5, 0x0F]);
            a.push(read(0x0F, 1));
            feed(&mut r, &a);
            despawn_hit(&mut r, k % 2 == 0);
            r.frame_end();
        }
        let facts = r.take_facts();
        let s = &facts.sites[&0x1004];
        assert_eq!((s.op, s.width, s.imm), (ImmOp::Sbc, 1, 0x48));
        assert_eq!(s.hits, 6);
        assert_eq!((s.obj_x, s.cam, s.both), (6, 6, 6));
        assert_eq!((s.taken, s.not_taken), (3, 3));
        assert_eq!(s.flip_down, 6);
        assert_eq!(facts.flips_down, 6);
        let c = infer(&facts);
        let top = &c[0];
        assert_eq!(top.kind, Kind::Despawn);
        assert_eq!((top.site, top.operand), (0x1004, 0x1005));
        assert_eq!(top.band, Band::High, "{top:?}");
    }

    #[test]
    fn a_compare_with_no_screen_relative_reads_is_not_a_candidate_unless_it_is_a_screen_edge() {
        let mut r = nes_reader(watch());
        feed(&mut r, &fetch(0x9000, &[0xC9, 0x07]));
        feed(&mut r, &fetch(0x9002, &[0xC9, 0xF0]));
        let c = infer(&r.take_facts());
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].kind, c[0].band, c[0].imm),
            (Kind::ScreenConst, Band::Low, 0xF0)
        );
    }

    #[test]
    fn a_camera_read_then_write_is_a_clamp() {
        let mut r = nes_reader(watch());
        for _ in 0..6 {
            let mut a = fetch(0x9000, &[0xA5, 0x1C]);
            a.push(read(0x071C, 0x30));
            feed(&mut r, &a);
            feed(&mut r, &fetch(0x9002, &[0xC9, 0x80]));
            let mut a = fetch(0x9004, &[0x85, 0x1C]);
            a.push(write(0x071C, 0x80));
            feed(&mut r, &a);
        }
        let facts = r.take_facts();
        let c = infer(&facts);
        assert_eq!(c[0].kind, Kind::CameraClamp);
        assert_eq!(facts.cam_writes, 6);
    }

    #[test]
    fn a_wide_65816_immediate_has_two_operand_bytes() {
        let mut r = WideReader::new(Platform::Snes, watch());
        r.set_resolver(Box::new(|a| Some(a & 0x7FFF)));
        feed(&mut r, &fetch(0x80_8000, &[0xC9, 0x00, 0x01]));
        let f = r.take_facts();
        let s = f.sites.values().next().unwrap();
        assert_eq!((s.width, s.imm), (2, 0x0100));
    }

    #[test]
    fn widened_constants_stay_in_range_and_above_the_original() {
        let mut c = Candidate {
            kind: Kind::Despawn,
            site: 0,
            operand: 1,
            function: None,
            op: Some(ImmOp::Sbc),
            width: 1,
            imm: 0x48,
            score: 0,
            band: Band::Low,
            evidence: String::new(),
        };
        assert_eq!(widen_values(&c), vec![0xA8, 0xFF]);
        c.imm = 0xFF;
        assert!(widen_values(&c).is_empty(), "already at the byte maximum");
        c.imm = 0xF8;
        assert_eq!(widen_values(&c), vec![0xFF]);
        c.width = 2;
        c.imm = 0x0100;
        assert_eq!(widen_values(&c), vec![0x0180, 0x0280]);
        assert_eq!(operand_bytes(2, 0x0180), vec![0x80, 0x01]);
        c.imm = 0xFFFF;
        assert!(widen_values(&c).is_empty());
    }

    /// A 3-slot table: x lo at 0x10.., x hi at 0x20.., state at 0x30.., camera
    /// lo 0x40 and page 0x41.
    fn view() -> View {
        View {
            ram_base: 0,
            table: ObjectTable {
                base: 0x10,
                slots: 3,
                stride: 1,
                x_lo: Some(0),
                x_hi: Some(0x10),
                state: Some(0x20),
                ..ObjectTable::default()
            },
            cam_lo: 0x40,
            cam_hi: Some(0x41),
            x_page_zero: false,
        }
    }

    fn snap() -> Snap {
        Snap {
            ram: vec![0; 0x50],
            oam: vec![0; 16],
            cam_x: 0,
        }
    }

    /// Slot 1 alive 300 px right of the camera in `patched` only.
    fn runs() -> (Vec<Snap>, Vec<Snap>) {
        let plain: Vec<Snap> = (0..20).map(|_| snap()).collect();
        let mut patched = plain.clone();
        for s in patched.iter_mut().skip(4) {
            s.ram[0x11] = 0x2C; // x lo
            s.ram[0x21] = 0x01; // x hi: 300
            s.ram[0x31] = 1; // state
        }
        (plain, patched)
    }

    #[test]
    fn an_object_alive_only_off_screen_is_a_keeper() {
        let (plain, patched) = runs();
        let t = compare_trial(&view(), &plain, &patched);
        let d = t.first.as_ref().unwrap();
        assert_eq!(d.frame, 4);
        assert_eq!(d.off_screen.len(), 3);
        assert!(d.on_screen.is_empty() && d.bookkeeping.is_empty());
        assert_eq!((t.gain_right, t.gain_left), (300 - VIEW_W, 0));
        assert_eq!(t.visible_at, None);
        assert!(t.keeper(), "{}", t.why_not());
        assert_eq!(grade_of(Some(&t)), Grade::Confirmed);
    }

    #[test]
    fn an_object_kept_for_one_frame_is_not_reach() {
        let (plain, mut patched) = runs();
        for s in patched.iter_mut().skip(5) {
            s.ram[0x31] = 0;
        }
        let t = compare_trial(&view(), &plain, &patched);
        assert_eq!(t.gain_right, 0, "{t:?}");
        assert!(!t.keeper());
        assert_eq!(t.reach_kind(), None);
        let (plain, patched) = runs();
        let t = compare_trial(&view(), &plain, &patched);
        assert_eq!(t.reach_kind(), Some(Kind::Activation));
    }

    #[test]
    fn a_page_column_or_a_zero_page_is_found_when_a_zero_page_puts_slots_off_the_camera() {
        // Camera lo 0x40, page 0x41; slot x lo at 0x10+i, state at 0x20+i,
        // page at 0x30+i. The camera runs 0..1500; each slot sits 100 px
        // right of it.
        let frames: Vec<Frame> = (0..120u32)
            .map(|k| {
                let cam = k * 12;
                let mut ram = vec![0u8; 0x60];
                ram[0x40] = (cam & 0xFF) as u8;
                ram[0x41] = (cam >> 8) as u8;
                for i in 0..4u32 {
                    let x = cam + 100 + i * 8;
                    ram[0x10 + i as usize] = (x & 0xFF) as u8;
                    ram[0x30 + i as usize] = (x >> 8) as u8;
                    ram[0x20 + i as usize] = 1;
                }
                Frame {
                    ram,
                    cam_x: cam as u16,
                    ..Frame::default()
                }
            })
            .collect();
        let t = Trace {
            ram_base: 0,
            wrap: 65536,
            frames,
        };
        let table = ObjectTable {
            base: 0x10,
            slots: 4,
            stride: 1,
            x_lo: Some(0),
            state: Some(0x10),
            ..ObjectTable::default()
        };
        let cam = Camera {
            lo: 0x40,
            hi: Some(0x41),
        };
        assert_eq!(find_x_page(&table, &cam, &t), Some(XPage::Column(0x20)));
        let with_hi = ObjectTable {
            x_hi: Some(0x20),
            ..table
        };
        assert_eq!(find_x_page(&with_hi, &cam, &t), None);
    }

    #[test]
    fn recipe_rows_widen_a_table_but_never_shrink_it() {
        use crate::recipe::RamMeaning;
        let table = ObjectTable {
            base: 0x0e,
            slots: 3,
            stride: 1,
            x_lo: Some(0x78),
            state: Some(0),
            ..ObjectTable::default()
        };
        let row = |addr, width| RamMeaning {
            addr,
            width,
            ..RamMeaning::default()
        };
        // Flags at $0F-$13 run to slot 5 of a table whose slot 0 is $0E.
        assert_eq!(slots_from_rows(&table, &[row(0x0f, 5)]).slots, 6);
        assert_eq!(slots_from_rows(&table, &[row(0x0f, 1)]).slots, 3);
        assert_eq!(slots_from_rows(&table, &[row(0x300, 9)]).slots, 3);
        assert_eq!(slots_from_rows(&table, &[]).slots, 3);
    }

    #[test]
    fn a_change_to_the_picture_or_an_on_screen_slot_is_not_a_keeper() {
        let (plain, mut patched) = runs();
        patched[4].oam[0] = 9;
        let t = compare_trial(&view(), &plain, &patched);
        assert!(!t.keeper());
        assert_eq!(t.visible_at, Some(4));
        // A slot on screen differs.
        let (plain, mut patched) = runs();
        for s in &mut patched {
            s.ram[0x12] = 0x20;
            s.ram[0x32] = 1;
        }
        let t = compare_trial(&view(), &plain, &patched);
        assert!(!t.first.as_ref().unwrap().on_screen.is_empty());
        assert!(!t.keeper());
        assert_eq!(grade_of(Some(&t)), Grade::Candidate);
    }

    #[test]
    fn a_constant_that_never_mattered_stays_a_candidate() {
        let (plain, _) = runs();
        let t = compare_trial(&view(), &plain, &plain);
        assert!(t.first.is_none() && !t.keeper());
        assert_eq!(grade_of(Some(&t)), Grade::Candidate);
        assert_eq!(grade_of(None), Grade::Candidate);
    }

    #[test]
    fn much_other_state_or_no_widening_is_refused() {
        let (plain, mut patched) = runs();
        for s in patched.iter_mut().skip(4) {
            for b in 0x44..0x4E {
                s.ram[b] = 7;
            }
        }
        let t = compare_trial(&view(), &plain, &patched);
        assert!(!t.keeper(), "ten bookkeeping bytes");
        // Off-screen but only 4 px farther out than plain.
        let mut plain2 = plain.clone();
        for s in &mut plain2 {
            s.ram[0x11] = 0x2C;
            s.ram[0x21] = 0x01;
            s.ram[0x31] = 1;
        }
        let mut patched2 = plain2.clone();
        for s in patched2.iter_mut().skip(4) {
            s.ram[0x11] = 0x30;
        }
        let t = compare_trial(&view(), &plain2, &patched2);
        assert!(!t.widened() && !t.keeper());
    }

    #[test]
    fn a_synthetic_trace_yields_the_camera_and_the_shadow() {
        let frames: Vec<Frame> = (0..120u16)
            .map(|k| {
                let cam = k * 7;
                let mut ram = vec![0u8; 0x200];
                ram[0x40] = (cam & 0xFF) as u8;
                ram[0x41] = (cam >> 8) as u8;
                let oam: Vec<u8> = (0..256).map(|i| (i * 3) as u8).collect();
                ram[0x80..0x80 + 256].copy_from_slice(&oam);
                Frame {
                    ram,
                    oam,
                    sprites: vec![
                        Sprite {
                            index: 2,
                            x: 24,
                            y: 16,
                        },
                        Sprite {
                            index: 3,
                            x: (k % 200) as u8,
                            y: 90,
                        },
                    ],
                    cam_x: cam,
                    cam_y: 0,
                }
            })
            .collect();
        let t = Trace {
            ram_base: 0,
            wrap: 65536,
            frames,
        };
        let c = find_camera(&t).unwrap();
        assert_eq!((c.lo, c.hi), (0x40, Some(0x41)));
        let st = static_sprites(&t);
        assert_eq!(st.len(), 1);
        assert_eq!((st[0].index, st[0].x, st[0].y), (2, 24, 16));
        assert_eq!(oam_shadow(&t), Some(0x80));
        let w = hud_watch(&t, 0x80, &st);
        assert_eq!(w, BTreeSet::from([0x88, 0x8B]));
    }
}
