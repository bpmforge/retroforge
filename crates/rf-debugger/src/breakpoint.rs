//! Breakpoint engine (ticket W4-06e; DEBUGGER.md §1, FR-DBG-004).
//!
//! ## The seam decision, made deliberately and stated
//!
//! The ticket required picking between two routes and saying why:
//!
//! - **(a)** the SHELL drives `cpu.step(bus)` per instruction and checks
//!   breakpoints at instruction boundaries;
//! - **(b)** a compiled table is checked INSIDE `rf-nes`'s step loop.
//!
//! **Route (a) is chosen**, and the deciding fact is what acceptance
//! criterion 1 actually lists: *"PC exec, value-conditional, scanline/dot
//! position, IRQ/mapper events"*. Every one of those is answerable at an
//! **instruction boundary** — a value-*conditional* breakpoint tests what
//! memory holds when the check runs; it is not a write-watch. So route
//! (b)'s only genuine advantage — seeing individual bus writes — buys
//! nothing here, while its cost is permanent: debugger concerns compiled
//! into the core, on the hot path of every instruction of every session.
//!
//! Route (a) also keeps the layer boundary intact: `rf-debugger` stays
//! core-free and UI-free (this module has no `rf-nes` dependency and no
//! `egui`), and the shell — which already owns `EmuStepper` and may
//! depend on `rf-nes` — does the driving.
//!
//! **What route (a) cannot do, stated rather than discovered later:** a
//! true WATCHPOINT ("break when value V is written to address A") needs
//! mid-instruction bus visibility, because a write that is overwritten
//! within the same instruction is invisible at the boundary.
//! Snapshot-diffing memory between steps would be O(memory) per
//! instruction *and* would still miss that case, so it is not a
//! workaround. Write-watchpoints are **not** in criterion 1's list and
//! are not built here; `rf_core_api::CoreEvent::WatchpointHit` remains a
//! defined event with no producer, which is worth a ticket of its own
//! and needs route (b) or something like it.
//!
//! ## "One-branch cost when no breakpoint is armed"
//!
//! [`BreakpointTable::armed`] is a single `bool` the caller checks before
//! doing anything else. The table is compiled once on edit, not walked
//! per instruction, and an empty table costs one predictable branch —
//! which is the claim DEBUGGER.md §1 makes and W4-06e's criterion 3
//! requires be *measured* rather than asserted.

/// What a breakpoint watches for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Break when the CPU is about to execute this address.
    Pc(u16),
    /// Break when `addr` holds `value` at an instruction boundary.
    ///
    /// Deliberately named "value-conditional" rather than "watchpoint":
    /// it tests what memory *holds*, not what was *written* — see this
    /// module's doc for why that distinction decided the whole design.
    MemoryEquals { addr: u16, value: u8 },
    /// Break at a specific raster position.
    Position { scanline: u16, dot: u16 },
    /// Break when the core reports one of these events this step.
    Event(EventKind),
    /// Break on a memory ACCESS — DEBUGGER.md §1's "memory read/write/
    /// access (CPU and PPU address spaces separately), value-conditional
    /// (`addr==X && val&mask`)" (ticket W13-02e).
    ///
    /// ## Why this is not `MemoryEquals` with more fields
    ///
    /// [`Condition::MemoryEquals`] tests what memory *holds* at an
    /// instruction boundary; this tests what the machine *did*, at the
    /// cycle it did it. Nothing this crate can see distinguishes them —
    /// only the bus can — so the condition is evaluated **inside the
    /// core** ([`rf_core_api::WatchTable`]) and arrives back here as a
    /// [`rf_core_api::CoreEvent::MemWatch`] carrying the id. That is why
    /// this variant stores the whole [`rf_core_api::MemWatch`]: the
    /// debugger owns the definition, the core owns the evaluation, and the
    /// id is the join.
    Watch(rf_core_api::MemWatch),
}

/// The event classes a breakpoint can arm on (criterion 1's "IRQ/mapper
/// events").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Irq,
    Nmi,
    MapperIrq,
}

/// One armed breakpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakpoint {
    pub id: u32,
    pub condition: Condition,
    pub enabled: bool,
}

/// What the engine needs to know about the machine at a boundary.
///
/// A borrowed view rather than a snapshot: building this must not copy
/// memory, or the "one-branch cost" claim would be false the moment a
/// breakpoint existed.
pub struct BreakCtx<'a> {
    pub pc: u16,
    pub scanline: u16,
    pub dot: u16,
    /// Non-perturbing memory read — the shell passes `NesBus::peek`, which
    /// is side-effect-free by construction (W4-06d's reasoning).
    pub peek: &'a dyn Fn(u16) -> u8,
    /// Event classes the core reported for the instruction just executed.
    pub events: &'a [EventKind],
    /// Ids of watchpoints the core reported tripping this step, from
    /// [`rf_core_api::CoreEvent::MemWatch`] (ticket W13-02e). Empty when
    /// nothing is watched, which is the normal case.
    pub watch_hits: &'a [u32],
}

/// Which breakpoint stopped execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub id: u32,
    pub condition: Condition,
}

/// The compiled breakpoint table.
///
/// "Compiled" means the enabled set is partitioned by kind once, on edit,
/// so a check never filters or matches on disabled entries — and `armed`
/// answers "is there anything to check at all" without touching a list.
#[derive(Debug, Clone, Default)]
pub struct BreakpointTable {
    all: Vec<Breakpoint>,
    pc: Vec<(u32, u16)>,
    memory: Vec<(u32, u16, u8)>,
    position: Vec<(u32, u16, u16)>,
    events: Vec<(u32, EventKind)>,
    watches: Vec<(u32, rf_core_api::MemWatch)>,
    armed: bool,
}

impl BreakpointTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Is anything armed? **This is the one branch** a caller pays per
    /// instruction when the debugger is idle.
    #[must_use]
    pub const fn armed(&self) -> bool {
        self.armed
    }

    pub fn add(&mut self, breakpoint: Breakpoint) {
        self.all.push(breakpoint);
        self.compile();
    }

    pub fn remove(&mut self, id: u32) {
        self.all.retain(|b| b.id != id);
        self.compile();
    }

    pub fn set_enabled(&mut self, id: u32, enabled: bool) {
        if let Some(b) = self.all.iter_mut().find(|b| b.id == id) {
            b.enabled = enabled;
        }
        self.compile();
    }

    #[must_use]
    pub fn all(&self) -> &[Breakpoint] {
        &self.all
    }

    /// Rebuild the per-kind lists. Called on every edit, never per
    /// instruction — that asymmetry is the whole point of "compiled".
    fn compile(&mut self) {
        self.pc.clear();
        self.memory.clear();
        self.position.clear();
        self.events.clear();
        self.watches.clear();
        for b in self.all.iter().filter(|b| b.enabled) {
            match &b.condition {
                Condition::Pc(addr) => self.pc.push((b.id, *addr)),
                Condition::MemoryEquals { addr, value } => {
                    self.memory.push((b.id, *addr, *value));
                }
                Condition::Position { scanline, dot } => {
                    self.position.push((b.id, *scanline, *dot));
                }
                Condition::Event(kind) => self.events.push((b.id, *kind)),
                Condition::Watch(watch) => self.watches.push((b.id, *watch)),
            }
        }
        self.armed = !self.pc.is_empty()
            || !self.memory.is_empty()
            || !self.position.is_empty()
            || !self.events.is_empty()
            || !self.watches.is_empty();
    }

    /// The watchpoints to install in the core (ticket W13-02e).
    ///
    /// The debugger defines them; only the bus can evaluate them, so this
    /// is what crosses the boundary — a caller pushes the result into
    /// `CoreConfig::watches` and the core reports hits back as
    /// `CoreEvent::MemWatch`.
    #[must_use]
    pub fn watches(&self) -> Vec<rf_core_api::MemWatch> {
        self.watches.iter().map(|(_, w)| *w).collect()
    }

    /// Check every armed breakpoint against `ctx`, cheapest kind first.
    ///
    /// Ordering is deliberate: PC and position are integer compares,
    /// while a memory condition costs a `peek` call each. A caller that
    /// hits a PC breakpoint never pays for the memory ones.
    #[must_use]
    pub fn check(&self, ctx: &BreakCtx<'_>) -> Option<Hit> {
        if !self.armed {
            return None;
        }
        for (id, addr) in &self.pc {
            if *addr == ctx.pc {
                return Some(Hit {
                    id: *id,
                    condition: Condition::Pc(*addr),
                });
            }
        }
        for (id, scanline, dot) in &self.position {
            if *scanline == ctx.scanline && *dot == ctx.dot {
                return Some(Hit {
                    id: *id,
                    condition: Condition::Position {
                        scanline: *scanline,
                        dot: *dot,
                    },
                });
            }
        }
        for (id, kind) in &self.events {
            if ctx.events.contains(kind) {
                return Some(Hit {
                    id: *id,
                    condition: Condition::Event(*kind),
                });
            }
        }
        for (id, watch) in &self.watches {
            if ctx.watch_hits.contains(&watch.id) {
                return Some(Hit {
                    id: *id,
                    condition: Condition::Watch(*watch),
                });
            }
        }
        for (id, addr, value) in &self.memory {
            if (ctx.peek)(*addr) == *value {
                return Some(Hit {
                    id: *id,
                    condition: Condition::MemoryEquals {
                        addr: *addr,
                        value: *value,
                    },
                });
            }
        }
        None
    }
}

/// What the shell's execution control was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMode {
    /// One instruction.
    Into,
    /// One instruction, but run a whole subroutine call to completion.
    Over,
    /// Run until the current subroutine returns.
    Out,
    /// Run until the PC reaches an address.
    ToCursor(u16),
}

/// Extract the watchpoint ids a core reported this step (ticket W13-02e).
///
/// The core's answer arrives as [`rf_core_api::CoreEvent::MemWatch`] in
/// the frame's event list, mixed in with everything else a subscriber
/// asked for; this is the one place that filtering is written, so a caller
/// feeding [`BreakCtx::watch_hits`] cannot get it subtly wrong.
#[must_use]
pub fn watch_hits(events: &[rf_core_api::CoreEvent]) -> Vec<u32> {
    events
        .iter()
        .filter_map(|e| match e {
            rf_core_api::CoreEvent::MemWatch { id } => Some(*id),
            _ => None,
        })
        .collect()
}

/// 6502 opcodes the step logic must recognise. Named rather than inlined
/// so the reasoning is checkable against nesdev's opcode table.
pub const OPCODE_JSR: u8 = 0x20;

/// Should `Over` behave as a plain single step?
///
/// Only `JSR` creates a frame worth stepping over. Everything else —
/// including branches and `JMP` — is a single instruction, and treating
/// `JMP` as a call would make step-over hang on a tail-call loop.
#[must_use]
pub const fn step_over_is_a_call(opcode: u8) -> bool {
    opcode == OPCODE_JSR
}

/// Has a step-over/step-out finished, given the stack pointer at entry?
///
/// Uses the STACK POINTER rather than counting `JSR`/`RTS` pairs, because
/// a program that manipulates the stack directly — or takes an interrupt
/// mid-subroutine — would desynchronise a counter. `S` moving back above
/// where it started means the frame this step was waiting on is gone,
/// however it left.
///
/// 6502 `S` grows DOWNWARD (a push decrements), so "returned" is
/// `current > entry`.
#[must_use]
pub const fn frame_has_returned(entry_sp: u8, current_sp: u8) -> bool {
    current_sp > entry_sp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(pc: u16, peek: &'a dyn Fn(u16) -> u8, events: &'a [EventKind]) -> BreakCtx<'a> {
        BreakCtx {
            pc,
            scanline: 0,
            dot: 0,
            peek,
            events,
            watch_hits: &[],
        }
    }

    /// The same helper with watch hits, for the W13-02e cases below.
    fn ctx_with_watches<'a>(
        pc: u16,
        peek: &'a dyn Fn(u16) -> u8,
        watch_hits: &'a [u32],
    ) -> BreakCtx<'a> {
        BreakCtx {
            pc,
            scanline: 0,
            dot: 0,
            peek,
            events: &[],
            watch_hits,
        }
    }

    /// The performance claim, expressed structurally: an empty table is
    /// not armed, and `check` returns before touching anything.
    #[test]
    fn an_empty_table_is_not_armed_and_checks_nothing() {
        let table = BreakpointTable::new();
        assert!(!table.armed());
        let peek = |_: u16| panic!("an unarmed table must not read memory");
        assert_eq!(table.check(&ctx(0x8000, &peek, &[])), None);
    }

    /// A table whose only breakpoint is DISABLED is also not armed —
    /// otherwise disabling one would silently keep paying for it.
    #[test]
    fn disabling_the_last_breakpoint_disarms_the_table() {
        let mut table = BreakpointTable::new();
        table.add(Breakpoint {
            id: 1,
            condition: Condition::Pc(0x8000),
            enabled: true,
        });
        assert!(table.armed());
        table.set_enabled(1, false);
        assert!(!table.armed(), "a disabled breakpoint must cost nothing");
        let peek = |_: u16| panic!("must not read memory");
        assert_eq!(table.check(&ctx(0x8000, &peek, &[])), None);
        table.set_enabled(1, true);
        assert!(table.armed(), "and re-enabling must arm it again");
    }

    #[test]
    fn each_condition_kind_fires_on_its_own_trigger() {
        let mut table = BreakpointTable::new();
        table.add(Breakpoint {
            id: 1,
            condition: Condition::Pc(0xC123),
            enabled: true,
        });
        table.add(Breakpoint {
            id: 2,
            condition: Condition::MemoryEquals {
                addr: 0x0300,
                value: 0x42,
            },
            enabled: true,
        });
        table.add(Breakpoint {
            id: 3,
            condition: Condition::Position {
                scanline: 100,
                dot: 20,
            },
            enabled: true,
        });
        table.add(Breakpoint {
            id: 4,
            condition: Condition::Event(EventKind::MapperIrq),
            enabled: true,
        });

        let no_match = |_: u16| 0x00;
        assert_eq!(table.check(&ctx(0xC123, &no_match, &[])).unwrap().id, 1);

        let matching = |a: u16| if a == 0x0300 { 0x42 } else { 0 };
        assert_eq!(table.check(&ctx(0x8000, &matching, &[])).unwrap().id, 2);

        let mut position = ctx(0x8000, &no_match, &[]);
        position.scanline = 100;
        position.dot = 20;
        assert_eq!(table.check(&position).unwrap().id, 3);

        assert_eq!(
            table
                .check(&ctx(0x8000, &no_match, &[EventKind::MapperIrq]))
                .unwrap()
                .id,
            4
        );

        // And none of them fire on an unrelated boundary — without this,
        // a `check` that always returned the first entry would pass every
        // assertion above.
        assert_eq!(table.check(&ctx(0x8001, &no_match, &[])), None);
    }

    /// A PC hit must not pay for the memory conditions — the ordering is
    /// a cost decision, so it is asserted rather than left to reading.
    #[test]
    fn a_pc_hit_never_reads_memory() {
        let mut table = BreakpointTable::new();
        table.add(Breakpoint {
            id: 1,
            condition: Condition::Pc(0x8000),
            enabled: true,
        });
        table.add(Breakpoint {
            id: 2,
            condition: Condition::MemoryEquals {
                addr: 0x0300,
                value: 0x42,
            },
            enabled: true,
        });
        let peek = |_: u16| panic!("a PC hit must short-circuit before any peek");
        assert_eq!(table.check(&ctx(0x8000, &peek, &[])).unwrap().id, 1);
    }

    /// Only JSR is a call. Treating JMP as one would make step-over hang
    /// forever on a tail-call loop.
    #[test]
    fn only_jsr_counts_as_a_call_for_step_over() {
        assert!(step_over_is_a_call(0x20));
        for not_a_call in [0x4Cu8, 0x6C, 0xD0, 0xEA, 0x60, 0x40] {
            assert!(
                !step_over_is_a_call(not_a_call),
                "{not_a_call:#04X} must not be treated as a call"
            );
        }
    }

    /// Stack-pointer based, not JSR/RTS counting: `S` grows downward, so a
    /// frame has returned once `S` is back above where it started —
    /// however it left, including via an interrupt or direct stack
    /// manipulation that would desynchronise a counter.
    #[test]
    fn frame_return_is_detected_by_the_stack_pointer_not_by_counting() {
        assert!(!frame_has_returned(0xFD, 0xFB), "still inside (pushed)");
        assert!(!frame_has_returned(0xFD, 0xFD), "exactly at entry");
        assert!(frame_has_returned(0xFD, 0xFE), "popped past entry");
        assert!(frame_has_returned(0xFB, 0xFD), "unwound two levels");
    }

    /// W13-02e: a watch condition fires on the core's report, not on a
    /// peek — that is the whole distinction from `MemoryEquals`.
    #[test]
    fn a_watch_breakpoint_fires_on_the_cores_report_and_is_installable() {
        use rf_core_api::{MemWatch, WatchAccess, WatchSpace};
        let watch = MemWatch::unconditional(9, WatchSpace::Cpu, 0x0086, WatchAccess::Write);
        let mut table = BreakpointTable::new();
        table.add(Breakpoint {
            id: 1,
            condition: Condition::Watch(watch),
            enabled: true,
        });

        let peek = |_: u16| 0u8;
        // Nothing reported: no hit, however the memory happens to read.
        assert!(table.check(&ctx_with_watches(0, &peek, &[])).is_none());
        // The core reported this watch's id.
        let hit = table
            .check(&ctx_with_watches(0, &peek, &[9]))
            .expect("a reported watch id must break");
        assert_eq!(hit.id, 1);
        assert_eq!(hit.condition, Condition::Watch(watch));
        // A different watch's id is not this breakpoint.
        assert!(table.check(&ctx_with_watches(0, &peek, &[8])).is_none());

        // And the table hands the core what to install.
        assert_eq!(table.watches(), vec![watch]);
        table.set_enabled(1, false);
        assert!(
            table.watches().is_empty(),
            "a disabled watch must not stay armed in the core"
        );
    }
}
