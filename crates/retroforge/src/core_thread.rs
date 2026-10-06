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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{mpsc, Arc, Once};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rf_core_api::InputFrame;

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
/// Everything an HD pack needs about one frame (tickets W11-05, W11-14).
///
/// **Boxed and kept together on purpose.** The two halves are meaningless
/// apart — placements say what to replace, the layer map says where a
/// replacement is allowed to paint — and `FrameMsg` travels through a
/// channel on every frame, so a pair of vectors inline would grow the
/// whole enum for every session, pack or no pack.
#[derive(Debug, Clone)]
pub struct HdFrame {
    /// Where every background and sprite tile of this frame landed.
    pub placements: Vec<rf_enhance::hd_render::Placement>,
    /// Which layer drew each pixel — the PPU's own priority answer,
    /// reused rather than re-derived. A replacement paints only where the
    /// original shows the same layer, so a sprite behind the background
    /// is masked out for free, as is a background tile with a sprite
    /// standing in front of it.
    pub layers: Vec<rf_enhance::hd_render::Layer>,
    /// Ticket W16-02: the same frame's tiles, decoded for the Upscale
    /// Studio (`crate::stepper::StudioTileCapture`) — empty unless
    /// `CoreCommand::SetStudioCapture(true)` is in effect, independent of
    /// `SetTileCapture`'s own reason for existing (a pack loaded and the
    /// studio open are two different asks; either alone should not pay
    /// for both).
    pub studio_tiles: Vec<crate::stepper::StudioTileCapture>,
}

/// A SNES session's debug memories, snapshotted on the core thread
/// (ticket W13-02b).
///
/// Copies, not borrows: project law 4 keeps the UI thread out of a
/// running core, so what the viewers decode is a snapshot that travelled
/// on the frame — exactly the shape the NES viewers' `vram`/`oam` fields
/// already have, only bigger.
#[derive(Debug, Clone)]
pub struct SnesDebugFrame {
    /// 64 KiB.
    pub vram: Vec<u8>,
    /// 512 bytes, little-endian BGR555 (`rf_snes::debug::cgram_rgb`).
    pub cgram: Vec<u8>,
    /// 544 bytes: 512 of entries plus the 32-byte high table.
    pub oam: Vec<u8>,
    /// `$2100`-`$213F`, indexed so `[n]` is `$21nn`.
    pub ppu_regs: Vec<u8>,
    /// 64 KiB of APU RAM — the third memory-view space DEBUGGER.md §3
    /// names for SNES, alongside VRAM and CGRAM.
    pub aram: Vec<u8>,
    /// Ticket W13-02c: the mode-7 matrix, for the playfield view and its
    /// camera trapezoid. Carried whatever the current BG mode is — a
    /// viewer showing the matrix of a game that has left mode 7 is
    /// showing the truth about the registers, and the panel says which
    /// mode is live.
    pub mode7: rf_snes::ppu::mode7::Mode7,
    /// Ticket W13-02c: which HDMA channels transferred on each hardware
    /// line of this frame, one bit per channel.
    pub hdma_lanes: Vec<u8>,
    /// Ticket W13-02c: the eight DSP voices.
    pub voices: Vec<rf_snes::debug::VoiceView>,
}

/// Ticket W20-13: what the scrub bar draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewindStatus {
    /// Snapshots held (how far back rewind can go, in steps).
    pub len: usize,
    /// The most it will hold.
    pub depth: usize,
    /// Running frames between snapshots.
    pub interval: u64,
    /// Compressed memory in use.
    pub bytes: usize,
}

pub struct FrameMsg {
    /// Ticket W20-13: the rewind ring's extent after this frame, `None`
    /// while rewind is off.
    pub rewind: Option<RewindStatus>,
    /// Ticket W11-02: the bytes the full-level view asked for, or `None`
    /// when no probe is armed. Peeked on this thread because only this
    /// thread can read memory without perturbing the machine.
    pub level_probe: Option<LevelProbeData>,
    /// Ticket W11-04: the bytes of the script's memory window, when one
    /// is armed. Peeked on this thread, for the same reason the level
    /// probe is.
    pub script_window: Option<Vec<u8>>,
    /// Ticket W10-01: audio-buffer fill (0.0..=1.0) at the moment this
    /// frame was produced, for the status bar's A/V sync indicator, or
    /// `None` when no audio device opened. It rides on the frame because
    /// `AudioOut` is owned by this thread and never leaves it — the UI
    /// thread reading the device directly would be exactly the
    /// cross-thread access ARCHITECTURE §3 forbids.
    pub audio_fill: Option<f32>,
    /// Ticket W11-05: where every background tile of this frame landed
    /// and what it was, or `None` when no pack is loaded.
    ///
    /// Built on the core thread because only it may read the PPU. The
    /// pack and its images stay on the UI thread — this is the small half
    /// of the pair, and shipping decoded tilesets across the channel
    /// every frame would not be.
    pub hd: Option<Box<HdFrame>>,
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
    /// Ticket W4-06a: the same frame's full 256-byte OAM
    /// (`EmuStepper::oam`, a side-effect-free `&self` read — never
    /// perturbs the core, same "read-only is a hard requirement" posture
    /// `crate::stepper::EmuStepper::peek` already documents), for
    /// `rf_debugger::oam::decode_oam`. Same "same-frame extraction" shape
    /// `bg_rgba`/`sprite_rgba` above already use (ticket W3-03) — one more
    /// field alongside them rather than a second channel. `Box`ed (not a
    /// bare `[u8; 256]`) so `CoreEvent::Frame`'s variant doesn't blow past
    /// `clippy::large_enum_variant`'s threshold — `Box<[u8; 256]>`
    /// auto-derefs to `&[u8; 256]` at every `rf_debugger::oam::decode_oam`
    /// call site, so this costs nothing at the call sites, only here.
    pub oam: Box<[u8; 256]>,
    /// Ticket W4-10b: per-channel scope traces for this frame, already
    /// reduced to a drawable envelope on THIS thread.
    ///
    /// Reduced here rather than shipped raw for the same reason
    /// `bg_rgba`/`sprite_rgba` are only cloned when someone is looking:
    /// a frame's raw per-channel audio is ~735 samples x 5 channels, and
    /// the panel draws a few dozen points. Empty when the scopes are
    /// closed.
    pub audio_traces: Vec<rf_debugger::audio_scope::ScopeTrace>,
    /// Ticket W4-06d: the PPU's nametable VRAM and palette RAM, read via
    /// `NesBus::vram()`/`palette()` — plain non-observing borrows, so
    /// snapshotting them here cannot perturb A12 edge timing (see
    /// `rf_nes::Ppu::vram`'s doc for why that matters).
    pub vram: Box<[u8; 0x1000]>,
    /// Ticket W13-02b: the SNES memories the viewer column needs, or
    /// `None` on a NES session **and** whenever nobody is looking.
    ///
    /// Gated for a real reason rather than symmetry: SNES VRAM alone is
    /// 64 KiB, so cloning it on every frame of every session would be
    /// ~3.8 MB/s of copying for a panel that is usually closed — the same
    /// trade `SetLayerExtraction` already makes for its two 240 KB
    /// buffers. `CoreCommand::SetSnesDebugCapture` is the switch.
    pub snes: Option<Box<SnesDebugFrame>>,
    /// The CPU register file at the end of this frame, typed per CPU
    /// family, for the register readout (ticket W13-02i). One field for
    /// both consoles, read through `EmulatorCore::state_view`; the UI
    /// thread matches on the variant and never reaches into a core.
    /// Boxed for the same `clippy::large_enum_variant` reason `oam` is:
    /// the value is a couple of dozen bytes, but this message is already
    /// at the lint's threshold and every inline field tips it.
    pub cpu_regs: Box<rf_core_api::CpuRegs>,
    pub palette_ram: Box<[u8; 32]>,
    /// Ticket W4-06b: the same frame's 2 KiB WRAM snapshot
    /// (`EmuStepper::wram_snapshot`, side-effect-free — same "read-only is
    /// a hard requirement" posture as `oam` above), for
    /// `rf_debugger::memory_view`'s memory-hex panel. `Box`ed for the same
    /// `clippy::large_enum_variant` reason `oam` already is.
    pub wram: Box<[u8; 0x0800]>,
    /// Ticket W4-06b: the same frame's cartridge PRG-RAM window
    /// (`EmuStepper::prg_ram`, side-effect-free), the memory viewer's
    /// second live range — see `EmuStepper::prg_ram`'s own doc for why one
    /// range alone (WRAM) isn't enough.
    pub prg_ram: Box<[u8; 0x2000]>,
    /// Ticket W16-13: PPUCTRL bit 5 for this frame — 8 or 16, the current
    /// sprite height (`EmuStepper::sprite_height_px`). The smallest
    /// additive field this message had no other way to carry: the Diorama
    /// billboard footprint (`enhanced_view::compose_diorama`) must not
    /// assume every sprite is 8x8 when the game itself is running in 8x16
    /// mode. `8` on a SNES session or when no core is loaded, the same
    /// "ordinary-case default" `EmuStepper::sprite_height_px` itself uses.
    pub sprite_height_px: u8,
    /// Ticket W16-14: this frame's [`rf_core_api::CoreEvent::Mode7`]
    /// payload, extracted from the frame's `FrameBundle::events` before
    /// the bundle is handed to `bundle_writer.publish` (which moves it) —
    /// `None` on any frame that did not emit one (BG mode not 7, or
    /// `EventMask::MODE7` not subscribed). Carried as its own field
    /// rather than a generic `events: Vec<CoreEvent>` on `FrameMsg`
    /// because Mode 7's per-frame consumer (`crate::app::
    /// refresh_diorama_render`) needs only this one variant, and forwarding
    /// the whole event list would ship every debug-viewer event
    /// (`ScrollWrite`, `MemWatch`, ...) to a consumer that reads none of
    /// them.
    pub mode7: Option<Box<rf_core_api::Mode7Frame>>,
}

/// Ticket W14-20 defect 1: the most `CoreEvent::Frame`s the core thread
/// will let sit unconsumed on `evt_tx` before it starts dropping new ones
/// instead of sending them. Before this existed the channel was
/// `mpsc::channel()` — unbounded — and a UI that stopped draining it (the
/// stall documented in defect 2) let the core keep publishing ~250 KB
/// `FrameMsg`s at 60 Hz forever: measured 15 MB/s RSS growth, 394 MB ->
/// 2826 MB in ~3 min on m4max, 2026-09-17. `2` (not `1`) leaves one frame
/// of slack for the ordinary race between the producer incrementing and
/// the consumer's next drain, without reopening the unbounded growth this
/// exists to close.
pub const MAX_PENDING_FRAMES: usize = 2;

/// What the core thread reports back to the UI thread.
pub enum CoreEvent {
    /// A new frame is ready to paint. Boxed (ticket W16-14): `FrameMsg`
    /// grew past `clippy::large_enum_variant`'s threshold the moment
    /// `mode7` (an `Option<Box<Mode7Frame>>`, already boxed on its own
    /// terms) pushed the struct's inline size over it — the same
    /// "everything else is already boxed for this reason" pattern
    /// `FrameMsg::oam`/`cpu_regs`/`wram`/`prg_ram` document, extended one
    /// level up since a per-FIELD box could not buy back enough this
    /// time.
    Frame(Box<FrameMsg>),
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
    /// Ticket W11-03 (FR-ENH-004): what the widescreen policy decided,
    /// per background, and WHY when the answer was no.
    ///
    /// **Refusals are surfaced, not swallowed.** bsnes-hd's lesson is
    /// that widening the wrong layer looks like a bug in the game: a
    /// status bar smeared across the margins, a full-screen image
    /// repeated. So a layer the policy declines keeps its 4:3 width AND
    /// says so, rather than the user seeing a narrower picture than they
    /// asked for with no explanation. Sent only when the decision
    /// CHANGES, not every frame.
    WidescreenDecisions([Option<&'static str>; 4]),
}

/// Which bytes the full-level view needs read each frame (ticket W11-02).
///
/// (This previously carried `CoreCommand`'s doc comment, orphaned when
/// this struct was inserted beneath it — RF-L-11's pattern exactly.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelProbe {
    /// Addresses `rf_enhance::level_view::live_camera` will read, derived
    /// from the profile's `[camera]` spec.
    pub addrs: Vec<u32>,
    /// Start of the profile's `[entities].table`.
    pub table_addr: u32,
    /// How many bytes of it to read.
    pub table_len: usize,
}

/// The bytes a [`LevelProbe`] asked for, as of this frame.
#[derive(Debug, Clone, Default)]
pub struct LevelProbeData {
    /// Parallel to `LevelProbe::addrs`.
    pub values: Vec<u8>,
    /// The entity table.
    pub table: Vec<u8>,
}

/// A widescreen request: how wide, and the profile's per-layer policies.
///
/// **The POLICIES travel, not the decisions.** A decision depends on
/// where the layer is scrolled to, which changes every frame, so the core
/// thread re-decides with live geometry rather than the UI deciding once
/// against a stale view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidescreenRequest {
    /// Output width in dots. Clamped by the PPU to `MAX_WIDTH`.
    pub width: usize,
    /// The profile's `[widescreen]` policies.
    pub policies: rf_enhance::widescreen::WidescreenPolicies,
}

/// What the UI thread can ask the core thread to do (FR-DBG-004).
///
/// Not `Clone` or `Debug`: `ArmTrace` carries a `TraceProducer`, which is
/// neither.
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
    /// Ticket W11-01 (FR-ENH-002): temporal de-flicker on/off.
    ///
    /// A SECOND enhancement command, and until W11-01 there was exactly
    /// one — which is the whole reason `GameSettings::deflicker` round-
    /// tripped to disk and changed nothing anyone could see. Like
    /// `SetSpriteOverlay` this only ever affects what is DRAWN: the
    /// reconstruction runs over a shared reference to the accuracy-exact
    /// frame and cannot perturb the simulation (`rf_enhance::
    /// sprite_historian::SpriteHistorian::observe` takes `&[PpuPixel]`,
    /// never `&mut`).
    SetDeflicker(bool),
    /// Ticket W11-03 (FR-ENH-004): decoded widescreen on/off.
    ///
    /// `None` turns it off and restores the accuracy path exactly — the
    /// core stops reaching the widening code at all rather than widening
    /// to 256 (law 6).
    SetWidescreen(Option<WidescreenRequest>),
    /// Ticket W11-05: record the tiles the PPU draws, for HD packs.
    ///
    /// **Pay-for-use, and that is why it is a command rather than always
    /// on**: a frame draws ~7680 background tiles, and building and
    /// shipping that list every frame for a session with no pack loaded
    /// would be pure waste.
    SetTileCapture(bool),
    /// Ticket W16-02: record this frame's tiles for the Upscale Studio.
    ///
    /// Deliberately a SEPARATE command from `SetTileCapture` rather than
    /// the studio window reusing that one: a pack loaded for playback and
    /// the studio open to capture new tiles are two independent reasons
    /// to pay the recording cost, and turning the studio off must not
    /// silently stop a loaded pack from compositing (or vice versa). Both
    /// still gate the SAME underlying `stepper.set_tile_capture` — see
    /// the handler below — so a session with neither active pays nothing,
    /// exactly as `SetTileCapture`'s own doc promises.
    SetStudioCapture(bool),
    /// Ticket W11-02: which bytes the full-level view needs each frame.
    ///
    /// A BOUNDED probe, not "send the UI some RAM". The profile declares
    /// which addresses matter — `[camera].x/.y` (at most four bytes) and
    /// `[entities].table` — so the app computes that list once and asks
    /// for exactly it. `None` switches the probe off and costs nothing.
    ///
    /// The split exists because the two halves live on different threads
    /// and cannot swap: only the core thread can peek without perturbing
    /// state, and only the UI thread has the GPU to composite with.
    SetLevelProbe(Option<LevelProbe>),
    /// Ticket W11-04: the memory window a loaded Lua script sees.
    ///
    /// `(base, len)`, or `None` when no script is loaded. Bounded and
    /// declared, like `SetLevelProbe` — a script gets the window the
    /// shell publishes and nothing else, which is what makes
    /// `MemoryWindow::read_u8` returning 0 outside it a sandbox rather
    /// than an accident.
    SetScriptWindow(Option<(u32, usize)>),
    /// Ticket W3-03a: opt into (or out of) per-frame layer extraction —
    /// the BG-only/sprite-only split `FanoutSink` feeds
    /// [`rf_renderer::LayeredFrame`], plus the two ~240 KB buffer clones
    /// that carry it to the UI in [`FrameMsg`].
    ///
    /// **Off by default**, and that is the whole point: W3-03 wired the
    /// split unconditionally, so every frame paid for it whether or not
    /// the "Layers (debug)" window was open. Measured at the time and
    /// found to fit inside W2-18's 16.64 ms budget (60.0 fps debug, 60.2
    /// release) — not a regression, but headroom spent on a window nobody
    /// was looking at.
    ///
    /// Exactly the same shape and the same principle as
    /// [`CoreCommand::SetEventMask`], which the debugger's event viewer
    /// sends as it opens and closes: DEBUGGER.md §6's "closed panels
    /// register no event subscriptions", one layer over.
    ///
    /// A pure state toggle: it does not itself produce a
    /// [`CoreEvent::Frame`], so the first frame carrying layers is the
    /// next one the core produces anyway (≤16.6 ms later at 60 Hz).
    SetLayerExtraction(bool),
    /// Ticket W13-02b: capture the SNES debug memories on each frame.
    ///
    /// Off by default and driven by whether a SNES-capable debug panel is
    /// docked, for the same pay-for-use reason `SetLayerExtraction` exists
    /// (DEBUGGER.md §6): 64 KiB of VRAM plus 64 KiB of ARAM per frame is
    /// not something to pay for while nobody is looking.
    SetSnesDebugCapture(bool),
    /// Ticket W4-03e: ask for a `CoreEvent::CanvasSnapshot` of the current
    /// scene's stitched canvas (see that variant's doc). Also flushes the
    /// canvas accumulator's cache (`CanvasAccumulator::flush`) — piggy-
    /// backing persistence on the same user-driven cadence (whenever the
    /// UI is actually showing/refreshing the Ultrawide view) rather than
    /// every frame.
    RequestCanvasSnapshot,
    /// Ticket W4-10a: arm the trace, handing the core thread the
    /// producing half of the transport (`crate::trace_capture::channel`).
    ///
    /// Sent by the trace viewer as it starts a capture, exactly like
    /// [`CoreCommand::SetEventMask`] and
    /// [`CoreCommand::SetLayerExtraction`] before it — DEBUGGER.md §6's
    /// "closed panels register no event subscriptions", a third time. The
    /// producer is `Box`ed for the same reason `FrameMsg`'s bundle is:
    /// it makes this variant no bigger than the others, so an untraced
    /// session does not pay for the enum being able to carry one.
    ArmTrace(Box<crate::trace_capture::TraceProducer>),
    /// Ticket W4-10a: stop tracing and drop the producer, which closes
    /// the file writer's channel and lets it finish its lz4 frame.
    DisarmTrace,
    /// Ticket W4-11: write the current machine into a save-state slot,
    /// thumbnail included.
    ///
    /// **The core thread does the whole save, and that is why this
    /// carries a directory rather than returning bytes.** It is the only
    /// thread holding both halves: the machine (for the container) and
    /// this frame's RGBA framebuffer (for the thumbnail). Handing the
    /// container back to the UI would need a new `CoreEvent` variant, and
    /// `rf-core-api` is outside this ticket's write scope — but it would
    /// also split one atomic user action across two threads, where a
    /// crash between halves leaves a listed slot with no picture or a
    /// picture with no slot.
    SaveStateToSlot {
        dir: std::path::PathBuf,
        stem: String,
    },
    /// Ticket W4-11: apply a container the UI already decoded.
    ///
    /// Decoding happens UI-side because that is where the migration
    /// warnings must surface (FRONTEND_UI §3.2's last clause) — by the
    /// time it reaches here the question "did this state have to be
    /// migrated?" has been asked and answered.
    ApplyState(Box<rf_state::Container>),
    /// Ticket W20-13: turn rewind on (snapshot every `interval` running
    /// frames, keep `depth` of them) or off (`None`, which frees the ring).
    SetRewind(Option<rf_state::RewindConfig>),
    /// Ticket W20-13: step back one snapshot and show it — load the state
    /// through the same `load_state` as Load State (so the simulation is
    /// never altered any other way), then render one frame from it and
    /// stay paused, exactly as `StepFrame` does.
    RewindStep,
    /// Ticket W4-10b: turn per-channel audio capture on or off. Off by
    /// default, so a session that never opens the scopes pays nothing
    /// (DEBUGGER.md §6).
    SetAudioChannelCapture(bool),
    /// Ticket W15-06 (FR-ENH-008): disable frame pacing entirely while
    /// held — the App-hotkey fast-forward. `false` runs the loop flat-out
    /// (`crate::pacer::FramePacer::set_enabled`'s own doc); `true`
    /// restores normal pacing. Never touches the machine itself — see
    /// `crate::pacer`'s module doc on why pacing cannot affect
    /// determinism.
    SetPacingEnabled(bool),
    /// Ticket W4-06a: widen/narrow which `CoreEvent`s this session emits
    /// (`EmuStepper::set_event_mask`) — the debugger's event-viewer panel
    /// sends this as it opens/closes (DEBUGGER.md §6: "closed panels
    /// register no event subscriptions"). Like `SetSpriteOverlay`, a pure
    /// state toggle: does not itself produce a `CoreEvent::Frame`.
    /// `EmuStepper::set_event_mask` always re-asserts
    /// `crate::stepper::CAMERA_BASELINE_EVENT_MASK` regardless of what is
    /// requested here, so this can never starve the enhanced-camera
    /// pipeline (see that function's own doc).
    SetEventMask(rf_core_api::EventMask),
    /// Ticket W13-02d: one out-of-band bus write from the memory editor.
    ///
    /// A command, like everything else here, because law 4 forbids the UI
    /// thread touching core state — and the app only sends it while the
    /// core is PAUSED, so the write lands at a boundary the user can see
    /// rather than at whatever cycle the queue happened to drain on.
    PokeBus {
        addr: u16,
        value: u8,
    },
    /// Ticket W13-02e: replace the core's memory watchpoints.
    ///
    /// A command rather than a direct call because the core lives on its
    /// own thread and law 4 forbids the UI thread touching it — the same
    /// reason `SetEventMask` is a command. The whole set travels each
    /// time: a watch table is at most `rf_core_api::MAX_WATCHES` entries,
    /// so sending the set is cheaper than reasoning about deltas, and it
    /// makes "what is armed" one value rather than a history.
    SetWatches(Vec<rf_core_api::MemWatch>),
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
pub fn spawn(rom: Vec<u8>) -> Result<CoreHandle, crate::stepper::OpenError> {
    spawn_with_waker(rom, None)
}

/// Same as [`spawn`], plus a waker the core thread calls after every
/// `CoreEvent::Frame` it successfully sends (ticket W14-20 defect 2).
///
/// **Why this exists at all**: `pump_core_events` already calls
/// `ctx.request_repaint()` every UI update while the core is running, and
/// on 2026-09-17 the main thread still sat in AppKit's
/// `_DPSBlockUntilNextEventMatchingListInMode` in 100% of samples while
/// the core kept emulating normally — i.e. something swallowed that
/// repaint request rather than waking the event loop. The root cause is
/// still open (plan.json W14-20's notes), so this is a defence that holds
/// regardless of it: the producer wakes the consumer directly, instead of
/// relying solely on a request the consumer's own event loop might not
/// act on. `egui::Context::request_repaint`'s own doc covers exactly this
/// case: "If called from outside the UI thread, the UI thread will wake
/// up and run, provided the egui integration has set that up via
/// `Self::set_request_repaint_callback` (this will work on `eframe`)"
/// (`egui-0.35.0/crates/egui/src/context.rs`, the doc comment immediately
/// above `fn request_repaint`) — and `Context` itself is proven `Send +
/// Sync` by that same file's own `context_impl_send_sync` test, so the
/// closure the caller hands in may call it straight from this core
/// thread.
///
/// `None` (what [`spawn`] passes) means no waker — used by every existing
/// test in this module, none of which needs one.
pub fn spawn_with_waker(
    rom: Vec<u8>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
) -> Result<CoreHandle, crate::stepper::OpenError> {
    // Ticket W11-12: whichever console this image is. This line called
    // `from_ines_bytes` unconditionally until now, which is why a SNES
    // ROM was refused as "only NES ROMs are supported" — while the
    // library scanner
    // identified it correctly as `Recognized { console: Snes }`, listed
    // it with a Play button, and offered a SNES filter. The product
    // advertised a feature it could not perform.
    //
    // Validate up front so a bad ROM is a synchronous `Result`, not a
    // channel message the UI has to poll for — the same pattern
    // `rom_open::load_rom_bytes` uses for the file-dialog path.
    EmuStepper::open(&rom)?;

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
    // Ticket W14-20 defect 1: shared between this handle and the core
    // thread's send site. The core thread increments it before a send and
    // the UI thread (`app::pump_core_events`) decrements it for every
    // `CoreEvent::Frame` it drains — see `MAX_PENDING_FRAMES`'s doc for
    // why this exists.
    let pending_frames = Arc::new(AtomicUsize::new(0));
    let thread_pending_frames = Arc::clone(&pending_frames);
    let handle = thread::spawn(move || {
        core_thread_main(
            rom,
            rom_sha256,
            cmd_rx,
            evt_tx,
            thread_input,
            bundle_writer,
            thread_pending_frames,
            waker,
        );
    });
    Ok(CoreHandle {
        cmd_tx,
        evt_rx,
        join_handle: handle,
        input,
        frame_bundle: bundle_reader,
        pending_frames,
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
    /// Ticket W14-20 defect 1: how many `CoreEvent::Frame`s are currently
    /// sitting unconsumed on `evt_rx`. The core thread increments this
    /// before each send (refusing to send at all once it reaches
    /// [`MAX_PENDING_FRAMES`]); whoever drains `evt_rx` — normally
    /// `app::pump_core_events`, or a test reading `evt_rx` directly — must
    /// `fetch_sub(1, Ordering::AcqRel)` for every `CoreEvent::Frame` it
    /// receives, or this count only ever grows and every frame after the
    /// first `MAX_PENDING_FRAMES` gets silently dropped for the rest of
    /// the session.
    pub pending_frames: Arc<AtomicUsize>,
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
/// The frame the UI will actually see (ticket W11-01, FR-ENH-002).
///
/// With de-flicker **off** this is exactly `sink.to_vec()` — the frame
/// `FrameBuffer` already composited, byte for byte. The feature costs
/// nothing when nobody asked for it, which is law 6's requirement and not
/// merely an optimisation.
///
/// With it **on**, the frame is rebuilt:
///
/// 1. `SpriteHistorian::observe` reconstructs the accuracy-exact INDEXED
///    frame, redrawing flicker candidates it recognises from cache. It
///    takes `&[PpuPixel]` — a shared reference, never `&mut` — which is
///    the structural half of the mode-invariant proof: nothing here can
///    perturb the simulation, and `FrameBundle::video` stays exactly as
///    the PPU produced it.
/// 2. The reconstruction is converted to rgba through the same palette
///    the original pipeline uses.
/// 3. **The dropped-sprite overlay is re-composited on top**, if it was
///    captured. This step is the one that is easy to miss and expensive
///    to get wrong: the overlay is painted into rgba inside
///    `FrameBuffer`, so rebuilding from indexed pixels throws it away.
///    Two enhancements aimed at the same artifact would then have
///    silently cancelled each other — the user switches on both, and one
///    of them stops working with nothing to say so.
fn display_rgba(
    historian: &mut rf_enhance::sprite_historian::SpriteHistorian,
    bundle: &rf_core_api::FrameBundle,
    sink: &rf_renderer::FrameBuffer,
    overlay: Option<&[rf_core_api::OverlayPixel]>,
) -> Vec<u8> {
    if !historian.enabled() {
        return sink.to_vec();
    }
    let (w, h) = (sink.width(), sink.height());
    let reconstructed = historian.observe(&bundle.video, w as u16, h as u16);
    let mut rgba = rf_renderer::original_rgba_from_indexed(&reconstructed, w as u32, h as u32);
    if let Some(overlay) = overlay {
        for (i, pixel) in overlay.iter().enumerate().take(w * h) {
            if !pixel.opaque {
                continue;
            }
            let [r, g, b] = rf_renderer::palette_index_to_rgb(pixel.palette_index);
            let o = i * 4;
            rgba[o] = r;
            rgba[o + 1] = g;
            rgba[o + 2] = b;
            rgba[o + 3] = 0xFF;
        }
    }
    rgba
}

struct FanoutSink<'a> {
    frame: &'a mut rf_renderer::FrameBuffer,
    /// `None` when layer extraction is off (ticket W3-03a) — the split is
    /// simply not performed, rather than performed into a buffer nobody
    /// reads.
    layers: Option<&'a mut rf_renderer::LayeredFrame>,
    bundle: &'a mut rf_core_api::FrameBundleBuilder,
    /// Ticket W2-05: the audio path. `None` when the chain could not be
    /// built at all, which is not fatal — a silent emulator is far better
    /// than one that refuses to start because a machine has no sound card.
    audio: Option<&'a mut crate::audio_out::AudioOut>,
    /// Ticket W11-01: the dropped-sprite overlay, captured as PIXELS
    /// rather than only painted into `frame`.
    ///
    /// `None` unless temporal de-flicker is on, on the same "only pay
    /// when someone is looking" rule W3-03a applied to layer extraction.
    /// It exists because the two enhancements meet in different spaces:
    /// the overlay is composited into rgba inside `FrameBuffer`, while
    /// de-flicker reconstructs INDEXED pixels — so rebuilding the frame
    /// from reconstructed indices would silently erase an overlay the
    /// user had also switched on. Capturing the overlay lets it be
    /// re-composited instead of quietly dropped.
    overlay: Option<&'a mut Vec<rf_core_api::OverlayPixel>>,
}

impl rf_core_api::CoreSink for FanoutSink<'_> {
    fn video_scanline(&mut self, y: u16, pixels: &[rf_core_api::PpuPixel]) {
        self.frame.video_scanline(y, pixels);
        if let Some(layers) = self.layers.as_mut() {
            layers.video_scanline(y, pixels);
        }
        self.bundle.video_scanline(y, pixels);
    }

    fn overlay_scanline(&mut self, y: u16, pixels: &[rf_core_api::OverlayPixel]) {
        self.frame.overlay_scanline(y, pixels);
        if let Some(buffer) = self.overlay.as_deref_mut() {
            let row = y as usize;
            let width = self.frame.width();
            let start = row * width;
            if start + width <= buffer.len() {
                for (x, pixel) in pixels.iter().enumerate().take(width) {
                    buffer[start + x] = *pixel;
                }
            }
        }
    }

    fn audio(&mut self, samples: &[i16]) {
        self.frame.audio(samples);
        self.bundle.audio(samples);
        if let Some(audio) = self.audio.as_deref_mut() {
            audio.push(samples);
        }
    }

    fn event(&mut self, ev: rf_core_api::CoreEvent) {
        self.frame.event(ev.clone());
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

// Ticket W14-20 added the trailing `pending_frames`/`waker` pair (defects
// 1 and 2) to what was already the widest call in this module — same
// shape as `library.rs`/`hd_render.rs`'s existing allows for the same
// lint, and splitting a single private, single-call-site function's
// params into a struct here would be a rename exercise, not a real
// simplification.
#[allow(clippy::too_many_arguments)]
fn core_thread_main(
    rom: Vec<u8>,
    rom_sha256: String,
    cmd_rx: Receiver<CoreCommand>,
    evt_tx: Sender<CoreEvent>,
    input: Arc<SharedInputFrame>,
    mut bundle_writer: rf_core_api::TripleBufferWriter<rf_core_api::FrameBundle>,
    pending_frames: Arc<AtomicUsize>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    install_panic_capture_hook();

    // Re-parse inside the thread too: `spawn`'s pre-check already proved
    // this succeeds, but re-deriving state here (rather than trying to
    // move a already-validated `EmuStepper` across the `thread::spawn`
    // boundary awkwardly) keeps this thread self-contained. `expect` is
    // safe: `spawn` already returned `Err` and never reached here if this
    // would fail.
    let mut stepper = EmuStepper::open(&rom).expect("rom already validated by spawn()");
    // Ticket W11-01: temporal de-flicker (FR-ENH-002). Lives on the CORE
    // thread because that is where the accuracy-exact indexed frame is,
    // and dies with it — it holds per-identity history that means nothing
    // across a ROM change.
    let mut historian = rf_enhance::sprite_historian::SpriteHistorian::new();
    // Allocated only while de-flicker is on. `None` is the whole cost of
    // the feature being off: no buffer, no per-scanline copy, nothing.
    let mut overlay_capture: Option<Vec<rf_core_api::OverlayPixel>> = None;
    // Ticket W11-02: `None` until the full-level view asks for something.
    let mut level_probe: Option<LevelProbe> = None;
    // Ticket W11-04: `None` until a script is loaded.
    let mut script_window: Option<(u32, usize)> = None;
    let mut sink = rf_renderer::FrameBuffer::new();
    // Ticket W3-03: same-frame BG/sprite layer extraction, fed alongside
    // `sink` via `FanoutSink` at every call site below.
    let mut layers = rf_renderer::LayeredFrame::new();
    // Ticket W3-03a: off until the UI asks, so a closed debug window costs
    // nothing on the frame path (see `CoreCommand::SetLayerExtraction`).
    let mut layers_enabled = false;
    // Widescreen (W11-03). `None` is off, and off means the core never
    // reaches its widening path at all.
    let mut widescreen: Option<WidescreenRequest> = None;
    let mut hd_capture = false;
    // Ticket W16-02: independent from `hd_capture` — see
    // `CoreCommand::SetStudioCapture`'s doc.
    let mut studio_capture = false;
    // Ticket W13-02b: whether to snapshot the SNES debug memories each
    // frame. Off until a panel asks — 128 KiB per frame is not a cost to
    // pay while nobody is looking.
    let mut snes_debug_capture = false;
    let mut last_decisions: Option<[Option<&'static str>; 4]> = None;
    // Ticket W4-10a: `None` is the shipped, untraced state. The run loop
    // below tests this once per frame and takes the ordinary path — the
    // whole cost of the debugger's tracing existing, on a session that is
    // not using it (DEBUGGER.md §6, and `benches/debugger_idle.rs`).
    let mut trace: Option<Box<crate::trace_capture::TraceProducer>> = None;
    // Ticket W4-10b: off by default (DEBUGGER.md §6's pay-for-use rule).
    let mut audio_capture = false;
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
    // Ticket W2-05: the audio path, and with it the audio clock. `open`
    // exists only with the `audio` feature (cpal); without it, or when
    // there is no device, the chain still runs headless so the same code
    // is exercised, and `crate::pacer` stays in charge of frame timing —
    // see `crate::audio_out`'s module doc.
    let mut audio = crate::audio_out::open_audio_out();
    // Ticket W20-13: the rewind ring lives HERE, on the thread that owns
    // the machine — snapshots are taken and restored without the machine
    // ever leaving this thread (ARCHITECTURE §6).
    let mut rewind: Option<rf_state::RewindRing> = None;
    // Ticket W14-20 defect 1: how many frames this thread has dropped
    // (see the `pending_frames` check at the send site below) since the
    // last time it logged about it, and when that last log happened.
    // Logged at most once a second — at 60 fps a stalled UI would
    // otherwise produce a debug line every ~16.6 ms, which is its own
    // performance problem and defeats the point of fixing one.
    let mut dropped_frames_since_log: u64 = 0;
    let mut last_drop_log = Instant::now();
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
                            layers: layers_enabled.then_some(&mut layers),
                            bundle: &mut bundle_builder,
                            audio: audio.as_mut(),
                            overlay: overlay_capture.as_mut(),
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
                        layers: layers_enabled.then_some(&mut layers),
                        bundle: &mut bundle_builder,
                        audio: audio.as_mut(),
                        overlay: overlay_capture.as_mut(),
                    });
                    stepped = true;
                }
                CoreCommand::SetSpriteOverlay(enabled) => {
                    stepper.set_sprite_overlay_enabled(enabled);
                }
                CoreCommand::SetLevelProbe(probe) => {
                    level_probe = probe;
                }
                CoreCommand::SetScriptWindow(window) => {
                    script_window = window;
                }
                CoreCommand::SetDeflicker(enabled) => {
                    historian.set_enabled(enabled);
                    overlay_capture = enabled.then(|| {
                        vec![
                            rf_core_api::OverlayPixel {
                                palette_index: 0,
                                opaque: false,
                            };
                            sink.width() * sink.height()
                        ]
                    });
                }
                CoreCommand::SetSnesDebugCapture(enabled) => {
                    snes_debug_capture = enabled;
                }
                CoreCommand::SetLayerExtraction(enabled) => {
                    layers_enabled = enabled;
                }
                CoreCommand::SetTileCapture(on) => {
                    hd_capture = on;
                    stepper.set_tile_capture(hd_capture || studio_capture);
                }
                CoreCommand::SetStudioCapture(on) => {
                    studio_capture = on;
                    stepper.set_tile_capture(hd_capture || studio_capture);
                }
                CoreCommand::SetWidescreen(request) => {
                    // **The frame buffer has to grow with the picture.**
                    // `FrameBuffer::video_scanline` truncates to its own
                    // width (`take(self.width)`), so a core emitting 400
                    // dots into a 256-wide buffer loses the margins in
                    // silence — the enhancement would look like it did
                    // nothing at all.
                    let height = sink.height();
                    let width = request.as_ref().map_or(rf_snes::ppu::WIDTH, |r| {
                        r.width.clamp(rf_snes::ppu::WIDTH, rf_snes::ppu::MAX_WIDTH)
                    });
                    if width != sink.width() {
                        sink = rf_renderer::FrameBuffer::with_size(width, height);
                    }
                    if let Some(buf) = overlay_capture.as_mut() {
                        buf.resize(
                            width * height,
                            rf_core_api::OverlayPixel {
                                palette_index: 0,
                                opaque: false,
                            },
                        );
                    }
                    widescreen = request;
                    if widescreen.is_none() {
                        stepper.set_widescreen_off();
                        // Force a re-send if it is switched back on, so the
                        // UI never shows a stale set of reasons.
                        last_decisions = None;
                    }
                }
                CoreCommand::SetEventMask(mask) => {
                    stepper.set_event_mask(mask);
                }
                CoreCommand::PokeBus { addr, value } => {
                    stepper.poke_bus(addr, value);
                }
                CoreCommand::SetWatches(watches) => {
                    // The refusal count is dropped here deliberately: the
                    // UI already knows how many it sent and what
                    // `MAX_WATCHES` is, so it reports the overflow without
                    // a round trip.
                    let _ = stepper.set_watches(&watches);
                }
                CoreCommand::RequestCanvasSnapshot => {
                    canvas_accum.flush();
                    let snapshot = canvas_accum.current_canvas().unwrap_or_default();
                    if frame_tx.send(CoreEvent::CanvasSnapshot(snapshot)).is_err() {
                        return LoopControl::Stop; // UI thread hung up.
                    }
                }
                CoreCommand::ArmTrace(producer) => {
                    trace = Some(producer);
                }
                CoreCommand::DisarmTrace => {
                    // Dropping the producer closes the file writer's
                    // channel; the writer then finishes its lz4 frame,
                    // which is what makes the file readable at all.
                    trace = None;
                }
                CoreCommand::SaveStateToSlot { dir, stem } => {
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    match stepper.save_state(timestamp) {
                        Ok(container) => {
                            let Ok(bytes) = container.encode() else {
                                continue;
                            };
                            let rgba = sink.to_vec();
                            let (w, h) = (
                                u32::try_from(sink.width()).unwrap_or(0),
                                u32::try_from(sink.height()).unwrap_or(0),
                            );
                            if let Some(slot) = crate::state_slots::SlotId::from_stem(&stem) {
                                let _ = crate::state_slots::save(
                                    &dir,
                                    slot,
                                    &bytes,
                                    Some((&rgba, w, h)),
                                );
                            }
                        }
                        Err(_) => { /* reported by the UI's own status line */ }
                    }
                }
                CoreCommand::ApplyState(container) => {
                    let _ = stepper.load_state(&container);
                }
                CoreCommand::SetRewind(config) => {
                    rewind = config.map(rf_state::RewindRing::new);
                }
                CoreCommand::RewindStep => {
                    let restored = rewind
                        .as_mut()
                        .and_then(|ring| ring.step_back().ok().flatten())
                        .and_then(|(_, bytes)| rf_state::Container::decode_default(&bytes).ok())
                        .is_some_and(|(container, _)| stepper.load_state(&container).is_ok());
                    if restored {
                        stepper.latch_and_advance_frame(
                            input.load(),
                            &mut FanoutSink {
                                frame: &mut sink,
                                layers: layers_enabled.then_some(&mut layers),
                                bundle: &mut bundle_builder,
                                audio: audio.as_mut(),
                                overlay: overlay_capture.as_mut(),
                            },
                        );
                        stepper.pause();
                        stepped = true;
                    }
                }
                CoreCommand::SetAudioChannelCapture(on) => {
                    stepper.set_audio_channel_capture(on);
                    audio_capture = on;
                }
                // Ticket W15-06 (FR-ENH-008): the fast-forward hotkey,
                // held. `pacer.set_enabled` alone is not the whole story —
                // see the branch below, which also has to skip the
                // audio-clock wait.
                CoreCommand::SetPacingEnabled(enabled) => pacer.set_enabled(enabled),
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
        } else if !pacer.is_enabled() {
            // Ticket W15-06 fast-forward: the audio-clock branch below
            // does not consult `pacer` at all (it waits on the device's
            // own ring instead), so `SetPacingEnabled(false)` alone would
            // leave fast-forward capped at the audio ring's drain rate in
            // an `audio`-feature build. Skip both wait mechanisms here.
            pacer.resync();
        } else if audio
            .as_ref()
            .is_some_and(crate::audio_out::AudioOut::is_clock)
        {
            // Ticket W2-05, ARCHITECTURE §8's audio-driven pacing: run a
            // frame only once the device has drained the ring back to its
            // target. This is a clock that cannot drift against itself,
            // which the wall-clock deadline below structurally can.
            while audio
                .as_ref()
                .is_some_and(crate::audio_out::AudioOut::should_wait)
            {
                thread::sleep(Duration::from_millis(1));
            }
            pacer.resync();
        } else {
            let delay = pacer.next_delay(Instant::now());
            if !delay.is_zero() {
                thread::sleep(delay);
            }
        }
        // Ticket W4-10a. Two whole loops rather than a per-instruction
        // branch inside one: an untraced session must not pay for tracing
        // being possible, which is DEBUGGER.md §6's pay-for-use rule and
        // what `benches/debugger_idle.rs` measures. The test below is the
        // entire per-frame cost.
        // **Widescreen decides EVERY FRAME**, because `auto` asks where
        // the layer is scrolled to and that changes as the player moves.
        // Deciding once when the toggle was flipped would freeze a HUD's
        // verdict onto a scrolling layer, or the reverse.
        if let Some(req) = &widescreen {
            if let Some(views) = stepper.bg_layer_views() {
                let decisions = req.policies.decide_all(views);
                let bg = std::array::from_fn(|i| decisions[i].widens());
                // `clip` is the one policy that keeps sprites inside the
                // 4:3 frame; `safe` and `unsafe` both let them into the
                // margins, differing in how much of a sprite must be
                // inside to qualify.
                let obj = req.policies.obj != rf_enhance::widescreen::ObjPolicy::Clip;
                stepper.set_widescreen(req.width, bg, obj);
                let reasons: [Option<&'static str>; 4] =
                    std::array::from_fn(|i| decisions[i].reason());
                // Only on change: this is a UI explanation, not telemetry.
                if last_decisions != Some(reasons) {
                    last_decisions = Some(reasons);
                    if frame_tx
                        .send(CoreEvent::WidescreenDecisions(reasons))
                        .is_err()
                    {
                        return LoopControl::Stop; // UI thread hung up.
                    }
                }
            }
        }
        let ran = match trace.as_mut() {
            None => stepper.tick_running_with_input(
                input.load(),
                &mut FanoutSink {
                    frame: &mut sink,
                    layers: layers_enabled.then_some(&mut layers),
                    bundle: &mut bundle_builder,
                    audio: audio.as_mut(),
                    overlay: overlay_capture.as_mut(),
                },
            ),
            Some(producer) => {
                if stepper.is_paused() {
                    false
                } else {
                    let wants_cpu = producer.wants(rf_debugger::trace::TraceKind::Cpu);
                    let advanced = stepper.latch_and_advance_frame_traced(
                        input.load(),
                        &mut FanoutSink {
                            frame: &mut sink,
                            layers: layers_enabled.then_some(&mut layers),
                            bundle: &mut bundle_builder,
                            audio: audio.as_mut(),
                            overlay: overlay_capture.as_mut(),
                        },
                        &mut |pc, cycle, text| {
                            if wants_cpu {
                                producer.push(rf_debugger::trace::TraceEntry {
                                    kind: rf_debugger::trace::TraceKind::Cpu,
                                    cycle,
                                    addr: pc,
                                    text,
                                });
                            }
                        },
                    );
                    advanced > 0
                }
            }
        };
        // Ticket W20-13: snapshot a RUNNING frame into the rewind ring at
        // its interval. `save_state` only reads the machine (`&self`), so
        // this cannot perturb it; a stepped (rewound/single-stepped) frame
        // is not recorded, so rewinding does not refill the history it is
        // walking back through.
        if ran {
            if let Some(ring) = rewind.as_mut() {
                let frame = stepper.frame_count();
                if ring.wants_snapshot(frame) {
                    if let Ok(bytes) = stepper.save_state(0).and_then(|c| {
                        c.encode()
                            .map_err(|e| crate::save_state::SaveStateError::Io(e.to_string()))
                    }) {
                        let _ = ring.push(frame, &bytes);
                    }
                }
            }
        }
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
            // Ticket W4-10a: the non-CPU chips' trace entries come from
            // the `CoreEvent` FIFO W4-00 already emits, NOT from new hooks
            // inside `rf-nes` — that crate is outside this ticket's write
            // scope, and inventing a parallel producer is what
            // `rf_debugger::event_timeline`'s module doc warns against.
            // The trade is stated rather than hidden: this gives
            // event-granularity records for PPU writes, DMA and mapper
            // IRQs (which is what those rows of DEBUGGER.md §2's list are
            // about) and it is bounded by the session's `EventMask`, so a
            // chip nobody subscribed to contributes nothing.
            if let Some(producer) = trace.as_mut() {
                push_event_traces(producer, &bundle);
            }
            canvas_accum.observe_frame(&bundle, &[]);
            // Ticket W4-01: publish before sending `FrameMsg` so a reader
            // that wakes on the `FrameMsg` channel never sees a
            // `frame_bundle` older than the frame it was just notified
            // about.
            // Ticket W11-01: built HERE, before `bundle` is moved into
            // `publish`, so the reconstruction can borrow the
            // accuracy-exact indexed frame instead of cloning it. A clone
            // would be ~500 KB per frame for a feature that is off by
            // default.
            let probe_data = level_probe.as_ref().map(|probe| LevelProbeData {
                // `peek` is the non-perturbing read (`EmuStepper::peek`'s
                // own doc) — the same one the debugger's memory viewer
                // uses, and the reason this can run every frame without
                // touching the simulation.
                values: probe
                    .addrs
                    .iter()
                    .map(|a| stepper.peek(u16::try_from(*a).unwrap_or(u16::MAX)))
                    .collect(),
                table: (0..probe.table_len)
                    .map(|i| {
                        let addr = probe.table_addr as usize + i;
                        stepper.peek(u16::try_from(addr).unwrap_or(u16::MAX))
                    })
                    .collect(),
            });
            let script_bytes = script_window.map(|(base, len)| {
                (0..len)
                    .map(|i| stepper.peek(u16::try_from(base as usize + i).unwrap_or(u16::MAX)))
                    .collect::<Vec<u8>>()
            });
            let display = display_rgba(&mut historian, &bundle, &sink, overlay_capture.as_deref());
            // Ticket W11-14: read the layer map BEFORE the bundle is
            // handed off — it is moved by `publish`, and the mask is a
            // cheap projection of it rather than a second copy of the
            // pixels.
            let mut hd_layers = hd_capture.then(|| {
                bundle
                    .video
                    .iter()
                    .map(|p| match p.layer {
                        rf_core_api::PixelLayer::Sprite => rf_enhance::hd_render::Layer::Sprite,
                        // Backdrop counts as background: nothing drew
                        // there, so a background replacement may.
                        _ => rf_enhance::hd_render::Layer::Background,
                    })
                    .collect::<Vec<_>>()
            });
            // Ticket W16-14: pull the frame's Mode7 event (at most one,
            // `CoreEvent::Mode7`'s own doc) out BEFORE `bundle` moves into
            // `publish` below — `FrameMsg::mode7` is this thread's only
            // other reader of it, alongside the trace producer above.
            let mode7_event = bundle.events.iter().find_map(|e| match e {
                rf_core_api::CoreEvent::Mode7(frame) => Some(frame.clone()),
                _ => None,
            });
            bundle_writer.publish(bundle);
            let msg = FrameMsg {
                rewind: rewind.as_ref().map(|ring| RewindStatus {
                    len: ring.len(),
                    depth: ring.config().depth,
                    interval: ring.config().interval,
                    bytes: ring.compressed_bytes(),
                }),
                audio_fill: audio.as_ref().map(crate::audio_out::AudioOut::fill),
                level_probe: probe_data,
                script_window: script_bytes,
                // Built when a pack is loaded OR the studio is capturing
                // — see SetTileCapture/SetStudioCapture.
                hd: (hd_capture || studio_capture).then(|| {
                    Box::new(HdFrame {
                        // ~1000 `Placement`s a frame: built only when a
                        // pack is actually loaded (`hd_capture`), not
                        // merely because the studio also wants THIS
                        // frame's tiles for a different reason (its own
                        // `studio_tiles`, right below) — pay-for-use per
                        // reason, not per union of reasons.
                        placements: if hd_capture {
                            stepper.hd_placements()
                        } else {
                            Vec::new()
                        },
                        layers: hd_layers.take().unwrap_or_default(),
                        studio_tiles: if studio_capture {
                            stepper.studio_captures()
                        } else {
                            Vec::new()
                        },
                    })
                }),
                rgba: display,
                width: sink.width(),
                height: sink.height(),
                frame_count: stepper.frame_count(),
                last_scanline: stepper.last_scanline(),
                // Ticket W3-03a: the two ~240 KB clones only happen when
                // someone is looking. Empty slices tell the UI "no layer
                // data this frame" without a second flag to keep in sync.
                bg_rgba: if layers_enabled {
                    layers.bg_rgba().to_vec()
                } else {
                    Vec::new()
                },
                sprite_rgba: if layers_enabled {
                    layers.sprite_rgba().to_vec()
                } else {
                    Vec::new()
                },
                oam: Box::new(*stepper.oam()),
                audio_traces: if audio_capture {
                    // ~64 points is more than a 32-pixel-tall scope can
                    // resolve; reducing here keeps a frame's 5x735
                    // samples off the channel entirely.
                    stepper
                        .take_audio_channel_samples()
                        .iter()
                        .map(|c| rf_debugger::audio_scope::trace(c, 64))
                        .collect()
                } else {
                    Vec::new()
                },
                snes: if snes_debug_capture {
                    stepper.snes_debug_snapshot().map(Box::new)
                } else {
                    None
                },
                vram: Box::new(*stepper.vram()),
                cpu_regs: Box::new(stepper.cpu_regs()),
                palette_ram: Box::new(*stepper.palette()),
                wram: Box::new(stepper.wram_snapshot()),
                prg_ram: Box::new(*stepper.prg_ram()),
                sprite_height_px: stepper.sprite_height_px(),
                mode7: mode7_event.map(Box::new),
            };
            if publish_frame(
                &frame_tx,
                &pending_frames,
                waker.as_deref(),
                &mut dropped_frames_since_log,
                &mut last_drop_log,
                msg,
            )
            .is_err()
            {
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

/// Ticket W14-20 defects 1 and 2: try to publish one frame, respecting
/// [`MAX_PENDING_FRAMES`], and wake `waker` after a successful send.
///
/// Extracted out of `core_thread_main`'s closure so the drop cap and the
/// waker can each be unit-tested directly against a bare channel and
/// `AtomicUsize` (below), instead of only indirectly through a whole
/// spawned core thread — the same "test the seam, not just the whole
/// pipeline" reasoning `step_frame_command_delivers_a_frame_to_the_ui`'s
/// own doc comment gives.
///
/// Returns `Err(())` when the receiver has hung up — `core_thread_main`'s
/// cue to stop the loop, same as the old inline `frame_tx.send(...)
/// .is_err()` check this replaces.
fn publish_frame(
    frame_tx: &Sender<CoreEvent>,
    pending_frames: &AtomicUsize,
    waker: Option<&(dyn Fn() + Send + Sync)>,
    dropped_frames_since_log: &mut u64,
    last_drop_log: &mut Instant,
    msg: FrameMsg,
) -> Result<(), ()> {
    // A newer frame is never worth more than an older unconsumed one to a
    // UI that has fallen behind (`app::pump_core_events` already keeps
    // only the LATEST `CoreEvent::Frame` of however many it drains in one
    // pass), so once `MAX_PENDING_FRAMES` are already sitting on the
    // channel this one is dropped rather than queued behind them —
    // bounding the channel's memory instead of building it up unbounded
    // the way the old `mpsc::channel()` did (measured 15 MB/s RSS growth,
    // 394 MB -> 2826 MB in ~3 min on m4max, 2026-09-17).
    if pending_frames.load(Ordering::Acquire) >= MAX_PENDING_FRAMES {
        *dropped_frames_since_log += 1;
        if last_drop_log.elapsed() >= Duration::from_secs(1) {
            log::debug!(
                "core thread: dropped {dropped_frames_since_log} frame(s) in the last \
                 second — evt_rx has {MAX_PENDING_FRAMES} pending and nobody is draining it"
            );
            *dropped_frames_since_log = 0;
            *last_drop_log = Instant::now();
        }
        return Ok(());
    }
    pending_frames.fetch_add(1, Ordering::AcqRel);
    frame_tx
        .send(CoreEvent::Frame(Box::new(msg)))
        .map_err(|_| ())?;
    // Defect 2: wake the UI right after a successful send, rather than
    // trusting a winit redraw that on 2026-09-17 did not arrive.
    // `spawn_with_waker`'s doc has the `Context::request_repaint`-from-
    // any-thread citation.
    if let Some(wake) = waker {
        wake();
    }
    Ok(())
}

/// Turn one frame's [`rf_core_api::CoreEvent`]s into trace entries for the
/// non-CPU chips (ticket W4-10a).
///
/// Events with no address of their own record 0; the `cycle` field is the
/// frame number rather than a master cycle, and **that difference is
/// deliberate and is why it is written down here**: the FIFO does not
/// carry per-event cycle stamps, so claiming one would be inventing
/// precision the source does not have. The trace viewer sorts by arrival,
/// which is emission order, which is what the FIFO guarantees.
fn push_event_traces(
    producer: &mut crate::trace_capture::TraceProducer,
    bundle: &rf_core_api::FrameBundle,
) {
    use rf_core_api::CoreEvent;
    use rf_debugger::trace::{TraceEntry, TraceKind};

    for event in &bundle.events {
        let (kind, addr, text) = match event {
            CoreEvent::ScrollWrite { x, y, layer } => (
                TraceKind::PpuWrite,
                0x2005,
                format!("scroll {layer:?} -> x={x} y={y}"),
            ),
            CoreEvent::OamRewrite => (TraceKind::PpuWrite, 0x2003, "OAM rewrite".to_string()),
            CoreEvent::DmaStart { chan } => {
                (TraceKind::Dma, 0x4014, format!("DMA start, channel {chan}"))
            }
            CoreEvent::MapperIrq => (TraceKind::Mapper, 0, "mapper IRQ asserted".to_string()),
            _ => continue,
        };
        if !producer.wants(kind) {
            continue;
        }
        producer.push(TraceEntry {
            kind,
            cycle: bundle.frame_count,
            addr,
            text,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `FrameMsg` with every field at its cheapest legal value — enough
    /// to exercise [`publish_frame`]'s bookkeeping without needing a real
    /// stepped frame's actual pixels, which `publish_frame` never looks
    /// at.
    fn empty_frame_msg() -> FrameMsg {
        FrameMsg {
            rewind: None,
            level_probe: None,
            script_window: None,
            audio_fill: None,
            hd: None,
            rgba: Vec::new(),
            width: 0,
            height: 0,
            frame_count: 0,
            last_scanline: None,
            bg_rgba: Vec::new(),
            sprite_rgba: Vec::new(),
            oam: Box::new([0u8; 256]),
            audio_traces: Vec::new(),
            snes: None,
            vram: Box::new([0u8; 0x1000]),
            cpu_regs: Box::new(rf_core_api::CpuRegs::None),
            palette_ram: Box::new([0u8; 32]),
            wram: Box::new([0u8; 0x0800]),
            prg_ram: Box::new([0u8; 0x2000]),
            sprite_height_px: 8,
            mode7: None,
        }
    }

    /// Ticket W14-20 defect 1, acceptance criterion 1: stall the receiver
    /// (never call `recv`/`try_recv`) and push far more frames than
    /// `MAX_PENDING_FRAMES` through [`publish_frame`] — the exact function
    /// `core_thread_main`'s send site calls. Before this ticket the
    /// channel behind this was `mpsc::channel()` (unbounded): this test
    /// would have piled up all 1000 messages (and their `RGBA`/VRAM/WRAM/
    /// OAM allocations) with nothing bounding either the count or the
    /// bytes — the exact shape of the measured 15 MB/s RSS growth.
    #[test]
    fn publish_frame_drops_once_max_pending_frames_are_unconsumed() {
        let (frame_tx, frame_rx) = mpsc::channel();
        let pending_frames = AtomicUsize::new(0);
        let mut dropped = 0u64;
        let mut last_log = Instant::now();

        for _ in 0..1000 {
            publish_frame(
                &frame_tx,
                &pending_frames,
                None,
                &mut dropped,
                &mut last_log,
                empty_frame_msg(),
            )
            .expect("the receiver is never dropped in this test");
        }

        assert_eq!(
            pending_frames.load(Ordering::Acquire),
            MAX_PENDING_FRAMES,
            "the shared counter must never exceed MAX_PENDING_FRAMES, no matter how many \
             frames were offered"
        );
        let queued = frame_rx.try_iter().count();
        assert_eq!(
            queued, MAX_PENDING_FRAMES,
            "the channel itself must hold at most MAX_PENDING_FRAMES messages — a stalled \
             receiver must not let it grow to 1000"
        );
    }

    /// Ticket W14-20 defect 2, acceptance criterion 2: the waker fires
    /// exactly once per frame [`publish_frame`] actually delivers — not
    /// once per call (a dropped frame, per the test above, must not wake
    /// anyone for a picture the UI will never see) and not more than once
    /// per delivered frame.
    #[test]
    fn publish_frame_calls_the_waker_once_per_delivered_frame() {
        let (frame_tx, frame_rx) = mpsc::channel();
        let pending_frames = AtomicUsize::new(0);
        let mut dropped = 0u64;
        let mut last_log = Instant::now();
        let wake_calls = Arc::new(AtomicU64::new(0));
        let counting_waker = {
            let wake_calls = Arc::clone(&wake_calls);
            move || {
                wake_calls.fetch_add(1, Ordering::AcqRel);
            }
        };

        // Drain after every send, so every one of these is a genuine
        // delivery rather than hitting the drop cap above.
        for _ in 0..5 {
            publish_frame(
                &frame_tx,
                &pending_frames,
                Some(&counting_waker),
                &mut dropped,
                &mut last_log,
                empty_frame_msg(),
            )
            .expect("the receiver is never dropped in this test");
            assert!(
                matches!(frame_rx.try_recv(), Ok(CoreEvent::Frame(_))),
                "publish_frame must have actually sent a frame this call"
            );
            // Mirror `app::pump_core_events`'s own contract: every
            // delivered `CoreEvent::Frame` a consumer drains must release
            // its slot back, or `pending_frames` would climb to
            // `MAX_PENDING_FRAMES` after just two iterations here and
            // start dropping frames 3 through 5 for the wrong reason.
            pending_frames.fetch_sub(1, Ordering::AcqRel);
        }

        assert_eq!(
            wake_calls.load(Ordering::Acquire),
            5,
            "the waker must fire exactly once per delivered frame"
        );
    }

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

        // **A liveness bound, not a performance assertion.** This waits
        // on a path whose expensive step is `Backtrace::force_capture()`
        // inside the panic hook — full symbolization, which in a debug
        // build is not fast. Unloaded the whole test takes ~0.7 s, and
        // the old 5 s bound was therefore a claim that the machine would
        // never be more than ~7x busy. It failed three times on
        // 2026-08-25 (W10-01/W10-03), every time with a release build or
        // the wgpu render tests running alongside, and never once when
        // run alone — a red test that meant "this laptop is busy".
        //
        // What the ticket actually demands is that a contained panic
        // REPORTS rather than vanishing; if `catch_unwind` were removed
        // the channel would never produce anything and this would still
        // fail, just later. So the bound is generous on purpose.
        let evt = evt_rx
            .recv_timeout(Duration::from_secs(30))
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
            CoreEvent::WidescreenDecisions(_) => {
                panic!("expected a crash report, got widescreen decisions")
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
    /// The RF-Scroller fixture's bytes, or `None` (skip) when not built.
    fn rf_scroller_rom() -> Option<Vec<u8>> {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/nes/rf-scroller/build/rf-scroller.nes");
        std::fs::read(&fixture).ok()
    }

    fn fnv64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    /// Receive the next frame, consuming it the way the UI does (W14-20's
    /// back-pressure counter), or the core stops sending.
    fn next_frame(core: &CoreHandle, wait: Duration) -> Option<Box<FrameMsg>> {
        let deadline = std::time::Instant::now() + wait;
        while std::time::Instant::now() < deadline {
            if let Ok(CoreEvent::Frame(m)) = core.evt_rx.recv_timeout(Duration::from_millis(200)) {
                core.pending_frames.fetch_sub(1, Ordering::AcqRel);
                return Some(m);
            }
        }
        None
    }

    type RewindRun = (
        std::collections::HashMap<u64, u64>,
        Vec<(u64, u64)>,
        RewindStatus,
    );

    /// Ticket W20-13: run with rewind on, then step back. Returns the
    /// per-frame fingerprints of the forward run, the rewound frames, and
    /// the last ring status.
    fn run_then_rewind(steps: usize) -> Option<RewindRun> {
        let rom = rf_scroller_rom()?;
        let core = spawn(rom).expect("fixture spawns");
        core.cmd_tx
            .send(CoreCommand::SetRewind(Some(
                rf_state::RewindConfig::enabled(10, 50),
            )))
            .unwrap();
        core.cmd_tx.send(CoreCommand::Resume).unwrap();
        let mut seen = std::collections::HashMap::new();
        let mut status = None;
        while let Some(m) = next_frame(&core, Duration::from_secs(10)) {
            seen.insert(m.frame_count, fnv64(&m.rgba));
            status = m.rewind;
            if m.frame_count >= 240 {
                break;
            }
        }
        core.cmd_tx.send(CoreCommand::Pause).unwrap();
        while next_frame(&core, Duration::from_millis(150)).is_some() {}
        let mut rewound = Vec::new();
        for _ in 0..steps {
            core.cmd_tx.send(CoreCommand::RewindStep).unwrap();
            if let Some(m) = next_frame(&core, Duration::from_secs(5)) {
                rewound.push((m.frame_count, fnv64(&m.rgba)));
            }
        }
        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
        Some((seen, rewound, status?))
    }

    /// Ticket W20-13 (`docs/design/UX_WAVE_20.md` §5): the mechanics — a
    /// ring fills while running, and each step goes further back and shows
    /// a frame. Skips when the fixture is not built.
    #[test]
    fn rewind_fills_while_running_and_each_step_goes_further_back() {
        let Some((seen, rewound, status)) = run_then_rewind(5) else {
            eprintln!("SKIP: RF-Scroller fixture not built");
            return;
        };
        assert!(
            status.len >= 10,
            "the ring filled while running: {status:?}"
        );
        assert!(status.bytes > 0);
        assert!(seen.len() > 100, "the forward run delivered frames");
        let frames: Vec<u64> = rewound.iter().map(|(f, _)| *f).collect();
        assert_eq!(frames.len(), 5, "every step showed a frame: {frames:?}");
        assert!(
            frames.windows(2).all(|w| w[1] < w[0]),
            "further back each step: {frames:?}"
        );
    }

    /// Ticket W20-13 acceptance 3 — **KNOWN RED, `#[ignore]`d with
    /// evidence** (plan.json W20-13 BLOCKED note; W20-22): a rewound frame
    /// should be byte-identical to the frame first played at that number.
    /// On RF-Scroller some are, some are a picture never displayed. The
    /// cause is NOT the ring: `save_load_step_reproduces_the_next_frame`
    /// shows the plain Save -> Load -> StepFrame path diverging the same
    /// way (1 of 5 probes, frame 171, 2026-10-06), so the NES `.rfstate`
    /// does not capture everything the next frame depends on.
    #[test]
    #[ignore = "known red: NES save/load does not reproduce the next frame (W20-22)"]
    fn rewinding_shows_exactly_the_frame_that_was_originally_rendered() {
        let Some((seen, rewound, _)) = run_then_rewind(10) else {
            return;
        };
        for (frame, hash) in rewound {
            if let Some(original) = seen.get(&frame) {
                assert_eq!(
                    *original, hash,
                    "frame {frame} after rewinding differs from when it was first played"
                );
            }
        }
    }

    /// W20-22's evidence, isolated from rewind: save at frame N, step,
    /// load, step again — the two frames N+1 should be identical.
    #[test]
    #[ignore = "known red: NES save/load does not reproduce the next frame (W20-22)"]
    fn save_load_step_reproduces_the_next_frame() {
        let Some(rom) = rf_scroller_rom() else { return };
        let dir = std::env::temp_dir().join(format!("rf_saveload_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let core = spawn(rom).unwrap();
        let mut mismatches = Vec::new();
        for start in [150u64, 157, 163, 170, 181] {
            loop {
                core.cmd_tx.send(CoreCommand::StepFrame).unwrap();
                let m = next_frame(&core, Duration::from_secs(5)).expect("frame");
                if m.frame_count >= start {
                    break;
                }
            }
            core.cmd_tx
                .send(CoreCommand::SaveStateToSlot {
                    dir: dir.clone(),
                    stem: "slot1".into(),
                })
                .unwrap();
            std::thread::sleep(Duration::from_millis(100));
            core.cmd_tx.send(CoreCommand::StepFrame).unwrap();
            let first = next_frame(&core, Duration::from_secs(5)).expect("frame");
            let (container, _) = crate::state_slots::load(
                &dir,
                crate::state_slots::SlotId::Numbered(1),
                &rf_state::MigrationRegistry::default(),
            )
            .unwrap();
            core.cmd_tx
                .send(CoreCommand::ApplyState(Box::new(container)))
                .unwrap();
            core.cmd_tx.send(CoreCommand::StepFrame).unwrap();
            let again = next_frame(&core, Duration::from_secs(5)).expect("frame");
            if fnv64(&first.rgba) != fnv64(&again.rgba) {
                mismatches.push(first.frame_count);
            }
        }
        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
        assert!(
            mismatches.is_empty(),
            "frames not reproduced after save/load: {mismatches:?}"
        );
    }

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

    /// A hand-assembled NROM program that writes ONE known sprite (Y=50,
    /// tile=7, attr=0, X=100) into OAM slot 0 via an `$2003`/`$2004`
    /// write sequence, then loops forever — same opcode layout/verification
    /// discipline as `crates/retroforge/tests/sprite_overlay_mode_
    /// invariant.rs`'s own (larger) OAM-writing fixture, trimmed to one
    /// sprite since this test only needs *some* non-zero OAM content to
    /// discriminate "forwarded" from "always zero" (see the test's own
    /// doc). Opcodes: `A9` LDA#, `8D` STA abs, `4C` JMP abs — the exact
    /// same three already verified working in that sibling fixture.
    ///
    /// | Addr  | Bytes       | Instruction             |
    /// |-------|-------------|-------------------------|
    /// | $8000 | A9 00       | LDA #$00                |
    /// | $8002 | 8D 03 20    | STA $2003 (OAMADDR = 0) |
    /// | $8005 | A9 32       | LDA #$32 (Y = 50)       |
    /// | $8007 | 8D 04 20    | STA $2004                |
    /// | $800A | A9 07       | LDA #$07 (tile = 7)     |
    /// | $800C | 8D 04 20    | STA $2004                |
    /// | $800F | A9 00       | LDA #$00 (attr = 0)     |
    /// | $8011 | 8D 04 20    | STA $2004                |
    /// | $8014 | A9 64       | LDA #$64 (X = 100)      |
    /// | $8016 | 8D 04 20    | STA $2004                |
    /// | $8019 | 4C 19 80    | forever: JMP forever     |
    #[rustfmt::skip]
    const OAM_WRITE_PROGRAM: &[(u16, &[u8])] = &[
        (0x8000, &[0xA9, 0x00]),
        (0x8002, &[0x8D, 0x03, 0x20]),
        (0x8005, &[0xA9, 0x32]),
        (0x8007, &[0x8D, 0x04, 0x20]),
        (0x800A, &[0xA9, 0x07]),
        (0x800C, &[0x8D, 0x04, 0x20]),
        (0x800F, &[0xA9, 0x00]),
        (0x8011, &[0x8D, 0x04, 0x20]),
        (0x8014, &[0xA9, 0x64]),
        (0x8016, &[0x8D, 0x04, 0x20]),
        (0x8019, &[0x4C, 0x19, 0x80]),
    ];

    fn oam_write_nrom() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
        data.push(1); // 1x16KiB PRG
        data.push(1); // 1x8KiB CHR
        data.extend_from_slice(&[0u8; 10]); // mapper 0, iNES 1.0

        let mut prg = vec![0u8; 16 * 1024];
        for (addr, bytes) in OAM_WRITE_PROGRAM {
            let offset = (*addr - 0x8000) as usize;
            prg[offset..offset + bytes.len()].copy_from_slice(bytes);
        }
        prg[0x3FFC] = 0x00; // reset vector low  -> $8000
        prg[0x3FFD] = 0x80; // reset vector high
        data.extend(prg);
        data.extend(vec![0u8; 8 * 1024]);
        data
    }

    /// Ticket W4-06a: `FrameMsg::oam` must carry the CORE's real OAM
    /// content, not a zeroed placeholder that happens to look plausible.
    /// `synthetic_nrom` above (all-zero, `BRK`-forever) can't discriminate
    /// this — it never writes OAM at all, so "forwarded correctly" and
    /// "field never wired up" would look identical (all zero either way,
    /// the exact trap `step_frame_command_also_delivers_bg_layer_content_
    /// not_just_size`'s own doc warns about for `bg_rgba`). `oam_write_nrom`
    /// writes a real, known, non-zero sprite via genuine `$2003`/`$2004`
    /// bus writes, so this test fails if `core_thread_main`'s `oam: Box::
    /// new(*stepper.oam())` line is ever dropped or reads the wrong slot.
    #[test]
    fn step_frame_command_delivers_real_oam_content_not_a_zeroed_placeholder() {
        let core = spawn(oam_write_nrom()).expect("OAM-writing NROM must spawn a core thread");
        core.cmd_tx
            .send(CoreCommand::StepFrame)
            .expect("core thread must accept a StepFrame command");
        let evt = core
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a stepped frame must be delivered");
        match evt {
            CoreEvent::Frame(msg) => {
                assert_eq!(
                    &msg.oam[0..4],
                    &[50, 7, 0, 100],
                    "OAM slot 0 must carry the exact bytes the fixture wrote \
                     (Y=50, tile=7, attr=0, X=100), not zeros: {:?}",
                    &msg.oam[0..8]
                );
                assert!(
                    msg.oam[4..].iter().all(|&b| b == 0),
                    "only slot 0 was written — every other slot must stay zero, \
                     not leak slot 0's bytes across the whole array"
                );
            }
            CoreEvent::Crashed(r) => panic!("expected a frame, got a crash: {}", r.message),
            CoreEvent::CanvasSnapshot(_) => panic!("expected a frame, got a canvas snapshot"),
            CoreEvent::WidescreenDecisions(_) => {
                panic!("expected a frame, got widescreen decisions")
            }
        }

        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
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
            CoreEvent::WidescreenDecisions(_) => {
                panic!("expected a frame, got widescreen decisions")
            }
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
        // Ticket W3-03a: extraction is opt-in now, so this test asks for it
        // first. `layer_extraction_is_off_until_asked_for` below is the
        // other half — without it, "the toggle works" and "the toggle is
        // ignored" would look identical from here.
        core.cmd_tx
            .send(CoreCommand::SetLayerExtraction(true))
            .expect("core thread must accept SetLayerExtraction");
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
            CoreEvent::WidescreenDecisions(_) => {
                panic!("expected a frame, got widescreen decisions")
            }
        }

        let _ = core.cmd_tx.send(CoreCommand::Shutdown);
        let _ = core.join_handle.join();
    }

    /// Ticket W3-03a, acceptance criterion 1: a closed debug window costs
    /// nothing on the frame path.
    ///
    /// The *cost* is measured in `tests/frame_bundle_perf.rs`; what is
    /// asserted here is the mechanism that produces it — that no layer
    /// buffers are cloned into `FrameMsg` at all until someone asks. Both
    /// halves are needed: this test and the one above fail in opposite
    /// directions, so an implementation that ignored the toggle either way
    /// is caught.
    #[test]
    fn layer_extraction_is_off_until_asked_for() {
        let core = spawn(synthetic_nrom()).expect("synthetic NROM must spawn a core thread");
        core.cmd_tx
            .send(CoreCommand::StepFrame)
            .expect("core thread must accept a StepFrame command");
        let evt = core
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stepped frame must be delivered");
        match evt {
            CoreEvent::Frame(msg) => {
                assert!(
                    !msg.rgba.is_empty(),
                    "the main frame must still be produced — this gates the LAYER split only"
                );
                assert!(
                    msg.bg_rgba.is_empty() && msg.sprite_rgba.is_empty(),
                    "no layer data may be cloned before SetLayerExtraction(true): got \
                     {} bg bytes and {} sprite bytes",
                    msg.bg_rgba.len(),
                    msg.sprite_rgba.len()
                );
            }
            CoreEvent::Crashed(r) => panic!("expected a frame, got a crash: {}", r.message),
            CoreEvent::CanvasSnapshot(_) => panic!("expected a frame, got a canvas snapshot"),
            CoreEvent::WidescreenDecisions(_) => {
                panic!("expected a frame, got widescreen decisions")
            }
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
            CoreEvent::WidescreenDecisions(_) => {
                panic!("expected a frame, got widescreen decisions")
            }
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

    use rf_core_api::CoreSink as _;

    /// A frame of `n` opaque pixels of `index`, rest background.
    fn indexed_frame(
        w: usize,
        h: usize,
        spots: &[(usize, usize, u8)],
    ) -> Vec<rf_core_api::PpuPixel> {
        let mut v = vec![
            rf_core_api::PpuPixel {
                palette_index: 0x0F,
                layer: rf_core_api::PixelLayer::Background(0),
                sprite_id: None,
                priority: 0,
            };
            w * h
        ];
        for &(x, y, idx) in spots {
            v[y * w + x] = rf_core_api::PpuPixel {
                palette_index: idx,
                layer: rf_core_api::PixelLayer::Sprite,
                sprite_id: Some(1),
                priority: 0,
            };
        }
        v
    }

    /// **Off costs exactly nothing.** Byte-for-byte the frame
    /// `FrameBuffer` already composited — law 6's requirement, not an
    /// optimisation, and the assertion that makes "opt-in" true rather
    /// than merely claimed.
    #[test]
    fn display_rgba_is_the_untouched_frame_when_deflicker_is_off() {
        let mut historian = rf_enhance::sprite_historian::SpriteHistorian::new();
        let sink = rf_renderer::FrameBuffer::new();
        let mut builder =
            rf_core_api::FrameBundleBuilder::new(sink.width() as u16, sink.height() as u16);
        let bundle = builder.take(0);
        assert!(!historian.enabled(), "precondition: off by default");
        assert_eq!(
            display_rgba(&mut historian, &bundle, &sink, None),
            sink.to_vec(),
            "with de-flicker off the display frame must be the accuracy frame, unchanged"
        );
    }

    /// **The overlay survives the rebuild.** The bug this guards is
    /// specific and silent: the dropped-sprite overlay is composited into
    /// rgba inside `FrameBuffer`, while de-flicker reconstructs INDEXED
    /// pixels — so rebuilding from indices throws the overlay away. A
    /// user with both enhancements on would have watched one of them stop
    /// working, with nothing on screen to say which or why.
    #[test]
    fn the_sprite_overlay_survives_a_deflicker_rebuild() {
        let mut historian = rf_enhance::sprite_historian::SpriteHistorian::new();
        historian.set_enabled(true);
        let sink = rf_renderer::FrameBuffer::new();
        let (w, h) = (sink.width(), sink.height());

        let mut builder =
            rf_core_api::FrameBundleBuilder::new(sink.width() as u16, sink.height() as u16);
        for y in 0..h {
            builder.video_scanline(y as u16, &indexed_frame(w, 1, &[])[..w]);
        }
        let bundle = builder.take(1);

        // One opaque overlay pixel, in a colour the background is not.
        let mut overlay = vec![
            rf_core_api::OverlayPixel {
                palette_index: 0,
                opaque: false,
            };
            w * h
        ];
        let (ox, oy) = (10usize, 20usize);
        overlay[oy * w + ox] = rf_core_api::OverlayPixel {
            palette_index: 0x16,
            opaque: true,
        };

        let rgba = display_rgba(&mut historian, &bundle, &sink, Some(&overlay));
        let want = rf_renderer::palette_index_to_rgb(0x16);
        let o = (oy * w + ox) * 4;
        assert_eq!(
            [rgba[o], rgba[o + 1], rgba[o + 2]],
            want,
            "the overlay pixel was erased by the de-flicker rebuild"
        );

        // And the guard against a vacuous version of the assertion above:
        // if the background happened to be the same colour, it would pass
        // with the overlay dropped.
        let bg = rf_renderer::palette_index_to_rgb(0x0F);
        assert_ne!(
            want, bg,
            "the overlay colour must differ from the background, or this test proves nothing"
        );
    }

    /// **`FrameBundle::video` is never perturbed.** The mode invariant in
    /// its smallest form: the accuracy-exact frame the rest of the system
    /// hashes must be identical before and after a reconstruction ran
    /// over it. `SpriteHistorian::observe` takes `&[PpuPixel]` so this
    /// cannot fail — which is exactly why it is worth pinning, since a
    /// future signature change would be silent otherwise.
    #[test]
    fn deflicker_cannot_perturb_the_accuracy_frame() {
        let mut historian = rf_enhance::sprite_historian::SpriteHistorian::new();
        historian.set_enabled(true);
        let sink = rf_renderer::FrameBuffer::new();
        let w = sink.width();
        let mut builder =
            rf_core_api::FrameBundleBuilder::new(sink.width() as u16, sink.height() as u16);
        for y in 0..sink.height() {
            builder.video_scanline(y as u16, &indexed_frame(w, 1, &[(5, 0, 0x21)])[..w]);
        }
        let bundle = builder.take(2);
        let before = bundle.video.clone();
        let _ = display_rgba(&mut historian, &bundle, &sink, None);
        assert_eq!(
            bundle.video, before,
            "the accuracy-exact frame changed while an enhancement observed it"
        );
    }
}
