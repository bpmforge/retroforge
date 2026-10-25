//! `rf_core_api::EmulatorCore` for the NES (ticket W11-10).
//!
//! ## Why this file exists at all
//!
//! `EmulatorCore` is the architecture's core contract — `ARCHITECTURE.md`
//! §3's "everything else sees cores through `rf-core-api`". Until W11-10
//! it was implemented by **two test mocks and by neither real console**:
//! `rf-core-api/tests/mock_core.rs` and
//! `rf-harness/tests/blargg_protocol.rs`. Both `rf-nes` and `rf-snes`
//! depended on `rf-core-api`, but only for its data types.
//!
//! `scripts/validate-arch.sh` rule 3 enforces the *dependency direction*
//! — only the app shell and the harness may reach a console core
//! directly — and nothing enforced that a core actually implements the
//! trait. So the contract was declared, tested against mocks, and never
//! adopted; the shell then grew around one concrete core, because that
//! was the only thing there was to grow around. That is the whole reason
//! a fully-tested SNES core cannot be opened in the application.
//!
//! ## What moved here, and why it belongs here
//!
//! The run-until-next-frame loop. It lived in
//! `retroforge::stepper::EmuStepper`, which is to say the *shell* owned
//! the definition of "one frame" for the NES. That is core knowledge:
//! how many CPU steps make a frame, when video and audio drain, and what
//! to do when a frame boundary never arrives are all facts about the
//! machine, not about the window drawing it.
//!
//! ## The cycle budget is not an optimisation
//!
//! `Cpu::step` is not guaranteed to reach a frame boundary: a `JAM`/`KIL`
//! opcode re-executes forever (that crate's own doc says so). The loop
//! therefore runs against a deadline and reports `frame_complete: false`
//! rather than spinning — which matters more than it looks, because in
//! the app this loop runs inside the core thread's guarded closure, and
//! an unbounded hang there would also block `CoreCommand::Shutdown` from
//! ever being drained. That is worse than the crash FM-01 exists to
//! contain.

use rf_core_api::coverage::{
    WindowMap, OPCODE, OPERAND, WINDOW_BASE, WINDOW_PAGES, WINDOW_PAGE_SHIFT,
};
use rf_core_api::{
    CartImage, CoreConfig, CoreError, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind,
    StateError, StateReader, StateView, StateWriter, Step, StepResult,
};
use rf_core_api::{
    CodeAddr, CoverageLog, CpuRegs, IndirectEdge, IndirectKind, InterruptEntry, InterruptKind,
    Mos6502Regs,
};

use crate::rebuild::{Atlas, Delta, RebuildExec, TraceEntry};
use rf_rebuild::Dispatch;

use crate::{Cpu, NesBus};

/// Four NTSC frames' worth of CPU cycles (29,781 per frame).
///
/// **The exact value `retroforge::stepper` used before W11-10**, and it
/// is copied rather than re-derived on purpose: this is a defensive
/// bound whose only job is to stop a machine that can never finish a
/// frame, and changing it while moving it would have made a behaviour
/// change look like a refactor. A first attempt here used
/// `4 * 341 * 262 * 4` — "four frames of dots" — which is roughly twelve
/// times larger and would have let a jammed core spin twelve times
/// longer before reporting.
///
/// Generous on purpose all the same: a budget tight enough to interrupt
/// ordinary emulation would turn a correctness backstop into a source of
/// dropped frames.
pub const CYCLE_BUDGET: u64 = 4 * 29_781;

/// The NES as an [`EmulatorCore`].
pub struct NesCore {
    bus: NesBus,
    cpu: Cpu,
    config: CoreConfig,
    cycle_budget: u64,
    /// Ticket W13-02e: what was last pushed to the bus, so the sync at the
    /// top of [`NesCore::step`] copies only when the caller has actually
    /// changed something. `CoreConfig` is `Copy` and comparable, so this
    /// is a value comparison, not a heap check.
    pushed_watches: rf_core_api::WatchTable,
    /// Ticket W36-01: the armed coverage recorder, `None` when off. Session
    /// observation like the watch table: never saved, never restored, and
    /// not touched by a state load. Boxed so the off case adds one pointer
    /// to the struct and one `is_some` to [`NesCore::tick`].
    coverage: Option<Box<CoverageLog>>,
    /// Ticket W36-02: the window map as of the last snapshot the recorder
    /// took. An executing instruction whose page maps somewhere else than
    /// this says the banking changed and triggers a fresh snapshot. Cleared
    /// whenever the log is replaced, so a new log always starts with one.
    window_cache: WindowMap,
    /// Ticket W36-03: the block executor's map, `None` when Rebuild is off.
    /// Derived from `pc` and the atlas, so it is not saved with the state and
    /// a state load leaves it alone (`state load` goes through
    /// [`NesCore::parts_mut`], which never touches it).
    rebuild: Option<Box<RebuildExec>>,
    /// Ticket W36-05: the atlas function ids asked to emit
    /// [`CoreSink::function_event`], kept so a freshly installed atlas gets
    /// them. Session observation: not saved, dropped by `load`.
    hook_ids: Vec<u32>,
    /// `coverage.is_some() || rebuild.is_some()`: the single flag the hot
    /// path in [`NesCore::tick`] tests.
    observing: bool,
}

impl NesCore {
    /// Build from an iNES image.
    ///
    /// # Errors
    /// Whatever [`NesBus::from_ines_bytes`] rejects — bad magic, an
    /// unimplemented mapper, a truncated image.
    pub fn from_ines_bytes(raw: &[u8]) -> Result<Self, crate::NesLoadError> {
        let mut bus = NesBus::from_ines_bytes(raw)?;
        let cpu = Cpu::power_on(&mut bus);
        Ok(Self {
            bus,
            cpu,
            config: CoreConfig::default(),
            cycle_budget: CYCLE_BUDGET,
            pushed_watches: rf_core_api::WatchTable::new(),
            coverage: None,
            window_cache: WindowMap::EMPTY,
            rebuild: None,
            hook_ids: Vec::new(),
            observing: false,
        })
    }

    /// Borrow the bus, for the shell's out-of-band reads and the
    /// debugger's viewers.
    #[must_use]
    pub fn bus(&self) -> &NesBus {
        &self.bus
    }

    /// Shrink the defensive cycle budget.
    ///
    /// Exists so the termination proofs can actually drive the bound
    /// rather than assert that a constant appears in the source.
    /// `retroforge::stepper`'s tests have set a tiny budget since W1-06
    /// for exactly that reason, and W11-10 briefly broke them by moving
    /// the budget here without bringing the lever with it — the tests
    /// went red, which is what they are for.
    pub fn set_cycle_budget(&mut self, budget: u64) {
        self.cycle_budget = budget;
    }

    /// Ticket W30-08: run `lines` extra scanlines of CPU-only time every
    /// frame (`0..=240`, default `0` = off). See
    /// [`NesBus::set_extra_vblank_lines`] for what is and is not frozen.
    /// Returns the value in force.
    pub fn set_extra_vblank_lines(&mut self, lines: u16) -> u16 {
        self.bus.set_extra_vblank_lines(lines)
    }

    /// The extra vblank lines in force (`0` is off).
    #[must_use]
    pub fn extra_vblank_lines(&self) -> u16 {
        self.bus.extra_vblank_lines()
    }

    /// Borrow the CPU, for the state serializer and the nestest-shaped
    /// trace formatter, which need the private bookkeeping fields too.
    ///
    /// The *register readout* no longer comes through here: since
    /// W13-02i `state_view().cpu_regs` carries a typed
    /// `rf_core_api::Mos6502Regs`, so a shell wanting `pc` reads it
    /// through the trait like any other console.
    #[must_use]
    pub fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// Both halves, mutably, for a state serializer that must write the
    /// CPU and the bus as one consistent snapshot.
    ///
    /// A pair rather than two calls because the borrow checker would
    /// otherwise refuse the second — and because a snapshot taken from
    /// two separate borrows at two moments is exactly the kind of torn
    /// state a save file must never contain.
    pub fn parts_mut(&mut self) -> (&mut Cpu, &mut NesBus) {
        (&mut self.cpu, &mut self.bus)
    }

    /// Mutable bus access, for out-of-band writes (the debugger's
    /// memory editor) and event-mask changes.
    pub fn bus_mut(&mut self) -> &mut NesBus {
        &mut self.bus
    }

    /// One CPU instruction, draining whatever video and audio it
    /// produced. Returns whether a frame boundary was crossed.
    ///
    /// Audio drains on the same cadence as video — per instruction, not
    /// per frame — so the ring is fed steadily instead of in one
    /// ~800-sample burst at each frame boundary. A burst is what makes a
    /// small ring underrun between frames (FAILURE_MODES.md FM-02); the
    /// drain costs one branch when nothing is queued.
    fn tick(&mut self, sink: &mut dyn CoreSink, start_frame: u64) -> bool {
        if self.observing {
            self.step_observed(sink);
        } else {
            self.cpu.step(&mut self.bus);
        }
        self.bus.drain_video(sink);
        self.bus.drain_audio(sink);
        self.bus.frame_count() != start_frame
    }

    /// One instruction with the coverage recorder and/or the block executor
    /// watching (tickets W36-01, W36-03). Both halves only peek before the
    /// step and read `pc` after it, so the instruction itself is always the
    /// plain `Cpu::step`.
    #[inline(never)]
    fn step_observed(&mut self, sink: &mut dyn CoreSink) {
        let cov = self.coverage.is_some().then(|| self.coverage_before());
        let reb = self.rebuild_before();
        self.cpu.step(&mut self.bus);
        if let Some((node, site)) = reb.indirect {
            let target = self.code_addr(self.cpu.pc);
            if let Some(x) = self.rebuild.as_deref_mut() {
                x.check_target(node, site, target);
            }
        }
        if reb.hook != HookStep::None {
            self.hook_after(reb.hook, sink);
        }
        if let Some(before) = cov {
            self.coverage_after(before);
        }
    }

    /// Executor, pre-step half (ticket W36-03): where is the next instruction
    /// in the atlas? A pending interrupt means the step is an NMI/IRQ
    /// sequence, not the instruction at `pc`.
    fn rebuild_before(&mut self) -> RebuildBefore {
        let Some(x) = self.rebuild.as_deref_mut() else {
            return RebuildBefore::PLAIN;
        };
        let hooks = x.hooks_active();
        let (nmi, irq) = self.cpu.pending_interrupt();
        if nmi || irq {
            x.note_interrupt(self.cpu.pc);
            // The interrupt returns to the instruction it interrupted.
            return RebuildBefore {
                indirect: None,
                hook: if hooks {
                    HookStep::Enter {
                        ret: self.cpu.pc.into(),
                    }
                } else {
                    HookStep::None
                },
            };
        }
        let pc = self.cpu.pc;
        let rom = self
            .bus
            .prg_rom_offset(pc)
            .and_then(|o| u32::try_from(o).ok());
        let indirect = match x.dispatch(pc, rom) {
            Dispatch::Atlas {
                node,
                indirect: true,
            } => Some((
                node,
                CodeAddr {
                    cpu: pc,
                    pbr: 0,
                    mode: 0,
                    rom,
                },
            )),
            _ => None,
        };
        // Peeked, not taken from the atlas node: a call from code the atlas
        // has no node for (RAM code) still pushes a frame.
        let hook = if hooks {
            match self.bus.peek(pc) {
                // JSR returns to the next instruction, BRK skips its padding
                // byte (nesdev, "CPU interrupts").
                0x20 => HookStep::Enter {
                    ret: u32::from(pc.wrapping_add(3)),
                },
                0x00 => HookStep::Enter {
                    ret: u32::from(pc.wrapping_add(2)),
                },
                0x60 | 0x40 => HookStep::Return,
                _ => HookStep::None,
            }
        } else {
            HookStep::None
        };
        RebuildBefore { indirect, hook }
    }

    /// Hooks, post-step half (ticket W36-05): tell the executor where the
    /// call, interrupt or return landed, then hand any Enter/Exit events to
    /// `sink` with the machine lent through a [`NesHookCtx`]. Runs at an
    /// instruction boundary, on the core thread.
    #[inline(never)]
    fn hook_after(&mut self, step: HookStep, sink: &mut dyn CoreSink) {
        let pc = self.cpu.pc;
        let rom = match step {
            HookStep::Enter { .. } => self
                .bus
                .prg_rom_offset(pc)
                .and_then(|o| u32::try_from(o).ok()),
            _ => None,
        };
        let Some(x) = self.rebuild.as_deref_mut() else {
            return;
        };
        match step {
            HookStep::Enter { ret } => x.landed(rom, ret & 0xFFFF),
            HookStep::Return => x.returned(u32::from(pc)),
            HookStep::None => {}
        }
        let events = x.take_events();
        for ev in events.iter().copied() {
            let mut ctx = NesHookCtx {
                cpu: &self.cpu,
                bus: &mut self.bus,
            };
            sink.function_event(ev, &mut ctx);
        }
        x.recycle_events(events);
    }

    /// Forget the in-flight hooked calls (ticket W36-05). A machine reset
    /// does it itself; a caller that restores a savestate through
    /// [`NesCore::parts_mut`] must call this, because the call stack the
    /// executor remembered belongs to the machine that was replaced.
    pub fn reset_hook_frames(&mut self) {
        if let Some(x) = self.rebuild.as_deref_mut() {
            x.reset_frames();
        }
    }

    fn refresh_observing(&mut self) {
        self.observing = self.coverage.is_some() || self.rebuild.is_some();
    }

    /// Install (`Some`) or drop (`None`) a code atlas for the block executor
    /// (ticket W36-03). Counters start from zero.
    ///
    /// # Errors
    /// The atlas was built for a PRG image of another size (wrong cartridge).
    pub fn set_rebuild(&mut self, atlas: Option<Atlas>) -> Result<(), CoreError> {
        if let Some(a) = &atlas {
            if a.prg_len as usize != self.bus.prg_rom_len() {
                return Err(CoreError::Other(format!(
                    "atlas is for a {}-byte PRG, this cartridge has {}",
                    a.prg_len,
                    self.bus.prg_rom_len()
                )));
            }
        }
        self.rebuild = atlas.map(|a| {
            let mut x = Box::new(RebuildExec::new(a));
            x.set_hooked(&self.hook_ids);
            x
        });
        self.refresh_observing();
        Ok(())
    }

    /// Dispatch misses seen so far (distinct, first-seen order); empty when
    /// Rebuild is off.
    #[must_use]
    pub fn rebuild_deltas(&self) -> &[Delta] {
        self.rebuild.as_deref().map_or(&[], RebuildExec::deltas)
    }

    /// Keep the last `capacity` executed steps on the rebuilt path (ticket
    /// W36-04's repro bundles); `0` turns it off. No effect while Rebuild is
    /// off.
    pub fn set_rebuild_trace(&mut self, capacity: usize) {
        if let Some(x) = self.rebuild.as_deref_mut() {
            x.enable_trace(capacity);
        }
    }

    /// The last executed steps with atlas function ids, oldest first.
    #[must_use]
    pub fn rebuild_trace(&self) -> Vec<TraceEntry> {
        self.rebuild
            .as_deref()
            .map_or_else(Vec::new, RebuildExec::trace)
    }

    /// The installed atlas, for naming trace entries' functions.
    #[must_use]
    pub fn rebuild_atlas(&self) -> Option<&Atlas> {
        self.rebuild.as_deref().map(RebuildExec::atlas)
    }

    /// A [`CodeAddr`] for `addr` under the bank state in force right now.
    fn code_addr(&self, addr: u16) -> CodeAddr {
        CodeAddr {
            cpu: addr,
            pbr: 0,
            mode: 0,
            rom: self
                .bus
                .prg_rom_offset(addr)
                .and_then(|o| u32::try_from(o).ok()),
        }
    }

    /// Recorder, pre-step half (ticket W36-01): classify what the next
    /// `cpu.step` is about to do and mark the instruction's bytes. Peeks
    /// only (`NesBus::peek` is side-effect-free), so the emulated machine
    /// cannot tell it ran.
    #[inline(never)]
    fn coverage_before(&mut self) -> CoverageBefore {
        let pc = self.cpu.pc;
        let (nmi, irq) = self.cpu.pending_interrupt();
        if nmi || irq {
            let kind = if nmi {
                InterruptKind::Nmi
            } else {
                InterruptKind::Irq
            };
            return CoverageBefore::Interrupt(kind);
        }
        let opcode = self.bus.peek(pc);
        let len = crate::trace::instruction_len(opcode);
        let mut marks = [(0u16, None, 0u8); 3];
        for (i, slot) in marks.iter_mut().enumerate().take(usize::from(len)) {
            let addr = pc.wrapping_add(i as u16);
            let flag = if i == 0 { OPCODE } else { OPERAND };
            *slot = (addr, self.bus.prg_rom_offset(addr), flag);
        }
        let site = self.code_addr(pc);
        if let Some(o) = marks[0].1 {
            self.coverage_note_window(pc, o);
        }
        if let Some(log) = self.coverage.as_deref_mut() {
            log.instructions_recorded += 1;
            for &(addr, rom, flag) in marks.iter().take(usize::from(len)) {
                match rom {
                    Some(o) => log.mark_prg(o, flag),
                    None => log.mark_ram(u32::from(addr), flag),
                }
            }
        }
        CoverageBefore::Instruction { opcode, site }
    }

    /// Recorder, window half (ticket W36-02): `pc` executes from PRG offset
    /// `rom`. Costs one compare per instruction; only when that page's base
    /// differs from the cached map (first sight of a bank, or a bank switch)
    /// does it peek all eight windows and log the whole simultaneous
    /// mapping. Pages that never execute are still captured by that
    /// snapshot, which is how an unexecuted call target's bank is learned.
    #[inline]
    fn coverage_note_window(&mut self, pc: u16, rom: usize) {
        let Some(page) = WindowMap::page_of(pc) else {
            return;
        };
        let Some(base) = rom
            .checked_sub(usize::from(pc) & ((1 << WINDOW_PAGE_SHIFT) - 1))
            .and_then(|b| u32::try_from(b).ok())
        else {
            return;
        };
        if self.window_cache.0[page] == base {
            return;
        }
        self.coverage_snapshot_windows();
    }

    /// Peek every window's current PRG base into the cache and the log.
    #[inline(never)]
    fn coverage_snapshot_windows(&mut self) {
        let mut map = WindowMap::EMPTY;
        for (page, slot) in map.0.iter_mut().enumerate().take(WINDOW_PAGES) {
            let addr = WINDOW_BASE + ((page as u16) << WINDOW_PAGE_SHIFT);
            if let Some(o) = self.bus.prg_rom_offset(addr) {
                *slot = u32::try_from(o).unwrap_or(rf_core_api::coverage::NO_ROM);
            }
        }
        self.window_cache = map;
        if let Some(log) = self.coverage.as_deref_mut() {
            log.note_windows(map);
        }
    }

    /// Recorder, post-step half: the indirect edge or interrupt entry the
    /// step just completed, with its target read off the new `pc`.
    #[inline(never)]
    fn coverage_after(&mut self, before: CoverageBefore) {
        let target = self.code_addr(self.cpu.pc);
        let Some(log) = self.coverage.as_deref_mut() else {
            return;
        };
        match before {
            CoverageBefore::Interrupt(kind) => log.note_interrupt(InterruptEntry { kind, target }),
            CoverageBefore::Instruction { opcode, site } => {
                let kind = match opcode {
                    0x6C => IndirectKind::JmpIndirect,
                    0x60 => IndirectKind::Rts,
                    0x40 => IndirectKind::Rti,
                    0x00 => IndirectKind::Brk,
                    _ => return,
                };
                log.note_indirect(IndirectEdge { site, target, kind });
            }
        }
    }

    /// Note a reset entry at the CPU's current `pc` (ticket W36-01).
    fn coverage_note_reset(&mut self) {
        let target = self.code_addr(self.cpu.pc);
        if let Some(log) = self.coverage.as_deref_mut() {
            log.note_interrupt(InterruptEntry {
                kind: InterruptKind::Reset,
                target,
            });
        }
    }
}

/// What the executor found before a step (tickets W36-03, W36-05).
#[derive(Clone, Copy)]
struct RebuildBefore {
    /// An indirect transfer to compare with the atlas's recorded targets.
    indirect: Option<(u32, CodeAddr)>,
    hook: HookStep,
}

impl RebuildBefore {
    const PLAIN: Self = Self {
        indirect: None,
        hook: HookStep::None,
    };
}

/// What the step about to run does to the hooked call stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HookStep {
    None,
    /// `JSR`, `BRK` or an interrupt entry; `ret` is where its return lands.
    Enter {
        ret: u32,
    },
    /// `RTS` or `RTI`.
    Return,
}

/// The machine lent to a hook for one event: registers, a non-perturbing
/// peek, and a clock-free poke into work RAM and cartridge RAM.
struct NesHookCtx<'a> {
    cpu: &'a Cpu,
    bus: &'a mut NesBus,
}

impl rf_core_api::HookCtx for NesHookCtx<'_> {
    fn regs(&self) -> CpuRegs {
        CpuRegs::Mos6502(Mos6502Regs {
            a: self.cpu.a,
            x: self.cpu.x,
            y: self.cpu.y,
            s: self.cpu.s,
            pc: self.cpu.pc,
            p: self.cpu.p,
        })
    }

    fn peek(&self, addr: u32) -> u8 {
        self.bus.peek(u16::try_from(addr & 0xFFFF).unwrap_or(0))
    }

    fn poke(&mut self, addr: u32, value: u8) -> bool {
        u16::try_from(addr).is_ok_and(|a| self.bus.poke(a, value))
    }
}

/// What the recorder saw before a step, carried to the after-step half.
#[derive(Clone, Copy)]
enum CoverageBefore {
    Interrupt(InterruptKind),
    Instruction { opcode: u8, site: CodeAddr },
}

impl EmulatorCore for NesCore {
    fn load(&mut self, cart: CartImage<'_>) -> Result<(), CoreError> {
        let mut bus = NesBus::from_ines_bytes(cart.rom)
            .map_err(|e| CoreError::InvalidImage(format!("{e:?}")))?;
        // A machine setting, not cartridge state: it survives a reload.
        bus.set_extra_vblank_lines(self.bus.extra_vblank_lines());
        self.cpu = Cpu::power_on(&mut bus);
        self.bus = bus;
        // A new cartridge invalidates a log sized for the old one; keep
        // recording, from scratch.
        if self.coverage.is_some() {
            self.coverage = Some(Box::new(CoverageLog::new(self.bus.prg_rom_len())));
            self.window_cache = WindowMap::EMPTY;
            self.coverage_note_reset();
        }
        // An atlas describes one cartridge, and so do the function ids hooked
        // in it.
        self.rebuild = None;
        self.hook_ids.clear();
        self.refresh_observing();
        Ok(())
    }

    fn reset(&mut self, _kind: ResetKind) {
        // Soft and hard are the same here for now, and saying so beats
        // pretending otherwise: `Cpu::power_on` re-reads the reset vector
        // through the bus, which is what both kinds do on real hardware;
        // what a hard reset would additionally clear (RAM to its
        // power-on pattern) `rf-nes` does not model separately today.
        self.cpu = Cpu::power_on(&mut self.bus);
        self.coverage_note_reset();
        self.reset_hook_frames();
    }

    fn run_frame(&mut self, input: &InputFrame, sink: &mut dyn CoreSink) {
        // The same two-port latch `retroforge::stepper` performed before
        // W11-10 — moved here because which buttons reach the machine is
        // core knowledge, not shell knowledge.
        self.bus.set_controller_buttons(0, input.ports[0] as u8);
        self.bus.set_controller_buttons(1, input.ports[1] as u8);
        let _ = self.step(Step::Frame, sink);
    }

    fn step(&mut self, granularity: Step, sink: &mut dyn CoreSink) -> StepResult {
        // `CoreConfig` is documented as the path a consumer configures a
        // core through, and this is what makes that true for watchpoints:
        // a caller sets `core.config().watches` and the next step picks it
        // up, with no second setter to keep in sync (ticket W13-02e).
        if self.config.watches != self.pushed_watches {
            self.bus.set_watches(self.config.watches);
            self.pushed_watches = self.config.watches;
        }
        let start_frame = self.bus.frame_count();
        let start_cycle = self.bus.master_cycle();
        let deadline = start_cycle + self.cycle_budget;
        let mut frame_complete = false;

        match granularity {
            Step::Instruction => {
                frame_complete = self.tick(sink, start_frame);
            }
            Step::Scanline | Step::Frame => {
                // Scanlines are COUNTED THROUGH THE SINK, not read off the
                // PPU. `NesBus` deliberately exposes no `scanline()`
                // accessor — the sink contract already hands us `y`, and
                // `retroforge::stepper` has counted them this way since
                // W1-06. Reaching into PPU-internal scanline/dot state to
                // get a second answer would be a second definition of
                // "a scanline".
                let want_scanline = matches!(granularity, Step::Scanline);
                let mut counting = ScanlineCounter {
                    inner: sink,
                    seen: 0,
                };
                // **A scanline step does not stop at a frame boundary.**
                // It stops when a scanline has been DRAINED, which is not
                // the same thing: the first frame after power-on ends
                // BEFORE any visible scanline is produced (the boot
                // artifact), so breaking on the boundary returns "zero
                // scanlines" for a step that had simply not got there
                // yet. `retroforge::stepper` looped on `scanlines > 0`
                // alone from W1-06, and its test caught this the moment
                // W11-10 changed it.
                while self.bus.master_cycle() < deadline {
                    let crossed = self.tick(&mut counting, start_frame);
                    frame_complete |= crossed;
                    if want_scanline {
                        if counting.seen > 0 {
                            break;
                        }
                    } else if crossed {
                        break;
                    }
                }
            }
        }

        StepResult {
            cycles: self.bus.master_cycle() - start_cycle,
            frame_complete,
        }
    }

    fn save_state(&self, _w: &mut dyn StateWriter) -> Result<(), StateError> {
        // Deliberately unimplemented here, and named rather than faked
        // (ticket W11-10's notes). The shipped save-state format is
        // `rf_state`'s `Container` — slots, metadata, PROF chunks, a
        // checked-in golden fixture — and it is built by
        // `retroforge::save_state` over this core, not by this trait
        // method. Routing it through `StateWriter` is a real piece of
        // work with a golden fixture to keep byte-identical, and doing it
        // as a side effect of adopting the trait is how that fixture
        // would quietly change.
        Err(StateError::Io(
            "NES state is carried by rf_state::Container, not this trait method (W11-10)"
                .to_string(),
        ))
    }

    fn load_state(&mut self, _r: &mut dyn StateReader) -> Result<(), StateError> {
        Err(StateError::Io(
            "NES state is carried by rf_state::Container, not this trait method (W11-10)"
                .to_string(),
        ))
    }

    fn state_view(&self) -> StateView<'_> {
        StateView {
            cpu_regs: CpuRegs::Mos6502(Mos6502Regs {
                a: self.cpu.a,
                x: self.cpu.x,
                y: self.cpu.y,
                s: self.cpu.s,
                pc: self.cpu.pc,
                p: self.cpu.p,
            }),
            wram: self.bus.ram(),
            vram: self.bus.vram(),
            cgram: self.bus.palette(),
            oam: self.bus.oam(),
            ppu_regs: &[],
            mapper_state: self.bus.prg_ram(),
        }
    }

    fn config(&mut self) -> &mut CoreConfig {
        &mut self.config
    }

    fn peek(&self, addr: u32) -> u8 {
        // `NesBus::peek` is the non-perturbing read the debugger has used
        // since W1-07 — it deliberately does not touch the PPU's
        // read-latch or any mapper side effect.
        self.bus.peek(u16::try_from(addr & 0xFFFF).unwrap_or(0))
    }

    fn set_coverage_recording(&mut self, on: bool) {
        self.coverage = on.then(|| Box::new(CoverageLog::new(self.bus.prg_rom_len())));
        self.window_cache = WindowMap::EMPTY;
        self.refresh_observing();
    }

    fn set_rebuild_atlas(
        &mut self,
        atlas: Option<Box<dyn std::any::Any + Send>>,
    ) -> Result<(), CoreError> {
        let atlas = match atlas {
            None => None,
            Some(b) => Some(
                *b.downcast::<Atlas>()
                    .map_err(|_| CoreError::Other("not an rf-rebuild atlas".to_string()))?,
            ),
        };
        self.set_rebuild(atlas)
    }

    fn set_function_hooks(&mut self, function_ids: &[u32]) {
        self.hook_ids = function_ids.to_vec();
        if let Some(x) = self.rebuild.as_deref_mut() {
            x.set_hooked(function_ids);
        }
    }

    fn rebuild_stats(&self) -> Option<rf_core_api::RebuildStats> {
        self.rebuild.as_deref().map(RebuildExec::stats)
    }

    fn take_coverage(&mut self) -> Option<CoverageLog> {
        let log = self.coverage.as_deref_mut()?;
        let fresh = CoverageLog::new(log.prg_flags.len());
        self.window_cache = WindowMap::EMPTY;
        Some(std::mem::replace(log, fresh))
    }
}

/// Counts scanlines as they pass through to the real sink.
///
/// A copy of `retroforge::stepper`'s `CountingSink` in the only part that
/// matters here — and it must forward EVERY method explicitly rather than
/// relying on defaults, because a silently-dropped `overlay_scanline`
/// would make the sprite-limit overlay vanish only while single-stepping.
struct ScanlineCounter<'a> {
    inner: &'a mut dyn CoreSink,
    seen: u32,
}

impl CoreSink for ScanlineCounter<'_> {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.seen += 1;
        self.inner.video_scanline(y, pixels);
    }

    fn overlay_scanline(&mut self, y: u16, pixels: &[rf_core_api::OverlayPixel]) {
        self.inner.overlay_scanline(y, pixels);
    }

    fn audio(&mut self, samples: &[i16]) {
        self.inner.audio(samples);
    }

    fn event(&mut self, event: rf_core_api::CoreEvent) {
        self.inner.event(event);
    }

    /// Forwarded explicitly: `run_frame` goes through this wrapper, so a
    /// default here would mean hooks never fire in normal play (W36-05).
    fn function_event(
        &mut self,
        ev: rf_core_api::FunctionEvent,
        ctx: &mut dyn rf_core_api::HookCtx,
    ) {
        self.inner.function_event(ev, ctx);
    }
}
