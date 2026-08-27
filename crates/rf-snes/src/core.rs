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
    CartImage, CoreConfig, CoreError, CoreSink, EmulatorCore, InputFrame, ResetKind, StateError,
    StateReader, StateView, StateWriter, Step, StepResult,
};

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

/// The SNES as an [`EmulatorCore`].
pub struct SnesCore {
    system: SnesSystem,
    config: CoreConfig,
    budget: u64,
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
            budget: FRAME_INSTRUCTION_BUDGET,
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
    pub fn set_frame_budget(&mut self, budget: u64) {
        self.budget = budget;
    }

    /// Replay the settled frame into `sink`, line by line.
    ///
    /// Both the picture and the dropped-sprite overlay: `Scanline`
    /// carries `overlay` alongside `pixels`, and forwarding only the
    /// pixels would make the 32-per-line sprite suppression invisible to
    /// exactly the feature built to show it.
    fn emit_frame(&mut self, sink: &mut dyn CoreSink) {
        let visible = self.system.bus.ppu.setini.visible_lines();
        for y in 0..visible {
            let line = self.system.bus.ppu.render_scanline(y);
            sink.video_scanline(y, &line.pixels);
            sink.overlay_scanline(y, &line.overlay);
        }
    }
}

impl EmulatorCore for SnesCore {
    fn load(&mut self, cart: CartImage<'_>) -> Result<(), CoreError> {
        self.system =
            SnesSystem::load(cart.rom).map_err(|e| CoreError::InvalidImage(format!("{e:?}")))?;
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
        let start_frame = self.system.bus.timing.frame;
        let mut instructions = 0u64;
        let frame_complete;

        match granularity {
            Step::Instruction => {
                // A halted CPU is not an error here: `SnesSystem::step`
                // reports the halt opcode, and the debugger wants to see
                // that state rather than have it swallowed.
                let _ = self.system.step();
                instructions = 1;
                frame_complete = self.system.bus.timing.frame != start_frame;
            }
            Step::Scanline | Step::Frame => {
                // Run to a settled vblank and out the other side, which
                // is what makes composition safe on this machine.
                while !self.system.bus.timing.in_vblank() && instructions < self.budget {
                    if self.system.step().is_err() {
                        break;
                    }
                    instructions += 1;
                }
                while (self.system.bus.timing.in_vblank()
                    || self.system.bus.timing.frame == start_frame)
                    && instructions < self.budget
                {
                    if self.system.step().is_err() {
                        break;
                    }
                    instructions += 1;
                }
                frame_complete = self.system.bus.timing.frame != start_frame;
                if frame_complete {
                    self.emit_frame(sink);
                }
            }
        }

        StepResult {
            // Instructions, not master cycles: this core counts its
            // budget in instructions, and reporting a number in a unit it
            // does not measure would be worse than reporting this one.
            cycles: instructions,
            frame_complete,
        }
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
            cpu_regs: &[],
            wram: &self.system.bus.wram,
            vram: &[],
            cgram: &[],
            oam: &[],
            ppu_regs: &[],
            mapper_state: &[],
        }
    }

    fn config(&mut self) -> &mut CoreConfig {
        &mut self.config
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
            crate::mapping::Target::Rom(i) => self.system.bus.rom.get(i).copied().unwrap_or(0),
            crate::mapping::Target::Wram(i) => self.system.bus.wram.get(i).copied().unwrap_or(0),
            crate::mapping::Target::Sram(i) => self.system.bus.sram.get(i).copied().unwrap_or(0),
            crate::mapping::Target::Register(_) | crate::mapping::Target::Open => 0,
        }
    }
}
