//! The block executor's map (ticket W36-03; `docs/design/REBUILD_LAB.md`
//! section 4).
//!
//! The executor is the interpreter with a map, not a second CPU. A console
//! core asks [`RebuildExec::dispatch`] where the next instruction sits in
//! the ROM image; the answer says whether the atlas knows it (and whether
//! it is an indirect transfer worth checking afterwards), then the core runs
//! the instruction through its own cycle-exact code. This module holds no
//! instruction semantics: no ALU, no flags, no bus.
//!
//! Interrupt deadlines are checked at every instruction boundary because the
//! executor never runs two instructions between the core's own polls: the
//! core calls `dispatch` once, steps once, calls
//! [`RebuildExec::check_target`] once.
//!
//! ## Modes (ticket W36-08b)
//!
//! A node is keyed `(ROM offset, mode)`: the same bytes entered in two
//! 65816 widths are two instructions. `slot` holds, per ROM offset, the head
//! of a short chain of the nodes at that offset (at most `mode_count`, so at
//! most five), which keeps the table `4 * prg_len` bytes whatever the ISA.
//! A lookup that finds the offset but not the mode is a **width mismatch**:
//! the core still runs its own instruction, the executor only counts it
//! ([`RebuildStats::fallback_width`]). The 6502 has one mode (0).
//!
//! Everything is looked up in tables built once at install ([`RebuildExec::new`]);
//! the per-instruction path allocates nothing. Only a new dispatch miss
//! allocates (deduplicated, bounded by [`MAX_DELTAS`]).
//!
//! ## Function hooks (ticket W36-05)
//!
//! With a hooked-entry set installed ([`RebuildExec::set_hooked`]) the core
//! also tells the executor where a call, interrupt or return landed
//! ([`RebuildExec::landed`], [`RebuildExec::returned`]) and drains the
//! resulting [`FunctionEvent`]s. Enter fires when a call-kind step lands on
//! the entry node of a hooked function. Exit does **not** mean "an RTS ran
//! inside F": a shadow stack of the *hooked* frames remembers where each is
//! expected to return (the instruction after its `JSR`, or the interrupted
//! `pc` for an interrupt entry), and a return step that lands on some
//! frame's expected address pops down to that frame, reporting Exit
//! innermost first. A return that lands anywhere else (an RTS trick, a
//! tail call that left the frame) leaves the stack alone. Only hooked
//! frames are tracked, so an unhooked function costs the one bitset load at
//! the landing, and with no hooks nothing is tracked at all (the core skips
//! the calls). The stack is bounded ([`MAX_SHADOW_DEPTH`], oldest dropped
//! and counted) and cleared by [`RebuildExec::reset_frames`].

use std::collections::{HashMap, HashSet};

pub use rf_core_api::FunctionEvent;
use rf_core_api::{CodeAddr, RebuildStats};

use crate::atlas::{Atlas, Provenance, Target};
use crate::isa::Flow;

/// Most distinct (site, target) misses kept for later consumers.
pub const MAX_DELTAS: usize = 4096;
/// Deepest shadow call stack of hooked frames; past it the oldest frame is
/// dropped (and counted in [`RebuildExec::shadow_dropped`]).
pub const MAX_SHADOW_DEPTH: usize = 64;

const NO_NODE: u32 = u32::MAX;
/// Trace-ring marker: the step was an NMI/IRQ sequence, not an instruction.
const TRACE_INTERRUPT: u32 = u32::MAX - 1;
/// Trace-ring marker: the offset is in the atlas, but not in this mode.
const TRACE_WIDTH: u32 = u32::MAX - 2;
const NO_SITE: u32 = u32::MAX;
/// Set on a target key that is a CPU address (outside PRG ROM) rather than a
/// `(ROM offset, mode)` pair.
const CPU_KEY: u64 = 1 << 63;

/// Key of a recorded or observed landing in PRG ROM.
fn rom_key(rom: u32, mode: u8) -> u64 {
    (u64::from(rom) << 8) | u64::from(mode)
}

/// Key of a landing outside PRG ROM: the full 24-bit CPU address.
fn cpu_key(pbr: u8, cpu: u32) -> u64 {
    CPU_KEY | (u64::from(pbr) << 16) | u64::from(cpu & 0xFF_FFFF)
}

/// Where the next instruction is fetched from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// Cartridge ROM at this offset.
    Rom(u32),
    /// RAM (NES: RAM or registers; SNES: work RAM). Volatile code.
    Ram,
    /// Memory a coprocessor owns, or anything else that is not ROM or RAM.
    Other,
}

/// A recorded indirect transfer the atlas did not know about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Delta {
    pub site: CodeAddr,
    pub target: CodeAddr,
}

/// One executed step as the trace ring remembers it (ticket W36-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceEntry {
    /// CPU address the step started at (the interrupted `pc` for an
    /// interrupt entry).
    pub cpu: u16,
    /// Program bank (always 0 on the 6502).
    pub bank: u8,
    /// Decode mode in force (always 0 on the 6502).
    pub mode: u8,
    pub kind: TraceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceKind {
    /// In the atlas; `function` is the index of the first function owning
    /// the block, `None` when no function does.
    Atlas {
        function: Option<u32>,
        prov: Provenance,
    },
    /// No atlas node: plain interpreter.
    Fallback,
    /// The offset is in the atlas, but only in other decode modes: plain
    /// interpreter.
    WidthMismatch,
    /// An NMI/IRQ sequence ran instead of the instruction at `cpu`.
    Interrupt,
}

/// What the atlas says about one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    /// Index into [`Atlas::blocks`].
    pub block: u32,
    /// Index into [`Atlas::functions`] of the first function owning the
    /// block, or `u32::MAX` when none does.
    pub function: u32,
    pub prov: Provenance,
    /// An indirect transfer: its new `pc` is compared with the recorded set.
    pub indirect: bool,
    /// The function whose entry is this instruction, `u32::MAX` when none
    /// (ticket W36-05: where a hooked call lands).
    pub entry_of: u32,
    /// Decode mode the node was decoded in.
    pub mode: u8,
    site: u32,
    /// Next node at the same ROM offset (another mode), `NO_NODE` at the end.
    next: u32,
}

/// One hooked call in flight: the function and where it should return to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    function: u32,
    ret: u32,
}

/// The answer to "what is at this ROM offset".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// No node (RAM, unmapped, never decoded): plain interpreter.
    Fallback,
    /// The offset has nodes, but not for this decode mode: plain
    /// interpreter, counted apart.
    WidthMismatch,
    /// Found; `indirect` says the caller should call
    /// [`RebuildExec::check_target`] after the step.
    Atlas { node: u32, indirect: bool },
}

/// Ticket W37-07: the first block after a state load runs on the plain
/// interpreter, then dispatch re-enters the atlas (`REBUILD_LAB.md`,
/// "W37-07").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resume {
    /// Normal dispatch.
    Off,
    /// A state was just loaded: the next instruction is the first of the
    /// resumed block.
    Armed,
    /// Inside the atlas block (index) the load landed in; still on the
    /// interpreter until a block leader or a node of another block.
    InBlock(u32),
}

pub struct RebuildExec {
    atlas: Atlas,
    /// PRG offset to the first node at that offset, `NO_NODE` when unknown;
    /// the rest are chained through [`Node::next`].
    slot: Vec<u32>,
    nodes: Vec<Node>,
    /// Per indirect site, sorted unique target keys.
    site_targets: Vec<Vec<u64>>,
    /// Ticket W36-05: bitset over function ids of the hooked entries.
    hooked: Vec<u64>,
    hooked_count: u32,
    /// Ticket W37-04: bitset over function ids the shadow-core check has
    /// demoted. A node of a demoted function dispatches as
    /// [`Dispatch::Fallback`], so the plain interpreter runs it.
    demoted: Vec<u64>,
    demoted_count: u32,
    /// Hooked calls in flight, outermost first.
    frames: Vec<Frame>,
    frames_dropped: u64,
    /// Ticket W37-07: where the post-load resume rule stands.
    resume: Resume,
    events: Vec<FunctionEvent>,
    stats: RebuildStats,
    deltas: Vec<Delta>,
    seen: HashSet<Delta>,
    /// Ticket W36-04: ring of the last executed steps, empty (and untouched
    /// by the per-instruction path beyond one `is_empty`) until
    /// [`RebuildExec::enable_trace`]. `(cpu, node)`, power-of-two length.
    /// `(bank << 16 | pc | mode << 24, node)`.
    trace: Vec<(u32, u32)>,
    trace_next: usize,
    trace_total: u64,
}

impl RebuildExec {
    /// Index `atlas` for per-instruction lookup.
    #[must_use]
    pub fn new(atlas: Atlas) -> Self {
        let mut function_of = vec![u32::MAX; atlas.blocks.len()];
        for (fi, f) in atlas.functions.iter().enumerate() {
            for &b in &f.blocks {
                if let Some(slot) = function_of.get_mut(b as usize) {
                    if *slot == u32::MAX {
                        *slot = fi as u32;
                    }
                }
            }
        }
        let site_targets: Vec<Vec<u64>> = atlas
            .indirect_sites
            .iter()
            .map(|s| {
                let mut keys: Vec<u64> = s.targets.iter().map(target_key).collect();
                keys.sort_unstable();
                keys.dedup();
                keys
            })
            .collect();
        let site_of = |rom: u32| {
            atlas
                .indirect_sites
                .binary_search_by_key(&rom, |s| s.site_rom)
                .map_or(NO_SITE, |i| i as u32)
        };
        let mut entry_fn: HashMap<(u32, u8), u32> = HashMap::with_capacity(atlas.functions.len());
        for (fi, f) in atlas.functions.iter().enumerate() {
            entry_fn
                .entry((f.entry_rom, f.entry_mode))
                .or_insert(fi as u32);
        }
        let mut slot = vec![NO_NODE; atlas.prg_len as usize];
        let mut nodes: Vec<Node> = Vec::new();
        for (bi, block) in atlas.blocks.iter().enumerate() {
            for insn in &block.insns {
                let Some(head) = slot.get(insn.rom as usize).copied() else {
                    continue;
                };
                // One node per (offset, mode): walk the (at most
                // `mode_count`) nodes already at this offset.
                let mut at = head;
                let mut have = false;
                // Terminates: `at` moves to `next` on every pass, chains are
                // built by prepending so they are acyclic, and `NO_NODE` is
                // past the end of `nodes`.
                while let Some(n) = nodes.get(at as usize) {
                    if n.mode == insn.mode {
                        have = true;
                        break;
                    }
                    at = n.next;
                }
                if have {
                    continue;
                }
                slot[insn.rom as usize] = nodes.len() as u32;
                nodes.push(Node {
                    block: bi as u32,
                    function: function_of[bi],
                    prov: insn.prov,
                    indirect: matches!(
                        insn.flow,
                        Flow::IndirectJump
                            | Flow::IndirectCall
                            | Flow::Return
                            | Flow::ReturnLong
                            | Flow::ReturnInterrupt
                            | Flow::Break
                    ),
                    entry_of: entry_fn
                        .get(&(insn.rom, insn.mode))
                        .copied()
                        .unwrap_or(u32::MAX),
                    mode: insn.mode,
                    site: site_of(insn.rom),
                    next: head,
                });
            }
        }
        let stats = RebuildStats {
            atlas_functions: atlas.functions.len() as u32,
            ..RebuildStats::default()
        };
        Self {
            atlas,
            slot,
            nodes,
            site_targets,
            hooked: Vec::new(),
            hooked_count: 0,
            demoted: Vec::new(),
            demoted_count: 0,
            frames: Vec::new(),
            frames_dropped: 0,
            resume: Resume::Off,
            events: Vec::new(),
            stats,
            deltas: Vec::with_capacity(MAX_DELTAS),
            seen: HashSet::new(),
            trace: Vec::new(),
            trace_next: 0,
            trace_total: 0,
        }
    }

    /// Keep the last `capacity` steps (rounded up to a power of two; `0`
    /// turns the trace off and clears it).
    pub fn enable_trace(&mut self, capacity: usize) {
        self.trace_next = 0;
        self.trace_total = 0;
        self.trace = if capacity == 0 {
            Vec::new()
        } else {
            vec![(0, NO_NODE); capacity.next_power_of_two()]
        };
    }

    #[inline]
    fn trace_push(&mut self, bank: u8, cpu: u16, mode: u8, node: u32) {
        let mask = self.trace.len().wrapping_sub(1);
        if let Some(slot) = self.trace.get_mut(self.trace_next & mask) {
            *slot = (
                (u32::from(mode) << 24) | (u32::from(bank) << 16) | u32::from(cpu),
                node,
            );
            self.trace_next = (self.trace_next + 1) & mask;
            self.trace_total += 1;
        }
    }

    /// The last executed steps, oldest first (empty when the trace is off).
    #[must_use]
    pub fn trace(&self) -> Vec<TraceEntry> {
        let len = self.trace.len();
        let kept = usize::try_from(self.trace_total).map_or(len, |t| t.min(len));
        // `trace_next` is the oldest slot once the ring has wrapped, 0 before.
        let start = if kept == len { self.trace_next } else { 0 };
        (0..kept)
            .filter_map(|i| self.trace.get((start + i) % len.max(1)))
            .map(|&(packed, node)| TraceEntry {
                cpu: packed as u16,
                bank: (packed >> 16) as u8,
                mode: (packed >> 24) as u8,
                kind: match node {
                    NO_NODE => TraceKind::Fallback,
                    TRACE_INTERRUPT => TraceKind::Interrupt,
                    TRACE_WIDTH => TraceKind::WidthMismatch,
                    n => self
                        .nodes
                        .get(n as usize)
                        .map_or(TraceKind::Fallback, |nd| TraceKind::Atlas {
                            function: (nd.function != u32::MAX).then_some(nd.function),
                            prov: nd.prov,
                        }),
                },
            })
            .collect()
    }

    /// Whether the trace ring is on.
    #[must_use]
    pub fn trace_enabled(&self) -> bool {
        !self.trace.is_empty()
    }

    /// What the most recent step was, from the trace ring (ticket W36-09:
    /// the understand pass attributes bus accesses to the function this
    /// names). `None` when the trace is off or empty. Compare
    /// [`Self::trace_total`] before and after a step to know whether it
    /// pushed one.
    #[must_use]
    pub fn last_step(&self) -> Option<rf_core_api::StepFacts> {
        if self.trace_total == 0 {
            return None;
        }
        let mask = self.trace.len().checked_sub(1)?;
        let (_, node) = *self.trace.get(self.trace_next.wrapping_sub(1) & mask)?;
        let mut facts = rf_core_api::StepFacts::default();
        match node {
            TRACE_INTERRUPT => facts.interrupt = true,
            NO_NODE | TRACE_WIDTH => {}
            n => {
                if let Some(nd) = self.nodes.get(n as usize) {
                    facts.function = (nd.function != u32::MAX).then_some(nd.function);
                    facts.entry_of = (nd.entry_of != u32::MAX).then_some(nd.entry_of);
                }
            }
        }
        Some(facts)
    }

    /// Steps executed since the trace was enabled (not bounded by capacity).
    #[must_use]
    pub fn trace_total(&self) -> u64 {
        self.trace_total
    }

    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// The node for `(rom, mode)`, if the atlas has one.
    fn node_at(&self, rom: u32, mode: u8) -> Option<&Node> {
        let mut at = self.slot.get(rom as usize).copied()?;
        // Terminates: see `new`.
        while let Some(nd) = self.nodes.get(at as usize) {
            if nd.mode == mode {
                return Some(nd);
            }
            at = nd.next;
        }
        None
    }

    /// The node record for a [`Dispatch::Atlas`] answer.
    #[must_use]
    pub fn node(&self, node: u32) -> Option<&Node> {
        self.nodes.get(node as usize)
    }

    /// Look up the instruction about to run at CPU address `pc` (6502: one
    /// mode, bank 0), at PRG offset `rom` (`None` when the CPU address is not
    /// PRG ROM right now), and count it.
    #[inline]
    pub fn dispatch(&mut self, pc: u16, rom: Option<u32>) -> Dispatch {
        self.dispatch_at(0, pc, rom.map_or(Fetch::Ram, Fetch::Rom), 0)
    }

    /// [`Self::dispatch`] for an ISA with banks and decode modes: the
    /// instruction at `bank:pc`, fetched from `fetch`, with the decode mode
    /// `mode` in force. A ROM offset the atlas knows only in other modes is
    /// [`Dispatch::WidthMismatch`].
    #[inline]
    pub fn dispatch_at(&mut self, bank: u8, pc: u16, fetch: Fetch, mode: u8) -> Dispatch {
        let mut mismatch = false;
        let node = match fetch {
            Fetch::Rom(rom) => {
                let mut at = self.slot.get(rom as usize).copied().unwrap_or(NO_NODE);
                let mut found = None;
                // Terminates: see `new` (acyclic chain, `NO_NODE` is out of range).
                while let Some(nd) = self.nodes.get(at as usize) {
                    if nd.mode == mode {
                        found = Some((at, *nd));
                        break;
                    }
                    mismatch = true;
                    at = nd.next;
                }
                found
            }
            Fetch::Ram | Fetch::Other => None,
        };
        if !self.trace.is_empty() {
            let marker = match node {
                Some((n, _)) => n,
                None if mismatch => TRACE_WIDTH,
                None => NO_NODE,
            };
            self.trace_push(bank, pc, mode, marker);
        }
        // Ticket W37-07: one compare per instruction while no load is pending.
        if self.resume != Resume::Off {
            let rom = match fetch {
                Fetch::Rom(r) => Some(r),
                Fetch::Ram | Fetch::Other => None,
            };
            if self.resume_step(node.map(|(_, nd)| (nd.block, nd.mode)), rom) {
                self.stats.fallback += 1;
                self.stats.fallback_after_load += 1;
                return Dispatch::Fallback;
            }
        }
        // Ticket W37-04: one bitset load per atlas hit, and nothing at all
        // while no function is demoted. The trace above still names the
        // function (it records what the atlas knew); only the answer changes.
        let demoted = self.demoted_count > 0
            && node.is_some_and(|(_, nd)| nd.function != u32::MAX && self.is_demoted(nd.function));
        if demoted {
            self.stats.fallback += 1;
            self.stats.fallback_demoted += 1;
            return Dispatch::Fallback;
        }
        match node {
            Some((n, nd)) => {
                match nd.prov {
                    Provenance::Observed => self.stats.observed += 1,
                    Provenance::Inferred => self.stats.inferred += 1,
                }
                Dispatch::Atlas {
                    node: n,
                    indirect: nd.indirect,
                }
            }
            None => {
                self.stats.fallback += 1;
                if mismatch {
                    self.stats.fallback_width += 1;
                    Dispatch::WidthMismatch
                } else {
                    match fetch {
                        Fetch::Rom(_) => self.stats.fallback_no_node += 1,
                        Fetch::Ram => self.stats.fallback_ram += 1,
                        Fetch::Other => self.stats.fallback_other += 1,
                    }
                    Dispatch::Fallback
                }
            }
        }
    }

    /// An NMI/IRQ sequence ran instead of an instruction.
    #[inline]
    pub fn note_interrupt(&mut self, pc: u16) {
        self.note_interrupt_at(0, pc, 0);
    }

    /// [`Self::note_interrupt`] with the interrupted bank and mode.
    #[inline]
    pub fn note_interrupt_at(&mut self, bank: u8, pc: u16, mode: u8) {
        self.stats.interrupt_entries += 1;
        // The vector's handler starts a new block.
        self.resume = Resume::Off;
        if !self.trace.is_empty() {
            self.trace_push(bank, pc, mode, TRACE_INTERRUPT);
        }
    }

    /// After an indirect transfer at `site` (the `node` dispatch returned),
    /// compare where it landed with the atlas's recorded targets. A miss is
    /// counted and appended to the delta list; it never changes behaviour.
    pub fn check_target(&mut self, node: u32, site: CodeAddr, target: CodeAddr) {
        self.stats.indirect_checked += 1;
        let key = target.rom.map_or_else(
            || cpu_key(target.pbr, u32::from(target.cpu)),
            |rom| rom_key(rom, target.mode),
        );
        let known = self
            .nodes
            .get(node as usize)
            .and_then(|n| self.site_targets.get(n.site as usize))
            .is_some_and(|t| t.binary_search(&key).is_ok());
        if known {
            return;
        }
        self.stats.dispatch_misses += 1;
        let delta = Delta { site, target };
        if self.seen.contains(&delta) {
            return;
        }
        if self.deltas.len() >= MAX_DELTAS {
            self.stats.delta_dropped += 1;
            return;
        }
        self.seen.insert(delta);
        self.deltas.push(delta);
        self.stats.delta_len = self.deltas.len() as u32;
    }

    /// Choose the functions whose entry and exit emit events (`ids` index
    /// [`Atlas::functions`]; out-of-range ids are ignored). Replaces any
    /// earlier set and clears the shadow stack. Returns how many ids took.
    pub fn set_hooked(&mut self, ids: &[u32]) -> usize {
        let n = self.atlas.functions.len();
        self.hooked = vec![0; n.div_ceil(64)];
        self.hooked_count = 0;
        for &id in ids.iter().filter(|&&id| (id as usize) < n) {
            let (w, b) = (id as usize / 64, id % 64);
            if self.hooked[w] & (1 << b) == 0 {
                self.hooked[w] |= 1 << b;
                self.hooked_count += 1;
            }
        }
        self.reset_frames();
        self.hooked_count as usize
    }

    /// Whether any function is hooked; the core skips all hook bookkeeping
    /// when this is `false`.
    #[inline]
    #[must_use]
    pub fn hooks_active(&self) -> bool {
        self.hooked_count > 0
    }

    /// Ticket W37-04: choose the functions the plain interpreter runs instead
    /// (`ids` index [`Atlas::functions`]; out-of-range ids are ignored).
    /// Replaces any earlier set. Returns how many ids took.
    pub fn set_demoted(&mut self, ids: &[u32]) -> usize {
        let n = self.atlas.functions.len();
        self.demoted = vec![0; n.div_ceil(64)];
        self.demoted_count = 0;
        for &id in ids.iter().filter(|&&id| (id as usize) < n) {
            let (w, b) = (id as usize / 64, id % 64);
            if self.demoted[w] & (1 << b) == 0 {
                self.demoted[w] |= 1 << b;
                self.demoted_count += 1;
            }
        }
        self.demoted_count as usize
    }

    #[inline]
    fn is_demoted(&self, function: u32) -> bool {
        self.demoted
            .get(function as usize / 64)
            .is_some_and(|w| w & (1 << (function % 64)) != 0)
    }

    #[inline]
    fn is_hooked(&self, function: u32) -> bool {
        self.hooked
            .get(function as usize / 64)
            .is_some_and(|w| w & (1 << (function % 64)) != 0)
    }

    /// Forget every in-flight frame (reset, cartridge load, state load).
    pub fn reset_frames(&mut self) {
        self.frames.clear();
        self.events.clear();
        self.resume = Resume::Off;
    }

    /// A savestate was just loaded into the machine (ticket W37-07). The
    /// executor's bookkeeping describes the machine that was replaced, so:
    /// in-flight hooked frames and undelivered events are dropped (an Exit
    /// for a call entered before the load is never reported), the trace ring
    /// is cleared (it would splice two timelines), and the next block runs on
    /// the interpreter ([`RebuildStats::fallback_after_load`]) before
    /// dispatch re-enters the atlas. Counters, deltas, the hooked and demoted
    /// sets are session settings and stay.
    pub fn state_loaded(&mut self) {
        self.reset_frames();
        self.resume = Resume::Armed;
        self.trace_next = 0;
        self.trace_total = 0;
    }

    /// Does this instruction still belong to the resumed block? `node` is
    /// the atlas node's `(block, mode)`, `rom` its offset when fetched from
    /// ROM. Disarms on the first instruction that is not.
    ///
    /// Terminates: every call either leaves [`Resume::InBlock`] or stays in
    /// it only for a node that is not its block's first instruction, and a
    /// block is a finite run of such nodes (a jump into one is a leader by
    /// construction).
    fn resume_step(&mut self, node: Option<(u32, u8)>, rom: Option<u32>) -> bool {
        match (self.resume, node) {
            (Resume::Off, _) => false,
            (Resume::Armed, found) => {
                // The first instruction after a load always takes the
                // interpreter, in the atlas or not.
                self.resume = found.map_or(Resume::Off, |(b, _)| Resume::InBlock(b));
                true
            }
            (Resume::InBlock(b), Some((nb, mode))) if nb == b => {
                let leader = self
                    .atlas
                    .blocks
                    .get(b as usize)
                    .is_some_and(|blk| Some(blk.start_rom) == rom && blk.start_mode == mode);
                if leader {
                    self.resume = Resume::Off;
                }
                !leader
            }
            (Resume::InBlock(_), _) => {
                self.resume = Resume::Off;
                false
            }
        }
    }

    /// A call, `BRK` or interrupt entry just ran and control is now at PRG
    /// offset `rom` (`None`: not PRG ROM). `ret` is where the matching
    /// return should land. If that is the entry of a hooked function, push
    /// its frame and queue Enter.
    #[inline]
    pub fn landed(&mut self, rom: Option<u32>, ret: u32) {
        self.landed_at(rom, 0, ret);
    }

    /// [`Self::landed`] with the decode mode the landing is entered in (a
    /// function's identity is its entry offset *and* mode). `ret` is the
    /// full address the return should reach (65816: `pbr << 16 | pc`).
    #[inline]
    pub fn landed_at(&mut self, rom: Option<u32>, mode: u8, ret: u32) {
        let Some(function) = rom
            .and_then(|r| self.node_at(r, mode))
            .map(|nd| nd.entry_of)
            .filter(|&f| f != u32::MAX && self.is_hooked(f))
        else {
            return;
        };
        if self.frames.len() >= MAX_SHADOW_DEPTH {
            self.frames.remove(0);
            self.frames_dropped += 1;
        }
        self.frames.push(Frame { function, ret });
        self.events.push(FunctionEvent::Enter { function });
    }

    /// A return step (`RTS`/`RTI`) just ran and control is at `pc`. If some
    /// frame expected exactly this address, pop down to the innermost such
    /// frame and queue Exit for each popped frame, innermost first.
    #[inline]
    pub fn returned(&mut self, pc: u32) {
        let Some(at) = self.frames.iter().rposition(|f| f.ret == pc) else {
            return;
        };
        while self.frames.len() > at {
            if let Some(f) = self.frames.pop() {
                self.events.push(FunctionEvent::Exit {
                    function: f.function,
                });
            }
        }
    }

    /// Take the queued events (oldest first). Give the buffer back with
    /// [`RebuildExec::recycle_events`] to keep the hot path allocation-free.
    pub fn take_events(&mut self) -> Vec<FunctionEvent> {
        std::mem::take(&mut self.events)
    }

    /// Return the (emptied) buffer from [`RebuildExec::take_events`].
    pub fn recycle_events(&mut self, mut buf: Vec<FunctionEvent>) {
        buf.clear();
        if self.events.is_empty() {
            self.events = buf;
        }
    }

    /// Hooked calls currently in flight.
    #[must_use]
    pub fn shadow_depth(&self) -> usize {
        self.frames.len()
    }

    /// Frames dropped because the shadow stack was full.
    #[must_use]
    pub fn shadow_dropped(&self) -> u64 {
        self.frames_dropped
    }

    #[must_use]
    pub fn stats(&self) -> RebuildStats {
        self.stats
    }

    /// Distinct misses so far, in first-seen order.
    #[must_use]
    pub fn deltas(&self) -> &[Delta] {
        &self.deltas
    }
}

fn target_key(t: &Target) -> u64 {
    match *t {
        Target::Rom { rom, mode, .. } => rom_key(rom, mode),
        Target::Unresolved { cpu, .. } => cpu_key((cpu >> 16) as u8, cpu),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::{Block, Function, IndirectSite, InsnRec, SiteKind, Stats};

    fn insn(rom: u32, flow: Flow, prov: Provenance) -> InsnRec {
        InsnRec {
            rom,
            cpu: 0x8000 + rom,
            len: 1,
            flow,
            prov,
            mode: 0,
        }
    }

    fn atlas() -> Atlas {
        Atlas {
            prg_len: 0x20,
            mode_count: 1,
            blocks: vec![Block {
                start_rom: 0,
                start_cpu: 0x8000,
                start_mode: 0,
                insns: vec![
                    insn(0, Flow::Fall, Provenance::Observed),
                    insn(1, Flow::Fall, Provenance::Inferred),
                    insn(2, Flow::Return, Provenance::Observed),
                ],
                edges: vec![],
            }],
            functions: vec![Function {
                entry_rom: 0,
                entry_cpu: 0x8000,
                entry_mode: 0,
                entry_kinds: 0,
                blocks: vec![0],
                insn_count: 3,
                byte_len: 3,
                byte_hash: 0,
                shape_hash: 0,
            }],
            indirect_sites: vec![IndirectSite {
                site_rom: 2,
                site_cpu: 0x8002,
                kind: SiteKind::Rts,
                targets: vec![
                    Target::Rom {
                        cpu: 0x8000,
                        rom: 0,
                        mode: 0,
                    },
                    Target::Unresolved {
                        cpu: 0x0300,
                        why: crate::atlas::Why::NotRom,
                    },
                ],
            }],
            volatile: vec![],
            stats: Stats::default(),
        }
    }

    fn addr(cpu: u16, rom: Option<u32>) -> CodeAddr {
        CodeAddr {
            cpu,
            pbr: 0,
            mode: 0,
            rom,
        }
    }

    #[test]
    fn dispatch_counts_observed_inferred_and_fallback() {
        let mut x = RebuildExec::new(atlas());
        assert!(matches!(
            x.dispatch(0x8000, Some(0)),
            Dispatch::Atlas {
                indirect: false,
                ..
            }
        ));
        assert!(matches!(
            x.dispatch(0x8000, Some(1)),
            Dispatch::Atlas { .. }
        ));
        assert_eq!(x.dispatch(0x8000, Some(5)), Dispatch::Fallback);
        assert_eq!(x.dispatch(0x8000, None), Dispatch::Fallback);
        assert_eq!(
            x.dispatch(0x8000, Some(0x1000)),
            Dispatch::Fallback,
            "past the image"
        );
        let s = x.stats();
        assert_eq!((s.observed, s.inferred, s.fallback), (1, 1, 3));
        assert_eq!(s.atlas_functions, 1);
        assert!((s.fallback_rate() - 0.6).abs() < 1e-9);
    }

    #[test]
    fn recorded_targets_hit_and_new_ones_miss_once_in_the_delta_list() {
        let mut x = RebuildExec::new(atlas());
        let Dispatch::Atlas { node, indirect } = x.dispatch(0x8000, Some(2)) else {
            panic!("site is in the atlas");
        };
        assert!(indirect);
        let site = addr(0x8002, Some(2));
        x.check_target(node, site, addr(0x8000, Some(0)));
        x.check_target(node, site, addr(0x0300, None));
        assert_eq!(x.stats().dispatch_misses, 0);
        for _ in 0..3 {
            x.check_target(node, site, addr(0x8001, Some(1)));
        }
        let s = x.stats();
        assert_eq!(
            (s.indirect_checked, s.dispatch_misses, s.delta_len),
            (5, 3, 1)
        );
        assert_eq!(
            x.deltas(),
            &[Delta {
                site,
                target: addr(0x8001, Some(1))
            }]
        );
    }

    #[test]
    fn the_trace_ring_keeps_the_last_steps_with_function_ids() {
        let mut x = RebuildExec::new(atlas());
        assert!(x.trace().is_empty(), "off until enabled");
        x.dispatch(0x8000, Some(0));
        assert_eq!(x.trace_total(), 0);
        x.enable_trace(3); // rounds up to 4
        x.dispatch(0x8000, Some(0));
        x.dispatch(0x8001, Some(1));
        x.note_interrupt(0x8002);
        x.dispatch(0x0300, None);
        let t = x.trace();
        assert_eq!(t.len(), 4);
        assert_eq!(t[0].cpu, 0x8000);
        assert!(matches!(
            t[0].kind,
            TraceKind::Atlas {
                function: Some(0),
                prov: Provenance::Observed
            }
        ));
        assert_eq!(t[2].kind, TraceKind::Interrupt);
        assert_eq!(t[3].kind, TraceKind::Fallback);
        // Wrap: two more steps push the two oldest out, order stays oldest first.
        x.dispatch(0x8002, Some(2));
        x.dispatch(0x9000, None);
        let t = x.trace();
        assert_eq!(
            t.iter().map(|e| e.cpu).collect::<Vec<_>>(),
            [0x8002, 0x0300, 0x8002, 0x9000]
        );
        assert_eq!(x.trace_total(), 6);
        x.enable_trace(0);
        assert!(x.trace().is_empty());
    }

    /// Four functions with entries at ROM 0x10, 0x20, 0x30, 0x40; each is a
    /// single block with two instructions.
    fn hook_atlas() -> Atlas {
        let mut blocks = Vec::new();
        let mut functions = Vec::new();
        for (i, entry) in [0x10u32, 0x20, 0x30, 0x40].into_iter().enumerate() {
            blocks.push(Block {
                start_rom: entry,
                start_cpu: 0x8000 + entry,
                start_mode: 0,
                insns: vec![
                    insn(entry, Flow::Fall, Provenance::Observed),
                    insn(entry + 1, Flow::Return, Provenance::Observed),
                ],
                edges: vec![],
            });
            functions.push(Function {
                entry_rom: entry,
                entry_cpu: 0x8000 + entry,
                entry_mode: 0,
                entry_kinds: 0,
                blocks: vec![i as u32],
                insn_count: 2,
                byte_len: 2,
                byte_hash: 0,
                shape_hash: 0,
            });
        }
        Atlas {
            prg_len: 0x100,
            mode_count: 1,
            blocks,
            functions,
            indirect_sites: vec![],
            volatile: vec![],
            stats: Stats::default(),
        }
    }

    fn drain(x: &mut RebuildExec) -> Vec<FunctionEvent> {
        let mut v = x.take_events();
        let out = v.clone();
        v.clear();
        x.recycle_events(v);
        out
    }

    fn enter(function: u32) -> FunctionEvent {
        FunctionEvent::Enter { function }
    }

    fn exit(function: u32) -> FunctionEvent {
        FunctionEvent::Exit { function }
    }

    #[test]
    fn nested_calls_enter_and_exit_innermost_first() {
        let mut x = RebuildExec::new(hook_atlas());
        assert_eq!(x.set_hooked(&[0, 1, 2]), 3);
        assert!(x.hooks_active());
        x.landed(Some(0x10), 0x9000);
        x.landed(Some(0x20), 0x9100);
        x.landed(Some(0x30), 0x9200);
        assert_eq!(drain(&mut x), [enter(0), enter(1), enter(2)]);
        assert_eq!(x.shadow_depth(), 3);
        // The innermost returns to its caller.
        x.returned(0x9200);
        assert_eq!(drain(&mut x), [exit(2)]);
        // One return that lands at the outermost frame's address pops the
        // frames in between too, innermost first.
        x.landed(Some(0x30), 0x9300);
        drain(&mut x);
        x.returned(0x9000);
        assert_eq!(drain(&mut x), [exit(2), exit(1), exit(0)]);
        assert_eq!(x.shadow_depth(), 0);
    }

    #[test]
    fn an_interrupt_mid_function_returns_to_the_interrupted_pc_not_the_callers() {
        let mut x = RebuildExec::new(hook_atlas());
        x.set_hooked(&[0, 3]);
        x.landed(Some(0x10), 0x9000); // F0 called
        drain(&mut x);
        // NMI arrives while F0 runs at 0x8123; its handler is function 3.
        x.landed(Some(0x40), 0x8123);
        assert_eq!(drain(&mut x), [enter(3)]);
        // The handler's RTI lands on the interrupted pc: only it is popped.
        x.returned(0x8123);
        assert_eq!(drain(&mut x), [exit(3)]);
        assert_eq!(x.shadow_depth(), 1, "F0 is still in flight");
        x.returned(0x9000);
        assert_eq!(drain(&mut x), [exit(0)]);
    }

    #[test]
    fn a_return_to_an_unexpected_address_leaves_the_stack_alone() {
        let mut x = RebuildExec::new(hook_atlas());
        x.set_hooked(&[0]);
        x.landed(Some(0x10), 0x9000);
        drain(&mut x);
        // An RTS trick / tail call: the return lands somewhere no frame
        // expected.
        x.returned(0x8777);
        assert!(drain(&mut x).is_empty());
        assert_eq!(x.shadow_depth(), 1);
        // An RTS executed inside F that lands at F's caller still exits F.
        x.returned(0x9000);
        assert_eq!(drain(&mut x), [exit(0)]);
    }

    #[test]
    fn the_shadow_stack_is_bounded_and_counts_what_it_drops() {
        let mut x = RebuildExec::new(hook_atlas());
        x.set_hooked(&[0]);
        for i in 0..(MAX_SHADOW_DEPTH as u32 + 5) {
            x.landed(Some(0x10), 0x9000 + i);
        }
        assert_eq!(x.shadow_depth(), MAX_SHADOW_DEPTH);
        assert_eq!(x.shadow_dropped(), 5);
        // The newest frames survive: the oldest five (0x9000..0x9004) are gone.
        x.returned(0x9000);
        assert_eq!(x.shadow_depth(), MAX_SHADOW_DEPTH, "no frame expects it");
        x.reset_frames();
        assert_eq!(x.shadow_depth(), 0);
    }

    #[test]
    fn only_hooked_entries_cost_anything_and_no_hooks_track_nothing() {
        let mut x = RebuildExec::new(hook_atlas());
        assert!(!x.hooks_active(), "no hooks installed: the core skips it");
        x.set_hooked(&[1]);
        x.landed(Some(0x10), 0x9000); // function 0: not hooked
        x.landed(Some(0x11), 0x9000); // not an entry at all
        x.landed(None, 0x9000); // not ROM
        x.landed(Some(0x1000), 0x9000); // past the image
        assert!(drain(&mut x).is_empty());
        assert_eq!(x.shadow_depth(), 0, "unhooked calls leave no frame");
        x.landed(Some(0x20), 0x9000);
        assert_eq!(drain(&mut x), [enter(1)]);
        // Out-of-range ids are ignored; an empty set turns hooks off.
        assert_eq!(x.set_hooked(&[99]), 0);
        assert!(!x.hooks_active());
        assert_eq!(x.shadow_depth(), 0, "installing hooks resets the stack");
    }

    #[test]
    fn the_delta_list_is_bounded() {
        let mut x = RebuildExec::new(atlas());
        let Dispatch::Atlas { node, .. } = x.dispatch(0x8000, Some(2)) else {
            panic!()
        };
        let site = addr(0x8002, Some(2));
        for t in 0..(MAX_DELTAS as u32 + 10) {
            x.check_target(node, site, addr(0x9000, Some(0x100 + t)));
        }
        let s = x.stats();
        assert_eq!(s.delta_len as usize, MAX_DELTAS);
        assert_eq!(s.delta_dropped, 10);
        assert_eq!(s.dispatch_misses, MAX_DELTAS as u64 + 10);
    }

    /// The same bytes in two decode modes (65816 widths): two functions at
    /// one ROM offset, each with its own entry; an indirect site at offset 4
    /// recorded as landing on `(0, mode 1)` only.
    fn mode_atlas() -> Atlas {
        let at = |rom: u32, flow, mode: u8| InsnRec {
            rom,
            cpu: 0x8000 + rom,
            len: 1,
            flow,
            prov: Provenance::Observed,
            mode,
        };
        let block = |mode: u8| Block {
            start_rom: 0,
            start_cpu: 0x8000,
            start_mode: mode,
            insns: vec![at(0, Flow::Fall, mode), at(4, Flow::Return, mode)],
            edges: vec![],
        };
        let func = |mode: u8, block: u32| Function {
            entry_rom: 0,
            entry_cpu: 0x8000,
            entry_mode: mode,
            entry_kinds: 0,
            blocks: vec![block],
            insn_count: 2,
            byte_len: 2,
            byte_hash: 0,
            shape_hash: 0,
        };
        Atlas {
            prg_len: 0x10,
            mode_count: 5,
            blocks: vec![block(1), block(4)],
            functions: vec![func(1, 0), func(4, 1)],
            indirect_sites: vec![IndirectSite {
                site_rom: 4,
                site_cpu: 0x8004,
                kind: SiteKind::Rts,
                targets: vec![Target::Rom {
                    cpu: 0x8000,
                    rom: 0,
                    mode: 1,
                }],
            }],
            volatile: vec![],
            stats: Stats::default(),
        }
    }

    #[test]
    fn nodes_are_keyed_by_offset_and_mode_and_a_wrong_mode_is_counted_apart() {
        let mut x = RebuildExec::new(mode_atlas());
        x.enable_trace(8);
        let hit = |d: Dispatch| matches!(d, Dispatch::Atlas { .. });
        assert!(hit(x.dispatch_at(0, 0x8000, Fetch::Rom(0), 1)));
        assert!(hit(x.dispatch_at(0, 0x8000, Fetch::Rom(0), 4)));
        assert_eq!(
            x.dispatch_at(0, 0x8000, Fetch::Rom(0), 2),
            Dispatch::WidthMismatch
        );
        assert_eq!(
            x.dispatch_at(0, 0x8000, Fetch::Rom(0), 0),
            Dispatch::WidthMismatch,
            "emulation mode is a mode too"
        );
        assert_eq!(
            x.dispatch_at(0, 0x8001, Fetch::Rom(1), 1),
            Dispatch::Fallback,
            "no node at the offset in any mode"
        );
        assert_eq!(
            x.dispatch_at(0x7E, 0x2000, Fetch::Ram, 1),
            Dispatch::Fallback
        );
        assert_eq!(
            x.dispatch_at(0x00, 0x3000, Fetch::Other, 1),
            Dispatch::Fallback
        );
        let s = x.stats();
        assert_eq!((s.observed, s.fallback), (2, 5));
        assert_eq!(
            (
                s.fallback_width,
                s.fallback_no_node,
                s.fallback_ram,
                s.fallback_other
            ),
            (2, 1, 1, 1),
            "the parts add up to the total"
        );
        let t = x.trace();
        assert_eq!(t[2].kind, TraceKind::WidthMismatch);
        assert_eq!((t[2].mode, t[5].bank, t[5].cpu), (2, 0x7E, 0x2000));
    }

    #[test]
    fn hook_entries_and_indirect_targets_are_per_mode() {
        let mut x = RebuildExec::new(mode_atlas());
        x.set_hooked(&[1]); // only the mode-4 function
        x.landed_at(Some(0), 1, 0x80_9000);
        assert!(drain(&mut x).is_empty(), "mode 1 is function 0: not hooked");
        x.landed_at(Some(0), 2, 0x80_9000);
        assert!(drain(&mut x).is_empty(), "no function in mode 2");
        x.landed_at(Some(0), 4, 0x80_9003);
        assert_eq!(drain(&mut x), [enter(1)]);
        x.returned(0x00_9003);
        assert!(drain(&mut x).is_empty(), "the bank is part of the address");
        x.returned(0x80_9003);
        assert_eq!(drain(&mut x), [exit(1)]);

        let Dispatch::Atlas { node, indirect } = x.dispatch_at(0, 0x8004, Fetch::Rom(4), 1) else {
            panic!("site is in the atlas");
        };
        assert!(indirect);
        let site = addr(0x8004, Some(4));
        let to = |mode| CodeAddr {
            mode,
            ..addr(0x8000, Some(0))
        };
        x.check_target(node, site, to(1));
        assert_eq!(x.stats().dispatch_misses, 0);
        x.check_target(node, site, to(4));
        assert_eq!(
            x.stats().dispatch_misses,
            1,
            "same offset in another width is an unrecorded landing"
        );
        // A landing outside ROM is keyed by its full 24-bit address.
        let ram = |pbr| CodeAddr {
            pbr,
            ..addr(0x2000, None)
        };
        x.check_target(node, site, ram(0x7E));
        x.check_target(node, site, ram(0x7F));
        assert_eq!(x.stats().dispatch_misses, 3);
    }
    /// Ticket W37-04: a demoted function's instructions dispatch as plain
    /// fallback (counted apart), every other function is untouched, and an
    /// empty set restores them.
    #[test]
    fn a_demoted_function_dispatches_as_fallback_and_others_do_not() {
        let mut x = RebuildExec::new(hook_atlas());
        // Function 1's two instructions are at ROM 0x20 and 0x21.
        assert!(matches!(
            x.dispatch(0x8020, Some(0x20)),
            Dispatch::Atlas { .. }
        ));
        assert_eq!(x.set_demoted(&[1, 1, 99]), 1, "dup and out-of-range ids");
        assert_eq!(x.dispatch(0x8020, Some(0x20)), Dispatch::Fallback);
        assert_eq!(x.dispatch(0x8021, Some(0x21)), Dispatch::Fallback);
        assert!(matches!(
            x.dispatch(0x8010, Some(0x10)),
            Dispatch::Atlas { .. }
        ));
        let s = x.stats();
        assert_eq!(s.fallback_demoted, 2);
        assert_eq!(s.fallback, 2);
        assert_eq!(s.fallback_no_node, 0, "demoted is not 'no node'");
        assert_eq!(s.fallback_width, 0);
        assert_eq!(x.set_demoted(&[]), 0);
        assert!(matches!(
            x.dispatch(0x8020, Some(0x20)),
            Dispatch::Atlas { .. }
        ));
    }

    /// Ticket W37-07: after a state load the first block runs on the plain
    /// interpreter (counted as its own reason), then dispatch re-enters the
    /// atlas at the next block leader.
    #[test]
    fn a_state_load_resumes_one_block_on_the_interpreter_then_reenters() {
        let mut x = RebuildExec::new(hook_atlas());
        x.state_loaded();
        // Landed on the leader of function 1's block: both of its
        // instructions are interpreter steps.
        assert_eq!(x.dispatch(0x8020, Some(0x20)), Dispatch::Fallback);
        assert_eq!(x.dispatch(0x8021, Some(0x21)), Dispatch::Fallback);
        assert!(matches!(
            x.dispatch(0x8010, Some(0x10)),
            Dispatch::Atlas { .. }
        ));
        let s = x.stats();
        assert_eq!((s.fallback, s.fallback_after_load), (2, 2));
        assert_eq!((s.fallback_no_node, s.fallback_demoted), (0, 0));
        assert_eq!(s.observed, 1);
        // A block that loops onto its own leader ends the resumed block there.
        x.state_loaded();
        assert_eq!(x.dispatch(0x8010, Some(0x10)), Dispatch::Fallback);
        assert!(matches!(
            x.dispatch(0x8010, Some(0x10)),
            Dispatch::Atlas { .. }
        ));
        // Landing mid-block: the rest of it, then the next leader.
        x.state_loaded();
        assert_eq!(x.dispatch(0x8021, Some(0x21)), Dispatch::Fallback);
        assert!(matches!(
            x.dispatch(0x8030, Some(0x30)),
            Dispatch::Atlas { .. }
        ));
        assert_eq!(x.stats().fallback_after_load, 2 + 1 + 1);
    }

    #[test]
    fn the_resume_rule_ends_at_code_the_atlas_does_not_know_and_at_interrupts() {
        let mut x = RebuildExec::new(hook_atlas());
        x.state_loaded();
        // RAM code is the first block: one reason only, then normal counting.
        assert_eq!(x.dispatch_at(0, 0x0300, Fetch::Ram, 0), Dispatch::Fallback);
        assert_eq!(x.dispatch_at(0, 0x0301, Fetch::Ram, 0), Dispatch::Fallback);
        let s = x.stats();
        assert_eq!((s.fallback_after_load, s.fallback_ram), (1, 1));
        x.state_loaded();
        x.note_interrupt(0x8000);
        assert!(matches!(
            x.dispatch(0x8010, Some(0x10)),
            Dispatch::Atlas { .. }
        ));
        assert_eq!(x.stats().fallback_after_load, 1, "the interrupt ended it");
        // A reset (frames forgotten, no load) does not arm the rule.
        x.reset_frames();
        assert!(matches!(
            x.dispatch(0x8010, Some(0x10)),
            Dispatch::Atlas { .. }
        ));
    }

    /// Ticket W37-07: what a load drops and what it keeps.
    #[test]
    fn a_state_load_drops_frames_and_events_and_keeps_the_session() {
        let mut x = RebuildExec::new(hook_atlas());
        x.set_hooked(&[0, 1]);
        x.set_demoted(&[2]);
        x.enable_trace(8);
        x.dispatch(0x8010, Some(0x10));
        x.landed(Some(0x10), 0x9000);
        x.landed(Some(0x20), 0x9100);
        assert_eq!(x.shadow_depth(), 2);
        x.state_loaded();
        assert_eq!(x.shadow_depth(), 0);
        assert!(drain(&mut x).is_empty(), "the Enters were never delivered");
        assert!(x.trace().is_empty() && x.trace_total() == 0);
        // The return of a call made before the load reports nothing.
        x.returned(0x9100);
        x.returned(0x9000);
        assert!(drain(&mut x).is_empty(), "no orphan Exit");
        // Session settings and counters survive.
        assert!(x.hooks_active());
        assert_eq!(x.stats().observed, 1);
        assert_eq!(x.dispatch(0x8030, Some(0x30)), Dispatch::Fallback);
        assert_eq!(x.dispatch(0x8031, Some(0x31)), Dispatch::Fallback);
        x.dispatch(0x8040, Some(0x40)); // leader of another block: re-entered
        x.landed(Some(0x10), 0x9000);
        assert_eq!(drain(&mut x), [enter(0)], "calls after the load pair up");
    }
}
