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
//! `hash_kind=full-v1` ([`crate::save_state::HASH_KIND`]); the old
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

    fn audio(&mut self, samples: &[i16]) {
        self.inner.audio(samples);
    }

    fn event(&mut self, ev: rf_core_api::CoreEvent) {
        self.inner.event(ev);
    }
}

/// Owns one running NES machine and the run/paused state a debugger UI
/// drives it through. See module doc.
pub struct EmuStepper {
    bus: NesBus,
    cpu: Cpu,
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
    /// # Errors
    /// Returns [`NesLoadError`] for anything `NesBus::from_ines_bytes`
    /// rejects (bad magic, unimplemented mapper, ...).
    pub fn from_ines_bytes(raw: &[u8]) -> Result<Self, NesLoadError> {
        let mut bus = NesBus::from_ines_bytes(raw)?;
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
        let cpu = Cpu::power_on(&mut bus);
        // Ticket W2-04: the NORMALIZED hash (header stripped), matching
        // `tests/rom-manifest.toml` and `.rfreplay`'s `rom_sha256`, so the
        // same cartridge dumped with a different header still matches its
        // own save states.
        let identity = rf_cart::hash::identity_nes(raw);
        let mut rom_sha256 = [0u8; 32];
        hex_to_bytes(&identity.normalized.sha256, &mut rom_sha256);
        Ok(EmuStepper {
            bus,
            cpu,
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
    pub fn bus_for_battery(&self) -> &NesBus {
        &self.bus
    }

    /// Mutable counterpart of [`Self::bus_for_battery`], for loading a
    /// `.sav` at startup.
    pub fn bus_for_battery_mut(&mut self) -> &mut NesBus {
        &mut self.bus
    }

    /// Borrowed CPU, for `crate::save_state`'s serializer.
    pub(crate) fn cpu_for_state(&self) -> &Cpu {
        &self.cpu
    }

    /// Borrowed bus, for `crate::save_state`'s serializer.
    pub(crate) fn bus_for_state(&self) -> &NesBus {
        &self.bus
    }

    /// Both halves mutably, for `crate::save_state`'s loader — one call
    /// rather than two accessors, so a caller cannot restore a CPU into a
    /// bus from a different state.
    pub(crate) fn machine_for_state(&mut self) -> (&mut Cpu, &mut NesBus) {
        (&mut self.cpu, &mut self.bus)
    }

    /// Test-only hook to prove [`CYCLE_BUDGET`]'s termination guarantee
    /// without waiting out the production budget — see module doc's "Why
    /// every loop in here is cycle-bounded".
    #[cfg(test)]
    fn set_cycle_budget_for_test(&mut self, budget: u64) {
        self.cycle_budget = budget;
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
        self.bus.frame_count()
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
        self.bus.peek(addr)
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
        use rf_nes::CpuBus;
        self.bus.write(addr, value);
    }

    /// The full 256-byte OAM as last written — forwards `NesBus::oam`
    /// (ticket W3-05a: the mode-invariant test suite's fixture-sanity
    /// check reads this to prove its sprite table actually landed).
    #[must_use]
    pub fn oam(&self) -> &[u8; 256] {
        self.bus.oam()
    }

    /// The PPU's nametable VRAM (ticket W4-06d) — forwards
    /// `NesBus::vram()`, a non-observing borrow.
    pub fn vram(&self) -> &[u8; 0x1000] {
        self.bus.vram()
    }

    /// The PPU's palette RAM (ticket W4-06d) — forwards
    /// `NesBus::palette()`, raw and unmirrored.
    pub fn palette(&self) -> &[u8; 32] {
        self.bus.palette()
    }

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
        self.bus.prg_ram()
    }

    /// Whether the sprite-limit-bypass overlay is currently recording
    /// (ticket W3-05a) — forwards `NesBus::sprite_overlay_enabled`. `false`
    /// on a freshly opened ROM (law 6: a fresh install boots in Accuracy
    /// Mode).
    #[must_use]
    pub fn sprite_overlay_enabled(&self) -> bool {
        self.bus.sprite_overlay_enabled()
    }

    /// Opt into (or out of) the sprite-limit-bypass overlay (ticket
    /// W3-05a) — forwards `NesBus::set_sprite_overlay_enabled`.
    pub fn set_sprite_overlay_enabled(&mut self, enabled: bool) {
        self.bus.set_sprite_overlay_enabled(enabled);
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
    pub fn set_event_mask(&mut self, mask: rf_core_api::EventMask) {
        self.bus
            .set_event_mask(mask.union(CAMERA_BASELINE_EVENT_MASK));
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
        let mut counting = CountingSink {
            inner: sink,
            scanlines: 0,
            last_scanline: self.last_scanline,
        };
        let cycles = self.cpu.step(&mut self.bus);
        self.bus.drain_video(&mut counting);
        self.bus.drain_audio(&mut counting);
        self.last_scanline = counting.last_scanline;
        self.state = RunState::Paused;
        cycles
    }

    /// The CPU's current program counter — the address the NEXT
    /// instruction will execute from, which is what a PC breakpoint and
    /// run-to-cursor both compare against.
    #[must_use]
    pub fn pc(&self) -> u16 {
        self.cpu.pc
    }

    /// The CPU's stack pointer, for step-over/step-out's frame tracking
    /// (`rf_debugger::breakpoint::frame_has_returned`).
    #[must_use]
    pub fn sp(&self) -> u8 {
        self.cpu.s
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
        let deadline = self.bus.master_cycle() + self.cycle_budget;
        let mut counting = CountingSink {
            inner: sink,
            scanlines: 0,
            last_scanline: self.last_scanline,
        };
        while self.bus.master_cycle() < deadline {
            self.cpu.step(&mut self.bus);
            self.bus.drain_video(&mut counting);
            // Ticket W2-05: audio drains on the same cadence as video —
            // per instruction, not per frame — so the ring is fed steadily
            // instead of in one ~800-sample burst at each frame boundary.
            // A burst is what makes a small ring underrun between frames
            // (`docs/design/FAILURE_MODES.md` FM-02); the drain costs one
            // branch when nothing is queued.
            self.bus.drain_audio(&mut counting);
            if counting.scanlines > 0 {
                break;
            }
        }
        let n = counting.scanlines;
        self.last_scanline = counting.last_scanline;
        self.state = RunState::Paused;
        n
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
        self.bus.set_controller_buttons(0, frame.ports[0] as u8);
        self.bus.set_controller_buttons(1, frame.ports[1] as u8);
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
    /// `hash_kind=full-v1` ([`crate::save_state::HASH_KIND`]), NOT the old
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
        let mut buf = StateBuf::default();
        for region in rf_nes::StateRegion::ALL {
            self.bus
                .save_region(&self.cpu, region, &mut buf)
                .unwrap_or_else(|e| {
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
    fn run_until_next_frame(&mut self, sink: &mut dyn CoreSink) -> u64 {
        let start = self.bus.frame_count();
        let deadline = self.bus.master_cycle() + self.cycle_budget;
        // Wrapped so the position readout keeps updating while running,
        // not only when single-stepping (ticket W2-15) — a readout that
        // froze during Run would be worse than none.
        let mut counting = CountingSink {
            inner: sink,
            scanlines: 0,
            last_scanline: self.last_scanline,
        };
        let mut advanced = 0;
        while self.bus.master_cycle() < deadline {
            self.cpu.step(&mut self.bus);
            self.bus.drain_video(&mut counting);
            self.bus.drain_audio(&mut counting);
            let now = self.bus.frame_count();
            if now != start {
                debug_assert_eq!(
                    now,
                    start + 1,
                    "a single Cpu::step must not be able to cross a whole frame boundary twice"
                );
                advanced = now - start;
                break;
            }
        }
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
        self.bus.ppu_vram_ascii()
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
        })
    }

    /// One instruction, then stop.
    pub fn step_into(
        stepper: &mut EmuStepper,
        sink: &mut dyn CoreSink,
        table: &BreakpointTable,
    ) -> StopReason {
        stepper.step_instruction(sink);
        match check(stepper, table, &[]) {
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
            if let Some(hit) = check(stepper, table, &[]) {
                return StopReason::Breakpoint(hit);
            }
            if done(stepper) {
                return StopReason::Completed;
            }
        }
        StopReason::BudgetExhausted
    }
}
