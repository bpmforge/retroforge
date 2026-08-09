//! `eframe::App` implementation (ticket W1-06 acceptance criteria 1-4).
//!
//! This is the **only** module in this crate allowed to depend on
//! `egui`/`eframe` (crate-level doc) — it is a thin presentation layer over
//! [`crate::core_thread`] (FM-01 containment, panic-guarded core thread)
//! and [`crate::rom_open`] (the file dialog). It owns no NES/6502 state
//! directly; [`crate::stepper::EmuStepper`] lives entirely on the core
//! thread.
//!
//! `egui`/`eframe` 0.35 API notes (verified against the pinned version's
//! source in `~/.cargo/registry`, not training data — both changed
//! recently):
//! - [`eframe::App::ui`] takes `&mut egui::Ui` directly (no
//!   `CentralPanel`/margin), not the older `update(&mut self, ctx:
//!   &Context, ...)` shape.
//! - `egui::TopBottomPanel`/`SidePanel` no longer exist; panels are all
//!   `egui::Panel::top/bottom/left/right(id).show(ui, |ui| ...)`, taking a
//!   `&mut Ui` like every other panel now.
//! - `ui.close_menu()` was renamed `ui.close()`.
//!
//! ## Keyboard input (ticket W1-07)
//!
//! `poll_input` samples `egui::InputState::key_down` once per repaint for
//! the fixed handful of keys [`crate::input_map::map_key`] knows, pushes
//! each into an [`rf_input::InputLatch`] (translated via that map — the
//! only place `egui::Key` and `rf_input::Key` ever meet), and stores the
//! resulting `InputFrame` into the core thread's `SharedInputFrame`
//! (`crate::core_thread`'s module doc: one atomic, no mutex in the frame
//! loop). This module never talks to `rf_nes` directly — the core thread
//! is the only thing that latches input into a running machine.
use eframe::egui;

use crate::core_thread::{self, CoreCommand, CoreCrashReport, CoreEvent, CoreHandle};
use crate::enhanced_view::{self, CameraToggle};
use crate::input_map;
use crate::rom_open;

/// How many repaints [`RetroForgeApp::maybe_request_canvas_snapshot`] lets
/// pass between `CoreCommand::RequestCanvasSnapshot` sends while the
/// Ultrawide camera is active — a fresh snapshot on every repaint would
/// clone the whole stitched `Canvas` at ~60Hz (`CanvasAccumulator::
/// current_canvas`'s own doc: "tens of MB/s for a level of any real
/// size"). 30 repaints is roughly twice a second at the app's normal
/// repaint cadence — frequent enough that Ultrawide visibly keeps up with
/// play, far below the cost of a per-frame clone.
const CANVAS_SNAPSHOT_REFRESH_INTERVAL: u32 = 30;

/// Every host key the default NES keymap binds — the fixed poll list
/// `poll_input` checks each repaint (module doc).
const POLLED_KEYS: [egui::Key; 8] = [
    egui::Key::ArrowUp,
    egui::Key::ArrowDown,
    egui::Key::ArrowLeft,
    egui::Key::ArrowRight,
    egui::Key::Z,
    egui::Key::X,
    egui::Key::Enter,
    egui::Key::ShiftRight,
];

/// The whole application's UI-thread-owned state.
pub struct RetroForgeApp {
    core: Option<CoreHandle>,
    texture: Option<egui::TextureHandle>,
    /// Ticket W3-03: the same frame's BG-only layer as a separate egui
    /// texture (`rf_renderer::LayeredFrame::bg_rgba`, via
    /// `core_thread::FrameMsg::bg_rgba`) — `None` until the first frame
    /// lands, same lifecycle as [`Self::texture`].
    bg_layer_texture: Option<egui::TextureHandle>,
    /// Ticket W3-03: the sprite-only layer, mirroring
    /// [`Self::bg_layer_texture`].
    sprite_layer_texture: Option<egui::TextureHandle>,
    status: String,
    crash: Option<CoreCrashReport>,
    /// Mirrors the core thread's run state for button labels/enablement;
    /// the core thread itself (`EmuStepper::state`) is the source of
    /// truth — this is only ever set right after sending a command, so it
    /// can't drift for more than one repaint.
    running: bool,
    /// Ticket W2-14: a step command's frame arrives over the channel a few
    /// milliseconds *after* the click's own repaint has already finished.
    /// While paused nothing else schedules a repaint, so without this the
    /// frame would sit unconsumed until some unrelated event (a mouse
    /// move) happened to wake the UI — a "works if you jiggle the mouse"
    /// bug. Set when a step is requested, cleared when a frame lands.
    awaiting_stepped_frame: bool,
    /// Latest machine position from the core thread, rendered in the
    /// status bar (ticket W2-15). `None` until the first frame lands.
    /// This is what makes Step Frame and Step Scanline observable: a
    /// scanline step changes 1/240th of the picture over a framebuffer
    /// that deliberately persists, so without a readout a correct step is
    /// indistinguishable from a dead button — which is exactly how it was
    /// first reported.
    position: Option<(u64, Option<u16>)>,
    /// Host-agnostic per-frame input latch (`rf_input`, FR-FE-003) — the
    /// UI thread's write side; `poll_input` samples it every repaint into
    /// the core thread's `SharedInputFrame` (module doc).
    input_latch: rf_input::InputLatch,
    keymap: rf_input::KeyMap,
    /// UI-thread mirror of the core thread's overlay setting (ticket
    /// W3-05a) — same "set right after sending a command" pattern
    /// `running` above uses, for the same reason (the checkbox needs
    /// something to read/write; the core thread's `EmuStepper` is the real
    /// source of truth). `false` by default: a freshly opened ROM boots in
    /// Accuracy Mode (law 6).
    sprite_overlay: bool,
    /// Ticket W3-03 acceptance criterion 2: whether the "Layers (debug)"
    /// window is shown. Off by default — a debug view, not part of the
    /// ordinary play experience.
    show_layers: bool,
    /// Ticket W4-03e: the enhanced compositor's shared-device `GpuContext`
    /// (`rf_renderer::GpuContext::from_shared`, built once from `eframe`'s
    /// own `wgpu_render_state` — never a second `request_headless()`
    /// device, see that constructor's doc). `None` only if this build ever
    /// ran on the glow backend (not expected — eframe 0.35's default
    /// feature set is `wgpu`), in which case the Ultrawide camera degrades
    /// to visibly unavailable rather than panicking.
    gpu: Option<rf_renderer::GpuContext>,
    /// Built once alongside [`Self::gpu`] (`EnhancedCompositor::new`'s own
    /// "build once, reuse per frame" shape).
    compositor: Option<rf_renderer::EnhancedCompositor>,
    /// Ticket W4-03e acceptance criterion 2: the runtime Original/
    /// Ultrawide toggle. `Original` by default — a fresh ROM boots showing
    /// exactly what it always has (law 6).
    camera: CameraToggle,
    /// Latest stitched-canvas snapshot from the core thread
    /// (`CoreEvent::CanvasSnapshot`), if the Ultrawide camera has ever been
    /// requested this session.
    ultrawide_canvas: Option<rf_enhance::stitcher::Canvas>,
    /// The most recent [`enhanced_view::compose_ultrawide`] result over
    /// [`Self::ultrawide_canvas`] — `Err` (e.g. "canvas is empty") is kept
    /// distinct from `None` ("never even tried yet") so
    /// [`enhanced_view::select_active_view`] can show a specific reason.
    ultrawide_render: Option<Result<enhanced_view::UltrawideRender, String>>,
    /// The egui texture built from [`Self::ultrawide_render`]'s `rgba`,
    /// same "persistent `TextureHandle`, `.set()` on later frames" shape
    /// [`Self::texture`] already uses.
    ultrawide_texture: Option<egui::TextureHandle>,
    /// FM-13 criterion 3: the "view too large for GPU, reduced" toast text
    /// (`enhanced_view::UltrawideRender::fm13_message`), surfaced in
    /// [`Self::controls_bar`] whenever the latest Ultrawide render was
    /// reduced — `None` swallows nothing; it means the latest render
    /// genuinely needed no reduction.
    fm13_message: Option<String>,
    /// Repaints remaining before [`Self::maybe_request_canvas_snapshot`]
    /// sends another `CoreCommand::RequestCanvasSnapshot` — `0` forces an
    /// immediate request on the very next repaint (set whenever the camera
    /// is switched to Ultrawide).
    ultrawide_refresh_countdown: u32,
    /// Ticket W2-14's "a reply arrives a few ms after the click's own
    /// repaint already finished" lesson, mirrored for canvas snapshots:
    /// set when a `RequestCanvasSnapshot` is sent, cleared when
    /// `CoreEvent::CanvasSnapshot` arrives — keeps the UI repainting until
    /// the reply lands even while otherwise Paused, so it doesn't take a
    /// stray mouse-move to show the first Ultrawide frame.
    awaiting_canvas_snapshot: bool,
    /// Ticket W4-06a: the debug-viewer dock (pattern/nametable/palette/OAM/
    /// event viewers, `egui_dock` layout). Owns its own persisted layout
    /// and per-panel state — see `crate::debug_dock`'s module doc for why
    /// it stays a separate module rather than folding into this one.
    debug_panels: crate::debug_dock::DebugPanels,
    /// Whether `crate::core_thread::CoreCommand::SetEventMask` was last
    /// sent with the event viewer's bits included — mirrors `running`'s
    /// "set right after sending a command" pattern above, so the app only
    /// re-sends the command when `debug_panels.wants_event_subscription()`
    /// actually *changes* rather than every single repaint.
    event_subscription_active: bool,
}

impl RetroForgeApp {
    #[must_use]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Ticket W4-03e: build the ultrawide compositor's `GpuContext` from
        // the SAME device/queue egui itself renders with
        // (`rf_renderer::GpuContext::from_shared`'s own doc) — never a
        // second `request_headless()` device, which would force every
        // composited frame through an extra GPU->CPU->GPU round trip.
        // `wgpu_render_state` is `None` only if this build ever ran on the
        // glow backend (not expected: eframe 0.35's *default* feature set
        // is `wgpu`, verified against `eframe-0.35.0/Cargo.toml`'s own
        // `default` array — `docs/TECH_STACK.md` §2 records this), in
        // which case Ultrawide degrades to visibly unavailable rather than
        // panicking.
        let (gpu, compositor) = match &cc.wgpu_render_state {
            Some(rs) => {
                let gpu = rf_renderer::GpuContext::from_shared(
                    rs.device.clone(),
                    rs.queue.clone(),
                    rs.adapter.get_info(),
                    rs.adapter.limits(),
                );
                let compositor = rf_renderer::EnhancedCompositor::new(&gpu);
                (Some(gpu), Some(compositor))
            }
            None => (None, None),
        };
        RetroForgeApp {
            core: None,
            texture: None,
            bg_layer_texture: None,
            sprite_layer_texture: None,
            status: "No ROM loaded \u{2014} File > Open ROM...".to_string(),
            crash: None,
            running: false,
            awaiting_stepped_frame: false,
            position: None,
            input_latch: rf_input::InputLatch::new(),
            keymap: rf_input::KeyMap::default_nes(),
            sprite_overlay: false,
            show_layers: false,
            gpu,
            compositor,
            camera: CameraToggle::Original,
            ultrawide_canvas: None,
            ultrawide_render: None,
            ultrawide_texture: None,
            fm13_message: None,
            ultrawide_refresh_countdown: 0,
            awaiting_canvas_snapshot: false,
            debug_panels: crate::debug_dock::DebugPanels::new(),
            event_subscription_active: false,
        }
    }

    /// Sample the fixed [`POLLED_KEYS`] list once per repaint into
    /// [`Self::input_latch`], then publish the resulting `InputFrame` to
    /// the core thread (module doc). A no-op if no core is loaded — there
    /// is nothing to publish to.
    fn poll_input(&mut self, ctx: &egui::Context) {
        ctx.input(|input_state| {
            for key in POLLED_KEYS {
                let Some(mapped) = input_map::map_key(key) else {
                    continue;
                };
                if input_state.key_down(key) {
                    self.input_latch.key_down(mapped);
                } else {
                    self.input_latch.key_up(mapped);
                }
            }
        });
        if let Some(core) = &self.core {
            core.input.store(self.input_latch.sample(&self.keymap));
        }
    }

    fn open_rom(&mut self) {
        let Some(path) = rom_open::pick_rom_file() else {
            return; // user cancelled the dialog
        };
        let bytes = match rom_open::load_rom_bytes(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status = format!("Failed to open ROM: {e}");
                return;
            }
        };
        // Ticket W4-06a: extract CHR *before* `bytes` moves into
        // `core_thread::spawn` below — the pattern viewer's only in-scope
        // data path (`rf_debugger::pattern`'s module doc: rf-debugger may
        // not depend on rf-nes at all, so this crate is the mediator).
        // Best-effort and independent of the real load below: a parse
        // failure here just means no CHR preview (`chr_rom` stays `None`),
        // never a reason to fail opening the ROM — `core_thread::spawn`'s
        // own `EmuStepper::from_ines_bytes` call is the authoritative
        // load path and reports its own error separately. `chr_is_ram()`
        // cartridges have no static pattern data to show at all (module
        // doc point 1) — filtered out here rather than in the viewer, so
        // the viewer's `None` always means "no data", never "zeroed RAM
        // dressed up as ROM content".
        let chr_rom = rf_nes::NesRom::from_ines_bytes(&bytes)
            .ok()
            .filter(|rom| !rom.chr_is_ram())
            .map(|rom| rom.chr_rom().to_vec());
        match core_thread::spawn(bytes) {
            Ok(handle) => {
                self.core = Some(handle);
                // A new ROM is a new debug session too — the previous
                // ROM's OAM/events would otherwise linger onscreen against
                // a completely different game (same reasoning the
                // Ultrawide-camera reset below already uses).
                self.debug_panels.data.chr_rom = chr_rom;
                self.debug_panels.data.oam = [0u8; 256];
                self.debug_panels.data.events = Vec::new();
                // A fresh `EmuStepper` (inside `core_thread::spawn` below)
                // starts back at `stepper::CAMERA_BASELINE_EVENT_MASK` —
                // if the event-viewer panel was already open before this
                // reload, `event_subscription_active` would otherwise
                // still read `true` from the OLD core and
                // `sync_event_subscription`'s "only send when it changes"
                // guard would skip re-sending `SetEventMask` to the NEW
                // one, silently leaving it under-subscribed. Resetting
                // here forces the very next `sync_event_subscription` call
                // to re-send, regardless of whether the panel's open/closed
                // state itself changed.
                self.event_subscription_active = false;
                self.crash = None;
                self.texture = None;
                self.bg_layer_texture = None;
                self.sprite_layer_texture = None;
                self.running = false;
                // A freshly loaded ROM's core boots in Accuracy Mode (law
                // 6) — mirror that in the checkbox too, rather than leaving
                // a previous ROM's overlay choice looking still-checked
                // against a core that just reset it.
                self.sprite_overlay = false;
                // Ticket W4-03e: a new ROM is a new session for the
                // enhanced camera too — the previous ROM's stitched canvas
                // must not linger onscreen (or get composited into) against
                // a completely different game. Camera resets to Original,
                // matching "a fresh install boots in Accuracy Mode" (law 6).
                self.camera = CameraToggle::Original;
                self.ultrawide_canvas = None;
                self.ultrawide_render = None;
                self.ultrawide_texture = None;
                self.fm13_message = None;
                self.ultrawide_refresh_countdown = 0;
                self.awaiting_canvas_snapshot = false;
                self.status = format!("Loaded {}", path.display());
            }
            Err(e) => {
                self.status = format!("Failed to load ROM: {e}");
            }
        }
    }

    fn send_command(&self, cmd: CoreCommand) {
        if let Some(core) = &self.core {
            // The core thread only ever disappears if it already crashed
            // (FM-01) and drained its channel; a send failing here just
            // means the crash report is already on its way/arrived.
            let _ = core.cmd_tx.send(cmd);
        }
    }

    /// Drain every pending [`CoreEvent`] this repaint, keeping only the
    /// latest frame (older ones are stale by the time we'd paint them) and
    /// latching a crash report the moment one arrives.
    fn pump_core_events(&mut self, ctx: &egui::Context) {
        let Some(core) = &self.core else { return };
        let mut latest_frame = None;
        let mut latest_canvas = None;
        let mut crashed = false;
        while let Ok(evt) = core.evt_rx.try_recv() {
            match evt {
                CoreEvent::Frame(msg) => latest_frame = Some(msg),
                // Ticket W4-03e: keep only the latest, same "older ones are
                // stale by the time we'd paint them" reasoning this
                // function's own doc already gives for `latest_frame`.
                CoreEvent::CanvasSnapshot(canvas) => latest_canvas = Some(canvas),
                CoreEvent::Crashed(report) => {
                    self.crash = Some(report);
                    self.running = false;
                    crashed = true;
                    break;
                }
            }
        }
        // Ticket W4-06a: read out of `core.frame_bundle` (an owned clone of
        // its `events`) here, AFTER draining `evt_rx` above but BEFORE the
        // canvas-snapshot handling below (which calls `&mut self` methods
        // the borrow checker cannot allow alongside a live borrow through
        // `core`). Ordering matters for more than the borrow checker: the
        // core thread always publishes a frame's `FrameBundle` before
        // sending its matching `CoreEvent::Frame` (`core_thread::
        // core_thread_main`'s own doc), so reading `frame_bundle` only
        // after `latest_frame` is captured guarantees these `events`
        // belong to a bundle at least as fresh as `latest_frame` below —
        // reading it any earlier could race a bundle published between the
        // two reads and pair a frame with a stale event log.
        let latest_bundle_events = core.frame_bundle.latest().events.clone();
        if crashed {
            // FM-01's recovery row is "Reload ROM / load last state" — the
            // core thread has already halted for good (`run_guarded_loop`
            // never retries), so drop the dead handle now rather than
            // leaving `self.core` pointing at a channel nobody reads.
            // `has_core` in `controls_bar` then goes false on its own,
            // which disables Run/Step Frame/Step Scanline until the user
            // opens a ROM again — no separate "is there a live core"
            // check needed anywhere else.
            self.core = None;
            self.status = "Core crashed (FM-01) — open a ROM to start a fresh session".to_string();
            return;
        }
        if let Some(canvas) = latest_canvas {
            // Ticket W2-14's lesson, mirrored: the reply has now landed.
            self.awaiting_canvas_snapshot = false;
            self.ultrawide_canvas = Some(canvas);
            self.refresh_ultrawide_render(ctx);
        }
        if let Some(msg) = latest_frame {
            // Ticket W2-14: a stepped frame has now been consumed.
            self.awaiting_stepped_frame = false;
            // Ticket W2-15: position travels with the frame.
            self.position = Some((msg.frame_count, msg.last_scanline));
            // Ticket W4-06a: OAM travels with the frame the same way
            // (`core_thread::FrameMsg::oam`'s own doc); the event FIFO is
            // read from the triple-buffered `FrameBundle` instead — it is
            // NOT carried on `FrameMsg` (that would duplicate a stream
            // that already crosses the thread boundary on its own,
            // `core_thread::CoreHandle::frame_bundle`'s doc). Cloning one
            // frame's (usually short, EventMask-gated) event `Vec` here is
            // cheap relative to the RGBA texture uploads already happening
            // in this same block.
            self.debug_panels.data.oam = *msg.oam;
            self.debug_panels.data.events = latest_bundle_events;
            let image =
                egui::ColorImage::from_rgba_unmultiplied([msg.width, msg.height], &msg.rgba);
            match &mut self.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::NEAREST),
                None => {
                    self.texture =
                        Some(ctx.load_texture("nes-frame", image, egui::TextureOptions::NEAREST));
                }
            }
            // Ticket W3-03 acceptance criterion 2: keep the debug layer
            // textures current every frame regardless of whether the
            // "Layers (debug)" window is currently shown — cheap relative
            // to the main texture upload above, and avoids a stale image
            // flashing the instant the window is toggled on mid-session.
            let bg_image =
                egui::ColorImage::from_rgba_unmultiplied([msg.width, msg.height], &msg.bg_rgba);
            match &mut self.bg_layer_texture {
                Some(tex) => tex.set(bg_image, egui::TextureOptions::NEAREST),
                None => {
                    self.bg_layer_texture = Some(ctx.load_texture(
                        "nes-frame-bg-layer",
                        bg_image,
                        egui::TextureOptions::NEAREST,
                    ));
                }
            }
            let sprite_image =
                egui::ColorImage::from_rgba_unmultiplied([msg.width, msg.height], &msg.sprite_rgba);
            match &mut self.sprite_layer_texture {
                Some(tex) => tex.set(sprite_image, egui::TextureOptions::NEAREST),
                None => {
                    self.sprite_layer_texture = Some(ctx.load_texture(
                        "nes-frame-sprite-layer",
                        sprite_image,
                        egui::TextureOptions::NEAREST,
                    ));
                }
            }
        }
        if self.running || self.awaiting_stepped_frame || self.awaiting_canvas_snapshot {
            // Keep repainting while running so the core thread's frames
            // keep getting picked up (CPU blit, "live frames" criterion),
            // and likewise until a requested step's frame (ticket W2-14) or
            // a requested canvas snapshot (ticket W4-03e, same lesson) has
            // landed — while paused nothing else would wake the UI to
            // consume it.
            ctx.request_repaint();
        }
    }

    /// Ticket W4-03e acceptance criterion 1: translate the current
    /// [`Self::ultrawide_canvas`] into a `CompositeLayer`-composited RGBA
    /// buffer (`crate::enhanced_view::compose_ultrawide` — this crate is
    /// the mediator, `ARCHITECTURE.md` §3), upload it as
    /// [`Self::ultrawide_texture`], and surface any FM-13 reduction
    /// (criterion 3) as [`Self::fm13_message`]. A no-op if this build has
    /// no GPU device ([`Self::gpu`]/[`Self::compositor`] both `None`) or no
    /// canvas has ever arrived yet.
    fn refresh_ultrawide_render(&mut self, ctx: &egui::Context) {
        let (Some(gpu), Some(compositor)) = (&self.gpu, &self.compositor) else {
            self.ultrawide_render = Some(Err(
                "no GPU device available for the ultrawide view".to_string()
            ));
            self.ultrawide_texture = None;
            self.fm13_message = None;
            return;
        };
        let Some(canvas) = &self.ultrawide_canvas else {
            return;
        };
        // FM-13 POLICY half (`crate::enhanced_view` module doc): the real
        // adapter limit, never a hardcoded constant.
        let policy_max_dim = gpu.adapter_limits.max_texture_dimension_2d;
        let result = enhanced_view::compose_ultrawide(gpu, compositor, canvas, policy_max_dim);

        // Criterion 3: surface (never swallow) whatever the latest render
        // says about FM-13 — `None` here means the latest render genuinely
        // needed no reduction, not that one was dropped.
        self.fm13_message = result
            .as_ref()
            .ok()
            .and_then(enhanced_view::UltrawideRender::fm13_message);

        match &result {
            Ok(render) => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [render.width as usize, render.height as usize],
                    &render.rgba,
                );
                match &mut self.ultrawide_texture {
                    Some(tex) => tex.set(image, egui::TextureOptions::NEAREST),
                    None => {
                        self.ultrawide_texture = Some(ctx.load_texture(
                            "ultrawide-frame",
                            image,
                            egui::TextureOptions::NEAREST,
                        ));
                    }
                }
            }
            Err(_) => {
                self.ultrawide_texture = None;
            }
        }
        self.ultrawide_render = Some(result);
    }

    /// Ticket W4-03e: keep the Ultrawide view live while it's the active
    /// camera, without cloning the whole stitched canvas every repaint
    /// (module-level [`CANVAS_SNAPSHOT_REFRESH_INTERVAL`] doc).
    fn maybe_request_canvas_snapshot(&mut self) {
        if self.camera != CameraToggle::Ultrawide || self.core.is_none() {
            return;
        }
        if self.ultrawide_refresh_countdown == 0 {
            self.send_command(CoreCommand::RequestCanvasSnapshot);
            self.awaiting_canvas_snapshot = true;
            self.ultrawide_refresh_countdown = CANVAS_SNAPSHOT_REFRESH_INTERVAL;
        } else {
            self.ultrawide_refresh_countdown -= 1;
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open ROM...").clicked() {
                        self.open_rom();
                        ui.close();
                    }
                });
            });
        });
    }

    fn controls_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("controls").show(ui, |ui| {
            ui.horizontal(|ui| {
                let has_core = self.core.is_some();
                let run_label = if self.running { "Pause" } else { "Run" };
                if ui
                    .add_enabled(has_core, egui::Button::new(run_label))
                    .clicked()
                {
                    self.running = !self.running;
                    self.send_command(if self.running {
                        CoreCommand::Resume
                    } else {
                        CoreCommand::Pause
                    });
                }
                if ui
                    .add_enabled(has_core, egui::Button::new("Step Frame"))
                    .clicked()
                {
                    self.running = false;
                    self.awaiting_stepped_frame = true;
                    self.send_command(CoreCommand::StepFrame);
                }
                if ui
                    .add_enabled(has_core, egui::Button::new("Step Scanline"))
                    .clicked()
                {
                    self.running = false;
                    self.awaiting_stepped_frame = true;
                    self.send_command(CoreCommand::StepScanline);
                }
                ui.separator();
                // Ticket W3-05a, FR-ENH-001: opt-in only, off by default
                // (law 6) — checking this does not touch the accuracy
                // simulation, only whether the dropped-sprite overlay gets
                // composited on top of it (`Ppu`'s module doc, "Sprite-
                // limit-bypass overlay" section).
                if ui
                    .add_enabled(
                        has_core,
                        egui::Checkbox::new(&mut self.sprite_overlay, "De-flicker overlay"),
                    )
                    .changed()
                {
                    self.send_command(CoreCommand::SetSpriteOverlay(self.sprite_overlay));
                }
                ui.separator();
                // Ticket W3-03 acceptance criterion 2: pure UI-thread
                // state, no core command — the layer textures are already
                // kept current every frame in `pump_core_events`
                // regardless of this checkbox, so toggling it just shows/
                // hides the window with no round trip to the core thread.
                ui.checkbox(&mut self.show_layers, "Layers (debug)");
                ui.separator();
                // Ticket W4-06a criterion 3: layout is saved the moment the
                // window closes (not only on process exit via
                // `eframe::App::save` below), so a session that opens,
                // rearranges panels, and closes without a clean shutdown
                // still keeps the change.
                if ui
                    .checkbox(&mut self.debug_panels.visible, "Debug Viewers")
                    .changed()
                    && !self.debug_panels.visible
                {
                    self.debug_panels.save();
                }
                ui.separator();
                // Ticket W4-03e acceptance criterion 2: the runtime camera
                // toggle. Disabled with no compositor at all (no GPU
                // device, `Self::compositor` doc) — there is nothing to
                // switch to in that case, and enabling the button would
                // just click through to `enhanced_view::ActiveView::
                // UltrawideUnavailable` every time.
                let camera_label = match self.camera {
                    CameraToggle::Original => "Camera: Original",
                    CameraToggle::Ultrawide => "Camera: Ultrawide",
                };
                if ui
                    .add_enabled(
                        has_core && self.compositor.is_some(),
                        egui::Button::new(camera_label),
                    )
                    .clicked()
                {
                    self.camera = self.camera.flipped();
                    if self.camera == CameraToggle::Ultrawide {
                        // Don't wait out the throttle interval for the
                        // FIRST view after switching — request now.
                        self.ultrawide_refresh_countdown = 0;
                    }
                }
                if self.compositor.is_none() {
                    ui.label("(no GPU device for ultrawide)");
                }
                // FM-13 criterion 3: "view too large for GPU, reduced" —
                // surfaced plainly, never swallowed
                // (`Self::refresh_ultrawide_render`'s doc).
                if let Some(msg) = &self.fm13_message {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(230, 180, 40), msg);
                }
                ui.separator();
                ui.label(&self.status);
                if let Some((frame, scanline)) = self.position {
                    ui.separator();
                    ui.monospace(match scanline {
                        Some(y) => format!("frame {frame} \u{b7} scanline {y}"),
                        None => format!("frame {frame} \u{b7} scanline --"),
                    });
                }
            });
        });
    }

    fn crash_dialog(&mut self, ctx: &egui::Context) {
        let Some(report) = self.crash.clone() else {
            return;
        };
        egui::Window::new("Core crashed")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("The emulator core panicked and was contained (FM-01); the core thread has halted.");
                ui.label(format!("Message: {}", report.message));
                if let Some(loc) = &report.location {
                    ui.label(format!("Location: {loc}"));
                }
                ui.separator();
                ui.label("Trace tail:");
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    ui.monospace(&report.trace_tail);
                });
                ui.separator();
                if ui.button("Dismiss").clicked() {
                    self.crash = None;
                }
            });
    }

    /// Ticket W3-03 acceptance criterion 2: shows the BG-only and
    /// sprite-only layers ([`Self::bg_layer_texture`]/
    /// [`Self::sprite_layer_texture`], kept current every frame in
    /// [`Self::pump_core_events`]) side by side in their own window, so a
    /// viewer can see the extraction is real without cross-referencing the
    /// main composited frame at all. The sprite layer's transparent
    /// (`rf_renderer::LayeredFrame` module doc) areas show through to
    /// egui's panel background, which is exactly what makes "isolated"
    /// visible: a game with few on-screen sprites renders as a
    /// mostly-empty pane, not a black one.
    fn layers_debug_window(&mut self, ctx: &egui::Context) {
        if !self.show_layers {
            return;
        }
        egui::Window::new("Layers (debug)")
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label("Background layer");
                        match &self.bg_layer_texture {
                            Some(tex) => {
                                ui.add(
                                    egui::Image::from_texture(tex)
                                        .max_width(256.0)
                                        .maintain_aspect_ratio(true),
                                );
                            }
                            None => {
                                ui.label("(no frame yet)");
                            }
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.label("Sprite layer");
                        match &self.sprite_layer_texture {
                            Some(tex) => {
                                ui.add(
                                    egui::Image::from_texture(tex)
                                        .max_width(256.0)
                                        .maintain_aspect_ratio(true),
                                );
                            }
                            None => {
                                ui.label("(no frame yet)");
                            }
                        }
                    });
                });
            });
    }

    /// Ticket W4-06a criteria 1-3: the debug-viewer dock window
    /// (pattern/nametable/palette/OAM/event panels + persisted
    /// `egui_dock` layout). Same "own window, toggled by a checkbox" shape
    /// [`Self::layers_debug_window`] already uses.
    fn debug_panels_window(&mut self, ctx: &egui::Context) {
        if !self.debug_panels.visible {
            return;
        }
        egui::Window::new("Debug Viewers")
            .collapsible(true)
            .resizable(true)
            .default_size([640.0, 480.0])
            .show(ctx, |ui| {
                self.debug_panels.ui(ui);
            });
    }

    /// Ticket W4-06a: DEBUGGER.md §6's "closed panels register no event
    /// subscriptions" — sends `CoreCommand::SetEventMask` only when
    /// [`crate::debug_dock::DebugPanels::wants_event_subscription`]'s
    /// answer actually *changes*, not every repaint (mirrors
    /// [`Self::event_subscription_active`]'s doc). `EmuStepper::
    /// set_event_mask` always re-asserts the camera baseline regardless of
    /// what's requested here (that function's own doc), so narrowing to
    /// `NONE` when every debug window is closed can never starve
    /// `crate::canvas_accum`.
    fn sync_event_subscription(&mut self) {
        let wants = self.debug_panels.visible && self.debug_panels.wants_event_subscription();
        if wants == self.event_subscription_active {
            return;
        }
        self.event_subscription_active = wants;
        let mask = if wants {
            rf_core_api::EventMask::ALL
        } else {
            rf_core_api::EventMask::NONE
        };
        self.send_command(CoreCommand::SetEventMask(mask));
    }

    /// Ticket W4-03e acceptance criterion 2: paints whichever camera view
    /// [`enhanced_view::select_active_view`] resolves to — the ONLY branch
    /// point between Original and Ultrawide, so a mutation that hardcodes
    /// `ActiveView::Original` here (or upstream) is exactly what that
    /// function's own tests (`enhanced_view::tests::
    /// ultrawide_toggle_with_a_ready_render_shows_ultrawide_content_not_original`)
    /// are written to catch. The Original arm is untouched from before this
    /// ticket (criterion 4: Accuracy Mode's own output, unmodified).
    fn video_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            match enhanced_view::select_active_view(self.camera, self.ultrawide_render.as_ref()) {
                enhanced_view::ActiveView::Original => {
                    if let Some(texture) = &self.texture {
                        ui.add(egui::Image::from_texture(texture).shrink_to_fit());
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label(&self.status);
                        });
                    }
                }
                enhanced_view::ActiveView::Ultrawide { .. } => {
                    if let Some(texture) = &self.ultrawide_texture {
                        ui.add(egui::Image::from_texture(texture).shrink_to_fit());
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("Ultrawide view: preparing texture\u{2026}");
                        });
                    }
                }
                enhanced_view::ActiveView::UltrawideUnavailable(reason) => {
                    ui.centered_and_justified(|ui| {
                        ui.label(format!("Ultrawide view unavailable: {reason}"));
                    });
                }
            }
        });
    }
}

impl eframe::App for RetroForgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_input(&ctx);
        self.pump_core_events(&ctx);
        self.maybe_request_canvas_snapshot();
        self.sync_event_subscription();

        self.menu_bar(ui);
        self.controls_bar(ui);
        self.video_panel(ui);
        self.crash_dialog(&ctx);
        self.layers_debug_window(&ctx);
        self.debug_panels_window(&ctx);
    }

    // Ticket W4-06a criterion 3: `eframe::App::save`/`auto_save_interval`
    // (its own periodic-autosave hook) is deliberately NOT overridden here.
    // Verified against `eframe-0.35.0`'s own `Cargo.toml`, not assumed: it
    // is only ever called when this crate's `eframe` dependency enables the
    // `persistence` feature ("Only called when the 'persistence' feature is
    // enabled", `epi::App::save`'s own doc), which pulls in `ron`/`home` as
    // new transitive dependencies — a new-crate licence surface the ticket
    // brief is explicit about ("Any other new crate does [need a
    // docs/TECH_STACK.md row] — stop and report instead"), and `docs/**` is
    // outside this ticket's write scope regardless. An override here would
    // therefore be dead code today — never invoked, never exercised by any
    // test — exactly the kind of unverified-because-unreachable path this
    // ticket's own "vacuity trap" guidance warns against. The "Debug
    // Viewers" checkbox's explicit `debug_panels.save()` on close
    // (`Self::controls_bar`) is therefore the ONLY persistence trigger this
    // build has; a session ended by `kill -9` or an OS-level force-quit
    // loses whatever layout change happened since the last checkbox
    // toggle. Recorded here as the cleaner long-term answer: enabling
    // `persistence` (a `docs/TECH_STACK.md`-and-`deny.toml` decision, both
    // outside this write scope) would add eframe's own native periodic
    // save as a second, real trigger.
}
