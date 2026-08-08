//! FM-01 core-panic containment (ticket W1-06 acceptance criterion 4,
//! `docs/design/FAILURE_MODES.md` FM-01: "Core panic mid-frame").
//!
//! The NES core runs on its own OS thread, separate from the UI thread
//! `eframe::App::ui` executes on (FM-01's "Detection: catch_unwind at the
//! core-thread boundary ... Containment: Core thread halts; host/UI
//! threads unaffected"). This module is the generic guarded-loop primitive
//! ([`run_guarded_loop`]) plus the real NES wiring ([`spawn`]); it has no
//! `egui`/`eframe` dependency, so it is fully testable headlessly — the
//! FM-01 injected-panic test below spins up a real thread and a real
//! channel, no window involved.
//!
//! ## Why a custom panic hook, not just `JoinHandle::join`
//!
//! `std::panic::catch_unwind`'s `Err` payload is only ever the value
//! passed to `panic!` (typically `&str`/`String`) — it does **not**
//! include a backtrace, and a backtrace captured *after* `catch_unwind`
//! returns would be a trace of the unwind-catching code, not of where the
//! panic actually happened (the stack has already unwound past that point
//! by then). The only way to capture a trace *at* the panic site is a
//! `std::panic::set_hook` closure, which runs while the stack is still
//! intact. [`install_panic_capture_hook`] installs one that force-captures
//! a backtrace (`Backtrace::force_capture`, which — unlike the default
//! hook's printing — ignores `RUST_BACKTRACE`/`RUST_LIB_BACKTRACE` so this
//! works the same in CI as on a dev machine) into a thread-local, which
//! [`CoreCrashReport`]'s (private) `capture` constructor reads back
//! immediately after `catch_unwind` returns `Err` **on the same thread**
//! (panic hooks run on the panicking
//! thread, and `catch_unwind` resumes on that same thread — no
//! cross-thread race).
use std::any::Any;
use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Once};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rf_core_api::InputFrame;
use rf_nes::NesLoadError;

use crate::canvas_accum::CanvasAccumulator;
use crate::pacer::FramePacer;
use crate::stepper::EmuStepper;

/// The UI thread's latest held-input sample, shared with the core thread
/// (ticket W1-07's live wiring). One `AtomicU64` packing all four
/// [`InputFrame`] ports (16 bits each) — not four separate atomics, which
/// could tear across ports and hand the core a frame that never actually
/// existed on either side. No mutex: the UI thread stores a fresh sample
/// every repaint and the core thread loads it once per frame (module doc
/// "one shared latch-then-advance path" in `crate::stepper`); a lock here
/// would mean the higher-priority core thread's frame loop contending with
/// the UI thread on every single frame for no benefit — the atomic already
/// gives torn-free "latest value wins" semantics, which is exactly what a
/// per-frame sample needs.
#[derive(Debug, Default)]
pub struct SharedInputFrame(AtomicU64);

impl SharedInputFrame {
    #[must_use]
    pub fn new() -> Self {
        SharedInputFrame(AtomicU64::new(0))
    }

    fn pack(frame: InputFrame) -> u64 {
        u64::from(frame.ports[0])
            | u64::from(frame.ports[1]) << 16
            | u64::from(frame.ports[2]) << 32
            | u64::from(frame.ports[3]) << 48
    }

    fn unpack(bits: u64) -> InputFrame {
        InputFrame {
            ports: [
                (bits & 0xFFFF) as u16,
                ((bits >> 16) & 0xFFFF) as u16,
                ((bits >> 32) & 0xFFFF) as u16,
                ((bits >> 48) & 0xFFFF) as u16,
            ],
        }
    }

    /// Called by the UI thread once per repaint with the latest sampled
    /// [`InputFrame`] (`rf_input::InputLatch::sample`).
    pub fn store(&self, frame: InputFrame) {
        self.0.store(Self::pack(frame), Ordering::Relaxed);
    }

    /// Called by the core thread once per frame, at the top of the shared
    /// latch-then-advance path.
    pub fn load(&self) -> InputFrame {
        Self::unpack(self.0.load(Ordering::Relaxed))
    }
}

thread_local! {
    /// Set by the panic hook installed by [`install_panic_capture_hook`],
    /// consumed by [`CoreCrashReport::capture`]. Thread-local because the
    /// hook is process-global (`panic::set_hook` has no per-thread
    /// variant) but each thread's own panic must never see another
    /// thread's leftover capture.
    static LAST_PANIC_LOCATION: RefCell<Option<String>> = const { RefCell::new(None) };
    static LAST_PANIC_BACKTRACE: RefCell<Option<String>> = const { RefCell::new(None) };
}

static INSTALL_HOOK_ONCE: Once = Once::new();

/// Install the backtrace-capturing panic hook (see module doc). Safe to
/// call more than once (idempotent via `Once`) and safe to call from
/// multiple threads — every thread that might panic under
/// [`run_guarded_loop`] must call this before the first guarded call.
pub fn install_panic_capture_hook() {
    INSTALL_HOOK_ONCE.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let location = info
                .location()
                .map_or_else(|| "<unknown location>".to_string(), ToString::to_string);
            let backtrace = std::backtrace::Backtrace::force_capture().to_string();
            LAST_PANIC_LOCATION.with(|cell| *cell.borrow_mut() = Some(location));
            LAST_PANIC_BACKTRACE.with(|cell| *cell.borrow_mut() = Some(backtrace));
            previous(info);
        }));
    });
}

/// Last N non-empty lines of `text` — the "trace-ring tail" FM-01's row
/// calls for, not the (often hundreds-of-lines) full backtrace.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// FM-01's "Error dialog with ROM hash + trace-ring tail" — the ROM hash
/// half is the app layer's job (it knows which ROM was loaded; this type
/// doesn't), this carries the panic message + captured trace tail.
#[derive(Debug, Clone)]
pub struct CoreCrashReport {
    /// The `panic!` payload, stringified (`&str`/`String` downcast; a
    /// fixed message for any other payload type).
    pub message: String,
    /// Source location of the `panic!` call, if the hook captured one.
    pub location: Option<String>,
    /// Tail of a force-captured backtrace (see module doc) — never empty
    /// when the panic hook was installed, which [`spawn`]/[`run_guarded_loop`]
    /// callers are responsible for doing before the first guarded call.
    pub trace_tail: String,
}

impl CoreCrashReport {
    /// Build a report from a caught `catch_unwind` payload, reading back
    /// whatever [`install_panic_capture_hook`]'s hook stashed for the
    /// current thread (see module doc for why this is race-free).
    fn capture(payload: Box<dyn Any + Send>) -> Self {
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "core thread panicked with a non-string payload".to_string()
        };
        let location = LAST_PANIC_LOCATION.with(RefCell::take);
        let backtrace = LAST_PANIC_BACKTRACE.with(RefCell::take).unwrap_or_default();
        CoreCrashReport {
            message,
            location,
            trace_tail: tail_lines(&backtrace, 20),
        }
    }
}

/// Tells [`run_guarded_loop`] whether to run another iteration.
pub enum LoopControl {
    Continue,
    Stop,
}

/// One RGBA frame plus its dimensions, cheap to send across a channel
/// (`Vec<u8>` is moved, not copied).
pub struct FrameMsg {
    pub rgba: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Machine position when this frame was produced (ticket W2-15):
    /// completed-frame count, and the last visible scanline drawn.
    /// Carried on the frame itself rather than as a separate event —
    /// every advance path already sends exactly one frame (W2-14), so
    /// there is no position change the UI could miss.
    pub frame_count: u64,
    pub last_scanline: Option<u16>,
    /// Ticket W3-03: the same frame's BG-only and sprite-only layers
    /// (`rf_renderer::LayeredFrame`, extracted from the identical
    /// `CoreSink::video_scanline` stream `rgba` above was built from), each
    /// `width * height * 4` bytes, transparent where the other layer drew.
    /// The app layer's debug view turns these into separate egui textures.
    pub bg_rgba: Vec<u8>,
    pub sprite_rgba: Vec<u8>,
}

/// What the core thread reports back to the UI thread.
pub enum CoreEvent {
    /// A new frame is ready to paint.
    Frame(FrameMsg),
    /// FM-01: the core thread panicked, was contained, and has now
    /// halted. No further `CoreEvent`s will ever arrive on this channel
    /// after this one.
    Crashed(CoreCrashReport),
    /// Ticket W4-03e: the reply to `CoreCommand::RequestCanvasSnapshot` —
    /// a clone of the CURRENT scene's stitched canvas
    /// (`crate::canvas_accum::CanvasAccumulator::current_canvas`), for the
    /// UI thread to resolve into an ultrawide render
    /// (`crate::enhanced_view::compose_ultrawide`). Pull, not push
    /// (`crate::canvas_accum`'s own module doc): cloning a whole `Canvas`
    /// every frame would be tens of MB/s at 60Hz for a level of any real
    /// size, so this only happens when the UI thread actually asks.
    CanvasSnapshot(rf_enhance::stitcher::Canvas),
}

/// What the UI thread can ask the core thread to do (FR-DBG-004).
pub enum CoreCommand {
    Pause,
    Resume,
    StepFrame,
    StepScanline,
    /// Ticket W3-05a: opt into (or out of) the sprite-limit-bypass overlay.
    /// A pure state toggle, not a stepping action — it does not itself
    /// produce a `CoreEvent::Frame`; the next running/stepped frame simply
    /// reflects the new setting.
    SetSpriteOverlay(bool),
    /// Ticket W4-03e: ask for a `CoreEvent::CanvasSnapshot` of the current
    /// scene's stitched canvas (see that variant's doc). Also flushes the
    /// canvas accumulator's cache (`CanvasAccumulator::flush`) — piggy-
    /// backing persistence on the same user-driven cadence (whenever the
    /// UI is actually showing/refreshing the Ultrawide view) rather than
    /// every frame.
    RequestCanvasSnapshot,
    Shutdown,
}

/// Run `body` repeatedly on the *current* thread until it returns
/// [`LoopControl::Stop`] or panics. Every call is individually guarded by
/// `catch_unwind` (FM-01): a panic inside `body` is caught, converted to a
/// [`CoreCrashReport`] (with a trace tail — see module doc), sent exactly
/// once on `evt_tx`, and the loop halts — it is never retried and the
/// panic is never allowed to unwind out of this function
/// (`docs/design/FAILURE_MODES.md` FM-01: "Core thread halts; host/UI
/// threads unaffected"). Callers are responsible for having called
/// [`install_panic_capture_hook`] first (both [`spawn`] and the FM-01 test
/// below do).
pub fn run_guarded_loop<F>(evt_tx: &Sender<CoreEvent>, mut body: F)
where
    F: FnMut() -> LoopControl,
{
    loop {
        match panic::catch_unwind(AssertUnwindSafe(&mut body)) {
            Ok(LoopControl::Continue) => {}
            Ok(LoopControl::Stop) => return,
            Err(payload) => {
                let report = CoreCrashReport::capture(payload);
                // Best-effort: if the UI thread is already gone, there's
                // nothing left to report to — just halt.
                let _ = evt_tx.send(CoreEvent::Crashed(report));
                return;
            }
        }
    }
}

/// Spawn the real NES core thread for `rom` bytes. Returns a command
/// sender, an event receiver, and the `JoinHandle` (the handle's own
/// closure never panics — FM-01 containment happens *inside*
/// `run_guarded_loop`, so `handle.join()` returning `Err` would itself be
/// a containment bug, not an expected outcome).
///
/// If `rom` fails to load, a [`CoreEvent::Crashed`]-shaped report is not
/// used (that's for *panics*, not ordinary load errors) — instead
/// [`NesLoadError`] is returned synchronously and no thread is spawned.
///
/// # Errors
/// Returns [`NesLoadError`] if `rom` is not a loadable iNES/NES 2.0 NROM
/// image.
pub fn spawn(rom: Vec<u8>) -> Result<CoreHandle, NesLoadError> {
    // Validate up front so a bad ROM is a synchronous `Result`, not a
    // channel message the UI has to poll for — the same pattern
    // `rom_open::load_rom_bytes` uses for the file-dialog path.
    EmuStepper::from_ines_bytes(&rom)?;

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (evt_tx, evt_rx) = mpsc::channel();
    let input = Arc::new(SharedInputFrame::new());
    let thread_input = Arc::clone(&input);
    // Ticket W4-01: the writer half is moved into the core thread; the
    // reader half is handed back to the caller (render/debug threads clone
    // it from `CoreHandle::frame_bundle`). Seeded with `FrameBundle::empty()`
    // so a reader that polls before the first real frame gets a
    // well-defined value (`triple_buffer`'s own doc).
    let (bundle_writer, bundle_reader) =
        rf_core_api::triple_buffer(rf_core_api::FrameBundle::empty());
    // Ticket W4-03e: identifies this ROM for the canvas cache key
    // (`rf_enhance::persistence::canvas_cache_key`'s own `rom_sha256`
    // field) — computed once here (off the hot per-frame path) rather
    // than inside `core_thread_main`.
    let rom_sha256 = crate::hash::sha256_hex(&rom);
    let handle = thread::spawn(move || {
        core_thread_main(rom, rom_sha256, cmd_rx, evt_tx, thread_input, bundle_writer);
    });
    Ok(CoreHandle {
        cmd_tx,
        evt_rx,
        join_handle: handle,
        input,
        frame_bundle: bundle_reader,
    })
}

/// Handle the UI thread holds for a spawned core thread.
pub struct CoreHandle {
    pub cmd_tx: Sender<CoreCommand>,
    pub evt_rx: Receiver<CoreEvent>,
    pub join_handle: JoinHandle<()>,
    /// The UI thread's write side of the live input latch (ticket W1-07):
    /// call `input.store(latch.sample(&keymap))` once per repaint.
    pub input: Arc<SharedInputFrame>,
    /// Ticket W4-01: the read side of the triple-buffered `FrameBundle`
    /// stream (`rf_core_api::triple_buffer`) — clone this for each
    /// independent consumer (render thread, debug panels; see
    /// `rf_core_api::TripleBufferReader::clone`'s doc for why cloning is
    /// cheap and each clone sees the same stream without contending with
    /// the others).
    pub frame_bundle: rf_core_api::TripleBufferReader<rf_core_api::FrameBundle>,
}

/// Ticket W3-03 (renamed from `DualSink` by ticket W4-01, which added the
/// third leg below): fans out each `CoreSink` call to the accuracy-frame
/// sink (`rf_renderer::FrameBuffer`, unchanged — still the only thing
/// W3-05a's overlay reaches), the layer-extraction sink
/// (`rf_renderer::LayeredFrame`), and the `FrameBundle` accumulator
/// (`rf_core_api::FrameBundleBuilder`), so `EmuStepper`'s single `&mut dyn
/// CoreSink` parameter can feed all three without widening `stepper.rs`'s
/// API. `overlay_scanline` is deliberately forwarded to `frame` only — the
/// dropped-sprite overlay stays exactly where W3-05a put it (plan.json
/// W3-03's forward note: migrating it into the extracted layers is W3-05's
/// job, not this ticket's) — and `FrameBundleBuilder` itself also
/// deliberately no-ops `overlay_scanline` (see its own `CoreSink` impl doc):
/// `FrameBundle::video` must stay accuracy-exact regardless of any
/// enhancement overlay.
struct FanoutSink<'a> {
    frame: &'a mut rf_renderer::FrameBuffer,
    layers: &'a mut rf_renderer::LayeredFrame,
    bundle: &'a mut rf_core_api::FrameBundleBuilder,
}

impl rf_core_api::CoreSink for FanoutSink<'_> {
    fn video_scanline(&mut self, y: u16, pixels: &[rf_core_api::PpuPixel]) {
        self.frame.video_scanline(y, pixels);
        self.layers.video_scanline(y, pixels);
        self.bundle.video_scanline(y, pixels);
    }

    fn overlay_scanline(&mut self, y: u16, pixels: &[rf_core_api::OverlayPixel]) {
        self.frame.overlay_scanline(y, pixels);
    }

    fn audio(&mut self, samples: &[i16]) {
        self.frame.audio(samples);
        self.bundle.audio(samples);
    }

    fn event(&mut self, ev: rf_core_api::CoreEvent) {
        self.frame.event(ev);
        self.bundle.event(ev);
    }
}

/// Placeholder cache root for the stitched-canvas cache (ticket W4-03e).
/// Not a considered app-data-directory policy — that is a separate,
/// unscoped decision (no XDG/platform-data-dir convention exists anywhere
/// else in this workspace to align with yet) — just enough of a real,
/// writable location that [`rf_cache::Cache::open`] genuinely persists to
/// disk across a `Cache` re-open within the same machine, which is all
/// this ticket's four acceptance criteria need. A future ticket choosing a
/// permanent location only has to change this one path.
fn canvas_cache_root() -> std::path::PathBuf {
    std::env::temp_dir().join("retroforge-canvas-cache")
}

/// Cap for the placeholder canvas cache above — generous for a handful of
/// stitched levels, not tuned against any measured workload (same "not a
/// considered policy" caveat as [`canvas_cache_root`]).
const CANVAS_CACHE_CAP_BYTES: u64 = 256 * 1024 * 1024;

fn core_thread_main(
    rom: Vec<u8>,
    rom_sha256: String,
    cmd_rx: Receiver<CoreCommand>,
    evt_tx: Sender<CoreEvent>,
    input: Arc<SharedInputFrame>,
    mut bundle_writer: rf_core_api::TripleBufferWriter<rf_core_api::FrameBundle>,
) {
    install_panic_capture_hook();

    // Re-parse inside the thread too: `spawn`'s pre-check already proved
    // this succeeds, but re-deriving state here (rather than trying to
    // move a already-validated `EmuStepper` across the `thread::spawn`
    // boundary awkwardly) keeps this thread self-contained. `expect` is
    // safe: `spawn` already returned `Err` and never reached here if this
    // would fail.
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("rom already validated by spawn()");
    let mut sink = rf_renderer::FrameBuffer::new();
    // Ticket W3-03: same-frame BG/sprite layer extraction, fed alongside
    // `sink` via `FanoutSink` at every call site below.
    let mut layers = rf_renderer::LayeredFrame::new();
    // Ticket W4-01: the indexed-pixel + event accumulator that becomes each
    // published `FrameBundle`, fed alongside `sink`/`layers` via the same
    // `FanoutSink`.
    let mut bundle_builder = rf_core_api::FrameBundleBuilder::new(
        rf_renderer::frame::NES_WIDTH as u16,
        rf_renderer::frame::NES_HEIGHT as u16,
    );
    // Ticket W4-03e: fed one real `FrameBundle` per frame below, right
    // where `bundle_builder` is drained — `crate::canvas_accum`'s own
    // module doc explains why this must happen on THIS thread (every real
    // frame, no gaps) rather than the UI thread polling the triple buffer.
    // A cache-open failure (unwritable placeholder dir, etc.) degrades to
    // `None` — persistence is strictly additive (`CanvasAccumulator`'s own
    // doc), never a reason to fail loading the ROM.
    let cache = rf_cache::Cache::open(canvas_cache_root(), CANVAS_CACHE_CAP_BYTES).ok();
    let mut canvas_accum = CanvasAccumulator::new(rom_sha256, cache);

    // `run_guarded_loop` needs its own `&Sender` (to report a crash) at
    // the same time the loop body needs to *own* a sender to `send` frames
    // from inside a `move` closure — clone rather than fight the borrow.
    let frame_tx = evt_tx.clone();
    // Ticket W2-18: without this the loop free-runs — 18.8x real speed in
    // a release build. See `crate::pacer` for why it is deadline-based
    // rather than a fixed sleep, and why it cannot affect determinism.
    let mut pacer = FramePacer::new();
    run_guarded_loop(&evt_tx, move || {
        // Ticket W2-14: a stepped frame must be SENT, not just rendered.
        // Before this flag existed the only `CoreEvent::Frame` send site
        // was inside `tick_running_with_input`'s `true` branch, which is
        // unreachable while Paused — so Step Frame/Step Scanline advanced
        // the machine correctly, drew into `sink`, and then silently
        // discarded the result. The emulator stepped; the user just never
        // saw it.
        let mut stepped = false;
        for cmd in cmd_rx.try_iter() {
            match cmd {
                CoreCommand::Pause => stepper.pause(),
                CoreCommand::Resume => stepper.resume(),
                // Debugger single-step latches held keys too (ticket
                // W1-07 conductor fix): a stepped frame is still a whole
                // frame, so it goes through the same shared
                // latch-then-advance path as live gameplay
                // (`crate::stepper`'s module doc says that path is the
                // ONLY one, and this arm advancing a frame outside it
                // would make that doc false), then force-pauses exactly
                // as `EmuStepper::step_frame` does (FR-DBG-004:
                // "stepping always leaves the machine stopped"). Without
                // this, holding a key and pressing Step Frame would be
                // ignored — which is precisely the case a debugger user
                // steps a frame to inspect.
                CoreCommand::StepFrame => {
                    stepper.latch_and_advance_frame(
                        input.load(),
                        &mut FanoutSink {
                            frame: &mut sink,
                            layers: &mut layers,
                            bundle: &mut bundle_builder,
                        },
                    );
                    stepper.pause();
                    stepped = true;
                }
                // Deliberately does NOT latch, and the asymmetry with
                // StepFrame above is intentional — do not "fix" it. A
                // scanline step stops *mid-frame*, so latching here would
                // apply input at a non-frame boundary, violating
                // ARCHITECTURE §6's determinism rule that input only ever
                // takes effect at frame boundaries.
                CoreCommand::StepScanline => {
                    stepper.step_scanline(&mut FanoutSink {
                        frame: &mut sink,
                        layers: &mut layers,
                        bundle: &mut bundle_builder,
                    });
                    stepped = true;
                }
                CoreCommand::SetSpriteOverlay(enabled) => {
                    stepper.set_sprite_overlay_enabled(enabled);
                }
                CoreCommand::RequestCanvasSnapshot => {
                    canvas_accum.flush();
                    let snapshot = canvas_accum.current_canvas().unwrap_or_default();
                    if frame_tx.send(CoreEvent::CanvasSnapshot(snapshot)).is_err() {
                        return LoopControl::Stop; // UI thread hung up.
                    }
                }
                CoreCommand::Shutdown => return LoopControl::Stop,
            }
        }

        // Live wiring (ticket W1-07): latch the UI thread's most recent
        // held-key sample and advance through the one shared
        // latch-then-advance path (`EmuStepper::tick_running_with_input`,
        // `crate::stepper` module doc) — same function the determinism/
        // replay test suite drives directly via `latch_and_advance_frame`.
        // `stepped` is checked alongside the running tick (ticket W2-14):
        // a debugger step produces a frame the UI must see just as much as
        // a free-running one does. Note `step_scanline` leaves `sink`
        // holding a *partial* frame — the new scanline over the previous
        // frame's content, since `rf_renderer::FrameBuffer` has no clear
        // step. That is correct for a scanline stepper, and it is why one
        // scanline step looks almost identical: Step Frame is the visible
        // one.
        // Pace BEFORE advancing, so the sleep replaces idle spinning
        // rather than being added on top of a frame's work. Only while
        // actually running: a paused core falls through to the 5 ms idle
        // sleep below, and `resync` stops that pause from later looking
        // like a backlog to repay (`crate::pacer`).
        if stepper.is_paused() {
            pacer.resync();
        } else {
            let delay = pacer.next_delay(Instant::now());
            if !delay.is_zero() {
                thread::sleep(delay);
            }
        }
        let ran = stepper.tick_running_with_input(
            input.load(),
            &mut FanoutSink {
                frame: &mut sink,
                layers: &mut layers,
                bundle: &mut bundle_builder,
            },
        );
        if ran || stepped {
            // Ticket W4-03e: feed the enhanced-camera pipeline THIS exact
            // frame's bundle before it moves into `bundle_writer.publish`
            // below — `canvas_accum`'s own module doc is explicit that
            // every real frame must reach it, with no gaps, which is only
            // true on this thread (the UI thread's own `frame_bundle`
            // reader is latest-wins/lossy). Read-only: nothing here
            // mutates `stepper`/`sink`/`layers`, only the already-
            // materialized bundle, so this cannot perturb core state
            // (Law 6).
            let bundle = bundle_builder.take(stepper.frame_count());
            canvas_accum.observe_frame(&bundle, &[]);
            // Ticket W4-01: publish before sending `FrameMsg` so a reader
            // that wakes on the `FrameMsg` channel never sees a
            // `frame_bundle` older than the frame it was just notified
            // about.
            bundle_writer.publish(bundle);
            let msg = FrameMsg {
                rgba: sink.to_vec(),
                width: sink.width(),
                height: sink.height(),
                frame_count: stepper.frame_count(),
                last_scanline: stepper.last_scanline(),
                bg_rgba: layers.bg_rgba().to_vec(),
                sprite_rgba: layers.sprite_rgba().to_vec(),
            };
            if frame_tx.send(CoreEvent::Frame(msg)).is_err() {
                // UI thread hung up; nothing left to serve.
                return LoopControl::Stop;
            }
        } else {
            // Paused with no pending commands: don't busy-spin a whole CPU
            // core waiting for the next one.
            thread::sleep(Duration::from_millis(5));
        }
        LoopControl::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_input_frame_defaults_to_empty() {
        let shared = SharedInputFrame::new();
        assert_eq!(shared.load(), InputFrame::empty());
    }

    #[test]
    fn shared_input_frame_store_load_round_trips_all_four_ports() {
        let shared = SharedInputFrame::new();
        let frame = InputFrame {
            ports: [0x00FF, 0xABCD, 0x1234, 0xFFFF],
        };
        shared.store(frame);
        assert_eq!(shared.load(), frame);
    }

    #[test]
    fn shared_input_frame_default_trait_matches_new() {
        let shared = SharedInputFrame::default();
        assert_eq!(shared.load(), InputFrame::empty());
    }

    #[test]
    fn guarded_loop_stop_sends_no_crash_report() {
        let (evt_tx, evt_rx) = mpsc::channel();
        let mut calls = 0;
        run_guarded_loop(&evt_tx, move || {
            calls += 1;
            if calls >= 3 {
                LoopControl::Stop
            } else {
                LoopControl::Continue
            }
        });
        assert!(
            evt_rx.try_recv().is_err(),
            "an ordinary Stop must not produce a CoreEvent::Crashed"
        );
    }

    /// FM-01, ticket acceptance criterion 4: inject a panic on the core
    /// thread, assert `catch_unwind` contains it (the spawned thread's own
    /// closure returns normally, does NOT itself panic), a structured
    /// crash report reaches the UI-side channel, and it carries a
    /// non-empty trace tail. This is the test the ticket explicitly
    /// demands, and it is written so it would FAIL if `run_guarded_loop`'s
    /// `catch_unwind` were removed: without containment, the spawned
    /// thread's closure would itself panic and unwind out (making
    /// `handle.join()` return `Err`), and no `CoreEvent::Crashed` would
    /// ever be sent (the `recv_timeout` below would time out).
    #[test]
    fn injected_panic_on_core_thread_is_caught_and_reported_with_trace_tail() {
        let (evt_tx, evt_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            install_panic_capture_hook();
            run_guarded_loop(&evt_tx, move || {
                panic!("FM-01 injected core panic (test)");
            });
        });

        let evt = evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("core thread must report a crash before halting, not just vanish");
        match evt {
            CoreEvent::Crashed(report) => {
                assert!(
                    report.message.contains("FM-01 injected core panic"),
                    "message was: {}",
                    report.message
                );
                assert!(
                    !report.trace_tail.trim().is_empty(),
                    "trace tail must be captured, not empty — a no-op catch_unwind-less \
                     implementation could not produce this"
                );
                assert!(
                    report.location.is_some(),
                    "panic hook must have recorded a source location"
                );
            }
            CoreEvent::Frame(_) => panic!("expected a crash report, got an ordinary frame"),
            CoreEvent::CanvasSnapshot(_) => {
                panic!("expected a crash report, got a canvas snapshot")
            }
        }

        // The spawned thread's own top-level closure must return
        // normally: the panic was contained *inside* run_guarded_loop, not
        // propagated past it. This is the assertion that fails outright
        // (Err) if `catch_unwind` is ever removed from `run_guarded_loop`.
        assert!(
            handle.join().is_ok(),
            "core thread must not itself panic — catch_unwind must contain it (FM-01)"
        );

        // Host/test process is demonstrably still alive and able to keep
        // running further code after the crash — the point of FM-01
        // ("host/UI threads unaffected").
        let (tx2, rx2) = mpsc::channel::<()>();
        drop(tx2);
        assert!(
            rx2.recv().is_err(),
            "process is still executing normally post-crash"
        );
    }

    /// Minimal synthetic NROM, same layout `crate::stepper`'s own tests
    /// use: all-zero PRG means the reset vector resolves to `$0000`, which
    /// is zeroed RAM (`BRK`) — a deterministic infinite loop that steps
    /// forever without needing real game code.
    fn synthetic_nrom() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
        data.push(1); // 1x16KiB PRG
        data.push(1); // 1x8KiB CHR
        data.extend_from_slice(&[0u8; 10]); // mapper 0, iNES 1.0
        data.extend(vec![0u8; 16 * 1024]);
        data.extend(vec![0u8; 8 * 1024]);
        data
    }

    /// Ticket W2-14, and **the test whose absence let the bug ship**: a
    /// `StepFrame` command must put a real `CoreEvent::Frame` on the
    /// channel. Before the fix, the step advanced the machine and rendered
    /// into the sink, then discarded it — the only send site sat inside
    /// `tick_running_with_input`'s `true` branch, unreachable while
    /// Paused — so this `recv_timeout` would time out.
    ///
    /// `EmuStepper`'s own state machine and the FM-01 panic path were both
    /// well covered; the *seam* between a command and a delivered frame
    /// was not, and that is exactly where the defect lived.
    #[test]
    fn step_frame_command_delivers_a_frame_to_the_ui() {
        let core = spawn(synthetic_nrom()).expect("synthetic NROM must spawn a core thread");
        core.cmd_tx
            .send(CoreCommand::StepFrame)
            .expect("core thread must accept a StepFrame command");

        let evt = core
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a stepped frame must be SENT to the UI, not just rendered and dropped");
        match evt {
            CoreEvent::Frame(msg) => {
                assert_eq!(
                    msg.width * msg.height * 4,
                    msg.rgba.len(),
                    "RGBA buffer must match its declared dimensions"
                );
                assert!(msg.width > 0 && msg.height > 0);
            }
            CoreEvent::Crashed(r) => panic!("expected a frame, got a crash: {}", r.message),
            CoreEvent::CanvasSnapshot(_) => panic!("expected a frame, got a canvas snapshot"),
        }

        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
    }

    /// Ticket W3-03: `FanoutSink` (named `DualSink` until ticket W4-01 added
    /// its third leg) must actually reach `FrameMsg`'s `bg_rgba` field with
    /// real content, not just a correctly-sized buffer — `LayeredFrame::new()`
    /// pre-allocates both buffers at the full frame size, so a length-only
    /// assertion would pass even if `FanoutSink::video_scanline` never
    /// forwarded to `layers` at all (the exact silent-drop-at-a-wrapper
    /// failure mode `stepper::CountingSink`'s own doc warns about for
    /// `overlay_scanline`). Mutation-verified: commenting out
    /// `FanoutSink::video_scanline`'s `self.layers.video_scanline(y,
    /// pixels)` forward makes this FAIL.
    ///
    /// Two `StepFrame`s, not one: a fresh core's first frame is the
    /// pre-render-scanline boot artifact with **zero visible scanlines
    /// drained** (`stepper.rs`'s `running_keeps_the_reported_scanline_
    /// updating` test documents this same trap) — asserting against that
    /// frame would fail for the wrong reason regardless of wiring.
    ///
    /// Only `bg_rgba` is checked for content: this synthetic NROM never
    /// enables rendering (`$2001`/PPUMASK stays 0), so every pixel is
    /// `PixelLayer::Backdrop` (routes into `bg_rgba`, per `layers.rs`'s
    /// module doc) and `sprite_rgba` legitimately stays all-transparent —
    /// asserting sprite content here would need a sprite-enabling fixture
    /// like `sprite_overlay_mode_invariant.rs`'s.
    #[test]
    fn step_frame_command_also_delivers_bg_layer_content_not_just_size() {
        let core = spawn(synthetic_nrom()).expect("synthetic NROM must spawn a core thread");
        core.cmd_tx
            .send(CoreCommand::StepFrame)
            .expect("core thread must accept a StepFrame command");
        let _boot_artifact_frame = core
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first stepped frame must be delivered");

        core.cmd_tx
            .send(CoreCommand::StepFrame)
            .expect("core thread must accept a second StepFrame command");
        let evt = core
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second stepped frame must be delivered");
        match evt {
            CoreEvent::Frame(msg) => {
                assert_eq!(msg.bg_rgba.len(), msg.rgba.len());
                assert_eq!(msg.sprite_rgba.len(), msg.rgba.len());
                assert!(
                    msg.bg_rgba.chunks_exact(4).any(|px| px[3] == 0xFF),
                    "bg layer must carry at least one opaque pixel from a real \
                     steady-state frame — an all-transparent buffer here means \
                     FanoutSink is not actually forwarding to `layers`"
                );
            }
            CoreEvent::Crashed(r) => panic!("expected a frame, got a crash: {}", r.message),
            CoreEvent::CanvasSnapshot(_) => panic!("expected a frame, got a canvas snapshot"),
        }

        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
    }

    /// Ticket W4-01, acceptance criterion 2: a stepped frame must reach
    /// `CoreHandle::frame_bundle`, not just the `FrameMsg` channel — the
    /// same "silently drop at a wrapper" failure mode `FanoutSink`'s own
    /// doc and `step_frame_command_also_delivers_bg_layer_content_not_just_size`
    /// above both guard against, now for the third leg
    /// (`bundle_writer.publish`). Mutation-verified: commenting out the
    /// `bundle_writer.publish(...)` call in `core_thread_main` leaves
    /// `frame_bundle.latest().frame_count` stuck at 0 while `FrameMsg`
    /// keeps arriving normally — this test is what would catch that.
    #[test]
    fn step_frame_command_also_publishes_a_matching_frame_bundle() {
        let core = spawn(synthetic_nrom()).expect("synthetic NROM must spawn a core thread");
        assert_eq!(
            core.frame_bundle.latest().frame_count,
            0,
            "before any frame is stepped, the reader must see the FrameBundle::empty() seed"
        );

        core.cmd_tx
            .send(CoreCommand::StepFrame)
            .expect("core thread must accept a StepFrame command");
        let evt = core
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a stepped frame must be delivered");
        let msg_frame_count = match evt {
            CoreEvent::Frame(msg) => msg.frame_count,
            CoreEvent::Crashed(r) => panic!("expected a frame, got a crash: {}", r.message),
            CoreEvent::CanvasSnapshot(_) => panic!("expected a frame, got a canvas snapshot"),
        };

        let bundle = core.frame_bundle.latest();
        assert_eq!(
            bundle.frame_count, msg_frame_count,
            "the published FrameBundle must carry the same frame_count as the \
             FrameMsg delivered for the same step"
        );
        assert_eq!(bundle.width, rf_renderer::frame::NES_WIDTH as u16);
        assert_eq!(bundle.height, rf_renderer::frame::NES_HEIGHT as u16);
        assert_eq!(
            bundle.video.len(),
            bundle.width as usize * bundle.height as usize
        );

        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
    }

    /// Ticket W4-01: "triple-buffered FrameBundle to render/debug
    /// threads" (plural) — two independent clones of the reader handle
    /// (standing in for a render thread and a debug panel) must each
    /// independently see the same published bundle, neither one
    /// interfering with the other or with the writer.
    #[test]
    fn frame_bundle_reader_can_be_cloned_for_multiple_independent_consumers() {
        let core = spawn(synthetic_nrom()).expect("synthetic NROM must spawn a core thread");
        let render_reader = core.frame_bundle.clone();
        let debug_reader = core.frame_bundle.clone();

        core.cmd_tx
            .send(CoreCommand::StepFrame)
            .expect("core thread must accept a StepFrame command");
        core.evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a stepped frame must be delivered");

        assert_eq!(
            render_reader.latest().frame_count,
            core.frame_bundle.latest().frame_count
        );
        assert_eq!(
            debug_reader.latest().frame_count,
            core.frame_bundle.latest().frame_count
        );

        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
    }

    /// Same seam for the scanline stepper — it renders a *partial* frame
    /// (the new scanline over the previous frame's content, since
    /// `FrameBuffer` has no clear step), but it must still be delivered.
    #[test]
    fn step_scanline_command_delivers_a_frame_to_the_ui() {
        let core = spawn(synthetic_nrom()).expect("synthetic NROM must spawn a core thread");
        core.cmd_tx
            .send(CoreCommand::StepScanline)
            .expect("core thread must accept a StepScanline command");

        let evt = core
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a stepped scanline must also deliver a frame to the UI");
        assert!(matches!(evt, CoreEvent::Frame(_)));

        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
    }

    /// Ticket W3-05a: `SetSpriteOverlay` must be accepted like any other
    /// command and must not itself break the ordinary step pipeline — sent
    /// immediately before a `StepFrame`, the frame must still arrive.
    #[test]
    fn set_sprite_overlay_command_does_not_disrupt_stepping() {
        let core = spawn(synthetic_nrom()).expect("synthetic NROM must spawn a core thread");
        core.cmd_tx
            .send(CoreCommand::SetSpriteOverlay(true))
            .expect("core thread must accept SetSpriteOverlay");
        core.cmd_tx
            .send(CoreCommand::StepFrame)
            .expect("core thread must still accept StepFrame afterward");

        let evt = core
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a stepped frame must still be delivered after toggling the overlay");
        assert!(matches!(evt, CoreEvent::Frame(_)));

        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
    }

    /// The negative half: a core sitting Paused with no command must NOT
    /// stream frames. Without this, "always send a frame every loop
    /// iteration" would pass the two tests above while busy-spinning and
    /// flooding the channel.
    #[test]
    fn a_paused_core_with_no_command_sends_no_frames() {
        let core = spawn(synthetic_nrom()).expect("synthetic NROM must spawn a core thread");
        // The core starts Paused (EmuStepper::from_ines_bytes).
        thread::sleep(Duration::from_millis(60));
        assert!(
            core.evt_rx.try_recv().is_err(),
            "a paused core must stay silent until commanded, not stream frames"
        );
        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
    }

    #[test]
    fn spawn_rejects_an_invalid_rom_synchronously_without_a_thread() {
        let result = spawn(vec![0u8; 4]);
        assert!(result.is_err(), "4 garbage bytes is not a valid iNES image");
        // Any NesLoadError is fine here; the point is this returns
        // synchronously rather than spawning a thread that immediately
        // crash-reports.
    }
}
