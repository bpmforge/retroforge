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
const NO_SITE: u32 = u32::MAX;
/// Set on a target key that is a CPU address (outside PRG ROM) rather than a
/// ROM offset.
const CPU_KEY: u32 = 1 << 31;

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
    site: u32,
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
    /// Found; `indirect` says the caller should call
    /// [`RebuildExec::check_target`] after the step.
    Atlas { node: u32, indirect: bool },
}

pub struct RebuildExec {
    atlas: Atlas,
    /// PRG offset to index into `nodes`, `NO_NODE` when unknown.
    slot: Vec<u32>,
    nodes: Vec<Node>,
    /// Per indirect site, sorted unique target keys.
    site_targets: Vec<Vec<u32>>,
    /// Ticket W36-05: bitset over function ids of the hooked entries.
    hooked: Vec<u64>,
    hooked_count: u32,
    /// Hooked calls in flight, outermost first.
    frames: Vec<Frame>,
    frames_dropped: u64,
    events: Vec<FunctionEvent>,
    stats: RebuildStats,
    deltas: Vec<Delta>,
    seen: HashSet<Delta>,
    /// Ticket W36-04: ring of the last executed steps, empty (and untouched
    /// by the per-instruction path beyond one `is_empty`) until
    /// [`RebuildExec::enable_trace`]. `(cpu, node)`, power-of-two length.
    trace: Vec<(u16, u32)>,
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
        let site_targets: Vec<Vec<u32>> = atlas
            .indirect_sites
            .iter()
            .map(|s| {
                let mut keys: Vec<u32> = s.targets.iter().map(target_key).collect();
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
        let mut entry_fn: HashMap<u32, u32> = HashMap::with_capacity(atlas.functions.len());
        for (fi, f) in atlas.functions.iter().enumerate() {
            entry_fn.entry(f.entry_rom).or_insert(fi as u32);
        }
        let mut slot = vec![NO_NODE; atlas.prg_len as usize];
        let mut nodes = Vec::new();
        for (bi, block) in atlas.blocks.iter().enumerate() {
            for insn in &block.insns {
                let Some(s) = slot.get_mut(insn.rom as usize) else {
                    continue;
                };
                if *s != NO_NODE {
                    continue;
                }
                *s = nodes.len() as u32;
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
                    entry_of: entry_fn.get(&insn.rom).copied().unwrap_or(u32::MAX),
                    site: site_of(insn.rom),
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
            frames: Vec::new(),
            frames_dropped: 0,
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
    fn trace_push(&mut self, cpu: u16, node: u32) {
        let mask = self.trace.len().wrapping_sub(1);
        if let Some(slot) = self.trace.get_mut(self.trace_next & mask) {
            *slot = (cpu, node);
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
            .map(|&(cpu, node)| TraceEntry {
                cpu,
                kind: match node {
                    NO_NODE => TraceKind::Fallback,
                    TRACE_INTERRUPT => TraceKind::Interrupt,
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

    /// Steps executed since the trace was enabled (not bounded by capacity).
    #[must_use]
    pub fn trace_total(&self) -> u64 {
        self.trace_total
    }

    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// The node record for a [`Dispatch::Atlas`] answer.
    #[must_use]
    pub fn node(&self, node: u32) -> Option<&Node> {
        self.nodes.get(node as usize)
    }

    /// Look up the instruction about to run at CPU address `pc`, at PRG
    /// offset `rom` (`None` when the CPU address is not PRG ROM right now),
    /// and count it.
    #[inline]
    pub fn dispatch(&mut self, pc: u16, rom: Option<u32>) -> Dispatch {
        let node = rom
            .and_then(|r| self.slot.get(r as usize))
            .copied()
            .filter(|&n| n != NO_NODE)
            .and_then(|n| self.nodes.get(n as usize).map(|nd| (n, *nd)));
        if !self.trace.is_empty() {
            self.trace_push(pc, node.map_or(NO_NODE, |(n, _)| n));
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
                Dispatch::Fallback
            }
        }
    }

    /// An NMI/IRQ sequence ran instead of an instruction.
    #[inline]
    pub fn note_interrupt(&mut self, pc: u16) {
        self.stats.interrupt_entries += 1;
        if !self.trace.is_empty() {
            self.trace_push(pc, TRACE_INTERRUPT);
        }
    }

    /// After an indirect transfer at `site` (the `node` dispatch returned),
    /// compare where it landed with the atlas's recorded targets. A miss is
    /// counted and appended to the delta list; it never changes behaviour.
    pub fn check_target(&mut self, node: u32, site: CodeAddr, target: CodeAddr) {
        self.stats.indirect_checked += 1;
        let key = target.rom.unwrap_or(CPU_KEY | u32::from(target.cpu));
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
    }

    /// A call, `BRK` or interrupt entry just ran and control is now at PRG
    /// offset `rom` (`None`: not PRG ROM). `ret` is where the matching
    /// return should land. If that is the entry of a hooked function, push
    /// its frame and queue Enter.
    #[inline]
    pub fn landed(&mut self, rom: Option<u32>, ret: u32) {
        let Some(function) = rom
            .and_then(|r| self.slot.get(r as usize))
            .and_then(|&n| self.nodes.get(n as usize))
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

fn target_key(t: &Target) -> u32 {
    match *t {
        Target::Rom { rom, .. } => rom,
        Target::Unresolved { cpu, .. } => CPU_KEY | cpu,
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
}
