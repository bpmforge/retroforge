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
//! [`EmuStepper::state_hash`] is **not** a full-machine hash and must never
//! be described as one: `rf-nes` does not implement
//! `rf_core_api::EmulatorCore` (no `save_state`/`state_view`) — NES state
//! serialization is ticket W2-04's job. What's reachable from this crate
//! today, and exactly what the hash covers (enumerated, not a 64 KiB
//! `peek` sweep, which would drag in constant PRG ROM for nothing):
//! WRAM `$0000-$07FF` (the real 2 KiB, not its `$0800`-stepped mirrors),
//! OAM (`NesBus::oam`), PRG-RAM (`NesBus::prg_ram`), CPU `a, x, y, s, pc,
//! p, jammed`, and `NesBus::master_cycle`/`frame_count`. PPU-internal state
//! (VRAM, palette RAM, loopy `v`/`t`/`x`/`w`) and APU state are **not**
//! reachable and are not covered — a PPU-internal divergence that happens
//! to render identically would not be caught by this hash. The rendered
//! framebuffer is a separate, separately-named digest the test suite
//! computes on its own (never folded in here): it is *output*, not state.
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
        Ok(EmuStepper {
            bus,
            cpu,
            state: RunState::Paused,
            cycle_budget: CYCLE_BUDGET,
            last_scanline: None,
        })
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
    /// view, and what the determinism suite's positive WRAM assertion
    /// reads (ticket W1-07).
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

    /// The reachable-state hash (module doc's "The reachable-state hash"
    /// section — read it before using this for anything: it is explicitly
    /// **not** a full-machine hash). SHA-256 over, in order: WRAM
    /// `$0000-$07FF` (2048 bytes via [`NesBus::peek`], side-effect-free),
    /// OAM, PRG-RAM, `Cpu::{a,x,y,s,p}`, `Cpu::pc` (little-endian),
    /// `Cpu::jammed`, `NesBus::master_cycle` (little-endian),
    /// `NesBus::frame_count` (little-endian).
    #[must_use]
    pub fn state_hash(&self) -> String {
        let mut buf = Vec::with_capacity(0x0800 + 256 + 0x2000 + 32);
        for addr in 0x0000u16..=0x07FF {
            buf.push(self.bus.peek(addr));
        }
        buf.extend_from_slice(self.bus.oam());
        buf.extend_from_slice(self.bus.prg_ram());
        buf.push(self.cpu.a);
        buf.push(self.cpu.x);
        buf.push(self.cpu.y);
        buf.push(self.cpu.s);
        buf.push(self.cpu.p);
        buf.extend_from_slice(&self.cpu.pc.to_le_bytes());
        buf.push(u8::from(self.cpu.jammed));
        buf.extend_from_slice(&self.bus.master_cycle().to_le_bytes());
        buf.extend_from_slice(&self.bus.frame_count().to_le_bytes());
        crate::hash::sha256_hex(&buf)
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
