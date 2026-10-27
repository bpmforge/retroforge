//! `rf_core_api::EmulatorCore` for the SNES (ticket W11-11).
//!
//! ## The shape problem this file solves
//!
//! The two consoles produce a frame in opposite directions, and that —
//! not the CPU, not the PPU — is why one shell could never drive both.
//!
//! `rf-nes` **pushes**: the PPU queues completed scanlines as the CPU
//! runs, and `drain_video` hands them to a `CoreSink` mid-frame. That is
//! what lets layer extraction, the compare view and the debugger watch a
//! frame being built.
//!
//! `rf-snes` **pulls**: [`crate::SnesSystem::render_frame`] runs the CPU
//! to a settled vblank and *then* asks the PPU for each visible line via
//! `Ppu::render_scanline`. Composition on a settled frame is deliberate
//! (ticket W7-06) — mid-frame register writes are the norm on this
//! machine, and rendering as you go would compose half-written state.
//!
//! So this adapter runs the frame, then replays the settled picture into
//! the sink line by line. The sink sees the same sequence it would from a
//! pushing core; what it does not see is the *timing* of those lines,
//! which for a settled-composition renderer does not exist to report.
//! Recorded plainly because a future mid-frame-effects feature will care.
//!
//! ## Visible lines are 224 **or 239**
//!
//! `SETINI`'s overscan bit decides, and a core that always emitted 224
//! would silently crop the bottom fifteen rows of an overscan game rather
//! than letterbox it (ticket W7-06). W11-08 is what made this
//! expressible at all: the display path carried a hardcoded 256x240 until
//! then, so a 256x224 frame had nowhere to live.

use rf_core_api::{
    CartImage, CoreConfig, CoreError, CoreEvent, CoreSink, EmulatorCore, EventMask, InputFrame,
    ResetKind, StateError, StateReader, StateView, StateWriter, Step, StepResult,
};
use rf_core_api::{CpuRegs, Wdc65816Regs};

use crate::SnesSystem;

/// How many instructions a single frame may take before the adapter gives
/// up and reports that it did not complete one.
///
/// The same defence `rf_nes::core::CYCLE_BUDGET` provides, in the unit
/// this core counts in: a `STP` instruction, or a CPU spinning on a
/// register that will never change, must not be able to hang the caller.
/// In the app this runs inside the core thread's guarded closure, where
/// an unbounded loop would also block `CoreCommand::Shutdown` from ever
/// being drained — worse than the crash FM-01 exists to contain.
pub const FRAME_INSTRUCTION_BUDGET: u64 = 4_000_000;

/// The machine lent to a function hook for one event (ticket W36-08b):
/// registers, a non-perturbing peek, and a clock-free poke into work RAM and
/// cartridge RAM.
struct SnesHookCtx<'a> {
    system: &'a mut SnesSystem,
}

impl rf_core_api::HookCtx for SnesHookCtx<'_> {
    fn regs(&self) -> CpuRegs {
        let c = &self.system.cpu;
        CpuRegs::Wdc65816(Wdc65816Regs {
            a: c.a,
            x: c.x,
            y: c.y,
            sp: c.sp,
            d: c.d,
            dbr: c.dbr,
            pbr: c.pbr,
            pc: c.pc,
            p: c.p,
            e: c.e,
        })
    }

    fn peek(&self, addr: u32) -> u8 {
        crate::cpu::CpuBus::peek(&self.system.bus, addr & 0xFF_FFFF)
    }

    fn poke(&mut self, addr: u32, value: u8) -> bool {
        use crate::mapping::Target;
        let bus = &mut self.system.bus;
        let slot = match bus.target(addr & 0xFF_FFFF) {
            Target::Wram(o) => bus.wram.get_mut(o),
            Target::Sram(o) => bus.sram.get_mut(o),
            _ => None,
        };
        slot.map(|b| *b = value).is_some()
    }
}

/// The SNES as an [`EmulatorCore`].
/// Registers `$2100`-`$213F`, the range `StateView::ppu_regs` covers
/// (ticket W13-02a).
pub const PPU_REG_COUNT: usize = 0x40;

/// Ticket W30-01: the Super FX's own clock, i.e. no overclock.
pub const GSU_OVERCLOCK_DEFAULT: u16 = 100;
/// Ticket W30-01: the highest Super FX overclock offered, 8x.
pub const GSU_OVERCLOCK_MAX: u16 = 800;

/// Ticket W30-08: the SA-1's own clock, i.e. no overclock.
pub const SA1_OVERCLOCK_DEFAULT: u16 = 100;
/// Ticket W30-08: the highest SA-1 overclock offered, 4x.
pub const SA1_OVERCLOCK_MAX: u16 = 400;
/// Ticket W30-08: the console's own 65816 speed, i.e. no overclock.
pub const CPU_OVERCLOCK_DEFAULT: u16 = 100;
/// Ticket W30-08: the highest 65816 overclock offered, 4x.
pub const CPU_OVERCLOCK_MAX: u16 = 400;

pub struct SnesCore {
    system: SnesSystem,
    config: CoreConfig,
    /// Ticket W13-02e: what was last pushed to the bus, so the sync at the
    /// top of `step` copies only when the caller changed something.
    pushed_watches: rf_core_api::WatchTable,
    /// Ticket W13-02a: CGRAM as bytes, refreshed each step.
    ///
    /// `StateView` lends `&[u8]` and the PPU stores CGRAM as `[u16; 256]`
    /// — a word array cannot be lent as bytes without either `unsafe`
    /// (which this workspace forbids outright) or a buffer. So: a buffer,
    /// 512 bytes, rebuilt at the end of each step. Little-endian, which is
    /// the order `$2122` writes them in, so a viewer reading a pair back
    /// sees exactly what the ROM wrote.
    cgram_bytes: Vec<u8>,
    /// Ticket W13-02a: the PPU register file, reconstructed. See
    /// [`SnesCore::refresh_state_snapshots`] for the layout and for why
    /// this is rebuilt rather than shadowed at write time.
    ppu_regs: Vec<u8>,
    budget: u64,
    /// The widescreen request: output width and which layers may use it.
    ///
    /// **`WIDTH` means off, and off is the default** — a fresh core
    /// renders 4:3 and never reaches the widening path at all (law 6:
    /// Accuracy Mode is the reference and enhancements are opt-in
    /// overlays). The mask travels with the width because a refusal is
    /// part of the request, not a separate setting the renderer could
    /// forget to consult.
    widescreen: (usize, crate::ppu::WidenMask),
    /// Ticket W32-03: render the widened picture under the bsnes-hd
    /// widescreen convention (patched games that draw their own margins).
    /// Held here, like the overclocks, because a cartridge load or a state
    /// restore rebuilds the PPU; `emit_frame` pushes it down every frame.
    wide_convention: bool,
    /// Ticket W30-01: the Super FX overclock, in percent of the
    /// cartridge's own clock. Held here rather than only on
    /// [`SnesSystem`] because a cartridge load or a state restore rebuilds
    /// the system; `step` pushes it down every time, so there is one place
    /// the two can disagree and it is corrected before any GSU code runs.
    gsu_overclock_percent: u16,
    /// Ticket W30-08: the SA-1 overclock, held here for the same reason as
    /// [`Self::gsu_overclock_percent`].
    sa1_overclock_percent: u16,
    /// Ticket W30-08: the 65816 overclock, likewise.
    cpu_overclock_percent: u16,
    /// Ticket W30-09: how the S-DSP interpolates voices for the main mix
    /// (presentation only; the machine stays exact), held here and pushed
    /// down every step for the same reason as the overclocks.
    interpolation: crate::apu::dsp::Interpolation,
    /// Ticket W31-02: the Super FX polygon-stream observer's config, held
    /// here (not only on the GSU) for the same reason as the overclocks: a
    /// cartridge load or state restore can rebuild the system, and `step`
    /// reinstalls it. Read-only observation; never part of a save state.
    gsu_poly: Option<crate::gsu_observer::GsuPolyConfig>,
    /// Ticket W7-22: the S-DSP's 32 kHz stereo turned into the 48 kHz mono
    /// stream every core hands `CoreSink::audio`.
    audio: AudioConverter,
    /// Ticket W36-08b: the atlas function ids asked to emit
    /// [`CoreSink::function_event`], kept so a freshly installed atlas gets
    /// them. Session observation: not saved, dropped by `load`.
    hook_ids: Vec<u32>,
}

/// The S-DSP outputs stereo at 32 kHz (fullsnes "SNES APU DSP": one
/// sample every 32 SPC700 cycles of ~1.024 MHz); `CoreSink::audio` takes
/// mono at `rf_audio::CORE_SAMPLE_RATE`, 48 kHz — the rate the NES core
/// already decimates to itself (`rf_nes::apu::OUTPUT_SAMPLE_RATE`). So:
/// average the two channels, and interpolate linearly at 2/3 of an input
/// sample per output, exactly 3 outputs per 2 inputs. Stereo is lost —
/// the host ring is mono — which is recorded as a limit (ticket W7-22).
/// Ticket W30-09 lifts that limit for a sink that asks
/// (`CoreSink::wants_stereo`): the same resampling, per channel.
#[derive(Debug, Clone, Default)]
struct AudioConverter {
    /// The last input sample of the previous batch.
    prev: i32,
    /// Ticket W30-09: the same, per channel, for the stereo path. Both
    /// paths advance `thirds` identically and keep all three `prev`
    /// values current, so a sink that switches between them mid-run
    /// picks up in phase.
    prev_lr: (i32, i32),
    /// Position of the next output between `prev` and the next input, in
    /// thirds of an input sample: always `0..3`.
    thirds: u32,
    out: Vec<i16>,
    out_stereo: Vec<[i16; 2]>,
}

/// Output samples per input step, in thirds: 32 kHz -> 48 kHz is 3:2.
const THIRDS_PER_OUTPUT: u32 = 2;

impl AudioConverter {
    fn convert(&mut self, input: &[(i16, i16)]) -> &[i16] {
        self.out.clear();
        for &(l, r) in input {
            let cur = (i32::from(l) + i32::from(r)) / 2;
            // Each pass emits one output and advances by 2/3, so at most
            // two outputs per input: terminates (law 8).
            while self.thirds < 3 {
                let v = self.prev + (cur - self.prev) * self.thirds as i32 / 3;
                self.out.push(v as i16);
                self.thirds += THIRDS_PER_OUTPUT;
            }
            self.thirds -= 3;
            self.prev = cur;
            self.prev_lr = (i32::from(l), i32::from(r));
        }
        &self.out
    }

    /// Ticket W30-09: the same 3:2 linear scheme and phase as
    /// [`Self::convert`], on each channel separately (stereo is kept).
    fn convert_stereo(&mut self, input: &[(i16, i16)]) -> &[[i16; 2]] {
        self.out_stereo.clear();
        for &(l, r) in input {
            let (l, r) = (i32::from(l), i32::from(r));
            let (pl, pr) = self.prev_lr;
            // As in `convert`: at most two outputs per input (law 8).
            while self.thirds < 3 {
                let t = self.thirds as i32;
                self.out_stereo.push([
                    (pl + (l - pl) * t / 3) as i16,
                    (pr + (r - pr) * t / 3) as i16,
                ]);
                self.thirds += THIRDS_PER_OUTPUT;
            }
            self.thirds -= 3;
            self.prev_lr = (l, r);
            self.prev = (l + r) / 2;
        }
        &self.out_stereo
    }
}

impl SnesCore {
    /// Build from a raw ROM image (LoROM or HiROM, copier header or not —
    /// `rf_cart` decides).
    ///
    /// # Errors
    /// Whatever [`SnesSystem::load`] rejects.
    pub fn load(raw: &[u8]) -> Result<Self, rf_cart::CartError> {
        Ok(Self {
            system: SnesSystem::load(raw)?,
            config: CoreConfig::default(),
            pushed_watches: rf_core_api::WatchTable::new(),
            cgram_bytes: Vec::new(),
            ppu_regs: Vec::new(),
            budget: FRAME_INSTRUCTION_BUDGET,
            widescreen: (crate::ppu::WIDTH, crate::ppu::WidenMask::ALL),
            wide_convention: false,
            gsu_overclock_percent: GSU_OVERCLOCK_DEFAULT,
            sa1_overclock_percent: SA1_OVERCLOCK_DEFAULT,
            cpu_overclock_percent: CPU_OVERCLOCK_DEFAULT,
            interpolation: crate::apu::dsp::Interpolation::default(),
            gsu_poly: None,
            audio: AudioConverter::default(),
            hook_ids: Vec::new(),
        })
    }

    /// Borrow the machine, for the shell's out-of-band reads.
    #[must_use]
    pub fn system(&self) -> &SnesSystem {
        &self.system
    }

    /// Mutable access to the machine.
    pub fn system_mut(&mut self) -> &mut SnesSystem {
        &mut self.system
    }

    /// Shrink the instruction budget, so a termination test can drive the
    /// bound instead of asserting a constant exists.
    /// Ask for a widened picture, with a per-layer mask (ticket W11-03).
    ///
    /// `width <= WIDTH` turns widescreen OFF and restores the accuracy
    /// path exactly. Clamped to the PPU's `MAX_WIDTH`.
    pub fn set_widescreen(&mut self, width: usize, mask: crate::ppu::WidenMask) {
        self.widescreen = (width.clamp(crate::ppu::WIDTH, crate::ppu::MAX_WIDTH), mask);
    }

    /// Ticket W32-03: render widened lines under the bsnes-hd widescreen
    /// convention (sneslab "Widescreen"): sprite X read signed in
    /// `-128..=383`, window positions mapped `2v - 128`, every background
    /// widened (the patched game drew the margins on purpose, so the
    /// per-layer policies are ignored). Has no effect unless a width above
    /// 256 is also requested; off is the accuracy path exactly.
    pub fn set_wide_convention(&mut self, on: bool) {
        self.wide_convention = on;
    }

    /// Ticket W30-09: choose how the S-DSP interpolates voices for the main
    /// L/R mix. [`Interpolation::Gaussian`](crate::apu::dsp::Interpolation)
    /// is the hardware filter and the default; the others change only what
    /// reaches the speakers, never the echo, `OUTX`, pitch modulation or
    /// any state a save captures.
    pub fn set_interpolation(&mut self, mode: crate::apu::dsp::Interpolation) {
        self.interpolation = mode;
    }

    /// The interpolation mode in force.
    #[must_use]
    pub fn interpolation(&self) -> crate::apu::dsp::Interpolation {
        self.interpolation
    }

    /// Ticket W30-01: run the Super FX at `percent` of its own clock.
    ///
    /// Rounded down to a whole multiple of 100 and clamped to
    /// [`GSU_OVERCLOCK_DEFAULT`]`..=`[`GSU_OVERCLOCK_MAX`]: whole multiples
    /// keep the GSU's master-cycle credit an exact integer (see
    /// [`SnesSystem::gsu_clock_multiplier`]). A cartridge without a GSU
    /// ignores it. Returns the value actually in force.
    pub fn set_gsu_overclock(&mut self, percent: u16) -> u16 {
        self.gsu_overclock_percent =
            (percent / 100 * 100).clamp(GSU_OVERCLOCK_DEFAULT, GSU_OVERCLOCK_MAX);
        self.gsu_overclock_percent
    }

    /// Ticket W31-02: watch the Super FX's polygon stream (`None` = stop).
    /// Pure observation: the machine's state is identical with it on or off,
    /// and a cartridge without a Super FX ignores it. Not saved.
    pub fn set_gsu_poly_observer(&mut self, cfg: Option<crate::gsu_observer::GsuPolyConfig>) {
        self.gsu_poly = cfg;
        if let Some(gsu) = self.system.bus.gsu.as_mut() {
            gsu.set_poly_observer(cfg);
        }
    }

    /// Ticket W31-02: render jobs the observer has completed since it was
    /// installed (a counter, so a caller can tell when there is a new one).
    #[must_use]
    pub fn gsu_poly_jobs_completed(&self) -> u64 {
        self.system
            .bus
            .gsu
            .as_ref()
            .map_or(0, crate::gsu::GsuState::poly_jobs_completed)
    }

    /// Ticket W31-02: remove and return the last (up to 3) completed render
    /// jobs, oldest first.
    pub fn take_gsu_poly_jobs(&mut self) -> Vec<std::sync::Arc<rf_core_api::gsu_poly::PolyJob>> {
        self.system
            .bus
            .gsu
            .as_mut()
            .map_or_else(Vec::new, crate::gsu::GsuState::take_poly_jobs)
    }

    /// The Super FX overclock in force, in percent (100 is off).
    #[must_use]
    pub fn gsu_overclock(&self) -> u16 {
        self.gsu_overclock_percent
    }

    /// Whether the loaded cartridge carries a Super FX (GSU), i.e. whether
    /// [`Self::set_gsu_overclock`] can do anything.
    #[must_use]
    pub fn has_gsu(&self) -> bool {
        self.system.bus.gsu.is_some()
    }

    /// Ticket W30-08: run the SA-1 at `percent` of its own clock.
    ///
    /// The same contract as [`Self::set_gsu_overclock`] — rounded down to a
    /// whole multiple of 100 and clamped to
    /// [`SA1_OVERCLOCK_DEFAULT`]`..=`[`SA1_OVERCLOCK_MAX`] — and the same
    /// signature, so a shell-side speed governor can drive the Super FX,
    /// the SA-1 and the 65816 ([`Self::set_cpu_overclock`]) through one
    /// path. The SA-1's credit is scaled; its timer (a hardware clock) and
    /// everything on the SNES side are not. A cartridge without an SA-1
    /// ignores it. Returns the value actually in force.
    pub fn set_sa1_overclock(&mut self, percent: u16) -> u16 {
        self.sa1_overclock_percent =
            (percent / 100 * 100).clamp(SA1_OVERCLOCK_DEFAULT, SA1_OVERCLOCK_MAX);
        self.sa1_overclock_percent
    }

    /// The SA-1 overclock in force, in percent (100 is off).
    #[must_use]
    pub fn sa1_overclock(&self) -> u16 {
        self.sa1_overclock_percent
    }

    /// Whether the loaded cartridge carries an SA-1, i.e. whether
    /// [`Self::set_sa1_overclock`] can do anything.
    #[must_use]
    pub fn has_sa1(&self) -> bool {
        self.system.bus.sa1.is_some()
    }

    /// Ticket W30-08: run the 65816 at `percent` of the console's speed.
    ///
    /// Same contract and signature as [`Self::set_gsu_overclock`]
    /// (`100..=`[`CPU_OVERCLOCK_MAX`], steps of 100). The frame rate, the
    /// audio rate, the picture's timing, DMA/HDMA and the timers do not
    /// change — only how many instructions the CPU gets in that time; see
    /// the "CPU overclock" section of [`crate::system`]. Returns the value
    /// actually in force.
    pub fn set_cpu_overclock(&mut self, percent: u16) -> u16 {
        self.cpu_overclock_percent =
            (percent / 100 * 100).clamp(CPU_OVERCLOCK_DEFAULT, CPU_OVERCLOCK_MAX);
        self.cpu_overclock_percent
    }

    /// The 65816 overclock in force, in percent (100 is off).
    #[must_use]
    pub fn cpu_overclock(&self) -> u16 {
        self.cpu_overclock_percent
    }

    /// Ticket W30-08: did the 65816 reach an idle wait during the last
    /// completed frame — a `WAI`, or a tight no-write polling loop? A game
    /// that finishes its frame's work early idles every frame; one that
    /// overran (slowdown) did not. A **heuristic observer**: see
    /// [`crate::idle`] for the exact rule, what fools it, and why reading
    /// it cannot change the emulation (it is `&self`, not saved, and fed
    /// values the step had already computed).
    #[must_use]
    pub fn frame_idle(&self) -> bool {
        self.system.idle.frame_idle()
    }

    /// Master cycles the 65816 spent idle in the last completed frame
    /// (real console cycles, whatever the CPU multiplier).
    #[must_use]
    pub fn frame_idle_cycles(&self) -> u64 {
        self.system.idle.frame_idle_cycles()
    }

    /// [`Self::frame_idle`] for the SA-1's own CPU (`false` without one).
    #[must_use]
    pub fn sa1_frame_idle(&self) -> bool {
        self.system
            .bus
            .sa1
            .as_ref()
            .is_some_and(|s| s.idle.frame_idle())
    }

    /// The width this core is currently emitting.
    #[must_use]
    pub fn frame_width(&self) -> usize {
        self.widescreen.0
    }

    /// Each background's geometry, for a widescreen policy decision.
    #[must_use]
    pub fn bg_geometry(&self) -> [crate::ppu::BgGeometry; 4] {
        self.system.bus.ppu.bg_geometry()
    }

    pub fn set_frame_budget(&mut self, budget: u64) {
        self.budget = budget;
    }

    /// Install (`Some`) or drop (`None`) a code atlas for the block executor
    /// (ticket W36-08b). Counters start from zero.
    ///
    /// # Errors
    /// The atlas was built for a ROM of another size (wrong cartridge) or
    /// for an ISA without the five 65816 decode modes.
    pub fn set_rebuild(&mut self, atlas: Option<rf_rebuild::Atlas>) -> Result<(), CoreError> {
        if let Some(a) = &atlas {
            if a.prg_len as usize != self.system.bus.rom.len() {
                return Err(CoreError::Other(format!(
                    "atlas is for a {}-byte ROM, this cartridge has {}",
                    a.prg_len,
                    self.system.bus.rom.len()
                )));
            }
            if a.mode_count != 5 {
                return Err(CoreError::Other(format!(
                    "atlas has {} decode modes, the 65816 has 5",
                    a.mode_count
                )));
            }
        }
        self.system.rebuild = atlas.map(|a| {
            let mut x = Box::new(rf_rebuild::RebuildExec::new(a));
            x.set_hooked(&self.hook_ids);
            x
        });
        Ok(())
    }

    /// The installed atlas, for naming trace entries' functions.
    #[must_use]
    pub fn rebuild_atlas(&self) -> Option<&rf_rebuild::Atlas> {
        self.system
            .rebuild
            .as_deref()
            .map(rf_rebuild::RebuildExec::atlas)
    }

    /// Dispatch misses seen so far (distinct, first-seen order); empty when
    /// Rebuild is off.
    #[must_use]
    pub fn rebuild_deltas(&self) -> &[rf_rebuild::Delta] {
        self.system
            .rebuild
            .as_deref()
            .map_or(&[], rf_rebuild::RebuildExec::deltas)
    }

    /// Ticket W37-04: run these atlas functions on the plain interpreter
    /// (`ids` index the installed atlas's function list; ids belong to one
    /// atlas, so call again after every [`Self::set_rebuild`]). Returns how
    /// many took; `0` with no atlas installed.
    pub fn set_rebuild_demoted(&mut self, ids: &[u32]) -> usize {
        self.system
            .rebuild
            .as_deref_mut()
            .map_or(0, |x| x.set_demoted(ids))
    }

    /// Keep the last `capacity` executed steps on the rebuilt path (repro
    /// bundles); `0` turns it off. No effect while Rebuild is off.
    pub fn set_rebuild_trace(&mut self, capacity: usize) {
        if let Some(x) = self.system.rebuild.as_deref_mut() {
            x.enable_trace(capacity);
        }
    }

    /// The last executed steps with atlas function ids, oldest first.
    #[must_use]
    pub fn rebuild_trace(&self) -> Vec<rf_rebuild::TraceEntry> {
        self.system
            .rebuild
            .as_deref()
            .map_or_else(Vec::new, rf_rebuild::RebuildExec::trace)
    }

    /// Hooked calls currently in flight (0 while Rebuild is off).
    #[must_use]
    pub fn rebuild_shadow_depth(&self) -> usize {
        self.system
            .rebuild
            .as_deref()
            .map_or(0, rf_rebuild::RebuildExec::shadow_depth)
    }

    /// Tell the executor a savestate was just loaded (tickets W36-08b,
    /// W37-07). A caller that restores a savestate into
    /// [`Self::system_mut`] must call this: the call stack the executor
    /// remembered belongs to the machine that was replaced, and the next
    /// block resumes on the plain interpreter. A machine reset forgets the
    /// frames itself and does not arm the resume rule.
    pub fn reset_hook_frames(&mut self) {
        if let Some(x) = self.system.rebuild.as_deref_mut() {
            x.state_loaded();
        }
    }

    /// One instruction, then (only with hooks installed) the function events
    /// it raised, delivered at this instruction boundary.
    fn step_system(&mut self, sink: &mut dyn CoreSink) -> Result<(), u8> {
        let result = self.system.step();
        if self.system.hooks_active() {
            self.deliver_function_events(sink);
        }
        result
    }

    #[inline(never)]
    fn deliver_function_events(&mut self, sink: &mut dyn CoreSink) {
        let Some(x) = self.system.rebuild.as_deref_mut() else {
            return;
        };
        let events = x.take_events();
        for ev in events.iter().copied() {
            let mut ctx = SnesHookCtx {
                system: &mut self.system,
            };
            sink.function_event(ev, &mut ctx);
        }
        if let Some(x) = self.system.rebuild.as_deref_mut() {
            x.recycle_events(events);
        }
    }

    /// Replay the settled frame into `sink`, line by line.
    ///
    /// Both the picture and the dropped-sprite overlay: `Scanline`
    /// carries `overlay` alongside `pixels`, and forwarding only the
    /// pixels would make the 32-per-line sprite suppression invisible to
    /// exactly the feature built to show it.
    fn emit_frame(&mut self, sink: &mut dyn CoreSink) {
        let visible = self.system.bus.ppu.setini.visible_lines();
        let (width, mask) = self.widescreen;
        self.system.bus.ppu.wide_convention = self.wide_convention;
        // Ticket W33-01: asked once per frame; a sink that did not ask (every
        // sink in Accuracy Mode) costs nothing here.
        let want_lines = sink.wants_line_registers();
        for y in 0..visible {
            let line = if width > crate::ppu::WIDTH {
                self.system.bus.ppu.render_scanline_masked(y, width, mask)
            } else {
                // The accuracy path, untouched. Not `render_scanline_masked`
                // with a full mask — the point is that widescreen being off
                // means this code is not reached at all (law 6).
                self.system.bus.ppu.render_scanline(y)
            };
            // Ticket W7-20: the line's palette first, so a sink resolves
            // this line's indices against the CGRAM the beam saw.
            let (palette, brightness) = self.system.bus.ppu.line_palette(y);
            sink.palette_scanline(y, &palette, brightness);
            if want_lines {
                sink.line_registers(y, &self.system.bus.ppu.line_registers(y));
            }
            // Ticket W7-21: the sub-screen, before the line it blends
            // into, on lines whose colour math can act. A true-hires line
            // already carries its sub-screen as half-dots of `line`, and
            // math on hires is not modelled.
            // Ticket W30-03 follow-up: a widescreen line carries its
            // sub-screen at the same width (it is not hires, even when it
            // is 512 dots long), or colour math silently drops on it.
            let wide_line = width > crate::ppu::WIDTH && !self.system.bus.ppu.line_hires(y);
            if (line.pixels.len() == crate::ppu::WIDTH || wide_line)
                && self.system.bus.ppu.line_color_math_active(y)
            {
                let (sub, fixed_color) = if wide_line {
                    self.system
                        .bus
                        .ppu
                        .render_sub_scanline_width(y, width, mask)
                } else {
                    self.system.bus.ppu.render_sub_scanline(y)
                };
                sink.sub_scanline(y, &sub, fixed_color);
            }
            if wide_line {
                let pad = line.pixels.len().saturating_sub(crate::ppu::WIDTH) / 2;
                sink.video_scanline_widened(y, &line.pixels, pad);
            } else {
                sink.video_scanline(y, &line.pixels);
            }
            sink.overlay_scanline(y, &line.overlay);
        }
        // Ticket W16-09: promote the Mode 7 matrix to the generic
        // CoreEvent path, pay-for-use like every other variant here --
        // constructed only when a subscriber asked AND the frame is
        // actually in BG mode 7 (a core in any other mode has nothing
        // meaningful to report, and must not manufacture a stale matrix
        // left over from a previous mode-7 frame).
        if self.config.event_mask.is_subscribed(EventMask::MODE7)
            && self.system.bus.ppu.bg_mode == 7
        {
            let frame = crate::debug::mode7_frame(&self.system.bus.ppu);
            sink.event(CoreEvent::Mode7(frame));
        }
        self.drain_events(sink);
    }

    /// Rebuild the byte views `state_view` lends (ticket W13-02a).
    ///
    /// Called at the end of every step, because a debugger reads state
    /// between steps and a snapshot older than the last instruction would
    /// be a viewer showing the wrong frame. ~570 bytes of copying against
    /// a frame that already carries ~245 KB of pixels.
    ///
    /// ## Why the registers are rebuilt rather than shadowed at write time
    ///
    /// A shadow array updated inside `write_register` would be a second
    /// copy of state that can silently disagree with the decoded fields
    /// the renderer actually uses — the exact failure `SnesBus`'s own doc
    /// warns about for VRAM ("writes would land here while rendering read
    /// `ppu.vram`, so every game would draw a black screen with nothing
    /// obviously wrong anywhere"). Deriving them from the live fields
    /// cannot drift, and pays only when someone is looking.
    ///
    /// ## Layout
    ///
    /// Indexed so `ppu_regs[n]` is register `$21nn` — a viewer asking for
    /// `$2105` reads index 5, rather than having to learn a private field
    /// order. Registers this core does not model read back as zero;
    /// [`PPU_REG_COUNT`] covers `$2100-$213F`.
    fn refresh_state_snapshots(&mut self) {
        let ppu = &self.system.bus.ppu;

        self.cgram_bytes.clear();
        self.cgram_bytes.reserve(ppu.cgram.len() * 2);
        for word in &ppu.cgram {
            self.cgram_bytes.extend_from_slice(&word.to_le_bytes());
        }

        self.ppu_regs.clear();
        self.ppu_regs.resize(PPU_REG_COUNT, 0);
        let mut set = |offset: usize, value: u8| {
            if let Some(slot) = self.ppu_regs.get_mut(offset) {
                *slot = value;
            }
        };
        // $2100 INIDISP: forced blank in bit 7, brightness in bits 0-3.
        set(
            0x00,
            (u8::from(ppu.forced_blank) << 7) | (ppu.brightness & 0x0F),
        );
        // $2101 OBSEL: size in bits 5-7, name select in 3-4, base in 0-2.
        set(
            0x01,
            ((ppu.obj_size & 0x07) << 5)
                | (((ppu.obj_name_select >> 13) as u8 & 0x03) << 3)
                | ((ppu.obj_name_base >> 14) as u8 & 0x07),
        );
        // $2102/$2103 OAMADDL/H — the word address, plus the rotation bit.
        set(0x02, (ppu.oam_addr >> 1) as u8);
        set(
            0x03,
            ((ppu.oam_addr >> 9) as u8 & 0x01) | (u8::from(ppu.oam_priority_rotation) << 7),
        );
        // $2105 BGMODE: tile sizes in bits 4-7, BG3 priority in 3, mode in 0-2.
        let mut bgmode = (ppu.bg_mode & 0x07) | (u8::from(ppu.bg3_priority) << 3);
        for (i, bg) in ppu.bgs.iter().enumerate() {
            bgmode |= u8::from(bg.tile_size_16) << (4 + i);
        }
        set(0x05, bgmode);
        // $2107-$210A BGnSC: tilemap base in bits 2-7, size in 0-1.
        for (i, bg) in ppu.bgs.iter().enumerate() {
            set(
                0x07 + i,
                (((bg.tilemap_base >> 10) as u8 & 0x3F) << 2) | (bg.tilemap_size & 0x03),
            );
        }
        // $210B/$210C BGnNBA: two 4-bit char bases per register.
        for pair in 0..2usize {
            let lo = (ppu.bgs[pair * 2].char_base >> 12) as u8 & 0x0F;
            let hi = (ppu.bgs[pair * 2 + 1].char_base >> 12) as u8 & 0x0F;
            set(0x0B + pair, lo | (hi << 4));
        }
        // $212C TM / $212D TS: which layers are on each screen.
        let mut tm = u8::from(ppu.obj_enabled) << 4;
        for (i, bg) in ppu.bgs.iter().enumerate() {
            tm |= u8::from(bg.enabled) << i;
        }
        set(0x2C, tm);
        set(0x2D, ppu.ts);
        // $213E STAT77: range-over in bit 6, time-over in bit 7.
        set(
            0x3E,
            (u8::from(ppu.range_over) << 6) | (u8::from(ppu.time_over) << 7),
        );
    }

    /// Hand everything the bus queued to the sink (ticket W13-02h).
    ///
    /// Drained at the frame boundary and at the end of every step, so a
    /// single-stepping debugger sees a watch hit at the instruction that
    /// caused it rather than at the next frame.
    fn drain_events(&mut self, sink: &mut dyn CoreSink) {
        for ev in self.system.bus.drain_events() {
            sink.event(ev);
        }
    }
}

impl SnesCore {
    /// [`EmulatorCore::step`] with `each` called after every instruction
    /// (ticket W36-09's observed frame). Generic, not `dyn`, so the
    /// ordinary `step` (a no-op closure) compiles to the code it had.
    fn step_with<F: FnMut(&mut Self)>(
        &mut self,
        granularity: Step,
        sink: &mut dyn CoreSink,
        mut each: F,
    ) -> StepResult {
        // Ticket W13-02h/W13-02e: `CoreConfig` is the documented path a
        // consumer configures a core through, and this is what makes that
        // true here. Pushed on entry rather than on a setter so there is
        // exactly one place the bus can disagree with the config.
        if self.system.bus.event_mask() != self.config.event_mask {
            self.system.bus.set_event_mask(self.config.event_mask);
        }
        if self.config.watches != self.pushed_watches {
            self.system.bus.set_watches(self.config.watches);
            self.pushed_watches = self.config.watches;
        }
        if let Some(gsu) = self.system.bus.gsu.as_mut() {
            if gsu.poly_observer_config() != self.gsu_poly {
                gsu.set_poly_observer(self.gsu_poly);
            }
        }
        self.system.gsu_clock_multiplier = u64::from(self.gsu_overclock_percent / 100);
        self.system.sa1_clock_multiplier = u64::from(self.sa1_overclock_percent / 100);
        self.system.cpu_clock_multiplier = u64::from(self.cpu_overclock_percent / 100);
        self.system.bus.apu.dsp.interpolation = self.interpolation;
        let start_frame = self.system.bus.timing.frame;
        let mut instructions = 0u64;
        let frame_complete;

        match granularity {
            Step::Instruction => {
                // A halted CPU is not an error here: `SnesSystem::step`
                // reports the halt opcode, and the debugger wants to see
                // that state rather than have it swallowed.
                let _ = self.step_system(sink);
                each(self);
                instructions = 1;
                frame_complete = self.system.bus.timing.frame != start_frame;
            }
            Step::Scanline | Step::Frame => {
                // Run to a settled vblank and out the other side, which
                // is what makes composition safe on this machine.
                while !self.system.bus.timing.in_vblank() && instructions < self.budget {
                    let stepped = self.step_system(sink);
                    each(self);
                    if stepped.is_err() {
                        break;
                    }
                    instructions += 1;
                }
                while (self.system.bus.timing.in_vblank()
                    || self.system.bus.timing.frame == start_frame)
                    && instructions < self.budget
                {
                    let stepped = self.step_system(sink);
                    each(self);
                    if stepped.is_err() {
                        break;
                    }
                    instructions += 1;
                }
                frame_complete = self.system.bus.timing.frame != start_frame;
                if frame_complete {
                    self.emit_frame(sink);
                    // Ticket W7-22: the frame's sound, after its picture.
                    let pending = std::mem::take(&mut self.system.bus.apu.out_samples);
                    // Ticket W30-09: stereo only for a sink that asked; every
                    // other sink takes the unchanged mono path.
                    if sink.wants_stereo() {
                        let out = self.audio.convert_stereo(&pending);
                        if !out.is_empty() {
                            sink.audio_stereo(out);
                        }
                    } else {
                        let out = self.audio.convert(&pending);
                        if !out.is_empty() {
                            sink.audio(out);
                        }
                    }
                    self.system.bus.apu.out_samples = pending;
                    self.system.bus.apu.out_samples.clear();
                }
            }
        }

        // Ticket W13-02a: refresh the byte views a debugger reads between
        // steps, before reporting the step as finished.
        self.refresh_state_snapshots();

        // Every step drains, not only a completed frame: an Instruction or
        // Scanline step that trips a watchpoint must report it at the step
        // that caused it, which is the whole point of single-stepping with
        // one armed. `emit_frame` has already drained on a frame boundary,
        // so this is empty in that case rather than a double delivery.
        self.drain_events(sink);

        StepResult {
            // Instructions, not master cycles: this core counts its
            // budget in instructions, and reporting a number in a unit it
            // does not measure would be worse than reporting this one.
            cycles: instructions,
            frame_complete,
        }
    }
}

impl EmulatorCore for SnesCore {
    fn load(&mut self, cart: CartImage<'_>) -> Result<(), CoreError> {
        // An atlas describes one cartridge, and so do the function ids hooked
        // in it (the new system below starts without an executor).
        self.hook_ids.clear();
        // A recording survives a load (fresh, for the new cartridge), like the
        // NES core's: it is a session setting, not cartridge state.
        let recording = self.system.coverage.is_some();
        self.system =
            SnesSystem::load(cart.rom).map_err(|e| CoreError::InvalidImage(format!("{e:?}")))?;
        if recording {
            self.system.set_coverage(true);
            self.system.coverage_note_reset();
        }
        Ok(())
    }

    fn reset(&mut self, _kind: ResetKind) {
        self.system.reset();
    }

    fn run_frame(&mut self, input: &InputFrame, sink: &mut dyn CoreSink) {
        let _ = input; // Controller wiring is W11-12's; see the notes there.
        let _ = self.step(Step::Frame, sink);
    }

    fn step(&mut self, granularity: Step, sink: &mut dyn CoreSink) -> StepResult {
        self.step_with(granularity, sink, |_| {})
    }

    fn save_state(&self, _w: &mut dyn StateWriter) -> Result<(), StateError> {
        Err(StateError::Io(
            "SNES state is carried by rf_state's container (W8-02), not this trait method"
                .to_string(),
        ))
    }

    fn load_state(&mut self, _r: &mut dyn StateReader) -> Result<(), StateError> {
        Err(StateError::Io(
            "SNES state is carried by rf_state's container (W8-02), not this trait method"
                .to_string(),
        ))
    }

    fn state_view(&self) -> StateView<'_> {
        StateView {
            // Typed, per ruling D-6 (ticket W13-02i): the contract names
            // the 65C816 register file rather than a byte layout every
            // consumer would decode per console. `e` travels alongside
            // `p` because it is not a bit of `p` and changes what two of
            // `p`'s bits mean.
            cpu_regs: CpuRegs::Wdc65816(Wdc65816Regs {
                a: self.system.cpu.a,
                x: self.system.cpu.x,
                y: self.system.cpu.y,
                sp: self.system.cpu.sp,
                d: self.system.cpu.d,
                dbr: self.system.cpu.dbr,
                pbr: self.system.cpu.pbr,
                pc: self.system.cpu.pc,
                p: self.system.cpu.p,
                e: self.system.cpu.e,
            }),
            wram: &self.system.bus.wram,
            vram: &self.system.bus.ppu.vram,
            cgram: &self.cgram_bytes,
            oam: &self.system.bus.ppu.oam,
            ppu_regs: &self.ppu_regs,
            // **Empty is the correct report here, not a stub.** A plain
            // LoROM/HiROM cartridge has no bank registers and no IRQ
            // counter — there is nothing to serialize, which is exactly
            // what this field's own doc calls for ("empty slice for
            // mappers with no persistent state"). It becomes non-empty
            // when a core supports a cartridge chip that has some (SA-1,
            // SuperFX), and `rf_cart` reports none of those yet.
            mapper_state: &[],
        }
    }

    fn config(&mut self) -> &mut CoreConfig {
        &mut self.config
    }

    fn set_coverage_recording(&mut self, on: bool) {
        self.system.set_coverage(on);
    }

    fn take_coverage(&mut self) -> Option<rf_core_api::CoverageLog> {
        self.system.take_coverage()
    }

    fn set_rebuild_atlas(
        &mut self,
        atlas: Option<Box<dyn std::any::Any + Send>>,
    ) -> Result<(), CoreError> {
        let atlas = match atlas {
            None => None,
            Some(b) => Some(
                *b.downcast::<rf_rebuild::Atlas>()
                    .map_err(|_| CoreError::Other("not an rf-rebuild atlas".to_string()))?,
            ),
        };
        self.set_rebuild(atlas)
    }

    fn set_function_hooks(&mut self, function_ids: &[u32]) {
        self.hook_ids = function_ids.to_vec();
        if let Some(x) = self.system.rebuild.as_deref_mut() {
            x.set_hooked(function_ids);
        }
    }

    fn set_rom_overrides(&mut self, overrides: &[rf_core_api::RomOverride]) -> bool {
        rf_core_api::install_rom_overrides(&mut self.system.bus.rom_overrides, overrides)
    }

    fn rebuild_stats(&self) -> Option<rf_core_api::RebuildStats> {
        self.system
            .rebuild
            .as_deref()
            .map(rf_rebuild::RebuildExec::stats)
    }

    /// Ticket W36-09: `Step::Frame` with the access log on and the observer
    /// called after every instruction (`step_with`'s hook). The pad is NOT
    /// latched here, exactly as `run_frame` does not: the caller writes
    /// `bus.joypads.ports` first. The `observed_frame_is_run_frame` test
    /// holds this equal to `run_frame`.
    fn run_frame_observed(
        &mut self,
        input: &InputFrame,
        sink: &mut dyn CoreSink,
        observer: &mut dyn rf_core_api::StepObserver,
    ) -> bool {
        let _ = input;
        self.config.watches.set_observe(true);
        let own_trace = self
            .system
            .rebuild
            .as_deref()
            .is_some_and(|x| !x.trace_enabled());
        if own_trace {
            self.set_rebuild_trace(2);
        }
        let mut seen = self
            .system
            .rebuild
            .as_deref()
            .map_or(0, rf_rebuild::RebuildExec::trace_total);
        self.system.bus.set_watches(self.config.watches);
        self.pushed_watches = self.config.watches;
        self.system.bus.clear_access_log();
        let _ = self.step_with(Step::Frame, sink, |core| {
            let facts = core
                .system
                .rebuild
                .as_deref()
                .map_or_else(Default::default, |x| {
                    if x.trace_total() == seen {
                        return rf_core_api::StepFacts::default();
                    }
                    seen = x.trace_total();
                    x.last_step().unwrap_or_default()
                });
            observer.step(facts, core.system.bus.access_log());
            core.system.bus.clear_access_log();
        });
        observer.frame_end();
        if own_trace {
            self.set_rebuild_trace(0);
        }
        self.config.watches.set_observe(false);
        self.system.bus.set_watches(self.config.watches);
        self.pushed_watches = self.config.watches;
        true
    }

    fn peek(&self, addr: u32) -> u8 {
        // **Memory targets only, and that is the point.** A peek must not
        // perturb the machine, and a SNES hardware register read very
        // much can — `$2139`/`$213A` advance the VRAM read address,
        // `$4218`-style ports latch. So this resolves the address through
        // the same mapping the CPU uses and answers for ROM, WRAM and
        // SRAM; a register or an unmapped address reports the quiescent
        // 0 rather than being read for real.
        //
        // The alternative — routing through the CPU's read path — would
        // make the debugger's memory viewer a participant in the
        // simulation, which is exactly what ARCHITECTURE §2's honesty
        // contract forbids. A viewer that changes what it observes is
        // worse than one with blanks in it.
        let bank = ((addr >> 16) & 0xFF) as u8;
        let offset = (addr & 0xFFFF) as u16;
        let target = crate::mapping::map(
            self.system.bus.mode,
            bank,
            offset,
            self.system.bus.rom.len(),
            self.system.bus.sram.len(),
        );
        match target {
            // W41-02: the debugger shows what the CPU sees, overrides included.
            crate::mapping::Target::Rom(i) => rf_core_api::resolve_rom_override(
                &self.system.bus.rom_overrides,
                addr & 0x00FF_FFFF,
                self.system.bus.rom.get(i).copied().unwrap_or(0),
            ),
            crate::mapping::Target::Wram(i) => self.system.bus.wram.get(i).copied().unwrap_or(0),
            crate::mapping::Target::Sram(i) => self.system.bus.sram.get(i).copied().unwrap_or(0),
            // This viewer resolves through the plain `map` above, never
            // `dsp1_target`/`sa1_target` — a DSP-1 window's DR/SR, or an
            // SA-1 cart's I-RAM/BW-RAM/register window, occupy the same
            // bank/offset space `map` alone would call ROM/SRAM/open bus
            // here, so these arms are unreachable in practice but must
            // still type-check (tickets W14-19, W17-01 added the
            // variants).
            crate::mapping::Target::Register(_)
            | crate::mapping::Target::Dsp1Dr
            | crate::mapping::Target::Dsp1Sr
            | crate::mapping::Target::Sa1IRam(_)
            | crate::mapping::Target::Sa1BwRam(_)
            | crate::mapping::Target::Sa1Register(_)
            | crate::mapping::Target::Sa1Bitmap(_)
            | crate::mapping::Target::GsuRam(_)
            | crate::mapping::Target::GsuRegister(_)
            | crate::mapping::Target::Cx4Ram(_)
            | crate::mapping::Target::Cx4Register(_)
            | crate::mapping::Target::Obc1Register(_)
            | crate::mapping::Target::Obc1Bits(_)
            | crate::mapping::Target::WindowRam(_)
            | crate::mapping::Target::St010Ram(_)
            | crate::mapping::Target::St010Register(_)
            | crate::mapping::Target::Open => 0,
        }
    }
}

#[cfg(test)]
mod audio_tests {
    use super::AudioConverter;

    #[test]
    fn two_inputs_make_three_outputs_interpolated_and_downmixed() {
        let mut c = AudioConverter::default();
        // Mono values 300 then 600 (each the mean of its pair).
        let out = c.convert(&[(200, 400), (600, 600)]).to_vec();
        // prev=0: outputs at 0, 2/3 of (0->300); then 1/3 of (300->600).
        assert_eq!(out, vec![0, 200, 400]);
        // The phase carries across calls: a steady stream stays 3:2.
        let mut total = 0;
        for _ in 0..1000 {
            total += c.convert(&[(100, 100), (100, 100)]).len();
        }
        assert_eq!(total, 3000);
    }

    /// Ticket W30-09: the stereo path keeps L and R apart, resampled with
    /// the same phase as the mono path (3 outputs per 2 inputs).
    #[test]
    fn stereo_path_keeps_channels_apart_and_in_phase() {
        let mut c = AudioConverter::default();
        let out = c.convert_stereo(&[(300, 0), (600, 0)]).to_vec();
        assert_eq!(out, vec![[0, 0], [200, 0], [400, 0]]);
        let mut total = 0;
        for _ in 0..1000 {
            total += c.convert_stereo(&[(1, 2), (3, 4)]).len();
        }
        assert_eq!(total, 3000);
    }

    /// Ticket W30-09: a sink that never asks for stereo hears exactly what
    /// it always did, whatever the stereo path does to the same input.
    #[test]
    fn mono_path_is_unchanged_by_the_stereo_path_existing() {
        // A fixed, lopsided stream; the pinned sum guards the mono output.
        let input: Vec<(i16, i16)> = (0..4000i32)
            .map(|i| {
                (
                    (i * 37 % 20011 - 9000) as i16,
                    (i * 91 % 15013 - 7000) as i16,
                )
            })
            .collect();
        let mut a = AudioConverter::default();
        let mono: Vec<i16> = input
            .chunks(333)
            .flat_map(|c| a.convert(c).to_vec())
            .collect();
        let mut b = AudioConverter::default();
        let mut mixed = Vec::new();
        for (i, c) in input.chunks(333).enumerate() {
            if i % 2 == 0 {
                mixed.extend_from_slice(b.convert(c));
            } else {
                // Interleaving stereo batches must not disturb the phase.
                let st = b.convert_stereo(c).to_vec();
                mixed.extend(
                    st.iter()
                        .map(|f| ((i32::from(f[0]) + i32::from(f[1])) / 2) as i16),
                );
            }
        }
        assert_eq!(mono.len(), mixed.len());
        // Even batches are byte-identical to the pure-mono run.
        let mut ofs = 0;
        let mut a2 = AudioConverter::default();
        for (i, c) in input.chunks(333).enumerate() {
            let n = a2.convert(c).len();
            if i % 2 == 0 {
                assert_eq!(&mono[ofs..ofs + n], &mixed[ofs..ofs + n], "batch {i}");
            }
            ofs += n;
        }
    }
}

#[cfg(test)]
mod overclock_tests {
    use super::{SnesCore, GSU_OVERCLOCK_DEFAULT, GSU_OVERCLOCK_MAX};

    /// A bare LoROM image with a Super FX chipset byte when `gsu`.
    fn image(gsu: bool) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        let base = 0x7FC0;
        rom[base + 0x15] = 0x20;
        rom[base + 0x16] = if gsu { 0x15 } else { 0x00 };
        rom[base + 0x17] = 0x08;
        rom[base + 0x1C..base + 0x1E].copy_from_slice(&0xFFFFu16.to_le_bytes());
        rom[base + 0x3C] = 0x00;
        rom[base + 0x3D] = 0x80;
        rom
    }

    /// Ticket W30-01: whole multiples of 100, clamped to 100..=800, and a
    /// fresh core runs the cartridge's own clock.
    #[test]
    fn overclock_defaults_off_and_rounds_and_clamps() {
        let mut core = SnesCore::load(&image(true)).expect("GSU image loads");
        assert!(core.has_gsu());
        assert_eq!(core.gsu_overclock(), GSU_OVERCLOCK_DEFAULT);
        assert_eq!(core.set_gsu_overclock(250), 200);
        assert_eq!(core.set_gsu_overclock(0), GSU_OVERCLOCK_DEFAULT);
        assert_eq!(core.set_gsu_overclock(5000), GSU_OVERCLOCK_MAX);
        assert!(!SnesCore::load(&image(false))
            .expect("plain image loads")
            .has_gsu());
    }
}
