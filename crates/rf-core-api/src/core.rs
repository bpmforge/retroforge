//! The core contract itself: [`EmulatorCore`] and [`CoreSink`]
//! (ARCHITECTURE §5; CONTRACTS §1; FR-CORE-001, FR-CORE-005).
use crate::cart::CartImage;
use crate::error::{CoreError, StateError};
use crate::event::{CoreEvent, EventMask};
use crate::input::InputFrame;
use crate::state_view::{StateReader, StateView, StateWriter};
use crate::video::{OverlayPixel, PpuPixel, SubPixel};

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
    /// Memory watchpoints the core evaluates on its own buses (ticket
    /// W13-02e; DEBUGGER.md §1). Empty by default, and
    /// [`crate::WatchTable::is_armed`] is the single bool a core checks
    /// before looking at an access — an unwatched session pays one
    /// predictable branch.
    ///
    /// Here rather than behind a new `EmulatorCore` method for the same
    /// reason `event_mask` is: consumers only ever reach a core through
    /// `&mut dyn EmulatorCore`, and `fn config(&mut self) -> &mut
    /// CoreConfig` is already the path. It is a fixed-capacity value type
    /// precisely so this struct stays `Copy`.
    pub watches: crate::WatchTable,
}

impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            accuracy_mode: true,
            event_mask: EventMask::NONE,
            extra_flags: 0,
            watches: crate::WatchTable::new(),
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

    /// One scanline's worth of overlay-only pixels (ticket W3-05a; see
    /// [`crate::video::OverlayPixel`]'s doc) — sprites the hardware
    /// per-scanline limit dropped, for a consumer that opted into drawing
    /// them anyway. A **separate** channel from [`Self::video_scanline`],
    /// never a replacement for it: [`PpuPixel`] stays accuracy-exact
    /// regardless of whether a core ever calls this.
    ///
    /// Defaulted to a no-op so every existing [`CoreSink`] implementor
    /// (test mocks included) keeps compiling unchanged — a sink that wants
    /// the overlay (e.g. `rf-renderer`'s `FrameBuffer`) overrides it; one
    /// that wraps another sink (e.g. `retroforge::stepper`'s
    /// `CountingSink`) MUST forward it explicitly, or the overlay silently
    /// vanishes at that wrapper regardless of what the core emitted.
    fn overlay_scanline(&mut self, _y: u16, _pixels: &[OverlayPixel]) {}

    /// One scanline's worth of SUB-SCREEN pixels, plus the scanline's
    /// `$2132` fixed colour (ticket W7-16; see
    /// [`crate::video::SubPixel`]).
    ///
    /// The third channel, and the same rule as [`Self::overlay_scanline`]:
    /// **parallel to [`Self::video_scanline`], never a replacement.** A
    /// sink that ignores this still receives the accuracy-exact main
    /// screen; it simply cannot draw colour math or true hires, both of
    /// which are defined by combining two screens.
    ///
    /// `fixed_color` is BGR555 and arrives once per line rather than per
    /// pixel because `$2132` is a register — and an HDMA-written one, so
    /// per-line delivery is what a gradient needs.
    ///
    /// Defaulted to a no-op so every existing implementor keeps compiling
    /// unchanged. A sink that WRAPS another must forward it explicitly, or
    /// the sub-screen vanishes at the wrapper exactly as the overlay would.
    fn sub_scanline(&mut self, _y: u16, _pixels: &[SubPixel], _fixed_color: u16) {}

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
    ///
    /// Memories are lent as slices; the CPU register file is a typed
    /// [`crate::CpuRegs`] value (W13-02i). The rule for growing this
    /// view — and the trait — is in `cpu_regs.rs`'s module doc: a new
    /// expression is added when a second core has a consumer for it, and
    /// it is typed, never a byte layout.
    fn state_view(&self) -> StateView<'_>;

    /// Mutable access to this core's accuracy/compatibility switches.
    fn config(&mut self) -> &mut CoreConfig;

    /// Read one byte at a CPU-bus address **without perturbing the
    /// machine** (ticket W11-10).
    ///
    /// [`Self::state_view`] lends the raw memories — WRAM, VRAM, CGRAM,
    /// OAM — and that is the right shape for a viewer that wants to walk
    /// a whole region. It cannot answer "what does the CPU see at
    /// `$6000`?", because that goes through the mapper, and mapping is
    /// exactly what a bus does.
    ///
    /// Three shipped features need this and nothing else will do: the
    /// debugger's memory viewer, the full-level view's camera probe
    /// (W11-02) and the plugin host's memory window (W11-04). It is on
    /// the trait rather than on a concrete core so the shell can keep
    /// working when the core underneath it is a different console.
    ///
    /// Non-perturbing is a hard requirement, not a courtesy: this runs
    /// every frame while a game is playing, and a peek with side effects
    /// would make the enhancement layer a participant in the simulation
    /// — the exact thing ARCHITECTURE §2's honesty contract forbids.
    /// Reads of open-bus or unmapped addresses return whatever the core
    /// considers the quiescent value; they must not latch anything.
    fn peek(&self, addr: u32) -> u8;
}
