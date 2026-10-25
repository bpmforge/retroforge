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

use std::collections::HashSet;

use rf_core_api::{CodeAddr, RebuildStats};

use crate::atlas::{Atlas, Provenance, Target};
use crate::isa::Flow;

/// Most distinct (site, target) misses kept for later consumers.
pub const MAX_DELTAS: usize = 4096;

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
    site: u32,
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

/// Called with function entry/exit by W36-05's hooks. Always `None` in
/// W36-03; the slot exists so hooks are additive.
pub type FunctionHook = Box<dyn FnMut(FunctionEvent) + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionEvent {
    Enter { function: u32 },
    Exit { function: u32 },
}

pub struct RebuildExec {
    atlas: Atlas,
    /// PRG offset to index into `nodes`, `NO_NODE` when unknown.
    slot: Vec<u32>,
    nodes: Vec<Node>,
    /// Per indirect site, sorted unique target keys.
    site_targets: Vec<Vec<u32>>,
    /// W36-05's hook point. Never set or called in W36-03.
    pub hook: Option<FunctionHook>,
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
                        Flow::IndirectJump | Flow::Return | Flow::ReturnInterrupt | Flow::Break
                    ),
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
            hook: None,
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
        }
    }

    fn atlas() -> Atlas {
        Atlas {
            prg_len: 0x20,
            blocks: vec![Block {
                start_rom: 0,
                start_cpu: 0x8000,
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
                entry_kinds: 0,
                blocks: vec![0],
                insn_count: 3,
                byte_len: 3,
                byte_hash: 0,
            }],
            indirect_sites: vec![IndirectSite {
                site_rom: 2,
                site_cpu: 0x8002,
                kind: SiteKind::Rts,
                targets: vec![
                    Target::Rom {
                        cpu: 0x8000,
                        rom: 0,
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
        CodeAddr { cpu, rom }
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
