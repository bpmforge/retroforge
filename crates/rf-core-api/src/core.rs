//! The core contract itself: [`EmulatorCore`] and [`CoreSink`]
//! (ARCHITECTURE §5; CONTRACTS §1; FR-CORE-001, FR-CORE-005).
use crate::cart::CartImage;
use crate::error::{CoreError, StateError};
use crate::event::{CoreEvent, EventMask};
use crate::input::InputFrame;
use crate::state_view::{StateReader, StateView, StateWriter};
use crate::video::PpuPixel;

/// How [`EmulatorCore::reset`] should behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetKind {
    /// Reset-line equivalent (console RESET button): CPU/PPU/APU state
    /// reinitializes, but WRAM and other retained state is left as-is,
    /// matching real hardware.
    Soft,
    /// Power-cycle equivalent: all volatile state (including WRAM) returns
    /// to its cold-boot value.
    Hard,
}

/// Step granularity for cycle-precise debugger stepping (FR-CORE-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Advance by one CPU instruction.
    Instruction,
    /// Advance to the end of the current scanline.
    Scanline,
    /// Advance to the end of the current frame (equivalent to one
    /// `run_frame`, but reachable through the same stepping entry point the
    /// debugger uses for the finer granularities).
    Frame,
}

/// Outcome of one [`EmulatorCore::step`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepResult {
    /// Master-clock cycles consumed by this step.
    pub cycles: u64,
    /// `true` if this step crossed a frame boundary (i.e. a
    /// [`crate::CoreEvent::FrameEnd`]-equivalent point was reached),
    /// regardless of the requested granularity.
    pub frame_complete: bool,
}

/// Accuracy/compatibility switches passed to a core (ARCHITECTURE §5,
/// §4). Fresh installs boot in Accuracy Mode (project law), hence
/// `accuracy_mode: true` in [`Default`].
///
/// This is also the public path for FR-CORE-006 subscription control:
/// `EmulatorCore` has no separate `set_event_mask` method in the §5
/// sketch, and consumers only ever reach a core through `&mut dyn
/// EmulatorCore` (never a concrete type), so `event_mask` lives here where
/// `fn config(&mut self) -> &mut CoreConfig` already exposes it —
/// `core.config().event_mask.insert(EventMask::SCANLINE)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreConfig {
    /// When `true`, the core must not take any documented "compatibility"
    /// fast path (ARCHITECTURE §4 mode 1: Accuracy) — bit-for-bit reference
    /// behavior only. When `false`, the core may use compatibility-tested
    /// fast paths (§4 mode 2) while remaining deterministic.
    pub accuracy_mode: bool,
    /// Events the core should push through `CoreSink::event`. Defaults to
    /// [`EventMask::NONE`]: Accuracy mode with no subscribers pays ~zero
    /// per-event cost (ARCHITECTURE §5).
    pub event_mask: EventMask,
    /// Reserved per-core toggle bits (e.g. sprite-limit bypass, W3-05).
    /// Kept as a plain bitmask here so individual cores can define their
    /// own switches without changing this shared struct's shape.
    pub extra_flags: u32,
}

impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            accuracy_mode: true,
            event_mask: EventMask::NONE,
            extra_flags: 0,
        }
    }
}

/// Push-style output sink an [`EmulatorCore`] writes video, audio and
/// events to during `run_frame`/`step` (ARCHITECTURE §5).
///
/// Kept object-safe (no generic methods, no `Self: Sized` bounds) because
/// callers hold it as `&mut dyn CoreSink` — the enhancement side never
/// reaches into a running core; everything crosses this one push boundary.
pub trait CoreSink {
    /// One fully-rendered scanline of indexed pixels (FR-CORE-004). Never
    /// RGB — see [`crate::video`].
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]);

    /// A block of interleaved audio samples produced since the last call.
    fn audio(&mut self, samples: &[i16]);

    /// A subscribed event fired. Callers must check the relevant
    /// [`crate::EventMask`] bit before constructing `ev` — see
    /// [`crate::EventMask`] (FR-CORE-006).
    fn event(&mut self, ev: CoreEvent);
}

/// The contract every emulator core implements (ARCHITECTURE §5;
/// CONTRACTS §1; FR-CORE-001). `rf-nes`/`rf-snes` implement this; the
/// frontend, harness, enhancement layer and debugger consume it only
/// through this trait plus [`StateView`] — never by reaching into a
/// concrete core type.
pub trait EmulatorCore {
    /// Load a cartridge image, replacing any previously loaded one.
    ///
    /// # Errors
    /// Returns [`CoreError`] if `cart` is not a valid image for this core
    /// (see [`crate::cart`] for why this takes [`CartImage`] rather than
    /// `rf_cart::Cartridge`).
    fn load(&mut self, cart: CartImage<'_>) -> Result<(), CoreError>;

    /// Reset the machine. See [`ResetKind`] for the soft/hard distinction.
    fn reset(&mut self, kind: ResetKind);

    /// Run exactly one video frame. Deterministic: same state + same input
    /// => same state. The only way time advances in normal operation
    /// (ARCHITECTURE §5, §6).
    fn run_frame(&mut self, input: &InputFrame, sink: &mut dyn CoreSink);

    /// Cycle-precise stepping for the debugger (FR-CORE-005).
    fn step(&mut self, granularity: Step, sink: &mut dyn CoreSink) -> StepResult;

    /// Serialize full machine state.
    ///
    /// # Errors
    /// Returns [`StateError`] if `w` rejects a write.
    fn save_state(&self, w: &mut dyn StateWriter) -> Result<(), StateError>;

    /// Restore full machine state previously written by [`Self::save_state`].
    ///
    /// # Errors
    /// Returns [`StateError`] if `r` is exhausted early or the stream is
    /// not valid for this core.
    fn load_state(&mut self, r: &mut dyn StateReader) -> Result<(), StateError>;

    /// Read-only, borrowed view of machine state for the enhancement/debug
    /// side. Valid only between frames.
    fn state_view(&self) -> StateView<'_>;

    /// Mutable access to this core's accuracy/compatibility switches.
    fn config(&mut self) -> &mut CoreConfig;
}
