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
use std::time::Duration;

use rf_core_api::InputFrame;
use rf_nes::NesLoadError;

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
}

/// What the core thread reports back to the UI thread.
pub enum CoreEvent {
    /// A new frame is ready to paint.
    Frame(FrameMsg),
    /// FM-01: the core thread panicked, was contained, and has now
    /// halted. No further `CoreEvent`s will ever arrive on this channel
    /// after this one.
    Crashed(CoreCrashReport),
}

/// What the UI thread can ask the core thread to do (FR-DBG-004).
pub enum CoreCommand {
    Pause,
    Resume,
    StepFrame,
    StepScanline,
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
    let handle = thread::spawn(move || core_thread_main(rom, cmd_rx, evt_tx, thread_input));
    Ok(CoreHandle {
        cmd_tx,
        evt_rx,
        join_handle: handle,
        input,
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
}

fn core_thread_main(
    rom: Vec<u8>,
    cmd_rx: Receiver<CoreCommand>,
    evt_tx: Sender<CoreEvent>,
    input: Arc<SharedInputFrame>,
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

    // `run_guarded_loop` needs its own `&Sender` (to report a crash) at
    // the same time the loop body needs to *own* a sender to `send` frames
    // from inside a `move` closure — clone rather than fight the borrow.
    let frame_tx = evt_tx.clone();
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
                    stepper.latch_and_advance_frame(input.load(), &mut sink);
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
                    stepper.step_scanline(&mut sink);
                    stepped = true;
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
        let ran = stepper.tick_running_with_input(input.load(), &mut sink);
        if ran || stepped {
            let msg = FrameMsg {
                rgba: sink.to_vec(),
                width: sink.width(),
                height: sink.height(),
                frame_count: stepper.frame_count(),
                last_scanline: stepper.last_scanline(),
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
        }

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
