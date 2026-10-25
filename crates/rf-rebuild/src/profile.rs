//! The understand pass, observation half (ticket W36-09;
//! `docs/design/REBUILD_LAB.md` section 5, step 5): run the game under the
//! block executor with every atlas function hooked and a per-instruction
//! observer, and build a behaviour profile of each function from what it
//! does to the machine.
//!
//! A [`FunctionProfile`] is **behaviour, not code**: how often the function
//! ran, in which interrupt context, who called it, which hardware registers
//! it touched (by class and address, with counts), which RAM it read and
//! wrote. It contains no byte of the ROM (law 5, D-021) and nothing a
//! decompiler could use, which is also what lets the opt-in AI naming pass
//! (D-026) send it to a cloud model without breaking D-024.
//!
//! The run is an analysis run: it steps every instruction through
//! [`rf_core_api::EmulatorCore::run_frame_observed`], which is far slower than
//! playing, and it never touches the shipped frame path.
//!
//! ## Attribution
//!
//! - An access belongs to the atlas function the executing instruction was in
//!   (the first function owning its block). Code the atlas has no node for
//!   (RAM code, unseen ROM) is charged to the innermost hooked caller.
//! - The call stack is mirrored from the executor's Enter/Exit events, so
//!   every function is hooked. The executor bounds its own stack at
//!   [`crate::MAX_SHADOW_DEPTH`]; a program that nests deeper loses its
//!   oldest frames and the mirror then misses their Exits (it searches for
//!   the leaving frame, so it cannot desynchronise, only forget).
//! - Context (main / NMI / IRQ) is the innermost interrupt frame on that
//!   stack at the time of the instruction. An interrupt frame is a function
//!   entered by an interrupt sequence; its kind comes from [`Vectors`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use rf_core_api::{
    BusAccess, CoreError, CoreEvent, CoreSink, EmulatorCore, FunctionEvent, HookCtx, InputFrame,
    PpuPixel, StepFacts, StepObserver,
};

use crate::atlas::{entry, Atlas};
use crate::hw::{classify, note_value, Hit, HwClass, Platform};

/// A set of addresses stored as sorted, disjoint, non-touching inclusive
/// ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeSet(Vec<(u32, u32)>);

impl RangeSet {
    pub fn insert(&mut self, a: u32) {
        // Ranges are sorted and disjoint, so `end + 1 < a` holds for a
        // prefix: `i` is the first range that contains, touches or follows `a`.
        let i = self.0.partition_point(|&(_, e)| e.saturating_add(1) < a);
        match self.0.get(i).copied() {
            Some((s, e)) if s <= a && a <= e => {}
            Some((_, e)) if e.checked_add(1) == Some(a) => {
                let next = self.0.get(i + 1).copied();
                self.0[i].1 = a;
                if let Some((ns, ne)) = next {
                    if ns <= a.saturating_add(1) {
                        self.0[i].1 = ne;
                        self.0.remove(i + 1);
                    }
                }
            }
            Some((s, _)) if s == a.wrapping_add(1) => self.0[i].0 = a,
            _ => self.0.insert(i, (a, a)),
        }
    }

    /// The ranges, ascending.
    #[must_use]
    pub fn ranges(&self) -> &[(u32, u32)] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of addresses in the set.
    #[must_use]
    pub fn byte_count(&self) -> u64 {
        self.0.iter().map(|&(s, e)| u64::from(e - s) + 1).sum()
    }

    #[must_use]
    pub fn contains(&self, a: u32) -> bool {
        let i = self.0.partition_point(|&(_, e)| e < a);
        self.0.get(i).is_some_and(|&(s, _)| s <= a)
    }
}

/// Where the interrupt vectors point on this machine, as CPU addresses read
/// from the running cartridge (addresses, not code). A SNES lists both the
/// native and the emulation-mode vector.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vectors {
    pub reset: Vec<u32>,
    pub nmi: Vec<u32>,
    pub irq: Vec<u32>,
}

impl Vectors {
    /// Read the vectors of a console from the machine's CPU bus.
    ///
    /// NES: `$FFFA` NMI, `$FFFC` reset, `$FFFE` IRQ/BRK (nesdev "CPU
    /// interrupts"). SNES: native `$FFEA` NMI, `$FFEE` IRQ; emulation
    /// `$FFFA` NMI, `$FFFC` reset, `$FFFE` IRQ/BRK (fullsnes "SNES Interrupt
    /// Vectors"); the SNES values are bank-0 addresses.
    #[must_use]
    pub fn read<M: EmulatorCore + ?Sized>(platform: Platform, core: &M) -> Self {
        let word = |at: u32| u32::from(core.peek(at)) | (u32::from(core.peek(at + 1)) << 8);
        match platform {
            Platform::Nes => Self {
                reset: vec![word(0xFFFC)],
                nmi: vec![word(0xFFFA)],
                irq: vec![word(0xFFFE)],
            },
            Platform::Snes => Self {
                reset: vec![word(0xFFFC)],
                nmi: vec![word(0xFFEA), word(0xFFFA)],
                irq: vec![word(0xFFEE), word(0xFFFE)],
            },
        }
    }

    fn is_nmi(&self, entry_cpu: u32) -> bool {
        self.nmi.contains(&(entry_cpu & 0xFFFF))
    }

    fn is_irq(&self, entry_cpu: u32) -> bool {
        self.irq.contains(&(entry_cpu & 0xFFFF))
    }
}

/// Access counts of one hardware register by one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RegUse {
    pub reads: u64,
    pub writes: u64,
}

/// What one atlas function was seen doing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FunctionProfile {
    /// Instructions executed in this function (first-owner attribution).
    pub insns: u64,
    /// Arrivals at its first instruction by any route (call, jump, fall-in).
    pub entries: u64,
    /// Arrivals by a call or an interrupt sequence (hooked Enter events).
    pub calls: u64,
    /// Of `calls`, how many were interrupt sequences.
    pub nmi_entries: u64,
    pub irq_entries: u64,
    /// Observed frames in which it executed at least one instruction.
    pub frames_active: u32,
    /// Instructions by context.
    pub main_insns: u64,
    pub nmi_insns: u64,
    pub irq_insns: u64,
    /// Hardware registers touched, by normalised register address.
    pub regs: BTreeMap<u16, (HwClass, RegUse)>,
    /// RAM addresses (folded; see [`crate::hw::Hit::Ram`]) read and written.
    pub ram_read: RangeSet,
    pub ram_write: RangeSet,
    pub ram_reads: u64,
    pub ram_writes: u64,
    /// Calls out, by callee atlas function id.
    pub callees: BTreeMap<u32, u64>,
    /// Calls in, by caller atlas function id.
    pub callers: BTreeMap<u32, u64>,
    /// Value-derived facts: [`crate::hw::flag`] bits.
    pub flags: u32,
}

impl FunctionProfile {
    /// Reads and writes summed over the registers of `class`.
    #[must_use]
    pub fn class_use(&self, class: HwClass) -> RegUse {
        let mut out = RegUse::default();
        for (c, u) in self.regs.values() {
            if *c == class {
                out.reads += u.reads;
                out.writes += u.writes;
            }
        }
        out
    }

    /// Use of one register (zero when never touched).
    #[must_use]
    pub fn reg(&self, reg: u16) -> RegUse {
        self.regs.get(&reg).map(|(_, u)| *u).unwrap_or_default()
    }

    /// Use summed over registers `lo..=hi` (inclusive).
    #[must_use]
    pub fn reg_range(&self, lo: u16, hi: u16) -> RegUse {
        let mut out = RegUse::default();
        for (_, (_, u)) in self.regs.range(lo..=hi) {
            out.reads += u.reads;
            out.writes += u.writes;
        }
        out
    }

    /// Whether anything was observed of this function at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.insns == 0 && self.entries == 0 && self.calls == 0
    }
}

/// The result of an understand run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Understanding {
    pub platform: Platform,
    /// Observed frames.
    pub frames: u32,
    /// One profile per atlas function, same indices as [`Atlas::functions`].
    pub functions: Vec<FunctionProfile>,
    pub vectors: Vectors,
    /// Instructions the atlas had no node for (charged to the caller).
    pub unattributed_insns: u64,
}

impl Understanding {
    /// Functions with any observed activity.
    #[must_use]
    pub fn observed_functions(&self) -> usize {
        self.functions.iter().filter(|f| !f.is_empty()).count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Main,
    Nmi,
    Irq,
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    function: u32,
    kind: Ctx,
}

/// Events from the hooked core, shared between the sink the core is given and
/// the observer that consumes them (single-threaded).
type Events = Rc<RefCell<Vec<FunctionEvent>>>;

/// The sink handed to `run_frame_observed`: keeps hook events, drops the
/// picture and sound.
pub struct ProfilerSink {
    events: Events,
}

impl CoreSink for ProfilerSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
    fn function_event(&mut self, ev: FunctionEvent, _ctx: &mut dyn HookCtx) {
        self.events.borrow_mut().push(ev);
    }
}

/// Builds an [`Understanding`] from the instruction stream
/// (`StepObserver`) and the hook events (`ProfilerSink`).
pub struct Profiler<'a> {
    platform: Platform,
    atlas: &'a Atlas,
    vectors: Vectors,
    events: Events,
    functions: Vec<FunctionProfile>,
    last_frame: Vec<u32>,
    stack: Vec<Frame>,
    nmi_depth: u32,
    irq_depth: u32,
    frame: u32,
    unattributed: u64,
}

impl<'a> Profiler<'a> {
    /// A profiler and the sink to hand to `run_frame_observed`.
    #[must_use]
    pub fn new(platform: Platform, atlas: &'a Atlas, vectors: Vectors) -> (Self, ProfilerSink) {
        let events: Events = Rc::default();
        let n = atlas.functions.len();
        (
            Self {
                platform,
                atlas,
                vectors,
                events: Rc::clone(&events),
                functions: vec![FunctionProfile::default(); n],
                last_frame: vec![u32::MAX; n],
                stack: Vec::new(),
                nmi_depth: 0,
                irq_depth: 0,
                frame: 0,
                unattributed: 0,
            },
            ProfilerSink { events },
        )
    }

    fn ctx(&self) -> Ctx {
        if self.nmi_depth > 0 {
            Ctx::Nmi
        } else if self.irq_depth > 0 {
            Ctx::Irq
        } else {
            Ctx::Main
        }
    }

    fn apply_events(&mut self, interrupt_step: bool) {
        let events = std::mem::take(&mut *self.events.borrow_mut());
        let mut first = true;
        for ev in events {
            match ev {
                FunctionEvent::Enter { function } => {
                    let interrupt = interrupt_step && first;
                    first = false;
                    self.enter(function, interrupt);
                }
                FunctionEvent::Exit { function } => self.exit(function),
            }
        }
    }

    fn enter(&mut self, function: u32, interrupt: bool) {
        let Some(f) = self.atlas.functions.get(function as usize) else {
            return;
        };
        let entry_cpu = f.entry_cpu;
        let kind = if !interrupt {
            Ctx::Main
        } else if self.vectors.is_nmi(entry_cpu) {
            Ctx::Nmi
        } else if self.vectors.is_irq(entry_cpu) {
            Ctx::Irq
        } else {
            // BRK/COP and anything else an interrupt sequence reached.
            Ctx::Irq
        };
        let caller = self.stack.last().map(|fr| fr.function);
        let p = &mut self.functions[function as usize];
        p.calls += 1;
        match (interrupt, kind) {
            (true, Ctx::Nmi) => p.nmi_entries += 1,
            (true, _) => p.irq_entries += 1,
            _ => {}
        }
        if let Some(c) = caller {
            *p.callers.entry(c).or_default() += 1;
            *self.functions[c as usize]
                .callees
                .entry(function)
                .or_default() += 1;
        }
        match (interrupt, kind) {
            (true, Ctx::Nmi) => self.nmi_depth += 1,
            (true, _) => self.irq_depth += 1,
            _ => {}
        }
        self.stack.push(Frame {
            function,
            kind: if interrupt { kind } else { Ctx::Main },
        });
    }

    fn exit(&mut self, function: u32) {
        // Exits arrive innermost first, so the top frame is the one leaving;
        // search anyway so a dropped (depth-bounded) frame cannot desync us.
        let Some(at) = self.stack.iter().rposition(|fr| fr.function == function) else {
            return;
        };
        for fr in self.stack.drain(at..) {
            match fr.kind {
                Ctx::Nmi => self.nmi_depth = self.nmi_depth.saturating_sub(1),
                Ctx::Irq => self.irq_depth = self.irq_depth.saturating_sub(1),
                Ctx::Main => {}
            }
        }
    }

    fn account(&mut self, owner: usize, a: BusAccess) {
        let platform = self.platform;
        let p = &mut self.functions[owner];
        match classify(platform, a.addr, a.write) {
            Hit::Reg { class, reg } => {
                let (_, u) = p.regs.entry(reg).or_insert((class, RegUse::default()));
                if a.write {
                    u.writes += 1;
                } else {
                    u.reads += 1;
                }
                note_value(platform, &mut p.flags, reg, a.write, a.value);
            }
            Hit::Ram { addr } => {
                if a.write {
                    p.ram_writes += 1;
                    p.ram_write.insert(addr);
                } else {
                    p.ram_reads += 1;
                    p.ram_read.insert(addr);
                }
            }
            Hit::Stack | Hit::Ignore => {}
        }
    }

    /// Finish: the profiles and run totals.
    #[must_use]
    pub fn finish(mut self) -> Understanding {
        // Anything still pending belongs to a frame boundary that never came.
        self.apply_events(false);
        Understanding {
            platform: self.platform,
            frames: self.frame,
            functions: self.functions,
            vectors: self.vectors,
            unattributed_insns: self.unattributed,
        }
    }
}

impl StepObserver for Profiler<'_> {
    fn step(&mut self, facts: StepFacts, accesses: &[BusAccess]) {
        let ctx = self.ctx();
        let owner = facts
            .function
            .or_else(|| self.stack.last().map(|fr| fr.function))
            .map(|f| f as usize)
            .filter(|&f| f < self.functions.len());
        if !facts.interrupt {
            // The function the machine was already running at the first
            // observed instruction (the reset code, usually) was never
            // Entered: seed it as the root so its calls have a caller.
            if self.stack.is_empty() {
                if let Some(f) = facts.function {
                    self.stack.push(Frame {
                        function: f,
                        kind: Ctx::Main,
                    });
                }
            }
            match (facts.function, owner) {
                (Some(_), Some(o)) => {
                    let p = &mut self.functions[o];
                    p.insns += 1;
                    match ctx {
                        Ctx::Main => p.main_insns += 1,
                        Ctx::Nmi => p.nmi_insns += 1,
                        Ctx::Irq => p.irq_insns += 1,
                    }
                    if self.last_frame[o] != self.frame {
                        self.last_frame[o] = self.frame;
                        p.frames_active += 1;
                    }
                }
                _ => self.unattributed += 1,
            }
            if let Some(e) = facts.entry_of {
                if let Some(p) = self.functions.get_mut(e as usize) {
                    p.entries += 1;
                }
            }
            if let Some(o) = owner {
                for &a in accesses {
                    self.account(o, a);
                }
            }
        }
        self.apply_events(facts.interrupt);
    }

    fn frame_end(&mut self) {
        self.frame += 1;
    }
}

/// Why an understand run could not start.
#[derive(Debug)]
pub enum ObserveError {
    /// The atlas does not fit this cartridge, or the core has no executor.
    Atlas(CoreError),
    /// The core does not implement observed frames.
    Unsupported,
}

impl std::fmt::Display for ObserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(e) => write!(f, "atlas rejected: {e:?}"),
            Self::Unsupported => write!(f, "this core cannot be observed"),
        }
    }
}

impl std::error::Error for ObserveError {}

/// Run `inputs` (one entry per frame) through `core` under `atlas`, every
/// function hooked, and return what each function was seen doing.
///
/// `before_frame(core, index, input)` is the caller's chance to latch the
/// pad the way its console needs (the SNES `run_frame` ignores `input`);
/// it must be deterministic. The core should be freshly reset (or restored);
/// its executor is replaced by one built from `atlas`.
///
/// # Errors
/// [`ObserveError`] when the atlas does not fit or the core cannot be
/// observed.
pub fn observe<M: EmulatorCore + ?Sized>(
    core: &mut M,
    platform: Platform,
    atlas: &Atlas,
    inputs: impl IntoIterator<Item = InputFrame>,
    mut before_frame: impl FnMut(&mut M, u32, &InputFrame),
) -> Result<Understanding, ObserveError> {
    core.set_rebuild_atlas(Some(Box::new(atlas.clone())))
        .map_err(ObserveError::Atlas)?;
    let all: Vec<u32> = (0..atlas.functions.len() as u32).collect();
    core.set_function_hooks(&all);
    let vectors = Vectors::read(platform, &*core);
    let (mut profiler, mut sink) = Profiler::new(platform, atlas, vectors);
    for (i, input) in (0u32..).zip(inputs) {
        before_frame(core, i, &input);
        if !core.run_frame_observed(&input, &mut sink, &mut profiler) {
            return Err(ObserveError::Unsupported);
        }
    }
    Ok(profiler.finish())
}

/// Whether a function is a hardware-interrupt entry in the atlas.
#[must_use]
pub fn is_interrupt_entry(atlas: &Atlas, function: u32) -> bool {
    atlas
        .functions
        .get(function as usize)
        .is_some_and(|f| f.entry_kinds & entry::INTERRUPT != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_set_merges_touching_addresses_and_stays_sorted() {
        let mut r = RangeSet::default();
        for a in [5, 7, 6, 1, 2, 10, 3, 9, 8, 4] {
            r.insert(a);
        }
        assert_eq!(r.ranges(), &[(1, 10)]);
        r.insert(12);
        r.insert(12);
        r.insert(0);
        assert_eq!(r.ranges(), &[(0, 10), (12, 12)]);
        assert_eq!(r.byte_count(), 12);
        assert!(r.contains(0) && r.contains(10) && !r.contains(11) && r.contains(12));
        r.insert(11);
        assert_eq!(r.ranges(), &[(0, 12)]);
    }

    #[test]
    fn range_set_handles_the_ends_of_the_address_space() {
        let mut r = RangeSet::default();
        r.insert(u32::MAX);
        r.insert(0);
        r.insert(u32::MAX - 1);
        assert_eq!(r.ranges(), &[(0, 0), (u32::MAX - 1, u32::MAX)]);
    }
}
