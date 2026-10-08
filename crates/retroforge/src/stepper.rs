//! Windowless, testable pause/step-frame/step-scanline state machine
//! (ticket W1-06 acceptance criterion 2, FR-DBG-004).
//!
//! `PLAYBOOK.md`/the ticket notes are explicit that this logic must be
//! testable *without a window*: [`EmuStepper`] owns an [`rf_nes::NesBus`] +
//! [`rf_nes::Cpu`] and exposes pause/resume/step as plain method calls with
//! no `egui`/`eframe` dependency anywhere in this module. `crates::app`
//! (the `eframe::App` impl) is the only thing that ever touches a window,
//! and it only ever calls into this type — never the other way around.
//!
//! ## Why `step_scanline` can occasionally over-shoot by more than one
//!
//! [`rf_nes::Cpu::step`] is instruction-granular (module doc,
//! `crates/rf-nes/src/cpu/mod.rs`): there is no way to stop it mid
//! instruction. For every opcode except the OAM-DMA-triggering `$4014`
//! write, one instruction (at most ~8 CPU cycles = 24 PPU dots) can never
//! cross more than one scanline boundary (341 dots/scanline), so
//! [`EmuStepper::step_scanline`] stopping "as soon as at least one
//! scanline has been drained" is exact in the overwhelmingly common case.
//! An instruction whose bus write triggers OAM DMA burns 513-514 cycles
//! (~1541 dots, ~4.5 scanlines) *inside that one `Cpu::step` call*
//! (`NesBus`'s module doc, "the master-clock seam") — during that one
//! step, more than one scanline can complete before `step_scanline` gets a
//! chance to check. Fixing this exactly would mean cycle-granular CPU
//! stepping, which is out of this ticket's write scope (`rf-nes` is
//! explicitly off limits — see the ticket's "DO NOT touch" list) and does
//! not exist in this crate yet. This is documented, not hidden: the return
//! value is the *actual* scanline count observed, not hard-coded to 1.
//!
//! ## Why every loop in here is cycle-bounded
//!
//! `step_frame`/`step_scanline`/`tick_running` all run `Cpu::step` in a
//! `loop` until a PPU-observable condition fires (a frame or scanline
//! boundary). Nothing in `rf-nes`'s public API *guarantees* that condition
//! is reachable from an arbitrary machine state — a `JAM`/`KIL` opcode
//! parks the CPU re-executing the same opcode forever (`cpu/exec.rs`'s
//! `jam` doc: "an actual unbounded loop inside one call" is avoided only
//! at the single-`Cpu::step`-call granularity, not across repeated calls),
//! and nothing rules out a future core bug doing something similar. This
//! type is called from `crate::core_thread`'s guarded loop body, whose
//! *own* liveness (draining `CoreCommand::Shutdown` etc.) depends on each
//! call into `EmuStepper` returning in bounded time — an unbounded loop
//! here would hang the whole core thread, including its ability to ever
//! see a Shutdown command, which is a worse failure than FM-01 was written
//! to contain. [`CYCLE_BUDGET`] bounds every loop below by elapsed
//! `NesBus::master_cycle`, generous enough (four NTSC frames' worth) that
//! it can never fire for any ROM this crate can actually run correctly —
//! its only job is to guarantee termination, not to be a normal exit path.
//!
//! ## The one shared latch-then-advance path (ticket W1-07)
//!
//! [`EmuStepper::latch_and_advance_frame`] is the single function every
//! input-driven caller — live gameplay (`crate::core_thread`, from the UI
//! thread's held-key atomic), `.rfreplay` recording, and `.rfreplay`
//! playback — goes through. It latches the given [`rf_core_api::InputFrame`]
//! into both controller ports *before* running a single instruction, then
//! advances exactly one frame. Two call sites that each "latch then
//! advance" separately is exactly how replay divergence gets built in
//! (record and playback silently doing the latch at different points), so
//! there is deliberately only one.
//!
//! ## The reachable-state hash (ticket W1-07)
//!
//! [`EmuStepper::state_hash`] IS a full-machine hash as of ticket W2-04:
//! it digests every `rf_nes::StateRegion`, so CPU, bus, PPU (VRAM, palette,
//! OAM, loopy registers, sprite units), APU (every channel's phase and the
//! frame counter), WRAM, mapper registers and battery PRG-RAM are all
//! covered. Before W2-04 it could not be — `rf-nes` had no state
//! serialization, so PPU and APU internals were structurally unreachable
//! and the hash was documented here as explicitly partial. That caveat is
//! deleted rather than softened, because it is no longer true; what remains
//! excluded is the rendered framebuffer, which is *output*, not state (the
//! test suite digests it separately and never folds it in here), and the
//! PPU's undrained output queues, which the core refuses to serialize at
//! all outside a frame boundary.
//!
//! Anything recording this hash in a `.rfreplay` writes
//! `hash_kind=full-v2` ([`crate::save_state::HASH_KIND`]); the old
//! `reachable-v1` names the narrower hash and must not be reused for this
//! one (SAVE_STATES.md §3).
use rf_core_api::{CoreSink, InputFrame};
use rf_nes::{Cpu, NesBus, NesLoadError};

/// One NTSC frame is `341 * 262 / 3` = 29,781 master (CPU) cycles
/// (`crate::ppu`'s dot-diagram doc, `DOTS_PER_SCANLINE * scanlines /
/// dots-per-cycle`). Four frames' worth gives generous headroom for
/// OAM-DMA-heavy frames while still bounding every loop in this module to
/// a small, fixed amount of work — see module doc's "Why every loop in
/// here is cycle-bounded".
const CYCLE_BUDGET: u64 = 4 * 29_781;

/// `CoreEvent` subscriptions the enhanced-camera pipeline
/// (`crate::canvas_accum`) needs on every frame, unconditionally — turned
/// on once by [`EmuStepper::from_ines_bytes`] below and re-asserted by
/// every [`EmuStepper::set_event_mask`] call (ticket W4-06a) so a
/// debugger-driven mask change can never silently starve scene tracking.
/// A single named constant rather than two call sites each spelling out
/// the same union keeps them from drifting apart.
/// Instruction cap for the traced frame loop (ticket W13-02g).
///
/// A frame is ~10k instructions on either console; this is generous
/// enough never to truncate a real frame and finite enough that a core
/// that stops completing frames cannot hang the caller. Law 8: a test
/// that hangs is a denial of service, not a failing test.
pub const TRACED_INSTRUCTION_BUDGET: u64 = 2_000_000;

pub const CAMERA_BASELINE_EVENT_MASK: rf_core_api::EventMask =
    rf_core_api::EventMask::SCANLINE.union(rf_core_api::EventMask::SCROLL_WRITE);

/// Whether the emulator is advancing on its own each repaint, or holding
/// still until the debugger asks for another step (FR-DBG-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Not advancing; only `step_frame`/`step_scanline` move time forward.
    Paused,
    /// Advancing by one frame per [`EmuStepper::tick_running`] call (the
    /// app layer calls that once per repaint while running).
    Running,
}

/// A `CoreSink` wrapper that both forwards to an inner sink (so callers
/// still see real pixel data) and counts how many `video_scanline` calls
/// went through it, so [`EmuStepper::step_scanline`] can detect "at least
/// one scanline completed" without reaching into `rf-nes`-internal PPU
/// state (which this crate cannot touch — see module doc).
struct CountingSink<'a> {
    inner: &'a mut dyn CoreSink,
    scanlines: u32,
    /// Last line number seen through [`CoreSink::video_scanline`] (ticket
    /// W2-15). This is where the position readout comes from: the sink
    /// contract already hands us `y`, so no `rf-nes` accessor is needed —
    /// which also keeps `docs/evidence/local-gate.json` from going stale
    /// over a status-bar feature.
    last_scanline: Option<u16>,
}

impl CoreSink for CountingSink<'_> {
    fn video_scanline(&mut self, y: u16, pixels: &[rf_core_api::PpuPixel]) {
        self.scanlines += 1;
        self.last_scanline = Some(y);
        self.inner.video_scanline(y, pixels);
    }

    /// Ticket W3-05a: MUST forward explicitly, not rely on
    /// `CoreSink::overlay_scanline`'s default no-op — this wrapper sits
    /// between every `EmuStepper` caller and the real sink (`FrameBuffer`
    /// in production), so an unforwarded default here would silently
    /// discard the overlay at this seam regardless of what `Ppu::drain`
    /// emitted, with no test above this layer able to tell the difference
    /// (`inner` would just never see a call).
    fn overlay_scanline(&mut self, y: u16, pixels: &[rf_core_api::OverlayPixel]) {
        self.inner.overlay_scanline(y, pixels);
    }

    /// Ticket W7-20: forwarded for the same reason as the overlay — an
    /// unforwarded default here left every SNES frame in NES colours.
    /// The sub-screen (W7-16) is forwarded alongside it, which it never
    /// was.
    fn palette_scanline(&mut self, y: u16, palette: &[u16], brightness: u8) {
        self.inner.palette_scanline(y, palette, brightness);
    }

    fn sub_scanline(&mut self, y: u16, pixels: &[rf_core_api::SubPixel], fixed_color: u16) {
        self.inner.sub_scanline(y, pixels, fixed_color);
    }

    fn audio(&mut self, samples: &[i16]) {
        self.inner.audio(samples);
    }

    fn event(&mut self, ev: rf_core_api::CoreEvent) {
        self.inner.event(ev);
    }
}

/// Empty stand-ins for the NES-only debug views, on a SNES session.
const EMPTY_VRAM: [u8; 0x1000] = [0; 0x1000];
const EMPTY_PALETTE: [u8; 32] = [0; 32];
const EMPTY_OAM: [u8; 256] = [0; 256];
const EMPTY_PRG_RAM: [u8; 0x2000] = [0; 0x2000];

/// Why a ROM could not be opened (ticket W11-12).
///
/// Three variants rather than one string, because the three failures
/// send a user to three different places: a bad NES image, a bad SNES
/// image, and a file that is neither.
#[derive(Debug)]
pub enum OpenError {
    /// `rf-nes` refused an image `rf-cart` identified as NES.
    Nes(NesLoadError),
    /// `rf-snes` refused an image `rf-cart` identified as SNES.
    Snes(String),
    /// Neither console claims this file.
    Unrecognized(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Nes(e) => write!(f, "not a usable NES image: {e:?}"),
            OpenError::Snes(e) => write!(f, "not a usable SNES image: {e}"),
            OpenError::Unrecognized(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Owns one running machine and the run/paused state a debugger UI
/// drives it with.
///
/// Which console this session is running (ticket W11-12).
///
/// **Emulation is unified; console-specific *inspection* is not.** Every
/// method that advances the machine goes through
/// [`rf_core_api::EmulatorCore`] — one code path, both consoles. What
/// branches here is the debugger's reach-through: the NES pattern viewer
/// wants a `NesBus`, the breakpoint engine wants a 6502 `pc` and `s`, and
/// the NES state serializer wants the whole private `Cpu`. (The register
/// *readout* no longer branches: since W13-02i it reads the typed
/// `StateView::cpu_regs` for both consoles.)
///
/// **This is not the "second bespoke SNES path" Brad ruled against on
/// 2026-08-26**, and the difference is worth being exact about. That
/// ruling was against duplicating ~20 methods of *emulation* behaviour,
/// which is how one shell ends up with two half-consoles that drift.
/// Emulation is not duplicated here at all — it is one `dyn
/// EmulatorCore` call. Only inspection branches.
///
/// Closing the remaining trait gaps — an out-of-band bus write, APU
/// access, and the NES-shaped viewers above — is what would let this
/// become a plain `Box<dyn EmulatorCore>`. Typed CPU registers closed
/// with W13-02i; `rf_core_api::cpu_regs`'s module doc records the rule
/// for the other two. Doing it before then would mean downcasting:
/// bypassing the trait while appearing to use it.
enum Machine {
    Nes(Box<rf_nes::core::NesCore>),
    Snes(Box<rf_snes::core::SnesCore>),
}

impl Machine {
    fn as_core(&mut self) -> &mut dyn rf_core_api::EmulatorCore {
        match self {
            Machine::Nes(c) => c.as_mut(),
            Machine::Snes(c) => c.as_mut(),
        }
    }

    fn as_core_ref(&self) -> &dyn rf_core_api::EmulatorCore {
        match self {
            Machine::Nes(c) => c.as_ref(),
            Machine::Snes(c) => c.as_ref(),
        }
    }

    /// The NES bus, or `None` on a SNES session.
    fn nes_bus(&self) -> Option<&rf_nes::NesBus> {
        match self {
            Machine::Nes(c) => Some(c.bus()),
            Machine::Snes(_) => None,
        }
    }

    fn nes_bus_mut(&mut self) -> Option<&mut rf_nes::NesBus> {
        match self {
            Machine::Nes(c) => Some(c.bus_mut()),
            Machine::Snes(_) => None,
        }
    }

    fn nes_cpu(&self) -> Option<&rf_nes::Cpu> {
        match self {
            Machine::Nes(c) => Some(c.cpu()),
            Machine::Snes(_) => None,
        }
    }

    /// Every mutating NES-only action goes through a named method rather
    /// than an inlined `if let`, so a SNES session is a documented no-op
    /// instead of an expression that happens not to typecheck.
    fn bus_write(&mut self, addr: u16, value: u8) {
        use rf_nes::CpuBus as _;
        if let Some(bus) = self.nes_bus_mut() {
            bus.write(addr, value);
        }
    }

    fn set_sprite_overlay(&mut self, enabled: bool) {
        if let Some(bus) = self.nes_bus_mut() {
            bus.set_sprite_overlay_enabled(enabled);
        }
    }

    /// The NES takes the low byte; the SNES (ticket W23-01) the whole
    /// `$4218`-layout word, latched by auto-joypad like the hardware.
    fn set_controller(&mut self, port: usize, buttons: u16) {
        match self {
            Machine::Nes(_) => {
                if let Some(bus) = self.nes_bus_mut() {
                    #[allow(clippy::cast_possible_truncation)]
                    bus.set_controller_buttons(port, buttons as u8);
                }
            }
            Machine::Snes(c) => {
                if let Some(p) = c.system_mut().bus.joypads.ports.get_mut(port) {
                    *p = buttons;
                }
            }
        }
    }

    fn set_channel_capture(&mut self, on: bool) {
        if let Some(bus) = self.nes_bus_mut() {
            bus.apu_mut().set_channel_capture(on);
        }
    }

    fn take_channel_samples(&mut self) -> Option<[Vec<i16>; rf_nes::apu::CHANNEL_COUNT]> {
        self.nes_bus_mut()
            .map(|b| b.apu_mut().take_channel_samples())
    }

    #[cfg(test)]
    fn set_cycle_budget(&mut self, budget: u64) {
        match self {
            Machine::Nes(c) => c.set_cycle_budget(budget),
            Machine::Snes(c) => c.set_frame_budget(budget),
        }
    }
}

/// drives it through. See module doc.
pub struct EmuStepper {
    /// Ticket W11-10: the machine, behind `rf_core_api::EmulatorCore`.
    ///
    /// `EmuStepper` used to hold `bus: NesBus` and `cpu: Cpu` directly
    /// and own the run-until-next-frame loop — which meant the SHELL
    /// owned the definition of "one NES frame". That loop now lives in
    /// `rf_nes::core::NesCore`, and everything below drives it through
    /// the trait.
    ///
    /// Concrete `NesCore` rather than `Box<dyn EmulatorCore>`, and that
    /// is W11-12's job rather than an oversight: two things the
    /// debugger needs have no trait expression yet — an out-of-band bus
    /// WRITE for the memory editor, and APU access for channel capture
    /// (typed CPU registers were the third, closed by W13-02i).
    /// Boxing before those close would mean bypassing the trait through a
    /// downcast, which is worse than holding the concrete type honestly.
    machine: Machine,
    /// Frames completed this session (ticket W11-12).
    ///
    /// Incremented by [`Self::advance`] — the ONE place that calls the
    /// core — so every granularity counts. Counting only on the
    /// `Step::Frame` path is a hang, not a miscount: a caller stepping
    /// scanlines and waiting for `frame_count` to move waits for ever,
    /// and two of this module's own tests do exactly that.
    ///
    /// Counted by the SHELL, from `StepResult::frame_complete`, rather
    /// than read off a console's own counter. `NesBus::frame_count`
    /// exists; the SNES has no equivalent the shell can reach, so
    /// consulting the NES bus returned 0 forever on a SNES session — and
    /// since `run_until_next_frame` compared that counter against itself
    /// to decide whether a frame had advanced, the answer was always
    /// "no" and the app presented nothing. One counter, both consoles.
    frames: u64,
    state: RunState,
    /// Defensive loop bound in master cycles — [`CYCLE_BUDGET`] in
    /// production, overridden much smaller by tests that need to prove the
    /// bound actually terminates a loop rather than merely existing in the
    /// source.
    cycle_budget: u64,
    /// Normalized SHA-256 of the loaded ROM (ticket W2-04), computed once
    /// at load time via `rf_cart::identity_nes` — the same normalized
    /// convention `tests/rom-manifest.toml` and `.rfreplay` use. Save
    /// states carry it in their header so a state can be refused against
    /// the wrong game (FR-STATE-003).
    rom_sha256: [u8; 32],
    /// Last visible scanline drawn, for the transport position readout
    /// (ticket W2-15). `None` until the first visible line completes.
    /// See [`Self::last_scanline`] for the deliberate limitation.
    last_scanline: Option<u16>,
}

impl EmuStepper {
    /// Parse `raw` as an iNES/NES 2.0 image and power on a fresh machine
    /// from it, starting [`RunState::Paused`] (a freshly opened ROM waits
    /// for the user to press Run, matching a debugger-first workflow).
    ///
    /// Build a session for whichever console this image is (ticket
    /// W11-12).
    ///
    /// Until now `core_thread::spawn` called
    /// [`Self::from_ines_bytes`] unconditionally, so a SNES ROM was
    /// refused as "only NES ROMs are supported" — while the library
    /// scanner happily
    /// identified it as `Recognized { console: Snes }`, listed it with a
    /// Play button and offered a SNES filter. The product advertised a
    /// feature it could not perform.
    ///
    /// # Errors
    /// [`OpenError::Nes`] or [`OpenError::Snes`] for an image the
    /// matching core refuses, and [`OpenError::Unrecognized`] for one
    /// neither claims.
    pub fn open(raw: &[u8]) -> Result<Self, OpenError> {
        // `rf_cart` decides which console this is, rather than this
        // function sniffing magic bytes a second time — one identifier,
        // used by the library scanner and by this, so a file cannot be
        // listed as one console and opened as another.
        match rf_cart::Cartridge::load(raw) {
            Ok(rf_cart::Cartridge::Nes { .. }) => {
                Self::from_ines_bytes(raw).map_err(OpenError::Nes)
            }
            Ok(rf_cart::Cartridge::Snes { .. }) => Self::from_snes_bytes(raw),
            Err(e) => Err(OpenError::Unrecognized(format!("{e}"))),
        }
    }

    /// Build a SNES session.
    ///
    /// # Errors
    /// [`OpenError::Snes`] for an image `rf_snes` refuses.
    pub fn from_snes_bytes(raw: &[u8]) -> Result<Self, OpenError> {
        let core =
            rf_snes::core::SnesCore::load(raw).map_err(|e| OpenError::Snes(format!("{e}")))?;
        let identity = rf_cart::hash::identity_snes(raw);
        let mut rom_sha256 = [0u8; 32];
        hex_to_bytes(&identity.normalized.sha256, &mut rom_sha256);
        Ok(EmuStepper {
            machine: Machine::Snes(Box::new(core)),
            frames: 0,
            state: RunState::Paused,
            cycle_budget: CYCLE_BUDGET,
            last_scanline: None,
            rom_sha256,
        })
    }
    /// rejects (bad magic, unimplemented mapper, ...).
    pub fn from_ines_bytes(raw: &[u8]) -> Result<Self, NesLoadError> {
        let mut core = rf_nes::core::NesCore::from_ines_bytes(raw)?;
        let bus = core.bus_mut();
        // Ticket W4-03e: the enhanced camera's scroll/scene tracking
        // (`crate::canvas_accum::CanvasAccumulator`, driven by
        // `crate::core_thread`) needs `CoreEvent::Scanline`/`ScrollWrite`
        // on every `FrameBundle` — before this, `EventMask` stayed at its
        // `CoreConfig` default (`EventMask::NONE`, `rf_core_api::core`'s own
        // doc), so nothing anywhere in this crate ever received those
        // events and the whole enhanced-camera pipeline would have silently
        // stitched nothing, forever. `EventMask` only gates which
        // `CoreEvent` variants get CONSTRUCTED (`rf_core_api::event`'s own
        // doc: "callers ... MUST check ... before building the matching
        // CoreEvent variant") — it cannot perturb simulation state, so
        // turning it on here does not touch Law 6 (Accuracy Mode stays an
        // unmodified simulation; this is metadata plumbing, not gameplay).
        bus.set_event_mask(CAMERA_BASELINE_EVENT_MASK);
        // Ticket W2-04: the NORMALIZED hash (header stripped), matching
        // `tests/rom-manifest.toml` and `.rfreplay`'s `rom_sha256`, so the
        // same cartridge dumped with a different header still matches its
        // own save states.
        let identity = rf_cart::hash::identity_nes(raw);
        let mut rom_sha256 = [0u8; 32];
        hex_to_bytes(&identity.normalized.sha256, &mut rom_sha256);
        Ok(EmuStepper {
            machine: Machine::Nes(Box::new(core)),
            frames: 0,
            state: RunState::Paused,
            cycle_budget: CYCLE_BUDGET,
            rom_sha256,
            last_scanline: None,
        })
    }

    /// The loaded ROM's normalized SHA-256 (ticket W2-04), as save-state
    /// headers and `.rfreplay` record it.
    #[must_use]
    pub fn rom_sha256(&self) -> [u8; 32] {
        self.rom_sha256
    }

    /// Borrowed bus for battery-RAM persistence (ticket W2-04) — narrower
    /// than exposing the whole machine, and the only reason it is `pub`:
    /// `crate::save_state`'s `.sav` helpers take a bus, so a caller that
    /// owns a stepper needs a way to hand one over.
    #[must_use]
    pub fn bus_for_battery(&self) -> Option<&NesBus> {
        self.machine.nes_bus()
    }

    /// Mutable counterpart of [`Self::bus_for_battery`], for loading a
    /// `.sav` at startup.
    pub fn bus_for_battery_mut(&mut self) -> Option<&mut NesBus> {
        self.machine.nes_bus_mut()
    }

    /// Borrowed CPU, for `crate::save_state`'s serializer.
    pub(crate) fn cpu_for_state(&self) -> Option<&Cpu> {
        self.machine.nes_cpu()
    }

    /// Borrowed bus, for `crate::save_state`'s serializer.
    pub(crate) fn bus_for_state(&self) -> Option<&NesBus> {
        self.machine.nes_bus()
    }

    /// Both halves mutably, for `crate::save_state`'s loader — one call
    /// rather than two accessors, so a caller cannot restore a CPU into a
    /// bus from a different state.
    pub(crate) fn machine_for_state(&mut self) -> Option<(&mut Cpu, &mut NesBus)> {
        match &mut self.machine {
            Machine::Nes(c) => Some(c.parts_mut()),
            Machine::Snes(_) => None,
        }
    }

    /// Test-only hook to prove [`CYCLE_BUDGET`]'s termination guarantee
    /// without waiting out the production budget — see module doc's "Why
    /// every loop in here is cycle-bounded".
    #[cfg(test)]
    fn set_cycle_budget_for_test(&mut self, budget: u64) {
        self.cycle_budget = budget;
        // Ticket W11-10: the bound lives in the core now, so the lever
        // has to reach it. Setting only the shell-side copy left the
        // termination proofs asserting against a budget nothing read.
        self.machine.set_cycle_budget(budget);
    }

    /// Current run/paused state.
    #[must_use]
    pub fn state(&self) -> RunState {
        self.state
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.state == RunState::Paused
    }

    /// Total frames completed since power-on (`NesBus::frame_count`).
    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.frames
    }

    /// Last visible scanline drawn, for the transport position readout
    /// (ticket W2-15). `None` until the first visible line completes.
    ///
    /// **Deliberate limitation, documented rather than papered over:**
    /// `CoreSink::video_scanline` only fires for the 240 *visible* lines,
    /// so this reports the last visible line and does not tick through
    /// vblank (lines 240-260). That matches what
    /// [`Self::step_scanline`] actually does — it runs until a visible
    /// scanline completes, so a single press during vblank advances
    /// through the rest of vblank into line 0 rather than one line. That
    /// quirk is `step_scanline`'s, not this readout's; this method simply
    /// does not lie about it.
    #[must_use]
    pub fn last_scanline(&self) -> Option<u16> {
        self.last_scanline
    }

    /// Side-effect-free memory peek (`NesBus::peek`) — a debugger memory
    /// view, what the determinism suite's positive WRAM assertion reads
    /// (ticket W1-07), and what a value-conditional breakpoint evaluates
    /// through plus what step-over reads to see whether the next opcode
    /// is a `JSR` (ticket W4-06e).
    #[must_use]
    pub fn peek(&self, addr: u16) -> u8 {
        self.machine.as_core_ref().peek(u32::from(addr))
    }

    /// Out-of-band bus write — the write-side counterpart to
    /// [`Self::peek`], routed through the same `rf_nes::CpuBus::write` a
    /// real CPU instruction uses (already public `rf-nes` API; this does
    /// not touch that crate's source at all), so writing e.g. `$2001`
    /// (PPUMASK) genuinely toggles rendering the way a real program's `STA
    /// $2001` would.
    ///
    /// Ticket W4-01's own use: `retroforge::mode_invariant`'s harness
    /// self-test simulates "an enhancement that reaches into PPU registers
    /// instead of going through the documented overlay channel", proving
    /// the strong (indexed-pixel) mode-invariant check catches what
    /// [`Self::state_hash`] structurally cannot (that hash never includes
    /// PPU registers — see its own doc). Also the generically useful
    /// write-side of a debugger memory view.
    pub fn poke_bus(&mut self, addr: u16, value: u8) {
        self.machine.bus_write(addr, value);
    }

    /// The full 256-byte OAM as last written — forwards `NesBus::oam`
    /// (ticket W3-05a: the mode-invariant test suite's fixture-sanity
    /// check reads this to prove its sprite table actually landed).
    #[must_use]
    pub fn oam(&self) -> &[u8; 256] {
        self.machine
            .nes_bus()
            .map_or(&EMPTY_OAM, rf_nes::NesBus::oam)
    }

    /// Every memory the running console exposes, through the one
    /// accessor that answers for **both** cores (ticket W13-02a).
    ///
    /// The typed helpers below it (`vram`, `palette`, `oam`) are
    /// NES-shaped by their return types — `&[u8; 0x1000]` is a NES
    /// nametable, not 64 KiB of SNES VRAM — so they answer with an empty
    /// default on a SNES session and always will. This is what a viewer
    /// that wants to serve both consoles reads instead; W13-02b builds
    /// those viewers on it.
    ///
    #[must_use]
    pub fn state_view(&self) -> rf_core_api::StateView<'_> {
        self.machine.as_core_ref().state_view()
    }

    /// The running console's CPU register file, typed, through the one
    /// trait path both cores answer (ticket W13-02i). This is what the
    /// register readout draws; it never reaches into a core.
    #[must_use]
    pub fn cpu_regs(&self) -> rf_core_api::CpuRegs {
        self.state_view().cpu_regs
    }

    /// One bsnes-shaped trace line for a SNES session, or empty on a NES
    /// one (ticket W13-02g).
    ///
    /// The peek is `SnesCore::peek`, which resolves through the same
    /// mapping the CPU uses and **answers 0 for a register rather than
    /// reading it** — a trace that latched `$2139` while describing the
    /// instruction would change the run it is describing.
    fn snes_trace_line(&self) -> String {
        let Machine::Snes(core) = &self.machine else {
            return String::new();
        };
        struct CorePeek<'a>(&'a rf_snes::core::SnesCore);
        impl rf_snes::trace::TracePeek for CorePeek<'_> {
            fn peek(&self, addr: u32) -> u8 {
                rf_core_api::EmulatorCore::peek(self.0, addr)
            }
        }
        rf_snes::trace::format_trace_line(&core.system().cpu, &CorePeek(core))
    }

    /// The SNES debug memories, or `None` on a NES session (ticket
    /// W13-02b).
    ///
    /// Returns owned copies because they cross a thread boundary on the
    /// frame message; four of the five come straight from
    /// [`Self::state_view`], and ARAM is the one that does not — the APU's
    /// RAM is not a `StateView` field, so it is read from the concrete
    /// core here rather than by widening the contract for one consumer.
    #[must_use]
    pub fn snes_debug_snapshot(&self) -> Option<crate::core_thread::SnesDebugFrame> {
        let Machine::Snes(core) = &self.machine else {
            return None;
        };
        let view = self.machine.as_core_ref().state_view();
        Some(crate::core_thread::SnesDebugFrame {
            vram: view.vram.to_vec(),
            cgram: view.cgram.to_vec(),
            oam: view.oam.to_vec(),
            ppu_regs: view.ppu_regs.to_vec(),
            aram: core.system().bus.apu.aram.clone(),
            mode7: core.system().bus.ppu.mode7,
            hdma_lanes: core.system().bus.hdma_lanes().to_vec(),
            voices: rf_snes::debug::voice_views(&core.system().bus.apu.dsp),
        })
    }

    /// The PPU's nametable VRAM (ticket W4-06d) — forwards
    /// `NesBus::vram()`, a non-observing borrow.
    pub fn vram(&self) -> &[u8; 0x1000] {
        self.machine
            .nes_bus()
            .map_or(&EMPTY_VRAM, rf_nes::NesBus::vram)
    }

    /// The PPU's palette RAM (ticket W4-06d) — forwards
    /// `NesBus::palette()`, raw and unmirrored.
    pub fn palette(&self) -> &[u8; 32] {
        self.machine
            .nes_bus()
            .map_or(&EMPTY_PALETTE, rf_nes::NesBus::palette)
    }

    /// PPUCTRL bit 5 (ticket W16-13): 8 or 16, the current sprite height —
    /// forwards `NesBus::sprite_height()`. Defaults to 8 on a SNES session
    /// (no NES PPUCTRL exists there) and whenever no core is loaded, the
    /// same "answer with the ordinary case" degrade `oam`/`vram`/`palette`
    /// above already use.
    #[must_use]
    pub fn sprite_height_px(&self) -> u8 {
        self.machine
            .nes_bus()
            .map_or(8, rf_nes::NesBus::sprite_height)
    }

    /// Ticket W11-05: record the tiles the NES PPU draws. No-op on SNES —
    /// Mesen HD packs are an NES format.
    pub fn set_tile_capture(&mut self, on: bool) {
        if let Some(bus) = self.machine.nes_bus_mut() {
            bus.set_tile_capture(on);
        }
    }

    /// The background tiles of the frame just completed. Empty unless
    /// capture is on, and always empty on SNES.
    #[must_use]
    pub fn completed_tiles(&self) -> &[rf_nes::ppu::DrawnTile] {
        self.machine
            .nes_bus()
            .map_or(&[], rf_nes::NesBus::completed_tiles)
    }

    /// The NES cartridge's CHR, for a pack rule keyed on tile bytes.
    #[must_use]
    pub fn chr(&self) -> &[u8] {
        self.machine.nes_bus().map_or(&[], rf_nes::NesBus::chr)
    }

    /// Ticket W11-05: the tiles this frame drew, as HD-pack placements.
    ///
    /// **Built here, in the shell, and that is the layer boundary doing
    /// its job** (ARCHITECTURE §6): `rf-enhance` is console-agnostic and
    /// `rf-nes` may not depend on it, so neither can name the other's
    /// type. The mediator converts, exactly as it does for widescreen's
    /// `LayerView`.
    ///
    /// Empty on SNES and whenever capture is off.
    #[must_use]
    pub fn hd_placements(&self) -> Vec<rf_enhance::hd_render::Placement> {
        let tiles = self.completed_tiles();
        let palette = self.palette();
        let chr = self.chr();
        let chr_is_ram = self
            .machine
            .nes_bus()
            .is_some_and(rf_nes::NesBus::chr_is_ram);
        let sprites = self
            .machine
            .nes_bus()
            .map_or(&[][..], rf_nes::NesBus::completed_sprites);
        tiles
            .iter()
            .map(|t| {
                // A pack rule is keyed on the FOUR palette bytes: the
                // universal backdrop at $3F00 plus the three colours of
                // the tile's own background palette. Entry 0 of every
                // palette mirrors the backdrop on hardware, so using
                // `palette[0]` here is the value a pack author sees.
                let p = usize::from(t.palette & 0x03) * 4;
                let colours = [
                    palette[0] & 0x3F,
                    palette[p + 1] & 0x3F,
                    palette[p + 2] & 0x3F,
                    palette[p + 3] & 0x3F,
                ];
                let tile = if chr_is_ram {
                    // CHR RAM: the rule is keyed on the tile's BYTES,
                    // because the index means nothing when the game
                    // rewrites the pattern table as it plays.
                    let at = usize::from(t.base) + usize::from(t.tile) * 16;
                    let mut bytes = [0u8; 16];
                    for (i, b) in bytes.iter_mut().enumerate() {
                        *b = chr.get(at + i).copied().unwrap_or(0);
                    }
                    rf_enhance::hdpack::TileData::ChrRam(bytes)
                } else {
                    // CHR ROM: the tile's index within the whole of CHR,
                    // so a tile in the $1000 table is 256 higher than the
                    // same index in the $0000 one — which is what Mesen's
                    // own writer emits.
                    let index = u32::from(t.base) / 16 + u32::from(t.tile);
                    rf_enhance::hdpack::TileData::ChrRom(index)
                };
                rf_enhance::hd_render::Placement {
                    x: t.x,
                    y: t.y as i16,
                    tile,
                    palette: colours,
                    layer: rf_enhance::hd_render::Layer::Background,
                    // Background tiles cannot be flipped on the NES.
                    flip_x: false,
                    flip_y: false,
                }
            })
            .chain(sprites.iter().map(|s| {
                // **Sprite palettes are `$3F10`-relative**, not `$3F00`.
                // Using the background base here would key every sprite
                // rule on the wrong four bytes and match nothing — the
                // kind of failure that looks like "packs do not work".
                let p = 0x10 + usize::from(s.palette & 0x03) * 4;
                let colours = [
                    palette[0] & 0x3F,
                    palette[p + 1] & 0x3F,
                    palette[p + 2] & 0x3F,
                    palette[p + 3] & 0x3F,
                ];
                let tile = if chr_is_ram {
                    let at = usize::from(s.base) + usize::from(s.tile) * 16;
                    let mut bytes = [0u8; 16];
                    for (i, b) in bytes.iter_mut().enumerate() {
                        *b = chr.get(at + i).copied().unwrap_or(0);
                    }
                    rf_enhance::hdpack::TileData::ChrRam(bytes)
                } else {
                    rf_enhance::hdpack::TileData::ChrRom(u32::from(s.base) / 16 + u32::from(s.tile))
                };
                rf_enhance::hd_render::Placement {
                    x: s.x,
                    y: s.y,
                    tile,
                    palette: colours,
                    layer: rf_enhance::hd_render::Layer::Sprite,
                    flip_x: s.flip_x,
                    flip_y: s.flip_y,
                }
            }))
            .collect()
    }
}

/// One tile the Upscale Studio (ticket W16-02) saw this frame,
/// decoded to indexed pixels rather than left as CHR bytes — the
/// studio needs `rf_ai::pipeline::ExtractedAsset`'s exact shape
/// (indexed pixels + an RGBA palette), and only this thread can peek
/// CHR/palette RAM without perturbing the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudioTileCapture {
    /// The pack-format identity Mesen and `rf_enhance::hdpack` match
    /// on — kept alongside the decoded pixels so a caller that wants
    /// the CHR-based identity later does not have to re-derive it.
    pub tile: rf_enhance::hdpack::TileData,
    /// The RAW NES 6-bit colour codes `rf_enhance::hdpack::TileKey::palette`
    /// matches on — the exact 4 bytes `hd_placements` resolves and Mesen's
    /// own `hires.txt` writes as 8 hex characters. NOT the same thing as
    /// `palette_rgba` below: that one is decoded RGB for image processing,
    /// this one is the raw identity a Mesen pack keys on.
    pub mesen_palette: [u8; 4],
    pub layer: rf_enhance::hd_render::Layer,
    pub x: i32,
    pub y: i32,
    /// 8x8, values `0..=3` — the tile's own 2bpp planar indices.
    pub indexed_pixels: [u8; 64],
    /// Four RGBA quads, one per index in `indexed_pixels`. Entry 0 is
    /// the backdrop for a background tile (opaque — hardware always
    /// draws it) and fully transparent for a sprite tile (index 0 is
    /// never drawn on a sprite — nesdev.org/wiki/PPU_OAM).
    pub palette_rgba: [u8; 16],
}

/// Decode one CHR tile's 16 pattern bytes into 8x8 palette indices.
///
/// The NES 2bpp planar layout (nesdev.org/wiki/PPU_pattern_tables):
/// the first 8 bytes are the low bitplane (one bit per pixel, one
/// byte per row), the next 8 are the high bitplane for the same
/// rows; a pixel's index is `(high_bit << 1) | low_bit`.
fn decode_nes_tile_indices(bytes: &[u8; 16]) -> [u8; 64] {
    let mut out = [0u8; 64];
    // Both loops are fixed 0..8 ranges (CLAUDE.md law 8): they
    // terminate structurally regardless of the tile's contents.
    for y in 0..8usize {
        let lo = bytes[y];
        let hi = bytes[y + 8];
        for x in 0..8usize {
            let bit = 7 - x;
            let index = ((hi >> bit) & 1) << 1 | ((lo >> bit) & 1);
            out[y * 8 + x] = index;
        }
    }
    out
}

/// The 4-entry RGBA palette a decoded tile's indices point into.
///
/// `codes` are the four NES 6-bit colour codes `hd_placements` already
/// resolves (`palette[0]` backdrop plus the tile's own three colours).
/// Index 0's alpha is the one place background and sprite tiles
/// differ: a sprite's colour 0 is never drawn on real hardware
/// (nesdev.org/wiki/PPU_OAM, "Sprite palette entry 0 is transparent"),
/// so a captured sprite tile must carry that as an actual alpha-0
/// pixel or the studio's post-processing edge mask (`rf_ai::studio`)
/// has nothing to preserve.
fn tile_palette_rgba(codes: [u8; 4], layer: rf_enhance::hd_render::Layer) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, code) in codes.iter().enumerate() {
        let rgb = rf_renderer::palette_index_to_rgb(*code);
        let alpha = if i == 0 && matches!(layer, rf_enhance::hd_render::Layer::Sprite) {
            0
        } else {
            255
        };
        out[i * 4] = rgb[0];
        out[i * 4 + 1] = rgb[1];
        out[i * 4 + 2] = rgb[2];
        out[i * 4 + 3] = alpha;
    }
    out
}

impl EmuStepper {
    /// This frame's tiles, decoded for the Upscale Studio (ticket
    /// W16-02) rather than left as [`Self::hd_placements`]'s bare
    /// identities.
    ///
    /// A sibling to [`Self::hd_placements`], not a wrapper around it:
    /// the two need different information from the same underlying
    /// `completed_tiles`/`completed_sprites` (this one needs the raw
    /// 16 CHR bytes, which `hd_placements` intentionally discards down
    /// to a bare index for the CHR-ROM case). Duplicating the
    /// base/tile addressing arithmetic here, rather than changing
    /// `hd_placements`'s return type, keeps that method's existing
    /// callers and its `rf_enhance::hd_render::Placement` contract
    /// untouched.
    ///
    /// Empty on SNES and whenever tile capture is off, same as
    /// [`Self::hd_placements`].
    #[must_use]
    pub fn studio_captures(&self) -> Vec<StudioTileCapture> {
        let tiles = self.completed_tiles();
        let palette = self.palette();
        let chr = self.chr();
        let chr_is_ram = self
            .machine
            .nes_bus()
            .is_some_and(rf_nes::NesBus::chr_is_ram);
        let sprites = self
            .machine
            .nes_bus()
            .map_or(&[][..], rf_nes::NesBus::completed_sprites);

        let chr_bytes_at = |at: usize| -> [u8; 16] {
            let mut bytes = [0u8; 16];
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = chr.get(at + i).copied().unwrap_or(0);
            }
            bytes
        };

        let mut out = Vec::with_capacity(tiles.len() + sprites.len());
        for t in tiles {
            let p = usize::from(t.palette & 0x03) * 4;
            let codes = [
                palette[0] & 0x3F,
                palette[p + 1] & 0x3F,
                palette[p + 2] & 0x3F,
                palette[p + 3] & 0x3F,
            ];
            let (tile, bytes) = if chr_is_ram {
                let at = usize::from(t.base) + usize::from(t.tile) * 16;
                let bytes = chr_bytes_at(at);
                (rf_enhance::hdpack::TileData::ChrRam(bytes), bytes)
            } else {
                let index = u32::from(t.base) / 16 + u32::from(t.tile);
                let bytes = chr_bytes_at(index as usize * 16);
                (rf_enhance::hdpack::TileData::ChrRom(index), bytes)
            };
            let layer = rf_enhance::hd_render::Layer::Background;
            out.push(StudioTileCapture {
                tile,
                mesen_palette: codes,
                layer,
                x: i32::from(t.x),
                y: i32::from(t.y),
                indexed_pixels: decode_nes_tile_indices(&bytes),
                palette_rgba: tile_palette_rgba(codes, layer),
            });
        }
        for s in sprites {
            let p = 0x10 + usize::from(s.palette & 0x03) * 4;
            let codes = [
                palette[0] & 0x3F,
                palette[p + 1] & 0x3F,
                palette[p + 2] & 0x3F,
                palette[p + 3] & 0x3F,
            ];
            let (tile, bytes) = if chr_is_ram {
                let at = usize::from(s.base) + usize::from(s.tile) * 16;
                let bytes = chr_bytes_at(at);
                (rf_enhance::hdpack::TileData::ChrRam(bytes), bytes)
            } else {
                let index = u32::from(s.base) / 16 + u32::from(s.tile);
                let bytes = chr_bytes_at(index as usize * 16);
                (rf_enhance::hdpack::TileData::ChrRom(index), bytes)
            };
            let layer = rf_enhance::hd_render::Layer::Sprite;
            out.push(StudioTileCapture {
                tile,
                mesen_palette: codes,
                layer,
                x: i32::from(s.x),
                y: i32::from(s.y),
                indexed_pixels: decode_nes_tile_indices(&bytes),
                palette_rgba: tile_palette_rgba(codes, layer),
            });
        }
        out
    }
}

impl EmuStepper {
    /// Side-effect-free 2 KiB WRAM snapshot (`$0000-$07FF`, the real
    /// backing 2 KiB — not its `$0800`-stepped mirrors, same span
    /// `Self::state_hash`'s own doc already enumerates as "reachable
    /// state") — ticket W4-06b's debug memory-viewer panel (FR-DBG-002).
    /// Built by looping [`Self::peek`], so it carries that method's exact
    /// side-effect-free guarantee: looping it 2048 times doesn't change
    /// which registers/counters it touches (none). `PRG-RAM`'s equivalent
    /// ([`Self::prg_ram`]) needs no loop because `NesBus` already exposes
    /// that window as a direct slice.
    #[must_use]
    pub fn wram_snapshot(&self) -> [u8; 0x0800] {
        let mut out = [0u8; 0x0800];
        for (addr, byte) in out.iter_mut().enumerate() {
            *byte = self.peek(addr as u16);
        }
        out
    }

    /// The cartridge PRG-RAM window (`$6000-$7FFF`, 8 KiB) — forwards
    /// `NesBus::prg_ram`, already side-effect-free (that method's own
    /// doc). Ticket W4-06b's debug memory-viewer panel's SECOND live
    /// range, alongside [`Self::wram_snapshot`]: commercial-game
    /// `memory_map` annotations very often live here, not in WRAM (e.g.
    /// `profiles/nes/rf-scroller-demo/profile.toml`'s `player_x`/
    /// `camera_x` at `$6029`/`$602B`) — a viewer covering WRAM alone would
    /// never surface the addresses this project's own example profile
    /// annotates.
    #[must_use]
    pub fn prg_ram(&self) -> &[u8; 0x2000] {
        self.machine
            .nes_bus()
            .map_or(&EMPTY_PRG_RAM, rf_nes::NesBus::prg_ram)
    }

    /// Whether the sprite-limit-bypass overlay is currently recording
    /// (ticket W3-05a) — forwards `NesBus::sprite_overlay_enabled`. `false`
    /// on a freshly opened ROM (law 6: a fresh install boots in Accuracy
    /// Mode).
    #[must_use]
    pub fn sprite_overlay_enabled(&self) -> bool {
        self.machine
            .nes_bus()
            .is_some_and(rf_nes::NesBus::sprite_overlay_enabled)
    }

    /// Opt into (or out of) the sprite-limit-bypass overlay (ticket
    /// W3-05a) — forwards `NesBus::set_sprite_overlay_enabled`.
    /// Ticket W11-03: ask the SNES core for a widened picture.
    ///
    /// A no-op on NES, and deliberately silent about it: widescreen is a
    /// SNES feature (`rf_enhance::widescreen` decides per SNES background
    /// layer), and a NES session simply has nothing to widen. Returning
    /// an error the caller would have to ignore would be noise.
    pub fn set_widescreen(&mut self, width: usize, bg: [bool; 4], obj: bool) {
        if let Machine::Snes(core) = &mut self.machine {
            core.set_widescreen(width, rf_snes::ppu::WidenMask { bg, obj });
        }
    }

    /// Turn widescreen off, restoring the accuracy path exactly.
    pub fn set_widescreen_off(&mut self) {
        if let Machine::Snes(core) = &mut self.machine {
            core.set_widescreen(rf_snes::ppu::WIDTH, rf_snes::ppu::WidenMask::ALL);
        }
    }

    /// Each SNES background's geometry, as the widescreen policy engine
    /// wants it. `None` on NES.
    ///
    /// **The conversion happens HERE, in the shell, and that is the layer
    /// boundary doing its job** (ARCHITECTURE §6): `rf-snes` may not
    /// import `rf-enhance`, so the core hands out plain numbers and the
    /// mediator turns them into a `LayerView`.
    #[must_use]
    pub fn bg_layer_views(&self) -> Option<[rf_enhance::widescreen::LayerView; 4]> {
        let Machine::Snes(core) = &self.machine else {
            return None;
        };
        let g = core.bg_geometry();
        Some(std::array::from_fn(|i| rf_enhance::widescreen::LayerView {
            tilemap_width: g[i].tilemap_width,
            tilemap_height: g[i].tilemap_height,
            hofs: g[i].hofs,
            vofs: g[i].vofs,
            enabled: g[i].enabled,
        }))
    }

    pub fn set_sprite_overlay_enabled(&mut self, enabled: bool) {
        self.machine.set_sprite_overlay(enabled);
    }

    /// Widen (or narrow) which `CoreEvent`s this machine emits (ticket
    /// W4-06a: the debugger's event viewer subscribes/unsubscribes as its
    /// panel opens/closes, DEBUGGER.md §6's "closed panels register no
    /// event subscriptions" discipline). `NesBus::set_event_mask` *replaces*
    /// the mask outright (`rf_nes::Ppu::set_event_mask`'s own doc: `self.
    /// event_mask = mask`) — a bare passthrough here would let a debugger
    /// toggle silently drop [`CAMERA_BASELINE_EVENT_MASK`], the bits
    /// `Self::from_ines_bytes` turns on unconditionally for the enhanced-
    /// camera pipeline (`crate::canvas_accum`), breaking scene tracking the
    /// moment the event panel is opened once. Always unioning the baseline
    /// back in makes that invariant hold by construction rather than by
    /// caller discipline.
    /// Install the debugger's watchpoints (ticket W13-02e).
    ///
    /// Goes through `CoreConfig`, not a concrete bus method, because this
    /// is the one debugger facility that works the same way on both
    /// cores: `EmulatorCore::config` is on the trait, so a SNES session
    /// installs — and since W13-02h *reports* — through the identical
    /// call.
    ///
    /// Returns how many watches the core refused for want of room, so a
    /// caller can say so rather than leaving someone watching an address
    /// that is not actually armed.
    pub fn set_watches(&mut self, watches: &[rf_core_api::MemWatch]) -> usize {
        self.machine.as_core().config().watches.set(watches)
    }

    pub fn set_event_mask(&mut self, mask: rf_core_api::EventMask) {
        // Ticket W13-02h: the SNES core reads its mask from `CoreConfig`
        // at the top of every step, so setting it there covers BOTH cores
        // — the NES bus call below stays because rf-nes's own mask lives
        // on its PPU and is not read from the config (a pre-existing
        // asymmetry, not one this ticket introduces).
        self.machine.as_core().config().event_mask = mask.union(CAMERA_BASELINE_EVENT_MASK);
        if let Some(bus) = self.machine.nes_bus_mut() {
            bus.set_event_mask(mask.union(CAMERA_BASELINE_EVENT_MASK));
        }
    }

    /// Stop advancing on repaint ticks. Idempotent.
    pub fn pause(&mut self) {
        self.state = RunState::Paused;
    }

    /// Start advancing one frame per [`Self::tick_running`] call.
    /// Idempotent.
    pub fn resume(&mut self) {
        self.state = RunState::Running;
    }

    /// Run exactly one whole frame's worth of instructions, streaming
    /// scanlines to `sink` as they complete, regardless of the current run
    /// state — then force [`RunState::Paused`] (stepping is a debugger
    /// action; it always leaves the machine stopped so the next click is
    /// unambiguous, FR-DBG-004). Returns the number of frames completed:
    /// always exactly 1 under normal operation (`NesBus::frame_count`
    /// cannot advance by more than 1 within a single `Cpu::step` — even
    /// OAM DMA's ~4.5-scanline stall is far short of one full 262-scanline
    /// frame), or 0 only if [`CYCLE_BUDGET`]'s defensive bound fired
    /// first (module doc's "Why every loop in here is cycle-bounded") —
    /// which should never happen for any ROM this emulator runs correctly.
    /// Execute exactly ONE CPU instruction (ticket W4-06e).
    ///
    /// This is the instruction-level seam W4-06e needed and `step_frame`/
    /// `step_scanline` did not provide. It lives HERE, in the shell,
    /// rather than as a breakpoint check inside `rf-nes`'s step loop —
    /// route (a) of the two the ticket named. See
    /// `rf_debugger::breakpoint`'s module doc for the full reasoning; the
    /// short version is that every condition acceptance criterion 1 lists
    /// is answerable at an instruction boundary, so putting debugger
    /// concerns on the core's hot path would buy nothing.
    ///
    /// Drains video/audio afterwards exactly as the other steppers do, so
    /// single-stepping cannot silently grow the PPU's completed-scanline
    /// queue (`Ppu::drain`'s own warning, and the trap `event_emission.rs`
    /// records having fallen into).
    pub fn step_instruction(&mut self, sink: &mut dyn CoreSink) -> u32 {
        // Ticket W11-10: through the trait. The CPU step and the two
        // drains that used to be written out here are one instruction's
        // worth of machine, and `rf_nes::core::NesCore` owns that now.
        let mut counting = CountingSink {
            inner: sink,
            scanlines: 0,
            last_scanline: self.last_scanline,
        };
        let result = self.advance(rf_core_api::Step::Instruction, &mut counting);
        self.last_scanline = counting.last_scanline;
        self.state = RunState::Paused;
        u32::try_from(result.cycles).unwrap_or(u32::MAX)
    }

    /// Turn per-channel audio capture on or off (ticket W4-10b).
    ///
    /// Off by default. Enabling it cannot change `take_samples`, the
    /// mixed output, or any machine state — see
    /// [`rf_nes::apu::Apu::set_channel_capture`] and the invariance test
    /// in `tests/audio_scope_invariance.rs`.
    pub fn set_audio_channel_capture(&mut self, on: bool) {
        self.machine.set_channel_capture(on);
    }

    /// Drain the per-channel scope streams captured since the last call.
    pub fn take_audio_channel_samples(&mut self) -> [Vec<i16>; rf_nes::apu::CHANNEL_COUNT] {
        self.machine.take_channel_samples().unwrap_or_default()
    }

    /// The CPU's current program counter — the address the NEXT
    /// instruction will execute from, which is what a PC breakpoint and
    /// run-to-cursor both compare against.
    #[must_use]
    pub fn pc(&self) -> u16 {
        self.machine.nes_cpu().map_or(0, |c| c.pc)
    }

    /// The CPU's stack pointer, for step-over/step-out's frame tracking
    /// (`rf_debugger::breakpoint::frame_has_returned`).
    #[must_use]
    pub fn sp(&self) -> u8 {
        self.machine.nes_cpu().map_or(0, |c| c.s)
    }

    pub fn step_frame(&mut self, sink: &mut dyn CoreSink) -> u64 {
        let advanced = self.run_until_next_frame(sink);
        self.state = RunState::Paused;
        advanced
    }

    /// Run until at least one more scanline has been drained through
    /// `sink`, then stop and force [`RunState::Paused`] (FR-DBG-004). See
    /// module doc for the one case (an OAM-DMA-triggering instruction)
    /// where more than one scanline can complete in a single call. Returns
    /// the actual number of scanlines drained: normally 1 (occasionally
    /// more, see module doc), or 0 only if [`CYCLE_BUDGET`]'s defensive
    /// bound fired before any scanline completed.
    pub fn step_scanline(&mut self, sink: &mut dyn CoreSink) -> u32 {
        // Ticket W11-10: the loop, the cycle budget and the per-
        // instruction audio drain all live in `rf_nes::core::NesCore`
        // now. The budget in particular is core knowledge — it exists
        // because `Cpu::step` is not guaranteed to reach a boundary — and
        // the shell had been the one enforcing it.
        let mut counting = CountingSink {
            inner: sink,
            scanlines: 0,
            last_scanline: self.last_scanline,
        };
        self.advance(rf_core_api::Step::Scanline, &mut counting);
        let n = counting.scanlines;
        self.last_scanline = counting.last_scanline;
        self.state = RunState::Paused;
        n
    }

    /// Adopt the machine's own frame count after a state load.
    ///
    /// The NES container carries `NesBus::frame_count`, so a restored
    /// machine knows how many frames it has seen; the shell's counter
    /// does not, and would otherwise keep counting from wherever this
    /// session happened to be. SNES states are refused today
    /// (`SaveStateError::UnsupportedConsole`), so there is nothing to
    /// adopt on that path yet.
    pub(crate) fn resync_frame_count(&mut self) {
        if let Some(bus) = self.machine.nes_bus() {
            self.frames = bus.frame_count();
        }
    }

    /// Drive the core one step, counting a frame if one completed.
    ///
    /// Every path through this type goes through here. The alternative —
    /// each caller remembering to bump the counter — lasted about ten
    /// minutes: `step_scanline` did not, so
    /// `step_scanline_calls_accumulate_a_full_visible_frame_between_frame_boundaries`
    /// span for ever waiting on a number nothing was incrementing.
    fn advance(
        &mut self,
        granularity: rf_core_api::Step,
        sink: &mut dyn CoreSink,
    ) -> rf_core_api::StepResult {
        let result = self.machine.as_core().step(granularity, sink);
        if result.frame_complete {
            self.frames += 1;
        }
        result
    }

    /// Called once per UI repaint. If [`RunState::Running`], advances
    /// exactly one frame (streaming scanlines to `sink`) and returns
    /// `true` if a whole frame actually completed (the ordinary case);
    /// returns `false` if [`RunState::Paused`] (nothing to do) or if
    /// [`CYCLE_BUDGET`]'s defensive bound fired before a frame boundary
    /// was reached (module doc) — either way, `false` means `sink` may
    /// hold a partial/no frame and the caller should not treat it as a
    /// fresh one to paint. Unlike [`Self::step_frame`], this does not
    /// change `state` — running stays running even if the budget fired.
    pub fn tick_running(&mut self, sink: &mut dyn CoreSink) -> bool {
        if self.state != RunState::Running {
            return false;
        }
        self.run_until_next_frame(sink) > 0
    }

    /// Set both controller ports' live button bytes from `frame` — the
    /// host-side per-frame latch hook (FR-FE-003), forwarding to
    /// `NesBus::set_controller_buttons`. Only the low 8 bits of each port
    /// are meaningful (a real NES pad has 8 buttons); `InputFrame` carries
    /// `u16`/4 ports for future consoles, so this truncates and only reads
    /// ports 0-1. Private: every real caller goes through
    /// [`Self::latch_and_advance_frame`], never this alone (module doc's
    /// "one shared latch-then-advance path").
    fn latch_input(&mut self, frame: InputFrame) {
        self.machine.set_controller(0, frame.ports[0]);
        self.machine.set_controller(1, frame.ports[1]);
    }

    /// THE one shared latch-then-advance path (module doc, ticket W1-07):
    /// latch `frame` into both controller ports, then run until the next
    /// frame boundary (like [`Self::step_frame`], but does not touch
    /// `state` — callers that need the FR-DBG-004 "stepping always leaves
    /// the machine paused" behavior call [`Self::pause`] themselves).
    /// Returns the number of frames completed (0 or 1, see
    /// `run_until_next_frame`'s doc).
    pub fn latch_and_advance_frame(&mut self, frame: InputFrame, sink: &mut dyn CoreSink) -> u64 {
        self.latch_input(frame);
        self.run_until_next_frame(sink)
    }

    /// Live-gameplay entry to the shared latch-then-advance path: behaves
    /// exactly like [`Self::tick_running`] (a no-op returning `false`
    /// unless [`RunState::Running`], never touching `state`), but latches
    /// `frame` first via [`Self::latch_and_advance_frame`] — used by
    /// `crate::core_thread`'s live wiring.
    pub fn tick_running_with_input(&mut self, frame: InputFrame, sink: &mut dyn CoreSink) -> bool {
        if self.state != RunState::Running {
            return false;
        }
        self.latch_and_advance_frame(frame, sink) > 0
    }

    /// The **full-machine** state hash (ticket W2-04): SHA-256 over every
    /// save-state region, in [`rf_nes::StateRegion::ALL`] order — CPU
    /// registers and interrupt latches, bus counters, the entire PPU
    /// (including VRAM, palette RAM, OAM and the loopy registers), the
    /// entire APU, WRAM, mapper registers and battery PRG-RAM.
    ///
    /// **This replaced a deliberately partial hash, and the replacement is
    /// the point.** Until this ticket `rf-nes` had no state serialization,
    /// so the hash could only reach WRAM, OAM, PRG-RAM, five CPU registers
    /// and two counters; PPU and APU internals were structurally invisible,
    /// which meant a PPU-internal divergence that happened to render
    /// identically went unseen. W2-04's own HANDOFF note (plan.json) called
    /// that out as blocking half of ROADMAP's Phase-1 determinism exit
    /// criterion, "identical per-frame state hashes", which a hash that
    /// cannot see the PPU could not satisfy at any frame count.
    ///
    /// Callers that record this value in a `.rfreplay` must write
    /// `hash_kind=full-v2` ([`crate::save_state::HASH_KIND`]), NOT the old
    /// `reachable-v1` — SAVE_STATES.md §3 requires the field to change
    /// rather than be silently redefined.
    ///
    /// Deliberately still excluded: the rendered framebuffer (output, not
    /// state — the test suite digests it separately and never folds it in
    /// here) and the PPU's undrained scanline/event queues, which are the
    /// same output and which the core refuses to serialize mid-frame at all.
    ///
    /// # Panics
    /// Panics only if the core refuses to serialize, which for an
    /// in-memory writer means the frame-boundary rule was violated —
    /// hashing mid-frame with undrained scanlines. Every caller in this
    /// crate hashes at a frame boundary.
    #[must_use]
    pub fn state_hash(&self) -> String {
        crate::hash::sha256_hex(&self.full_state_bytes())
    }

    /// The bytes [`Self::state_hash`] digests: every region's payload,
    /// concatenated in [`rf_nes::StateRegion::ALL`] order. Exposed because
    /// a divergence report wants the bytes, not just the digest.
    ///
    /// # Panics
    /// See [`Self::state_hash`].
    #[must_use]
    pub fn full_state_bytes(&self) -> Vec<u8> {
        // NES-only: `rf_nes::StateRegion` is a NES concept, and SNES
        // state has its own container (W8-02). An empty vector on a SNES
        // session is honest — this hash is used to compare NES runs —
        // and a panic here would take down a session for a diagnostic.
        let (Some(bus), Some(cpu)) = (self.machine.nes_bus(), self.machine.nes_cpu()) else {
            return Vec::new();
        };
        let mut buf = StateBuf::default();
        for region in rf_nes::StateRegion::ALL {
            bus.save_region(cpu, region, &mut buf).unwrap_or_else(|e| {
                panic!("state_hash: core refused to serialize {region:?}: {e}")
            });
        }
        buf.bytes
    }

    /// Shared by `step_frame`/`tick_running`: run instructions, draining
    /// video after each one, until `NesBus::frame_count` has advanced by
    /// exactly one, or [`CYCLE_BUDGET`]'s defensive bound elapses first
    /// (module doc). Never touches `self.state`. Returns the number of
    /// frames completed (0 or 1 — see the two public callers' docs).
    /// [`Self::latch_and_advance_frame`], but calling `on_instruction`
    /// with a formatted nestest-style line before every instruction
    /// (ticket W4-10a; DEBUGGER.md §2).
    ///
    /// **The formatter is `rf_nes::trace::format_trace_line`, and that is
    /// the requirement rather than a convenience.** DEBUGGER.md §2: "the
    /// golden-trace diff in CI and the on-screen trace viewer share one
    /// formatter." A second formatter would be a second thing to keep
    /// correct, and only one of the two would be under test — the CI diff
    /// compares this exact function's output byte-for-byte against
    /// `nestest.log`.
    ///
    /// The peek is non-perturbing: `format_trace_line` takes `&Cpu` and a
    /// `&dyn TracePeek`, and `NesBus`'s peek path is the one that
    /// deliberately avoids `$2002`/`$2007`'s read side effects — so
    /// tracing cannot change what it traces.
    ///
    /// This is a **separate loop** from the untraced path on purpose: the
    /// shipped run loop pays nothing for tracing existing, not even a
    /// per-instruction branch, which is what DEBUGGER.md §6's pay-for-use
    /// rule asks for and what `benches/debugger_idle.rs` measures.
    pub fn latch_and_advance_frame_traced(
        &mut self,
        frame: InputFrame,
        sink: &mut dyn CoreSink,
        on_instruction: &mut dyn FnMut(u16, u64, String),
    ) -> u64 {
        self.latch_input(frame);
        let start = self.frames;
        let deadline = self
            .machine
            .nes_bus()
            .map_or(0, rf_nes::NesBus::master_cycle)
            + self.cycle_budget;
        // **LAW 8, and this loop had no working guard for a SNES session
        // until ticket W13-02g.** Every exit below used to read through
        // `nes_bus()`, which is `None` on SNES: the deadline compared 0
        // against a budget (always true), and the frame check compared 0
        // against `self.frames` (never true on a fresh stepper). Both
        // exits were dead and the loop ran forever — a latent hang that
        // nothing reached only because nothing had traced a SNES session
        // yet. This counter advances on EVERY path through the body,
        // whatever the console, which is what law 8 asks for.
        let mut instructions = 0u64;
        let mut counting = CountingSink {
            inner: sink,
            scanlines: 0,
            last_scanline: self.last_scanline,
        };
        let mut advanced = 0;
        // Ticket W11-10: `Step::Instruction` through the trait, one at a
        // time, because the trace callback has to run BEFORE each
        // instruction executes — that is what makes the line a record of
        // what was about to happen rather than of what already had.
        while self
            .machine
            .nes_bus()
            .map_or(0, rf_nes::NesBus::master_cycle)
            < deadline
            && instructions < TRACED_INSTRUCTION_BUDGET
        {
            instructions += 1;
            let pc = self.machine.nes_cpu().map_or(0, |c| c.pc);
            let cycle = self
                .machine
                .nes_bus()
                .map_or(0, rf_nes::NesBus::master_cycle);
            // Ticket W13-02g: each console's own formatter, so the
            // viewer and any golden diff read the same text. The NES line
            // is nestest-shaped; the SNES line follows bsnes conventions
            // (bank:addr, m/x-aware) — DEBUGGER.md §2.
            let line = match (self.machine.nes_cpu(), self.machine.nes_bus()) {
                (Some(cpu), Some(bus)) => rf_nes::trace::format_trace_line(cpu, bus, cycle),
                _ => self.snes_trace_line(),
            };
            on_instruction(pc, cycle, line);
            self.advance(rf_core_api::Step::Instruction, &mut counting);
            // `self.frames` rather than the NES bus's own counter: it is
            // incremented by `advance` from `StepResult::frame_complete`,
            // so it is the one frame count that exists for both consoles.
            // (The two were also different counters that merely both
            // started at zero — comparing one against `start`, taken from
            // the other, was correct only by coincidence.)
            if self.frames != start {
                advanced = self.frames - start;
                break;
            }
        }
        self.last_scanline = counting.last_scanline;
        advanced
    }

    fn run_until_next_frame(&mut self, sink: &mut dyn CoreSink) -> u64 {
        let _start = self.frames;
        // Wrapped so the position readout keeps updating while running,
        // not only when single-stepping (ticket W2-15) — a readout that
        // froze during Run would be worse than none.
        let mut counting = CountingSink {
            inner: sink,
            scanlines: 0,
            last_scanline: self.last_scanline,
        };
        // Ticket W11-10: one call through the trait. The loop, its
        // deadline, and the per-instruction video/audio drains are the
        // core's business now — `rf_nes::core::NesCore::step` — and the
        // shell keeps only what it owns: the scanline readout and the
        // frame count.
        let result = self.advance(rf_core_api::Step::Frame, &mut counting);
        let advanced = if result.frame_complete {
            // One `Step::Frame` is one frame, by the trait's own
            // definition — no console counter consulted. `advance` did
            // the counting.
            1
        } else {
            // The cycle budget fired: a machine that cannot reach a frame
            // boundary (a JAM opcode) reports zero rather than spinning.
            0
        };
        self.last_scanline = counting.last_scanline;
        advanced
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::PpuPixel;

    /// Minimal synthetic NROM iNES image (mapper 0), same layout
    /// `crates/rf-nes/src/system/tests/mod.rs::build_nrom_ines` uses --
    /// that helper is `pub(super)` (test-only, private to `rf-nes`'s own
    /// test tree) so it isn't reachable from here; this crate is outside
    /// `rf-nes`'s write scope, so the ~10 lines are duplicated rather than
    /// widening that crate's public API for a test fixture. All-zero
    /// PRG/CHR means the reset and IRQ/BRK vectors all resolve to
    /// `$0000`, which is zeroed RAM (opcode `$00` = `BRK`) -- a
    /// deterministic, infinite, 7-cycles-per-instruction loop that never
    /// needs real game code to drive stepping.
    fn synthetic_nrom() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
        data.push(1); // 1x16KiB PRG
        data.push(1); // 1x8KiB CHR
        data.extend_from_slice(&[0u8; 10]); // flags6/7 + 8 reserved => mapper 0, iNES 1.0
        data.extend(vec![0u8; 16 * 1024]);
        data.extend(vec![0u8; 8 * 1024]);
        data
    }

    struct NullSink;
    impl CoreSink for NullSink {
        fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
        fn audio(&mut self, _samples: &[i16]) {}
        fn event(&mut self, _ev: rf_core_api::CoreEvent) {}
    }

    fn stepper() -> EmuStepper {
        EmuStepper::from_ines_bytes(&synthetic_nrom()).expect("valid synthetic NROM image")
    }

    /// Ticket W23-01: an SNES gets its controller — the word reaches the
    /// joypad port, and once the game's auto-joypad read runs, `$4218`'s
    /// latched word (what the game reads) carries it too.
    #[test]
    fn snes_controller_reaches_auto_joypad() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc");
        let raw = std::fs::read(&path).expect("SNES fixture");
        let mut st = EmuStepper::from_snes_bytes(&raw).expect("opens");
        let y = 1u16 << rf_input::SnesButton::Y.bit();
        let mut frame = InputFrame::empty();
        frame.ports[0] = y;
        let mut sink = NullSink;
        for _ in 0..30 {
            st.latch_and_advance_frame(frame, &mut sink);
        }
        let Machine::Snes(c) = &mut st.machine else {
            panic!("an SNES machine");
        };
        let pads = &c.system_mut().bus.joypads;
        assert_eq!(pads.ports[0], y, "the word reached the port");
        assert_eq!(pads.latched[0], y, "auto-joypad latched it for $4218");
    }

    #[test]
    fn starts_paused() {
        assert!(stepper().is_paused());
    }

    #[test]
    fn resume_then_pause_round_trip() {
        let mut s = stepper();
        s.resume();
        assert_eq!(s.state(), RunState::Running);
        assert!(!s.is_paused());
        s.pause();
        assert_eq!(s.state(), RunState::Paused);
        assert!(s.is_paused());
    }

    #[test]
    fn resume_is_idempotent() {
        let mut s = stepper();
        s.resume();
        s.resume();
        assert_eq!(s.state(), RunState::Running);
    }

    #[test]
    fn step_frame_advances_exactly_one_frame_and_ends_paused() {
        let mut s = stepper();
        let mut sink = NullSink;
        let before = s.frame_count();
        let advanced = s.step_frame(&mut sink);
        assert_eq!(advanced, 1, "step_frame must report exactly one frame");
        assert_eq!(
            s.frame_count(),
            before + 1,
            "frame_count must have advanced by exactly one"
        );
        assert!(s.is_paused(), "stepping always leaves the machine paused");
    }

    #[test]
    fn step_frame_from_running_still_stops_after_exactly_one_frame() {
        let mut s = stepper();
        s.resume();
        let mut sink = NullSink;
        let before = s.frame_count();
        s.step_frame(&mut sink);
        assert_eq!(
            s.frame_count(),
            before + 1,
            "must not run past one frame even though state was Running when step_frame was called"
        );
        assert!(
            s.is_paused(),
            "step_frame forces Paused regardless of prior state"
        );
    }

    #[test]
    fn two_consecutive_step_frames_advance_two_separate_frames() {
        let mut s = stepper();
        let mut sink = NullSink;
        let before = s.frame_count();
        s.step_frame(&mut sink);
        s.step_frame(&mut sink);
        assert_eq!(s.frame_count(), before + 2);
    }

    #[test]
    fn step_scanline_advances_exactly_one_scanline_and_stops() {
        let mut s = stepper();
        let mut sink = NullSink;
        let n = s.step_scanline(&mut sink);
        assert_eq!(
            n, 1,
            "an ordinary (non-DMA) instruction step must not cross more than one scanline boundary"
        );
        assert!(s.is_paused());
    }

    /// The defensive cycle-budget bound (module doc's "Why every loop in
    /// here is cycle-bounded") must actually terminate a loop that would
    /// otherwise run forever, not just exist unreachably in the source. A
    /// single scanline needs ~114 CPU cycles (341 dots / 3); a budget of
    /// 10 guarantees `step_scanline` cannot possibly reach one before the
    /// bound fires. If this test hangs (rather than completing), the
    /// bound isn't wired into the loop condition.
    #[test]
    fn step_scanline_cycle_budget_terminates_before_a_scanline_completes() {
        let mut s = stepper();
        s.set_cycle_budget_for_test(10);
        let mut sink = NullSink;
        let n = s.step_scanline(&mut sink);
        assert_eq!(
            n, 0,
            "a 10-cycle budget cannot reach a scanline boundary (~114 cycles) — must return 0, not hang"
        );
        assert!(
            s.is_paused(),
            "the budget firing must still leave the machine Paused, not Running"
        );
    }

    /// Same guarantee for `step_frame`'s shared `run_until_next_frame`
    /// path. Budget chosen as 50 (< ~114 cycles), not some large-but-still
    /// -short-of-a-frame number: a fresh `EmuStepper` starts mid-way
    /// through the pre-render scanline (see the
    /// `step_scanline_calls_accumulate_a_full_visible_frame_between_frame_boundaries`
    /// test's doc for why), so the very *first* frame boundary is only
    /// ~114 cycles away, not the ~29,781 a steady-state frame needs — a
    /// budget merely "far short of 29,781" could still accidentally clear
    /// that first, short boundary and pass for the wrong reason.
    #[test]
    fn step_frame_cycle_budget_terminates_before_a_frame_completes() {
        let mut s = stepper();
        s.set_cycle_budget_for_test(50);
        let mut sink = NullSink;
        let before = s.frame_count();
        let advanced = s.step_frame(&mut sink);
        assert_eq!(
            advanced, 0,
            "a 50-cycle budget cannot reach even the nearest possible frame boundary (~114 cycles) — must return 0, not hang"
        );
        assert_eq!(s.frame_count(), before, "no frame actually completed");
        assert!(s.is_paused());
    }

    /// `tick_running` must inherit the same bound (it shares
    /// `run_until_next_frame`) — a stuck Running machine must not hang the
    /// caller either. Same budget-choice reasoning as the `step_frame`
    /// test above.
    #[test]
    fn tick_running_cycle_budget_terminates_before_a_frame_completes() {
        let mut s = stepper();
        s.set_cycle_budget_for_test(50);
        s.resume();
        let mut sink = NullSink;
        let before = s.frame_count();
        let produced = s.tick_running(&mut sink);
        assert!(
            !produced,
            "tick_running must report false when the budget fires before a frame completes \
             (the caller must not treat a partial frame as fresh)"
        );
        assert_eq!(
            s.frame_count(),
            before,
            "no frame actually completed under the tiny budget"
        );
        assert_eq!(
            s.state(),
            RunState::Running,
            "unlike step_frame, tick_running must not force-pause even when the budget fires"
        );
    }

    #[test]
    fn step_scanline_calls_accumulate_a_full_visible_frame_between_frame_boundaries() {
        // `Ppu::new` starts mid-way through the pre-render scanline
        // (`crate::ppu`'s `scanline: PRERENDER_SCANLINE` initial value —
        // see that module doc's dot diagram), so the very *first*
        // `frame_count` tick (0 -> 1) is a boot-state artifact: it fires
        // after only that partial initial sweep, with zero *visible*
        // scanlines drained yet (only visible lines 0-239 call
        // `CoreSink::video_scanline`, via `finish_scanline` in
        // `crate::ppu::background` — pre-render/post-render/vblank lines
        // never do). Skip past that one artifact transition so this test
        // checks a steady-state frame instead.
        let mut s = stepper();
        let mut sink = NullSink;

        let boot_frame = s.frame_count();
        loop {
            s.step_scanline(&mut sink);
            if s.frame_count() != boot_frame {
                break;
            }
        }

        let before = s.frame_count();
        let mut total_scanlines = 0u32;
        for _ in 0..300 {
            total_scanlines += s.step_scanline(&mut sink);
            if s.frame_count() != before {
                break;
            }
        }
        assert_eq!(
            s.frame_count(),
            before + 1,
            "one steady-state frame must complete within 300 scanline-steps"
        );
        assert_eq!(
            total_scanlines, 240,
            "a steady-state NTSC frame must drain exactly the 240 visible scanlines, got {total_scanlines}"
        );
    }

    #[test]
    fn tick_running_is_a_noop_while_paused() {
        let mut s = stepper();
        let mut sink = NullSink;
        let before = s.frame_count();
        let produced = s.tick_running(&mut sink);
        assert!(!produced, "tick_running must do nothing while Paused");
        assert_eq!(s.frame_count(), before);
        assert!(s.is_paused());
    }

    #[test]
    fn tick_running_advances_one_frame_per_call_while_running_and_stays_running() {
        let mut s = stepper();
        s.resume();
        let mut sink = NullSink;
        let before = s.frame_count();
        let produced = s.tick_running(&mut sink);
        assert!(
            produced,
            "tick_running must report a completed frame while Running"
        );
        assert_eq!(s.frame_count(), before + 1);
        assert_eq!(
            s.state(),
            RunState::Running,
            "tick_running must not force-pause, unlike step_frame/step_scanline"
        );
    }

    #[test]
    fn latch_and_advance_frame_advances_and_does_not_touch_state() {
        let mut s = stepper();
        let mut sink = NullSink;
        let before = s.frame_count();
        s.latch_and_advance_frame(InputFrame::empty(), &mut sink);
        assert_eq!(s.frame_count(), before + 1);
        assert!(
            s.is_paused(),
            "state started Paused and latch_and_advance_frame must not change it either way"
        );

        s.resume();
        let before = s.frame_count();
        s.latch_and_advance_frame(InputFrame::empty(), &mut sink);
        assert_eq!(s.frame_count(), before + 1);
        assert_eq!(
            s.state(),
            RunState::Running,
            "latch_and_advance_frame must not force-pause, unlike step_frame"
        );
    }

    #[test]
    fn tick_running_with_input_is_a_noop_while_paused() {
        let mut s = stepper();
        let mut sink = NullSink;
        let before = s.frame_count();
        let produced = s.tick_running_with_input(InputFrame::empty(), &mut sink);
        assert!(!produced);
        assert_eq!(s.frame_count(), before);
    }

    #[test]
    fn tick_running_with_input_advances_while_running() {
        let mut s = stepper();
        s.resume();
        let mut sink = NullSink;
        let before = s.frame_count();
        let produced = s.tick_running_with_input(InputFrame::empty(), &mut sink);
        assert!(produced);
        assert_eq!(s.frame_count(), before + 1);
    }

    #[test]
    fn state_hash_is_stable_across_repeated_calls_with_no_intervening_advance() {
        let s = stepper();
        assert_eq!(
            s.state_hash(),
            s.state_hash(),
            "hashing must be a pure read, not itself mutate state"
        );
    }

    #[test]
    fn state_hash_changes_after_advancing() {
        let mut s = stepper();
        let mut sink = NullSink;
        let before = s.state_hash();
        s.step_frame(&mut sink);
        assert_ne!(
            before,
            s.state_hash(),
            "frame_count alone (part of the hashed state) must change after a frame"
        );
    }

    /// Ticket W2-15: the readout must actually advance when you step a
    /// scanline — this is the assertion that replaces "take it on faith",
    /// which is how a working Step Scanline came to look like a dead
    /// button.
    #[test]
    fn step_scanline_advances_the_reported_scanline() {
        let mut s = stepper();
        let mut sink = NullSink;
        // Get past the boot-artifact partial frame (see the 240-scanline
        // test's doc) so we are stepping inside a steady-state frame.
        let boot = s.frame_count();
        while s.frame_count() == boot {
            s.step_scanline(&mut sink);
        }
        s.step_scanline(&mut sink);
        let first = s
            .last_scanline()
            .expect("a visible scanline has been drawn");
        s.step_scanline(&mut sink);
        let second = s.last_scanline().expect("still drawing visible scanlines");
        assert_eq!(
            second,
            first + 1,
            "each Step Scanline must advance the reported line by exactly one \
             (got {first} then {second})"
        );
    }

    #[test]
    fn a_fresh_stepper_reports_no_scanline_yet() {
        assert_eq!(
            stepper().last_scanline(),
            None,
            "before any visible line is drawn the readout must say so, not invent a 0"
        );
    }

    /// The readout must keep moving while *running*, not only while
    /// single-stepping — a position display that froze during Run would
    /// be worse than none.
    ///
    /// Note the deliberate *second* `tick_running`: a fresh `EmuStepper`
    /// starts mid pre-render scanline, so its very first `frame_count`
    /// tick is the boot-state artifact this module's other tests already
    /// document — it completes with **zero visible scanlines drawn**, so
    /// `last_scanline` is legitimately still `None` at that point. The
    /// first draft of this test asserted after one tick and failed for
    /// exactly that reason: the test was naive, the readout was right.
    #[test]
    fn running_keeps_the_reported_scanline_updating() {
        let mut s = stepper();
        s.resume();
        let mut sink = NullSink;
        s.tick_running(&mut sink); // boot-artifact frame: no visible lines
        s.tick_running(&mut sink); // first steady-state frame
        assert_eq!(
            s.last_scanline(),
            Some(239),
            "a steady-state frame while Running must leave the readout on the \
             last visible line (239), not frozen at None"
        );
    }

    #[test]
    fn peek_reads_wram_written_by_a_previous_write() {
        // The all-zero synthetic NROM never writes WRAM itself (BRK-forever
        // from a zeroed reset vector), so freshly-peeked WRAM is 0 — this
        // just proves `EmuStepper::peek` reaches through to `NesBus::peek`
        // without panicking, matching that method's own side-effect-free
        // contract.
        let s = stepper();
        assert_eq!(s.peek(0x0000), 0);
        assert_eq!(s.peek(0x07FF), 0);
    }

    /// Records every `CoreEvent` seen, for [`set_event_mask`]'s tests below
    /// — `NullSink` above deliberately discards events, and nothing else in
    /// this test module already collects them.
    #[derive(Default)]
    struct RecordingSink {
        events: Vec<rf_core_api::CoreEvent>,
    }
    impl CoreSink for RecordingSink {
        fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
        fn audio(&mut self, _samples: &[i16]) {}
        fn event(&mut self, ev: rf_core_api::CoreEvent) {
            self.events.push(ev);
        }
    }

    /// Ticket W4-06a: `set_event_mask` must NOT be able to drop
    /// [`CAMERA_BASELINE_EVENT_MASK`] even when the caller asks for
    /// `EventMask::NONE` — otherwise a debugger session that ever narrows
    /// the mask (e.g. closing the event-viewer panel) would silently starve
    /// `crate::canvas_accum`'s scene tracking for the rest of the session.
    /// `Scanline` is code-independent (driven by PPU tick logic, not game
    /// code — unlike `ScrollWrite`, which the all-zero synthetic NROM never
    /// triggers), so a full stepped frame is guaranteed to emit at least
    /// one if the baseline actually survived.
    #[test]
    fn set_event_mask_cannot_drop_the_camera_baseline_even_when_asked_to() {
        let mut s = stepper();
        s.set_event_mask(rf_core_api::EventMask::NONE);
        // Get past the boot-artifact partial frame (see
        // `running_keeps_the_reported_scanline_updating`'s doc: a fresh
        // stepper's very first frame_count tick completes with zero
        // visible scanlines drawn, hence zero Scanline events too).
        s.step_frame(&mut NullSink);
        let mut sink = RecordingSink::default();
        s.step_frame(&mut sink);
        assert!(
            sink.events
                .iter()
                .any(|e| matches!(e, rf_core_api::CoreEvent::Scanline(_))),
            "Scanline events must survive an EventMask::NONE request: {:?}",
            sink.events
        );
    }

    /// The other half: `set_event_mask` must actually narrow subscriptions,
    /// not silently behave as `EventMask::ALL` regardless of what's asked
    /// for — otherwise the mask parameter would be decorative. `VblankStart`
    /// is outside `CAMERA_BASELINE_EVENT_MASK` and, like `Scanline`, is
    /// code-independent (fires every frame at a fixed scanline regardless
    /// of ROM content), so it is a clean discriminator: present only when
    /// explicitly subscribed.
    #[test]
    fn set_event_mask_actually_narrows_which_events_get_constructed() {
        let mut s = stepper();
        s.set_event_mask(rf_core_api::EventMask::NONE);
        s.step_frame(&mut NullSink); // past the boot-artifact partial frame
        let mut narrow_sink = RecordingSink::default();
        s.step_frame(&mut narrow_sink);
        assert!(
            !narrow_sink
                .events
                .iter()
                .any(|e| matches!(e, rf_core_api::CoreEvent::VblankStart)),
            "VblankStart must NOT appear when not subscribed: {:?}",
            narrow_sink.events
        );

        s.set_event_mask(rf_core_api::EventMask::VBLANK_START);
        let mut wide_sink = RecordingSink::default();
        s.step_frame(&mut wide_sink);
        assert!(
            wide_sink
                .events
                .iter()
                .any(|e| matches!(e, rf_core_api::CoreEvent::VblankStart)),
            "VblankStart must appear once explicitly subscribed: {:?}",
            wide_sink.events
        );
    }
}

/// In-memory `StateWriter` for [`EmuStepper::full_state_bytes`].
#[derive(Default)]
struct StateBuf {
    bytes: Vec<u8>,
}

impl rf_core_api::StateWriter for StateBuf {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), rf_core_api::StateError> {
        self.bytes.extend_from_slice(buf);
        Ok(())
    }
}

/// Parses 64 lowercase hex chars into 32 bytes, leaving `out` zeroed if the
/// input is malformed — `rf_cart::identity_nes` always produces well-formed
/// hex, so this cannot silently truncate a real hash; the fallback exists so
/// a hash helper never panics inside a ROM-open path.
fn hex_to_bytes(hex: &str, out: &mut [u8; 32]) {
    if hex.len() != 64 {
        return;
    }
    for (index, slot) in out.iter_mut().enumerate() {
        match u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16) {
            Ok(byte) => *slot = byte,
            Err(_) => {
                *out = [0u8; 32];
                return;
            }
        }
    }
}

impl EmuStepper {
    /// The text a blargg shell printed to the nametable, ASCII-decoded —
    /// for the older ROMs whose only output is the screen (ticket W2-05;
    /// same reader `rf-nes`'s `dmc_dma_during_read4` suite uses).
    #[must_use]
    pub fn screen_text_for_test(&self) -> String {
        self.machine
            .nes_bus()
            .map(rf_nes::NesBus::ppu_vram_ascii)
            .unwrap_or_default()
    }
}

/// Execution control built on [`EmuStepper::step_instruction`] (ticket
/// W4-06e, route (a) — see `rf_debugger::breakpoint`'s module doc).
///
/// Free functions rather than `EmuStepper` methods so the stepper keeps
/// knowing nothing about the debugger, and so each is testable on its own
/// against a real machine.
pub mod exec_control {
    use rf_core_api::CoreSink;
    use rf_debugger::breakpoint::{
        frame_has_returned, step_over_is_a_call, BreakCtx, BreakpointTable, Hit,
    };

    use super::EmuStepper;

    /// Why a run stopped.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum StopReason {
        /// The requested step completed.
        Completed,
        /// A breakpoint fired first — breakpoints outrank the step
        /// request, or "run to cursor" past a breakpoint would silently
        /// skip it.
        Breakpoint(Hit),
        /// The instruction budget ran out. Every loop here is bounded for
        /// the same reason `EmuStepper`'s are: a step-out inside a routine
        /// that never returns must stop, not hang the UI.
        BudgetExhausted,
    }

    /// Defensive bound for the multi-instruction controls. Generous
    /// (a frame is ~10k instructions) but finite.
    pub const INSTRUCTION_BUDGET: u32 = 2_000_000;

    /// Check armed breakpoints at the current boundary.
    fn check(
        stepper: &EmuStepper,
        table: &BreakpointTable,
        events: &[rf_debugger::breakpoint::EventKind],
        watch_hits: &[u32],
    ) -> Option<Hit> {
        if !table.armed() {
            // The one-branch fast path — see BreakpointTable::armed.
            return None;
        }
        let peek = |addr: u16| stepper.peek(addr);
        table.check(&BreakCtx {
            pc: stepper.pc(),
            scanline: stepper.last_scanline().unwrap_or(0),
            dot: 0,
            peek: &peek,
            events,
            watch_hits,
        })
    }

    /// One instruction, then stop.
    pub fn step_into(
        stepper: &mut EmuStepper,
        sink: &mut dyn CoreSink,
        table: &BreakpointTable,
    ) -> StopReason {
        stepper.step_instruction(sink);
        match check(stepper, table, &[], &[]) {
            Some(hit) => StopReason::Breakpoint(hit),
            None => StopReason::Completed,
        }
    }

    /// One instruction, but run a `JSR`'s whole subroutine to completion.
    ///
    /// Only `JSR` is treated as a call — see
    /// `rf_debugger::breakpoint::step_over_is_a_call` for why `JMP` must
    /// not be.
    pub fn step_over(
        stepper: &mut EmuStepper,
        sink: &mut dyn CoreSink,
        table: &BreakpointTable,
    ) -> StopReason {
        let opcode = stepper.peek(stepper.pc());
        if !step_over_is_a_call(opcode) {
            return step_into(stepper, sink, table);
        }
        // Enter the call, THEN step out of it. The entry SP must be
        // sampled from INSIDE the subroutine: `RTS` restores the stack
        // pointer to exactly its pre-`JSR` value, so a predicate anchored
        // before the call waits for `S` to rise strictly above a value it
        // only ever returns to — and never fires. (Found by
        // `step_over_runs_the_subroutine_and_lands_after_the_call`
        // exhausting its budget; only a real machine shows this.)
        stepper.step_instruction(sink); // the JSR itself
        let inside_sp = stepper.sp();
        run_until(stepper, sink, table, |s| {
            frame_has_returned(inside_sp, s.sp())
        })
    }

    /// Run until the current subroutine returns.
    pub fn step_out(
        stepper: &mut EmuStepper,
        sink: &mut dyn CoreSink,
        table: &BreakpointTable,
    ) -> StopReason {
        let entry_sp = stepper.sp();
        run_until(stepper, sink, table, |s| {
            frame_has_returned(entry_sp, s.sp())
        })
    }

    /// Run until the PC reaches `target`.
    pub fn run_to_cursor(
        stepper: &mut EmuStepper,
        sink: &mut dyn CoreSink,
        table: &BreakpointTable,
        target: u16,
    ) -> StopReason {
        run_until(stepper, sink, table, |s| s.pc() == target)
    }

    /// Shared loop: step until `done`, a breakpoint fires, or the budget
    /// runs out. Breakpoints are checked BEFORE `done` so a breakpoint
    /// inside a subroutine being stepped over is not silently skipped.
    fn run_until(
        stepper: &mut EmuStepper,
        sink: &mut dyn CoreSink,
        table: &BreakpointTable,
        done: impl Fn(&EmuStepper) -> bool,
    ) -> StopReason {
        for _ in 0..INSTRUCTION_BUDGET {
            stepper.step_instruction(sink);
            if let Some(hit) = check(stepper, table, &[], &[]) {
                return StopReason::Breakpoint(hit);
            }
            if done(stepper) {
                return StopReason::Completed;
            }
        }
        StopReason::BudgetExhausted
    }
}

#[cfg(test)]
mod save_load_determinism {
    //! Ticket W2-22: at every frame boundary, Save -> StepFrame must equal
    //! Load(that save) -> StepFrame — the pixels of the next frame and the
    //! whole machine state after it. This is the property Load State and
    //! rewind rest on, checked frame by frame over every NES fixture that
    //! is built (a missing fixture is skipped, not failed).
    use super::EmuStepper;
    use rf_core_api::{CoreSink, PpuPixel};

    /// FNV-1a over every pixel of a frame, scanline number included.
    #[derive(Default)]
    struct FrameHash(u64);
    impl CoreSink for FrameHash {
        fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
            for p in pixels {
                let layer = match p.layer {
                    rf_core_api::PixelLayer::Backdrop => 0,
                    rf_core_api::PixelLayer::Background(n) => 1 + u64::from(n),
                    rf_core_api::PixelLayer::Sprite => 9,
                };
                for v in [
                    u64::from(y),
                    u64::from(p.palette_index),
                    layer,
                    u64::from(p.priority),
                ] {
                    self.0 = (self.0 ^ v).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        fn audio(&mut self, _samples: &[i16]) {}
        fn event(&mut self, _ev: rf_core_api::CoreEvent) {}
    }

    const FIXTURES: [&str; 2] = [
        "../../fixtures/nes/rf-scroller/build/rf-scroller.nes",
        "../../fixtures/nes/action53/build/action53.nes",
    ];

    #[test]
    fn save_load_step_reproduces_every_frame_on_nes_fixtures() {
        let mut checked = 0;
        for rel in FIXTURES {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
            let Ok(rom) = std::fs::read(&path) else {
                eprintln!("SKIP: {rel} not built");
                continue;
            };
            let mut s = EmuStepper::from_ines_bytes(&rom).expect("fixture loads");
            let mut bad = Vec::new();
            // A fixed trip count: terminates by construction (law 8).
            for frame in 0..120u64 {
                let saved = s.save_state(0).expect("frame-boundary save");
                let before = s.full_state_bytes();
                let mut played = FrameHash::default();
                s.step_frame(&mut played);
                let after_played = s.full_state_bytes();
                s.load_state(&saved).expect("load own save");
                let restored = s.full_state_bytes();
                let mut replayed = FrameHash::default();
                s.step_frame(&mut replayed);
                let after_replayed = s.full_state_bytes();
                if restored != before || played.0 != replayed.0 || after_played != after_replayed {
                    bad.push((frame, restored == before, played.0 == replayed.0));
                }
            }
            assert!(
                bad.is_empty(),
                "{rel}: (frame, state restored, picture reproduced) mismatches: {bad:?}"
            );
            checked += 1;
        }
        eprintln!("checked {checked} NES fixture(s)");
    }
}
