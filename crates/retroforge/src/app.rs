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
use rf_enhance::trust::TrustState;

/// How many repaints [`RetroForgeApp::maybe_request_canvas_snapshot`] lets
/// pass between `CoreCommand::RequestCanvasSnapshot` sends while the
/// Ultrawide camera is active — a fresh snapshot on every repaint would
/// clone the whole stitched `Canvas` at ~60Hz (`CanvasAccumulator::
/// current_canvas`'s own doc: "tens of MB/s for a level of any real
/// size"). 30 repaints is roughly twice a second at the app's normal
/// repaint cadence — frequent enough that Ultrawide visibly keeps up with
/// play, far below the cost of a per-frame clone.
const CANVAS_SNAPSHOT_REFRESH_INTERVAL: u32 = 30;

/// The size `main.rs` opens the window at, and the size
/// `tests/hud_fits.rs` proves the HUD fits inside (ticket W10-01).
///
/// 768 is three whole NES pixels wide (256 x 3), which is the point: the
/// default window shows the picture at an integer scale
/// (`settings::ScaleMode::Integer`) with nothing to spare. That makes it
/// the *tightest* case the chrome has to survive, so it is the one worth
/// asserting — a HUD tested only at some comfortable developer size is a
/// HUD tested at a size no user runs.
pub const WINDOW_SIZE: [f32; 2] = [768.0, 720.0];

/// Floor for manual resizing. Below this the status bar cannot hold its
/// own contents and would start clipping again — the exact failure
/// W10-01 exists to fix — so the window refuses to get there rather than
/// silently hiding controls.
pub const MIN_WINDOW_SIZE: [f32; 2] = [640.0, 480.0];

/// How many frames [`RetroForgeApp::note_frame`] averages before it will
/// report an FPS reading. Averaging rather than timing one frame: a
/// single-frame reading at 60 Hz swings several frames per second on
/// scheduler noise alone, and a number that never settles is one nobody
/// can use to answer "is this running full speed?".
const FPS_WINDOW_FRAMES: u32 = 30;

/// Where a floating panel window first appears (ticket W10-01).
///
/// **Below the menu bar, deliberately.** egui's default area position is
/// the top-left corner, which in this app is exactly where File / View /
/// Enhance live — so every window opened from the View menu came up
/// covering the only navigation the app has, and the menu you opened it
/// from was underneath it. Found because `tests/ui_smoke.rs` clicked
/// "View" and hit the Controls window instead; it is a real defect, not
/// a test artifact, and one nobody would have reported as a bug so much
/// as a vague sense that the app fights back.
const PANEL_WINDOW_ORIGIN: [f32; 2] = [24.0, 56.0];

/// Longest profile name the status chip will lay out, in characters.
///
/// The chip's text is a profile's file stem, so its width is decided by
/// the *user's filesystem*, not by this code — unbounded, and therefore
/// able to re-create W10-01's exact failure (a bar wider than the window)
/// at run time on a build whose tests are green because the fixture is
/// called `fixture.nes`. It is a chip: a fixed budget with the full value
/// on hover is what a chip is.
///
/// The status *text* is not bounded this way — it gets
/// `egui::Label::truncate`, which fits it to the space actually left
/// after the fixed readouts rather than to a guess about how wide a
/// character is.
const PROFILE_CHIP_BUDGET: usize = 20;

/// The non-ASCII characters this UI draws in the **proportional** font,
/// and the only ones it may draw (ticket W10-01 polish pass).
///
/// egui's bundled font does not cover the Geometric Shapes block evenly,
/// and a missing glyph renders as a tofu box rather than failing — so it
/// is invisible to every test and visible only in a screenshot. This
/// project shipped `◆` that way, then shipped `◇` as its "fix", which is
/// also absent. `hud_tests::every_glyph_the_ui_draws_exists_in_the_font`
/// asks the font instead of guessing.
///
/// This is what the UI **draws**, not what the font happens to have —
/// `○` and `■` are present and were used until the A/V indicator moved
/// to words, and they came straight back out. A vocabulary listing
/// glyphs nothing renders is a claim nobody checks.
///
/// Verified present (wider than this list): `·` `…` `—` `○` `■` `★` `☆` `›`.
/// Verified ABSENT, do not use: `◆` `◇` `●` `▸` `▪` `▫` `□`.
pub const PROPORTIONAL_GLYPHS: &str = "\u{b7}\u{2026}\u{2014}\u{2605}\u{2606}\u{203a}";

/// The non-ASCII characters drawn in the **monospace** font.
///
/// Empty, and that is a finding rather than an oversight: the bundled
/// monospace face has **no** `·`, `…` or `—` at all, and the
/// frame/scanline readout used to render `f12 · sl34` in monospace —
/// a tofu box between every frame number.
pub const MONOSPACE_GLYPHS: &str = "";

/// One of §3.2's ambient readouts, at [`egui::TextStyle::Small`].
///
/// A named helper rather than `RichText::small()` at five call sites,
/// because the point is that these four things are ONE class of thing
/// and share one treatment — five independent `.small()` calls is how a
/// type scale drifts back into five sizes.
fn readout(text: impl Into<egui::RichText>) -> egui::Label {
    // `selectable(false)`: these are readouts, and a text cursor
    // appearing over the FPS counter is an affordance that leads nowhere.
    egui::Label::new(text.into().small()).selectable(false)
}

/// `text` shortened to [`PROFILE_CHIP_BUDGET`] characters, keeping the
/// **end** and eliding the front.
///
/// The end, because both strings this is used on are paths and the
/// distinguishing part of a path is its tail: `…/mario/prg/level1.nes`
/// tells you something, `/Users/someone/very/long/…` tells you nothing.
/// Counts `chars`, not bytes — slicing a UTF-8 path mid-codepoint would
/// panic, and a ROM directory with a non-ASCII name is completely
/// ordinary.
fn elide_front(text: &str) -> std::borrow::Cow<'_, str> {
    let count = text.chars().count();
    if count <= PROFILE_CHIP_BUDGET {
        return std::borrow::Cow::Borrowed(text);
    }
    let skip = count - (PROFILE_CHIP_BUDGET - 1);
    std::borrow::Cow::Owned(format!(
        "\u{2026}{}",
        text.chars().skip(skip).collect::<String>()
    ))
}

/// Every host key the default NES keymap binds — the fixed poll list
/// `poll_input` checks each repaint (module doc).
/// Which app-wide settings tab is showing (ticket W2-08; FRONTEND_UI §2's
/// Settings tree). Input has its own window (W2-06's Controls) and Plugins
/// belongs to W4-04, so this build's tabs are the three W2-08 owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Video,
    Audio,
    Paths,
    /// Ticket W10-01. `crate::accessibility` shipped in W8-04 with a UI
    /// scale and a high-contrast palette and no way for a user to reach
    /// either. This tab is that way.
    Accessibility,
}

/// Open the gamepad backend, or carry on without one (ticket W2-06).
/// A missing or unopenable gamepad subsystem is not an error: the keyboard
/// still works, and refusing to start over it would be absurd.
#[cfg(feature = "gamepad")]
fn pad_backend_or_none() -> Option<rf_input::GilrsBackend> {
    match rf_input::GilrsBackend::new() {
        Ok(backend) => Some(backend),
        Err(e) => {
            eprintln!("retroforge: gamepads unavailable ({e}); keyboard only");
            None
        }
    }
}

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
    /// Ticket W10-01: frames-per-second for the status bar, `None` until
    /// [`FPS_WINDOW_FRAMES`] frames have been seen. This is the first
    /// answer the app has ever been able to give to "is it running at
    /// full speed?" — before W10-01 the bar had no FPS at all, despite
    /// `docs/design/FRONTEND_UI.md` §3.2 listing it as one of four things
    /// the status bar shows.
    /// Ticket W10-01: audio-buffer fill (0.0..=1.0) as of the last frame,
    /// for §3.2's A/V sync indicator. `None` until a frame carries one.
    /// The core thread owns `AudioOut`, so this arrives with the frame
    /// rather than being read from here — the UI thread never touches the
    /// audio device.
    audio_fill: Option<f32>,
    /// Ticket W10-01 polish pass: whether the Run button was hovered on
    /// the previous frame — the state `animate_bool_responsive` needs to
    /// ease between fills. An immediate-mode button has no memory of its
    /// own, so without this the hover is a step function.
    run_hovered: bool,
    /// Ticket W10-03: §3.1's search box, filtering the library home by
    /// title. Not persisted — a search is a gesture within a session, and
    /// an app that reopened tomorrow still filtered by "castle" would be
    /// hiding the user's library with no visible cause.
    library_search: String,
    /// §3.1's console filter. `None` is "every console", deliberately
    /// rather than a `Console::All` variant: `All` would be a console that
    /// does not exist, and every match on `Console` in the codebase would
    /// then have to handle it.
    library_console_filter: Option<crate::library::Console>,
    /// How many folder walks this session has done (ticket W10-03).
    /// Counted so a test can prove the play-then-close round trip does
    /// not re-scan — see `library_scan_count_for_test`.
    library_scans: u32,
    /// Ticket W10-02: the Enhance workspace (FRONTEND_UI §3.3's
    /// [Compare][Features][Map]). Its own dock, with its own tab type in
    /// `crate::enhance_dock` — deliberately NOT `rf_debugger`'s
    /// `DebugTab`, which is a lower-layer type and the thing
    /// `PersistedLayout` serialises.
    enhance: crate::enhance_dock::EnhanceWorkspace,
    /// Ticket W10-01: the rect the status bar's right-aligned readouts
    /// occupied last frame, and the right edge of the transport controls
    /// to their left. The bar is correct exactly when the first does not
    /// reach back past the second.
    ///
    /// Instrumentation, because the accessibility tree cannot answer this
    /// question: egui publishes AccessKit nodes for the bar's *buttons*
    /// but **not for its plain labels**, so a test walking that tree can
    /// see Run and Step Frame clip and is structurally blind to the FPS,
    /// A/V, profile-chip and status readouts — the four things
    /// FRONTEND_UI §3.2 actually specifies. Verified by dumping every
    /// node in the panel: only `Role::Button` appears. Measured here
    /// instead, and asserted by `tests/hud_fits.rs`.
    status_readouts: Option<(egui::Rect, f32)>,
    fps: Option<f32>,
    /// Frames counted since [`Self::fps_window_start`].
    fps_frames: u32,
    /// Wall clock at the start of the current averaging window.
    fps_window_start: std::time::Instant,
    /// Host-agnostic per-frame input latch (`rf_input`, FR-FE-003) — the
    /// UI thread's write side; `poll_input` samples it every repaint into
    /// the core thread's `SharedInputFrame` (module doc).
    input_latch: rf_input::InputLatch,
    /// Keyboard + gamepad bindings (ticket W2-06), loaded from the user's
    /// config at startup and written back the moment a remap changes —
    /// see `crate::bindings_store`.
    bindings: rf_input::Bindings,
    /// Where those bindings live, `None` when the platform gave us no
    /// config directory (a sandboxed or headless run): remapping still
    /// works for the session, it just cannot be saved, and the UI says so.
    config_root: Option<std::path::PathBuf>,
    /// Pad-to-port assignment and held state (ticket W2-06). Present even
    /// without the `gamepad` feature, because the routing rules are what
    /// the frontend reads; with no backend it simply stays empty.
    pad_router: rf_input::PadRouter,
    /// UI navigation driven by the same pad events the emulated ports see
    /// (ticket W8-04). Present without the `gamepad` feature for the same
    /// reason `pad_router` is: the model is what the frontend reads, and
    /// with no backend it simply never sees an event.
    ui_nav: crate::ui_nav::GamepadNav,
    /// When the last `poll_input` ran, so `GamepadNav::tick` gets a real
    /// delta rather than an assumed frame time. Auto-repeat measured in
    /// frames would speed up on a fast display and crawl on a slow one —
    /// the repeat rate a user feels must be wall-clock.
    last_nav_tick: Option<std::time::Instant>,
    /// The live gamepad backend, when this build has one and the platform
    /// let us open it.
    #[cfg(feature = "gamepad")]
    pad_backend: Option<rf_input::GilrsBackend>,
    /// Whether the Controls (remap) window is open.
    show_controls: bool,
    /// Whether the app-wide Settings window is open (ticket W2-08).
    show_settings: bool,
    /// Which Settings tab is showing.
    settings_tab: SettingsTab,
    /// App-wide settings, loaded at startup and written back on change.
    settings: crate::settings::AppSettings,
    /// Whether the Esc overlay menu is showing (FRONTEND_UI §2).
    show_overlay_menu: bool,
    /// Configured library roots, as the user chose them (resolved at scan
    /// time, never stored canonicalized — see `crate::library_roots`).
    library_roots: Vec<std::path::PathBuf>,
    /// Last scan's result. `None` until the first scan, which is why the
    /// window scans on open rather than at startup: a cold start must not
    /// wait on a folder walk over a network share.
    library: Option<crate::library::Library>,
    /// Normalized hash of the ROM currently loaded (ticket W2-07) — the key
    /// its per-game settings are stored under. `None` for a ROM this build
    /// could not identify, which is deliberate: settings keyed by a hash we
    /// could not compute would be settings that silently apply to the wrong
    /// game later.
    current_game_hash: Option<String>,
    /// The current game's settings, loaded on open and written back the
    /// moment one changes.
    current_game_settings: crate::game_settings::GameSettings,
    /// What the remap UI is waiting to capture, if anything: the
    /// `(port, button)` a next key press should bind.
    awaiting_key: Option<(usize, rf_input::NesButton)>,
    /// Last thing the binding store said, shown in the Controls window so a
    /// failed save is visible rather than silent.
    bindings_status: String,
    /// Set when a key-capture completed inside the input closure, which
    /// cannot call `save_bindings` itself (it holds a borrow of `self`).
    pending_binding_save: bool,
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
    /// Ticket W3-04 (FR-REND-005): how the original/enhanced compare view
    /// is presented, or `Off`. Pure UI state — the comparison itself is
    /// `rf_renderer::compare`'s pure functions, which is where its tests
    /// live.
    /// Ticket W4-05: held while the user is peeking at the original
    /// (FRONTEND_UI.md §1's hold-to-peek). Not persisted — it is a
    /// momentary gesture, not a setting.
    peeking_original: bool,
    /// Whether a profile matched this ROM (FR-PROF-005). `false` until a
    /// profile loader is wired; the inspector says so rather than
    /// implying a match.
    profile_matched: bool,
    show_enhance: bool,
    compare_mode: rf_renderer::CompareMode,
    /// Divider position for `CompareMode::Split`, kept across toggles so
    /// turning compare off and on again does not reset the drag.
    compare_divider: f32,
    /// Ticket W3-04 (FR-FE-005): the two buffers the last frame produced,
    /// kept ONLY while something needs them.
    ///
    /// Deliberately not populated unconditionally: W3-03a removed exactly
    /// this shape of per-frame cost (two ~240 KB clones for a window
    /// nobody had open), and re-adding it here for a compare view that is
    /// off by default would have undone that ticket a day later. Populated
    /// when compare mode is on, or for the single frame a screenshot is
    /// pending.
    compare_buffers: Option<CompareBuffers>,
    /// Set by the Screenshot action; consumed by the next frame, which is
    /// the first one whose buffers are guaranteed to exist.
    screenshot_pending: bool,
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
    /// Ticket W4-10a: the UI-thread half of the trace transport, present
    /// only while a capture is armed.
    trace_drain: Option<crate::trace_capture::TraceDrain>,
    /// The background lz4 writer, present only while writing to file.
    trace_writer: Option<crate::trace_capture::TraceFileWriter>,
    /// Ticket W4-11: the save-state manager modal's open flag and the
    /// slot listing it draws. The listing is refreshed when the modal
    /// opens and after every save/load, not per frame — it is a directory
    /// scan, and a modal that stat()ed thirteen files at 60 Hz would be
    /// paying for a picture that changes when the user presses a button.
    show_states: bool,
    state_slots: Vec<crate::state_slots::SlotInfo>,
    /// Warnings from the most recent load, shown in the modal and
    /// summarised in the status line (FRONTEND_UI §3.2's last clause).
    state_warnings: Vec<String>,
    /// Ticket W5-06: the author workspace (FRONTEND_UI §3.5). `None`
    /// until a profile is matched — there is nothing to author against
    /// before then.
    author_watch: Option<crate::authoring::Watch>,
    author_outcome: crate::authoring::ReloadOutcome,
    show_author: bool,
    /// The open ROM, header-stripped — what `[[rom_map]]` offsets are
    /// relative to, and therefore what the authoring loop must decode
    /// against (`rf_enhance::decode`'s module doc).
    normalized_rom: Option<Vec<u8>>,
    /// Path of the profile matched to the open ROM, if any.
    matched_profile: Option<std::path::PathBuf>,
    /// Ticket W9-02: the in-GUI profile editor. `None` until the author
    /// creates or opens one — the workspace is useful without it (the
    /// external-editor loop W5-06 built), so the editor is a mode of the
    /// panel rather than the panel itself.
    editor_draft: Option<crate::profile_editor::Draft>,
    editor_form: crate::profile_editor::NewProfileForm,
    /// Where a never-yet-saved draft will be written. Held as text
    /// because it is a text field the author types into.
    editor_path: String,
    /// The last save's outcome, shown next to the button that caused it.
    editor_status: Option<String>,
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

        // Ticket W2-06: load the user's bindings before the first frame, so
        // a remapped controller works from the first key press rather than
        // after some later "apply".
        let config_root = crate::bindings_store::config_root();
        let (bindings, bindings_status) = match &config_root {
            Some(root) => {
                let (bindings, outcome) = crate::bindings_store::load(root);
                let status = match outcome {
                    crate::bindings_store::LoadOutcome::Defaulted => {
                        "Using default bindings (no config file yet).".to_string()
                    }
                    crate::bindings_store::LoadOutcome::Loaded(warnings) if warnings.is_empty() => {
                        format!(
                            "Bindings loaded from {}.",
                            crate::bindings_store::bindings_path(root).display()
                        )
                    }
                    crate::bindings_store::LoadOutcome::Loaded(warnings) => format!(
                        "Bindings loaded with {} skipped line(s): {warnings:?}",
                        warnings.len()
                    ),
                    crate::bindings_store::LoadOutcome::Rejected(reason) => format!(
                        "Binding file could not be read ({reason}); using defaults. Your file \
                         was left untouched."
                    ),
                };
                (bindings, status)
            }
            None => (
                rf_input::Bindings::default(),
                "No config directory on this platform; remapping works for this session only."
                    .to_string(),
            ),
        };

        // Ticket W2-08: app-wide settings load at startup. A parse
        // problem is reported into the status line rather than swallowed;
        // the defaults are in force and the user's file is untouched.
        let (app_settings, settings_problem) = match &config_root {
            Some(root) => crate::settings::load(root),
            None => (crate::settings::AppSettings::default(), None),
        };
        let bindings_status = match settings_problem {
            Some(problem) => format!("{bindings_status}  {problem}"),
            None => bindings_status,
        };

        // Ticket W2-07: the configured folders load at startup (cheap: one
        // small file), but the SCAN waits until the library window opens —
        // a cold start must not block on a folder walk over a network
        // share.
        let library_roots = config_root
            .as_ref()
            .map(|root| crate::library_roots::load(root))
            .unwrap_or_default();

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
            audio_fill: None,
            run_hovered: false,
            library_search: String::new(),
            library_console_filter: None,
            library_scans: 0,
            enhance: crate::enhance_dock::EnhanceWorkspace::new(),
            status_readouts: None,
            fps: None,
            fps_frames: 0,
            fps_window_start: std::time::Instant::now(),
            input_latch: rf_input::InputLatch::new(),
            bindings,
            config_root,
            pad_router: rf_input::PadRouter::new(),
            ui_nav: crate::ui_nav::GamepadNav::new(),
            last_nav_tick: None,
            #[cfg(feature = "gamepad")]
            pad_backend: pad_backend_or_none(),
            show_controls: false,
            show_settings: false,
            settings_tab: SettingsTab::Video,
            settings: app_settings,
            show_overlay_menu: false,
            library_roots: library_roots.clone(),
            library: None,
            current_game_hash: None,
            current_game_settings: crate::game_settings::GameSettings::default(),
            awaiting_key: None,
            bindings_status,
            pending_binding_save: false,
            sprite_overlay: false,
            show_layers: false,
            gpu,
            compositor,
            camera: CameraToggle::Original,
            ultrawide_canvas: None,
            peeking_original: false,
            profile_matched: false,
            show_enhance: false,
            compare_mode: rf_renderer::CompareMode::Off,
            compare_divider: 0.5,
            compare_buffers: None,
            screenshot_pending: false,
            ultrawide_render: None,
            ultrawide_texture: None,
            fm13_message: None,
            ultrawide_refresh_countdown: 0,
            awaiting_canvas_snapshot: false,
            debug_panels: crate::debug_dock::DebugPanels::new(),
            event_subscription_active: false,
            trace_drain: None,
            trace_writer: None,
            show_states: false,
            state_slots: Vec::new(),
            state_warnings: Vec::new(),
            author_watch: None,
            author_outcome: crate::authoring::ReloadOutcome::default(),
            show_author: false,
            normalized_rom: None,
            matched_profile: None,
            editor_draft: None,
            editor_form: crate::profile_editor::NewProfileForm::default(),
            editor_path: String::new(),
            editor_status: None,
        }
    }

    /// Sample every bindable key once per repaint into
    /// [`Self::input_latch`], then publish the resulting `InputFrame` to
    /// the core thread (module doc). A no-op if no core is loaded — there
    /// is nothing to publish to.
    fn poll_input(&mut self, ctx: &egui::Context) {
        // Ticket W2-06: poll every key a binding could name, not W1-07's
        // fixed eight — a user who binds Start to `Q` must have `Q` reach
        // the keymap, and before this the translation dropped it first.
        ctx.input(|input_state| {
            for &key in rf_input::Key::ALL {
                let Some(egui_key) = input_map::egui_key_for(key) else {
                    continue;
                };
                if !input_state.key_down(egui_key) {
                    self.input_latch.key_up(key);
                    continue;
                }
                // A remap in progress swallows the press rather than also
                // feeding it to the game -- otherwise binding Start would
                // press Start at the same moment.
                if let Some((port, button)) = self.awaiting_key.take() {
                    self.bindings.keys.rebind(key, port, button);
                    self.pending_binding_save = true;
                    continue;
                }
                self.input_latch.key_down(key);
            }
        });

        // The pads, then the OR: neither input wins, because a player using
        // a pad while a hand rests on the keyboard should not have one
        // silently cancel the other.
        // ONE poll, two consumers. `PadBackend::poll` DRAINS, so calling
        // it once for the emulated ports and again for UI navigation
        // would hand each of them roughly half the events and produce a
        // d-pad that moves the menu on some presses and the game on
        // others. So the events are taken once here and fanned out:
        // `PadRouter::apply` for the ports (what `poll` does internally),
        // and `GamepadNav` for the UI.
        #[cfg(feature = "gamepad")]
        if let Some(backend) = self.pad_backend.as_mut() {
            // Fully-qualified: `PadBackend` is not imported in this file,
            // and a `use` inside a cfg block would be dead in a default
            // build. Note this whole block is `#[cfg(feature =
            // "gamepad")]`, so a DEFAULT `cargo build` never type-checks
            // it — verify with `--features gamepad`.
            let events = rf_input::PadBackend::poll(backend);
            self.pad_router.apply(&events);
            let actions = self.ui_nav.on_events(&events);
            self.push_nav_events(ctx, &actions);
        }

        // Auto-repeat is wall-clock, not per-frame — see `last_nav_tick`.
        let now = std::time::Instant::now();
        let dt = self
            .last_nav_tick
            .map_or(std::time::Duration::ZERO, |prev| now - prev);
        self.last_nav_tick = Some(now);
        let repeated = self.ui_nav.tick(dt);
        self.push_nav_events(ctx, &repeated);
        if let Some(core) = &self.core {
            let mut frame = self.input_latch.sample(&self.bindings.keys);
            let pads = self.pad_router.sample(&self.bindings.pads);
            for (port, bits) in frame.ports.iter_mut().enumerate() {
                *bits |= pads.ports[port];
            }
            core.input.store(frame);
        }

        if std::mem::take(&mut self.pending_binding_save) {
            self.save_bindings();
        }
    }

    /// Push navigation actions into egui as real input events.
    ///
    /// **They go through `RawInput`, not through a bespoke focus model**,
    /// which is the whole reason this tier works: egui's own focus is
    /// what AccessKit reports, so a gamepad that drives it is visible to
    /// a screen reader and to the headless harness alike. A parallel
    /// model would have been more code, invisible to the accessibility
    /// tree, and free to drift from what the keyboard does.
    ///
    /// A repaint is requested only when something actually happened —
    /// asking every frame would keep the UI awake on an idle pad.
    fn push_nav_events(&mut self, ctx: &egui::Context, actions: &[crate::ui_nav::NavAction]) {
        if actions.is_empty() {
            return;
        }
        let events = crate::ui_nav::GamepadNav::events_for(actions);
        if events.is_empty() {
            return;
        }
        ctx.input_mut(|i| i.events.extend(events));
        ctx.request_repaint();
    }

    /// Whether a frame from the core has actually reached the screen —
    /// `texture` is uploaded only in `video_panel`, and only from a
    /// `FrameBundle` the core thread delivered.
    ///
    /// `pub` since ticket W4-09 (FR-DBG-007's sibling, NFR-004): the UI
    /// smoke test needs to assert "first frame arrived", and there is no
    /// way to observe that through the accessibility tree — an
    /// `egui::Image` contributes no labelled node, so a test that only
    /// queried AccessKit could watch a permanently black window and call
    /// it a pass. This reports the shipped field rather than a test-only
    /// mirror of it.
    #[must_use]
    pub fn has_presented_frame(&self) -> bool {
        self.texture.is_some()
    }

    /// The status line's current text (ticket W4-09). Same reasoning as
    /// [`RetroForgeApp::has_presented_frame`]: the status line is drawn as
    /// a label inside a panel and does not surface as a queryable node, so
    /// a smoke test asserting "the ROM loaded rather than failed" has to
    /// read it directly.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Render a save's timestamp. Unix seconds as a plain date-time
    /// rather than "3 minutes ago": a relative label is unreadable in a
    /// screenshot, in a bug report, or after the session that produced it
    /// has ended, which is most of when someone reads this list.
    #[must_use]
    fn format_timestamp(secs: u64) -> String {
        // Deliberately arithmetic rather than a date crate: adding a
        // dependency for one label would need a docs/TECH_STACK.md row
        // and a licence review, which is a lot of process for a string.
        let days = secs / 86_400;
        let time = secs % 86_400;
        let (mut y, mut d) = (1970u64, days);
        loop {
            let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
            let len = if leap { 366 } else { 365 };
            if d < len {
                break;
            }
            d -= len;
            y += 1;
        }
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let months = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let mut m = 0usize;
        while m < 12 && d >= months[m] {
            d -= months[m];
            m += 1;
        }
        format!(
            "{y:04}-{:02}-{:02} {:02}:{:02}",
            m + 1,
            d + 1,
            time / 3600,
            (time % 3600) / 60
        )
    }

    /// Point the state manager at a game hash without opening a ROM
    /// (ticket W4-11's harness test).
    ///
    /// Test-only in practice but not `#[cfg(test)]`: an integration test
    /// lives in another crate and cannot see a cfg-gated method, and a
    /// second door into the same field would be a door the shipped code
    /// does not use. `open_rom_path` sets the same field on the real
    /// path.
    pub fn set_game_hash_for_test(&mut self, hash: Option<String>) {
        self.current_game_hash = hash;
    }

    /// Poll the watched profile and re-decode if it changed (ticket
    /// W5-06, FRONTEND_UI §3.5's "hot-reloads on save").
    ///
    /// Called once per repaint alongside `pump_trace`, and cheap by
    /// construction: one `fs::metadata` unless something actually
    /// changed. Gated on the panel being open, so a session that never
    /// opens the author workspace does not stat a file at 60 Hz — the
    /// same pay-for-use rule DEBUGGER.md §6 sets for the debugger.
    fn pump_authoring(&mut self) {
        if !self.show_author {
            return;
        }
        let Some(watch) = self.author_watch.as_mut() else {
            return;
        };
        if !watch.poll() {
            return;
        }
        let path = watch.path().to_path_buf();
        let rom = self.normalized_rom.clone();
        self.author_outcome = crate::authoring::reload(&path, rom.as_deref());
    }

    /// Point the author workspace at a profile and open it.
    pub fn open_author_workspace(&mut self, profile_path: std::path::PathBuf) {
        self.author_outcome =
            crate::authoring::reload(&profile_path, self.normalized_rom.as_deref());
        self.author_watch = Some(crate::authoring::Watch::new(profile_path));
        self.show_author = true;
    }

    /// Open the editor on a brand-new draft (ticket W9-02).
    ///
    /// Public because the menu, a future "new profile" command and the
    /// headless harness all need the same entry point; a test that had to
    /// synthesise clicks to reach the editor's *initial* state would be
    /// testing egui's click routing rather than the editor.
    pub fn open_author_editor(&mut self) {
        self.editor_draft = Some(crate::profile_editor::Draft::from_form(&self.editor_form));
        self.editor_status = None;
        self.show_author = true;
    }

    /// The open draft, mutably \u{2014} the headless harness's way to put a
    /// specific buffer in front of the real widget code without typing it
    /// character by character through synthesised key events.
    pub fn author_draft_mut(&mut self) -> Option<&mut crate::profile_editor::Draft> {
        self.editor_draft.as_mut()
    }

    /// Where the editor's Save button will write.
    pub fn set_author_save_path(&mut self, path: impl Into<String>) {
        self.editor_path = path.into();
    }

    /// The last save attempt's message, as shown in the panel.
    #[must_use]
    pub fn author_editor_status(&self) -> Option<&str> {
        self.editor_status.as_deref()
    }

    /// Open the editor on the profile currently being watched.
    pub fn edit_watched_profile(&mut self) {
        let Some(path) = self.author_watch.as_ref().map(|w| w.path().to_path_buf()) else {
            return;
        };
        match crate::profile_editor::Draft::open(&path) {
            Ok(draft) => {
                self.editor_path = path.display().to_string();
                self.editor_draft = Some(draft);
                self.editor_status = None;
            }
            Err(e) => self.editor_status = Some(format!("{}: {e}", path.display())),
        }
        self.show_author = true;
    }

    /// §3.5's embedded text editor plus the inspector's "validation
    /// output" pane (ticket W9-02).
    ///
    /// The validation shown here is `Draft::check`, which is
    /// `rf_profiles::loader::load_str` — so this pane cannot drift from
    /// what the loader will say, and the Save button below it cannot
    /// write a file the loader would refuse. The loader is fail-fast, so
    /// there is at most one error at a time; the pane says as much rather
    /// than implying a complete list.
    fn profile_editor_ui(&mut self, ui: &mut egui::Ui) {
        use crate::profile_editor::{Check, Console, NewProfileForm};

        if self.editor_draft.is_none() {
            let mut want_new = false;
            let mut want_edit = false;
            let has_watch = self.author_watch.is_some();
            ui.horizontal(|ui| {
                want_new = ui.button("New profile").clicked();
                if has_watch {
                    want_edit = ui.button("Edit this profile").clicked();
                }
            });
            if want_new {
                self.open_author_editor();
            } else if want_edit {
                self.edit_watched_profile();
            }
            if self.editor_draft.is_some() {
                return;
            }
            let form: &mut NewProfileForm = &mut self.editor_form;
            egui::Grid::new("rf_profile_new_form")
                .num_columns(2)
                .show(ui, |ui| {
                    for (label, field) in [
                        ("Title", &mut form.title),
                        ("Region", &mut form.region),
                        ("Author", &mut form.author),
                        ("Source citation", &mut form.source),
                    ] {
                        let _ = ui.selectable_label(false, label);
                        ui.add(
                            egui::TextEdit::singleline(field)
                                .desired_width(320.0)
                                .hint_text(label),
                        );
                        ui.end_row();
                    }
                    let _ = ui.selectable_label(false, "Console");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut form.console, Console::Nes, "NES");
                        ui.radio_value(&mut form.console, Console::Snes, "SNES");
                    });
                    ui.end_row();
                });
            if let Some(status) = &self.editor_status {
                let _ = ui.selectable_label(false, status.clone());
            }
            return;
        }
        let draft = self.editor_draft.as_mut().expect("checked above");

        // ---- validation, above the buffer -------------------------
        //
        // Above, not below, for the same reason the decode errors sit
        // above the decode preview: a verdict placed under a screenful of
        // text is a verdict nobody scrolls to.
        let check = draft.check();
        match &check {
            Check::Accepted { warnings } => {
                let _ = ui.selectable_label(
                    false,
                    egui::RichText::new("Valid \u{2014} rf-profiles loads this")
                        .color(egui::Color32::from_rgb(0x40, 0xC0, 0x60)),
                );
                for w in warnings {
                    let _ = ui.selectable_label(
                        false,
                        egui::RichText::new(format!("unknown key: {w}"))
                            .color(egui::Color32::from_rgb(0xE0, 0x80, 0x30)),
                    );
                }
            }
            Check::Refused { diagnostic } => {
                let _ = ui.selectable_label(
                    false,
                    egui::RichText::new(format!("Invalid: {diagnostic}"))
                        .color(egui::Color32::from_rgb(0xE0, 0x50, 0x40)),
                );
                let _ = ui.selectable_label(
                    false,
                    "The loader stops at the first problem, so more may follow this one.",
                );
            }
        }

        ui.horizontal(|ui| {
            let _ = ui.selectable_label(false, "Save to");
            ui.add(
                egui::TextEdit::singleline(&mut self.editor_path)
                    .desired_width(420.0)
                    .hint_text("path/to/profile.toml"),
            );
        });

        let mut close = false;
        ui.horizontal(|ui| {
            // Disabled rather than absent when the draft is invalid: a
            // button that vanishes reads as a missing feature, a greyed
            // one reads as a blocked action, and the diagnostic directly
            // above it says what unblocks it.
            let can_save = check.is_accepted() && !self.editor_path.trim().is_empty();
            if ui
                .add_enabled(can_save, egui::Button::new("Save profile"))
                .clicked()
            {
                let path = std::path::PathBuf::from(self.editor_path.trim());
                match draft.save_to(&path) {
                    Ok(warnings) => {
                        self.editor_status = Some(if warnings.is_empty() {
                            format!("saved {}", path.display())
                        } else {
                            format!(
                                "saved {} with {} warning(s)",
                                path.display(),
                                warnings.len()
                            )
                        });
                        // Point the watch at what was just written, so
                        // §3.5's hot-reload loop closes without a second
                        // preview path: `pump_authoring` takes it from
                        // here.
                        self.author_outcome =
                            crate::authoring::reload(&path, self.normalized_rom.as_deref());
                        self.author_watch = Some(crate::authoring::Watch::new(path));
                    }
                    Err(refused) => {
                        self.editor_status = Some(format!("not saved: {}", refused.diagnostic));
                    }
                }
            }
            close = ui.button("Close editor").clicked();
            if draft.is_dirty() {
                let _ = ui.selectable_label(false, "unsaved changes");
            }
        });
        if close {
            self.editor_draft = None;
            self.editor_status = None;
            return;
        }
        let draft = self.editor_draft.as_mut().expect("still present");

        if let Some(status) = &self.editor_status {
            let _ = ui.selectable_label(false, status.clone());
        }

        let _ = ui.selectable_label(false, "Profile TOML");
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .id_salt("rf_profile_editor_text")
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(draft.text_mut())
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(16)
                        .hint_text("Profile TOML"),
                );
            });
    }

    /// The author workspace (FRONTEND_UI §3.5, minimal Phase-4 form:
    /// live decode preview + inline error list).
    fn author_window(&mut self, ctx: &egui::Context) {
        if !self.show_author {
            return;
        }
        let mut open = true;
        egui::Window::new("Author")
            .open(&mut open)
            .resizable(true)
            .show(ctx, |ui| {
                match self.author_watch.as_ref() {
                    Some(w) => {
                        let _ =
                            ui.selectable_label(false, format!("watching {}", w.path().display()));
                    }
                    None => {
                        let _ = ui.selectable_label(
                            false,
                            "No profile matched this ROM \u{2014} create one below.",
                        );
                    }
                }
                ui.separator();
                // Ticket W9-02. Above the decode preview, and before the
                // `author_watch` check that used to end this closure: the
                // editor's whole point is that a profile can be brought
                // into existence when there is no profile yet, so it
                // cannot sit behind a control that requires one.
                self.profile_editor_ui(ui);
                if self.author_watch.is_none() {
                    return;
                }
                ui.separator();
                // §3.5's "error list inline". Errors first: a preview
                // shown above its own errors invites the author to read
                // the stale picture and miss why it is stale.
                for e in &self.author_outcome.errors {
                    let _ = ui.selectable_label(
                        false,
                        egui::RichText::new(e.line())
                            .color(egui::Color32::from_rgb(0xE0, 0x50, 0x40)),
                    );
                }
                for w in &self.author_outcome.warnings {
                    let _ = ui.selectable_label(
                        false,
                        egui::RichText::new(w).color(egui::Color32::from_rgb(0xE0, 0x80, 0x30)),
                    );
                }
                ui.separator();
                match &self.author_outcome.level {
                    Some(level) => {
                        let _ = ui.selectable_label(
                            false,
                            format!("preview {}x{} metatiles", level.width, level.height),
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(crate::authoring::preview_text(level, 48, 14))
                                    .monospace(),
                            )
                            .sense(egui::Sense::hover()),
                        );
                    }
                    None => {
                        let _ = ui.selectable_label(false, "no preview");
                    }
                }
            });
        self.show_author = open;
    }

    /// Keep the Audio tab fed and act on its buttons (ticket W4-10b).
    ///
    /// Same shape as `pump_trace`: the panel is a draw function with no
    /// channel, so it records a request and this acts on it. Capture is
    /// turned on when the debug dock opens the tab and off when it
    /// closes, so a session that never looks at the scopes never pays for
    /// them (DEBUGGER.md §6).
    fn pump_audio_scopes(&mut self) {
        if self.core.is_none() {
            self.debug_panels.data.audio = None;
            return;
        }
        let panel = self.debug_panels.data.audio.get_or_insert_with(|| {
            Box::new(crate::debug_dock::AudioPanelData {
                traces: Vec::new(),
                mute: rf_debugger::audio_scope::MuteState::new(),
                request: None,
                capturing: false,
            })
        });

        let Some(request) = panel.request.take() else {
            return;
        };
        match request {
            crate::debug_dock::AudioRequest::Start => {
                panel.capturing = true;
                self.send_command(CoreCommand::SetAudioChannelCapture(true));
            }
            crate::debug_dock::AudioRequest::Stop => {
                panel.capturing = false;
                panel.traces.clear();
                self.send_command(CoreCommand::SetAudioChannelCapture(false));
            }
            crate::debug_dock::AudioRequest::ClearMutes => panel.mute.clear(),
        }
    }

    /// Refresh the slot listing and open the manager (ticket W4-11).
    pub fn open_states_modal(&mut self) {
        self.state_slots = match self.states_dir() {
            Some(dir) => crate::state_slots::scan(&dir),
            // No config directory (or no ROM open): show the empty grid
            // rather than nothing, so the modal still explains itself.
            None => crate::state_slots::SlotId::all()
                .into_iter()
                .map(|id| crate::state_slots::SlotInfo { id, saved: None })
                .collect(),
        };
        self.show_states = true;
    }

    /// Where this game's states live, or `None` with no config dir or no
    /// ROM open.
    fn states_dir(&self) -> Option<std::path::PathBuf> {
        Some(crate::state_slots::slots_dir(
            self.config_root.as_ref()?,
            self.current_game_hash.as_ref()?,
        ))
    }

    /// The save-state manager (FRONTEND_UI §3.2).
    fn states_modal(&mut self, ctx: &egui::Context) {
        if !self.show_states {
            return;
        }
        let mut open = true;
        let mut action: Option<(crate::state_slots::SlotId, bool)> = None;
        egui::Window::new("Save states")
            .open(&mut open)
            .resizable(true)
            .show(ctx, |ui| {
                if self.states_dir().is_none() {
                    ui.label("No ROM open — save states are per game.");
                }
                for info in &self.state_slots {
                    ui.horizontal(|ui| {
                        let _ = ui.selectable_label(false, info.id.label());
                        match &info.saved {
                            Some(saved) => {
                                // The two flags FRONTEND_UI §3.2 names,
                                // plus the timestamp.
                                //
                                // `Label::sense(hover)` rather than a
                                // bare `ui.label`: a plain label
                                // contributes NO node to the
                                // accessibility tree, so a screen reader
                                // — and W4-09's harness, which is the
                                // same tree — cannot see the mode badge
                                // or the mods warning at all. Ticket
                                // W4-09 recorded this trap for the
                                // emulator viewport; it applies to any
                                // information-bearing label, and these
                                // three are the ones a user opens this
                                // modal to read.
                                // `selectable_label`, not `Label` with a
                                // hover sense: measured against the real
                                // accessibility tree, neither a bare
                                // `ui.label` NOR a hover-sensed `Label`
                                // contributes a node, so both are
                                // invisible to a screen reader and to
                                // W4-09's harness. A selectable label
                                // renders the same and is a real widget.
                                let badge = |ui: &mut egui::Ui, text: String| {
                                    let _ = ui.selectable_label(false, text);
                                };
                                badge(ui, saved.mode.label().to_string());
                                if saved.contains_mods {
                                    let _ = ui.selectable_label(
                                        false,
                                        egui::RichText::new("\u{26a0} contains mods")
                                            .color(egui::Color32::from_rgb(0xE0, 0x80, 0x30)),
                                    );
                                }
                                badge(ui, Self::format_timestamp(saved.timestamp));
                                badge(
                                    ui,
                                    if saved.thumbnail.is_some() {
                                        "thumbnail".to_string()
                                    } else {
                                        "no thumbnail".to_string()
                                    },
                                );
                                if ui.button(format!("Load {}", info.id.label())).clicked() {
                                    action = Some((info.id, false));
                                }
                            }
                            None => {
                                let _ = ui.selectable_label(false, "empty");
                            }
                        }
                        if ui.button(format!("Save {}", info.id.label())).clicked() {
                            action = Some((info.id, true));
                        }
                    });
                }
                if !self.state_warnings.is_empty() {
                    ui.separator();
                    for line in &self.state_warnings {
                        ui.colored_label(egui::Color32::from_rgb(0xE0, 0x80, 0x30), line);
                    }
                }
            });
        if let Some((slot, is_save)) = action {
            if is_save {
                self.save_to_slot(slot);
            } else {
                self.load_from_slot(slot);
            }
        }
        self.show_states = open;
    }

    fn save_to_slot(&mut self, slot: crate::state_slots::SlotId) {
        let Some(dir) = self.states_dir() else {
            self.status = "No ROM open".to_string();
            return;
        };
        self.status = format!("Saving {}\u{2026}", slot.label());
        self.send_command(CoreCommand::SaveStateToSlot {
            dir,
            stem: slot.stem(),
        });
        // The core thread writes the file; re-scan on the next open so
        // the listing reflects it rather than guessing it succeeded.
        self.state_warnings.clear();
    }

    fn load_from_slot(&mut self, slot: crate::state_slots::SlotId) {
        let Some(dir) = self.states_dir() else {
            return;
        };
        match crate::state_slots::load(&dir, slot, &rf_state::MigrationRegistry::default()) {
            Ok((container, warnings)) => {
                // §3.2's last clause. Surfaced in the modal AND the status
                // line: a warning only visible in a modal the user is
                // about to close is a warning they will not read.
                self.state_warnings = crate::state_slots::warning_lines(&warnings);
                self.status = if self.state_warnings.is_empty() {
                    format!("Loaded {}", slot.label())
                } else {
                    format!(
                        "Loaded {} with {} warning(s)",
                        slot.label(),
                        self.state_warnings.len()
                    )
                };
                self.send_command(CoreCommand::ApplyState(Box::new(container)));
            }
            Err(e) => self.status = format!("Load failed: {e}"),
        }
    }

    fn open_rom(&mut self) {
        let Some(path) = rom_open::pick_rom_file() else {
            return; // user cancelled the dialog
        };
        self.open_rom_path(&path);
    }

    /// Open a ROM by path (ticket W2-07: the library's Play button uses
    /// this, the File menu's picker calls it with what the user chose).
    /// One body, so a game launched from the library goes through exactly
    /// the same load path as one opened by hand.
    ///
    /// `pub` since ticket W4-09, for one reason worth stating: the native
    /// file dialog is the single step of the boot-to-first-frame flow that
    /// no headless harness can drive (`rfd` opens a real OS window). The
    /// UI smoke test therefore calls what the dialog's callback calls,
    /// which is this — the same body the library's Play button uses — so
    /// everything after the file picker is the shipped path rather than a
    /// test-only one.
    pub fn open_rom_path(&mut self, path: &std::path::Path) {
        let bytes = match rom_open::load_rom_bytes(path) {
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
        // Ticket W2-07 (FR-FE-002): identify the ROM and load its settings
        // BEFORE the core starts, so a game configured for Enhanced mode
        // opens in it rather than flipping a frame later.
        self.current_game_hash = match rf_cart::Cartridge::load(&bytes) {
            Ok(rf_cart::Cartridge::Nes { identity, .. }) => Some(identity.normalized.sha256),
            Ok(rf_cart::Cartridge::Snes { identity, .. }) => Some(identity.normalized.sha256),
            Err(_) => None,
        };
        // Ticket W5-06: keep the header-stripped image and find the
        // profile that claims this ROM, so the author workspace has both
        // the bytes to decode and the file to watch. Both are `None` for
        // a ROM nobody has written a profile for, which is the normal
        // case and never an error.
        self.normalized_rom = Some(
            bytes
                .strip_prefix(b"NES\x1a")
                .map_or_else(|| bytes.clone(), |_| bytes[16..].to_vec()),
        );
        self.matched_profile = self.current_game_hash.as_ref().and_then(|hash| {
            crate::level_view::find_matching_profile(std::path::Path::new("profiles"), hash)
                .map(|(_, path)| path)
        });

        self.current_game_settings = match (&self.config_root, &self.current_game_hash) {
            (Some(root), Some(hash)) => crate::game_settings::load(root, hash),
            _ => crate::game_settings::GameSettings::default(),
        };

        match core_thread::spawn(bytes) {
            Ok(handle) => {
                self.core = Some(handle);
                // A new ROM is a new debug session too — the previous
                // ROM's OAM/events would otherwise linger onscreen against
                // a completely different game (same reasoning the
                // Ultrawide-camera reset below already uses).
                self.debug_panels.data.chr_rom = chr_rom;
                self.debug_panels.data.oam = [0u8; 256];
                self.debug_panels.data.previous_oam = [0u8; 256];
                self.debug_panels.data.events = Vec::new();
                self.debug_panels.data.wram = [0u8; 0x0800];
                self.debug_panels.data.prg_ram = [0u8; 0x2000];
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
                //
                // Ticket W2-07 (FR-FE-002): then apply THIS game's saved
                // setting on top. The default stays Accuracy/off, so law 6
                // still holds for a game nobody has configured; a game the
                // user turned the overlay on for gets it back, which is the
                // entire point of per-game settings.
                self.sprite_overlay = self.current_game_settings.sprite_overlay;
                if self.sprite_overlay {
                    self.send_command(CoreCommand::SetSpriteOverlay(true));
                }
                // Ticket W3-03a: a fresh core thread starts with layer
                // extraction OFF. If the Layers window was already open
                // before this reload, re-assert it — the identical
                // stale-state-across-a-reload hazard that
                // `event_subscription_active` is reset for above, and the
                // reason that reset has a paragraph of its own.
                if self.show_layers {
                    self.send_command(CoreCommand::SetLayerExtraction(true));
                }
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
        // Ticket W3-04: the accuracy-exact half of the compare pair, taken
        // from the SAME read ordering the comment above establishes, so
        // the two halves cannot land a frame apart. Cloned only when
        // something actually needs them (W3-03a's rule: a closed feature
        // costs nothing on the frame path).
        let want_compare_buffers =
            self.compare_mode != rf_renderer::CompareMode::Off || self.screenshot_pending;
        let latest_bundle_video = if want_compare_buffers {
            let b = core.frame_bundle.latest();
            Some((b.video.clone(), u32::from(b.width), u32::from(b.height)))
        } else {
            None
        };
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
            // Ticket W3-04: pair the two renderings of THIS frame.
            self.compare_buffers = latest_bundle_video.and_then(|(video, bw, bh)| {
                // Geometry must agree, or there is a scaling decision to
                // make that this ticket deliberately does not guess at —
                // drop the pair rather than compose a misaligned image.
                let (w, h) = (
                    u32::try_from(msg.width).ok()?,
                    u32::try_from(msg.height).ok()?,
                );
                if bw != w || bh != h || video.len() != (w as usize) * (h as usize) {
                    return None;
                }
                Some(CompareBuffers {
                    original: rf_renderer::original_rgba_from_indexed(&video, w, h),
                    enhanced: msg.rgba.clone(),
                    width: w,
                    height: h,
                })
            });
            if self.screenshot_pending {
                self.screenshot_pending = false;
                self.write_screenshots();
            }
            self.audio_fill = msg.audio_fill;
            self.note_frame();
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
            // Ticket W4-06c: roll the previous frame's OAM forward before
            // overwriting, so the diff panel has both sides. This is a
            // 256-byte copy of a buffer the frame already carried — not a
            // new clone of core state, and not gated behind the panel
            // being open, because 256 bytes is genuinely nothing next to
            // the ~245 KB frame it arrives with (contrast W3-03a, where
            // two 240 KB clones were worth gating).
            self.debug_panels.data.previous_oam = self.debug_panels.data.oam;
            self.debug_panels.data.oam = *msg.oam;
            if let Some(audio) = self.debug_panels.data.audio.as_mut() {
                if !msg.audio_traces.is_empty() {
                    audio.traces = msg.audio_traces.clone();
                }
            }
            // Ticket W4-06d: live VRAM/palette for the nametable and
            // palette viewers. Carried on the frame message like OAM, so
            // the UI thread never reaches into the core — the same
            // read-only discipline W4-06c's diff panel relies on.
            self.debug_panels.data.vram = *msg.vram;
            self.debug_panels.data.palette_ram = *msg.palette_ram;
            self.debug_panels.data.events = latest_bundle_events;
            // Ticket W4-06b: WRAM/PRG-RAM travel with the frame the same
            // way OAM already does — see `core_thread::FrameMsg::wram`'s
            // own doc.
            self.debug_panels.data.wram = *msg.wram;
            self.debug_panels.data.prg_ram = *msg.prg_ram;
            // Ticket W3-04 (FR-REND-005): when comparing, the displayed
            // image IS the composed comparison — one buffer, so what is
            // on screen and what a screenshot of the compare view would
            // show cannot disagree. `rf_renderer::compare` owns the
            // branching; this only picks the bytes.
            let compare_rgba = match (self.compare_mode, self.compare_buffers.as_ref()) {
                (rf_renderer::CompareMode::Off, _) | (_, None) => None,
                (rf_renderer::CompareMode::Split { divider }, Some(b)) => {
                    Some(rf_renderer::compose_split(
                        &b.original,
                        &b.enhanced,
                        b.width,
                        b.height,
                        divider,
                    ))
                }
                (rf_renderer::CompareMode::Blink { period_frames }, Some(b)) => {
                    if rf_renderer::blink_shows_original(msg.frame_count, period_frames) {
                        Some(b.original.clone())
                    } else {
                        Some(b.enhanced.clone())
                    }
                }
            };
            let displayed: &[u8] = compare_rgba.as_deref().unwrap_or(&msg.rgba);
            let image =
                egui::ColorImage::from_rgba_unmultiplied([msg.width, msg.height], displayed);
            match &mut self.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::NEAREST),
                None => {
                    self.texture =
                        Some(ctx.load_texture("nes-frame", image, egui::TextureOptions::NEAREST));
                }
            }
            // Ticket W3-03a: empty means the core thread is not extracting
            // layers (the window is closed), so there is nothing to
            // upload. Keyed off the data itself rather than a second copy
            // of `show_layers` on this side, which could disagree with
            // what the core thread is actually doing for the one frame a
            // command is in flight.
            if msg.bg_rgba.is_empty() || msg.sprite_rgba.is_empty() {
                return;
            }
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
    /// Drain the trace transport and act on whatever the Trace panel's
    /// buttons asked for (ticket W4-10a).
    ///
    /// Called once per repaint from `eframe::App::ui`, alongside
    /// `pump_core_events` and `sync_event_subscription` — the same
    /// pattern, for the same reason: the panel is a draw function with no
    /// channel of its own, so it records a request and this acts on it.
    fn pump_trace(&mut self) {
        if self.core.is_none() {
            self.debug_panels.data.trace = None;
            return;
        }
        let panel = self.debug_panels.data.trace.get_or_insert_with(|| {
            Box::new(crate::debug_dock::TracePanelData {
                scrollback: rf_debugger::trace::TraceScrollback::new(
                    crate::trace_capture::SCROLLBACK_CAPACITY,
                ),
                filter: rf_debugger::trace::TraceFilter::default(),
                request: None,
                armed: false,
                file: None,
                pc_from: String::new(),
                pc_to: String::new(),
            })
        });

        if let Some(drain) = self.trace_drain.as_mut() {
            drain.drain_into(&mut panel.scrollback);
        }

        let Some(request) = panel.request.take() else {
            return;
        };
        match request {
            crate::debug_dock::TraceRequest::Start => {
                let (producer, drain) = crate::trace_capture::channel(panel.filter.clone());
                self.trace_drain = Some(drain);
                panel.armed = true;
                self.send_command(CoreCommand::ArmTrace(Box::new(producer)));
            }
            crate::debug_dock::TraceRequest::Stop => {
                panel.armed = false;
                self.trace_drain = None;
                self.send_command(CoreCommand::DisarmTrace);
                self.finish_trace_file();
            }
            crate::debug_dock::TraceRequest::Clear => panel.scrollback.clear(),
            crate::debug_dock::TraceRequest::StartFile => {
                // Written under the config root rather than through a
                // native file dialog: `rfd` opens a real OS window, which
                // the W4-09 UI smoke test cannot drive, and a control that
                // no automated flow can exercise is one that regresses
                // unnoticed. The path is reported in the status line and
                // shown in the panel, so it is not hidden either.
                let filter = panel.filter.clone();
                match self.trace_file_path() {
                    Some(path) => match crate::trace_capture::TraceFileWriter::spawn(&path) {
                        Ok((writer, file_tx)) => {
                            let (producer, drain) =
                                crate::trace_capture::channel_with_file(filter, file_tx);
                            self.trace_drain = Some(drain);
                            if let Some(panel) = self.debug_panels.data.trace.as_mut() {
                                panel.armed = true;
                                panel.file = Some(path.clone());
                            }
                            self.trace_writer = Some(writer);
                            self.status = format!("Tracing to {}", path.display());
                            self.send_command(CoreCommand::ArmTrace(Box::new(producer)));
                        }
                        Err(e) => self.status = format!("Trace file failed: {e}"),
                    },
                    None => {
                        self.status =
                            "No config directory — nowhere to write a trace file".to_string();
                    }
                }
            }
            crate::debug_dock::TraceRequest::StopFile => self.finish_trace_file(),
        }
    }

    /// Where a trace file goes: `<config root>/traces/trace-<n>.lz4`,
    /// with `n` the number of files already there, so repeated captures
    /// in one session never overwrite each other.
    fn trace_file_path(&self) -> Option<std::path::PathBuf> {
        let dir = self.config_root.as_ref()?.join("traces");
        std::fs::create_dir_all(&dir).ok()?;
        let n = std::fs::read_dir(&dir).map(Iterator::count).unwrap_or(0);
        Some(dir.join(format!("trace-{n}.lz4")))
    }

    /// Stop the lz4 writer and report what it wrote.
    ///
    /// **Order matters and is the reason this is its own function:** the
    /// producer must be gone before `finish()` is called, or the writer
    /// thread is still waiting on a channel that will never close and
    /// `join` blocks the UI thread forever. `DisarmTrace` drops the
    /// producer on the core thread, which is why every caller sends that
    /// first.
    fn finish_trace_file(&mut self) {
        let Some(writer) = self.trace_writer.take() else {
            return;
        };
        let path = writer.path().to_path_buf();
        self.status = match writer.finish() {
            Ok(n) => format!("Trace written: {n} entries to {}", path.display()),
            Err(e) => format!("Trace file failed: {e}"),
        };
        if let Some(panel) = self.debug_panels.data.trace.as_mut() {
            panel.file = None;
        }
    }

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

    /// Count a delivered frame toward the status bar's FPS reading
    /// (ticket W10-01).
    ///
    /// Counts frames that actually *reached the screen*, not repaints:
    /// egui repaints for a mouse move, and an FPS number that rose when
    /// you wiggled the pointer would be measuring the wrong machine.
    /// Open or close the Controls window without going through the menu
    /// (ticket W10-01), so `tests/hud_fits.rs` can assert where a
    /// floating window lands without also depending on menu behaviour.
    #[doc(hidden)]
    pub fn show_controls_for_test(&mut self, show: bool) {
        self.show_controls = show;
    }

    fn note_frame(&mut self) {
        self.fps_frames += 1;
        if self.fps_frames < FPS_WINDOW_FRAMES {
            return;
        }
        let elapsed = self.fps_window_start.elapsed().as_secs_f32();
        // Guard the divide: a window that measures as zero seconds is a
        // clock-resolution artifact, not 'infinite frames per second'.
        if elapsed > 0.0 {
            self.fps = Some(self.fps_frames as f32 / elapsed);
        }
        self.fps_frames = 0;
        self.fps_window_start = std::time::Instant::now();
    }

    /// Apply the palette and UI scale from settings to egui (ticket
    /// W10-01).
    ///
    /// `crate::accessibility` has defined an AAA-verified accent, a
    /// high-contrast palette and a bounded `ui_scale` since W8-04, and
    /// until this function existed **nothing imported it**: the module's
    /// tests asserted the palette *arithmetic* was AAA-compliant, which
    /// passes forever whether or not egui ever draws with those colours.
    /// This is the line that makes those tests mean something.
    ///
    /// Style, not just colour. Every control in the old bar rendered as
    /// the same grey rectangle — the primary action, a disabled button
    /// and a *non-clickable status badge* were visually identical — so
    /// the accent is spent on giving the active thing a visible edge.
    fn apply_theme(&self, ctx: &egui::Context) {
        let a = self.settings.accessibility.normalized();
        let p = a.palette();
        let col = |c: [u8; 3]| egui::Color32::from_rgb(c[0], c[1], c[2]);
        let (bg, raised, text, muted, accent) = (
            col(p.background),
            col(p.raised),
            col(p.text),
            col(p.text_muted),
            col(p.accent),
        );

        let mut v = egui::Visuals::dark();
        v.panel_fill = bg;
        v.window_fill = bg;
        v.faint_bg_color = raised;
        v.extreme_bg_color = bg;
        v.override_text_color = Some(text);
        v.hyperlink_color = accent;
        v.window_stroke = egui::Stroke::new(1.0, raised);

        // The four widget states, given four DIFFERENT looks. This is
        // the whole point of the ticket's visual half: before W10-01 the
        // crate set no visuals at all, so the primary action, a disabled
        // button and a non-clickable status badge were three identical
        // grey rectangles and the eye had nothing to sort them by.
        //
        // `noninteractive` is the flat one — it is what a *label* gets,
        // and it must not look like something you can press.
        v.widgets.noninteractive.bg_fill = bg;
        v.widgets.noninteractive.weak_bg_fill = bg;
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, raised);
        v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, muted);

        // `inactive` is a resting control: it sits on `raised`, which is
        // what says "this is a thing, and you may press it".
        v.widgets.inactive.bg_fill = raised;
        v.widgets.inactive.weak_bg_fill = raised;
        v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text);

        v.widgets.hovered.bg_fill = raised.lerp_to_gamma(accent, 0.18);
        v.widgets.hovered.weak_bg_fill = raised.lerp_to_gamma(accent, 0.18);
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent);
        v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, text);

        v.widgets.active.bg_fill = raised.lerp_to_gamma(accent, 0.32);
        v.widgets.active.weak_bg_fill = raised.lerp_to_gamma(accent, 0.32);
        v.widgets.active.bg_stroke = egui::Stroke::new(1.0, accent);
        v.widgets.active.fg_stroke = egui::Stroke::new(1.0, text);

        // The focus ring. `accessibility::Palette`'s own doc calls this
        // out: a focus indicator a user cannot distinguish is the one
        // element that makes keyboard and gamepad navigation unusable —
        // and the accent is verified against `raised`, the surface it is
        // actually drawn on, not merely against the page background.
        v.widgets.open.bg_fill = raised;
        v.widgets.open.bg_stroke = egui::Stroke::new(1.0, accent);
        v.selection.bg_fill = accent.gamma_multiply(0.35);
        v.selection.stroke = egui::Stroke::new(1.0, accent);

        ctx.set_visuals(v);

        // Rounder than egui's default and with real breathing room. The
        // old bar packed sixteen controls behind thirteen separators,
        // which is why spacing had to be this tight; with five things in
        // it there is room to let them be legible.
        // `all_styles_mut`, not `style_mut`: egui 0.35 removed the latter
        // (verified in egui-0.35.0 context.rs:2145/2169 — this crate
        // pins versions newer than most training data, project law 2).
        ctx.all_styles_mut(|style| {
            // A type scale, where before there was exactly one size.
            // Hierarchy comes from SIZE, not from colour or weight —
            // those are held in reserve for the two things that actually
            // need to stand out (the primary action, and a warning).
            //
            // Small is where the §3.2 readouts live: FPS, A/V and the
            // profile chip are ambient telemetry you glance at, not
            // labels you read, and rendering them at body size gave
            // them the same claim on the eye as the transport buttons.
            use egui::{FontFamily, FontId, TextStyle};
            style.text_styles = [
                (
                    TextStyle::Heading,
                    FontId::new(17.0, FontFamily::Proportional),
                ),
                (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
                (
                    TextStyle::Button,
                    FontId::new(13.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Small,
                    FontId::new(11.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(11.5, FontFamily::Monospace),
                ),
            ]
            .into();
            // **Scrollbars you can see without hovering.** egui's default
            // is `ScrollStyle::floating` — a thin bar that fades in only
            // when the pointer is over the area — and the effect is that
            // a pane full of content it cannot show looks identical to a
            // pane whose content simply ends. That is how W10-02 shipped
            // an Enhance workspace whose profile inspector was cut off
            // mid-list: the content WAS reachable, and nothing on screen
            // said so. `solid` keeps the bar and its trough visible
            // whenever there is anything to scroll, which is the whole
            // point of a scrollbar — it is a readout of how much you are
            // not looking at, not just a control.
            style.spacing.scroll = egui::style::ScrollStyle::solid();
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(8.0, 3.0);
            for w in [
                &mut style.visuals.widgets.noninteractive,
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
                &mut style.visuals.widgets.open,
            ] {
                w.corner_radius = egui::CornerRadius::same(4);
            }
        });
        // Safe to call every frame: `Context::set_zoom_factor` compares
        // against the stored value and only requests a repaint when it
        // actually differs (egui-0.35.0 `context.rs:2272`, read rather
        // than assumed) — an unconditional repaint request here would
        // make the app spin at 100% doing nothing.
        ctx.set_zoom_factor(a.ui_scale);
    }

    /// The menu bar (ticket W10-01; `docs/design/FRONTEND_UI.md` §2).
    ///
    /// Before W10-01 this held exactly one menu — File > Open ROM… — and
    /// every other control in the app lived in one flat horizontal row
    /// along the bottom. That row needed 1539 px of width in a window
    /// `main.rs` opens at 768 px, so **eight controls were unreachable**,
    /// among them the only openers for the Library, Settings and Controls
    /// windows. The menus below are where those controls went.
    ///
    /// The split is by *concern*, which is what the old row lacked:
    /// `View` is what you are looking at and which surfaces are open,
    /// `Enhance` is everything that changes the picture away from the
    /// unmodified simulation — the law 6 boundary, kept legible by giving
    /// it its own menu rather than interleaving it with window toggles.
    /// The frame the two chrome panels share (ticket W10-01 polish pass).
    ///
    /// Both panels used to inherit `panel_fill`, which is the same colour
    /// as the play area — so the app rendered as one flat rectangle with
    /// text floating at the top and bottom of it, and the chrome had no
    /// edge at all. Putting the chrome on `raised` gives each strip a
    /// body and a boundary, which is the difference between "a bar" and
    /// "some widgets that happen to be near the edge".
    ///
    /// **No border line.** The obvious move is a hairline along the edge
    /// facing the play area, and it is wrong here for a specific reason:
    /// there is no colour in the palette to draw it with. Every colour
    /// this app renders is one of five whose contrast is asserted
    /// (`accessibility::Palette::pairs`), a hairline wants a mid-tone
    /// between `raised` and `background`, and inventing one would put an
    /// unverified colour on screen to save a step. Two surfaces meeting
    /// is already a boundary; the line would be decoration on top of a
    /// boundary that exists.
    fn chrome_frame(ui: &egui::Ui) -> egui::Frame {
        egui::Frame::NONE
            .fill(ui.visuals().widgets.inactive.bg_fill)
            .inner_margin(egui::Margin::symmetric(10, 6))
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu_bar")
            .frame(Self::chrome_frame(ui))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.menu_button("File", |ui| {
                        if ui.button("Open ROM...").clicked() {
                            self.open_rom();
                            ui.close();
                        }
                        // Ticket W10-03: the way BACK to the library.
                        // Without it the home is reachable exactly once
                        // per process — open a game and the only route to
                        // a different one is to quit — which would make
                        // the "home screen" really just a launcher.
                        if ui
                            .add_enabled(self.core.is_some(), egui::Button::new("Close ROM"))
                            .clicked()
                        {
                            self.close_rom();
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Quit").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });

                    let has_core = self.core.is_some();
                    ui.menu_button("View", |ui| {
                        // **Each of these closes the menu when it changes.**
                        // Platform convention (a menu item you pick puts the
                        // menu away), and it is also what makes the sequence
                        // deterministic: a menu left open after a click sits
                        // under the "View" button, so the next click on
                        // "View" lands on a menu row instead. That is not
                        // hypothetical — it opened the Controls window while
                        // `tests/ui_smoke.rs` was trying to open the menu.
                        //
                        // Checkboxes, not "open" buttons: these four each own
                        // a window whose visibility is a piece of app state,
                        // and a checkbox is the control that SHOWS that state
                        // — you can see at a glance what is open. It also
                        // keeps the toggle semantics `tests/ui_smoke.rs`
                        // exercises (open, assert, close, assert) meaningful
                        // now that the controls live behind a menu.
                        //
                        // Ticket W10-03: there is no "Library…" toggle any
                        // more. The library IS the home screen, so a menu
                        // item opening a second copy of it in a floating
                        // window would be two routes to one surface — the
                        // duplication this ticket exists to remove.
                        // Ticket W2-08: app-wide settings (FRONTEND_UI §2).
                        if ui
                            .checkbox(&mut self.show_settings, "Settings\u{2026}")
                            .changed()
                        {
                            ui.close();
                        }
                        // Ticket W2-06's remap window. Pure UI-thread state —
                        // bindings are sampled on this thread too
                        // (`poll_input`), so a remap takes effect on the very
                        // next frame with no round trip to the core thread.
                        if ui
                            .checkbox(&mut self.show_controls, "Controls\u{2026}")
                            .changed()
                        {
                            ui.close();
                        }
                        ui.separator();
                        // Ticket W4-03e criterion 2: the runtime camera
                        // toggle. Disabled with no compositor at all (no GPU
                        // device, `Self::compositor`'s doc) — there is
                        // nothing to switch to, and enabling it would just
                        // click through to `UltrawideUnavailable` every time.
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
                                // Don't wait out the throttle interval for
                                // the FIRST view after switching.
                                self.ultrawide_refresh_countdown = 0;
                            }
                        }
                        if self.compositor.is_none() {
                            ui.label("(no GPU device for ultrawide)");
                        }
                        ui.separator();
                        // Ticket W3-03a: this DOES round-trip to the core
                        // thread. W3-03 kept the layer textures current every
                        // frame regardless of the checkbox, on the reasoning
                        // that the upload was cheap next to the main one and
                        // that a stale image would flash when the window
                        // opened. True, but it left the core thread paying
                        // for the split and two ~240 KB clones on every frame
                        // of every session, open window or not — so the
                        // toggle switches the work off at the source, exactly
                        // as the debugger's event viewer does with
                        // `SetEventMask` (DEBUGGER.md §6).
                        if ui
                            .checkbox(&mut self.show_layers, "Layers (debug)")
                            .changed()
                        {
                            self.send_command(core_thread::CoreCommand::SetLayerExtraction(
                                self.show_layers,
                            ));
                            ui.close();
                        }
                        // Ticket W4-06a criterion 3: layout is saved the
                        // moment the window closes (not only on process exit
                        // via `eframe::App::save`), so a session that opens,
                        // rearranges panels and closes without a clean
                        // shutdown still keeps the change.
                        if ui
                            .checkbox(&mut self.debug_panels.visible, "Debug Viewers")
                            .changed()
                        {
                            if !self.debug_panels.visible {
                                self.debug_panels.save();
                            }
                            ui.close();
                        }
                    });

                    ui.menu_button("Enhance", |ui| {
                        // Ticket W4-05 (FR-MODE-001): ARCHITECTURE §4's five
                        // modes as presets. Persisted per game immediately,
                        // same no-Apply-button stance as every other setting.
                        //
                        // `from_id_salt`, not `from_label`: `ComboBox::
                        // from_label` renders its label to the RIGHT of the
                        // control, so the old bottom bar read
                        // "[Accuracy v] Mode" — backwards — and nobody
                        // noticed because it sat next to a badge saying the
                        // same word.
                        let mut mode = self.current_game_settings.mode;
                        ui.horizontal(|ui| {
                            ui.label("Mode");
                            egui::ComboBox::from_id_salt("mode_preset")
                                .selected_text(mode.display_name())
                                .show_ui(ui, |ui| {
                                    for option in crate::game_settings::Mode::all() {
                                        ui.selectable_value(
                                            &mut mode,
                                            option,
                                            option.display_name(),
                                        );
                                    }
                                });
                        });
                        if mode != self.current_game_settings.mode {
                            self.current_game_settings.mode = mode;
                            self.save_current_game_settings();
                        }
                        ui.separator();
                        if ui.button("Enhance\u{2026}").clicked() {
                            self.show_enhance = !self.show_enhance;
                            ui.close();
                        }
                        // Ticket W3-05a, FR-ENH-001: opt-in only, off by
                        // default (law 6) — checking this does not touch the
                        // accuracy simulation, only whether the dropped-
                        // sprite overlay gets composited on top of it
                        // (`Ppu`'s "Sprite-limit-bypass overlay" section).
                        if ui
                            .add_enabled(
                                has_core,
                                egui::Checkbox::new(&mut self.sprite_overlay, "De-flicker overlay"),
                            )
                            .changed()
                        {
                            self.send_command(CoreCommand::SetSpriteOverlay(self.sprite_overlay));
                            // Ticket W2-07: persist immediately, keyed by
                            // hash. No Apply button anywhere in this app's
                            // settings — an unsaved change a crash discards
                            // is the kind of small betrayal that makes people
                            // stop trusting a settings screen.
                            self.current_game_settings.sprite_overlay = self.sprite_overlay;
                            self.save_current_game_settings();
                        }
                        ui.separator();
                        self.heuristics_menu(ui);
                    });
                });
            });
    }

    /// Ticket W3-05c (FR-ENH-012): the per-game report card, surfaced
    /// locally and only locally — NFR-005 forbids telemetry, and
    /// `rf_enhance::trust` has no I/O of any kind, so there is nowhere
    /// for this to leak to even by accident.
    fn heuristics_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Heuristics\u{2026}", |ui| {
            ui.label("Trust ladder (D-004) — fresh install is all-shadow");
            ui.separator();
            let mut changed = false;
            for heuristic in HEURISTICS {
                let before = self.current_game_settings.trust.state(heuristic);
                let mut state = before;
                ui.horizontal(|ui| {
                    ui.label(*heuristic);
                    ui.radio_value(&mut state, TrustState::Shadow, "Shadow");
                    ui.radio_value(&mut state, TrustState::Advisory, "Advisory");
                    ui.radio_value(&mut state, TrustState::Active, "Active");
                });
                if state != before {
                    self.current_game_settings.trust.set_state(heuristic, state);
                    changed = true;
                }
                if let Some(s) = self.current_game_settings.trust.suppression(heuristic) {
                    ui.label(format!(
                        "    suppressed in {}: {}",
                        s.granted_in_scene, s.justification
                    ));
                }
            }
            if changed {
                // Persist immediately, same stance as every other
                // per-game setting: a toggle that survives only until the
                // next crash is the small betrayal that makes people stop
                // trusting a settings screen.
                self.save_current_game_settings();
            }
            ui.separator();
            let card = self.current_game_settings.trust.report_card();
            if card.is_empty() {
                ui.label("Report card: no contradictions recorded this session");
            } else {
                ui.label(format!("Report card ({} contradiction(s)):", card.len()));
                for c in card {
                    ui.label(format!("  [{}] {} — {}", c.scene, c.heuristic, c.detail));
                }
            }
        });
    }

    // `compare_menu` is gone (ticket W10-02). Compare is a TAB of the
    // Enhance workspace now, per FRONTEND_UI §3.3, and a menu that opened
    // a second copy of the same three radio buttons would be two routes
    // to one control — the duplication W10-03 removed for the Library and
    // the same mistake in a smaller place.

    /// The status bar (ticket W10-01; `docs/design/FRONTEND_UI.md` §3.2).
    ///
    /// §3.2 specifies four things — **mode badge · FPS · A/V sync ·
    /// profile chip** — and this is the first build in which the bar
    /// holds those four things and not sixteen. What was here before mixed
    /// transport, enhancement controls, window toggles and status text in
    /// one non-wrapping `ui.horizontal` behind thirteen separators, needed
    /// 1539 px, and got 768.
    ///
    /// **The status text is laid out FIRST from the right**, before any
    /// optional item, using `with_layout(right_to_left)`. In the old row
    /// it was last in a left-to-right sequence, which made the one widget
    /// whose entire job is telling you what happened the single most
    /// reliably invisible thing in the application.
    ///
    /// Not `horizontal_wrapped`: three rows of mixed buttons, checkboxes
    /// and combos is uglier than one clipped row and hides the spec gap
    /// from the next reader. The controls moved to menus instead
    /// ([`Self::menu_bar`]).
    fn controls_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("controls")
            .frame(Self::chrome_frame(ui))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let has_core = self.core.is_some();
                    let run_label = if self.running { "Pause" } else { "Run" };
                    // The primary action, and the only one in the bar drawn
                    // in the accent. Before W10-01 it was one of sixteen
                    // identical grey rectangles; the eye had nothing to find.
                    //
                    // **Hand-animated, because egui will not do it for you.**
                    // In an immediate-mode UI a button's fill is recomputed
                    // from scratch every frame, so hover is a step function
                    // unless something carries state across frames.
                    // `animate_bool_responsive` is that something: it eases
                    // toward the target over `style.animation_time` and snaps
                    // in on the way. Applied HERE ONLY — one moving element
                    // in a status bar is a highlight, five is a fidget.
                    let accent = ui.visuals().selection.stroke.color;
                    let base = ui.visuals().widgets.inactive.bg_fill;
                    let warmth = ui
                        .ctx()
                        .animate_bool_responsive(egui::Id::new("run_hover"), self.run_hovered);
                    let run = ui.add_enabled(
                        has_core,
                        egui::Button::new(egui::RichText::new(run_label).strong())
                            .fill(base.lerp_to_gamma(accent, 0.30 + 0.22 * warmth))
                            .stroke(egui::Stroke::new(1.0, accent)),
                    );
                    self.run_hovered = run.hovered();
                    if run.clicked() {
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

                    // FRONTEND_UI.md §1: the honesty badge, with its hover
                    // breakdown and hold-to-peek. Not decoration — §1 is
                    // explicit that enhancement is never on silently, and
                    // this is where a user finds out what they are looking
                    // at. It stays in the bar for exactly that reason: a
                    // badge behind a menu is a badge nobody reads.
                    let badge = crate::enhance_ui::badge_text(
                        "NES",
                        &self.current_game_settings,
                        self.profile_matched,
                    );
                    // A CHIP, not a button. It is a status readout that
                    // happens to carry a hover breakdown and a hold-to-peek
                    // gesture — rendering it as a button put it in a row of
                    // buttons looking exactly like them, so it read as
                    // "something to press" rather than "what you are looking
                    // at". `frame(false)` plus a sense keeps both behaviours
                    // and drops the affordance that was lying.
                    let response = ui.add(
                        egui::Button::new(egui::RichText::new(badge).strong())
                            .frame(false)
                            .stroke(egui::Stroke::new(
                                1.0,
                                ui.visuals().widgets.inactive.bg_fill,
                            )),
                    );
                    let breakdown = crate::enhance_ui::badge_breakdown(
                        &self.current_game_settings,
                        self.profile_matched,
                    );
                    response.clone().on_hover_ui(|ui| {
                        for line in &breakdown {
                            ui.label(line);
                        }
                    });
                    // Held, not toggled: peeking is a gesture with an obvious
                    // end, and a toggle would leave someone stuck looking at
                    // the original wondering why their enhancements stopped.
                    self.peeking_original = response.is_pointer_button_down_on();

                    // §3.2's remaining three, right-aligned so the status
                    // text has the first claim on the space.
                    // Where the transport controls and the honesty badge
                    // end. The right-hand readouts must not reach back past
                    // this, or they are drawn on top of them.
                    let left_edge = ui.min_rect().right();
                    // **Grouped by whitespace, not by rules.** Six
                    // `ui.separator()` calls in a five-item bar is a habit
                    // carried over from the sixteen-control row this
                    // replaced, where they were the only thing keeping four
                    // unrelated concerns apart. With one concern left they
                    // are noise — a vertical rule between every readout
                    // draws more ink than the readouts do. ONE survives:
                    // the one dividing controls from telemetry, which is a
                    // real boundary rather than a gap.
                    //
                    // Order is still a PRIORITY order. In a right-to-left
                    // layout the FIRST item placed is the RIGHTMOST and
                    // claims its space first, so §3.2's fixed readouts are
                    // laid out before the status text and the status text
                    // lives on whatever is left. The other order is what
                    // shipped in the first draft of this ticket: an
                    // unbounded `Loaded /Users/.../Some Game (USA).nes`
                    // claimed the space first and pushed the FPS, A/V and
                    // profile chip off the left edge, under the mode badge.
                    let group =
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            match self.fps {
                                Some(fps) => ui.add(readout(
                                    egui::RichText::new(format!("{fps:5.1} fps")).monospace(),
                                )),
                                // A dash, not a hidden widget: the bar must not
                                // change width when a reading arrives, or every
                                // item left of it jumps a second after boot.
                                None => {
                                    ui.add(readout(egui::RichText::new("  --- fps").monospace()))
                                }
                            };
                            self.av_sync_indicator(ui);
                            self.profile_chip(ui);
                            if let Some((frame, scanline)) = self.position {
                                ui.add(readout(
                                    egui::RichText::new(match scanline {
                                        // ASCII only: the bundled
                                        // monospace face has no `·`.
                                        Some(y) => format!("f{frame} sl{y}"),
                                        None => format!("f{frame} sl--"),
                                    })
                                    .monospace(),
                                ));
                            }
                            // FM-13 criterion 3: "view too large for GPU,
                            // reduced" — surfaced plainly, never swallowed
                            // (`Self::refresh_ultrawide_render`'s doc). The one
                            // readout allowed to interrupt, so it keeps its
                            // warning colour.
                            if let Some(msg) = &self.fm13_message {
                                ui.add(readout(
                                    egui::RichText::new(msg)
                                        .color(egui::Color32::from_rgb(230, 180, 40)),
                                ));
                            }
                            ui.separator();
                            // `truncate`, not a character budget: `self.status`
                            // is `format!("Loaded {}", path.display())` on every
                            // ROM open, so its width is the user's directory
                            // depth. egui fits it to the space actually
                            // remaining, which is the honest bound — a guess
                            // about how wide a character is would be wrong at
                            // any other `ui_scale`.
                            ui.add(egui::Label::new(&self.status).truncate())
                                .on_hover_text(&self.status);
                        });
                    self.status_readouts = Some((group.response.rect, left_edge));
                });
            });
    }

    /// The rect last frame's §3.2 readouts occupied, and the right edge
    /// of the controls to their left (ticket W10-01). `None` before the
    /// first repaint.
    ///
    /// `#[doc(hidden)]` and named for its purpose: this exists so
    /// `tests/hud_fits.rs` can assert the readouts fit, which the
    /// accessibility tree cannot answer — see
    /// [`Self::status_readouts`]'s own doc.
    #[doc(hidden)]
    #[must_use]
    pub fn status_readouts_for_test(&self) -> Option<(egui::Rect, f32)> {
        self.status_readouts
    }

    /// §3.2's profile chip: whether a game profile claims this ROM.
    ///
    /// Says "no profile" rather than rendering nothing, because those are
    /// different facts and the user cannot tell an absent chip from an
    /// unmatched one — the same rule design review G-21 imposed on the
    /// library's two empty states (`Self::library_window`).
    fn profile_chip(&mut self, ui: &mut egui::Ui) {
        // U+25C7 WHITE DIAMOND and U+25CB WHITE CIRCLE, both present in
        // egui's bundled font. The first draft used U+25C6 BLACK DIAMOND,
        // which is NOT, and rendered as a hollow tofu box in the shipped
        // window — visible only in a screenshot, which is why the
        // screenshot is part of this ticket's evidence and not a
        // formality.
        match (&self.matched_profile, self.profile_matched) {
            (Some(path), _) => {
                let name: String = path.file_stem().map_or_else(
                    || path.display().to_string(),
                    |s| s.to_string_lossy().into(),
                );
                ui.add(readout(format!("\u{2605} {}", elide_front(&name))))
                    .on_hover_text(path.display().to_string());
            }
            (None, true) => {
                ui.add(readout("\u{2605} profile"));
            }
            (None, false) => {
                ui.add(readout(egui::RichText::new("\u{2606} no profile").weak()));
            }
        }
    }

    /// §3.2's A/V sync indicator: is the audio buffer being kept fed?
    ///
    /// This is the honest reading of a real number, not an ornament.
    /// W2-05 makes the *audio device* the clock, so a draining buffer is
    /// the earliest visible sign that emulation is not keeping up —
    /// earlier than the FPS counter beside it, which only falls once
    /// frames are already being missed. `None` before any frame carries a
    /// reading; the core thread owns `AudioOut` and reports fill with
    /// each frame (`core_thread::FrameMsg::audio_fill`).
    fn av_sync_indicator(&mut self, ui: &mut egui::Ui) {
        // **The state is in the WORD, not only the colour.** This
        // indicator used to say everything with hue: red for starved,
        // amber for ahead, green for healthy, one glyph for all three.
        // That is WCAG 1.4.1 — colour as the sole carrier of meaning —
        // and it fails for the eight percent of men with a red/green
        // deficiency, on a washed-out projector, and in a screenshot
        // printed in grey.
        //
        // Words rather than three distinct shapes because the bundled
        // font does not HAVE three distinct shapes: the verified set is
        // `○ ■ ★ ☆`, the stars are spoken for by the profile chip, and
        // reaching outside that set is how three tofu boxes shipped.
        // `low` / `ok` / `high` is unambiguous, always renders, and
        // leaves the colour doing what colour is good at — carrying the
        // same message a second time, faster.
        let Some(fill) = self.audio_fill else {
            ui.add(readout(egui::RichText::new("a/v \u{2014}").weak()))
                .on_hover_text("No audio device open, so there is no buffer to report on.");
            return;
        };
        // Thresholds are about the buffer's job, not aesthetics: near
        // empty is an underrun about to be audible, near full means the
        // core is outrunning the device and will be throttled.
        let (word, colour, tip) = if fill < 0.15 {
            (
                "a/v low",
                egui::Color32::from_rgb(220, 90, 80),
                "audio buffer nearly empty \u{2014} emulation is behind the audio clock",
            )
        } else if fill > 0.95 {
            (
                "a/v high",
                egui::Color32::from_rgb(230, 180, 40),
                "audio buffer nearly full \u{2014} the core is ahead and being throttled",
            )
        } else {
            (
                "a/v ok",
                egui::Color32::from_rgb(110, 190, 120),
                "audio buffer healthy \u{2014} a/v in sync",
            )
        };
        ui.add(readout(egui::RichText::new(word).color(colour)))
            .on_hover_text(format!("{tip}\nbuffer fill: {:.0}%", fill * 100.0));
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
    /// The app-wide Settings window (ticket W2-08; FRONTEND_UI §2).
    ///
    /// Every control writes through immediately — no Apply button anywhere
    /// in this app, for the reason W2-06's Controls window already states:
    /// an unsaved change a crash discards is the kind of small betrayal
    /// that makes people stop trusting a settings screen.
    ///
    /// Which of these take effect live and which need a restart is stated
    /// ON the control rather than left to be discovered: vsync and the
    /// audio device are owned by objects created at startup
    /// (`eframe`'s window, `rf_audio::AudioDevice`), and pretending
    /// otherwise would be worse than saying so.
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = self.show_settings;
        let mut changed = false;
        egui::Window::new("Settings")
            .default_pos(egui::pos2(PANEL_WINDOW_ORIGIN[0], PANEL_WINDOW_ORIGIN[1]))
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (SettingsTab::Video, "Video"),
                        (SettingsTab::Audio, "Audio"),
                        (SettingsTab::Paths, "Paths"),
                        (SettingsTab::Accessibility, "Accessibility"),
                    ] {
                        ui.selectable_value(&mut self.settings_tab, tab, label);
                    }
                });
                ui.separator();

                match self.settings_tab {
                    SettingsTab::Accessibility => {
                        let a = &mut self.settings.accessibility;
                        ui.label("UI scale");
                        if ui
                            .add(
                                egui::Slider::new(
                                    &mut a.ui_scale,
                                    crate::accessibility::MIN_UI_SCALE
                                        ..=crate::accessibility::MAX_UI_SCALE,
                                )
                                .text("x"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        ui.weak(
                            "Bounded deliberately: below 0.5x the UI is unreadable, and a \
                             user who cannot read the UI cannot open this window to undo it \
                             (crate::accessibility's module doc).",
                        );
                        ui.separator();

                        ui.label("Contrast");
                        if ui
                            .checkbox(&mut a.high_contrast, "High-contrast palette")
                            .changed()
                        {
                            changed = true;
                        }
                        // The numbers, not a claim about them: these are
                        // the ratios `accessibility`'s tests assert, read
                        // from the palette actually in use.
                        let p = self.settings.accessibility.palette();
                        ui.weak(format!(
                            "text {:.1}:1 · focus accent {:.1}:1 · WCAG AAA is {:.0}:1",
                            p.text_contrast(),
                            p.accent_contrast(),
                            crate::accessibility::WCAG_AAA,
                        ));
                        ui.weak(
                            "Off by default: high contrast is an accommodation, not an \
                             improvement.",
                        );
                    }
                    SettingsTab::Video => {
                        ui.label("Scaling");
                        for mode in crate::settings::ScaleMode::ALL {
                            if ui
                                .radio_value(
                                    &mut self.settings.video.scale_mode,
                                    mode,
                                    mode.label(),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                        ui.separator();

                        ui.label("Shader");
                        // W3-02a owns the shader set; until it lands the
                        // only honest options are "none" and whatever a
                        // config already names, so this is a text field
                        // rather than a dropdown pretending to a catalogue.
                        let mut shader = self.settings.video.shader.clone().unwrap_or_default();
                        if ui.text_edit_singleline(&mut shader).changed() {
                            self.settings.video.shader =
                                (!shader.trim().is_empty()).then(|| shader.trim().to_string());
                            changed = true;
                        }
                        ui.small(
                            "Shader names arrive with W3-02a; empty means the plain pipeline.",
                        );
                        ui.separator();

                        if ui
                            .checkbox(&mut self.settings.video.vsync, "V-sync")
                            .changed()
                        {
                            changed = true;
                        }
                        ui.small(
                            "Takes effect on restart (the window surface is created at startup).",
                        );
                    }
                    SettingsTab::Audio => {
                        let mut device = self.settings.audio.device.clone().unwrap_or_default();
                        ui.label("Output device (empty = system default)");
                        if ui.text_edit_singleline(&mut device).changed() {
                            self.settings.audio.device =
                                (!device.trim().is_empty()).then(|| device.trim().to_string());
                            changed = true;
                        }
                        ui.small("Takes effect on restart.");
                        ui.separator();

                        if ui
                            .add(
                                egui::Slider::new(&mut self.settings.audio.latency_ms, 10..=200)
                                    .text("Buffer latency (ms)"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        ui.small(
                            "Lower is more responsive, higher survives a stalled frame. Takes \
                             effect on restart.",
                        );
                        ui.separator();

                        if ui
                            .add(
                                egui::Slider::new(&mut self.settings.audio.volume, 0.0..=1.0)
                                    .text("Volume"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    }
                    SettingsTab::Paths => {
                        ui.label("Library folders");
                        let mut remove: Option<usize> = None;
                        for (index, folder) in self
                            .settings
                            .paths
                            .library_folders
                            .clone()
                            .iter()
                            .enumerate()
                        {
                            ui.horizontal(|ui| {
                                ui.label(folder.display().to_string());
                                if ui.small_button("Remove").clicked() {
                                    remove = Some(index);
                                }
                            });
                        }
                        if let Some(index) = remove {
                            self.settings.paths.library_folders.remove(index);
                            changed = true;
                        }
                        if ui.button("Add folder\u{2026}").clicked() {
                            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                if !self.settings.paths.library_folders.contains(&folder) {
                                    self.settings.paths.library_folders.push(folder);
                                    changed = true;
                                }
                            }
                        }
                        ui.separator();

                        ui.label("Cache");
                        let mut cache = self
                            .settings
                            .paths
                            .cache_dir
                            .clone()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        if ui.text_edit_singleline(&mut cache).changed() {
                            self.settings.paths.cache_dir = (!cache.trim().is_empty())
                                .then(|| std::path::PathBuf::from(cache.trim()));
                            changed = true;
                        }
                        ui.small("Empty = the default location under the config directory.");
                        if ui
                            .add(
                                egui::Slider::new(
                                    &mut self.settings.paths.cache_cap_mb,
                                    128..=32_768,
                                )
                                .text("Cache cap (MB)"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    }
                }

                ui.separator();
                ui.small(&self.bindings_status);
            });
        self.show_settings = open;
        if changed {
            self.save_settings();
            // The library screen reads its roots from here, so a folder
            // added in Paths must be visible in Library without a restart.
            self.library_roots
                .clone_from(&self.settings.paths.library_folders);
            self.library = None;
        }
    }

    /// Persist app-wide settings, reporting failure rather than swallowing it.
    fn save_settings(&mut self) {
        let Some(root) = self.config_root.clone() else {
            self.status = "No config directory; settings apply to this session only.".to_string();
            return;
        };
        if let Err(e) = crate::settings::save(&root, &self.settings) {
            self.status = format!("Could not save settings: {e}");
        }
    }

    /// The Esc overlay menu (ticket W2-08; FRONTEND_UI §2: "resume · states
    /// · settings · switch mode · quit").
    ///
    /// Shown as a modal-ish window rather than a full-screen takeover
    /// because the point is to pause *access*, not to hide the game: a
    /// player pressing Esc mid-level wants to see where they were.
    fn overlay_menu(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_overlay_menu = !self.show_overlay_menu;
        }
        if !self.show_overlay_menu {
            return;
        }
        let mut open = self.show_overlay_menu;
        egui::Window::new("Menu")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if ui.button("Resume").clicked() {
                    self.show_overlay_menu = false;
                    if self.core.is_some() {
                        self.send_command(CoreCommand::Resume);
                        self.running = true;
                    }
                }
                // Save states are W4-11's modal; the entry is present and
                // says what it is waiting for rather than being silently
                // absent from a menu FRONTEND_UI §2 enumerates.
                if ui.button("States\u{2026}").clicked() {
                    self.show_overlay_menu = false;
                    self.open_states_modal();
                }
                if ui.button("Settings\u{2026}").clicked() {
                    self.show_settings = true;
                }
                // Ticket W5-06: only offered when a profile actually
                // claims this ROM — an author workspace with nothing to
                // author against would be a control that does nothing.
                if let Some(path) = self.matched_profile.clone() {
                    if ui.button("Author\u{2026}").clicked() {
                        self.show_overlay_menu = false;
                        self.open_author_workspace(path);
                    }
                }
                if ui.button("Controls\u{2026}").clicked() {
                    self.show_controls = true;
                }
                // W10-01: the disabled "Switch mode (W4-05)" placeholder is
                // gone. W4-05 shipped — the mode preset is live under
                // Enhance > Mode — so what stood here was a permanently
                // dead control advertising a ticket that had already
                // closed, which is worse than an absent one: it tells the
                // user the feature does not exist.
                if ui.button("Mode\u{2026}").clicked() {
                    self.show_overlay_menu = false;
                    self.show_enhance = true;
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        self.show_overlay_menu = open;
    }

    /// **The library home** (ticket W10-03; `docs/design/FRONTEND_UI.md`
    /// §3.1, and §2's information architecture, which puts Library at the
    /// root: *Library (home) → Play view → Workspaces*).
    ///
    /// Until W10-03 this was a floating window behind a checkbox, and the
    /// app booted to an empty play area with one sentence in it. The
    /// scanning, hash identity and the three first-run states were all
    /// already here and unchanged — this ticket moved **where they live**,
    /// not what they do.
    ///
    /// The three states stay distinct, and that is the point design review
    /// G-21 forced: "no folders configured" and "folders with nothing in
    /// them" are different facts, and an empty grid that says neither is
    /// the bug. [`crate::library::first_run_state`] decides which, so the
    /// rule is unit-tested rather than only rendered.
    ///
    /// Returns the ROM to open, if the user picked one.
    fn library_home(&mut self, ui: &mut egui::Ui) {
        if self.library.is_none() {
            self.rescan_library();
        }
        let library = self.library.clone().unwrap_or_default();
        let state = crate::library::first_run_state(&self.library_roots, &library);
        let mut rescan = false;
        let mut to_play: Option<std::path::PathBuf> = None;

        match &state {
            crate::library::FirstRunState::NoRootsConfigured => {
                Self::home_empty(ui, "No ROM folders yet", |ui| {
                    ui.add(readout(egui::RichText::new(
                        "RetroForge finds games by scanning folders you choose. Nothing is ever \
                         sent anywhere \u{2014} identification is local, by hashing the file.",
                    ).weak()));
                    ui.add_space(10.0);
                    if ui.button("Add a ROM folder\u{2026}").clicked() {
                        rescan = self.pick_library_folder();
                    }
                });
            }
            crate::library::FirstRunState::NoRomsFound { roots } => {
                Self::home_empty(ui, "No ROMs found", |ui| {
                    // Named, not summarised: "0 ROMs found" without the
                    // path is indistinguishable from a scan that never ran.
                    for root in roots {
                        ui.add(readout(
                            egui::RichText::new(format!("0 ROMs found in {}", root.display()))
                                .weak(),
                        ));
                    }
                    ui.add_space(10.0);
                    if ui.button("Add another folder\u{2026}").clicked() {
                        rescan = self.pick_library_folder();
                    }
                });
            }
            crate::library::FirstRunState::Populated { count } => {
                self.library_toolbar(ui, *count, &mut rescan);
                ui.separator();
                to_play = self.library_grid(ui, &library);
            }
        }

        if !library.issues.is_empty() {
            ui.separator();
            // NFR-010/FM-15: a refused path is named, never silently
            // dropped — a scan that quietly ignores half a library looks
            // identical to one that found nothing.
            ui.collapsing(format!("Skipped ({})", library.issues.len()), |ui| {
                for issue in &library.issues {
                    ui.add(readout(egui::RichText::new(format!("{issue:?}")).weak()));
                }
            });
        }

        if rescan {
            self.rescan_library();
        }
        if let Some(path) = to_play {
            self.open_rom_path(&path);
        }
    }

    /// The shared shape of the two empty states: a title, then the one
    /// thing to do about it, with size carrying the hierarchy.
    ///
    /// One helper rather than two hand-built blocks, because these two
    /// states differ in *what they say*, not in how they look — and the
    /// moment they are built separately they start to drift apart.
    fn home_empty(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
        ui.vertical_centered(|ui| {
            // Pushed off dead-centre: optically centred text sits a little
            // above the true middle, and a block starting exactly halfway
            // down reads as low.
            ui.add_space(ui.available_height() * 0.30);
            ui.label(egui::RichText::new(title).heading());
            ui.add_space(6.0);
            ui.scope(|ui| {
                ui.set_max_width(420.0);
                body(ui);
            });
        });
    }

    /// §3.1's search box and console filters. Returns the filtered titles.
    fn library_toolbar(&mut self, ui: &mut egui::Ui, count: usize, rescan: &mut bool) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.library_search)
                    .hint_text("Search\u{2026}")
                    .desired_width(180.0),
            );
            // `None` is "every console", which is why the filter is an
            // Option rather than a Console with an `All` variant: `All`
            // would be a console that does not exist, and every match on
            // `Console` elsewhere would have to handle it.
            for (label, filter) in [
                ("All", None),
                ("NES", Some(crate::library::Console::Nes)),
                ("SNES", Some(crate::library::Console::Snes)),
            ] {
                ui.selectable_value(&mut self.library_console_filter, filter, label);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Add folder\u{2026}").clicked() {
                    *rescan = self.pick_library_folder();
                }
                if ui.button("Rescan").clicked() {
                    *rescan = true;
                }
                ui.add(readout(
                    egui::RichText::new(format!("{count} game(s)")).weak(),
                ));
            });
        });
    }

    /// §3.1's grid, filtered by the toolbar.
    ///
    /// Rows, not picture cards: §3.1 asks for thumbnails from "last
    /// save-state screenshot or first-frame capture", and this build has
    /// neither wired to the library yet. A grid of identical grey
    /// placeholder rectangles would look more like the spec and tell the
    /// user strictly less than the title does, so the cards wait until
    /// there is an image to put in them. **No box art** either way —
    /// NON_GOALS #5 rules out fetching it.
    ///
    /// Returns the ROM to open, if one was picked.
    fn library_grid(
        &self,
        ui: &mut egui::Ui,
        library: &crate::library::Library,
    ) -> Option<std::path::PathBuf> {
        let needle = self.library_search.trim().to_lowercase();
        let matches: Vec<&crate::library::LibraryEntry> = library
            .entries
            .iter()
            .filter(|e| {
                let console_ok = match self.library_console_filter {
                    None => true,
                    Some(want) => matches!(
                        &e.identity,
                        crate::library::EntryIdentity::Recognized { console, .. }
                            if *console == want
                    ),
                };
                let text_ok = needle.is_empty() || e.title.to_lowercase().contains(&needle);
                console_ok && text_ok
            })
            .collect();

        if matches.is_empty() {
            // A fourth state, and it is NOT one of `first_run_state`'s
            // three: the library has games, the filter just excluded them
            // all. Saying "no ROMs found" here would be a lie about the
            // library, and rendering nothing would look like a bug.
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.add(readout(
                    egui::RichText::new("No games match this search.").weak(),
                ));
            });
            return None;
        }

        let mut to_play = None;
        // Rows, not an `egui::Grid`. A Grid sizes every column to its
        // content, so the whole library huddled into the left third of
        // the window with two thirds of empty space beside it — a list
        // that looked like a rendering accident rather than the app's
        // home screen. A row that lays its title out left-to-right and
        // its Play button right-to-left fills the width by construction,
        // at any window size, with no arithmetic to keep in sync.
        egui::ScrollArea::vertical()
            // `auto_shrink` off horizontally: otherwise the scroll area
            // shrinks to its content and takes the rows back down to the
            // left third, which is the same bug one level up.
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, entry) in matches.iter().enumerate() {
                    // Striping by frame rather than by `Grid::striped`,
                    // since the Grid is gone. Zebra rows earn their keep
                    // at library scale even though three rows do not
                    // need them.
                    let fill = if i % 2 == 1 {
                        ui.visuals().faint_bg_color
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    egui::Frame::NONE
                        .fill(fill)
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(&entry.title);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Play").clicked() {
                                            to_play = Some(entry.path.clone());
                                        }
                                        match &entry.identity {
                                            crate::library::EntryIdentity::Recognized {
                                                console,
                                                normalized_sha256,
                                            } => {
                                                ui.add(readout(
                                                    egui::RichText::new(console.name()).weak(),
                                                ))
                                                .on_hover_text(format!(
                                                    "sha256 (normalized): {normalized_sha256}"
                                                ));
                                            }
                                            crate::library::EntryIdentity::Unrecognized {
                                                reason,
                                            } => {
                                                // Still playable — §3.1 is
                                                // explicit that an
                                                // unidentified ROM gets a
                                                // generic card, not a
                                                // refusal.
                                                ui.add(readout(
                                                    egui::RichText::new("unrecognized").weak(),
                                                ))
                                                .on_hover_text(reason);
                                            }
                                        }
                                    },
                                );
                            });
                        });
                }
            });
        to_play
    }

    /// Open a folder picker and add what it returns to the roots.
    /// `true` if a rescan is now owed.
    fn pick_library_folder(&mut self) -> bool {
        let Some(folder) = rfd::FileDialog::new().pick_folder() else {
            return false;
        };
        self.library_roots.push(folder);
        self.save_library_roots();
        true
    }

    /// Persist the current game's settings (ticket W2-07, FR-FE-002).
    /// A no-op for a ROM this build could not identify — settings keyed by
    /// a hash we could not compute would eventually apply to the wrong game.
    fn save_current_game_settings(&mut self) {
        let (Some(root), Some(hash)) = (self.config_root.clone(), self.current_game_hash.clone())
        else {
            return;
        };
        if let Err(e) = crate::game_settings::save(&root, &hash, &self.current_game_settings) {
            self.status = format!("Could not save game settings: {e}");
        }
    }

    /// Stop the running core and return to the library home (ticket
    /// W10-03).
    ///
    /// Sends `Shutdown` and drops the handle rather than merely pausing:
    /// a paused core still owns its audio device and still costs a thread,
    /// and "close" that leaves the game resident is not close. The
    /// library is deliberately NOT rescanned — the scan is unchanged by
    /// having played something, and re-walking the user's folders on every
    /// return would make going back feel expensive.
    fn close_rom(&mut self) {
        self.send_command(CoreCommand::Shutdown);
        self.core = None;
        self.texture = None;
        self.bg_layer_texture = None;
        self.sprite_layer_texture = None;
        self.ultrawide_texture = None;
        self.ultrawide_render = None;
        self.running = false;
        self.position = None;
        self.fps = None;
        self.fps_frames = 0;
        self.audio_fill = None;
        self.crash = None;
        self.status = "No ROM loaded".to_string();
    }

    /// Rescan the configured roots.
    fn rescan_library(&mut self) {
        self.library_scans += 1;
        self.library = Some(crate::library::scan(&self.library_roots));
    }

    /// Point the library at these roots and force a rescan (ticket
    /// W10-03), so `tests/library_home.rs` can drive §3.1's three
    /// first-run states without a folder-picker dialog no headless
    /// harness can operate.
    #[doc(hidden)]
    pub fn set_library_roots_for_test(&mut self, roots: Vec<std::path::PathBuf>) {
        self.library_roots = roots;
        self.rescan_library();
    }

    /// Open the Enhance workspace without going through the menu
    /// (ticket W10-02), so `tests/renders.rs` can photograph it.
    #[doc(hidden)]
    pub fn show_enhance_for_test(&mut self, show: bool) {
        self.show_enhance = show;
    }

    /// Set §3.1's search text directly (ticket W10-03).
    #[doc(hidden)]
    pub fn set_library_search_for_test(&mut self, needle: &str) {
        self.library_search = needle.to_string();
    }

    /// Close the running ROM, as File > Close ROM does (ticket W10-03).
    #[doc(hidden)]
    pub fn close_rom_for_test(&mut self) {
        self.close_rom();
    }

    /// How many times the library has been scanned this session.
    ///
    /// Exists so a test can assert the round trip is FREE. "No rescan on
    /// return" is otherwise unfalsifiable from the outside: a rescan of an
    /// unchanged folder produces an identical library, so the only visible
    /// difference is the folder walk itself.
    #[doc(hidden)]
    #[must_use]
    pub fn library_scan_count_for_test(&self) -> u32 {
        self.library_scans
    }

    /// Persist the configured roots, reporting failure into the status line
    /// rather than swallowing it.
    fn save_library_roots(&mut self) {
        let Some(root) = self.config_root.clone() else {
            self.status =
                "No config directory; library folders apply to this session only.".to_string();
            return;
        };
        if let Err(e) = crate::library_roots::save(&root, &self.library_roots) {
            self.status = format!("Could not save library folders: {e}");
        }
    }

    /// The remap window (ticket W2-06): one row per NES button per port,
    /// showing what is bound and offering to rebind it.
    ///
    /// **Capture, not a dropdown.** Rebinding waits for the user to press
    /// the key they want — a list of 60-odd key names is unusable, and the
    /// press is unambiguous in a way "pick `Semicolon` from a list" is not.
    /// `awaiting_key` is that state; the capture happens in `poll_input`,
    /// which is the only place that sees raw key events.
    ///
    /// Every change saves immediately. There is no Apply button, because an
    /// unsaved remap that a crash discards is exactly the kind of small
    /// betrayal that makes people stop trusting a settings screen.
    fn controls_window(&mut self, ctx: &egui::Context) {
        if !self.show_controls {
            return;
        }
        let mut changed = false;
        let mut open = self.show_controls;
        egui::Window::new("Controls")
            .default_pos(egui::pos2(PANEL_WINDOW_ORIGIN[0], PANEL_WINDOW_ORIGIN[1]))
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            // Ticket W10-01. Two players x every NES button, each with a
            // Clear, is 1105 px of content — TALLER THAN THE WHOLE APP
            // WINDOW, which `main.rs` opens at 720. egui cannot honour a
            // position for a window that does not fit, so it pinned this
            // one to y=0, on top of the menu bar, and the File/View/
            // Enhance menus became unclickable while it was open. Bound
            // it to the viewport and scroll the overflow.
            // `viewport_rect`, not `screen_rect`: egui 0.35 renamed it
            // (verified in egui-0.35.0 context.rs:2819 — project law 2,
            // this crate pins versions newer than most training data).
            .max_height(ctx.viewport_rect().height() - PANEL_WINDOW_ORIGIN[1] - 24.0)
            .show(ctx, |ui| {
                ui.label(&self.bindings_status);
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for port in 0..2usize {
                        ui.heading(format!("Player {}", port + 1));
                        egui::Grid::new(format!("controls-port-{port}"))
                            .num_columns(3)
                            .striped(true)
                            .show(ui, |ui| {
                                for button in rf_input::NesButton::ALL {
                                    ui.label(button.name());

                                    let bound = self
                                        .bindings
                                        .keys
                                        .entries()
                                        .iter()
                                        .find(|(_, p, b)| *p == port && *b == button)
                                        .map(|(key, _, _)| key.name());
                                    let label = match (self.awaiting_key, bound) {
                                        (Some((p, b)), _) if p == port && b == button => {
                                            "press a key\u{2026}".to_string()
                                        }
                                        (_, Some(name)) => name.to_string(),
                                        (_, None) => "\u{2014}".to_string(),
                                    };
                                    if ui.button(label).clicked() {
                                        self.awaiting_key = Some((port, button));
                                    }

                                    if ui.small_button("Clear").clicked() {
                                        let bound_key = self
                                            .bindings
                                            .keys
                                            .entries()
                                            .iter()
                                            .find(|(_, p, b)| *p == port && *b == button)
                                            .map(|(key, _, _)| *key);
                                        if let Some(key) = bound_key {
                                            self.bindings.keys.unbind(key);
                                            changed = true;
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                        ui.separator();
                    }

                    ui.heading("Gamepad");
                    ui.label(format!(
                        "{} pad(s) connected",
                        self.pad_router.connected_count()
                    ));
                    egui::Grid::new("controls-pad")
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| {
                            for pad_button in rf_input::PadButton::ALL {
                                ui.label(pad_button.name());
                                let current = self.bindings.pads.lookup(pad_button);
                                let label = current.map_or("\u{2014}", rf_input::NesButton::name);
                                egui::ComboBox::from_id_salt(pad_button.name())
                                    .selected_text(label)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(current.is_none(), "\u{2014}")
                                            .clicked()
                                        {
                                            self.bindings.pads.unbind(pad_button);
                                            changed = true;
                                        }
                                        for nes in rf_input::NesButton::ALL {
                                            if ui
                                                .selectable_label(current == Some(nes), nes.name())
                                                .clicked()
                                            {
                                                self.bindings.pads.bind(pad_button, nes);
                                                changed = true;
                                            }
                                        }
                                    });
                                ui.end_row();
                            }
                        });

                    ui.separator();
                    if ui.button("Restore defaults").clicked() {
                        self.bindings = rf_input::Bindings::default();
                        changed = true;
                    }
                });
            });
        self.show_controls = open;
        if changed {
            self.save_bindings();
        }
    }

    /// Persist the current bindings, reporting the result into
    /// `bindings_status` rather than silently.
    fn save_bindings(&mut self) {
        let Some(root) = self.config_root.clone() else {
            self.bindings_status =
                "No config directory; this remap applies to the current session only.".to_string();
            return;
        };
        self.bindings_status = match crate::bindings_store::save(&root, &self.bindings) {
            Ok(path) => format!("Bindings saved to {}.", path.display()),
            Err(e) => format!("Could not save bindings: {e}"),
        };
    }

    fn layers_debug_window(&mut self, ctx: &egui::Context) {
        if !self.show_layers {
            return;
        }
        egui::Window::new("Layers (debug)")
            .default_pos(egui::pos2(PANEL_WINDOW_ORIGIN[0], PANEL_WINDOW_ORIGIN[1]))
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
            .default_pos(egui::pos2(PANEL_WINDOW_ORIGIN[0], PANEL_WINDOW_ORIGIN[1]))
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
    /// Write the current frame's two renderings as PNGs (ticket W3-04,
    /// FR-FE-005: "Screenshot capture (original and enhanced buffers
    /// **separately**)").
    ///
    /// Two files, not one composed image, because that is what the
    /// requirement says and it is also the more useful artefact: a
    /// composed split is reproducible from the two halves, but neither
    /// half is recoverable from a composed split.
    fn write_screenshots(&mut self) {
        let Some(buffers) = self.compare_buffers.as_ref() else {
            self.status = "Screenshot: no frame captured yet".to_string();
            return;
        };
        let stamp = self.position.map_or(0, |(frame, _)| frame);
        let dir = std::env::current_dir().unwrap_or_default();
        let mut written = Vec::new();
        for (name, png) in screenshot_files(buffers, stamp) {
            let path = dir.join(name);
            match std::fs::write(&path, png) {
                Ok(()) => written.push(path.display().to_string()),
                Err(e) => {
                    // A screenshot failing is never fatal — say so and
                    // keep emulating, same stance as every other optional
                    // side-effect in this shell.
                    self.status = format!("Screenshot failed: {e}");
                    return;
                }
            }
        }
        self.status = format!("Screenshot: wrote {}", written.join(", "));
    }

    /// The Enhance workspace (ticket W4-05; FRONTEND_UI.md §3.3's
    /// Features tab plus the profile inspector of GAME_PROFILES.md §4).
    ///
    /// Every string here comes from `crate::enhance_ui`, which is tested
    /// headlessly — this only lays them out, so there is no judgement in
    /// this function that could disagree with what those tests assert.
    /// The Enhance workspace (ticket W10-02; FRONTEND_UI §3.3).
    ///
    /// A window hosting a dock area rather than a pile of headings: §3.3
    /// specifies three tabs, and until W10-02 only one of them existed
    /// here. Compare was a bottom-bar menu and Map was nowhere at all.
    ///
    /// This function owns none of the drawing. `crate::enhance_dock`
    /// renders the tabs from an [`crate::enhance_dock::EnhanceCtx`] and
    /// reports back what the user did; everything that needs the core
    /// thread, per-game persistence or the GPU stays here, where the rest
    /// of it already lives.
    fn enhance_window(&mut self, ctx: &egui::Context) {
        if !self.show_enhance {
            return;
        }
        let mut open = self.show_enhance;
        let mut actions = crate::enhance_dock::EnhanceActions::default();
        egui::Window::new("Enhance")
            .default_pos(egui::pos2(PANEL_WINDOW_ORIGIN[0], PANEL_WINDOW_ORIGIN[1]))
            // Wide enough that §3.3's three panes each have usable
            // width at the DEFAULT split; the feature rows wrap rather
            // than clip below this, but starting a workspace already
            // wrapping reads as broken.
            .default_size([660.0, 420.0])
            // Bounded to the viewport for the same reason the Controls
            // window is (W10-01): egui cannot honour a position for a
            // window that does not fit, and pins it to y=0 on top of the
            // menu bar.
            .max_height(ctx.viewport_rect().height() - PANEL_WINDOW_ORIGIN[1] - 24.0)
            .open(&mut open)
            .show(ctx, |ui| {
                let mut view = crate::enhance_dock::EnhanceCtx {
                    settings: &mut self.current_game_settings,
                    profile_matched: self.profile_matched,
                    compare_mode: &mut self.compare_mode,
                    compare_divider: &mut self.compare_divider,
                    map_texture: self.ultrawide_texture.as_ref(),
                    fm13_message: self.fm13_message.as_deref(),
                    has_compositor: self.compositor.is_some(),
                };
                actions = self.enhance.ui(ui, &mut view);
            });

        if let Some(on) = actions.sprite_overlay_set {
            self.sprite_overlay = on;
            self.send_command(CoreCommand::SetSpriteOverlay(on));
        }
        if actions.settings_changed {
            self.save_current_game_settings();
        }
        if actions.screenshot_requested {
            // Deferred to the next frame rather than taken here: with
            // compare off, no buffers are being kept (W3-03a's rule), so
            // the first frame that HAS them is the next one.
            self.screenshot_pending = true;
            self.status = "Screenshot: capturing next frame\u{2026}".to_string();
        }
        self.show_enhance = open;
    }

    fn video_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            // Ticket W4-05 (FRONTEND_UI.md §1): hold-to-peek forces the
            // ORIGINAL view for as long as the badge is held. Applied
            // here rather than by mutating `self.camera`, so releasing
            // restores whatever the user had chosen without this having
            // to remember it.
            let camera = if self.peeking_original {
                CameraToggle::Original
            } else {
                self.camera
            };
            match enhanced_view::select_active_view(camera, self.ultrawide_render.as_ref()) {
                enhanced_view::ActiveView::Original => {
                    // Ticket W10-03. Which surface this is depends on
                    // whether a CORE exists, not on whether a texture
                    // does. Keying it on the texture — the obvious
                    // reading of "is there a picture to show?" — leaves
                    // the library on screen for the whole gap between
                    // pressing Play and the first frame arriving, so the
                    // library visibly flashes back at you after you have
                    // already chosen a game.
                    if self.core.is_none() {
                        // With no ROM open this surface IS the library:
                        // §2's IA puts Library at the root.
                        self.library_home(ui);
                    } else if let Some(texture) = &self.texture {
                        ui.add(egui::Image::from_texture(texture).shrink_to_fit());
                    } else {
                        // Core up, no frame yet. One line rather than an
                        // empty rectangle, because a black screen is
                        // exactly what a ROM that FAILED to start also
                        // looks like.
                        ui.vertical_centered(|ui| {
                            ui.add_space(ui.available_height() * 0.45);
                            ui.add(readout(egui::RichText::new(&self.status).weak()));
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
        self.apply_theme(&ctx);
        self.poll_input(&ctx);
        self.pump_core_events(&ctx);
        self.maybe_request_canvas_snapshot();
        self.sync_event_subscription();
        self.pump_trace();
        self.pump_audio_scopes();

        self.menu_bar(ui);
        self.controls_bar(ui);
        self.video_panel(ui);
        self.crash_dialog(&ctx);
        self.layers_debug_window(&ctx);
        self.enhance_window(&ctx);
        self.controls_window(&ctx);
        self.settings_window(&ctx);
        self.overlay_menu(&ctx);
        self.states_modal(&ctx);
        self.pump_authoring();
        self.author_window(&ctx);
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

/// Every heuristic subject to D-004's trust ladder, in one place.
///
/// A plain list rather than something derived: `rf_enhance::trust` is
/// deliberately generic over heuristic *names* (W3-05c's brief: "design
/// it as a general mechanism"), so the set of names that actually exist
/// is the shell's knowledge, not the mechanism's. New heuristics —
/// stitcher scene-cut, profile decoders — join by appearing here.
pub(crate) const HEURISTICS: &[&str] = &["anti-flicker"];

/// The two same-geometry renderings of one frame the compare view and the
/// screenshot both work from (ticket W3-04).
///
/// "Same frame" is structural rather than maintained: both are derived
/// from a single `FrameMsg`/`FrameBundle` pair inside one
/// `pump_core_events` call, so there is no code path that can pair an
/// original from frame N with an enhanced from frame N-1.
/// Both screenshots as (filename, PNG bytes), with no I/O — the pure half
/// of [`RetroForgeApp::write_screenshots`], so FR-FE-005's "captures
/// both" is testable without an egui app or a filesystem (ticket W3-04).
pub(crate) fn screenshot_files(buffers: &CompareBuffers, stamp: u64) -> [(String, Vec<u8>); 2] {
    [
        (
            format!("retroforge-{stamp}-original.png"),
            rf_renderer::png::encode_rgba(&buffers.original, buffers.width, buffers.height),
        ),
        (
            format!("retroforge-{stamp}-enhanced.png"),
            rf_renderer::png::encode_rgba(&buffers.enhanced, buffers.width, buffers.height),
        ),
    ]
}

pub(crate) struct CompareBuffers {
    /// Accuracy-exact, resolved from `FrameBundle::video` — documented as
    /// assembled from `video_scanline` only, never `overlay_scanline`.
    pub original: Vec<u8>,
    /// What the shell actually displays: the same frame with any
    /// enhancement overlay painted over it (`FrameBuffer::overlay_scanline`).
    pub enhanced: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[cfg(test)]
mod compare_tests {
    use super::*;

    fn buffers() -> CompareBuffers {
        // Two DIFFERENT images — the whole point of a compare view is
        // that the halves differ, and a test using one buffer twice would
        // pass on an implementation that wrote the same file twice.
        CompareBuffers {
            original: [255, 0, 0, 255].repeat(4),
            enhanced: [0, 0, 255, 255].repeat(4),
            width: 2,
            height: 2,
        }
    }

    /// FR-FE-005: "original and enhanced buffers **separately**" — two
    /// files, distinctly named, with distinct contents.
    #[test]
    fn screenshot_captures_both_buffers_as_two_distinct_pngs() {
        let files = screenshot_files(&buffers(), 1234);
        assert_eq!(files[0].0, "retroforge-1234-original.png");
        assert_eq!(files[1].0, "retroforge-1234-enhanced.png");
        for (name, png) in &files {
            assert_eq!(
                &png[..8],
                &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
                "{name} must be a PNG"
            );
        }
        assert_ne!(
            files[0].1, files[1].1,
            "the two captures must differ — identical bytes would mean one buffer was \
             written twice, which is the failure this requirement exists to prevent"
        );
    }

    /// The compare composition the shell paints is the same function the
    /// renderer tests cover, wired to the same buffers the screenshot
    /// uses — so "what you see" and "what you capture" come from one
    /// source of truth.
    #[test]
    fn split_compare_uses_both_halves_of_the_same_frame() {
        let b = buffers();
        let split = rf_renderer::compose_split(&b.original, &b.enhanced, b.width, b.height, 0.5);
        assert_eq!(&split[0..4], &[255, 0, 0, 255], "left half is the original");
        assert_eq!(&split[4..8], &[0, 0, 255, 255], "right half is enhanced");
        assert_ne!(split, b.original);
        assert_ne!(split, b.enhanced);
    }
}

/// The status bar's own helpers (ticket W10-01).
#[cfg(test)]
mod hud_tests {
    /// `elide_front` keeps the tail, because the distinguishing part of a
    /// profile name is its end.
    #[test]
    fn elide_front_keeps_the_end_and_marks_the_cut() {
        let long = "an-extremely-verbose-profile-name-nobody-would-choose";
        let out = super::elide_front(long);
        assert_eq!(out.chars().count(), super::PROFILE_CHIP_BUDGET);
        assert!(
            out.starts_with('\u{2026}'),
            "the cut must be visible: {out}"
        );
        assert!(
            long.ends_with(out.trim_start_matches('\u{2026}')),
            "the surviving text must be the END of the original: {out}"
        );
    }

    /// Short enough to fit is returned untouched — and borrowed, not
    /// reallocated, on the path the bar takes every single frame.
    #[test]
    fn elide_front_leaves_a_short_name_alone() {
        let short = "rf-scroller";
        assert!(matches!(
            super::elide_front(short),
            std::borrow::Cow::Borrowed("rf-scroller")
        ));
    }

    /// **Counts characters, not bytes.** A ROM directory with a
    /// non-ASCII name is completely ordinary, and slicing UTF-8
    /// mid-codepoint panics — which in this codepath would take the whole
    /// status bar down every frame, on nothing worse than an accented
    /// filename.
    #[test]
    fn elide_front_does_not_split_a_multibyte_character() {
        // 30 chars, 60 bytes.
        let name = "\u{e9}".repeat(30);
        let out = super::elide_front(&name);
        assert_eq!(out.chars().count(), super::PROFILE_CHIP_BUDGET);
        assert!(out.chars().all(|c| c == '\u{2026}' || c == '\u{e9}'));
    }
}
