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

use rf_core_api::{
    CartImage, CoreConfig, CoreError, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind,
    StateError, StateReader, StateView, StateWriter, Step, StepResult,
};

use crate::{Cpu, NesBus};

/// Four NTSC frames' worth of master cycles.
///
/// Generous on purpose: it is a defence against a machine that can never
/// finish a frame, not a frame-pacing mechanism. A budget tight enough to
/// interrupt ordinary emulation would turn a correctness backstop into a
/// source of dropped frames.
pub const CYCLE_BUDGET: u64 = 4 * 341 * 262 * 4;

/// The NES as an [`EmulatorCore`].
pub struct NesCore {
    bus: NesBus,
    cpu: Cpu,
    config: CoreConfig,
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
        })
    }

    /// Borrow the bus, for the shell's out-of-band reads and the
    /// debugger's viewers.
    #[must_use]
    pub fn bus(&self) -> &NesBus {
        &self.bus
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
        self.cpu.step(&mut self.bus);
        self.bus.drain_video(sink);
        self.bus.drain_audio(sink);
        self.bus.frame_count() != start_frame
    }
}

impl EmulatorCore for NesCore {
    fn load(&mut self, cart: CartImage<'_>) -> Result<(), CoreError> {
        let mut bus = NesBus::from_ines_bytes(cart.rom)
            .map_err(|e| CoreError::InvalidImage(format!("{e:?}")))?;
        self.cpu = Cpu::power_on(&mut bus);
        self.bus = bus;
        Ok(())
    }

    fn reset(&mut self, _kind: ResetKind) {
        // Soft and hard are the same here for now, and saying so beats
        // pretending otherwise: `Cpu::power_on` re-reads the reset vector
        // through the bus, which is what both kinds do on real hardware;
        // what a hard reset would additionally clear (RAM to its
        // power-on pattern) `rf-nes` does not model separately today.
        self.cpu = Cpu::power_on(&mut self.bus);
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
        let start_frame = self.bus.frame_count();
        let start_cycle = self.bus.master_cycle();
        let deadline = start_cycle + CYCLE_BUDGET;
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
                while self.bus.master_cycle() < deadline {
                    if self.tick(&mut counting, start_frame) {
                        frame_complete = true;
                        break;
                    }
                    if want_scanline && counting.seen > 0 {
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
            cpu_regs: &[],
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
}
