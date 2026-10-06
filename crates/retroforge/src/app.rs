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
use std::sync::Arc;

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

/// Ticket W20-09 (`docs/design/ENHANCEMENT_AUDIT.md` §2): whether the play
/// view actually runs `rf_renderer::metalfx::MetalFxScaler`. It does not
/// yet — W16-08 shipped the scaler, the Settings radio and the badge
/// suffix, and nothing constructs the scaler — so the badge must not
/// claim MetalFX and the Settings radio says it is not used yet.
pub const METALFX_SCALER_WIRED: bool = false;

/// Ticket W20-04: how long a resized window must hold still before its
/// size is written to `settings.toml`.
const WINDOW_SIZE_SETTLE: std::time::Duration = std::time::Duration::from_millis(750);

/// Ticket W20-06: a save-slot card's content width and thumbnail height
/// (the thumbnail is fitted inside, letterboxed, whatever its aspect).
const SLOT_CARD_WIDTH: f32 = 168.0;
const SLOT_THUMB_HEIGHT: f32 = 126.0;

/// How wide decoded widescreen renders, in dots (ticket W11-03).
///
/// 400 is 16:9 at the SNES's 224 visible lines (398.2, rounded to an even
/// number so the 72-dot margin splits evenly either side of the 256-dot
/// picture). The PPU clamps to its own `MAX_WIDTH`, so this cannot ask
/// for something the renderer will not deliver.
pub const WIDESCREEN_WIDTH: usize = 400;

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

/// The four shades a decoded level is drawn with (ticket W11-02).
///
/// A neutral ramp, NOT a palette sampled from the live frame. A level
/// view tinted by whatever palette happened to be loaded when you opened
/// it would look authoritative and be wrong in a way nobody could check
/// — the same class of error as fog that guesses geometry, which
/// FR-ENH-004 forbids the stitcher from making. When a profile learns to
/// declare its level palette, this is the constant that gives way.
/// Where a script's memory window starts, and how long it is (ticket
/// W11-04).
///
/// PRG-RAM at `$6000`, 8 KiB — where cc65 puts a game's globals, and the
/// window `lua_overlay_demo.rs` publishes. It is a WINDOW rather than
/// "all of memory" on purpose: `MemoryWindow::read_u8` returns 0 outside
/// it, so what a script can see is a decision the shell makes rather
/// than an accident of what happened to be mapped.
const SCRIPT_WINDOW_BASE: u32 = 0x6000;
const SCRIPT_WINDOW_LEN: usize = 0x2000;

const LEVEL_PALETTE: [[u8; 3]; 4] = [
    [0x10, 0x12, 0x16],
    [0x44, 0x4C, 0x5A],
    [0x8A, 0x95, 0xA6],
    [0xD6, 0xDA, 0xE2],
];

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
///
/// **Ticket W20-05: symbols no longer come from here at all.** Every
/// symbol is a Phosphor icon named by role in [`crate::icons`]; this list
/// is now typographic punctuation only (`★`/`☆` left it with the profile
/// chip). And it is no longer the only guard: `tests/glyphs_render.rs`
/// scans every string literal in `src/` against the installed fonts, so a
/// character missing from this list can no longer ship as tofu unseen.
pub const PROPORTIONAL_GLYPHS: &str = "\u{b7}\u{2026}\u{2014}\u{203a}";

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
pub(crate) fn readout(text: impl Into<egui::RichText>) -> egui::Label {
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

/// Ticket W15-03: which game the one Game Settings window is editing when
/// it is not the game currently running (`RetroForgeApp::game_settings_target`
/// is `None` for that case). Carries a working copy of the settings loaded
/// when the context menu's "Game settings" item was clicked; every change
/// is saved back through `game_settings::save` immediately, the same
/// no-Apply-button rule as every other setting in this app.
struct GameSettingsTarget {
    hash: String,
    title: String,
    settings: crate::game_settings::GameSettings,
}

/// Ticket W15-03: the "Hash info" context-menu item's popup content — the
/// normalized hash family `rf-cart` computes for one library entry, plus
/// the profile that claims it, if any.
struct HashInfoPopup {
    title: String,
    /// `None` when the file no longer parses as a cartridge this build
    /// recognizes (e.g. it changed on disk since the scan).
    hashes: Option<rf_cart::RomHashes>,
    profile: Option<std::path::PathBuf>,
    /// Set instead of `hashes` when the file could not even be read.
    error: Option<String>,
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

/// Ticket W16-14: a cheap FNV-1a fingerprint of two byte slices in
/// sequence — [`RetroForgeApp::refresh_mode7_ground_render`]'s "has the
/// live VRAM/CGRAM snapshot's plane region actually changed" cache key.
/// Not cryptographic and not meant to be: a collision only costs one
/// stale-looking frame of ground texture, never a correctness bug, the
/// same trade-off `tests/mode7_plane_golden.rs`'s own `fingerprint` makes
/// for its golden hashes.
fn fnv1a_hash(a: &[u8], b: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in a.iter().chain(b.iter()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Ticket W15-05: per-hash badge facts for the card grid (`UX_WAVE_15.md`
/// §3.1), read once per library scan alongside `library_meta` rather than
/// once per card per frame — same "cheap enough to do for every row"
/// reasoning `RecencyMeta`'s own doc gives, extended to the two extra
/// badges a card shows that a row (pre-W15-05) never did.
#[derive(Debug, Clone, Default)]
struct LibraryBadges {
    /// Normalized hashes with at least one occupied save-state slot.
    has_states: std::collections::HashSet<String>,
    /// Normalized hashes whose game settings have enhancement active
    /// (`crate::game_settings::Mode::enhancement_active`).
    enhanced: std::collections::HashSet<String>,
    /// Normalized hashes a profile's identity list names by sha256
    /// (`crate::level_view::all_profile_sha256s`).
    profile_matched: std::collections::HashSet<String>,
}

/// Ticket W15-05, §3: card layout constants, shared between
/// `RetroForgeApp::library_cards` (drawing) and
/// `RetroForgeApp::library_grid_columns` (the arrow-key stride) so the
/// two can never disagree about how many cards fit in a row.
const CARD_WIDTH: f32 = 150.0;
const CARD_SPACING: f32 = 10.0;
/// "16:15-ish console aspect" (§3's Card bullet) — close to the NES's own
/// 256x240 (16:15) frame, so a save-state/first-frame thumbnail from
/// either console letterboxes with only a thin margin rather than a
/// visibly wrong box.
const CARD_THUMB_HEIGHT: f32 = (CARD_WIDTH - 12.0) * 15.0 / 16.0;

/// The whole application's UI-thread-owned state.
pub struct RetroForgeApp {
    /// Ticket W11-03: why each background did or did not widen. `Some` is
    /// a refusal with its reason; `None` means widened (or off).
    widescreen_decisions: [Option<&'static str>; 4],
    /// Ticket W11-05: the loaded HD pack and its decoded images.
    hd_pack: Option<(
        rf_enhance::hdpack::HdPack,
        Vec<rf_enhance::hd_render::PackImage>,
    )>,
    /// What the import said, and what it could not satisfy.
    hd_summary: Option<String>,
    hd_unsatisfied: Vec<String>,
    /// What the last composite actually replaced.
    hd_report: Option<rf_enhance::hd_render::CompositeReport>,
    /// Ticket W16-02: whether the Upscale Studio window is open. Also
    /// what gates `CoreCommand::SetStudioCapture` — see
    /// `Self::set_upscale_studio_open`.
    show_upscale_studio: bool,
    /// Tiles captured while the studio was open, deduplicated, with the
    /// position history animation grouping needs.
    upscale_studio_session: crate::upscale_studio::CaptureSession,
    /// Per-tile review decisions, by asset hash. A tile with no entry is
    /// `Decision::Approved` — see that variant's own doc.
    upscale_studio_decisions: std::collections::BTreeMap<String, crate::upscale_studio::Decision>,
    /// The last completed Run's pack, if one has finished.
    upscale_studio_pack: Option<rf_ai::studio::StudioPack>,
    /// A Run in flight on a worker thread — polled every frame
    /// (`Self::poll_upscale_studio_run`), never blocked on.
    upscale_studio_run: Option<std::sync::mpsc::Receiver<crate::upscale_studio::RunOutcome>>,
    /// The model/provider/licence line criterion 3 asks for, plus write
    /// results and errors — one line, always current, never silently
    /// stale (same "state is in the word" principle `crate::toast`'s
    /// module doc cites).
    upscale_studio_status: String,
    /// Preview textures, nearest-scaled ORIGINAL tiles — decoded once per
    /// asset hash and cached, same pattern as
    /// `Self::library_thumbnail_texture`.
    upscale_studio_original_textures: std::collections::HashMap<String, egui::TextureHandle>,
    /// Preview textures for the UPSCALED result, invalidated (cleared)
    /// every time a new Run completes.
    upscale_studio_upscaled_textures: std::collections::HashMap<String, egui::TextureHandle>,
    /// Where the last "Write pack" wrote to, shown next to the button per
    /// criterion 3 ("the destination shown").
    upscale_studio_write_dir: Option<std::path::PathBuf>,
    /// Ticket W14-20 defect 2: the handle `eframe::CreationContext` hands
    /// `new` below, kept so `open_rom_path` can build a waker
    /// (`core_thread::spawn_with_waker`'s `Option<Arc<dyn Fn() + Send +
    /// Sync>>`) for each core it spawns. `egui::Context` is cheap to
    /// clone (an `Arc`-backed handle — `context_impl_send_sync`'s own test
    /// in `egui::context` proves it is also `Send + Sync`), so storing an
    /// owned clone here rather than threading `&egui::Context` through
    /// `open_rom_path`'s call sites is the smaller change.
    ctx: egui::Context,
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
    /// Ticket W15-07: the last-shown crash report, kept alive purely for
    /// rendering while `crash_dialog`'s fade-out plays after `crash` has
    /// already been cleared. Never read for anything but that render —
    /// `crash.is_some()` (via `crash`, not this field) is still what
    /// actually gates the dialog's presence.
    crash_fade_cache: Option<CoreCrashReport>,
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
    /// Ticket W11-01: the last frame's rgba as the UI received it, kept
    /// so a test can prove an enhancement changed the PICTURE rather than
    /// merely that a command was sent. A core thread that accepted
    /// `SetDeflicker` and ignored it would pass any plumbing assertion.
    last_frame_rgba: Option<Vec<u8>>,
    /// Dimensions of `last_frame_rgba` (ticket W11-03).
    last_frame_size: Option<(usize, usize)>,
    /// Ticket W20-01: the CORE's frame size, before any HD-pack upscale —
    /// what `crate::play_view` sizes the picture from. `last_frame_size`
    /// is the composited texture's size, which an HD pack multiplies.
    core_frame_size: Option<(usize, usize)>,
    /// Ticket W20-01: the present mode last handed to eframe, so V-sync is
    /// re-applied only when the setting changes (and once at startup).
    applied_vsync: Option<bool>,
    /// Ticket W20-08: cached output-device names for Settings › Audio;
    /// cleared when the Settings window closes so a newly plugged device
    /// shows up next time.
    audio_devices: Option<Vec<String>>,
    /// Ticket W20-01: where the picture was drawn last frame.
    last_play_rect: Option<egui::Rect>,
    /// Ticket W20-04: a windowed size seen this frame that differs from
    /// the saved one, and when it was first seen — saved once it has held
    /// still for [`WINDOW_SIZE_SETTLE`], so a drag-resize writes the file
    /// once rather than every frame.
    pending_window_size: Option<([f32; 2], std::time::Instant)>,
    /// Edge detector for a pad-bound Fullscreen (a pad button is polled
    /// as held, not pressed).
    fullscreen_pad_was_held: bool,
    /// The last fullscreen state a hotkey/menu asked the window for —
    /// `ViewportCommand`s go to the windowing backend, which a headless
    /// test harness does not have, so this is what a test can read.
    last_fullscreen_request: Option<bool>,
    /// Ticket W20-02: the shader chain, built on first use from the shared
    /// `GpuContext`; its budget gate; and the one-line note Settings shows
    /// when a shader was paused for speed or failed.
    shader_chain: Option<rf_renderer::ShaderChain>,
    shader_budget: rf_renderer::fog::BudgetGate,
    shader_note: Option<String>,
    /// Test-only: fingerprint every displayed frame (off otherwise — it
    /// would hash megabytes per frame for nothing).
    hash_display_for_test: bool,
    display_hash: Option<u64>,
    /// Ticket W20-12: on-screen-display cards over the game picture.
    osd: crate::toast::ToastStack,
    /// Ticket W20-06: decoded save-slot thumbnails.
    slot_textures: crate::slot_cards::SlotTextures,
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
    /// Ticket W15-01: the selected library row/card, by PATH rather than
    /// an index into the filtered list — an index silently points at a
    /// different game the moment the search box or console filter
    /// changes what is filtered out from under it. `None` until the user
    /// clicks a row or moves the selection with an arrow key/gamepad
    /// direction. Not persisted across launches for the same reason
    /// `library_search` isn't: a selection is a within-session gesture.
    library_selected: Option<std::path::PathBuf>,
    /// Whether the library search box has keyboard focus as of the last
    /// frame the toolbar ran (ticket W15-01). Enter-to-launch and the
    /// arrow-key selection walk both stand down while this is true —
    /// otherwise typing in the search box would steer the selection and
    /// Enter would launch a game instead of just accepting the filter.
    library_search_focused: bool,
    /// Ticket W15-02: the Recently played / Favourites chips. Radio-like
    /// with each other by construction (`RecencyFilter` is one field, not
    /// two booleans) and combined (AND) with `library_console_filter` in
    /// `library_grid`. Not persisted, same reasoning as `library_search`.
    library_recency_filter: crate::library::RecencyFilter,
    /// Ticket W15-02: the sort control's explicit choice, `None` for "not
    /// chosen yet" — `crate::library::filter_and_sort`'s own doc explains
    /// why that is a `None` rather than a fourth enum variant.
    library_sort: Option<crate::library::SortMode>,
    /// Ticket W15-02: play history per normalized ROM hash, read from
    /// `game_settings` once per scan (or refreshed for one entry on
    /// launch) rather than once per row per frame — UX_WAVE_15 §11
    /// acceptance 1's "cheap enough to do for every row".
    library_meta: std::collections::BTreeMap<String, crate::library::RecencyMeta>,
    /// Ticket W15-05: extra per-hash badge facts (has save states, an
    /// enhanced-mode game setting, a matched profile) — read once per
    /// scan for the same reason `library_meta` is (`Self::load_badges`).
    library_badges: LibraryBadges,
    /// Ticket W15-05, `UX_WAVE_15.md` §3: the toolbar's Grid/List toggle.
    /// Mirrors `settings.library.view`; kept as its own field (rather than
    /// read through `self.settings` every frame) so a test can flip it
    /// directly (`set_library_view_for_test`) without going through a
    /// settings file.
    library_view: crate::library::LibraryView,
    /// Ticket W15-05: decoded card thumbnails, keyed by normalized ROM
    /// hash. `None` means "resolved, and there is no thumbnail" (the
    /// placeholder card is drawn instead) — distinct from "not looked up
    /// yet" (no entry at all), so a hash with no art is not re-resolved
    /// (state-slot scan, art-folder walk) every single frame it is on
    /// screen.
    library_thumbnail_textures: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    /// Ticket W15-05: the on-disk cache the one-time first-frame capture
    /// persists through (`crate::thumbnail`, `rf-cache`'s size-capped
    /// LRU). `None` when there is nowhere to root it or opening it
    /// failed — degrades to "no first-frame capture persists this
    /// session", never a reason to fail loading a ROM.
    thumbnail_cache: Option<rf_cache::Cache>,
    /// Ticket W15-09, ruling D-011: the fetched-art cache, a separate
    /// `rf-cache::Cache` with its own cap (`art_cache_cap_mb`) — see
    /// `crate::art`'s module doc for why this is a second store rather
    /// than a namespace inside `thumbnail_cache`. `None` under the same
    /// conditions as `thumbnail_cache`.
    art_cache: Option<rf_cache::Cache>,
    /// Ticket W15-09: the box-art HTTP client. A trait object so tests
    /// can swap in `crate::art::test_support::FakeArtClient` (the kittest
    /// scenario does this via `set_art_client_for_test`) — production
    /// code always constructs `crate::art::UreqArtClient`.
    art_client: Arc<dyn crate::art::ArtClient>,
    /// Ticket W15-09: the background fetch queue. `None` until the first
    /// frame that finds the toggle on and a config root to cache into —
    /// created lazily rather than unconditionally at startup so a player
    /// who never opts in never pays for a spawned thread.
    art_fetcher: Option<crate::art::ArtFetcher>,
    /// Ticket W15-09: which hashes' current thumbnail texture came from a
    /// fetch, so `library_cards` can draw the network indicator
    /// (`UX_WAVE_15.md` §4: "nothing on screen should imply 'local' when
    /// it wasn't"). Kept alongside `library_thumbnail_textures` rather
    /// than folded into it — that map's value type is a rendering
    /// primitive (`egui::TextureHandle`), this one is provenance.
    library_thumbnail_is_fetched: std::collections::HashSet<String>,
    /// Ticket W15-09 acceptance 4: "one toast per session at most" for a
    /// failed fetch. Set the first time any fetch fails; never reset
    /// during a session.
    art_fetch_failure_toast_shown: bool,
    /// Ticket W11-02: the decoded level for the running ROM, when a
    /// profile matched and declared one. `None` otherwise, which is the
    /// ordinary case and never an error.
    level_session: Option<crate::level_view::LevelSession>,
    /// The decoded level as an egui texture. Built ONCE — a level does
    /// not change while you play it, so re-rendering it per frame would
    /// be ~688 KB of work to produce an identical image. The only thing
    /// that moves is the viewport outline, and that is painted over the
    /// texture rather than baked into it.
    level_texture: Option<egui::TextureHandle>,
    /// Where the live camera is in level space, from the bounded probe.
    level_camera: Option<(i64, i64)>,
    /// Which console the running ROM is (ticket W11-12).
    ///
    /// The honesty badge said "NES" unconditionally until the app could
    /// open a SNES ROM — at which point a hardcoded string became a
    /// false statement on the one surface FRONTEND_UI §1 exists to keep
    /// honest.
    console_label: &'static str,
    /// Ticket W11-04: the loaded Lua script, if any.
    ///
    /// Lives on the UI thread, deliberately and necessarily:
    /// `mlua::Lua` is not `Send`, which `rf_plugin_sdk::sandbox`'s own
    /// doc gives as the reason its `Bridge` uses `Rc<RefCell<..>>` rather
    /// than `Arc<Mutex<..>>`. The memory it reads is published to it from
    /// the core thread instead.
    script_host: Option<rf_plugin_sdk::ScriptHost>,
    /// What the script asked to draw on the last frame it ran.
    script_overlay: Vec<rf_plugin_sdk::sandbox::OverlayCmd>,
    /// Why the last script load failed, for the console to show. A script
    /// that refuses to load must SAY so — a silently absent overlay is
    /// indistinguishable from one that drew nothing.
    script_status: Option<String>,
    /// Ticket W15-04: a script load/manifest failure queued for the next
    /// frame's toast. A separate field rather than reading
    /// `script_status` directly at toast time, because `load_script`
    /// (and its `#[doc(hidden)] load_script_for_test` twin, called by
    /// `tests/script_reaches_the_app.rs` with no `egui::Context` in
    /// scope) has no `Context` to push a toast with — this queues the
    /// TEXT instead, and `impl eframe::App::ui` drains it into a toast
    /// once a frame, when a `Context` is always available.
    script_error_toast_pending: Option<String>,
    /// Ticket W15-04: non-blocking toasts (`docs/design/UX_WAVE_15.md`
    /// §5) — ROM folder added, library rescanned, state saved, script
    /// error. Drawn once per frame from `impl eframe::App::ui`.
    toasts: crate::toast::ToastStack,
    /// Set by the toolbar's "Rescan" button so `poll_library_scan` knows
    /// the scan it is about to adopt was USER-requested and should toast
    /// with a count — the automatic first scan on boot
    /// (`library_home`'s own `self.rescan_library()` when
    /// `self.library.is_none()`) must not toast, since nobody asked for
    /// it and a toast on every launch would just be noise.
    library_rescan_toast_pending: bool,
    /// Ticket W15-04: which occupied slot the user tried to Save over,
    /// awaiting the overwrite-confirmation `egui::Modal`. `None` slots
    /// (`state_slots::SlotInfo::saved.is_none()`) never populate this —
    /// there is nothing to overwrite, so nothing to confirm.
    pending_overwrite: Option<crate::state_slots::SlotId>,
    /// Ticket W15-07: `pending_overwrite`'s last value, kept for
    /// `overwrite_confirm_modal`'s fade-out render only — see
    /// `crash_fade_cache`'s doc for why this is a separate field rather
    /// than delaying when `pending_overwrite` itself clears.
    pending_overwrite_fade_cache: Option<crate::state_slots::SlotId>,
    /// Ticket W15-04: true while the quit-with-unsaved-state confirmation
    /// is up. Set by `request_quit`, cleared by the modal's own Quit/
    /// Cancel buttons or an outside click.
    pending_quit: bool,
    /// The core's frame number ([`Self::position`]'s first field) as of
    /// the last successful save, so `request_quit` can tell "a state was
    /// saved for this exact frame" from "the core has moved on since".
    /// `None` before any save this session, which `request_quit` treats
    /// as "unsaved" whenever the core has run at all.
    last_save_frame: Option<u64>,
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
    /// Ticket W15-08 (`docs/design/UX_WAVE_15.md` §9): the most-recently-
    /// active input device, updated in `poll_input` from real hardware
    /// signals only (never from the pad's own synthesized `egui::Event`s
    /// — `Self::track_input_device`'s doc explains why that distinction
    /// matters). Consulted by `apply_theme` (larger type while a gamepad
    /// drives) and by `library_cards`/`library_rows` (the pad focus
    /// ring). Live state, never persisted: `AppSettings` has no field for
    /// it, by design.
    last_active_input: crate::ui_nav::InputDevice,
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
    /// Ticket W20-03: the in-game menu paused a running game, so closing
    /// it must resume. `false` when the game was already paused — the
    /// menu must not start a game the player had stopped.
    menu_paused_game: bool,
    /// Ticket W20-10: the Quick Menu's selected section, and whether its
    /// rail entry should take keyboard focus on the next draw (just opened).
    quick_section: crate::quick_menu::Section,
    quick_focus_pending: bool,
    /// When `state_slots` was last read from disk.
    state_slots_scanned: Option<std::time::Instant>,
    /// Ticket W20-10: the file the running game was opened from (Reset).
    current_rom_path: Option<std::path::PathBuf>,
    /// Ticket W15-03 (`UX_WAVE_15.md` §5, §11): whether the one Game
    /// Settings window is open. Opened identically from the context menu,
    /// the Enhance menu, and the overlay menu — the SAME instance, which is
    /// what `game_settings_target` (below) exists to make true even though
    /// those three routes can name three different games.
    show_game_settings: bool,
    /// `None` means "the game currently running" — the window then reads
    /// and writes `current_game_hash`/`current_game_settings` directly,
    /// exactly as the Enhance menu's Mode/De-flicker/Heuristics controls
    /// did before this ticket moved them into this window. `Some` names a
    /// library row picked from its context menu, which the window instead
    /// loads and saves through its own settings file — deliberately never
    /// touching `current_game_settings`, so picking "Game settings…" on one
    /// game from the library can never clobber another game's (or the
    /// running game's) settings.
    game_settings_target: Option<GameSettingsTarget>,
    /// Ticket W15-03: a gamepad `Start` press this frame, latched here by
    /// `poll_input` and consumed by `library_grid` on the very same frame
    /// — `NavAction::Menu` deliberately produces no `egui::Event`
    /// (`ui_nav.rs`'s own doc), since it is a shell decision rather than
    /// focus movement, so this flag is that decision's wire.
    pad_menu_requested: bool,
    /// Ticket W15-03: whether the pad-opened context menu (as opposed to
    /// the mouse's right-click one) is showing for the currently selected
    /// library row. A plain bool rather than `library_selected` itself,
    /// because the row a `Start` press opened a menu for must stay open
    /// even if a later frame moves keyboard/pad focus elsewhere before the
    /// player dismisses it.
    library_context_menu_open: bool,
    /// Ticket W15-03: the small "Hash info" popup content, or `None` when
    /// closed.
    hash_info: Option<HashInfoPopup>,
    /// Configured library roots, as the user chose them (resolved at scan
    /// time, never stored canonicalized — see `crate::library_roots`).
    library_roots: Vec<crate::library::LibraryRoot>,
    /// Last scan's result. `None` until the first scan, which is why the
    /// window scans on open rather than at startup: a cold start must not
    /// wait on a folder walk over a network share.
    library: Option<crate::library::Library>,
    /// A scan running on a worker thread (ticket W14-02).
    ///
    /// The scan reads and hashes every file it finds, and since W14-01
    /// decompresses every archive too — 17 seconds in release for a real
    /// 2546-archive collection. That ran on the UI thread until now, which
    /// froze the window for the whole of it.
    library_scan: Option<std::sync::mpsc::Receiver<crate::library::Library>>,
    /// Normalized hash of the ROM currently loaded (ticket W2-07) — the key
    /// its per-game settings are stored under. `None` for a ROM this build
    /// could not identify, which is deliberate: settings keyed by a hash we
    /// could not compute would be settings that silently apply to the wrong
    /// game later.
    current_game_hash: Option<String>,
    /// Ticket W11-09: every hash family for the running ROM, not just the
    /// normalized sha256 `current_game_hash` holds.
    ///
    /// Profile matching needs all four — `IdentityEntry` declares
    /// sha256, sha1, md5 and crc32, and a profile identified by a
    /// published No-Intro CRC32 is the only way to name a commercial
    /// title nobody here holds a copy of. `current_game_hash` stays as
    /// the per-game settings key, which is a different job: one identity
    /// per game, chosen once.
    current_game_hashes: Option<rf_cart::RomHashes>,
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
    /// Ticket W15-06: the App-namespaced hotkeys (`crate::app_bindings`),
    /// namespaced apart from `bindings` above so the two tables can never
    /// share a row — collision is instead the runtime check
    /// `crate::app_bindings::key_conflicts_with_game`/
    /// `game_key_conflicts_with_app` run on every remap in either
    /// direction.
    app_bindings: crate::app_bindings::AppBindings,
    /// What the App section of the Controls window is waiting to
    /// capture, if anything — the App remap's own `awaiting_key`.
    awaiting_app_key: Option<crate::app_bindings::AppAction>,
    /// The most recent inline conflict message either remap flow (game or
    /// App) produced, shown next to the row that triggered it. `None`
    /// once the next successful bind clears it.
    binding_conflict: Option<String>,
    /// Ticket W15-06: which save/load slot the App hotkeys act on — set
    /// whenever the states modal's Save/Load is clicked, so "Save state"
    /// (F5) always means "the slot I was just looking at", not a fixed
    /// slot. `None` until the modal has been used once this session, at
    /// which point `Self::active_slot` degrades to Slot 1 (§6: "or slot 1
    /// if none chosen").
    active_slot: Option<crate::state_slots::SlotId>,
    /// Held state of the App hotkeys' two HOLD actions (fast-forward,
    /// hold-to-peek), tracked across frames so `poll_app_hotkeys` sends
    /// the pacing command only on the transition, not every frame the key
    /// is down.
    fast_forward_held: bool,
    /// OR'd into `peeking_original` by `video_panel` — the hotkey's
    /// contribution to hold-to-peek, alongside the existing badge-hold
    /// gesture. A separate field because `video_panel` computes the badge
    /// gesture with a hard assignment each frame; this keeps the hotkey
    /// from being clobbered by it or vice versa.
    peek_key_held: bool,
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
    /// Ticket W16-13: the Diorama pass, built lazily on [`Self::gpu`] the
    /// first frame Diorama is actually wanted — never built eagerly at
    /// startup, so a session that never touches Diorama never pays for a
    /// pipeline/bind-group-layout/sampler it will never use (`DioramaPass::
    /// new`'s own cost, same "build once, reuse" shape [`Self::compositor`]
    /// already uses for the Ultrawide camera, just deferred one step
    /// further).
    diorama_pass: Option<rf_renderer::diorama::DioramaPass>,
    /// The most recent [`enhanced_view::compose_diorama`] result, or
    /// `None` when Diorama is not currently wanted (`Self::diorama_wanted`)
    /// — cleared the same frame Diorama turns off, unlike
    /// [`Self::ultrawide_render`] (which keeps its last `Err` around as a
    /// user-visible reason). Diorama has no such message: an unavailable
    /// Diorama degrades silently to the flat view (`enhanced_view::
    /// select_active_view`'s own doc explains why).
    diorama_render: Option<enhanced_view::DioramaRender>,
    /// The egui texture built from [`Self::diorama_render`]'s `rgba`, same
    /// "persistent `TextureHandle`, `.set()` on later frames" shape
    /// [`Self::ultrawide_texture`] already uses.
    diorama_texture: Option<egui::TextureHandle>,
    /// Whether Diorama is currently EFFECTIVE (ticket W16-13:
    /// `crate::enhance_ui::feature_rows`'s "diorama" row, `FeatureRow::
    /// effective`) as of the last [`Self::sync_diorama_subscription`]
    /// call — the single source of truth this struct keeps for "should
    /// the level probe/layer extraction be armed for Diorama's sake, and
    /// should `Self::refresh_diorama_render` do any work this frame".
    /// Re-derived every repaint (`sync_diorama_subscription`'s own doc);
    /// this field only exists to detect the CHANGE (mirrors
    /// `Self::event_subscription_active`'s identical "wants vs currently
    /// armed" shape).
    diorama_wanted: bool,
    /// The decoded level's ground texture for the Diorama pass, computed
    /// once per ROM and cached (`Self::diorama_ground_rgba`'s own doc) —
    /// `Rc` so a cache hit is a refcount bump, not a ~200 KB clone, on
    /// every one of 60 frames/second Diorama is active.
    diorama_ground_rgba: Option<(std::rc::Rc<Vec<u8>>, u32, u32)>,
    /// Ticket W16-13 acceptance 1: the SAME budget-gate mechanism
    /// `rf_renderer::fog`'s own pass uses (`tests/diorama_golden.rs`'s own
    /// "reuses the fog pass's budget gate, not a second one" test proves
    /// this is the genuine shared type), fed this session's own measured
    /// `DioramaPass::render` wall-clock time. If Diorama's own p95 ever
    /// creeps past the gate's threshold on THIS machine, `Self::
    /// refresh_diorama_render` stops calling `DioramaPass::render` and the
    /// view falls back to flat — the "drop rather than block" half of
    /// acceptance 1, enforced on the UI thread since the pass runs
    /// synchronously there (see that method's own doc for why sync is the
    /// right call at the ~2 ms this pass measures).
    diorama_budget: rf_renderer::fog::BudgetGate,
    /// Ticket W16-14: whether this session has ever seen a live
    /// `CoreEvent::Mode7` (BG mode 7 actually running) — the fact
    /// `enhance_ui::feature_rows`'s "mode7_ground" row is gated on
    /// (`Availability::NeedsGameState`). Sticky for the session rather
    /// than re-derived every frame: a game that briefly leaves mode 7
    /// (a menu, a pause screen) must not make the row flicker
    /// unavailable. Reset on every `open_rom_path` so a NEW game starts
    /// honest about what it has actually shown.
    mode7_seen: bool,
    /// Whether "Mode 7 as 3D" is currently EFFECTIVE, mirroring
    /// [`Self::diorama_wanted`]'s identical role for the walls tier —
    /// re-derived every repaint by [`Self::sync_diorama_subscription`],
    /// this field only exists to detect the change.
    mode7_ground_wanted: bool,
    /// The Mode 7 ground texture built from the live VRAM/CGRAM snapshot
    /// (`rf_snes::debug::render_mode7_plane_rgba`), cached by a cheap hash
    /// of the plane's own bytes so a VRAM snapshot that has not changed
    /// (the overwhelmingly common case — playfield tiles rarely change
    /// every frame) does not re-render 128x128 tiles at density 2 sixty
    /// times a second. `None` until the first Mode 7 frame arrives.
    mode7_plane_cache: Option<(u64, std::rc::Rc<Vec<u8>>, u32, u32)>,
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
    /// Ticket W13-02b: whether `SetSnesDebugCapture(true)` was last sent.
    snes_capture_active: bool,
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
        // Ticket W15-07: embed IBM Plex Sans before the first frame paints,
        // so nothing ever flashes egui's bundled `Ubuntu-Light` (whose
        // Ubuntu Font Licence is not OFL/Apache, NFR-011) even for one
        // repaint.
        crate::theme::install_fonts(&cc.egui_ctx);

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

        // Ticket W15-06: the App hotkeys load the same way, from their
        // own file (`crate::app_bindings`'s module doc on why it is not a
        // section of `bindings.rfbind`). A load problem here folds into
        // the same status line rather than a second one nobody reads.
        let (app_bindings, app_bindings_outcome) = match &config_root {
            Some(root) => crate::bindings_store::load_app(root),
            None => (
                crate::app_bindings::AppBindings::default(),
                crate::bindings_store::AppLoadOutcome::Defaulted,
            ),
        };
        let bindings_status = match app_bindings_outcome {
            crate::bindings_store::AppLoadOutcome::Defaulted
            | crate::bindings_store::AppLoadOutcome::Loaded(_) => bindings_status,
            crate::bindings_store::AppLoadOutcome::Rejected(reason) => format!(
                "{bindings_status}  App hotkey file could not be read ({reason}); using \
                 defaults. Your file was left untouched."
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

        // Ticket W15-05: opened once at startup, same lifetime as
        // `bindings`/`app_settings` above — a per-frame open would
        // re-read/self-heal `index.bin` sixty times a second for no
        // reason.
        let thumbnail_cache =
            crate::thumbnail::open_cache(config_root.as_deref(), &app_settings.paths);
        // Ticket W15-09: opened unconditionally too (opening a cache that
        // is never written to is cheap and self-healing, same as
        // `thumbnail_cache`) — the toggle gates FETCHING, not whether the
        // cache directory exists, so turning the toggle on mid-session
        // needs no re-open.
        let art_cache = crate::art::open_art_cache(config_root.as_deref(), &app_settings.paths);
        let library_view = app_settings.library.view;

        let app = RetroForgeApp {
            widescreen_decisions: [None; 4],
            hd_pack: None,
            hd_summary: None,
            hd_unsatisfied: Vec::new(),
            hd_report: None,
            show_upscale_studio: false,
            upscale_studio_session: crate::upscale_studio::CaptureSession::default(),
            upscale_studio_decisions: std::collections::BTreeMap::new(),
            upscale_studio_pack: None,
            upscale_studio_run: None,
            upscale_studio_status: String::new(),
            upscale_studio_original_textures: std::collections::HashMap::new(),
            upscale_studio_upscaled_textures: std::collections::HashMap::new(),
            upscale_studio_write_dir: None,
            ctx: cc.egui_ctx.clone(),
            core: None,
            texture: None,
            bg_layer_texture: None,
            sprite_layer_texture: None,
            status: "No ROM loaded \u{2014} File > Open ROM...".to_string(),
            crash: None,
            crash_fade_cache: None,
            running: false,
            awaiting_stepped_frame: false,
            position: None,
            audio_fill: None,
            run_hovered: false,
            last_frame_rgba: None,
            last_frame_size: None,
            core_frame_size: None,
            applied_vsync: None,
            audio_devices: None,
            last_play_rect: None,
            pending_window_size: None,
            fullscreen_pad_was_held: false,
            last_fullscreen_request: None,
            slot_textures: crate::slot_cards::SlotTextures::default(),
            osd: crate::toast::ToastStack::osd(),
            shader_chain: None,
            shader_budget: rf_renderer::fog::BudgetGate::new(),
            shader_note: None,
            hash_display_for_test: false,
            display_hash: None,
            library_search: String::new(),
            library_console_filter: None,
            library_scans: 0,
            library_selected: None,
            library_search_focused: false,
            library_recency_filter: crate::library::RecencyFilter::All,
            library_sort: None,
            library_meta: std::collections::BTreeMap::new(),
            library_badges: LibraryBadges::default(),
            library_view,
            library_thumbnail_textures: std::collections::HashMap::new(),
            thumbnail_cache,
            art_cache,
            art_client: Arc::new(crate::art::UreqArtClient),
            art_fetcher: None,
            library_thumbnail_is_fetched: std::collections::HashSet::new(),
            art_fetch_failure_toast_shown: false,
            level_session: None,
            level_texture: None,
            level_camera: None,
            console_label: "NES",
            script_host: None,
            script_overlay: Vec::new(),
            script_status: None,
            script_error_toast_pending: None,
            toasts: crate::toast::ToastStack::default(),
            library_rescan_toast_pending: false,
            pending_overwrite: None,
            pending_overwrite_fade_cache: None,
            pending_quit: false,
            last_save_frame: None,
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
            last_active_input: crate::ui_nav::InputDevice::default(),
            last_nav_tick: None,
            #[cfg(feature = "gamepad")]
            pad_backend: pad_backend_or_none(),
            show_controls: false,
            show_settings: false,
            settings_tab: SettingsTab::Video,
            settings: app_settings,
            show_overlay_menu: false,
            menu_paused_game: false,
            quick_section: crate::quick_menu::Section::Resume,
            quick_focus_pending: false,
            state_slots_scanned: None,
            current_rom_path: None,
            show_game_settings: false,
            game_settings_target: None,
            pad_menu_requested: false,
            library_context_menu_open: false,
            hash_info: None,
            library_roots: library_roots.clone(),
            library: None,
            library_scan: None,
            current_game_hash: None,
            current_game_hashes: None,
            current_game_settings: crate::game_settings::GameSettings::default(),
            awaiting_key: None,
            bindings_status,
            pending_binding_save: false,
            app_bindings,
            awaiting_app_key: None,
            binding_conflict: None,
            active_slot: None,
            fast_forward_held: false,
            peek_key_held: false,
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
            diorama_pass: None,
            diorama_render: None,
            diorama_texture: None,
            diorama_wanted: false,
            diorama_ground_rgba: None,
            diorama_budget: rf_renderer::fog::BudgetGate::new(),
            mode7_seen: false,
            mode7_ground_wanted: false,
            mode7_plane_cache: None,
            ultrawide_refresh_countdown: 0,
            awaiting_canvas_snapshot: false,
            debug_panels: crate::debug_dock::DebugPanels::new(),
            event_subscription_active: false,
            snes_capture_active: false,
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
        };
        // Ticket W20-08: saved audio choices reach the audio path before
        // the first game starts.
        app.publish_audio_settings();
        app
    }

    /// Sample every bindable key once per repaint into
    /// [`Self::input_latch`], then publish the resulting `InputFrame` to
    /// the core thread (module doc). A no-op if no core is loaded — there
    /// is nothing to publish to.
    fn poll_input(&mut self, ctx: &egui::Context) {
        // Ticket W15-08: capture real keyboard/pointer activity BEFORE
        // anything below (the gamepad block's `push_nav_events`) adds
        // this frame's pad-synthesized `egui::Event::Key`s to the same
        // queue — `Self::track_input_device`'s doc explains why the
        // ordering is load-bearing.
        let (keyboard_active, mouse_active) = ctx.input(|i| {
            let keyboard = i
                .events
                .iter()
                .any(|e| matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)));
            let mouse = i.pointer.is_moving() || i.pointer.any_click() || i.pointer.any_pressed();
            (keyboard, mouse)
        });

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
                    // Ticket W15-06 criterion 2's "vice versa": a game
                    // binding can never claim a key the App namespace
                    // already uses, checked here before `rebind` ever
                    // touches the keymap.
                    if let Some(conflict) =
                        crate::app_bindings::game_key_conflicts_with_app(&self.app_bindings, key)
                    {
                        self.binding_conflict = Some(conflict);
                    } else {
                        self.bindings.keys.rebind(key, port, button);
                        self.pending_binding_save = true;
                        self.binding_conflict = None;
                    }
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
        //
        // `mut` even in a build without the `gamepad` feature: the block
        // that would set it true is compiled out entirely there, and the
        // variable still has to exist for the `InputDevice::resolve` call
        // below to compile in that configuration too.
        #[allow(unused_mut)]
        let mut pad_active_this_frame = false;
        #[cfg(feature = "gamepad")]
        if let Some(backend) = self.pad_backend.as_mut() {
            // Fully-qualified: `PadBackend` is not imported in this file,
            // and a `use` inside a cfg block would be dead in a default
            // build. Note this whole block is `#[cfg(feature =
            // "gamepad")]`, so a DEFAULT `cargo build` never type-checks
            // it — verify with `--features gamepad`.
            let events = rf_input::PadBackend::poll(backend);
            pad_active_this_frame = !events.is_empty();
            self.pad_router.apply(&events);
            let actions = self.ui_nav.on_events(&events);
            // Ticket W15-03: `Start` (`NavAction::Menu`) opens the library's
            // context menu on the focused row — chosen over a South
            // long-press because W15-08 (`plan.json`, "Controller-first
            // library") already commits to Start for exactly this, and
            // `Start` produces no `egui::Event` of its own
            // (`ui_nav.rs`'s doc), so it was otherwise dead on arrival in
            // this shell. `library_grid` consumes and clears the flag the
            // same frame.
            if actions.contains(&crate::ui_nav::NavAction::Menu) {
                self.pad_menu_requested = true;
            }
            // Ticket W20-03: Guide, or Select+Start, opens/closes the
            // in-game menu — checked after `apply` so "held" is current.
            if self.show_overlay_menu && actions.contains(&crate::ui_nav::NavAction::Back) {
                self.set_overlay_menu(false);
            } else if crate::ui_nav::menu_requested(&events, |b| self.pad_button_held(b)) {
                self.pad_menu_requested = false;
                self.set_overlay_menu(!self.show_overlay_menu);
            }
            self.push_nav_events(ctx, &actions);
        }

        // Ticket W15-08: fold this frame's three signals into the
        // most-recently-active device. Deliberately AFTER the gamepad
        // block (so `pad_active_this_frame` is known) but built from
        // `keyboard_active`/`mouse_active` captured at the very top of
        // this function (before `push_nav_events` added anything) — see
        // that capture's own comment.
        self.last_active_input = crate::ui_nav::InputDevice::resolve(
            self.last_active_input,
            keyboard_active,
            mouse_active,
            pad_active_this_frame,
        );

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

    /// Ticket W15-06 (`docs/design/UX_WAVE_15.md` §6): capture an App
    /// remap in progress, or otherwise fire whichever of the five App
    /// hotkeys the user is pressing/holding.
    ///
    /// A separate poll from [`Self::poll_input`], not folded into it:
    /// that loop drives `rf_input::Key::ALL` (the game namespace only),
    /// and an App key like `F5` has no `rf_input::Key` variant to iterate
    /// over in the first place (`crate::app_bindings`'s module doc).
    fn poll_app_hotkeys(&mut self, ctx: &egui::Context) {
        if let Some(action) = self.awaiting_app_key {
            let pressed = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key {
                        key, pressed: true, ..
                    } => Some(*key),
                    _ => None,
                })
            });
            if let Some(key) = pressed {
                if let Some(conflict) =
                    crate::app_bindings::key_conflicts_with_game(&self.bindings, key)
                {
                    self.binding_conflict = Some(conflict);
                } else {
                    self.app_bindings.bind_key(key, action);
                    self.binding_conflict = None;
                    self.save_app_bindings();
                }
                self.awaiting_app_key = None;
            }
            // A remap capture swallows the keystroke here too — the same
            // reason `poll_input`'s own capture does: pressing the new
            // Screenshot key must not also take a screenshot the instant
            // it is bound.
            return;
        }

        let save_key = self
            .app_bindings
            .key_for(crate::app_bindings::AppAction::SaveState);
        let load_key = self
            .app_bindings
            .key_for(crate::app_bindings::AppAction::LoadState);
        let shot_key = self
            .app_bindings
            .key_for(crate::app_bindings::AppAction::Screenshot);
        let ff_key = self
            .app_bindings
            .key_for(crate::app_bindings::AppAction::FastForward);
        let peek_key = self
            .app_bindings
            .key_for(crate::app_bindings::AppAction::HoldToPeek);
        let fullscreen_key = self
            .app_bindings
            .key_for(crate::app_bindings::AppAction::Fullscreen);
        // Ticket W20-04: F11 (remappable) or the macOS convention
        // Cmd+Ctrl+F (fixed, like the platform's own menu shortcut).
        let fullscreen_pressed = ctx.input(|i| {
            fullscreen_key.is_some_and(|k| i.key_pressed(k))
                || (i.modifiers.mac_cmd && i.modifiers.ctrl && i.key_pressed(egui::Key::F))
        }) || self
            .app_bindings
            .pad_for(crate::app_bindings::AppAction::Fullscreen)
            .is_some_and(|b| self.pad_button_held(b) && !self.fullscreen_pad_was_held);
        self.fullscreen_pad_was_held = self
            .app_bindings
            .pad_for(crate::app_bindings::AppAction::Fullscreen)
            .is_some_and(|b| self.pad_button_held(b));
        if fullscreen_pressed {
            self.toggle_fullscreen(ctx);
        }

        // One-shot actions use `key_pressed` (the edge) — `key_down`
        // would save/load/screenshot on every frame the key stays down.
        // The two "(hold)" actions use `key_down`, and their EFFECT is
        // applied only on a transition below, for the same reason.
        let (save_pressed, load_pressed, shot_pressed, ff_key_down, peek_key_down) =
            ctx.input(|i| {
                (
                    save_key.is_some_and(|k| i.key_pressed(k)),
                    load_key.is_some_and(|k| i.key_pressed(k)),
                    shot_key.is_some_and(|k| i.key_pressed(k)),
                    ff_key.is_some_and(|k| i.key_down(k)),
                    peek_key.is_some_and(|k| i.key_down(k)),
                )
            });

        let ff_pad = self
            .app_bindings
            .pad_for(crate::app_bindings::AppAction::FastForward);
        let peek_pad = self
            .app_bindings
            .pad_for(crate::app_bindings::AppAction::HoldToPeek);
        let ff_down = ff_key_down || ff_pad.is_some_and(|b| self.pad_button_held(b));
        let peek_down = peek_key_down || peek_pad.is_some_and(|b| self.pad_button_held(b));

        if save_pressed {
            let slot = self
                .active_slot
                .unwrap_or(crate::state_slots::SlotId::Numbered(1));
            self.save_to_slot(slot, ctx);
        }
        if load_pressed {
            let slot = self
                .active_slot
                .unwrap_or(crate::state_slots::SlotId::Numbered(1));
            self.load_from_slot(slot);
        }
        if shot_pressed {
            // Deferred to the next frame, same as the Enhance workspace's
            // own Screenshot button (`Self::enhance_window`'s comment):
            // with compare off, no buffers are kept, so the first frame
            // that has them is the next one.
            self.screenshot_pending = true;
            self.status = "Screenshot: capturing next frame\u{2026}".to_string();
        }

        if ff_down != self.fast_forward_held {
            self.fast_forward_held = ff_down;
            self.send_command(CoreCommand::SetPacingEnabled(!ff_down));
        }
        // Ticket W20-12: while held, ONE keyed OSD card (refreshed each
        // frame, so it stays up and fades shortly after release) — it
        // replaces the status bar's "FF" chip.
        if ff_down && self.core.is_some() {
            self.osd.push_card(
                crate::toast::ToastKind::Info,
                format!("{} Fast-forward", egui_phosphor::regular::FAST_FORWARD),
                None,
                Some("fast-forward"),
                ctx,
            );
        }
        self.peek_key_held = peek_down;
    }

    /// Whether `button` is currently held on any connected pad.
    ///
    /// `rf_input::PadRouter` (crates/rf-input, outside this ticket's write
    /// scope) keeps its per-pad `held` set private and exposes it only
    /// through [`rf_input::PadRouter::buttons`], which needs a
    /// [`rf_input::PadMap`] to translate into NES-button bits. A
    /// throwaway one-entry map is the public-API way to ask the question
    /// this needs without reaching into that private state.
    fn pad_button_held(&self, button: rf_input::PadButton) -> bool {
        let mut probe = rf_input::PadMap::new();
        probe.bind(button, rf_input::NesButton::A);
        let bit = 1u16 << rf_input::NesButton::A.bit();
        (0..rf_core_api::MAX_INPUT_PORTS)
            .any(|port| self.pad_router.buttons(port, &probe) & bit != 0)
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

    /// Load the open game's annotations into the debug panel (ticket
    /// W13-02f).
    ///
    /// A ROM this build could not identify gets **no identity**, which the
    /// panel renders as "no game open": annotations keyed by a hash we
    /// could not compute would be annotations that silently attach to the
    /// wrong game later — the same reasoning `current_game_hash`'s own doc
    /// gives for per-game settings.
    fn load_annotations_for_current_game(&mut self, path: &std::path::Path) {
        let title = path.file_stem().map_or_else(
            || "Untitled".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let identity =
            self.current_game_hash
                .clone()
                .map(|hash| crate::debug_dock::AnnotationIdentity {
                    title,
                    normalized_sha256: hash,
                    console: if self.console_label == "SNES" {
                        rf_debugger::profile_export::Console::Snes
                    } else {
                        rf_debugger::profile_export::Console::Nes
                    },
                });
        let loaded = match (&self.config_root, &self.current_game_hash) {
            (Some(root), Some(hash)) => crate::annotation_store::load(root, hash),
            _ => crate::annotation_store::Loaded::default(),
        };
        let problem = loaded.problem.clone().or_else(|| {
            // Rows the file held that the store's invariants refused.
            // Reported, because an author who sees 12 of their 14 labels
            // needs to be told why, not left to wonder.
            (!loaded.rejected.is_empty()).then(|| {
                format!(
                    "{} annotation(s) in the file were refused",
                    loaded.rejected.len()
                )
            })
        });
        self.debug_panels
            .annotations
            .adopt(loaded.store, identity, problem);
    }

    /// Act on whatever the annotations panel asked for this frame (ticket
    /// W13-02f).
    ///
    /// The panel has no filesystem and no profile editor; this method has
    /// both. Called once per repaint, like `pump_trace`.
    fn pump_annotation_request(&mut self) {
        let Some(request) = self.debug_panels.annotations.request.take() else {
            return;
        };
        // Ticket W13-02e: installing watches is the one request that does
        // not need a game identity — disarming is exactly what a session
        // with no ROM should do, and `adopt` asks for it on every open.
        if request == crate::debug_dock::AnnotationRequest::InstallWatches {
            let watches = self.debug_panels.annotations.watches.clone();
            let refused = watches.len().saturating_sub(rf_core_api::MAX_WATCHES);
            self.send_command(crate::core_thread::CoreCommand::SetWatches(watches));
            self.debug_panels.annotations.problem = (refused > 0).then(|| {
                format!(
                    "{refused} watchpoint(s) refused: the core holds {}",
                    rf_core_api::MAX_WATCHES
                )
            });
            return;
        }
        let Some(identity) = self.debug_panels.annotations.identity.clone() else {
            return;
        };
        match request {
            crate::debug_dock::AnnotationRequest::InstallWatches => unreachable!("handled above"),
            crate::debug_dock::AnnotationRequest::Save => {
                let Some(root) = self.config_root.clone() else {
                    self.debug_panels.annotations.problem =
                        Some("no config directory — annotations cannot be saved".to_string());
                    return;
                };
                match crate::annotation_store::save(
                    &root,
                    &identity.normalized_sha256,
                    &self.debug_panels.annotations.store,
                ) {
                    Ok(path) => {
                        self.debug_panels.annotations.dirty = false;
                        self.debug_panels.annotations.problem = None;
                        self.debug_panels.annotations.status =
                            Some(format!("saved to {}", path.display()));
                    }
                    Err(e) => self.debug_panels.annotations.problem = Some(e),
                }
            }
            crate::debug_dock::AnnotationRequest::ExportSkeleton => {
                let meta = rf_debugger::profile_export::ExportMeta {
                    title: identity.title.clone(),
                    console: identity.console,
                    region: "ntsc".to_string(),
                    authors: Vec::new(),
                };
                match rf_debugger::profile_export::export_skeleton(
                    self.debug_panels.annotations.store.entries(),
                    &meta,
                ) {
                    Ok(text) => {
                        // Straight into the profile editor rather than to
                        // a file: W9-02 already owns "edit a profile and
                        // save only what the real loader accepts", and
                        // GAME_PROFILES.md §3 step 2 hands the skeleton to
                        // step 3 rather than to a directory. A second save
                        // path would be a second thing to keep correct.
                        let mut draft = crate::profile_editor::Draft::from_text(&text);
                        // `export_skeleton` emits no `[[identity]]` — it
                        // has the annotations, not the ROM. Without this
                        // the skeleton could never match the game it came
                        // from.
                        draft.append_identity(&identity.normalized_sha256, None);
                        self.editor_draft = Some(draft);
                        self.debug_panels.annotations.problem = None;
                        self.debug_panels.annotations.status = Some(
                            "exported — the profile editor now holds the skeleton".to_string(),
                        );
                    }
                    Err(e) => self.debug_panels.annotations.problem = Some(e.to_string()),
                }
            }
        }
    }

    /// Apply a byte the memory editor committed (ticket W13-02d).
    ///
    /// Refuses while the core is running rather than queueing: the panel
    /// already hides the editor when not paused, and a poke that arrived
    /// after a resume would land at an unrepeatable cycle. Two guards for
    /// one rule, because this one writes to a live machine.
    fn pump_memory_poke(&mut self) {
        let paused = !self.running;
        self.debug_panels.data.memory.paused = paused;
        let Some((addr, value)) = self.debug_panels.data.memory.poke.take() else {
            return;
        };
        if !paused {
            self.debug_panels.data.memory.status =
                Some("not written: the core is running".to_string());
            return;
        }
        let Ok(addr) = u16::try_from(addr) else {
            return;
        };
        self.send_command(core_thread::CoreCommand::PokeBus { addr, value });
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
    /// The annotations panel's state (ticket W13-02f).
    ///
    /// Public for the same reason `set_game_hash_for_test` is: the
    /// end-to-end test that proves the §4 workflow reaches a user lives in
    /// another crate and cannot see a `#[cfg(test)]` method. The panel
    /// itself is what the UI mutates, so this is the same door, not a
    /// second one.
    pub fn debug_annotations_mut(&mut self) -> &mut crate::debug_dock::AnnotationPanelData {
        &mut self.debug_panels.annotations
    }

    /// Open a debug tab, exactly as the "Add panel" picker does (ticket
    /// W13-02f).
    pub fn debug_open_tab(&mut self, tab: rf_debugger::layout::DebugTab) {
        self.debug_panels.open_tab(tab);
    }

    /// Which debug tabs are currently docked (ticket W13-02f's picker).
    #[must_use]
    pub fn debug_tabs_for_test(&self) -> Vec<rf_debugger::layout::DebugTab> {
        self.debug_panels
            .dock_state
            .iter_all_tabs()
            .map(|(_, tab)| *tab)
            .collect()
    }

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
                // Ticket W10-04: the authoring preview is a decoded level
                // rendered as text, whose length is the level's size —
                // unbounded by anything this window controls.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        match self.author_watch.as_ref() {
                            Some(w) => {
                                let _ = ui.selectable_label(
                                    false,
                                    format!("watching {}", w.path().display()),
                                );
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
                                egui::RichText::new(w)
                                    .color(egui::Color32::from_rgb(0xE0, 0x80, 0x30)),
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
                                        egui::RichText::new(crate::authoring::preview_text(
                                            level, 48, 14,
                                        ))
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
        self.refresh_state_slots();
        self.show_states = true;
    }

    /// Re-read this game's slot directory into `state_slots`.
    fn refresh_state_slots(&mut self) {
        self.state_slots_scanned = Some(std::time::Instant::now());
        self.state_slots = match self.states_dir() {
            Some(dir) => crate::state_slots::scan(&dir),
            // No config directory (or no ROM open): show the empty grid
            // rather than nothing, so the modal still explains itself.
            None => crate::state_slots::SlotId::all()
                .into_iter()
                .map(|id| crate::state_slots::SlotInfo { id, saved: None })
                .collect(),
        };
    }

    /// Ticket W20-10: the Quick Menu's Save/Load sections re-read the slot
    /// directory at most twice a second — the core thread writes a save
    /// asynchronously (`save_to_slot`'s own doc), so a once-only scan
    /// would never show the slot just saved.
    fn refresh_state_slots_if_stale(&mut self) {
        let stale = self
            .state_slots_scanned
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(500));
        if stale {
            self.refresh_state_slots();
        }
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
    /// Ticket W20-06/W20-10: the save-slot cards, shared by the States
    /// window (both buttons) and the Quick Menu's Save and Load sections
    /// (one each). Returns the slot clicked and whether it was a save. A
    /// Save on an occupied slot goes through `pending_overwrite` instead
    /// (W15-04), so it returns nothing.
    fn slot_card_grid(
        &mut self,
        ui: &mut egui::Ui,
        allow_load: bool,
        allow_save: bool,
        columns: Option<usize>,
    ) -> Option<(crate::state_slots::SlotId, bool)> {
        let mut picked: Option<(crate::state_slots::SlotId, bool)> = None;
        // Ticket W20-06: cards with the slot's own
        // screenshot, drawn — until W20-06 this printed the
        // word "thumbnail" beside a selectable label.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let tokens = crate::theme::Tokens::from_accessibility(&self.settings.accessibility);
        let slots = self.state_slots.clone();
        // `columns` given: explicit rows (the Quick Menu, an Area with no
        // width bound for `horizontal_wrapped` to wrap against — it grew
        // the panel to ~2000 px). `None`: wrap (the resizable States window).
        let rows: Vec<Vec<crate::state_slots::SlotInfo>> = match columns {
            Some(n) => slots.chunks(n.max(1)).map(<[_]>::to_vec).collect(),
            None => vec![slots.clone()],
        };
        for row in &rows {
            let mut body = |ui: &mut egui::Ui| {
                ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                for info in row {
                    let thumb = info
                        .saved
                        .as_ref()
                        .and_then(|s| s.thumbnail.as_deref())
                        .and_then(|p| self.slot_textures.get(ui.ctx(), p));
                    egui::Frame::new()
                        .fill(tokens.surface)
                        .corner_radius(tokens.radius_md)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            ui.set_width(SLOT_CARD_WIDTH);
                            ui.vertical(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(SLOT_CARD_WIDTH, SLOT_THUMB_HEIGHT),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(rect, tokens.radius_sm, tokens.bg);
                                if let Some(tex) = &thumb {
                                    let [w, h] = tex.size();
                                    #[allow(clippy::cast_precision_loss)]
                                    let fit = crate::play_view::play_rect(
                                        rect,
                                        crate::play_view::DisplayGrid::exact(w as f32, h as f32),
                                        1.0,
                                        crate::settings::ScaleMode::Fit,
                                    );
                                    ui.put(
                                        fit,
                                        egui::Image::from_texture(tex)
                                            .fit_to_exact_size(fit.size())
                                            .alt_text(format!("{} screenshot", info.id.label())),
                                    );
                                }
                                ui.label(egui::RichText::new(info.id.label()).strong());
                                match &info.saved {
                                    Some(saved) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} \u{b7} {}",
                                                crate::slot_cards::relative_age(
                                                    saved.timestamp,
                                                    now,
                                                    || Self::format_timestamp(saved.timestamp),
                                                ),
                                                saved.mode.label()
                                            ))
                                            .small()
                                            .color(tokens.muted),
                                        );
                                        if saved.contains_mods {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{} contains mods",
                                                    crate::icons::WARNING
                                                ))
                                                .small()
                                                .color(tokens.warn),
                                            );
                                        }
                                    }
                                    None => {
                                        ui.label(
                                            egui::RichText::new("Empty")
                                                .small()
                                                .color(tokens.muted),
                                        );
                                    }
                                }
                                ui.horizontal(|ui| {
                                    if allow_load
                                        && info.saved.is_some()
                                        && ui.button(format!("Load {}", info.id.label())).clicked()
                                    {
                                        picked = Some((info.id, false));
                                    }
                                    if allow_save
                                        && ui.button(format!("Save {}", info.id.label())).clicked()
                                    {
                                        if info.saved.is_some() {
                                            // Ticket W15-04: an
                                            // occupied slot asks
                                            // first.
                                            self.pending_overwrite = Some(info.id);
                                            self.active_slot = Some(info.id);
                                        } else {
                                            picked = Some((info.id, true));
                                        }
                                    }
                                });
                            });
                        });
                }
            };
            if columns.is_some() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                    body(ui);
                });
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                    body(ui);
                });
            }
        }
        picked
    }

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
                // Ticket W10-04: ten slots plus auto-slots, each with a
                // screenshot, a timestamp and a mode — taller than a
                // window bounded to a 720px viewport.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.states_dir().is_none() {
                            ui.label("No ROM open — save states are per game.");
                        }
                        if let Some(chosen) = self.slot_card_grid(ui, true, true, None) {
                            action = Some(chosen);
                        }
                        if !self.state_warnings.is_empty() {
                            ui.separator();
                            for line in &self.state_warnings {
                                ui.colored_label(egui::Color32::from_rgb(0xE0, 0x80, 0x30), line);
                            }
                        }
                    });
            });
        if let Some((slot, is_save)) = action {
            // Ticket W15-06: whichever slot the modal was just used on
            // becomes the App hotkeys' "active slot" (§6: save/load act
            // on the active slot, defaulting to Slot 1 if none was ever
            // chosen).
            self.active_slot = Some(slot);
            if is_save {
                self.save_to_slot(slot, ctx);
            } else {
                self.load_from_slot(slot);
            }
        }
        self.show_states = open;
    }

    /// Ticket W15-04: the overwrite-confirmation `egui::Modal` for a Save
    /// click on a slot that already holds a state. `states_modal`
    /// defers into `self.pending_overwrite` rather than saving directly
    /// so a mis-click cannot silently discard a state — the whole point
    /// of the acceptance criterion.
    fn overwrite_confirm_modal(&mut self, ctx: &egui::Context) {
        // Ticket W15-07: the modal's own open/close fade. `open_now` gates
        // the REAL state (`pending_overwrite`, what `pending_overwrite_for_test`
        // reads) exactly as before this ticket — cleared the same frame a
        // click closes it, never delayed by the fade. `alpha` only decides
        // whether this function still has anything to PAINT this frame;
        // `pending_overwrite_fade_cache` is what it paints from once
        // `pending_overwrite` itself has already gone back to `None`.
        let open_now = self.pending_overwrite.is_some();
        if let Some(slot) = self.pending_overwrite {
            self.pending_overwrite_fade_cache = Some(slot);
        }
        let alpha =
            crate::theme::modal_fade_alpha(ctx, egui::Id::new("rf_overwrite_modal_fade"), open_now);
        if alpha <= 0.0 {
            return;
        }
        let Some(slot) = self.pending_overwrite.or(self.pending_overwrite_fade_cache) else {
            return;
        };
        let tokens = crate::theme::Tokens::from_accessibility(&self.settings.accessibility);
        let mut overwrite = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("rf_overwrite_modal"))
            .backdrop_color(tokens.modal_backdrop().gamma_multiply(alpha))
            .frame(
                egui::Frame::popup(&ctx.global_style())
                    .fill(tokens.surface.gamma_multiply(alpha))
                    .stroke(egui::Stroke::new(1.0, tokens.line)),
            )
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.heading("Overwrite save?");
                ui.label(format!(
                    "{} already holds a save. Saving now replaces it — this cannot be undone.",
                    slot.label()
                ));
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Overwrite").clicked() {
                        overwrite = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if !open_now {
            // Fading out on cached data — the real decision already
            // happened the frame `pending_overwrite` cleared; a click
            // landing on the disappearing dialog now is not acted on.
            return;
        }
        if overwrite {
            self.save_to_slot(slot, ctx);
            self.pending_overwrite = None;
        } else if cancel || modal.should_close() {
            self.pending_overwrite = None;
        }
    }

    /// Ticket W15-04: the minimal quit-with-unsaved-state flow the ticket
    /// asked for — "quitting with a running core whose last save-state is
    /// older than the current frame asks once". Both Quit buttons (File
    /// menu and the overlay menu) route through this instead of sending
    /// `ViewportCommand::Close` directly.
    fn request_quit(&mut self, ctx: &egui::Context) {
        if self.has_unsaved_progress() {
            self.pending_quit = true;
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// A core is running and has advanced past the frame its last save
    /// (if any) was taken at. No core open, or no frame reported yet,
    /// means nothing to lose — the empty-library home screen and a ROM
    /// that has not rendered its first frame both quit without asking.
    fn has_unsaved_progress(&self) -> bool {
        let Some((frame, _)) = self.position else {
            return false;
        };
        self.core.is_some() && frame > self.last_save_frame.unwrap_or(0)
    }

    /// The quit-confirmation `egui::Modal`, shown once `request_quit`
    /// sets `pending_quit`. Cancel and an outside click both just close
    /// the modal — "asks once" (the acceptance wording) means one modal
    /// per quit attempt, not zero on a second try.
    fn quit_confirm_modal(&mut self, ctx: &egui::Context) {
        // Ticket W15-07: no payload to cache (unlike the overwrite modal) —
        // "quit with unsaved progress?" needs nothing from the moment it
        // opened, so the fade-out can keep rendering its own fixed text
        // straight off `alpha` alone.
        let open_now = self.pending_quit;
        let alpha =
            crate::theme::modal_fade_alpha(ctx, egui::Id::new("rf_quit_modal_fade"), open_now);
        if alpha <= 0.0 {
            return;
        }
        let tokens = crate::theme::Tokens::from_accessibility(&self.settings.accessibility);
        let mut quit = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("rf_quit_modal"))
            .backdrop_color(tokens.modal_backdrop().gamma_multiply(alpha))
            .frame(
                egui::Frame::popup(&ctx.global_style())
                    .fill(tokens.surface.gamma_multiply(alpha))
                    .stroke(egui::Stroke::new(1.0, tokens.line)),
            )
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.heading("Quit with unsaved progress?");
                ui.label(
                    "The running game has advanced since its last save state. Quitting now \
                     loses that progress.",
                );
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Quit").clicked() {
                        quit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if !open_now {
            return;
        }
        if quit {
            self.pending_quit = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if cancel || modal.should_close() {
            self.pending_quit = false;
        }
    }

    fn save_to_slot(&mut self, slot: crate::state_slots::SlotId, ctx: &egui::Context) {
        let Some(dir) = self.states_dir() else {
            self.status = "No ROM open".to_string();
            return;
        };
        self.send_command(CoreCommand::SaveStateToSlot {
            dir,
            stem: slot.stem(),
        });
        // The core thread writes the file; re-scan on the next open so
        // the listing reflects it rather than guessing it succeeded.
        self.state_warnings.clear();
        // Ticket W15-04: the toast fires here, on the request, same as
        // `self.status` above — there is no confirmation message back
        // from the core thread to hang it off instead (the file write is
        // fire-and-forget; the next modal open re-scans and shows the
        // truth either way).
        // Ticket W20-12: an OSD card over the game, with the frame that
        // was saved, instead of a toast in the far corner and a status
        // string ("Saving Slot 1…") that never changed afterwards.
        let thumb = self.frame_thumb(ctx);
        self.osd.push_card(
            crate::toast::ToastKind::Success,
            format!("Saved to {}", slot.label()),
            thumb,
            None,
            ctx,
        );
        // Ticket W15-04's quit-confirmation reads this: a save just
        // requested for the CURRENT frame means nothing has changed
        // since, so quitting the instant after a Save must not ask.
        self.last_save_frame = self.position.map(|(f, _)| f);
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
                // Ticket W20-12: the slot's own picture on the card.
                let thumb = crate::state_slots::scan(&dir)
                    .into_iter()
                    .find(|i| i.id == slot)
                    .and_then(|i| i.saved)
                    .and_then(|s| s.thumbnail);
                let ctx = self.ctx.clone();
                let tex = thumb.and_then(|path| self.slot_textures.get(&ctx, &path));
                let text = if self.state_warnings.is_empty() {
                    format!("Loaded {}", slot.label())
                } else {
                    format!(
                        "Loaded {} ({} warning(s))",
                        slot.label(),
                        self.state_warnings.len()
                    )
                };
                self.osd
                    .push_card(crate::toast::ToastKind::Success, text, tex, None, &ctx);
            }
            Err(e) => {
                self.status = format!("Load failed: {e}");
                let ctx = self.ctx.clone();
                self.osd.push(
                    crate::toast::ToastKind::Error,
                    format!("Load failed: {e}"),
                    &ctx,
                );
            }
        }
    }

    /// Ticket W20-12: a snapshot texture of the frame on screen now, for
    /// an OSD card — a COPY, so the card keeps showing the moment it
    /// reports while the game runs on.
    fn frame_thumb(&self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        let rgba = self.last_frame_rgba.as_ref()?;
        let (w, h) = self.last_frame_size?;
        if rgba.len() != w * h * 4 {
            return None;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([w, h], rgba);
        Some(ctx.load_texture("osd-frame-thumb", image, egui::TextureOptions::NEAREST))
    }

    fn open_rom(&mut self) {
        let Some(path) = rom_open::pick_rom_file() else {
            return; // user cancelled the dialog
        };
        self.launch_rom(&path);
    }

    /// Ticket W20-07: open a ROM **and play it** — what the library's
    /// Play/double-click/Enter/pad-A and File › Open ROM… do.
    ///
    /// [`Self::open_rom_path`] opens paused: the debugger's convention,
    /// so the first frame can be inspected before anything runs, and what
    /// every test that drives the core frame-by-frame relies on. Until
    /// W20-07 the player got that convention too — pressing Play showed a
    /// paused game and the only way to start it was the status bar's Run
    /// button. A player who launches a game wants to play it; the debugger
    /// convention survives when the Debug viewers are open.
    pub fn launch_rom(&mut self, path: &std::path::Path) {
        self.open_rom_path(path);
        if self.core.is_some() && !self.debug_panels.visible {
            self.send_command(CoreCommand::Resume);
            self.running = true;
        }
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
        // Ticket W20-09: an HD pack belongs to the game it was loaded
        // for. Kept across a ROM change it went quietly dead — the new
        // core starts with tile capture off and nothing re-armed it — while
        // the Enhance panel still showed its summary.
        self.hd_pack = None;
        self.hd_summary = None;
        self.hd_unsatisfied.clear();
        self.hd_report = None;
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
        self.current_game_hashes = match rf_cart::Cartridge::load(&bytes) {
            Ok(
                rf_cart::Cartridge::Nes { identity, .. }
                | rf_cart::Cartridge::Snes { identity, .. },
            ) => Some(identity.normalized),
            Err(_) => None,
        };
        self.current_game_hash = self.current_game_hashes.as_ref().map(|h| h.sha256.clone());
        self.console_label = match rf_cart::Cartridge::load(&bytes) {
            Ok(rf_cart::Cartridge::Snes { .. }) => "SNES",
            _ => "NES",
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
        self.matched_profile = self.current_game_hashes.as_ref().and_then(|hashes| {
            crate::level_view::find_matching_profile(&Self::profiles_root(), hashes)
                .map(|(_, path)| path)
        });
        // Ticket W16-06 bug fix: `self.profile_matched` (the `bool` this
        // struct's own doc comment calls "false until a profile loader is
        // wired") was never actually assigned anywhere once the loader
        // above WAS wired (W5-06/W11-02) -- `self.matched_profile`
        // (`Option<PathBuf>`) became the real signal and this bool was
        // simply left behind, always false. That silently broke every
        // profile-gated feature row's `Availability` (`crate::enhance_ui::
        // feature_rows`/`badge_text`/`badge_breakdown` all take this bool)
        // regardless of whether a profile genuinely matched — found while
        // wiring the Diorama row, which inherits the same gating. Kept in
        // sync with the real signal here, the one place `matched_profile`
        // itself is (re)computed.
        self.profile_matched = self.matched_profile.is_some();
        // Ticket W11-02: decode the level now, once. `None` when no
        // profile matched or it declares no decodable level — both
        // ordinary, neither an error (`LevelSession::open`'s own doc).
        self.level_session = self.current_game_hashes.as_ref().and_then(|hashes| {
            crate::level_view::LevelSession::open(&Self::profiles_root(), &bytes, hashes)
        });
        self.level_texture = None;
        self.level_camera = None;

        self.current_game_settings = match (&self.config_root, &self.current_game_hash) {
            (Some(root), Some(hash)) => crate::game_settings::load(root, hash),
            _ => crate::game_settings::GameSettings::default(),
        };

        // Ticket W16-10: a profile that declares `[atmosphere]` pins the
        // fog plane and the heuristic's ladder rung before the per-game
        // settings are consulted, the one point where the loaded profile
        // and the per-game trust ladder are both in scope. Absent table,
        // absent pin: `apply_profile_pin` is a no-op then.
        if let Some(session) = &self.level_session {
            rf_enhance::atmosphere::apply_profile_pin(
                &session.profile,
                &mut self.current_game_settings.trust,
            );
        }

        // Ticket W15-02, acceptance 1: every launch records a play. This
        // is deliberately unconditional on the ROM having been recognized
        // by `rf_cart` above — `current_game_hash` is `None` for an
        // unrecognized cartridge, and `record_launch`/`save` both no-op
        // in that case (`save_current_game_settings`'s own doc states the
        // same rule) rather than keying play history on a hash we do not
        // have. Only the ONE launched entry's cache is refreshed here —
        // `library_meta`'s doc explains why a full re-scan is not needed.
        if let (Some(root), Some(hash)) = (&self.config_root, &self.current_game_hash) {
            self.current_game_settings
                .record_launch(std::time::SystemTime::now());
            if let Err(e) = crate::game_settings::save(root, hash, &self.current_game_settings) {
                self.status = format!("Could not save game settings: {e}");
            }
            self.library_meta.insert(
                hash.clone(),
                crate::library::RecencyMeta {
                    last_played_epoch_secs: self.current_game_settings.last_played_epoch_secs,
                    play_count: self.current_game_settings.play_count,
                    favourite: self.current_game_settings.favourite,
                },
            );
        }

        // Ticket W13-02f: this game's annotations, keyed by the same
        // normalized hash the settings above use. Loaded here rather than
        // when the panel is first drawn, so the labels are already there
        // the moment someone opens the tab.
        self.load_annotations_for_current_game(path);

        // Ticket W14-20 defect 2: wake the UI thread directly after every
        // frame the new core sends, rather than relying solely on the
        // winit redraw `pump_core_events`'s `ctx.request_repaint()` asks
        // for — see `core_thread::spawn_with_waker`'s doc for why the
        // request alone was not enough on 2026-09-17.
        let ctx_for_waker = self.ctx.clone();
        let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx_for_waker.request_repaint());
        match core_thread::spawn_with_waker(bytes, Some(waker)) {
            Ok(handle) => {
                self.core = Some(handle);
                self.current_rom_path = Some(path.to_path_buf());
                // A new ROM is a new debug session too — the previous
                // ROM's OAM/events would otherwise linger onscreen against
                // a completely different game (same reasoning the
                // Ultrawide-camera reset below already uses).
                self.debug_panels.data.chr_rom = chr_rom;
                self.debug_panels.data.oam = [0u8; 256];
                self.debug_panels.data.previous_oam = [0u8; 256];
                self.debug_panels.data.events = Vec::new();
                // Ticket W13-02b: and the previous game's SNES memories,
                // for the same reason — opening a NES ROM after a SNES one
                // must not leave the SNES column drawn against it.
                self.debug_panels.data.snes = None;
                self.debug_panels.data.cpu_regs = rf_core_api::CpuRegs::None;
                self.snes_capture_active = false;
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
                // Ticket W11-01, same shape and the same reason: a fresh
                // core starts with de-flicker OFF (law 6), so a game the
                // user had turned it on for must have it re-applied here
                // or the setting would appear checked against a core that
                // had just reset it.
                if self.current_game_settings.deflicker {
                    self.send_command(CoreCommand::SetDeflicker(true));
                }
                if self.current_game_settings.full_level_view {
                    self.set_level_probe(true);
                }
                // Ticket W20-09 (ENHANCEMENT_AUDIT.md §3 row D): decoded
                // widescreen was the one saved toggle NOT re-applied here,
                // so a game saved with it on reopened showing the box
                // checked over a 4:3 picture. Only when the row can act
                // (SNES, Game-Aware, profile) — the same gate the badge
                // counts by.
                if self.current_game_settings.widescreen_decoded
                    && crate::enhance_ui::feature_rows(
                        &self.current_game_settings,
                        &self.game_facts(),
                    )
                    .iter()
                    .any(|r| r.id == "widescreen_decoded" && r.effective())
                {
                    self.set_widescreen(true);
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
                // Ticket W16-02, same stale-state-across-a-reload hazard:
                // a fresh core thread starts with `SetStudioCapture` off.
                // If the Upscale Studio was already open (perhaps before
                // any ROM was loaded at all — `set_upscale_studio_open`
                // only ever reaches a core through `send_command`, which
                // silently no-ops with none running), re-assert it here or
                // the window would sit at "0 tile(s) captured" for the
                // rest of the session with no core ever told to record.
                if self.show_upscale_studio {
                    self.send_command(CoreCommand::SetStudioCapture(true));
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
                // Ticket W16-13: same "a new ROM is a new session" reset,
                // for Diorama — the previous ROM's ground texture/render
                // must not linger against a completely different level.
                // `diorama_wanted` resets to `false` so the very next
                // `sync_diorama_subscription` call (every repaint) treats
                // this as a fresh transition and re-arms the probe/layer
                // extraction if the new ROM's settings/profile want it —
                // it does not need to happen here directly.
                self.diorama_render = None;
                self.diorama_texture = None;
                self.diorama_ground_rgba = None;
                self.diorama_wanted = false;
                // Ticket W16-14: same reset, for Mode 7 -- a new ROM has
                // shown nothing yet, so the "mode7_ground" row must not
                // still read Available from the PREVIOUS game's BG mode 7.
                self.mode7_seen = false;
                self.mode7_ground_wanted = false;
                self.mode7_plane_cache = None;
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
                CoreEvent::Frame(msg) => {
                    // Ticket W14-20 defect 1: every `CoreEvent::Frame`
                    // drained here must release one slot of
                    // `core_thread::MAX_PENDING_FRAMES` back to the
                    // producer — the core thread only checks this counter
                    // before a send, so a drain that forgot to decrement
                    // it would leave the cap permanently exhausted after
                    // the first `MAX_PENDING_FRAMES` frames of the whole
                    // session, silently dropping every frame after that
                    // even once the UI is repainting normally again.
                    core.pending_frames
                        .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                    latest_frame = Some(msg);
                }
                // Ticket W4-03e: keep only the latest, same "older ones are
                // stale by the time we'd paint them" reasoning this
                // function's own doc already gives for `latest_frame`.
                CoreEvent::CanvasSnapshot(canvas) => latest_canvas = Some(canvas),
                // Ticket W11-03: keep the policy's reasons so the Enhance
                // panel can say WHY a layer stayed narrow. Stored rather
                // than logged: a refusal the user cannot see is the same
                // as a swallowed one.
                CoreEvent::WidescreenDecisions(reasons) => {
                    self.widescreen_decisions = reasons;
                }
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
                self.write_screenshots(ctx);
            }
            self.audio_fill = msg.audio_fill;
            // Ticket W11-05: apply the HD pack, if one is loaded and the
            // core reported this frame's tiles. Done HERE rather than on
            // the core thread because the pack and its decoded tilesets
            // live on this side — shipping them across the channel every
            // frame would be the expensive half of the pair.
            let (rgba, size) = match (&self.hd_pack, &msg.hd) {
                (Some((pack, images)), Some(hd)) => {
                    let (out, report) = rf_enhance::hd_render::composite(
                        pack,
                        images,
                        &hd.placements,
                        &msg.rgba,
                        &hd.layers,
                        msg.width,
                        msg.height,
                    );
                    self.hd_report = Some(report);
                    let s = pack.scale.max(1) as usize;
                    (out, (msg.width * s, msg.height * s))
                }
                _ => (msg.rgba.clone(), (msg.width, msg.height)),
            };
            // Ticket W16-02: feed the Upscale Studio's accumulator
            // whenever it asked for tiles this frame (`msg.hd.studio_tiles`
            // is empty otherwise — see `CoreCommand::SetStudioCapture`).
            if self.show_upscale_studio {
                if let Some(hd) = &msg.hd {
                    self.upscale_studio_session.observe(&hd.studio_tiles);
                }
            }
            // Ticket W15-05: the first-frame capture hook (acceptance 1) —
            // the first `CoreEvent::Frame` after a successful boot for a
            // hash with no thumbnail yet. Reads `rgba` before it moves
            // into `self.last_frame_rgba` below.
            self.maybe_capture_first_frame(&rgba, size.0, size.1);
            self.last_frame_rgba = Some(rgba);
            self.last_frame_size = Some(size);
            self.core_frame_size = Some((msg.width, msg.height));
            // Ticket W11-02: the probe's bytes become a live camera. The
            // `read` closure is a lookup into what the CORE peeked, not a
            // read of anything on this thread — the UI never touches
            // emulator memory (ARCHITECTURE §3).
            // Ticket W11-04: publish this frame's window, run the
            // script, collect what it wants drawn. Order matters — a
            // script that reads memory BEFORE the window is published
            // would see the previous frame and draw one frame behind.
            if let (Some(bytes), Some(host)) = (&msg.script_window, self.script_host.as_mut()) {
                host.bridge().publish(
                    rf_plugin_sdk::sandbox::MemoryWindow {
                        base: SCRIPT_WINDOW_BASE,
                        bytes: bytes.clone(),
                    },
                    std::collections::BTreeMap::new(),
                );
                host.on_frame(msg.frame_count);
                self.script_overlay = host.bridge().take_overlay();
                // Ticket W11-04: the Lua console tab shows the LIVE host.
                // `crate::script_panel` had been an orphan since W4-04 —
                // its capability, status and ledger lines were written,
                // tested, and called from nowhere. These are those calls.
                self.debug_panels.data.script =
                    Some(Box::new(crate::debug_dock::ScriptPanelData {
                        status: crate::script_panel::status_line(host),
                        capabilities: crate::script_panel::capability_lines(host),
                        ledger: crate::script_panel::ledger_lines(host),
                        console: host.log.lines(),
                    }));
            }
            if let (Some(probe), Some(session)) = (&msg.level_probe, &self.level_session) {
                let addrs = Self::probe_addrs(session);
                let values = probe.values.clone();
                let read = move |addr: u32| -> u8 {
                    addrs
                        .iter()
                        .position(|a| *a == addr)
                        .and_then(|i| values.get(i).copied())
                        .unwrap_or(0)
                };
                let cam = rf_enhance::level_view::live_camera(&session.profile, &read);
                self.level_camera = Some((cam.x, cam.y));
            }
            // Ticket W16-14: latch "this session has shown BG mode 7 at
            // least once" — sticky rather than re-derived every frame
            // (`Self::mode7_seen`'s own doc: a menu/pause screen must not
            // flicker the feature row unavailable).
            if msg.mode7.is_some() {
                self.mode7_seen = true;
            }
            // Ticket W16-13 acceptance 1: refresh the Diorama compositor's
            // output for THIS frame — placed BEFORE the bg/sprite-layer
            // early return a little further down, which tests exactly
            // `msg.sprite_rgba` (empty means layer extraction is off);
            // Diorama needs that same buffer even when the Layers debug
            // window itself is closed (`Self::sync_diorama_subscription`
            // arms extraction for Diorama's own sake).
            self.refresh_diorama_render(&msg);
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
            // Ticket W13-02b: the SNES memories, when the core captured
            // them. `None` on a NES session and whenever the capture is
            // off, which is what makes the panels console-aware.
            self.debug_panels.data.snes = msg.snes;
            // Ticket W13-02i: the typed register file, one field for both
            // consoles.
            self.debug_panels.data.cpu_regs = *msg.cpu_regs;
            self.debug_panels.data.vram = *msg.vram;
            self.debug_panels.data.palette_ram = *msg.palette_ram;
            // Ticket W13-02e: fold this frame's MemWatch events into the
            // per-watch hit counts BEFORE the events are handed to the
            // viewer, because the viewer's list is replaced every frame
            // and a count that only existed there would reset with it.
            self.debug_panels
                .annotations
                .record_watch_hits(&latest_bundle_events);
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
            // Ticket W20-09 follow-up: the picture on screen is the frame
            // the shell RESOLVED this pass — with an HD pack composited in
            // — not the core's raw `msg.rgba`. Until this fix the composite
            // went only into `last_frame_rgba` (what `hdpack_reaches_the_
            // app` asserts on), so a loaded pack changed nothing a player
            // could see. Compare mode keeps its own same-geometry pair.
            let (displayed, dw, dh): (&[u8], usize, usize) =
                match (&compare_rgba, &self.last_frame_rgba, self.last_frame_size) {
                    (Some(c), _, _) => (c.as_slice(), msg.width, msg.height),
                    (None, Some(resolved), Some((w, h))) => (resolved.as_slice(), w, h),
                    _ => (&msg.rgba, msg.width, msg.height),
                };
            // Ticket W20-02: Settings › Video's shader, over the picture
            // the player sees — never over the compare pair (a research
            // view of the accuracy-exact frame) and never into a capture.
            let shader = if compare_rgba.is_none() {
                self.settings
                    .video
                    .shader
                    .as_deref()
                    .and_then(crate::shader_select::kind_from_id)
            } else {
                None
            };
            if shader.is_some() && self.shader_chain.is_none() {
                if let Some(gpu) = &self.gpu {
                    self.shader_chain = Some(rf_renderer::ShaderChain::new(gpu));
                }
            }
            let shaded = match (shader, &self.shader_chain, &self.gpu) {
                (Some(kind), Some(chain), Some(gpu)) if self.shader_budget.is_enabled() => {
                    // Relative to the frame actually being shaded (an HD
                    // pack's is already larger), so the shader draws at
                    // about the size it will be shown.
                    let n = crate::shader_select::output_scale(
                        self.last_play_rect.map_or(0.0, |r| r.height()),
                        dh,
                    );
                    let (w32, h32) = (
                        u32::try_from(dw).unwrap_or(0),
                        u32::try_from(dh).unwrap_or(0),
                    );
                    let stage =
                        crate::shader_select::stage(kind, &self.settings.shaders.values(kind))
                            .with_out_size(w32 * n, h32 * n);
                    let started = std::time::Instant::now();
                    let result = chain.render(gpu, displayed, w32, h32, &[stage]);
                    let ms = started.elapsed().as_secs_f64() * 1000.0;
                    match result {
                        Ok(out) => {
                            if self.shader_budget.record_sample_ms(ms) {
                                self.shader_note = Some(
                                    "Shader paused: this machine cannot run it at full speed. Choose it again to retry."
                                        .to_string(),
                                );
                            }
                            Some((out, (w32 * n) as usize, (h32 * n) as usize))
                        }
                        Err(e) => {
                            // FR-REND-007's shape: fall back to the plain
                            // picture and say so once, never a black frame.
                            self.shader_note =
                                Some(format!("Shader failed ({e}); showing the plain picture."));
                            self.settings.video.shader = None;
                            None
                        }
                    }
                }
                _ => None,
            };
            let (displayed, dw, dh, filter) = match &shaded {
                Some((out, w, h)) => (out.as_slice(), *w, *h, egui::TextureOptions::LINEAR),
                None => (displayed, dw, dh, egui::TextureOptions::NEAREST),
            };
            if self.hash_display_for_test {
                self.display_hash = Some(fnv1a_hash(displayed, &[]));
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([dw, dh], displayed);
            match &mut self.texture {
                Some(tex) => tex.set(image, filter),
                None => {
                    self.texture = Some(ctx.load_texture("nes-frame", image, filter));
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

    /// The honesty badge exactly as the status bar draws it: mode, the
    /// effective-enhancement count, then the MetalFX and HD-pack suffixes.
    /// One function so the bar and the tests cannot disagree.
    pub fn status_badge(&self) -> String {
        let badge = crate::enhance_ui::append_metalfx_badge_suffix(
            crate::enhance_ui::badge_text(
                self.console_label,
                &self.current_game_settings,
                &self.game_facts(),
            ),
            self.settings.video.metalfx,
            rf_renderer::metalfx_detect(),
            METALFX_SCALER_WIRED,
        );
        // Ticket W20-09: replaced art is an enhancement the player can
        // see, so the badge says so (principle 2) — until W20-09 a loaded
        // pack changed the picture with the badge still reading
        // "Accuracy".
        if self.hd_pack.is_some() {
            format!("{badge} · HD pack")
        } else {
            badge
        }
    }

    /// Ticket W20-09: everything about the running game that decides
    /// whether a feature row can actually act — the one place it is
    /// computed, for the badge, the breakdown, the Features tab and the
    /// diorama/mode-7 subscriptions alike.
    pub(crate) fn game_facts(&self) -> crate::enhance_ui::GameFacts {
        crate::enhance_ui::GameFacts {
            profile_matched: self.profile_matched,
            level_decoded: self.level_session.is_some(),
            diorama_available: self
                .level_session
                .as_ref()
                .is_some_and(crate::level_view::LevelSession::has_collision),
            mode7_active: self.mode7_seen,
            widescreen_supported: self.console_label == "SNES",
            // Until W20-17 runs `rf_renderer::fog::FogPass` in the live
            // view (ENHANCEMENT_AUDIT.md §2).
            fog_rendered: false,
        }
    }

    /// Ticket W16-13: whether Diorama is currently EFFECTIVE — Game-Aware
    /// mode, a profile with collision, and the toggle on
    /// (`crate::enhance_ui::feature_rows`'s "diorama" row, the single
    /// place this gating is computed; re-deriving mode+profile+settings
    /// logic here instead of calling it is exactly the drift that left
    /// `self.profile_matched` unassigned for a whole ticket, see the bug
    /// fix noted where `self.profile_matched` is set above).
    fn diorama_effective(&self) -> bool {
        crate::enhance_ui::feature_rows(&self.current_game_settings, &self.game_facts())
            .into_iter()
            .find(|r| r.id == "diorama")
            .is_some_and(|r| r.effective())
    }

    /// Ticket W16-14: whether "Mode 7 as 3D" is currently EFFECTIVE —
    /// `enhance_ui::feature_rows`'s "mode7_ground" row, the single place
    /// this gating is computed (mirrors [`Self::diorama_effective`]'s own
    /// doc and its reason for existing: re-deriving the gating logic here
    /// instead of calling it is exactly the drift that once left
    /// `self.profile_matched` unassigned for a whole ticket).
    fn mode7_ground_effective(&self) -> bool {
        crate::enhance_ui::feature_rows(&self.current_game_settings, &self.game_facts())
            .into_iter()
            .find(|r| r.id == "mode7_ground")
            .is_some_and(|r| r.effective())
    }

    /// Ticket W16-13 acceptance 1: keep [`Self::diorama_wanted`] in sync
    /// with [`Self::diorama_effective`], and arm/disarm exactly the two
    /// core commands `enhanced_view::compose_diorama` needs data from —
    /// `CoreCommand::SetLayerExtraction` (for `FrameMsg::sprite_rgba`) and
    /// the level probe (`Self::set_level_probe`, for `FrameMsg::
    /// level_probe`'s camera/entity bytes) — the moment Diorama's own
    /// effective state changes.
    ///
    /// Called every repaint (`eframe::App::ui`), so a mode switch, a
    /// settings toggle from the Enhance workspace, or a test harness
    /// flipping `GameSettings.diorama` directly (`Self::
    /// set_diorama_for_test`) are all picked up uniformly within one
    /// frame — no separate call needed at every place `full_level_view`/
    /// `show_layers`/the mode combo could change.
    ///
    /// **Never clobbers what something else still wants.** Turning
    /// Diorama OFF only actually disarms a command when nothing else is
    /// still asking for it (the Layers debug window for extraction, Full-
    /// level view for the probe) — the OR-safety `full_level_set`'s own
    /// handler and the "Layers (debug)" checkbox handler mirror on their
    /// own turning-off path (both now OR in `Self::diorama_effective`
    /// too, so the disarm direction is symmetric whichever toggle moves
    /// last).
    fn sync_diorama_subscription(&mut self) {
        let wants = self.diorama_effective();
        if wants != self.diorama_wanted {
            self.diorama_wanted = wants;
            if wants {
                self.send_command(CoreCommand::SetLayerExtraction(true));
                self.set_level_probe(true);
            } else {
                // Ticket W16-14: Mode 7 ground needs the SAME sprite-layer
                // extraction (billboards on its ground, same as walls) —
                // an OR-safety check, same shape `!self.show_layers`
                // already is.
                if !self.show_layers && !self.mode7_ground_wanted {
                    self.send_command(CoreCommand::SetLayerExtraction(false));
                }
                if !self.current_game_settings.full_level_view {
                    self.set_level_probe(false);
                }
                // Acceptance 1: "disabling returns to the flat enhanced
                // view the same frame" — clear immediately rather than
                // waiting for a stale render to age out on its own.
                self.diorama_render = None;
                self.diorama_texture = None;
            }
        }

        // Ticket W16-14: "Mode 7 as 3D" — its own independent wanted/
        // armed transition, same shape as the walls tier above but
        // needing neither a collision-derived level probe nor
        // `diorama_ground_rgba` (its ground comes from VRAM/CGRAM every
        // frame, `Self::refresh_diorama_render`'s own doc for the
        // caching rule). Shares `CoreCommand::SetLayerExtraction` with
        // the walls tier (same OR-safety in both directions) and shares
        // `Self::diorama_render`/`Self::diorama_texture` (the same live-
        // view slot, composited in place of the flat plane either way).
        let mode7_wants = self.mode7_ground_effective();
        if mode7_wants != self.mode7_ground_wanted {
            self.mode7_ground_wanted = mode7_wants;
            if mode7_wants {
                self.send_command(CoreCommand::SetLayerExtraction(true));
            } else {
                if !self.show_layers && !self.diorama_wanted {
                    self.send_command(CoreCommand::SetLayerExtraction(false));
                }
                self.diorama_render = None;
                self.diorama_texture = None;
            }
        }
    }

    /// The decoded level's Diorama ground texture, computed once per ROM
    /// and cached in [`Self::diorama_ground_rgba`] (ticket W16-13).
    ///
    /// **Why cached, not rebuilt every frame.** `enhanced_view::
    /// render_level_rgba` decodes every metatile's tiles through
    /// `rf_debugger::pattern::decode_tile` — real CPU work, proportional
    /// to the level's size, and `level_view.rs`'s own module doc already
    /// establishes the reason it must not repeat: "ROM bytes do not
    /// change, so re-deriving \[it\] every frame would produce the same
    /// answer at 60 Hz forever." `Self::level_texture` already caches the
    /// egui-texture rendering of this exact same data for the Enhance
    /// workspace's Map tab; this is the same rendering, kept as raw bytes
    /// instead of a GPU texture because `DioramaPass::render` needs to
    /// upload it as its OWN texture, not read an egui one.
    fn diorama_ground_rgba(&mut self) -> Option<(std::rc::Rc<Vec<u8>>, u32, u32)> {
        if self.diorama_ground_rgba.is_none() {
            let session = self.level_session.as_ref()?;
            let chr = self.debug_panels.data.chr_rom.as_deref()?;
            let rgba = enhanced_view::render_level_rgba(
                &session.level,
                chr,
                rf_debugger::pattern::PatternTable::Left,
                LEVEL_PALETTE,
                session.geometry,
            );
            let (w, h) = (session.geometry.width_px, session.geometry.height_px);
            self.diorama_ground_rgba = Some((std::rc::Rc::new(rgba), w, h));
        }
        self.diorama_ground_rgba.clone()
    }

    /// Ticket W16-13 acceptance 1: recompute the Diorama pass's output for
    /// THIS frame and upload it as [`Self::diorama_texture`] — the
    /// per-live-frame counterpart to [`Self::refresh_ultrawide_render`]
    /// (which only fires on a `CanvasSnapshot` reply; Diorama needs no
    /// such round trip, since the sprite layer and level probe already
    /// ride on every `FrameMsg`, tickets W3-03/W11-02).
    ///
    /// ## Why synchronous on this thread, not off-thread (acceptance 1)
    ///
    /// W16-06's own bench rows measured `DioramaPass::render` at
    /// 1.82/2.19 ms p95 for the fixture room — comfortably under this
    /// ticket's 3 ms bar, and the same order of magnitude
    /// `EnhancedCompositor`'s own per-frame work already costs on THIS
    /// thread every time the Ultrawide/Map view refreshes. Standing up a
    /// second worker thread, a result channel, and an "is the previous
    /// refresh still running" latch for ~2 ms of work would add a new
    /// thread-safety surface `ARCHITECTURE.md` §3 does not otherwise need,
    /// to buy nothing this budget does not already have.
    ///
    /// [`Self::diorama_budget`] (the same `rf_renderer::fog::BudgetGate`
    /// `tests/diorama_golden.rs`'s own reuse test proves is the real one)
    /// watches the ACTUAL measured time on THIS machine and stops calling
    /// `DioramaPass::render` if its own p95 ever crosses the shared
    /// disable threshold — the "drop a refresh that would blow the
    /// budget" half of acceptance 1, applied here instead of across a
    /// channel: the core thread is never blocked either way, because it
    /// was never involved — `compose_diorama` only ever runs on the UI
    /// thread, reading data the core thread already published.
    fn refresh_diorama_render(&mut self, msg: &core_thread::FrameMsg) {
        // Ticket W16-14: Mode 7 ground takes priority when both are
        // somehow wanted at once (a collision-sourced profile matching a
        // BG-mode-7 title would be unusual, but never ambiguous this way)
        // — it has its own early-return ladder, entirely separate from
        // the walls path below.
        if self.mode7_ground_wanted {
            self.refresh_mode7_ground_render(msg);
            return;
        }
        if !self.diorama_wanted || msg.sprite_rgba.is_empty() {
            return;
        }
        let Some(probe) = &msg.level_probe else {
            return;
        };
        if !self.diorama_budget.is_enabled() {
            self.diorama_render = None;
            self.diorama_texture = None;
            return;
        }
        let Some((ground_rgba, ground_w, ground_h)) = self.diorama_ground_rgba() else {
            return;
        };
        let (Some(gpu), Some(session)) = (&self.gpu, &self.level_session) else {
            return;
        };
        if self.diorama_pass.is_none() {
            self.diorama_pass = Some(rf_renderer::diorama::DioramaPass::new(gpu));
        }
        let pass = self
            .diorama_pass
            .as_ref()
            .expect("just constructed above when absent");

        let addrs = Self::probe_addrs(session);
        let values = probe.values.clone();
        let read = move |addr: u32| -> u8 {
            addrs
                .iter()
                .position(|a| *a == addr)
                .and_then(|i| values.get(i).copied())
                .unwrap_or(0)
        };
        let (width, height) = (
            u32::try_from(msg.width).unwrap_or(0),
            u32::try_from(msg.height).unwrap_or(0),
        );

        let start = std::time::Instant::now();
        let result = enhanced_view::compose_diorama(
            gpu,
            pass,
            session,
            &ground_rgba,
            ground_w,
            ground_h,
            &read,
            &probe.table,
            &msg.sprite_rgba,
            width,
            height,
            msg.sprite_height_px,
            width,
            height,
        );
        self.diorama_budget
            .record_sample_ms(start.elapsed().as_secs_f64() * 1000.0);

        match result {
            Ok(render) => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [render.width as usize, render.height as usize],
                    &render.rgba,
                );
                match &mut self.diorama_texture {
                    Some(tex) => tex.set(image, egui::TextureOptions::NEAREST),
                    None => {
                        self.diorama_texture = Some(self.ctx.load_texture(
                            "diorama-frame",
                            image,
                            egui::TextureOptions::NEAREST,
                        ));
                    }
                }
                self.diorama_render = Some(render);
            }
            Err(_) => {
                // Silent fallback to the flat view (`enhanced_view::
                // select_active_view`'s own doc: Diorama degrades quietly
                // rather than blocking play or showing an error box over
                // the game).
                self.diorama_render = None;
                self.diorama_texture = None;
            }
        }
    }

    /// Ticket W16-14: [`Self::refresh_diorama_render`]'s Mode 7 ground
    /// counterpart. Same synchronous-on-this-thread reasoning that
    /// method's own doc gives (`DioramaPass::render`'s measured cost),
    /// and it shares the same [`Self::diorama_render`]/[`Self::
    /// diorama_texture`] live-view slot — "composite in place of the flat
    /// plane" (acceptance criterion 3) means the SAME slot `video_panel`
    /// already paints, not a second one `select_active_view` would need
    /// to learn about.
    ///
    /// ## The caching rule
    ///
    /// The ground texture (`rf_snes::debug::render_mode7_plane_rgba` at
    /// density 2) is rebuilt only when a cheap FNV-1a hash of the raw
    /// VRAM/CGRAM bytes changes (`Self::mode7_plane_cache`) — decoding
    /// 128x128 tiles at 2x density every frame would cost real CPU time
    /// for a playfield that, for most of a level, does not change tile
    /// data frame to frame (only the matrix/scroll registers move).
    fn refresh_mode7_ground_render(&mut self, msg: &core_thread::FrameMsg) {
        let Some(frame) = &msg.mode7 else {
            // This session wants Mode 7 ground, but the game is not IN BG
            // mode 7 this particular frame (a menu, a pause screen) —
            // acceptance criterion 3's "off returns to flat the same
            // frame" applies just as much to "temporarily not in mode 7".
            self.diorama_render = None;
            self.diorama_texture = None;
            return;
        };
        if msg.sprite_rgba.is_empty() {
            return;
        }
        if !self.diorama_budget.is_enabled() {
            self.diorama_render = None;
            self.diorama_texture = None;
            return;
        }
        let Some(snes) = &msg.snes else {
            return;
        };
        let Some(gpu) = &self.gpu else {
            return;
        };
        if self.diorama_pass.is_none() {
            self.diorama_pass = Some(rf_renderer::diorama::DioramaPass::new(gpu));
        }
        let pass = self
            .diorama_pass
            .as_ref()
            .expect("just constructed above when absent");

        const MODE7_PLANE_DENSITY: u32 = 2;
        const MODE7_PLANE_TILES: u16 = 128;
        let hash = fnv1a_hash(&snes.vram, &snes.cgram);
        let need_rebuild = self
            .mode7_plane_cache
            .as_ref()
            .is_none_or(|(cached, ..)| *cached != hash);
        if need_rebuild {
            let rgba = rf_snes::debug::render_mode7_plane_rgba(
                &snes.vram,
                &snes.cgram,
                MODE7_PLANE_TILES,
                MODE7_PLANE_TILES,
                MODE7_PLANE_DENSITY,
            );
            let side = u32::from(MODE7_PLANE_TILES) * 8 * MODE7_PLANE_DENSITY;
            self.mode7_plane_cache = Some((hash, std::rc::Rc::new(rgba), side, side));
        }
        let (_, plane_rgba, plane_w, plane_h) = self
            .mode7_plane_cache
            .as_ref()
            .expect("just populated above when absent");
        let (plane_rgba, plane_w, plane_h) = (plane_rgba.clone(), *plane_w, *plane_h);

        let sprites = rf_snes::debug::decode_oam(&snes.oam);
        // `$2101` bits 5-7 -- `ppu_regs` is indexed so `[n]` is `$21nn`
        // (`core_thread::FrameMsg::snes`'s own doc).
        let obj_size_select = snes.ppu_regs.get(0x01).copied().unwrap_or(0) >> 5;
        let (width, height) = (
            u32::try_from(msg.width).unwrap_or(0),
            u32::try_from(msg.height).unwrap_or(0),
        );

        let start = std::time::Instant::now();
        let result = enhanced_view::compose_mode7_ground(
            gpu,
            pass,
            &frame.top,
            &frame.bottom,
            &plane_rgba,
            plane_w,
            plane_h,
            &sprites,
            obj_size_select,
            // The SNES's own native resolution — `matrix_scale`'s "1.0,
            // no zoom" is defined relative to THIS, not to whatever
            // width/height this particular frame happens to be rendered
            // at (`rf_renderer::mode7_plane::SNES_NATIVE_WIDTH_PX`'s own
            // doc; `mode7_sprite_billboard`'s own doc for why sprite
            // placement uses `sprite_w`/`sprite_h` instead, below).
            rf_renderer::mode7_plane::SNES_NATIVE_WIDTH_PX,
            &msg.sprite_rgba,
            width,
            height,
            width,
            height,
        );
        self.diorama_budget
            .record_sample_ms(start.elapsed().as_secs_f64() * 1000.0);

        match result {
            Ok(render) => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [render.width as usize, render.height as usize],
                    &render.rgba,
                );
                match &mut self.diorama_texture {
                    Some(tex) => tex.set(image, egui::TextureOptions::NEAREST),
                    None => {
                        self.diorama_texture = Some(self.ctx.load_texture(
                            "diorama-frame",
                            image,
                            egui::TextureOptions::NEAREST,
                        ));
                    }
                }
                self.diorama_render = Some(render);
            }
            Err(_) => {
                // Same silent-degrade posture `refresh_diorama_render`'s
                // own doc states.
                self.diorama_render = None;
                self.diorama_texture = None;
            }
        }
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
        if self.core.is_none() {
            return;
        }
        // Ticket W10-05: the Map TAB is a consumer of the stitched canvas
        // in its own right, not a passenger on the camera toggle.
        //
        // It used to be gated on `camera == Ultrawide` alone, so a tab
        // called "Map" sat reading "Nothing stitched yet" while the
        // canvas demonstrably existed — the user had to know to flip an
        // unrelated View-menu toggle, and nothing said so. Found by
        // photographing the workspace at frame 308 of a live session
        // against a canvas the same tour rendered in full at frame 1200.
        //
        // Same throttle either way: `CanvasAccumulator::current_canvas`
        // clones the whole stitched canvas ("tens of MB/s for a level of
        // any real size", W4-03e), so driving it from tab visibility must
        // keep the interval, not drop it — which is why this is one
        // condition and not a second request path.
        let wanted = self.camera == CameraToggle::Ultrawide || self.show_enhance;
        if !wanted {
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
        // Ticket W15-07: every colour below reads from `Tokens`, not the
        // raw `Palette` — `Tokens::from_accessibility` makes the exact
        // same DEFAULT/HIGH_CONTRAST choice `AccessibilitySettings::palette`
        // always made, just expressed as the derived token set.
        let a = self.settings.accessibility.normalized();
        let tokens = crate::theme::Tokens::from_accessibility(&self.settings.accessibility);
        let (bg, raised, text, muted, accent) = (
            tokens.bg,
            tokens.surface,
            tokens.ink,
            tokens.muted,
            tokens.accent,
        );

        // Ticket W20-08: start from egui's light visuals for the Light
        // theme, so anything this function does not override (selection
        // text, shadows, scrollbar ink) is drawn for a light surface.
        let mut v = if !a.high_contrast && a.theme == crate::accessibility::ThemeChoice::Light {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
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
            // Ticket W15-08 acceptance 3: a larger type scale while a
            // gamepad is the most-recently-active device — a couch/TV
            // distance the mouse-tuned sizes above were never chosen for.
            // Cards and rows reflow automatically: neither names an
            // explicit `FontId` for its title (`library_rows`'/
            // `library_cards`' `Label`s use whatever `TextStyle::Body`
            // resolves to), so changing the style here is the whole fix —
            // there is no second size to update at either call site.
            let pad_active = self.last_active_input == crate::ui_nav::InputDevice::Gamepad;
            style.text_styles = if pad_active {
                [
                    (
                        TextStyle::Heading,
                        FontId::new(21.0, FontFamily::Name("display".into())),
                    ),
                    (TextStyle::Body, FontId::new(16.0, FontFamily::Proportional)),
                    (
                        TextStyle::Button,
                        FontId::new(16.0, FontFamily::Proportional),
                    ),
                    (
                        TextStyle::Small,
                        FontId::new(13.0, FontFamily::Proportional),
                    ),
                    (
                        TextStyle::Monospace,
                        FontId::new(14.0, FontFamily::Monospace),
                    ),
                ]
                .into()
            } else {
                // Ticket W15-07: the display face (IBM Plex Sans SemiBold,
                // `theme::install_fonts`) for headings — the one text
                // style §8's "headings and badges" names explicitly. Body/
                // Button/Small stay `Proportional`, which `install_fonts`
                // itself points at the body face, so nothing else needs a
                // family override.
                [
                    (
                        TextStyle::Heading,
                        FontId::new(17.0, FontFamily::Name("display".into())),
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
                .into()
            };
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
            // Ticket W15-07: `space_4`/`space_3` are `theme::SPACE`'s exact
            // pre-existing values (8.0/6.0) — the token names this number,
            // it does not change it. `button_padding.y` stays a literal:
            // 3.0 is not one of the five scale steps and forcing it onto
            // the scale would move a pixel value `tests/hud_fits.rs`
            // indirectly depends on for no reason but tidiness.
            style.spacing.item_spacing = egui::vec2(tokens.space_4, tokens.space_3);
            style.spacing.button_padding = egui::vec2(tokens.space_4, 3.0);
            for w in [
                &mut style.visuals.widgets.noninteractive,
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
                &mut style.visuals.widgets.open,
            ] {
                w.corner_radius = egui::CornerRadius::same(tokens.radius_sm as u8);
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
    /// **Still no border line, though the reason changed under this
    /// paragraph (ticket W15-07).** `theme::Tokens::line` now exists —
    /// derived, measured-enough (`theme::tests::line_is_between_muted_and_bg_and_visible_against_bg`)
    /// mid-tone between `muted` and `bg` — so "there is no colour to draw
    /// it with" is no longer true. The frame still omits it: two surfaces
    /// meeting is already a boundary, and adding a hairline on top is a
    /// separate visual decision this ticket (token plumbing, not new
    /// chrome) does not make. A future ticket can spend `line` here if a
    /// reviewer wants the edge; this comment no longer blocks it on a
    /// missing colour.
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
                        ui.separator();
                        // Ticket W11-04 (FR-PLUG-001). Until W11-04 there
                        // was no way to load a script at all: the SDK,
                        // the sandbox, the budget and a shipped example
                        // plugin all existed, and nothing in the app
                        // could run any of it.
                        if ui.button("Load script\u{2026}").clicked() {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.load_script(&dir);
                            }
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.script_host.is_some(),
                                egui::Button::new("Unload script"),
                            )
                            .clicked()
                        {
                            self.unload_script();
                            ui.close();
                        }
                        ui.separator();
                        // Ticket W20-09 (ENHANCEMENT_AUDIT.md §2): W11-05
                        // built HD-pack import end to end and nothing in
                        // the app called it — only a test did. Mesen's
                        // pack format is NES-only (`hires.txt` keys NES
                        // CHR tiles), so the item says so on SNES.
                        let nes_running = self.core.is_some() && self.console_label == "NES";
                        let load = ui
                            .add_enabled(nes_running, egui::Button::new("Load HD pack\u{2026}"))
                            .on_disabled_hover_text(
                                "Open an NES game first (HD packs are NES-only)",
                            );
                        if load.clicked() {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                let ctx = ui.ctx().clone();
                                self.load_hd_pack_from_menu(&dir, &ctx);
                            }
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                self.hd_pack.is_some(),
                                egui::Button::new("Remove HD pack"),
                            )
                            .clicked()
                        {
                            self.clear_hd_pack();
                            ui.close();
                        }
                        ui.separator();
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
                            let ctx = ui.ctx().clone();
                            self.request_quit(&ctx);
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
                        // Ticket W20-04: a checkbox, because it shows
                        // state like the rest of this menu (UX_WAVE_15 §7).
                        let mut fullscreen = ui.input(|i| i.viewport().fullscreen.unwrap_or(false));
                        if ui.checkbox(&mut fullscreen, "Fullscreen").changed() {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
                            ui.close();
                        }
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
                            // Ticket W16-13: OR'd with Diorama's own want
                            // — unchecking this must not disarm sprite-
                            // layer extraction out from under a still-
                            // effective Diorama, which reads the identical
                            // `FrameMsg::sprite_rgba`.
                            self.send_command(core_thread::CoreCommand::SetLayerExtraction(
                                self.show_layers || self.diorama_effective(),
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
                        // Ticket W15-03 (`UX_WAVE_15.md` §5, §11): Mode, the
                        // De-flicker checkbox and the Heuristics submenu all
                        // moved OUT of this menu and into the one Game
                        // Settings window — reachable identically from
                        // here, from the library's per-game context menu,
                        // and from the overlay menu, rather than being one
                        // of three different control surfaces for the same
                        // three settings. `_ = has_core` is not needed: the
                        // window itself checks `self.core.is_some()` for
                        // the De-flicker checkbox's enabled state, the same
                        // guard this menu used to apply here.
                        if ui.button("Game settings\u{2026}").clicked() {
                            self.game_settings_target = None;
                            self.show_game_settings = true;
                            ui.close();
                        }
                        // Ticket W16-02: next to "Game settings…" per this
                        // ticket's own brief.
                        if ui.button("Upscale Studio\u{2026}").clicked() {
                            self.set_upscale_studio_open(true);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Enhance\u{2026}").clicked() {
                            self.show_enhance = !self.show_enhance;
                            ui.close();
                        }
                    });
                });
            });
    }

    /// Ticket W3-05c (FR-ENH-012): the per-game heuristics report card,
    /// surfaced locally and only locally — NFR-005 forbids telemetry, and
    /// `rf_enhance::trust` has no I/O of any kind, so there is nowhere for
    /// this to leak to even by accident.
    ///
    /// Ticket W15-03 moved this out of the Enhance menu's `Heuristics…`
    /// submenu and into a section of the one Game Settings window — a free
    /// function now, taking whichever `GameSettings` the window is
    /// currently editing (the running game's, or a library entry's picked
    /// from its context menu) rather than reaching for
    /// `self.current_game_settings` itself, since it must work for both.
    fn heuristics_panel(
        ui: &mut egui::Ui,
        settings: &mut crate::game_settings::GameSettings,
    ) -> bool {
        ui.label("Trust ladder (D-004) — fresh install is all-shadow");
        ui.separator();
        let mut changed = false;
        for heuristic in HEURISTICS {
            let before = settings.trust.state(heuristic);
            let mut state = before;
            ui.horizontal(|ui| {
                ui.label(*heuristic);
                ui.radio_value(&mut state, TrustState::Shadow, "Shadow");
                ui.radio_value(&mut state, TrustState::Advisory, "Advisory");
                ui.radio_value(&mut state, TrustState::Active, "Active");
            });
            if state != before {
                settings.trust.set_state(heuristic, state);
                changed = true;
            }
            if let Some(s) = settings.trust.suppression(heuristic) {
                ui.label(format!(
                    "    suppressed in {}: {}",
                    s.granted_in_scene, s.justification
                ));
            }
        }
        ui.separator();
        let card = settings.trust.report_card();
        if card.is_empty() {
            ui.label("Report card: no contradictions recorded this session");
        } else {
            ui.label(format!("Report card ({} contradiction(s)):", card.len()));
            for c in card {
                ui.label(format!("  [{}] {} — {}", c.scene, c.heuristic, c.detail));
            }
        }
        changed
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
                    // Ticket W20-07: Run / Step Frame / Step Scanline and
                    // the f/sl readout moved to the Debug Viewers window
                    // (`Self::transport_controls`). A player starts a game
                    // by launching it (`Self::launch_rom` runs it) and
                    // pauses with Space or the in-game menu; single-
                    // stepping is a debugger's tool. What stays here is
                    // the state, not the controls: a paused game says so.
                    if self.core.is_some() && !self.running {
                        ui.add(readout(
                            egui::RichText::new(format!(
                                "{} Paused",
                                egui_phosphor::regular::PAUSE
                            ))
                            .strong(),
                        ));
                    }
                    ui.separator();

                    // FRONTEND_UI.md §1: the honesty badge, with its hover
                    // breakdown and hold-to-peek. Not decoration — §1 is
                    // explicit that enhancement is never on silently, and
                    // this is where a user finds out what they are looking
                    // at. It stays in the bar for exactly that reason: a
                    // badge behind a menu is a badge nobody reads.
                    let facts = self.game_facts();
                    let badge = self.status_badge();
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
                    let mut breakdown =
                        crate::enhance_ui::badge_breakdown(&self.current_game_settings, &facts);
                    if let Some(summary) = &self.hd_summary {
                        breakdown.push(format!("HD pack: {summary}"));
                    }
                    response.clone().on_hover_ui(|ui| {
                        for line in &breakdown {
                            ui.label(line);
                        }
                    });
                    // Held, not toggled: peeking is a gesture with an obvious
                    // end, and a toggle would leave someone stuck looking at
                    // the original wondering why their enhancements stopped.
                    // Ticket W15-06: OR'd with the App hotkey's own hold
                    // (`Self::poll_app_hotkeys`, computed earlier this
                    // frame) — either gesture forces the same view.
                    self.peeking_original =
                        response.is_pointer_button_down_on() || self.peek_key_held;

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
                ui.add(readout(format!(
                    "{} {}",
                    crate::icons::PROFILE,
                    elide_front(&name)
                )))
                .on_hover_text(path.display().to_string());
            }
            (None, true) => {
                ui.add(readout(format!("{} profile", crate::icons::PROFILE)));
            }
            (None, false) => {
                ui.add(readout(
                    egui::RichText::new(format!("{} no profile", crate::icons::PROFILE)).weak(),
                ));
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

    /// Ticket W15-04: an `egui::Modal` (built into egui since 0.31,
    /// verified against `egui-0.35.0/src/containers/modal.rs`) instead of
    /// a plain `egui::Window` — `docs/design/UX_WAVE_15.md` §5's table.
    /// A crash is the one dialog this app can put up uninvited, so it is
    /// also the one where "the UI never blocks emulation" (FRONTEND_UI §1
    /// principle 3) is moot: the core thread has already halted (FM-01)
    /// by the time this shows, so blocking input to the rest of the shell
    /// costs nothing that was still running.
    fn crash_dialog(&mut self, ctx: &egui::Context) {
        // Ticket W15-07: same open/close-fade shape as `overwrite_confirm_modal`
        // — `crash` itself still clears the instant Dismiss/outside-click
        // fires, `crash_fade_cache` is only what the fade-out paints.
        let open_now = self.crash.is_some();
        if let Some(report) = self.crash.clone() {
            self.crash_fade_cache = Some(report);
        }
        let alpha =
            crate::theme::modal_fade_alpha(ctx, egui::Id::new("rf_crash_modal_fade"), open_now);
        if alpha <= 0.0 {
            return;
        }
        let Some(report) = self.crash.clone().or_else(|| self.crash_fade_cache.clone()) else {
            return;
        };
        let tokens = crate::theme::Tokens::from_accessibility(&self.settings.accessibility);
        let mut dismiss = false;
        let modal = egui::Modal::new(egui::Id::new("rf_crash_modal"))
            .backdrop_color(tokens.modal_backdrop().gamma_multiply(alpha))
            .frame(
                egui::Frame::popup(&ctx.global_style())
                    .fill(tokens.surface.gamma_multiply(alpha))
                    .stroke(egui::Stroke::new(1.0, tokens.error)),
            )
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("Core crashed");
                ui.label(
                    "The emulator core panicked and was contained (FM-01); the core thread has \
                     halted.",
                );
                ui.label(format!("Message: {}", report.message));
                if let Some(loc) = &report.location {
                    ui.label(format!("Location: {loc}"));
                }
                ui.separator();
                ui.label("Trace tail:");
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        ui.monospace(&report.trace_tail);
                    });
                ui.separator();
                if ui.button("Dismiss").clicked() {
                    dismiss = true;
                }
            });
        if !open_now {
            return;
        }
        // "outside-click dismisses like its Dismiss button" (ticket
        // W15-04 acceptance 1): `should_close` covers the backdrop click
        // AND Escape, which is the modal's own idiomatic close gesture —
        // both are the same "never mind" as pressing Dismiss.
        if dismiss || modal.should_close() {
            self.crash = None;
        }
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

                // Ticket W10-04: the tab strip stays put; the CONTENT
                // scrolls. Paths lists one row per configured library
                // folder, which is as many as the user has added, and the
                // Accessibility tab's contrast readouts sit below a
                // slider — both can exceed a window bounded to the
                // viewport.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
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
                                    "Limited to 0.5x\u{2013}4x so the window always stays \
                                     readable enough to change it back.",
                                );
                                ui.separator();

                                // Ticket W20-08: the Light palette and its
                                // tokens existed with no way to choose them.
                                ui.label("Theme");
                                ui.horizontal(|ui| {
                                    for (theme, label) in [
                                        (crate::accessibility::ThemeChoice::Dark, "Dark"),
                                        (crate::accessibility::ThemeChoice::Light, "Light"),
                                    ] {
                                        if ui
                                            .add_enabled(
                                                !a.high_contrast,
                                                egui::RadioButton::new(a.theme == theme, label),
                                            )
                                            .clicked()
                                        {
                                            a.theme = theme;
                                            changed = true;
                                        }
                                    }
                                });
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
                                ui.weak("Overrides the theme while on.");
                            }
                            SettingsTab::Video => {
                                if self.video_controls(ui) {
                                    changed = true;
                                }
                                ui.separator();

                                if ui
                                    .checkbox(&mut self.settings.video.vsync, "V-sync")
                                    .changed()
                                {
                                    changed = true;
                                }
                                ui.small("Off can tear; on can add a frame of display latency.");
                                ui.separator();

                                // MetalFX (ticket W16-08): a scaler choice,
                                // not an enhancement -- it lives here next
                                // to scale_mode/shader/vsync, not on the
                                // enhancement ladder (module doc on
                                // rf_renderer::metalfx, CLAUDE.md law 6).
                                ui.label("MetalFX");
                                let availability = rf_renderer::metalfx_detect();
                                if ui
                                    .add_enabled(
                                        availability.is_available() && METALFX_SCALER_WIRED,
                                        egui::RadioButton::new(
                                            self.settings.video.metalfx
                                                == crate::settings::MetalFxSetting::Spatial,
                                            "MetalFX spatial",
                                        ),
                                    )
                                    .clicked()
                                {
                                    self.settings.video.metalfx =
                                        crate::settings::MetalFxSetting::Spatial;
                                    changed = true;
                                }
                                if ui
                                    .radio_value(
                                        &mut self.settings.video.metalfx,
                                        crate::settings::MetalFxSetting::Off,
                                        "Off",
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                                if !METALFX_SCALER_WIRED {
                                    ui.small(
                                        "Not used by the play view yet: choosing it changes nothing \
                                         you can see.",
                                    );
                                } else if let Some(reason) = availability.reason() {
                                    ui.small(reason);
                                } else {
                                    ui.small(
                                        "A scaler, not a content enhancement -- available in \
                                         Accuracy Mode too.",
                                    );
                                }
                                // Temporal is not offered: W16-08's attempt
                                // did not reach a shippable steady-state
                                // measurement (rf_renderer::metalfx module
                                // doc + plan.json W16-08 note).
                            }
                            SettingsTab::Audio => {
                                // Ticket W20-08: a list of the devices the
                                // system actually has, not a free-text box
                                // a typo silently ignored — and, until
                                // W20-08, a box nothing read at all.
                                ui.label("Output device");
                                let current = self
                                    .settings
                                    .audio
                                    .device
                                    .clone()
                                    .unwrap_or_else(|| "System default".to_string());
                                let devices = self.audio_device_names();
                                let has_audio = cfg!(feature = "audio");
                                ui.add_enabled_ui(has_audio, |ui| {
                                    egui::ComboBox::from_id_salt("audio-device")
                                        .selected_text(&current)
                                        .show_ui(ui, |ui| {
                                            if ui
                                                .selectable_label(
                                                    self.settings.audio.device.is_none(),
                                                    "System default",
                                                )
                                                .clicked()
                                            {
                                                self.settings.audio.device = None;
                                                changed = true;
                                            }
                                            for name in &devices {
                                                let selected = self.settings.audio.device.as_deref()
                                                    == Some(name.as_str());
                                                if ui.selectable_label(selected, name).clicked() {
                                                    self.settings.audio.device = Some(name.clone());
                                                    changed = true;
                                                }
                                            }
                                        });
                                });
                                if !has_audio {
                                    ui.small("This build has no sound output.");
                                } else {
                                    if let Some(opened) = crate::audio_out::opened_device_name() {
                                        ui.small(format!("Playing through: {opened}"));
                                    }
                                    ui.small("Applies from the next game you start.");
                                }
                                ui.separator();

                                if ui
                                    .add(
                                        egui::Slider::new(
                                            &mut self.settings.audio.latency_ms,
                                            crate::audio_out::LATENCY_RANGE_MS,
                                        )
                                        .text("Buffer (ms)"),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                                ui.small(
                                    "Lower answers your button presses sooner; higher keeps sound \
                                     smooth on a busy machine. Applies from the next game you start.",
                                );
                                ui.separator();

                                if ui
                                    .add(
                                        egui::Slider::new(
                                            &mut self.settings.audio.volume,
                                            0.0..=1.0,
                                        )
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
                                for index in 0..self.settings.paths.library_folders.len() {
                                    let path = self.settings.paths.library_folders[index]
                                        .path()
                                        .display()
                                        .to_string();
                                    let mut console =
                                        self.settings.paths.library_folders[index].console();
                                    ui.horizontal(|ui| {
                                        ui.label(&path);
                                        // Ticket W14-01: the hint is set HERE or it is a
                                        // feature nobody can reach — the same trap W2-13
                                        // named when the ROM picker could not select the
                                        // archives the loader had just learned to read.
                                        egui::ComboBox::from_id_salt(("library-root", index))
                                            .selected_text(match console {
                                                None => "Any console",
                                                Some(crate::library::Console::Nes) => "NES only",
                                                Some(crate::library::Console::Snes) => "SNES only",
                                            })
                                            .show_ui(ui, |ui| {
                                                for (label, value) in [
                                                    ("Any console", None),
                                                    (
                                                        "NES only",
                                                        Some(crate::library::Console::Nes),
                                                    ),
                                                    (
                                                        "SNES only",
                                                        Some(crate::library::Console::Snes),
                                                    ),
                                                ] {
                                                    ui.selectable_value(&mut console, value, label);
                                                }
                                            });
                                        if ui.small_button("Remove").clicked() {
                                            remove = Some(index);
                                        }
                                    });
                                    if console
                                        != self.settings.paths.library_folders[index].console()
                                    {
                                        self.settings.paths.library_folders[index] =
                                            crate::library::LibraryRoot::Hinted {
                                                path: std::path::PathBuf::from(&path),
                                                console,
                                            };
                                        changed = true;
                                    }
                                }
                                if let Some(index) = remove {
                                    self.settings.paths.library_folders.remove(index);
                                    changed = true;
                                }
                                if ui.button("Add folder\u{2026}").clicked() {
                                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                        if !self
                                            .settings
                                            .paths
                                            .library_folders
                                            .iter()
                                            .any(|root| root.path() == folder)
                                        {
                                            self.settings
                                                .paths
                                                .library_folders
                                                .push(crate::library::LibraryRoot::Bare(folder));
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
                                ui.small(
                                    "Empty = the default location under the config directory.",
                                );
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

                                ui.separator();
                                // Ticket W15-05, §4.3: the user art
                                // folder, the last-resort thumbnail
                                // source — matched by normalized title,
                                // never a network fetch (module doc on
                                // `crate::thumbnail`).
                                ui.label("Art folder");
                                let mut art_folder = self
                                    .settings
                                    .paths
                                    .art_folder
                                    .clone()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default();
                                ui.horizontal(|ui| {
                                    if ui.text_edit_singleline(&mut art_folder).changed() {
                                        self.settings.paths.art_folder = (!art_folder
                                            .trim()
                                            .is_empty())
                                        .then(|| std::path::PathBuf::from(art_folder.trim()));
                                        changed = true;
                                    }
                                    if ui.button("Browse\u{2026}").clicked() {
                                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                            self.settings.paths.art_folder = Some(folder);
                                            changed = true;
                                        }
                                    }
                                });
                                ui.small(
                                    "Empty = no user art folder; the library falls back to \
                                     save-state screenshots and first-frame captures only.",
                                );

                                ui.separator();
                                // Ticket W15-09, ruling D-011: opt-in,
                                // off by default (NON_GOALS #6) — this
                                // is the ONLY place the toggle lives, and
                                // `crate::art::should_fetch` is the only
                                // reader that gates the fetch worker on
                                // it.
                                if ui
                                    .checkbox(
                                        &mut self.settings.paths.fetch_art,
                                        "Fetch box art from the internet",
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                                ui.small(
                                    "Off by default. When on, box art missing from all local \
                                     sources is fetched over HTTPS from libretro-thumbnails by \
                                     title, cached locally, and never re-requested once cached. \
                                     No accounts, no ROM data ever sent — only picture requests.",
                                );
                                ui.small("Art: libretro-thumbnails");
                                if ui
                                    .add(
                                        egui::Slider::new(
                                            &mut self.settings.paths.art_cache_cap_mb,
                                            16..=8_192,
                                        )
                                        .text("Art cache cap (MB)"),
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                            }
                        }
                    });

                ui.separator();
                ui.small(&self.bindings_status);
            });
        self.show_settings = open;
        if !open {
            // Ticket W20-08: re-enumerate audio devices next time, so one
            // plugged in meanwhile shows up.
            self.audio_devices = None;
        }
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
    /// Ticket W20-08: hand Settings › Audio to the audio path — device and
    /// buffer for the next game started, volume immediately.
    fn publish_audio_settings(&self) {
        crate::audio_out::set_prefs(crate::audio_out::AudioPrefs {
            device: self.settings.audio.device.clone(),
            latency_ms: self.settings.audio.latency_ms,
        });
        crate::audio_out::set_volume(self.settings.audio.volume);
    }

    /// Ticket W20-08: output device names, enumerated once per Settings
    /// session (enumeration talks to the OS audio service; not every
    /// frame). Empty without the `audio` feature.
    fn audio_device_names(&mut self) -> Vec<String> {
        #[cfg(feature = "audio")]
        if self.audio_devices.is_none() {
            self.audio_devices = Some(rf_audio::output_device_names());
        }
        self.audio_devices.clone().unwrap_or_default()
    }

    /// Ticket W20-10: scaling, pixel shape and shader — the controls
    /// Settings › Video and the Quick Menu's Display section share, so the
    /// two cannot drift. Returns whether anything changed.
    fn video_controls(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.label("Scaling");
        for mode in crate::settings::ScaleMode::ALL {
            if ui
                .radio_value(&mut self.settings.video.scale_mode, mode, mode.label())
                .changed()
            {
                changed = true;
            }
        }
        ui.add_space(4.0);
        ui.label("Pixel shape");
        for aspect in crate::settings::PixelAspect::ALL {
            if ui
                .radio_value(
                    &mut self.settings.video.pixel_aspect,
                    aspect,
                    aspect.label(),
                )
                .changed()
            {
                changed = true;
            }
        }
        ui.separator();

        // Ticket W20-02: a picker over the shaders
        // that exist, each with the sliders its own
        // manifest declares — not a free-text box.
        ui.label("Shader");
        let selected = self
            .settings
            .video
            .shader
            .as_deref()
            .and_then(crate::shader_select::kind_from_id);
        egui::ComboBox::from_id_salt("shader-picker")
            .selected_text(selected.map_or("None", |k| k.manifest().display_name))
            .show_ui(ui, |ui| {
                if ui.selectable_label(selected.is_none(), "None").clicked() {
                    self.settings.video.shader = None;
                    changed = true;
                }
                for kind in crate::shader_select::KINDS {
                    let m = kind.manifest();
                    if ui
                        .selectable_label(selected == Some(kind), m.display_name)
                        .clicked()
                    {
                        self.settings.video.shader = Some(m.id.to_string());
                        self.shader_budget = rf_renderer::fog::BudgetGate::new();
                        self.shader_note = None;
                        changed = true;
                    }
                }
            });
        if let Some(kind) = selected {
            let values = self.settings.shaders.values(kind);
            for (p, mut v) in kind.manifest().params.iter().zip(values) {
                if ui
                    .add(egui::Slider::new(&mut v, p.min..=p.max).text(p.label))
                    .changed()
                {
                    self.settings.shaders.set(kind, p.name, v);
                    changed = true;
                }
            }
            if !kind.manifest().params.is_empty() && ui.small_button("Reset to defaults").clicked()
            {
                self.settings.shaders.reset(kind);
                changed = true;
            }
            if self.gpu.is_none() {
                ui.small("No GPU available to this window, so shaders cannot run.");
            }
        } else if self.settings.video.shader.is_some() {
            ui.small("The saved shader is not in this version; showing the plain picture.");
        }
        if let Some(note) = &self.shader_note {
            ui.small(note);
        }
        ui.small("Changes how the picture looks, not how the game runs.");
        changed
    }

    fn save_settings(&mut self) {
        self.publish_audio_settings();
        let Some(root) = self.config_root.clone() else {
            self.status = "No config directory; settings apply to this session only.".to_string();
            return;
        };
        if let Err(e) = crate::settings::save(&root, &self.settings) {
            self.status = format!("Could not save settings: {e}");
        }
    }

    /// Ticket W15-03 (`UX_WAVE_15.md` §5, §11): the one Game Settings
    /// window — Mode, De-flicker, Heuristics — opened identically from the
    /// context menu, the Enhance menu, and the overlay menu.
    ///
    /// `game_settings_target` decides which game's settings this frame
    /// edits: `None` reads/writes `current_game_hash`/
    /// `current_game_settings` directly (the running game — the same
    /// fields the Enhance menu's controls wrote before this ticket moved
    /// them here); `Some` reads/writes a library entry's OWN settings file,
    /// independent of whatever is currently running. Both branches share
    /// [`Self::heuristics_panel`] for the trust-ladder section, so there is
    /// exactly one place that renders it.
    fn game_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_game_settings {
            return;
        }
        let mut open = true;
        let title = match &self.game_settings_target {
            Some(target) => format!("Game settings \u{2014} {}", target.title),
            None => "Game settings".to_string(),
        };
        egui::Window::new(title)
            .id(egui::Id::new("game_settings_window"))
            .collapsible(true)
            .resizable(true)
            .default_width(420.0)
            .open(&mut open)
            .show(ctx, |ui| {
                // Ticket W15-03: the Heuristics report card
                // (`heuristics_panel`) can grow without bound over a
                // session (`tests/surfaces_can_scroll.rs`'s own rule: a
                // surface earns a place in `BOUNDED` only when its content
                // CANNOT exceed its container, and a growing report card
                // fails that outright).
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(target) = &mut self.game_settings_target {
                            let mut mode_changed = false;
                            ui.horizontal(|ui| {
                                ui.label("Mode");
                                egui::ComboBox::from_id_salt("game_settings_mode")
                                    .selected_text(target.settings.mode.display_name())
                                    .show_ui(ui, |ui| {
                                        for option in crate::game_settings::Mode::all() {
                                            if ui
                                                .selectable_value(
                                                    &mut target.settings.mode,
                                                    option,
                                                    option.display_name(),
                                                )
                                                .changed()
                                            {
                                                mode_changed = true;
                                            }
                                        }
                                    });
                            });
                            ui.separator();
                            // Ticket W15-03: this checkbox binds to `sprite_overlay`
                            // — the SAME field the Enhance menu's "De-flicker
                            // overlay" checkbox always wrote, per that setting's own
                            // doc (`GameSettings::sprite_overlay`, "the W3-05a
                            // sprite-limit-bypass overlay"). There is no live core
                            // to send `SetSpriteOverlay` to here: this branch edits
                            // a library entry that may not even be running.
                            let deflicker_changed = ui
                                .checkbox(&mut target.settings.sprite_overlay, "De-flicker overlay")
                                .changed();
                            ui.separator();
                            let heuristics_changed =
                                Self::heuristics_panel(ui, &mut target.settings);

                            if mode_changed || deflicker_changed || heuristics_changed {
                                if let Some(root) = self.config_root.clone() {
                                    if let Err(e) = crate::game_settings::save(
                                        &root,
                                        &target.hash,
                                        &target.settings,
                                    ) {
                                        self.status = format!("Could not save game settings: {e}");
                                    } else {
                                        self.library_meta.insert(
                                            target.hash.clone(),
                                            crate::library::RecencyMeta {
                                                last_played_epoch_secs: target
                                                    .settings
                                                    .last_played_epoch_secs,
                                                play_count: target.settings.play_count,
                                                favourite: target.settings.favourite,
                                            },
                                        );
                                    }
                                }
                            }
                        } else {
                            let mut mode = self.current_game_settings.mode;
                            ui.horizontal(|ui| {
                                ui.label("Mode");
                                egui::ComboBox::from_id_salt("game_settings_mode")
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
                            let mut changed = false;
                            if mode != self.current_game_settings.mode {
                                self.current_game_settings.mode = mode;
                                changed = true;
                            }
                            ui.separator();
                            let has_core = self.core.is_some();
                            if ui
                                .add_enabled(
                                    has_core,
                                    egui::Checkbox::new(
                                        &mut self.sprite_overlay,
                                        "De-flicker overlay",
                                    ),
                                )
                                .changed()
                            {
                                self.send_command(CoreCommand::SetSpriteOverlay(
                                    self.sprite_overlay,
                                ));
                                self.current_game_settings.sprite_overlay = self.sprite_overlay;
                                changed = true;
                            }
                            ui.separator();
                            if Self::heuristics_panel(ui, &mut self.current_game_settings) {
                                changed = true;
                            }
                            if changed {
                                // No Apply button anywhere in this app's settings —
                                // see `save_current_game_settings`'s own doc.
                                self.save_current_game_settings();
                            }
                        }
                    });
            });
        self.show_game_settings = open;
        if !open {
            self.game_settings_target = None;
        }
    }

    /// The Esc overlay menu (ticket W2-08; FRONTEND_UI §2: "resume · states
    /// · settings · switch mode · quit").
    ///
    /// Shown as a modal-ish window rather than a full-screen takeover
    /// because the point is to pause *access*, not to hide the game: a
    /// player pressing Esc mid-level wants to see where they were.
    /// Ticket W20-03: the one way the in-game menu opens or closes.
    ///
    /// Opening pauses a RUNNING game; closing resumes only a game the menu
    /// itself paused. Every route — Esc, the pad's Guide button or
    /// Select+Start, the window's close box — goes through here, so "the
    /// menu is open" and "the game is frozen" cannot disagree.
    fn set_overlay_menu(&mut self, open: bool) {
        if open == self.show_overlay_menu {
            return;
        }
        self.show_overlay_menu = open;
        if open {
            self.quick_section = crate::quick_menu::Section::Resume;
            self.quick_focus_pending = true;
            if self.core.is_some() && self.running {
                self.send_command(CoreCommand::Pause);
                self.running = false;
                self.menu_paused_game = true;
            }
        } else if std::mem::take(&mut self.menu_paused_game) && self.core.is_some() {
            self.send_command(CoreCommand::Resume);
            self.running = true;
        }
    }

    /// Ticket W20-03: leave the menu for another window (States,
    /// Settings, …) WITHOUT resuming — the player is still busy, and a game
    /// that ran on behind a settings window is the bug this ticket fixes.
    fn leave_overlay_menu_paused(&mut self) {
        self.show_overlay_menu = false;
        self.menu_paused_game = false;
    }

    /// Ticket W20-10 (`docs/design/UX_WAVE_20.md` §5): the Quick Menu.
    ///
    /// Replaces the plain "Menu" window Esc opened before. The game is
    /// paused (W20-03) and stays drawn underneath, dimmed by a scrim — egui
    /// has no blur, and a GPU blur pass for one menu is not worth a render
    /// path, so dim it is. A rail of sections on the left, the chosen
    /// section on the right, the honesty badge in the header (principle 2:
    /// even here, an enhanced picture says so), and a hint bar for the pad.
    ///
    /// Keyboard and pad drive it through egui focus, like the rest of the
    /// shell (`crate::ui_nav`): moving focus along the rail selects that
    /// section, so the right-hand side follows the cursor.
    fn overlay_menu(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.set_overlay_menu(!self.show_overlay_menu);
        }
        if !self.show_overlay_menu {
            return;
        }
        let tokens = crate::theme::Tokens::from_accessibility(&self.settings.accessibility);
        let screen = ctx.viewport_rect();

        // The scrim: the frozen frame stays visible, dimmed, and clicks
        // on it do nothing (a stray click must not reach the library or a
        // window behind).
        egui::Area::new(egui::Id::new("quick-menu-scrim"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(screen.size(), egui::Sense::hover());
                let bg = tokens.bg;
                ui.painter().rect_filled(
                    rect,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), 200),
                );
            });

        let has_core = self.core.is_some();
        // A FIXED size, not one derived from the content: an anchored Area
        // sized from last frame's content re-centres every frame, and a
        // scroll area that fills "available" height feeds that back — the
        // panel crept 16 px a frame and clicks landed where a button used
        // to be (found by tests/quick_menu.rs).
        let width = (screen.width() - 32.0).clamp(320.0, 720.0);
        let body_height = (screen.height() - 220.0).clamp(160.0, 440.0);
        let mut close = false;
        let mut leave_paused = false;
        egui::Area::new(egui::Id::new("quick-menu"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(tokens.surface)
                    .stroke(egui::Stroke::new(1.0, tokens.line))
                    .corner_radius(tokens.radius_md)
                    .inner_margin(16.0)
                    .show(ui, |ui| {
                        ui.set_width(width);
                        // Header: the mode pill (honesty badge) and state.
                        ui.horizontal(|ui| {
                            let badge = if has_core {
                                self.status_badge()
                            } else {
                                "No game running".to_string()
                            };
                            egui::Frame::new()
                                .fill(tokens.accent_soft)
                                .corner_radius(tokens.radius_sm)
                                .inner_margin(egui::Margin::symmetric(8, 3))
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new(badge).strong().color(tokens.ink));
                                });
                            // Bounded height: an unbounded right-to-left
                            // layout in a row takes the AREA's height, which
                            // is last frame's size — a feedback loop.
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), 24.0),
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if has_core {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} Paused",
                                                egui_phosphor::regular::PAUSE
                                            ))
                                            .color(tokens.muted),
                                        );
                                    }
                                },
                            );
                        });
                        ui.add_space(8.0);
                        ui.separator();
                        ui.horizontal_top(|ui| {
                            // The rail.
                            ui.vertical(|ui| {
                                ui.set_width(180.0);
                                ui.set_height(body_height);
                                // Left-aligned, full-width entries: a rail
                                // reads down its left edge.
                                ui.with_layout(
                                    egui::Layout::top_down_justified(egui::Align::Min),
                                    |ui| {
                                        for section in crate::quick_menu::Section::ALL {
                                            let selected = self.quick_section == section;
                                            let response = ui.add(
                                                egui::Button::selectable(
                                                    selected,
                                                    section.rail_text(),
                                                )
                                                .min_size(egui::vec2(180.0, 30.0)),
                                            );
                                            // Follow focus only when it MOVES here
                                            // (arrow keys / d-pad), so a mouse
                                            // click elsewhere is not overruled by
                                            // whichever entry still holds focus.
                                            if response.clicked() || response.gained_focus() {
                                                self.quick_section = section;
                                            }
                                            if self.quick_focus_pending && selected {
                                                response.request_focus();
                                                self.quick_focus_pending = false;
                                            }
                                        }
                                    },
                                );
                            });
                            ui.add_sized([1.0, body_height], egui::Separator::default().vertical());
                            // The section.
                            ui.vertical(|ui| {
                                // Fixed width as well as height — a card
                                // grid wrapping against "available" width
                                // widened the panel and moved it.
                                let content_width = width - 180.0 - 24.0;
                                ui.set_width(content_width);
                                ui.set_max_width(content_width);
                                ui.set_height(body_height);
                                egui::ScrollArea::vertical()
                                    .max_height(body_height)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        let (c, l) = self.quick_section_body(ui, has_core);
                                        close |= c;
                                        leave_paused |= l;
                                    });
                            });
                        });
                        ui.separator();
                        ui.label(
                            egui::RichText::new(crate::quick_menu::HINT)
                                .small()
                                .color(tokens.muted),
                        );
                    });
            });
        if leave_paused {
            self.leave_overlay_menu_paused();
        } else if close {
            self.set_overlay_menu(false);
        }
    }

    /// One Quick Menu section's content. Returns `(close, leave_paused)`:
    /// close the menu (resuming a game it paused), or leave it for another
    /// window with the game still paused.
    fn quick_section_body(&mut self, ui: &mut egui::Ui, has_core: bool) -> (bool, bool) {
        use crate::quick_menu::Section;
        let mut close = false;
        let mut leave_paused = false;
        let needs_game = matches!(
            self.quick_section,
            Section::Save | Section::Load | Section::Rewind | Section::Reset | Section::Quit
        );
        // Cards per row for the Save/Load grids at this panel's width.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let quick_columns = ((ui.available_width() + 10.0) / (SLOT_CARD_WIDTH + 26.0))
            .floor()
            .max(1.0) as usize;
        ui.heading(self.quick_section.label());
        ui.add_space(6.0);
        if needs_game && !has_core {
            ui.label("No game is running.");
            return (false, false);
        }
        match self.quick_section {
            Section::Resume => {
                ui.label("Back to the game.");
                if ui.button(egui::RichText::new("Resume").strong()).clicked() {
                    // Explicit, like the old menu: Resume runs the game
                    // even if it was paused before the menu opened.
                    leave_paused = true;
                    if has_core {
                        self.send_command(CoreCommand::Resume);
                        self.running = true;
                    }
                }
            }
            Section::Save => {
                self.refresh_state_slots_if_stale();
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
                if let Some((slot, _)) = self.slot_card_grid(ui, false, true, Some(quick_columns)) {
                    self.active_slot = Some(slot);
                    let ctx = ui.ctx().clone();
                    self.save_to_slot(slot, &ctx);
                }
            }
            Section::Load => {
                self.refresh_state_slots_if_stale();
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
                if let Some((slot, _)) = self.slot_card_grid(ui, true, false, Some(quick_columns)) {
                    self.active_slot = Some(slot);
                    self.load_from_slot(slot);
                    close = true;
                }
            }
            Section::Rewind => {
                ui.label("Rewind isn't available in this version yet.");
            }
            Section::Display => {
                if self.video_controls(ui) {
                    self.save_settings();
                }
                let mut fullscreen = ui.input(|i| i.viewport().fullscreen.unwrap_or(false));
                if ui.checkbox(&mut fullscreen, "Fullscreen").changed() {
                    let ctx = ui.ctx().clone();
                    self.toggle_fullscreen(&ctx);
                }
            }
            Section::Enhancements => {
                // The badge's hover breakdown, minus its last line — "hold
                // to peek" describes the status-bar badge, not this menu.
                for line in crate::enhance_ui::badge_breakdown(
                    &self.current_game_settings,
                    &self.game_facts(),
                )
                .into_iter()
                .filter(|l| !l.starts_with("Hold to peek"))
                {
                    ui.label(line);
                }
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Game settings\u{2026}").clicked() {
                        self.game_settings_target = None;
                        self.show_game_settings = true;
                        leave_paused = true;
                    }
                    if ui.button("Enhance workspace\u{2026}").clicked() {
                        self.show_enhance = true;
                        leave_paused = true;
                    }
                    // Ticket W5-06: only when a profile claims this ROM.
                    if let Some(path) = self.matched_profile.clone() {
                        if ui.button("Author\u{2026}").clicked() {
                            self.open_author_workspace(path);
                            leave_paused = true;
                        }
                    }
                });
            }
            Section::Controls => {
                // W15-06: bindings are learned by seeing them.
                egui::Grid::new("quick-menu-hotkeys")
                    .num_columns(2)
                    .show(ui, |ui| {
                        for action in crate::app_bindings::AppAction::ALL {
                            let key = self
                                .app_bindings
                                .key_for(action)
                                .map(|k| k.name().to_string());
                            let pad = self
                                .app_bindings
                                .pad_for(action)
                                .map(|b| b.name().to_string());
                            let binding = match (key, pad) {
                                (Some(k), Some(p)) => format!("{k} / {p}"),
                                (Some(k), None) => k,
                                (None, Some(p)) => p,
                                (None, None) => "\u{2014}".to_string(),
                            };
                            ui.label(action.label());
                            ui.label(binding);
                            ui.end_row();
                        }
                    });
                if ui.button("Remap controls\u{2026}").clicked() {
                    self.show_controls = true;
                    leave_paused = true;
                }
            }
            Section::Settings => {
                if ui.button("Open Settings\u{2026}").clicked() {
                    self.show_settings = true;
                    leave_paused = true;
                }
            }
            Section::Reset => {
                ui.label("Restart the game from power-on. Progress since your last save is lost.");
                if ui.button("Reset").clicked() {
                    self.reset_game();
                }
            }
            Section::Quit => {
                ui.label("Return to the library. Progress since your last save is lost.");
                if ui.button("Quit to library").clicked() {
                    self.close_rom();
                }
            }
        }
        (close, leave_paused)
    }

    /// Ticket W20-10: power-cycle the running game — open the same file
    /// again, which is the shell's whole-machine reset (a fresh core).
    fn reset_game(&mut self) {
        let Some(path) = self.current_rom_path.clone() else {
            return;
        };
        self.show_overlay_menu = false;
        self.menu_paused_game = false;
        self.launch_rom(&path);
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
        // Ticket W14-02: a first scan of a real collection takes seconds,
        // and an empty grid during it would render as "no games found" —
        // which is a different fact, and the one G-21 forced this screen
        // to keep straight.
        if self.library.is_none() && self.library_scan_in_flight() {
            ui.ctx().request_repaint();
            Self::home_empty(ui, "Scanning your ROM folders\u{2026}", |ui| {
                ui.add(readout(
                    egui::RichText::new(
                        "Every file is read once and identified by hashing it. The result is \
                         remembered, so this is only slow the first time.",
                    )
                    .weak(),
                ));
                ui.add_space(10.0);
                ui.spinner();
            });
            return;
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
                        rescan = self.pick_library_folder(ui.ctx());
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
                        rescan = self.pick_library_folder(ui.ctx());
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
            self.launch_rom(&path);
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
        // Two rows, matching UX_WAVE_15 §3's wireframe, rather than one —
        // search, the console filter, and the two new recency chips
        // already fill `WINDOW_SIZE`'s width on their own; adding the
        // sort control and the folder/rescan buttons to the SAME
        // `ui.horizontal` overflowed it, and because the folder/rescan
        // block is laid out `right_to_left` independently of the
        // left-to-right cursor, an overflow does not wrap — it OVERLAPS,
        // silently handing every click in the shared region to whichever
        // widget was added later. Splitting the row is what fixes the
        // click, not just the look.
        ui.horizontal(|ui| {
            let search_response = ui.add(
                egui::TextEdit::singleline(&mut self.library_search)
                    .hint_text("Search\u{2026}")
                    .desired_width(180.0),
            );
            // Ticket W15-01: read every frame the toolbar runs, so
            // `library_grid`'s Enter/arrow-key handling always sees this
            // frame's truth rather than a stale one from before the user
            // clicked into (or tabbed out of) the search box.
            self.library_search_focused = search_response.has_focus();
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
            ui.separator();
            // Ticket W15-02, §3: Recently played / Favourites, radio-like
            // with each other. `selectable_value` would need a visible
            // "All" chip to click back to — these two toggle themselves
            // off instead, since "neither" is the ordinary state, not a
            // third chip a player has to remember to press.
            for (label, variant) in [
                (
                    "Recently played",
                    crate::library::RecencyFilter::RecentlyPlayed,
                ),
                ("Favourites", crate::library::RecencyFilter::Favourites),
            ] {
                let active = self.library_recency_filter == variant;
                if ui.selectable_label(active, label).clicked() {
                    self.library_recency_filter = if active {
                        crate::library::RecencyFilter::All
                    } else {
                        variant
                    };
                }
            }
        });

        ui.horizontal(|ui| {
            // Ticket W15-02, §3: "Sort: [Title/Last played/Console]".
            // Three small chips rather than a `ComboBox` — this toolbar
            // already reads as a row of chips, and a fourth control that
            // looked different would stand out for no reason. Clicking
            // the active chip again returns to `None` (the auto default),
            // same toggle-off behaviour as the recency chips above.
            ui.add(readout(egui::RichText::new("Sort:").weak()));
            for (label, variant) in [
                ("Title", crate::library::SortMode::Title),
                ("Last played", crate::library::SortMode::LastPlayed),
                ("Console", crate::library::SortMode::Console),
            ] {
                let active = self.library_sort == Some(variant);
                if ui.selectable_label(active, label).clicked() {
                    self.library_sort = if active { None } else { Some(variant) };
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Add folder\u{2026}").clicked() {
                    *rescan = self.pick_library_folder(ui.ctx());
                }
                if ui.button("Rescan").clicked() {
                    *rescan = true;
                    // Ticket W15-04: only the EXPLICIT Rescan button
                    // arms the "library rescanned" toast —
                    // `poll_library_scan` reads this flag once the
                    // background scan it triggers lands.
                    self.library_rescan_toast_pending = true;
                }
                ui.add(readout(
                    egui::RichText::new(format!("{count} game(s)")).weak(),
                ));
                ui.separator();
                // Ticket W15-05, §3: "[▦][≡]" — the Grid/List toggle,
                // persisted immediately (like every other Settings write
                // in this app) rather than only on the next Settings
                // save, since a view choice made from the toolbar should
                // survive a restart without the user ever opening
                // Settings.
                let mut view_changed = false;
                if crate::icons::icon_toggle(
                    ui,
                    self.library_view == crate::library::LibraryView::List,
                    crate::icons::LIST_VIEW,
                    "List view",
                )
                .clicked()
                {
                    self.library_view = crate::library::LibraryView::List;
                    view_changed = true;
                }
                if crate::icons::icon_toggle(
                    ui,
                    self.library_view == crate::library::LibraryView::Grid,
                    crate::icons::GRID_VIEW,
                    "Grid view",
                )
                .clicked()
                {
                    self.library_view = crate::library::LibraryView::Grid;
                    view_changed = true;
                }
                if view_changed {
                    self.settings.library.view = self.library_view;
                    self.save_settings();
                }
            });
        });
    }

    /// §3.1's library screen, filtered by the toolbar, rendered as either
    /// the row list or the card grid per `self.library_view` (ticket
    /// W15-05's Grid/List toggle) — the two views share every OTHER piece
    /// of state on this page (selection, launch, favourite, context menu),
    /// so a toggle mid-session never loses the user's place. **No box
    /// art fetch** either way — NON_GOALS #5 rules out a network source;
    /// only the three LOCAL sources `crate::thumbnail` implements ever
    /// feed a thumbnail.
    ///
    /// Returns the ROM to open, if one was picked.
    ///
    /// Ticket W15-01 added the selection/launch layer on top of the row
    /// list W10-03 built: a selected-row focus ring, double-click and
    /// Enter both launching, and arrow keys walking the selection. `&mut
    /// self` (this was `&self`) because that state — `library_selected`
    /// — lives on `App`, not on the grid. Ticket W15-05 extended the
    /// arrow-key walk with Left/Right and a column stride for the grid
    /// view (`library_cards`'s own doc on `columns_per_row`).
    fn library_grid(
        &mut self,
        ui: &mut egui::Ui,
        library: &crate::library::Library,
    ) -> Option<std::path::PathBuf> {
        // Ticket W15-02: the filter+sort pipeline is now the pure
        // `crate::library::filter_and_sort`, unit-tested on its own in
        // `library.rs` — this call site is just wiring live toolbar state
        // into it.
        let matches: Vec<&crate::library::LibraryEntry> = crate::library::filter_and_sort(
            &library.entries,
            &self.library_meta,
            self.library_console_filter,
            self.library_recency_filter,
            &self.library_search,
            self.library_sort,
        );

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

        // Ticket W15-01: keyboard (and, via `ui_nav.rs`, gamepad — the
        // pad bridge turns d-pad/Activate into these exact `egui::Key`
        // events, so there is no separate branch for it here) selection
        // over the FILTERED list. Both stand down while the search box
        // has focus, or ArrowDown while typing "beta" would also walk
        // the selection and Enter would launch instead of just accepting
        // the search term.
        let search_has_focus = self.library_search_focused;
        let grid_mode = self.library_view == crate::library::LibraryView::Grid;
        // Ticket W15-05, §11 acceptance 2: "Left/Right move across a row
        // of cards, Up/Down by row" — only meaningful in Grid, so
        // `columns_per_row` is computed even when List is active (cheap)
        // but only consulted when `grid_mode`. `library_cards` lays cards
        // out with the SAME width/spacing constants, so a card's actual
        // per-row count on screen always matches what this arithmetic
        // predicts.
        let columns_per_row = Self::library_grid_columns(ui.available_width());
        let mut moved_by_keyboard = false;
        if !search_has_focus {
            let (down, up, left, right, home, end) = ui.ctx().input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowLeft),
                    i.key_pressed(egui::Key::ArrowRight),
                    i.key_pressed(egui::Key::Home),
                    i.key_pressed(egui::Key::End),
                )
            });
            let horizontal = grid_mode && (left || right);
            if down || up || home || end || horizontal {
                let last = matches.len() - 1;
                let current = self
                    .library_selected
                    .as_ref()
                    .and_then(|p| matches.iter().position(|e| &e.path == p));
                let stride = if grid_mode { columns_per_row } else { 1 };
                let next = if home {
                    0
                } else if end {
                    last
                } else if grid_mode && right {
                    current.map_or(0, |i| (i + 1).min(last))
                } else if grid_mode && left {
                    current.map_or(last, |i| i.saturating_sub(1))
                } else if down {
                    current.map_or(0, |i| (i + stride).min(last))
                } else {
                    // `up`, the only remaining case in this branch.
                    current.map_or(last, |i| i.saturating_sub(stride))
                };
                self.library_selected = Some(matches[next].path.clone());
                moved_by_keyboard = true;

                // Ticket W15-08 acceptance 5: an ArrowUp/Down/Left/Right
                // key ALSO arms egui's own built-in spatial focus
                // navigation (`Memory::Focus::begin_pass` reads the very
                // same key from `RawInput` and records a cardinal
                // `FocusDirection`, independent of anything this
                // function does with it) — and, unless cancelled,
                // `Focus::end_pass` uses it to redirect keyboard focus to
                // whatever OTHER focusable widget sits spatially in that
                // direction from wherever focus was BEFORE this key
                // (typically the star/Play button beside the card this
                // very key just selected). That redirect would run AFTER
                // `library_cards`/`library_rows` calls `request_focus()`
                // on the entry this key selected, silently overriding it
                // and breaking the exact equivalence acceptance 5 asks
                // for. Cancelling the direction here — before either
                // renders — means egui's own spatial search never runs,
                // and `request_focus()` is the only thing left deciding
                // where focus goes.
                ui.ctx()
                    .memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            }
        }

        // Enter launches the current selection — the keyboard/gamepad
        // half of principle 6 (§2): mouse gets double-click, keyboard and
        // pad get Enter/Activate, Play stays as the explicit affordance
        // for anyone who has learned neither gesture.
        if !search_has_focus && ui.ctx().input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Some(selected) = self.library_selected.clone() {
                if matches.iter().any(|e| e.path == selected) {
                    to_play = Some(selected);
                }
            }
        }

        // Ticket W15-02, §3: Space toggles Favourite on the selected row —
        // stands down while the search box has focus, same reasoning as
        // Enter above (typing "space invaders" must not favourite a game).
        if !search_has_focus && ui.ctx().input(|i| i.key_pressed(egui::Key::Space)) {
            if let Some(selected) = self.library_selected.clone() {
                if let Some(entry) = matches.iter().find(|e| e.path == selected) {
                    if let crate::library::EntryIdentity::Recognized {
                        normalized_sha256, ..
                    } = &entry.identity
                    {
                        self.toggle_favourite(normalized_sha256);
                    }
                }
            }
        }

        // Ticket W15-03: `Start` opens the context menu for the currently
        // selected row — consumed (and cleared) exactly once per press so
        // holding the button does not reopen the menu every frame it stays
        // down. Stands down while the search box has focus, same reasoning
        // as Enter/Space above.
        let pad_menu_requested = !search_has_focus && std::mem::take(&mut self.pad_menu_requested);
        if pad_menu_requested && self.library_selected.is_some() {
            self.library_context_menu_open = true;
        }

        // Ticket W15-07: `tokens.accent` is exactly what the old
        // `Palette::accent` conversion here produced — same colour, now
        // read through the token set every other W15 surface uses.
        // Ticket W15-08: `library_rows` now takes the whole token set
        // (not a bare `accent` `Color32`) so it can pick the pad ring's
        // stroke via `Self::focus_ring_stroke`, same as `library_cards`
        // already does.
        let tokens = crate::theme::Tokens::from_accessibility(&self.settings.accessibility);

        if grid_mode {
            self.library_cards(ui, &matches, &tokens, moved_by_keyboard, &mut to_play);
        } else {
            self.library_rows(ui, &matches, &tokens, moved_by_keyboard, &mut to_play);
        }
        to_play
    }

    /// Ticket W15-08 (`docs/design/UX_WAVE_15.md` §9): the stroke the
    /// library's selection ring draws with — thicker and in a stronger
    /// accent when the pad was the most-recently-active device, exactly
    /// acceptance 2's "the selection ring on the focused card/row is
    /// thicker... and uses a stronger accent". Shared by `library_rows`
    /// and `library_cards` so neither can silently drift from the other's
    /// idea of what the pad ring looks like — the same reasoning
    /// `library_grid_columns` already gives for being one function both
    /// layouts call.
    fn focus_ring_stroke(&self, tokens: &crate::theme::Tokens) -> egui::Stroke {
        if self.last_active_input == crate::ui_nav::InputDevice::Gamepad {
            egui::Stroke::new(crate::theme::FOCUS_RING_PAD, tokens.accent_strong)
        } else {
            egui::Stroke::new(crate::theme::FOCUS_RING_MOUSE, tokens.accent)
        }
    }

    /// How many cards fit per row at `available_width` — shared by
    /// `library_cards`'s own layout and `library_grid`'s Left/Right/Up/
    /// Down arithmetic, so the two can never disagree about what "one
    /// row" means. `.max(1)`: a window narrower than one card still
    /// shows a single column rather than dividing by zero.
    #[must_use]
    fn library_grid_columns(available_width: f32) -> usize {
        let per_card = CARD_WIDTH + CARD_SPACING;
        ((available_width / per_card).floor() as usize).max(1)
    }

    /// The row list (pre-W15-05 `library_grid`, unchanged in behaviour):
    /// rows, not an `egui::Grid`. A `Grid` sizes every column to its
    /// content, so the whole library huddled into the left third of the
    /// window with two thirds of empty space beside it — a list that
    /// looked like a rendering accident rather than the app's home
    /// screen. A row that lays its title out left-to-right and its Play
    /// button right-to-left fills the width by construction, at any
    /// window size, with no arithmetic to keep in sync.
    fn library_rows(
        &mut self,
        ui: &mut egui::Ui,
        matches: &[&crate::library::LibraryEntry],
        tokens: &crate::theme::Tokens,
        moved_by_keyboard: bool,
        to_play: &mut Option<std::path::PathBuf>,
    ) {
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
                    let is_selected =
                        self.library_selected.as_deref() == Some(entry.path.as_path());
                    let mut row_clicked = false;
                    let mut row_double_clicked = false;
                    // Ticket W15-03: hoisted out of the `ui.horizontal`
                    // closure below so the context menu (mouse right-click
                    // AND the pad's Start path) can be attached to it after
                    // the row finishes drawing.
                    let mut title_response: Option<egui::Response> = None;
                    let row = egui::Frame::NONE
                        .fill(fill)
                        .inner_margin(egui::Margin::symmetric(6, 3))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // The title is the click/double-click
                                // target, via `Label::sense` — NOT a
                                // whole-row `Response::interact` behind
                                // the content. Measured, not guessed: a
                                // row-wide interact added (as `Frame`'s
                                // own response must be) AFTER the Play
                                // button also wins clicks addressed AT
                                // the button, because egui resolves
                                // overlapping widgets by add order for
                                // the whole pass, not by where each
                                // `interact()` call sits in this
                                // function's source. The title and the
                                // button never overlap, so sensing the
                                // title directly can never take a click
                                // the button was supposed to get.
                                let response = ui.add(
                                    egui::Label::new(&entry.title).sense(egui::Sense::click()),
                                );
                                row_double_clicked = response.double_clicked();
                                row_clicked = response.clicked();
                                title_response = Some(response);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Play").clicked() {
                                            *to_play = Some(entry.path.clone());
                                        }
                                        match &entry.identity {
                                            crate::library::EntryIdentity::Recognized {
                                                console,
                                                normalized_sha256,
                                            } => {
                                                // Ticket W15-02, §3: the star
                                                // toggle next to Play. Only
                                                // for a Recognized entry — an
                                                // unrecognized ROM has no
                                                // hash to favourite by.
                                                let favourite = self
                                                    .library_meta
                                                    .get(normalized_sha256)
                                                    .is_some_and(|m| m.favourite);
                                                // W20-05: filled when on, outline when off —
                                                // shape, not colour alone.
                                                let star = if favourite {
                                                    crate::icons::filled(crate::icons::FAVOURITE)
                                                } else {
                                                    egui::RichText::new(crate::icons::FAVOURITE)
                                                };
                                                let label = if favourite {
                                                    "Unfavourite"
                                                } else {
                                                    "Favourite"
                                                };
                                                if crate::icons::icon_button(ui, star, label)
                                                    .clicked()
                                                {
                                                    self.toggle_favourite(normalized_sha256);
                                                }
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

                    if row_double_clicked {
                        // Ticket W15-01 acceptance 1: double-click launches
                        // regardless of what was selected before it.
                        *to_play = Some(entry.path.clone());
                    } else if row_clicked {
                        // Acceptance 2: a single click SELECTS ONLY — it
                        // must not also set `to_play`.
                        self.library_selected = Some(entry.path.clone());
                        // Ticket W15-08 acceptance 5: a mouse-driven
                        // selection change also becomes egui's real
                        // keyboard focus, same as the keyboard/pad path
                        // below — so AccessKit (which reports THAT focus,
                        // not `library_selected`) never disagrees with
                        // what is actually selected, regardless of which
                        // device did the selecting.
                        if let Some(title_response) = &title_response {
                            title_response.request_focus();
                        }
                    }

                    // Ticket W15-03: the context menu. Mouse right-click is
                    // handled by `Response::context_menu` itself (it reads
                    // `secondary_clicked()` internally, so nothing here
                    // needs to test for the button); the pad path is a
                    // second, explicitly-opened `Popup` on the SAME
                    // response, sharing the SAME `library_context_menu_body`
                    // so "the same menu" is a fact about which method
                    // renders the items, not a claim two call sites happen
                    // to agree on today.
                    if let Some(title_response) = &title_response {
                        title_response.context_menu(|ui| {
                            self.library_context_menu_body(ui, entry, &mut *to_play);
                        });
                        if is_selected && self.library_context_menu_open {
                            // `open_bool`'s `&mut bool` must not alias
                            // `self` while `.show`'s closure below also
                            // borrows `self` — a local copy, written back
                            // after, sidesteps that without any unsafe
                            // code.
                            let mut open = true;
                            let popup = egui::Popup::from_response(title_response)
                                .id(title_response.id.with("pad_context_menu"))
                                .open_bool(&mut open);
                            if popup.is_open() {
                                popup.show(|ui| {
                                    self.library_context_menu_body(ui, entry, &mut *to_play);
                                });
                            }
                            self.library_context_menu_open = open;
                        }
                    }

                    if is_selected {
                        // The ring is drawn around the whole row (`row`,
                        // the frame's own response) even though only the
                        // title senses the click — a focus indicator that
                        // only outlined the title text would look like it
                        // was highlighting a search match, not marking
                        // what Enter/Activate will launch.
                        //
                        // WCAG 2.2 non-text contrast (SC 1.4.11): the ring
                        // is `accent` (or, on a pad, `accent_strong`), and
                        // `theme::tests`/`accessibility::tests` already
                        // prove both clear the 3:1 floor against both
                        // `background` and `raised` in every palette.
                        ui.painter().rect_stroke(
                            row.response.rect,
                            2.0,
                            self.focus_ring_stroke(tokens),
                            egui::StrokeKind::Inside,
                        );
                        if moved_by_keyboard {
                            // Scroll only on a keyboard/pad move — a mouse
                            // click already means the row is visible.
                            row.response.scroll_to_me(Some(egui::Align::Center));
                            // Ticket W15-08 acceptance 5: mirror the
                            // keyboard/pad move into egui's own focus, the
                            // same call the mouse-click branch above makes
                            // — so `ctx.memory(|m| m.focused())` equals
                            // this row's id regardless of which device
                            // moved the selection here.
                            if let Some(title_response) = &title_response {
                                title_response.request_focus();
                            }
                        }
                    }
                }
            });
    }

    /// Ticket W15-05, §3: the card grid, an alternative rendering of the
    /// SAME `matches` the row list draws — selection, launch, favourite
    /// and context-menu state all live on `self` and are read/written
    /// identically to `library_rows`, so switching Grid/List mid-session
    /// changes nothing about what is selected or how it behaves.
    ///
    /// `ui.horizontal_wrapped` rather than `egui::Grid`: a `Grid` wants a
    /// fixed column COUNT and this wants a fixed column WIDTH with the
    /// count following from available space (`library_grid_columns`),
    /// which is exactly what a wrapping horizontal layout gives for free.
    fn library_cards(
        &mut self,
        ui: &mut egui::Ui,
        matches: &[&crate::library::LibraryEntry],
        tokens: &crate::theme::Tokens,
        moved_by_keyboard: bool,
        to_play: &mut Option<std::path::PathBuf>,
    ) {
        let accent = tokens.accent;
        let high_contrast = self.settings.accessibility.normalized().high_contrast;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(CARD_SPACING, CARD_SPACING);
                ui.horizontal_wrapped(|ui| {
                    for entry in matches {
                        let is_selected =
                            self.library_selected.as_deref() == Some(entry.path.as_path());
                        let mut card_clicked = false;
                        let mut card_double_clicked = false;
                        let mut card_response: Option<egui::Response> = None;

                        let hash = match &entry.identity {
                            crate::library::EntryIdentity::Recognized {
                                normalized_sha256, ..
                            } => Some(normalized_sha256.clone()),
                            crate::library::EntryIdentity::Unrecognized { .. } => None,
                        };

                        let card = egui::Frame::group(ui.style())
                            .inner_margin(egui::Margin::same(6))
                            .show(ui, |ui| {
                                ui.set_width(CARD_WIDTH);
                                ui.vertical(|ui| {
                                    // `Sense::hover()`: the thumbnail box
                                    // is decoration, not the click
                                    // target. The title label below is
                                    // (same as `library_rows`'s title
                                    // `Label`), which is also what gives
                                    // the card an AccessKit/kittest label
                                    // to query by — a bare painted rect
                                    // carries no accessible name at all.
                                    let (thumb_rect, thumb_response) = ui.allocate_exact_size(
                                        egui::vec2(CARD_WIDTH - 12.0, CARD_THUMB_HEIGHT),
                                        egui::Sense::hover(),
                                    );
                                    let ctx = ui.ctx().clone();
                                    let console = match &entry.identity {
                                        crate::library::EntryIdentity::Recognized {
                                            console,
                                            ..
                                        } => Some(*console),
                                        crate::library::EntryIdentity::Unrecognized { .. } => None,
                                    };
                                    let texture = hash.as_ref().and_then(|h| {
                                        self.library_thumbnail_texture(
                                            &ctx,
                                            h,
                                            &entry.title,
                                            console,
                                        )
                                    });
                                    // Ticket W15-09 acceptance 3: a card
                                    // whose current thumbnail came from a
                                    // libretro-thumbnails fetch gets a
                                    // small network-indicator glyph and an
                                    // attribution line on hover — never
                                    // implying "local" when it wasn't
                                    // (`UX_WAVE_15.md` §4).
                                    let is_fetched = hash.as_ref().is_some_and(|h| {
                                        self.library_thumbnail_is_fetched.contains(h)
                                    });
                                    match texture {
                                        Some(tex) => {
                                            // Letterboxed (acceptance 2):
                                            // fit inside `thumb_rect`
                                            // preserving aspect, never
                                            // stretched or cropped, on a
                                            // filled backdrop so a
                                            // narrower-than-box image
                                            // still reads as intentional.
                                            ui.painter().rect_filled(
                                                thumb_rect,
                                                2.0,
                                                ui.visuals().extreme_bg_color,
                                            );
                                            let img_size = tex.size_vec2();
                                            let scale = (thumb_rect.width() / img_size.x)
                                                .min(thumb_rect.height() / img_size.y);
                                            let draw_size = img_size * scale;
                                            let draw_rect = egui::Rect::from_center_size(
                                                thumb_rect.center(),
                                                draw_size,
                                            );
                                            ui.painter().image(
                                                tex.id(),
                                                draw_rect,
                                                egui::Rect::from_min_max(
                                                    egui::pos2(0.0, 0.0),
                                                    egui::pos2(1.0, 1.0),
                                                ),
                                                egui::Color32::WHITE,
                                            );
                                        }
                                        None => {
                                            // Acceptance 4: a generic,
                                            // console-tinted placeholder —
                                            // never a picture implying an
                                            // image exists when it does
                                            // not. The title is drawn
                                            // below the box either way, so
                                            // the placeholder itself
                                            // carries no text of its own
                                            // that could be mistaken for
                                            // box art.
                                            ui.painter().rect_filled(
                                                thumb_rect,
                                                2.0,
                                                crate::theme::console_tint(
                                                    tokens,
                                                    high_contrast,
                                                    &entry.identity,
                                                ),
                                            );
                                        }
                                    }
                                    if is_fetched {
                                        // A small glyph in the corner,
                                        // distinct from the console-tint
                                        // placeholder and drawn with theme
                                        // tokens (never a bare literal
                                        // colour) — acceptance 3's network
                                        // indicator.
                                        let dot_center =
                                            thumb_rect.right_top() + egui::vec2(-8.0, 8.0);
                                        ui.painter().circle_filled(
                                            dot_center,
                                            4.0,
                                            tokens.accent_strong,
                                        );
                                        thumb_response.on_hover_text(
                                            "Fetched from the internet — Art: libretro-thumbnails",
                                        );
                                    }
                                    let title_response = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&entry.title).strong(),
                                        )
                                        .sense(egui::Sense::click())
                                        .truncate(),
                                    );
                                    card_clicked = title_response.clicked();
                                    card_double_clicked = title_response.double_clicked();
                                    card_response = Some(title_response);

                                    ui.horizontal(|ui| {
                                        match &entry.identity {
                                            crate::library::EntryIdentity::Recognized {
                                                console,
                                                normalized_sha256,
                                            } => {
                                                ui.add(readout(
                                                    egui::RichText::new(console.name())
                                                        .small()
                                                        .weak(),
                                                ));
                                                let favourite = self
                                                    .library_meta
                                                    .get(normalized_sha256)
                                                    .is_some_and(|m| m.favourite);
                                                // W20-05: filled when on, outline when off —
                                                // shape, not colour alone.
                                                let star = if favourite {
                                                    crate::icons::filled(crate::icons::FAVOURITE)
                                                } else {
                                                    egui::RichText::new(crate::icons::FAVOURITE)
                                                };
                                                let label = if favourite {
                                                    "Unfavourite"
                                                } else {
                                                    "Favourite"
                                                };
                                                if crate::icons::icon_button(ui, star, label)
                                                    .clicked()
                                                {
                                                    self.toggle_favourite(normalized_sha256);
                                                }
                                                // §3.1's badges: profile
                                                // matched / has states /
                                                // enhanced settings on.
                                                if self
                                                    .library_badges
                                                    .profile_matched
                                                    .contains(normalized_sha256)
                                                {
                                                    crate::icons::icon_label(
                                                        ui,
                                                        crate::icons::PROFILE,
                                                        "Profile matched",
                                                    );
                                                }
                                                if self
                                                    .library_badges
                                                    .has_states
                                                    .contains(normalized_sha256)
                                                {
                                                    crate::icons::icon_label(
                                                        ui,
                                                        crate::icons::HAS_STATES,
                                                        "Has save states",
                                                    );
                                                }
                                                if self
                                                    .library_badges
                                                    .enhanced
                                                    .contains(normalized_sha256)
                                                {
                                                    crate::icons::icon_label(
                                                        ui,
                                                        crate::icons::ENHANCED_SET,
                                                        "Enhanced settings on",
                                                    );
                                                }
                                            }
                                            crate::library::EntryIdentity::Unrecognized {
                                                reason,
                                            } => {
                                                ui.add(readout(
                                                    egui::RichText::new("unrecognized")
                                                        .small()
                                                        .weak(),
                                                ))
                                                .on_hover_text(reason);
                                            }
                                        }
                                    });
                                });
                            });

                        if card_double_clicked {
                            *to_play = Some(entry.path.clone());
                        } else if card_clicked {
                            self.library_selected = Some(entry.path.clone());
                            // Ticket W15-08 acceptance 5: see the matching
                            // comment in `library_rows` — the click
                            // becomes egui's real focus too, so AccessKit
                            // never disagrees with `library_selected`.
                            if let Some(card_response) = &card_response {
                                card_response.request_focus();
                            }
                        }

                        if let Some(card_response) = &card_response {
                            card_response.context_menu(|ui| {
                                self.library_context_menu_body(ui, entry, &mut *to_play);
                            });
                            if is_selected && self.library_context_menu_open {
                                let mut open = true;
                                let popup = egui::Popup::from_response(card_response)
                                    .id(card_response.id.with("pad_context_menu"))
                                    .open_bool(&mut open);
                                if popup.is_open() {
                                    popup.show(|ui| {
                                        self.library_context_menu_body(ui, entry, &mut *to_play);
                                    });
                                }
                                self.library_context_menu_open = open;
                            }
                        }

                        // Ticket W15-07 acceptance 4: hover elevation on
                        // library cards, the grid's version of the Run
                        // button's `animate_bool_responsive` hover — the
                        // ONLY other place this crate hand-animates
                        // anything. Keyed on the entry's own path (not a
                        // shared id) so hovering one card does not also
                        // animate every other card sharing the id. The
                        // card's `Frame::show` allocates the rect with
                        // hover sense already (`egui::Ui::allocate_rect`'s
                        // default), so `ui.interact` here reads that same
                        // interaction rather than creating a second,
                        // competing sense.
                        let hover_id = egui::Id::new("rf_card_hover").with(&entry.path);
                        let hovered = ui
                            .interact(card.response.rect, hover_id, egui::Sense::hover())
                            .hovered();
                        let warmth = ui.ctx().animate_bool_responsive(hover_id, hovered);
                        if warmth > 0.0 {
                            ui.painter().rect_stroke(
                                card.response.rect,
                                tokens.radius_md,
                                egui::Stroke::new(
                                    1.0 + warmth,
                                    crate::theme::mix(tokens.line, accent, 0.6 * warmth),
                                ),
                                egui::StrokeKind::Outside,
                            );
                        }

                        if is_selected {
                            ui.painter().rect_stroke(
                                card.response.rect,
                                tokens.radius_sm,
                                self.focus_ring_stroke(tokens),
                                egui::StrokeKind::Inside,
                            );
                            if moved_by_keyboard {
                                card.response.scroll_to_me(Some(egui::Align::Center));
                                // Ticket W15-08 acceptance 5: mirror a
                                // keyboard/pad selection move into egui's
                                // real focus — same reasoning as
                                // `library_rows`'s matching branch.
                                if let Some(card_response) = &card_response {
                                    card_response.request_focus();
                                }
                            }
                        }
                    }
                });
            });
    }

    /// Ticket W15-03 (`UX_WAVE_15.md` §3): the per-row context menu's
    /// contents, shared verbatim by the mouse (`Response::context_menu`,
    /// right-click) and pad (`Start` on the focused row) call sites in
    /// `library_grid`.
    fn library_context_menu_body(
        &mut self,
        ui: &mut egui::Ui,
        entry: &crate::library::LibraryEntry,
        to_play: &mut Option<std::path::PathBuf>,
    ) {
        if ui.button("Play").clicked() {
            *to_play = Some(entry.path.clone());
            ui.close();
        }
        ui.menu_button("Play in mode", |ui| {
            for option in crate::game_settings::Mode::all() {
                if ui.button(option.display_name()).clicked() {
                    // Only a Recognized entry has a hash to persist the
                    // mode against — an unrecognized ROM still launches,
                    // it just cannot remember which mode it launched in.
                    if let crate::library::EntryIdentity::Recognized {
                        normalized_sha256, ..
                    } = &entry.identity
                    {
                        if let Some(root) = self.config_root.clone() {
                            let mut settings = crate::game_settings::load(&root, normalized_sha256);
                            settings.mode = option;
                            match crate::game_settings::save(&root, normalized_sha256, &settings) {
                                Ok(_) => {
                                    self.library_meta.insert(
                                        normalized_sha256.clone(),
                                        crate::library::RecencyMeta {
                                            last_played_epoch_secs: settings.last_played_epoch_secs,
                                            play_count: settings.play_count,
                                            favourite: settings.favourite,
                                        },
                                    );
                                }
                                Err(e) => {
                                    self.status = format!("Could not save game settings: {e}");
                                }
                            }
                        }
                    }
                    *to_play = Some(entry.path.clone());
                    ui.close();
                }
            }
        });
        match &entry.identity {
            crate::library::EntryIdentity::Recognized {
                normalized_sha256, ..
            } => {
                let favourite = self
                    .library_meta
                    .get(normalized_sha256)
                    .is_some_and(|m| m.favourite);
                if ui
                    .button(if favourite {
                        "Unfavourite"
                    } else {
                        "Favourite"
                    })
                    .clicked()
                {
                    self.toggle_favourite(normalized_sha256);
                    ui.close();
                }
                if ui.button("Game settings").clicked() {
                    let settings = self
                        .config_root
                        .clone()
                        .map(|root| crate::game_settings::load(&root, normalized_sha256))
                        .unwrap_or_default();
                    self.game_settings_target = Some(GameSettingsTarget {
                        hash: normalized_sha256.clone(),
                        title: entry.title.clone(),
                        settings,
                    });
                    self.show_game_settings = true;
                    ui.close();
                }
            }
            crate::library::EntryIdentity::Unrecognized { .. } => {
                // Neither control has a hash to key by — disabled rather
                // than absent, so the menu shape stays the same for every
                // row and the reason is a tooltip away.
                ui.add_enabled(false, egui::Button::new("Favourite"))
                    .on_disabled_hover_text("Unrecognized ROM: no hash to key a favourite by");
                ui.add_enabled(false, egui::Button::new("Game settings"))
                    .on_disabled_hover_text("Unrecognized ROM: no hash to key settings by");
            }
        }
        if ui.button(Self::reveal_menu_label()).clicked() {
            crate::reveal::reveal_in_file_manager(&entry.path);
            ui.close();
        }
        if ui.button("Hash info").clicked() {
            self.open_hash_info(entry);
            ui.close();
        }
    }

    /// Platform-appropriate label for the "reveal in file manager" item
    /// (`UX_WAVE_15.md` §3's "Show in Finder/Explorer").
    const fn reveal_menu_label() -> &'static str {
        if cfg!(target_os = "macos") {
            "Show in Finder"
        } else if cfg!(target_os = "windows") {
            "Show in Explorer"
        } else {
            "Show in file manager"
        }
    }

    /// Ticket W15-03: compute the "Hash info" popup content for one library
    /// entry. Re-reads and re-hashes the file rather than trusting
    /// `entry.identity`'s single normalized sha256 — the popup's whole
    /// point is to show the FULL family (crc32/md5/sha1/sha256) `rf-cart`
    /// computes, which `LibraryEntry` does not carry (`library.rs`'s
    /// `EntryIdentity::Recognized` keeps only the one hash profiles key on).
    fn open_hash_info(&mut self, entry: &crate::library::LibraryEntry) {
        let bytes = match rom_open::load_rom_bytes(&entry.path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.hash_info = Some(HashInfoPopup {
                    title: entry.title.clone(),
                    hashes: None,
                    profile: None,
                    error: Some(format!("Could not read ROM: {e}")),
                });
                return;
            }
        };
        let (hashes, profile) = match rf_cart::Cartridge::load(&bytes) {
            Ok(
                rf_cart::Cartridge::Nes { identity, .. }
                | rf_cart::Cartridge::Snes { identity, .. },
            ) => {
                let profile = crate::level_view::find_matching_profile(
                    &Self::profiles_root(),
                    &identity.normalized,
                )
                .map(|(_, path)| path);
                (Some(identity.normalized), profile)
            }
            Err(_) => (None, None),
        };
        self.hash_info = Some(HashInfoPopup {
            title: entry.title.clone(),
            hashes,
            profile,
            error: None,
        });
    }

    /// The "Hash info" popup window (ticket W15-03).
    fn hash_info_window(&mut self, ctx: &egui::Context) {
        let Some(info) = &self.hash_info else {
            return;
        };
        let mut open = true;
        egui::Window::new(format!("Hash info \u{2014} {}", info.title))
            .id(egui::Id::new("hash_info_window"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                if let Some(e) = &info.error {
                    ui.label(e);
                } else if let Some(h) = &info.hashes {
                    ui.monospace(format!("sha256: {}", h.sha256));
                    ui.monospace(format!("sha1:   {}", h.sha1));
                    ui.monospace(format!("md5:    {}", h.md5));
                    ui.monospace(format!("crc32:  {}", h.crc32));
                    ui.separator();
                    match &info.profile {
                        Some(p) => {
                            ui.label(format!("Profile match: {}", p.display()));
                        }
                        None => {
                            ui.label("Profile match: none");
                        }
                    }
                } else {
                    ui.label(
                        "Not a recognized cartridge \u{2014} no normalized hash family available.",
                    );
                }
            });
        if !open {
            self.hash_info = None;
        }
    }

    /// Open a folder picker and add what it returns to the roots.
    /// `true` if a rescan is now owed.
    fn pick_library_folder(&mut self, ctx: &egui::Context) -> bool {
        let Some(folder) = rfd::FileDialog::new().pick_folder() else {
            return false;
        };
        self.library_roots
            .push(crate::library::LibraryRoot::Bare(folder));
        self.save_library_roots();
        // Ticket W15-04: "ROM folder added" toast. The rescan this
        // triggers (the caller sets `rescan = true` and `library_home`
        // calls `self.rescan_library()`) gets its OWN toast only when the
        // user pressed Rescan directly — see
        // `library_rescan_toast_pending`'s doc comment for why adding a
        // folder does not also fire "library rescanned".
        self.toasts
            .push(crate::toast::ToastKind::Info, "ROM folder added", ctx);
        true
    }

    /// Flip the favourite star for one game, by hash (ticket W15-02, §3).
    /// Loads that game's OWN settings file rather than trusting
    /// `library_meta`'s cache for the write — the cache is a read-side
    /// convenience (`library_meta`'s own doc), and starting the flip from
    /// a stale in-memory copy risks clobbering a field this session never
    /// loaded. Updates the cache afterwards so the star repaints this
    /// frame without a rescan.
    fn toggle_favourite(&mut self, normalized_sha256: &str) {
        let Some(root) = self.config_root.clone() else {
            return;
        };
        let mut settings = crate::game_settings::load(&root, normalized_sha256);
        settings.favourite = !settings.favourite;
        if let Err(e) = crate::game_settings::save(&root, normalized_sha256, &settings) {
            self.status = format!("Could not save game settings: {e}");
            return;
        }
        self.library_meta.insert(
            normalized_sha256.to_string(),
            crate::library::RecencyMeta {
                last_played_epoch_secs: settings.last_played_epoch_secs,
                play_count: settings.play_count,
                favourite: settings.favourite,
            },
        );
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

    /// Paint the script's overlay commands over the video rect.
    ///
    /// Coordinates are in EMULATED pixels — the script thinks in the
    /// game's own space, which is the only space its memory reads mean
    /// anything in — so they are scaled to wherever the frame landed on
    /// screen. Colours come from the NES palette by index, the same
    /// mapping the PPU output uses, so a script cannot name a colour the
    /// hardware could not show.
    fn draw_script_overlay(&self, ui: &egui::Ui, video: egui::Rect) {
        if self.script_overlay.is_empty() {
            return;
        }
        let Some(texture) = &self.texture else { return };
        let size = texture.size();
        let (fw, fh) = (size[0] as f32, size[1] as f32);
        if fw <= 0.0 || fh <= 0.0 {
            return;
        }
        let sx = video.width() / fw;
        let sy = video.height() / fh;
        let map = |x: i32, y: i32| {
            egui::pos2(
                video.left() + (x as f32) * sx,
                video.top() + (y as f32) * sy,
            )
        };
        let colour = |index: u8| {
            let [r, g, b] = rf_renderer::palette_index_to_rgb(index);
            egui::Color32::from_rgb(r, g, b)
        };
        let painter = ui.painter_at(video);
        for cmd in &self.script_overlay {
            match *cmd {
                rf_plugin_sdk::sandbox::OverlayCmd::Rect {
                    x,
                    y,
                    width,
                    height,
                    color_index,
                } => {
                    let rect = egui::Rect::from_min_max(
                        map(x, y),
                        map(x + i32::from(width), y + i32::from(height)),
                    );
                    painter.rect_stroke(
                        rect,
                        0.0,
                        egui::Stroke::new(1.5, colour(color_index)),
                        egui::StrokeKind::Middle,
                    );
                }
                rf_plugin_sdk::sandbox::OverlayCmd::Line {
                    x0,
                    y0,
                    x1,
                    y1,
                    color_index,
                } => {
                    painter.line_segment(
                        [map(x0, y0), map(x1, y1)],
                        egui::Stroke::new(1.5, colour(color_index)),
                    );
                }
            }
        }
    }

    /// Load a Lua overlay script from a plugin directory (ticket
    /// W11-04).
    ///
    /// Takes the DIRECTORY, not the script: a plugin is its manifest plus
    /// its source, and `plugin.toml` is what declares the capabilities
    /// the sandbox will grant. Loading a bare `.lua` would mean the shell
    /// inventing a capability set, which is precisely the decision
    /// FR-PLUG-002 says belongs to the plugin author and the user.
    fn load_script(&mut self, dir: &std::path::Path) {
        let manifest_path = dir.join("plugin.toml");
        let source_path = dir.join("main.lua");
        let (manifest_src, source) = match (
            std::fs::read_to_string(&manifest_path),
            std::fs::read_to_string(&source_path),
        ) {
            (Ok(m), Ok(s)) => (m, s),
            _ => {
                let msg = format!("{} needs both plugin.toml and main.lua", dir.display());
                self.script_error_toast_pending = Some(msg.clone());
                self.script_status = Some(msg);
                return;
            }
        };
        let manifest = match rf_plugin_sdk::Manifest::parse(&manifest_src) {
            Ok(m) => m,
            Err(e) => {
                let msg = format!("manifest: {e}");
                self.script_error_toast_pending = Some(msg.clone());
                self.script_status = Some(msg);
                return;
            }
        };
        let bridge = rf_plugin_sdk::sandbox::Bridge::default();
        // Labels BEFORE load: a script resolving `rf.profile.addr` once
        // at load time is the natural way to write one, so the labels
        // have to already be there (`ScriptHost::load_with_bridge`'s own
        // doc, and lua_overlay_demo.rs learned it the same way).
        bridge.publish(
            rf_plugin_sdk::sandbox::MemoryWindow::default(),
            self.profile_labels(),
        );
        match rf_plugin_sdk::ScriptHost::load_with_bridge(
            manifest,
            &source,
            rf_plugin_sdk::Budget::default(),
            bridge,
        ) {
            Ok(host) => {
                self.script_host = Some(host);
                self.script_overlay.clear();
                self.script_status = Some(format!("Loaded {}", dir.display()));
                self.send_command(CoreCommand::SetScriptWindow(Some((
                    SCRIPT_WINDOW_BASE,
                    SCRIPT_WINDOW_LEN,
                ))));
            }
            Err(e) => {
                // Named, never swallowed: a script that failed to load
                // and one that drew nothing look identical on screen.
                let msg = format!("script refused to load: {e}");
                self.script_error_toast_pending = Some(msg.clone());
                self.script_status = Some(msg);
            }
        }
    }

    /// Unload the running script and stop the core peeking for it.
    fn unload_script(&mut self) {
        self.script_host = None;
        self.script_overlay.clear();
        self.script_status = Some("No script loaded".to_string());
        self.send_command(CoreCommand::SetScriptWindow(None));
    }

    /// The matched profile's published addresses, which is what
    /// FR-PLUG-001's "using profile-published labels" means.
    fn profile_labels(&self) -> std::collections::BTreeMap<String, u32> {
        self.level_session
            .as_ref()
            .map(|s| {
                s.profile
                    .memory_map
                    .iter()
                    .map(|e| (e.label.clone(), e.addr))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where game profiles live.
    ///
    /// **This was `Path::new("profiles")` until W11-02 — a path relative
    /// to the process working directory.** It resolved only when the app
    /// happened to be launched from the repository root, so for anyone
    /// running the built binary from anywhere else NO PROFILE EVER
    /// MATCHED: the status bar's chip always read "no profile", and every
    /// profile-gated feature row stayed disabled saying "requires
    /// profile". It looked exactly like correct honest behaviour, which
    /// is why it survived — including in this session's own screenshot
    /// tour, where I read "no profile" against a ROM that has one and
    /// took it for the honesty contract working.
    ///
    /// Resolution order, most specific first:
    /// 1. `RETROFORGE_PROFILES_DIR`, for tests, portable installs and
    ///    anyone with an opinion — the same escape hatch
    ///    `RETROFORGE_CONFIG_DIR` provides for config.
    /// 2. `profiles/` beside the executable, which is where an installed
    ///    build's data sits.
    /// 3. `profiles/` under the working directory, which is what a
    ///    `cargo run` from the repository root gets.
    ///
    /// Returns the first that exists, and the last as a fallback so the
    /// failure is a profile that does not match rather than a panic.
    fn profiles_root() -> std::path::PathBuf {
        if let Some(dir) = std::env::var_os("RETROFORGE_PROFILES_DIR") {
            return std::path::PathBuf::from(dir);
        }
        if let Some(beside) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|p| p.join("profiles")))
            .filter(|p| p.is_dir())
        {
            return beside;
        }
        std::path::PathBuf::from("profiles")
    }

    /// The addresses the profile's `[camera]` spec will be read at.
    ///
    /// Derived from the profile rather than guessed, and bounded: at most
    /// four bytes (`x` and `y`, each one or two bytes little-endian). A
    /// probe that shipped a RAM page instead would work and would be a
    /// standing invitation for the UI to start reading things nobody
    /// declared.
    fn probe_addrs(session: &crate::level_view::LevelSession) -> Vec<u32> {
        let mut addrs = Vec::new();
        if let Some(camera) = session.profile.camera.as_ref() {
            for axis in [camera.x.as_ref(), camera.y.as_ref()].into_iter().flatten() {
                addrs.push(axis.addr);
                if axis.ty == "u16" {
                    addrs.push(axis.addr + 1);
                }
            }
        }
        addrs
    }

    /// Arm or disarm the level probe (ticket W11-02).
    fn set_level_probe(&mut self, on: bool) {
        let probe = on
            .then_some(self.level_session.as_ref())
            .flatten()
            .map(|session| {
                let (table_addr, table_len) = session.entity_table_range().unwrap_or((0, 0));
                core_thread::LevelProbe {
                    addrs: Self::probe_addrs(session),
                    table_addr,
                    table_len,
                }
            });
        // `None` when the feature is off OR no level decoded, so the core
        // stops peeking either way.
        self.send_command(CoreCommand::SetLevelProbe(probe));
        if !on {
            self.level_camera = None;
        }
    }

    /// The decoded level as a texture, built once per ROM.
    fn level_texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if let Some(tex) = &self.level_texture {
            return Some(tex.clone());
        }
        let session = self.level_session.as_ref()?;
        let chr = self.debug_panels.data.chr_rom.as_deref()?;
        let rgba = enhanced_view::render_level_rgba(
            &session.level,
            chr,
            rf_debugger::pattern::PatternTable::Left,
            LEVEL_PALETTE,
            session.geometry,
        );
        let (w, h) = (
            session.geometry.width_px as usize,
            session.geometry.height_px as usize,
        );
        if w == 0 || h == 0 || rgba.len() != w * h * 4 {
            return None;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
        let tex = ctx.load_texture("decoded-level", image, egui::TextureOptions::NEAREST);
        self.level_texture = Some(tex.clone());
        Some(tex)
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
        // Ticket W20-10: Quit to library closes the Quick Menu with it.
        self.show_overlay_menu = false;
        self.menu_paused_game = false;
        self.current_rom_path = None;
        self.send_command(CoreCommand::Shutdown);
        self.core = None;
        self.texture = None;
        self.bg_layer_texture = None;
        self.sprite_layer_texture = None;
        self.ultrawide_texture = None;
        self.ultrawide_render = None;
        // Ticket W16-13: same reasoning as the ROM-load reset above.
        self.diorama_render = None;
        self.diorama_texture = None;
        self.diorama_ground_rgba = None;
        self.diorama_wanted = false;
        self.running = false;
        self.position = None;
        self.fps = None;
        self.fps_frames = 0;
        self.audio_fill = None;
        self.crash = None;
        self.status = "No ROM loaded".to_string();
    }

    /// Rescan the configured roots, on a worker thread (ticket W14-02).
    ///
    /// Returns immediately. [`Self::poll_library_scan`] picks the result
    /// up; until then the previous library keeps showing, because a grid
    /// that empties itself while refreshing reads as "your games are
    /// gone".
    ///
    /// A scan already in flight is left alone rather than joined by a
    /// second one: `library_home` asks for a scan whenever it has no
    /// library, which is every frame until one arrives, and spawning a
    /// thread per frame over a 6 GB collection is its own outage.
    fn rescan_library(&mut self) {
        if self.library_scan.is_some() {
            return;
        }
        self.library_scans += 1;
        let roots = self.library_roots.clone();
        let config_root = crate::bindings_store::config_root();
        let (tx, rx) = std::sync::mpsc::channel();
        // Detached deliberately: nothing waits for this thread, and if the
        // receiver is gone because the app closed, `send` fails and the
        // thread ends. A scan holds no lock and owns its inputs.
        std::thread::Builder::new()
            .name("library-scan".to_string())
            .spawn(move || {
                let library = Self::scan_with_cache(&roots, config_root.as_deref());
                let _ = tx.send(library);
            })
            .map_or_else(
                |_| {
                    // A machine that cannot spawn a thread still deserves a
                    // library; do it here rather than show nothing for ever.
                    let config_root = crate::bindings_store::config_root();
                    let library =
                        Self::scan_with_cache(&self.library_roots, config_root.as_deref());
                    self.library_meta = Self::load_library_meta(&library, config_root.as_deref());
                    self.library_badges = Self::load_badges(&library, config_root.as_deref());
                    self.library = Some(library);
                },
                |_handle| {
                    self.library_scan = Some(rx);
                },
            );
    }

    /// Ticket W15-02: this scan's play history, keyed by normalized hash —
    /// read once here rather than once per row per frame in `library_grid`
    /// (UX_WAVE_15 §11 acceptance 1). Two entries that share a hash (a
    /// bare file beside its `.zip`, `library::scan`'s own doc on why those
    /// are not deduped) read the same settings file, so the map is keyed
    /// by hash rather than by entry.
    fn load_library_meta(
        library: &crate::library::Library,
        config_root: Option<&std::path::Path>,
    ) -> std::collections::BTreeMap<String, crate::library::RecencyMeta> {
        let Some(root) = config_root else {
            return std::collections::BTreeMap::new();
        };
        let mut meta = std::collections::BTreeMap::new();
        for entry in &library.entries {
            if let crate::library::EntryIdentity::Recognized {
                normalized_sha256, ..
            } = &entry.identity
            {
                if meta.contains_key(normalized_sha256) {
                    continue;
                }
                let settings = crate::game_settings::load(root, normalized_sha256);
                meta.insert(
                    normalized_sha256.clone(),
                    crate::library::RecencyMeta {
                        last_played_epoch_secs: settings.last_played_epoch_secs,
                        play_count: settings.play_count,
                        favourite: settings.favourite,
                    },
                );
            }
        }
        meta
    }

    /// Ticket W15-05: the card grid's extra badges (`LibraryBadges`),
    /// computed once per scan for the same "cheap enough per row" reason
    /// [`Self::load_library_meta`] is. Profile matching is looked up ONCE
    /// (`crate::level_view::all_profile_sha256s` walks the profiles
    /// directory) and then checked per-hash in memory, rather than
    /// re-walking the directory per entry.
    fn load_badges(
        library: &crate::library::Library,
        config_root: Option<&std::path::Path>,
    ) -> LibraryBadges {
        let profile_sha256s = crate::level_view::all_profile_sha256s(&Self::profiles_root());
        let mut badges = LibraryBadges::default();
        let Some(root) = config_root else {
            return badges;
        };
        for entry in &library.entries {
            if let crate::library::EntryIdentity::Recognized {
                normalized_sha256, ..
            } = &entry.identity
            {
                if profile_sha256s.contains(normalized_sha256) {
                    badges.profile_matched.insert(normalized_sha256.clone());
                }
                if badges.has_states.contains(normalized_sha256)
                    && badges.enhanced.contains(normalized_sha256)
                {
                    continue; // already known both ways for this hash
                }
                let states_dir = crate::state_slots::slots_dir(root, normalized_sha256);
                if crate::state_slots::scan(&states_dir)
                    .iter()
                    .any(|s| s.saved.is_some())
                {
                    badges.has_states.insert(normalized_sha256.clone());
                }
                let settings = crate::game_settings::load(root, normalized_sha256);
                if settings.mode.enhancement_active() {
                    badges.enhanced.insert(normalized_sha256.clone());
                }
            }
        }
        badges
    }

    /// Ticket W15-05 acceptance 1: capture the first rendered frame once
    /// per ROM hash, on the first frame the CURRENTLY RUNNING game
    /// produces that is not a flat colour (`crate::thumbnail::
    /// frame_is_non_uniform` — never frame 0's black). A no-op with no
    /// loaded ROM, no thumbnail cache, or a hash already captured.
    fn maybe_capture_first_frame(&mut self, rgba: &[u8], width: usize, height: usize) {
        let Some(hash) = self.current_game_hash.clone() else {
            return;
        };
        let Some(cache) = self.thumbnail_cache.as_mut() else {
            return;
        };
        if crate::thumbnail::has_first_frame(cache, &hash) {
            return;
        }
        if !crate::thumbnail::frame_is_non_uniform(rgba) {
            return;
        }
        let Ok(w) = u32::try_from(width) else { return };
        let Ok(h) = u32::try_from(height) else { return };
        let png = rf_renderer::png::encode_rgba(rgba, w, h);
        if crate::thumbnail::put_first_frame(cache, &hash, &png).is_ok() {
            // The grid may already have cached "no thumbnail" for this
            // hash from before this capture landed — drop it so the next
            // frame the grid draws re-resolves and picks the new capture
            // up (§4's priority order still runs; a save-state screenshot
            // taken since would still win, correctly).
            self.library_thumbnail_textures.remove(&hash);
        }
    }

    /// Ticket W15-05, §4: the most recent OCCUPIED save-state slot's
    /// screenshot bytes for `rom_sha256`, or `None` if there is no config
    /// root, no slots directory, or no slot carries a thumbnail yet.
    fn latest_save_state_screenshot_bytes(&self, rom_sha256: &str) -> Option<Vec<u8>> {
        let root = self.config_root.as_ref()?;
        let dir = crate::state_slots::slots_dir(root, rom_sha256);
        let best = crate::state_slots::scan(&dir)
            .into_iter()
            .filter_map(|slot| slot.saved)
            .filter(|saved| saved.thumbnail.is_some())
            .max_by_key(|saved| saved.timestamp)?;
        std::fs::read(best.thumbnail.expect("filtered on is_some above")).ok()
    }

    /// Ticket W15-05/W15-09, §4: resolve the winning thumbnail source's
    /// PNG bytes for one library entry, per
    /// `crate::thumbnail::thumbnail_source_with_fetch`'s priority order
    /// (save-state screenshot, first-frame, user art folder, then
    /// fetched art). Returns the bytes plus whether they came from a
    /// fetch — the second half is what lets the caller draw the network
    /// indicator (acceptance 3). `(None, false)` means no thumbnail from
    /// any of the four sources — the caller draws the placeholder card.
    ///
    /// As a side effect, when nothing local exists and the "Fetch box art
    /// from the internet" toggle is on, this queues a background fetch
    /// (`Self::request_art_fetch`) for next frame to pick up — never
    /// blocking THIS frame on network I/O.
    fn resolve_thumbnail_png(
        &mut self,
        rom_sha256: &str,
        title: &str,
        console: Option<crate::library::Console>,
    ) -> (Option<Vec<u8>>, bool) {
        let save_state_png = self.latest_save_state_screenshot_bytes(rom_sha256);
        let first_frame_png = self.thumbnail_cache.as_mut().and_then(|cache| {
            crate::thumbnail::get_first_frame(cache, rom_sha256)
                .ok()
                .flatten()
        });
        let user_art_png = self
            .settings
            .paths
            .art_folder
            .as_ref()
            .and_then(|folder| crate::thumbnail::find_user_art(folder, title))
            .and_then(|path| std::fs::read(path).ok());
        let fetched_png = self.art_cache.as_mut().and_then(|cache| {
            crate::art::get_fetched_art(cache, rom_sha256)
                .ok()
                .flatten()
        });

        let has_local =
            save_state_png.is_some() || first_frame_png.is_some() || user_art_png.is_some();
        if fetched_png.is_none()
            && crate::art::should_fetch(self.settings.paths.fetch_art, has_local)
        {
            if let Some(console) = console {
                self.request_art_fetch(rom_sha256, title, console);
            }
        }

        match crate::thumbnail::thumbnail_source_with_fetch(
            save_state_png.is_some(),
            first_frame_png.is_some(),
            user_art_png.is_some(),
            fetched_png.is_some(),
        ) {
            Some(crate::thumbnail::ThumbnailSource::SaveState) => (save_state_png, false),
            Some(crate::thumbnail::ThumbnailSource::FirstFrame) => (first_frame_png, false),
            Some(crate::thumbnail::ThumbnailSource::UserArt) => (user_art_png, false),
            Some(crate::thumbnail::ThumbnailSource::Fetched) => (fetched_png, true),
            None => (None, false),
        }
    }

    /// Ticket W15-09: queue a background fetch for `rom_sha256`, spawning
    /// the fetch worker on first use (module doc on the `art_fetcher`
    /// field). A no-op with no config root — a fetched image with
    /// nowhere to cache it would just be re-fetched every frame it is on
    /// screen, and this is strictly a "nice to have" enhancement over the
    /// three local sources, never worth that.
    fn request_art_fetch(
        &mut self,
        rom_sha256: &str,
        title: &str,
        console: crate::library::Console,
    ) {
        if self.config_root.is_none() {
            return;
        }
        if self.art_fetcher.is_none() {
            // Same waker pattern as `spawn_with_waker`'s call site (ticket
            // W14-20 defect 2): wake the UI thread directly when a result
            // lands, rather than relying solely on the ordinary redraw
            // cadence, since the library screen can sit idle for a while
            // between input events.
            let ctx_for_waker = self.ctx.clone();
            let waker: Arc<dyn Fn() + Send + Sync> =
                Arc::new(move || ctx_for_waker.request_repaint());
            self.art_fetcher = Some(crate::art::ArtFetcher::spawn(
                Arc::clone(&self.art_client),
                Some(waker),
            ));
        }
        let url = crate::art::thumbnail_url(crate::art::system_name(console), title);
        if let Some(fetcher) = self.art_fetcher.as_mut() {
            fetcher.request(rom_sha256, &url);
        }
    }

    /// Ticket W15-09: adopt whatever fetch results have arrived since the
    /// last poll — cache successes, drop the stale "no thumbnail" texture
    /// entry so the grid re-resolves and picks the new art up, and toast
    /// at most once per session on any failure (acceptance 4).
    fn poll_art_fetch(&mut self, ctx: &egui::Context) {
        let Some(fetcher) = self.art_fetcher.as_mut() else {
            return;
        };
        let results = fetcher.poll();
        if results.is_empty() {
            return;
        }
        let mut any_failure = false;
        for result in results {
            match result.outcome {
                Ok(bytes) => {
                    if let Some(cache) = self.art_cache.as_mut() {
                        let _ = crate::art::put_fetched_art(cache, &result.rom_sha256, &bytes);
                    }
                    self.library_thumbnail_textures.remove(&result.rom_sha256);
                }
                Err(_) => any_failure = true,
            }
        }
        if any_failure && !self.art_fetch_failure_toast_shown {
            self.art_fetch_failure_toast_shown = true;
            self.toasts.push(
                crate::toast::ToastKind::Error,
                "Box art fetch failed, will retry next launch",
                ctx,
            );
        }
    }

    /// Ticket W15-05: the card grid's thumbnail texture for one hash,
    /// resolved and decoded at most once per session per hash
    /// (`library_thumbnail_textures`'s own doc explains why `None` is
    /// cached too). Decoding lives HERE rather than in `crate::thumbnail`
    /// because this is the one place that already owns an `egui::Context`
    /// to load a texture into (module doc on `crate::thumbnail`).
    fn library_thumbnail_texture(
        &mut self,
        ctx: &egui::Context,
        rom_sha256: &str,
        title: &str,
        console: Option<crate::library::Console>,
    ) -> Option<egui::TextureHandle> {
        if let Some(cached) = self.library_thumbnail_textures.get(rom_sha256) {
            return cached.clone();
        }
        let (png, is_fetched) = self.resolve_thumbnail_png(rom_sha256, title, console);
        let texture = png.and_then(|png| {
            let decoded = image::load_from_memory(&png).ok()?.to_rgba8();
            let (w, h) = (decoded.width() as usize, decoded.height() as usize);
            if w == 0 || h == 0 {
                return None;
            }
            let color = egui::ColorImage::from_rgba_unmultiplied([w, h], decoded.as_raw());
            Some(ctx.load_texture(
                format!("library-thumb-{rom_sha256}"),
                color,
                egui::TextureOptions::LINEAR,
            ))
        });
        if texture.is_some() && is_fetched {
            self.library_thumbnail_is_fetched
                .insert(rom_sha256.to_string());
        } else {
            self.library_thumbnail_is_fetched.remove(rom_sha256);
        }
        self.library_thumbnail_textures
            .insert(rom_sha256.to_string(), texture.clone());
        texture
    }

    /// Scan, reusing the on-disk cache and writing back anything new
    /// (ticket W14-02). Pure: no `&self`, so it can run on a worker.
    fn scan_with_cache(
        roots: &[crate::library::LibraryRoot],
        config_root: Option<&std::path::Path>,
    ) -> crate::library::Library {
        let mut cache = config_root
            .map(crate::library_cache::LibraryCache::load)
            .unwrap_or_default();
        let (library, learned) = crate::library::scan_cached(roots, &mut cache);
        // Only rewrite when something was actually read, so an unchanged
        // library does not rewrite a multi-thousand-entry file on every
        // launch. Failing to save is cosmetic: it costs the next scan its
        // speed, never its correctness.
        if learned {
            if let Some(root) = config_root {
                let _ = cache.save(root);
            }
        }
        library
    }

    /// Adopt a finished background scan, if one has finished.
    fn poll_library_scan(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.library_scan.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(library) => {
                // Ticket W15-04: "library rescanned (with count)" —
                // only for a scan the Rescan button asked for; the
                // initial automatic scan on boot (`library_home`) never
                // sets this flag, so it stays silent as it always was.
                if self.library_rescan_toast_pending {
                    self.library_rescan_toast_pending = false;
                    self.toasts.push(
                        crate::toast::ToastKind::Info,
                        format!("Library rescanned ({} game(s))", library.entries.len()),
                        ctx,
                    );
                }
                self.library_meta = Self::load_library_meta(
                    &library,
                    crate::bindings_store::config_root().as_deref(),
                );
                self.library_badges =
                    Self::load_badges(&library, crate::bindings_store::config_root().as_deref());
                self.library = Some(library);
                self.library_scan = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            // The worker died without sending. Drop the receiver so a
            // later rescan can start; leaving it would wedge the library
            // for the rest of the session.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.library_scan = None;
            }
        }
    }

    /// Whether a scan is running right now (ticket W14-02).
    #[must_use]
    fn library_scan_in_flight(&self) -> bool {
        self.library_scan.is_some()
    }

    /// Point the library at these roots and force a rescan (ticket
    /// W10-03), so `tests/library_home.rs` can drive §3.1's three
    /// first-run states without a folder-picker dialog no headless
    /// harness can operate.
    #[doc(hidden)]
    pub fn set_library_roots_for_test(&mut self, roots: Vec<std::path::PathBuf>) {
        self.library_roots = roots.into_iter().map(Into::into).collect();
        // Blocking on purpose (ticket W14-02): a harness that renders one
        // frame and asserts on the grid cannot wait for a worker, and a
        // test that slept until a thread finished would be a flake
        // generator. The production path is `rescan_library`.
        self.library_scans += 1;
        self.library_scan = None;
        let library = Self::scan_with_cache(&self.library_roots, None);
        // Ticket W15-02: meta is read from the REAL config root (the
        // `RETROFORGE_CONFIG_DIR` a test points at), unlike the scan cache
        // above which is deliberately skipped here — play history is what
        // `tests/library_filters.rs` exercises through this path.
        self.library_meta =
            Self::load_library_meta(&library, crate::bindings_store::config_root().as_deref());
        self.library_badges =
            Self::load_badges(&library, crate::bindings_store::config_root().as_deref());
        self.library = Some(library);
    }

    /// Ticket W15-05: flip the Grid/List toggle directly, so
    /// `tests/library_grid.rs` can drive the card grid without a mouse
    /// click on the toolbar's toggle button.
    #[doc(hidden)]
    pub fn set_library_view_for_test(&mut self, view: crate::library::LibraryView) {
        self.library_view = view;
    }

    /// Ticket W15-09: swap in a fake `ArtClient` (and reset any fetcher
    /// already spawned against the real one), so a kittest scenario can
    /// turn the "Fetch box art from the internet" toggle on and observe
    /// the network indicator with zero real network access. Also clears
    /// cached "no thumbnail" texture entries, so a hash resolved before
    /// the toggle was on re-resolves through the fake client.
    /// Ticket W15-09: flip the "Fetch box art from the internet" toggle
    /// directly, so a kittest scenario can turn it on without a mouse
    /// click on the Settings checkbox.
    #[doc(hidden)]
    pub fn set_fetch_art_for_test(&mut self, on: bool) {
        self.settings.paths.fetch_art = on;
    }

    #[doc(hidden)]
    pub fn set_art_client_for_test(&mut self, client: Arc<dyn crate::art::ArtClient>) {
        self.art_client = client;
        self.art_fetcher = None;
        self.library_thumbnail_textures.clear();
        self.library_thumbnail_is_fetched.clear();
    }

    /// Ticket W15-09: whether the card grid's CURRENT thumbnail for the
    /// entry titled `title` came from a fetch (network indicator state)
    /// — looked up by title rather than hash, since a test fixture's ROM
    /// hash is an implementation detail the test itself should not need
    /// to recompute.
    #[doc(hidden)]
    #[must_use]
    pub fn thumbnail_is_fetched_for_test(&self, title: &str) -> bool {
        let Some(library) = self.library.as_ref() else {
            return false;
        };
        library
            .entries
            .iter()
            .find(|entry| entry.title == title)
            .and_then(|entry| match &entry.identity {
                crate::library::EntryIdentity::Recognized {
                    normalized_sha256, ..
                } => Some(normalized_sha256.clone()),
                crate::library::EntryIdentity::Unrecognized { .. } => None,
            })
            .is_some_and(|hash| self.library_thumbnail_is_fetched.contains(&hash))
    }

    /// Whether a stitched-canvas texture exists for the Map tab
    /// (ticket W10-05).
    #[doc(hidden)]
    #[must_use]
    pub fn has_stitched_map_for_test(&self) -> bool {
        self.ultrawide_texture.is_some()
    }

    /// Load a plugin directory as File > Load script… does (W11-04).
    #[doc(hidden)]
    pub fn load_script_for_test(&mut self, dir: &std::path::Path) {
        self.load_script(dir);
    }

    /// What the script asked to draw on the last frame it ran.
    #[doc(hidden)]
    #[must_use]
    pub fn script_overlay_for_test(&self) -> Vec<rf_plugin_sdk::sandbox::OverlayCmd> {
        self.script_overlay.clone()
    }

    /// The script status line — load failures included, since a script
    /// that refused to load and one that drew nothing look identical.
    #[doc(hidden)]
    #[must_use]
    pub fn script_status_for_test(&self) -> Option<String> {
        self.script_status.clone()
    }

    /// Turn on the full-level view exactly as its Enhance-workspace row
    /// does, probe and all (ticket W11-02).
    #[doc(hidden)]
    pub fn set_full_level_view_for_test(&mut self, on: bool) {
        self.current_game_settings.full_level_view = on;
        self.set_level_probe(on);
    }

    /// Whether a decoded level exists for the running ROM.
    #[doc(hidden)]
    #[must_use]
    pub fn has_decoded_level_for_test(&self) -> bool {
        self.level_session.is_some()
    }

    /// Ticket W16-06: switch the current game's mode exactly as the mode
    /// picker does, without going through egui.
    #[doc(hidden)]
    pub fn set_mode_for_test(&mut self, mode: crate::game_settings::Mode) {
        self.current_game_settings.mode = mode;
    }

    /// Ticket W20-09: persist "Widescreen: decoded" for the open game the
    /// way the Features row does, without sending the command — so a test
    /// can prove the REOPEN path applies it.
    #[doc(hidden)]
    pub fn set_widescreen_setting_and_save_for_test(&mut self, on: bool) {
        self.current_game_settings.widescreen_decoded = on;
        self.save_current_game_settings();
    }

    /// Ticket W16-06: turn Diorama on/off exactly as its Enhance-workspace
    /// row does (`crate::enhance_ui`'s "diorama" row) — a settings flip,
    /// no probe of its own (unlike full-level view) since the geometry is
    /// derived from the already-decoded, already-persistent level.
    #[doc(hidden)]
    pub fn set_diorama_for_test(&mut self, on: bool) {
        self.current_game_settings.diorama = on;
    }

    /// Ticket W16-14: turn "Mode 7 as 3D" on/off exactly as its Enhance-
    /// workspace row does — same shape as [`Self::set_diorama_for_test`].
    #[doc(hidden)]
    pub fn set_mode7_ground_for_test(&mut self, on: bool) {
        self.current_game_settings.mode7_ground = on;
    }

    /// Ticket W16-14: feed a synthetic [`rf_core_api::Mode7Frame`] plus a
    /// VRAM/CGRAM snapshot through the SAME `refresh_diorama_render` path
    /// a real running SNES core's `FrameMsg` would — the kittest hook this
    /// ticket's acceptance names ("through the app's test hooks like
    /// `diorama_live_view.rs`"), needed because no Mode 7 fixture ROM (and
    /// no cc65 toolchain to build one) exists in this repo, the same
    /// constraint `tests/mode7_plane_golden.rs`'s own header states for
    /// `rf-renderer`'s golden.
    ///
    /// Marks [`Self::mode7_seen`] the same way a real frame would, then
    /// calls the identical private render path so this hook cannot drift
    /// from what a real `CoreEvent::Mode7` frame does.
    #[doc(hidden)]
    pub fn inject_mode7_frame_for_test(
        &mut self,
        frame: rf_core_api::Mode7Frame,
        vram: Vec<u8>,
        cgram: Vec<u8>,
        sprite_rgba: Vec<u8>,
        width: usize,
        height: usize,
    ) {
        self.mode7_seen = true;
        let msg = core_thread::FrameMsg {
            level_probe: None,
            script_window: None,
            audio_fill: None,
            hd: None,
            rgba: vec![0u8; width * height * 4],
            width,
            height,
            frame_count: 0,
            last_scanline: None,
            bg_rgba: Vec::new(),
            sprite_rgba,
            oam: Box::new([0u8; 256]),
            audio_traces: Vec::new(),
            snes: Some(Box::new(core_thread::SnesDebugFrame {
                vram,
                cgram,
                oam: vec![0u8; 544],
                ppu_regs: vec![0u8; 0x40],
                aram: Vec::new(),
                mode7: rf_snes::ppu::mode7::Mode7::default(),
                hdma_lanes: Vec::new(),
                voices: Vec::new(),
            })),
            vram: Box::new([0u8; 0x1000]),
            cpu_regs: Box::new(rf_core_api::CpuRegs::None),
            palette_ram: Box::new([0u8; 32]),
            wram: Box::new([0u8; 0x0800]),
            prg_ram: Box::new([0u8; 0x2000]),
            sprite_height_px: 8,
            mode7: Some(Box::new(frame)),
        };
        self.refresh_diorama_render(&msg);
    }

    /// Ticket W16-13: whether this build has a real GPU device
    /// ([`Self::gpu`]) — the same `gpu_or_skip` clean-skip convention
    /// `rf-renderer`'s own GPU tests use, exposed so a kittest scenario
    /// that needs the Diorama pass to actually run (not just its settings
    /// bit to flip) can skip cleanly on an environment with no wgpu
    /// adapter instead of asserting on a texture that was never built.
    #[doc(hidden)]
    #[must_use]
    pub fn gpu_available_for_test(&self) -> bool {
        self.gpu.is_some()
    }

    /// A cheap content fingerprint of [`Self::diorama_texture`]'s current
    /// bytes, or `None` when no Diorama render exists yet (ticket W16-13).
    ///
    /// `TextureHandle`s are reused across `.set()` calls
    /// (`Self::refresh_diorama_render`'s own doc), so comparing handle
    /// identity across frames proves nothing about whether the CONTENT
    /// changed — this hashes [`Self::diorama_render`]'s own `rgba` bytes
    /// instead, the same "assert content, not handle identity" discipline
    /// `enhanced_view`'s own pure-function tests already apply to
    /// `select_active_view`.
    #[doc(hidden)]
    #[must_use]
    pub fn diorama_rgba_hash_for_test(&self) -> Option<u64> {
        let render = self.diorama_render.as_ref()?;
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in &render.rgba {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        Some(hash)
    }

    /// The status-bar badge text exactly as the toolbar renders it
    /// (ticket W16-06's kittest scenario needs this without a screenshot).
    #[doc(hidden)]
    #[must_use]
    pub fn badge_text_for_test(&self) -> String {
        crate::enhance_ui::badge_text(
            self.console_label,
            &self.current_game_settings,
            &self.game_facts(),
        )
    }

    /// The badge's hover breakdown exactly as the toolbar renders it
    /// (ticket W16-06's kittest scenario: "sees the badge text").
    #[doc(hidden)]
    #[must_use]
    pub fn badge_breakdown_for_test(&self) -> Vec<String> {
        crate::enhance_ui::badge_breakdown(&self.current_game_settings, &self.game_facts())
    }

    /// The live camera position the probe reported, in level space.
    #[doc(hidden)]
    #[must_use]
    pub fn level_camera_for_test(&self) -> Option<(i64, i64)> {
        self.level_camera
    }

    /// Open the Enhance workspace without going through the menu
    /// (ticket W10-02), so `tests/renders.rs` can photograph it.
    #[doc(hidden)]
    pub fn show_enhance_for_test(&mut self, show: bool) {
        self.show_enhance = show;
    }

    /// Start the core running, as the Run button does (ticket W11-01).
    #[doc(hidden)]
    pub fn resume_for_test(&mut self) {
        self.running = true;
        self.send_command(CoreCommand::Resume);
    }

    /// Ticket W20-03: whether the shell believes the core is running, and
    /// whether the in-game menu is open.
    #[doc(hidden)]
    pub fn running_and_menu_for_test(&self) -> (bool, bool) {
        (self.running, self.show_overlay_menu)
    }

    /// Pause the core, as the Pause button does (ticket W13-02d).
    #[doc(hidden)]
    pub fn pause_for_test(&mut self) {
        self.running = false;
        self.send_command(CoreCommand::Pause);
    }

    /// The memory panel's state (ticket W13-02d) — the same door the UI
    /// uses, for the end-to-end test that proves an edit reaches the
    /// machine.
    #[doc(hidden)]
    pub fn debug_memory_mut(&mut self) -> &mut crate::debug_dock::MemoryPanelData {
        &mut self.debug_panels.data.memory
    }

    /// Step one scanline, as the debugger's step control does — the
    /// smallest advance that makes the core send a fresh frame message,
    /// which is how a paused session's memory view refreshes at all
    /// (ticket W13-02d).
    #[doc(hidden)]
    pub fn step_scanline_for_test(&mut self) {
        self.send_command(CoreCommand::StepScanline);
    }

    /// The debug panels' last frame of `CoreEvent`s (ticket W13-02h) —
    /// exactly what the event-timeline panel plots.
    #[doc(hidden)]
    #[must_use]
    pub fn debug_events_for_test(&self) -> &[rf_core_api::CoreEvent] {
        &self.debug_panels.data.events
    }

    /// Show or hide the debug window, as the menu item does — the event
    /// subscription is gated on it (DEBUGGER.md §6).
    #[doc(hidden)]
    pub fn show_debug_for_test(&mut self, show: bool) {
        self.debug_panels.visible = show;
    }

    /// The SNES debug memories the last frame carried, if any (ticket
    /// W13-02b) — the input every SNES viewer decodes.
    #[doc(hidden)]
    #[must_use]
    pub fn snes_debug_frame_for_test(&self) -> Option<&crate::core_thread::SnesDebugFrame> {
        self.debug_panels.data.snes.as_deref()
    }

    /// The memory panel's last live WRAM snapshot (ticket W13-02d).
    #[doc(hidden)]
    #[must_use]
    pub fn wram_for_test(&self) -> &[u8; 0x0800] {
        &self.debug_panels.data.wram
    }

    /// Toggle temporal de-flicker exactly as the Enhance workspace's
    /// checkbox does, command and all (ticket W11-01).
    #[doc(hidden)]
    /// Ticket W11-05: load a Mesen HD pack from a directory.
    ///
    /// The directory is the pack as distributed: a `hires.txt` beside the
    /// PNGs it names. Everything about it is reported rather than
    /// assumed — a pack that half-applies silently is the most confusing
    /// possible outcome, which is why `hdpack::Import` exists and why the
    /// result of this is kept for the UI rather than logged.
    ///
    /// # Errors
    /// The pack is not loaded if `hires.txt` is missing or unparsable.
    /// A pack whose IMAGES are missing still loads — that is a `Partial`
    /// import, and refusing it would throw away work the author did do.
    pub fn load_hd_pack(&mut self, dir: &std::path::Path) -> Result<(), String> {
        let text = std::fs::read_to_string(dir.join("hires.txt"))
            .map_err(|e| format!("no hires.txt in {}: {e}", dir.display()))?;
        let pack = rf_enhance::hdpack::parse_hires(&text).map_err(|e| format!("{e:?}"))?;

        // Decode what the pack names. This crate owns the image decoder;
        // `rf_enhance::hdpack` deliberately decodes no pixels and takes
        // only dimensions, so the split is respected in both directions.
        let mut infos = std::collections::BTreeMap::new();
        let mut images = Vec::new();
        for name in &pack.images {
            match image::open(dir.join(name)) {
                Ok(img) => {
                    let rgba = img.to_rgba8();
                    infos.insert(
                        name.clone(),
                        rf_enhance::hdpack::ImageInfo {
                            width: rgba.width(),
                            height: rgba.height(),
                        },
                    );
                    images.push(rf_enhance::hd_render::PackImage {
                        width: rgba.width(),
                        height: rgba.height(),
                        rgba: rgba.into_raw(),
                    });
                }
                Err(_) => {
                    // A named image that will not decode is NOT fatal: it
                    // becomes `Unsatisfied::MissingImage`, which the panel
                    // shows. An empty placeholder keeps `img` indices
                    // aligned with `pack.images`, which the rules index by.
                    images.push(rf_enhance::hd_render::PackImage {
                        width: 0,
                        height: 0,
                        rgba: Vec::new(),
                    });
                }
            }
        }

        let import = rf_enhance::hdpack::import(pack, &infos);
        self.hd_summary = Some(import.summary());
        self.hd_unsatisfied = import
            .unsatisfied()
            .iter()
            .map(|u| format!("{u:?}"))
            .collect();
        self.hd_pack = Some((import.pack().clone(), images));
        self.send_command(CoreCommand::SetTileCapture(true));
        Ok(())
    }

    /// Ticket W20-09: File › Load HD pack… — load, then say what happened
    /// (the import summary, or why it failed) as a toast, never silently.
    fn load_hd_pack_from_menu(&mut self, dir: &std::path::Path, ctx: &egui::Context) {
        match self.load_hd_pack(dir) {
            Ok(()) => {
                let summary = self.hd_summary.clone().unwrap_or_default();
                self.toasts.push(
                    crate::toast::ToastKind::Info,
                    format!("HD pack loaded: {summary}"),
                    ctx,
                );
            }
            Err(e) => {
                self.toasts.push(
                    crate::toast::ToastKind::Error,
                    format!("HD pack not loaded: {e}"),
                    ctx,
                );
            }
        }
    }

    /// Unload the pack and stop paying for tile capture.
    pub fn clear_hd_pack(&mut self) {
        self.hd_pack = None;
        self.hd_summary = None;
        self.hd_unsatisfied.clear();
        self.hd_report = None;
        self.send_command(CoreCommand::SetTileCapture(false));
    }

    /// Ticket W16-02: open or close the Upscale Studio, toggling capture
    /// with it (`CoreCommand::SetStudioCapture`'s own doc: independent of
    /// `SetTileCapture`, pay-for-use the same way). Captured tiles
    /// survive a close/reopen — only the "Clear" button in the window
    /// itself drops them — so closing the window to check something else
    /// mid-session does not lose the recording.
    fn set_upscale_studio_open(&mut self, open: bool) {
        self.show_upscale_studio = open;
        self.send_command(CoreCommand::SetStudioCapture(open));
    }

    /// Nearest-scaled preview texture for one captured tile's ORIGINAL
    /// pixels, cached by asset hash — same pattern as
    /// `Self::library_thumbnail_texture`.
    fn upscale_studio_original_texture(
        &mut self,
        ctx: &egui::Context,
        tile: &crate::upscale_studio::CapturedTile,
    ) -> egui::TextureHandle {
        if let Some(t) = self
            .upscale_studio_original_textures
            .get(&tile.asset.asset_hash)
        {
            return t.clone();
        }
        // Nearest-neighbour x8 so an 8x8 tile is visible at all in a list
        // row — a raw 8x8 texture would be an unreadable speck.
        const PREVIEW_SCALE: u32 = 8;
        use rf_ai::upscale::Upscaler as _;
        let up = rf_ai::upscale::NearestUpscaler::new(PREVIEW_SCALE)
            .expect("a fixed non-zero literal cannot be ZeroScale");
        let src = rf_ai::upscale::Rgba8::from_indexed(
            &tile.asset.indexed_pixels,
            &tile.asset.palette,
            tile.asset.width,
            tile.asset.height,
        );
        let handle = match src.and_then(|s| up.upscale(&s)) {
            Ok(img) => {
                let color = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width as usize, img.height as usize],
                    &img.pixels,
                );
                ctx.load_texture(
                    format!("upscale-studio-orig-{}", tile.asset.asset_hash),
                    color,
                    egui::TextureOptions::NEAREST,
                )
            }
            Err(_) => {
                // A malformed capture is not fatal to the whole window —
                // show a 1x1 placeholder rather than panicking or hiding
                // the row entirely.
                ctx.load_texture(
                    format!("upscale-studio-orig-{}", tile.asset.asset_hash),
                    egui::ColorImage::from_rgba_unmultiplied([1, 1], &[128, 128, 128, 255]),
                    egui::TextureOptions::NEAREST,
                )
            }
        };
        self.upscale_studio_original_textures
            .insert(tile.asset.asset_hash.clone(), handle.clone());
        handle
    }

    /// The upscaled preview texture for one asset, from the last
    /// completed Run — decoded once and cached, cleared whenever a new
    /// Run starts (`Self::upscale_studio_window`).
    fn upscale_studio_upscaled_texture(
        &mut self,
        ctx: &egui::Context,
        asset_hash: &str,
    ) -> Option<egui::TextureHandle> {
        if let Some(t) = self.upscale_studio_upscaled_textures.get(asset_hash) {
            return Some(t.clone());
        }
        let pack = self.upscale_studio_pack.as_ref()?;
        let img = pack.built.images.get(asset_hash)?;
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [img.width as usize, img.height as usize],
            &img.pixels,
        );
        let handle = ctx.load_texture(
            format!("upscale-studio-up-{asset_hash}"),
            color,
            egui::TextureOptions::NEAREST,
        );
        self.upscale_studio_upscaled_textures
            .insert(asset_hash.to_string(), handle.clone());
        Some(handle)
    }

    /// Which upscaler a Run uses (ticket W16-02 criterion 2: "through
    /// `OnnxUpscaler` when a model is configured").
    ///
    /// Configured through environment variables rather than a settings
    /// UI field, for now: `ORT_DYLIB_PATH` and `RF_AI_MODEL_PATH` are
    /// already the exact variables `scripts/fetch-onnx-runtime.sh` and
    /// `scripts/fetch-ai-upscale-model.sh --print-path` hand back, and
    /// `crates/rf-ai/tests/onnx_bench.rs`/`studio_onnx.rs` already read
    /// them the same way — one vocabulary for "where the model/runtime
    /// live" across the fetch scripts, the ignored rf-ai tests, and this
    /// window, rather than a fourth. Falls back to the stub whenever
    /// either is unset, or the `ai-onnx` feature is not compiled in.
    fn upscale_studio_model_choice() -> crate::upscale_studio::ModelChoice {
        #[cfg(feature = "ai-onnx")]
        {
            if let (Ok(dylib), Ok(model)) = (
                std::env::var("ORT_DYLIB_PATH"),
                std::env::var("RF_AI_MODEL_PATH"),
            ) {
                let scale = std::env::var("RF_AI_MODEL_SCALE")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(4);
                let model_path = std::path::PathBuf::from(model);
                let model_id = model_path.file_stem().map_or_else(
                    || "onnx-model".to_string(),
                    |s| s.to_string_lossy().to_string(),
                );
                return crate::upscale_studio::ModelChoice::Onnx {
                    dylib_path: std::path::PathBuf::from(dylib),
                    model_path,
                    model_id,
                    license: "see crates/rf-ai/ai-model-manifest.toml".to_string(),
                    version: "n/a".to_string(),
                    scale,
                };
            }
        }
        crate::upscale_studio::ModelChoice::Stub { scale: 4 }
    }

    /// Start a Run on a worker thread. The UI thread never blocks on
    /// inference — same "the UI never blocks" principle
    /// `Self::rescan_library`'s worker follows, and the same
    /// spawn-detached-with-a-channel shape.
    fn start_upscale_studio_run(&mut self) {
        if self.upscale_studio_run.is_some() {
            return; // one Run at a time, same guard `rescan_library` uses.
        }
        let assets: Vec<rf_ai::pipeline::ExtractedAsset> = self
            .upscale_studio_session
            .tiles()
            .values()
            .map(|t| t.asset.clone())
            .collect();
        if assets.is_empty() {
            self.upscale_studio_status =
                "Nothing captured yet — play with the studio open, or start capture first."
                    .to_string();
            return;
        }
        // Grouping (`rf_ai::animation::group`) is O(sprites-per-frame²)
        // per consecutive frame pair, so it happens INSIDE
        // `crate::upscale_studio::run`, on the spawned thread below, not
        // here — see `CaptureSession::observations`'s own doc.
        let observations = self.upscale_studio_session.observations();
        let rom_hash = self.current_game_hash.clone().unwrap_or_default();
        let choice = Self::upscale_studio_model_choice();
        let post_process = rf_ai::studio::PostProcessOptions::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("upscale-studio-run".to_string())
            .spawn(move || {
                let outcome = crate::upscale_studio::run(
                    "upscale-studio-pack",
                    &rom_hash,
                    &assets,
                    &observations,
                    &choice,
                    post_process,
                );
                let _ = tx.send(outcome);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_handle) => {
                self.upscale_studio_run = Some(rx);
                self.upscale_studio_status = "Running\u{2026}".to_string();
            }
            Err(e) => {
                self.upscale_studio_status = format!("Could not start Run: {e}");
            }
        }
    }

    /// Adopt a finished Run the moment it lands (ticket W16-02, mirroring
    /// `Self::poll_library_scan`'s "adopt before anything draws" shape).
    fn poll_upscale_studio_run(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.upscale_studio_run.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.upscale_studio_run = None;
                self.upscale_studio_status = outcome.status_line;
                match outcome.pack {
                    Ok(pack) => {
                        self.upscale_studio_upscaled_textures.clear();
                        self.upscale_studio_pack = Some(pack);
                        self.toasts.push(
                            crate::toast::ToastKind::Success,
                            "Upscale Studio: run complete",
                            ctx,
                        );
                    }
                    Err(e) => {
                        self.upscale_studio_status = format!("{}: {e}", self.upscale_studio_status);
                        self.toasts.push(
                            crate::toast::ToastKind::Error,
                            format!("Upscale Studio run failed: {e}"),
                            ctx,
                        );
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.upscale_studio_run = None;
                self.upscale_studio_status = "Run thread ended without a result".to_string();
            }
        }
    }

    /// Write the last completed Run's pack to `dir`, honouring
    /// `upscale_studio_decisions`. Shared by the window's "Write pack…"
    /// button (which picks `dir` via `rfd::FileDialog`) and
    /// `Self::write_upscale_studio_pack_for_test` (which cannot go
    /// through a native file dialog in a headless test) — same split
    /// `Self::load_script_for_test` already uses for the same reason.
    fn write_upscale_studio_pack(&mut self, ctx: &egui::Context, dir: std::path::PathBuf) {
        let Some(pack) = self.upscale_studio_pack.clone() else {
            return;
        };
        match crate::upscale_studio::write_pack(
            &dir,
            &pack,
            self.upscale_studio_session.tiles(),
            &self.upscale_studio_decisions,
        ) {
            Ok(n) => {
                self.upscale_studio_write_dir = Some(dir.clone());
                self.upscale_studio_status = format!("Wrote {n} tile(s) to {}", dir.display());
                self.toasts.push(
                    crate::toast::ToastKind::Success,
                    format!("Upscale Studio: wrote pack to {}", dir.display()),
                    ctx,
                );
            }
            Err(e) => {
                self.upscale_studio_status = format!("Write pack failed: {e}");
                self.toasts.push(
                    crate::toast::ToastKind::Error,
                    format!("Upscale Studio write failed: {e}"),
                    ctx,
                );
            }
        }
    }

    /// Ticket W16-02: the Upscale Studio window — lists captured tiles,
    /// original-vs-upscaled previews, per-tile Approve/Reject/Replace,
    /// Run, and Write pack.
    fn upscale_studio_window(&mut self, ctx: &egui::Context) {
        if !self.show_upscale_studio {
            return;
        }
        let mut open = true;
        egui::Window::new("Upscale Studio")
            .id(egui::Id::new("upscale_studio_window"))
            .collapsible(true)
            .resizable(true)
            .default_width(520.0)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{} tile(s) captured",
                        self.upscale_studio_session.len()
                    ));
                    if ui.button("Clear captures").clicked() {
                        self.upscale_studio_session.clear();
                        self.upscale_studio_decisions.clear();
                        self.upscale_studio_pack = None;
                        self.upscale_studio_original_textures.clear();
                        self.upscale_studio_upscaled_textures.clear();
                    }
                    if ui.button("Run upscaler").clicked() {
                        self.start_upscale_studio_run();
                    }
                    if self.upscale_studio_run.is_some() {
                        ui.spinner();
                    }
                });
                if !self.upscale_studio_status.is_empty() {
                    ui.label(&self.upscale_studio_status);
                }
                ui.separator();

                let hashes: Vec<String> = self
                    .upscale_studio_session
                    .tiles()
                    .keys()
                    .cloned()
                    .collect();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for hash in &hashes {
                            let Some(tile) = self.upscale_studio_session.tiles().get(hash).cloned()
                            else {
                                continue;
                            };
                            ui.horizontal(|ui| {
                                let orig = self.upscale_studio_original_texture(ctx, &tile);
                                ui.image((orig.id(), egui::vec2(64.0, 64.0)));
                                if let Some(up) = self.upscale_studio_upscaled_texture(ctx, hash) {
                                    ui.image((up.id(), egui::vec2(64.0, 64.0)));
                                } else {
                                    ui.label("(run to preview)");
                                }
                                ui.label(&hash[..8.min(hash.len())]);

                                let decision = self
                                    .upscale_studio_decisions
                                    .entry(hash.clone())
                                    .or_default();
                                let mut approved =
                                    matches!(decision, crate::upscale_studio::Decision::Approved);
                                if ui.checkbox(&mut approved, "Approve").changed() {
                                    *decision = if approved {
                                        crate::upscale_studio::Decision::Approved
                                    } else {
                                        crate::upscale_studio::Decision::Rejected
                                    };
                                }
                                // `write_pack` refuses (aborting the WHOLE
                                // write) a replacement that is not exactly
                                // `8 * scale` square — naming the size here
                                // is cheaper than a failed write after
                                // picking the wrong file.
                                let replace_label = self
                                    .upscale_studio_pack
                                    .as_ref()
                                    .and_then(|p| p.built.images.values().next())
                                    .map_or_else(
                                        || "Replace with PNG\u{2026}".to_string(),
                                        |img| {
                                            format!(
                                                "Replace with PNG ({}x{})\u{2026}",
                                                img.width, img.height
                                            )
                                        },
                                    );
                                if ui.button(replace_label).clicked() {
                                    if let Some(path) = rfd::FileDialog::new()
                                        .add_filter("PNG", &["png"])
                                        .pick_file()
                                    {
                                        *decision = crate::upscale_studio::Decision::Replaced(path);
                                    }
                                }
                                if let crate::upscale_studio::Decision::Replaced(path) = decision {
                                    ui.label(format!(
                                        "{} {}",
                                        crate::icons::ARROW,
                                        path.file_name().map_or_else(
                                            || path.display().to_string(),
                                            |n| n.to_string_lossy().to_string()
                                        )
                                    ));
                                }
                            });
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    let can_write = self.upscale_studio_pack.is_some();
                    if ui
                        .add_enabled(can_write, egui::Button::new("Write pack\u{2026}"))
                        .clicked()
                    {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.write_upscale_studio_pack(ctx, dir);
                        }
                    }
                    if let Some(dir) = &self.upscale_studio_write_dir {
                        ui.label(format!("{} {}", crate::icons::ARROW, dir.display()));
                    }
                });
            });
        if !open {
            self.set_upscale_studio_open(false);
        }
    }

    /// What the last import said, for the UI.
    #[doc(hidden)]
    pub fn hd_summary_for_test(&self) -> Option<&str> {
        self.hd_summary.as_deref()
    }

    /// What the last COMPOSITE said — how many tiles the pack actually
    /// replaced this frame, which is a different question from whether
    /// its rules loaded.
    #[doc(hidden)]
    pub fn hd_report_for_test(&self) -> Option<rf_enhance::hd_render::CompositeReport> {
        self.hd_report
    }

    /// Ticket W11-03: turn decoded widescreen on or off.
    ///
    /// **The profile decides the policies; the user decides whether it is
    /// on.** A profile that declares no `[widescreen]` table still gets a
    /// widened picture with default policies — `auto` per background,
    /// which is bsnes-hd's own default and refuses the layers that should
    /// not move. What a profile CANNOT do is switch this on by itself
    /// (law 6), which `WidescreenPolicies::from_profile` enforces by
    /// leaving `enabled` false whatever the file says.
    pub fn set_widescreen(&mut self, on: bool) {
        if !on {
            self.widescreen_decisions = [None; 4];
            self.send_command(CoreCommand::SetWidescreen(None));
            return;
        }
        let mut policies = self
            .level_session
            .as_ref()
            .and_then(|s| rf_enhance::widescreen::WidescreenPolicies::from_profile(&s.profile).ok())
            .unwrap_or_default();
        policies.enabled = true;
        self.send_command(CoreCommand::SetWidescreen(Some(
            crate::core_thread::WidescreenRequest {
                width: WIDESCREEN_WIDTH,
                policies,
            },
        )));
    }

    /// What the widescreen policy last decided, per background: `Some`
    /// reason means that layer stayed at 4:3.
    #[must_use]
    pub fn widescreen_decisions(&self) -> [Option<&'static str>; 4] {
        self.widescreen_decisions
    }

    pub fn set_deflicker_for_test(&mut self, on: bool) {
        self.current_game_settings.deflicker = on;
        self.send_command(CoreCommand::SetDeflicker(on));
    }

    /// The rgba of the most recent frame the UI received.
    #[doc(hidden)]
    #[must_use]
    /// The dimensions of the last frame the core delivered.
    ///
    /// Ticket W11-03: a widescreen test needs to prove the picture got
    /// WIDER, and the RGBA blob alone cannot say that.
    #[doc(hidden)]
    pub fn frame_size_for_test(&self) -> Option<(usize, usize)> {
        self.last_frame_size
    }

    /// Ticket W20-01: where the Original view's picture was drawn, and
    /// the core frame size it was sized from.
    #[doc(hidden)]
    pub fn play_rect_for_test(&self) -> Option<(egui::Rect, (usize, usize))> {
        self.last_play_rect.zip(self.core_frame_size)
    }

    /// Ticket W20-02: fingerprint each displayed frame from now on, and
    /// read the latest fingerprint.
    #[doc(hidden)]
    pub fn hash_display_for_test(&mut self) -> Option<u64> {
        self.hash_display_for_test = true;
        self.display_hash
    }

    /// The size of the texture the play view is actually drawing.
    #[doc(hidden)]
    pub fn display_texture_size_for_test(&self) -> Option<[usize; 2]> {
        self.texture.as_ref().map(egui::TextureHandle::size)
    }

    /// Ticket W20-04: the last fullscreen state requested.
    #[doc(hidden)]
    pub fn last_fullscreen_request_for_test(&self) -> Option<bool> {
        self.last_fullscreen_request
    }

    /// Ticket W20-01: Settings › Video, for tests that need a mode set.
    #[doc(hidden)]
    pub fn video_settings_mut_for_test(&mut self) -> &mut crate::settings::VideoSettings {
        &mut self.settings.video
    }

    pub fn last_frame_rgba_for_test(&self) -> Option<Vec<u8>> {
        self.last_frame_rgba.clone()
    }

    /// The core's crash message, if it reported one.
    #[doc(hidden)]
    #[must_use]
    pub fn crash_message_for_test(&self) -> Option<String> {
        self.crash.as_ref().map(|c| c.message.clone())
    }

    /// Ticket W15-04: whether the overwrite-confirmation modal is up.
    #[doc(hidden)]
    #[must_use]
    pub fn pending_overwrite_for_test(&self) -> bool {
        self.pending_overwrite.is_some()
    }

    /// Ticket W15-04: whether the quit-confirmation modal is up.
    #[doc(hidden)]
    #[must_use]
    pub fn pending_quit_for_test(&self) -> bool {
        self.pending_quit
    }

    /// Drive the same Quit path the menu buttons use
    /// (`RetroForgeApp::request_quit`), for a headless test — neither
    /// Quit button is reachable from `egui_kittest` without first opening
    /// the menu it lives in, and this ticket's acceptance is about the
    /// confirmation gate, not menu navigation `ui_smoke.rs` already
    /// covers.
    #[doc(hidden)]
    pub fn request_quit_for_test(&mut self, ctx: &egui::Context) {
        self.request_quit(ctx);
    }

    /// Ticket W15-04: whether any toast is currently visible.
    #[doc(hidden)]
    #[must_use]
    pub fn has_visible_toast_for_test(&self) -> bool {
        // Ticket W20-12: save/load/screenshot feedback moved from the toast
        // stack to the OSD stack — the same promise (it appears, it does
        // not pause emulation, it expires), drawn over the game instead.
        !self.toasts.is_empty() || !self.osd.is_empty()
    }

    /// Ticket W20-12: the OSD cards currently live.
    #[doc(hidden)]
    pub fn osd_texts_for_test(&self) -> Vec<String> {
        self.osd.texts()
    }

    /// Ticket W15-06: the current key bound to an App hotkey, so a test
    /// can confirm a default rather than assume it.
    #[doc(hidden)]
    #[must_use]
    pub fn app_bindings_key_for_test(
        &self,
        action: crate::app_bindings::AppAction,
    ) -> Option<egui::Key> {
        self.app_bindings.key_for(action)
    }

    /// The last frame number the core reported, for a test that needs to
    /// wait until a ROM has actually run rather than merely started.
    #[doc(hidden)]
    #[must_use]
    pub fn frame_count_for_test(&self) -> u64 {
        self.position.map_or(0, |(f, _)| f)
    }

    /// Open the debug-viewer dock without the menu (screenshot tour).
    #[doc(hidden)]
    pub fn show_debug_panels_for_test(&mut self, show: bool) {
        self.debug_panels.visible = show;
    }

    /// Open the Settings window without the menu (screenshot tour).
    #[doc(hidden)]
    pub fn show_settings_for_test(&mut self, show: bool) {
        self.show_settings = show;
    }

    /// Switch to the Ultrawide camera and request a canvas snapshot now
    /// (screenshot tour), rather than waiting out the refresh throttle.
    #[doc(hidden)]
    pub fn set_ultrawide_for_test(&mut self) {
        self.camera = CameraToggle::Ultrawide;
        self.ultrawide_refresh_countdown = 0;
    }

    /// Set §3.1's search text directly (ticket W10-03).
    #[doc(hidden)]
    pub fn set_library_search_for_test(&mut self, needle: &str) {
        self.library_search = needle.to_string();
    }

    /// Set the library's selected row directly (ticket W15-01), so a test
    /// can put the selection into a known state — including clearing it —
    /// without first driving a click or a key press to get there.
    #[doc(hidden)]
    pub fn set_library_selected_for_test(&mut self, path: Option<std::path::PathBuf>) {
        self.library_selected = path;
    }

    /// Close the running ROM, as File > Close ROM does (ticket W10-03).
    #[doc(hidden)]
    pub fn close_rom_for_test(&mut self) {
        self.close_rom();
    }

    /// Whether the one Game Settings window (ticket W15-03) is open — the
    /// accessor the test suite asserts through, since the context menu,
    /// the Enhance menu, and the overlay menu all set the same
    /// `show_game_settings` field, and a test proving "the same window
    /// instance" needs to see that shared field directly rather than
    /// inferring it from what is drawn on screen.
    #[doc(hidden)]
    #[must_use]
    pub fn show_game_settings_for_test(&self) -> bool {
        self.show_game_settings
    }

    /// Ticket W16-02: seed the Upscale Studio's capture session directly,
    /// bypassing a running core thread — a kittest scenario has no PPU to
    /// draw real tiles, and this is the same "test-only accessor" pattern
    /// as `show_game_settings_for_test`.
    #[doc(hidden)]
    pub fn upscale_studio_seed_for_test(&mut self, captures: &[crate::stepper::StudioTileCapture]) {
        self.upscale_studio_session.observe(captures);
    }

    /// How many distinct tiles the capture session actually recorded —
    /// the assertion that catches "the window opened before a ROM did,
    /// so the newly spawned core was never told to capture at all"
    /// (ticket W16-02's `open_rom_path` re-assertion, next to
    /// `SetLayerExtraction`'s identical stale-state-across-a-reload
    /// fix).
    #[doc(hidden)]
    #[must_use]
    pub fn upscale_studio_capture_len_for_test(&self) -> usize {
        self.upscale_studio_session.len()
    }

    /// Whether the Upscale Studio window is open.
    #[doc(hidden)]
    #[must_use]
    pub fn show_upscale_studio_for_test(&self) -> bool {
        self.show_upscale_studio
    }

    /// Where the last "Write pack" wrote to, for a test to check files
    /// against without re-deriving the path from a click.
    #[doc(hidden)]
    #[must_use]
    pub fn upscale_studio_write_dir_for_test(&self) -> Option<std::path::PathBuf> {
        self.upscale_studio_write_dir.clone()
    }

    /// Write the last completed Run's pack to `dir`, bypassing the
    /// "Write pack…" button's `rfd::FileDialog` — a headless kittest
    /// harness has no OS file picker to drive, same reasoning as
    /// `Self::load_script_for_test`.
    #[doc(hidden)]
    pub fn write_upscale_studio_pack_for_test(
        &mut self,
        ctx: &egui::Context,
        dir: std::path::PathBuf,
    ) {
        self.write_upscale_studio_pack(ctx, dir);
    }

    /// Set one captured tile's review decision directly, for a test that
    /// wants to exercise Reject/Replace without clicking the checkbox
    /// pixel-for-pixel.
    #[doc(hidden)]
    pub fn upscale_studio_set_decision_for_test(
        &mut self,
        asset_hash: &str,
        decision: crate::upscale_studio::Decision,
    ) {
        self.upscale_studio_decisions
            .insert(asset_hash.to_string(), decision);
    }

    /// Close the Game Settings window and clear its target, without
    /// clicking the window's own close button (ticket W15-03).
    #[doc(hidden)]
    pub fn close_game_settings_for_test(&mut self) {
        self.show_game_settings = false;
        self.game_settings_target = None;
    }

    /// Which game the Game Settings window is currently targeting:
    /// `None` for "the running game", `Some(hash)` for a library entry
    /// picked from its context menu (ticket W15-03).
    #[doc(hidden)]
    #[must_use]
    pub fn game_settings_target_hash_for_test(&self) -> Option<String> {
        self.game_settings_target.as_ref().map(|t| t.hash.clone())
    }

    /// Open the context menu for `path`'s row directly, as if it had been
    /// right-clicked (ticket W15-03) — used by tests that need the menu
    /// open without first computing the row's on-screen rect.
    #[doc(hidden)]
    pub fn set_library_selected_and_open_context_menu_for_test(
        &mut self,
        path: std::path::PathBuf,
    ) {
        self.library_selected = Some(path);
        self.library_context_menu_open = true;
    }

    /// Ticket W15-08: the two things `poll_input`'s real gamepad branch
    /// does that a hand-synthesized `egui::Event` alone cannot reproduce
    /// — mark the pad as the most-recently-active device, and (matching
    /// `Start`'s real handling) latch `pad_menu_requested` when `actions`
    /// contains `Menu`. A test has no `GilrsBackend` to poll (the
    /// `gamepad` feature needs real hardware), so it pushes the actions'
    /// own `egui::Event`s into the harness's `RawInput` itself — exactly
    /// as `tests/gamepad_nav.rs` already does via `GamepadNav::events_for`
    /// — and calls this alongside for the two effects that live on `self`
    /// rather than in an `egui::Event`.
    ///
    /// Call this AFTER the frame that actually processes a direction's
    /// `egui::Event` (the one `library_grid`'s own `key_pressed` check
    /// reads), not before: `poll_input`'s own device tracker runs every
    /// frame and, in a test build with no real `PadEvent` source, would
    /// otherwise see that same injected key as ordinary keyboard input
    /// and reclassify the device right back — see `tests/
    /// library_controller.rs`'s `press_pad` helper for the exact
    /// two-frame sequencing this implies.
    #[doc(hidden)]
    pub fn mark_pad_active_for_test(&mut self, actions: &[crate::ui_nav::NavAction]) {
        self.last_active_input = crate::ui_nav::InputDevice::Gamepad;
        if actions.contains(&crate::ui_nav::NavAction::Menu) {
            self.pad_menu_requested = true;
        }
    }

    /// Ticket W15-08: which device the app currently believes is driving
    /// the UI — the same field `apply_theme`'s type scale and the
    /// library's focus ring read.
    #[doc(hidden)]
    #[must_use]
    pub fn last_active_input_for_test(&self) -> crate::ui_nav::InputDevice {
        self.last_active_input
    }

    /// Ticket W15-08: the width the library's selection ring is CURRENTLY
    /// drawing at, reading the exact same `Self::focus_ring_stroke` the
    /// real drawing code calls — so a test can assert "thick ring" without
    /// duplicating the mouse-vs-pad decision and risking it drifting from
    /// what actually renders.
    #[doc(hidden)]
    #[must_use]
    pub fn library_focus_ring_width_for_test(&self) -> f32 {
        let tokens = crate::theme::Tokens::from_accessibility(&self.settings.accessibility);
        self.focus_ring_stroke(&tokens).width
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
        let mut app_changed = false;
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
                    // Ticket W15-06 (`docs/design/UX_WAVE_15.md` §6): the
                    // App section, namespaced apart from the per-port game
                    // bindings below — a separate `AppAction` enum and
                    // store section (`crate::app_bindings`), so the two
                    // tables cannot collide by construction. What DOES
                    // need a runtime check is one App action and one game
                    // button both claiming the same physical key/pad
                    // button, which is what `binding_conflict` reports
                    // inline, the same way a rejected remap is reported
                    // anywhere else in this window.
                    ui.heading("App");
                    if let Some(conflict) = &self.binding_conflict {
                        ui.colored_label(egui::Color32::from_rgb(0xE0, 0x80, 0x30), conflict);
                    }
                    egui::Grid::new("controls-app")
                        .num_columns(4)
                        .striped(true)
                        .show(ui, |ui| {
                            for action in crate::app_bindings::AppAction::ALL {
                                ui.label(action.label());

                                let bound_key = self.app_bindings.key_for(action);
                                let label = match self.awaiting_app_key {
                                    Some(a) if a == action => "press a key\u{2026}".to_string(),
                                    _ => bound_key.map_or_else(
                                        || "\u{2014}".to_string(),
                                        |k| k.name().to_string(),
                                    ),
                                };
                                if ui.button(label).clicked() {
                                    self.awaiting_app_key = Some(action);
                                }
                                if ui.small_button("Clear").clicked() {
                                    self.app_bindings.unbind_key_for(action);
                                    app_changed = true;
                                }

                                let current_pad = self.app_bindings.pad_for(action);
                                let pad_label =
                                    current_pad.map_or("\u{2014}", rf_input::PadButton::name);
                                egui::ComboBox::from_id_salt(("controls-app-pad", action))
                                    .selected_text(pad_label)
                                    .show_ui(ui, |ui| {
                                        if ui
                                            .selectable_label(current_pad.is_none(), "\u{2014}")
                                            .clicked()
                                        {
                                            self.app_bindings.unbind_pad_for(action);
                                            app_changed = true;
                                        }
                                        for button in rf_input::PadButton::ALL {
                                            if ui
                                                .selectable_label(
                                                    current_pad == Some(button),
                                                    button.name(),
                                                )
                                                .clicked()
                                            {
                                                if let Some(conflict) =
                                                    crate::app_bindings::pad_conflicts_with_game(
                                                        &self.bindings,
                                                        button,
                                                    )
                                                {
                                                    self.binding_conflict = Some(conflict);
                                                } else {
                                                    self.app_bindings.bind_pad(button, action);
                                                    app_changed = true;
                                                    self.binding_conflict = None;
                                                }
                                            }
                                        }
                                    });
                                ui.end_row();
                            }
                        });
                    if ui.button("Restore App defaults").clicked() {
                        self.app_bindings = crate::app_bindings::AppBindings::default();
                        app_changed = true;
                    }
                    ui.separator();

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
                                                // Ticket W15-06 criterion 2's "vice versa" for
                                                // pads: a game binding can never claim a pad
                                                // button the App namespace already uses.
                                                if let Some(conflict) =
                                                    crate::app_bindings::game_pad_conflicts_with_app(
                                                        &self.app_bindings,
                                                        pad_button,
                                                    )
                                                {
                                                    self.binding_conflict = Some(conflict);
                                                } else {
                                                    self.bindings.pads.bind(pad_button, nes);
                                                    changed = true;
                                                    self.binding_conflict = None;
                                                }
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
        if app_changed {
            self.save_app_bindings();
        }
    }

    /// Persist the App hotkeys, same shape as [`Self::save_bindings`].
    fn save_app_bindings(&mut self) {
        let Some(root) = self.config_root.clone() else {
            self.bindings_status =
                "No config directory; this App hotkey change applies to the current session only."
                    .to_string();
            return;
        };
        self.bindings_status = match crate::bindings_store::save_app(&root, &self.app_bindings) {
            Ok(path) => format!("App hotkeys saved to {}.", path.display()),
            Err(e) => format!("Could not save App hotkeys: {e}"),
        };
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
    /// Ticket W20-07: the debugger's transport — Run/Pause, Step Frame,
    /// Step Scanline and the frame/scanline position — drawn at the top of
    /// the Debug Viewers window rather than in the player's status bar.
    ///
    /// Unchanged behaviour, moved: until W20-07 these sat in the status
    /// bar on every screen, including the library with no ROM open, where
    /// all three were disabled buttons advertising nothing.
    fn transport_controls(&mut self, ui: &mut egui::Ui) {
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
            if let Some((frame, scanline)) = self.position {
                ui.separator();
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
        });
    }

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
                self.transport_controls(ui);
                ui.separator();
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
    /// Turn the SNES debug-memory capture on or off to match what is
    /// docked (ticket W13-02b) — the same shape as
    /// [`Self::sync_event_subscription`], and for the same reason: 128 KiB
    /// per frame is not a cost to pay while nobody is looking.
    fn sync_snes_capture(&mut self) {
        // Ticket W16-14: Mode 7 ground needs the live VRAM/CGRAM snapshot
        // (`FrameMsg::snes`) to build its plane texture, same OR-safety
        // shape as `sync_event_subscription`'s own `wants` just below.
        let wants = self.debug_panels.wants_snes_capture() || self.mode7_ground_wanted;
        if wants == self.snes_capture_active {
            return;
        }
        self.snes_capture_active = wants;
        self.send_command(CoreCommand::SetSnesDebugCapture(wants));
    }

    fn sync_event_subscription(&mut self) {
        self.sync_snes_capture();
        // The event VIEWER only needs events while it is on screen, which
        // is DEBUGGER.md §6's "closed panels register no event
        // subscriptions". A WATCHPOINT is different and the distinction is
        // deliberate (ticket W13-02e): a watch is armed until it is
        // disarmed, so closing the debug window must not silently stop it
        // counting — a watch that quietly stopped would read as "the game
        // never touches this address", the worst answer a debugger can
        // give. Pay-for-use still holds: nothing armed, no subscription.
        // Ticket W16-14: Mode 7 ground needs `CoreEvent::Mode7`
        // (`EventMask::MODE7`, part of `EventMask::ALL`) to know the
        // frame's registers — same OR-safety shape as the two sources
        // already here.
        let wants = (self.debug_panels.visible && self.debug_panels.wants_event_subscription())
            || self.debug_panels.annotations.has_watches()
            || self.mode7_ground_wanted;
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
    fn write_screenshots(&mut self, ctx: &egui::Context) {
        let Some(buffers) = self.compare_buffers.as_ref() else {
            self.status = "Screenshot: no frame captured yet".to_string();
            return;
        };
        let stamp = self.position.map_or(0, |(frame, _)| frame);
        let dir = self.screenshots_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.status = format!("Screenshot failed: {e}");
            self.toasts.push(
                crate::toast::ToastKind::Error,
                format!("Screenshot failed: {e}"),
                ctx,
            );
            return;
        }
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
                    self.toasts.push(
                        crate::toast::ToastKind::Error,
                        format!("Screenshot failed: {e}"),
                        ctx,
                    );
                    return;
                }
            }
        }
        self.status = format!("Screenshot: wrote {}", written.join(", "));
        // Ticket W15-06: the App-hotkey screenshot (and the Enhance
        // workspace's identical button) both land here, so both get the
        // same confirmation rather than only the hotkey path having one.
        let thumb = self.frame_thumb(ctx);
        self.osd.push_card(
            crate::toast::ToastKind::Success,
            "Screenshot saved",
            thumb,
            None,
            ctx,
        );
    }

    /// Where screenshots land (ticket W15-06): `<config-dir>/retroforge/
    /// screenshots`, mirroring `crate::state_slots::slots_dir`'s "under
    /// the config root" convention. Falls back to the current directory
    /// when there is no config root at all — the same degrade
    /// `Self::save_bindings` uses for a platform with nowhere to put a
    /// config.
    fn screenshots_dir(&self) -> std::path::PathBuf {
        match &self.config_root {
            Some(root) => root
                .join(crate::bindings_store::APP_DIR)
                .join("screenshots"),
            None => std::env::current_dir().unwrap_or_default(),
        }
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
                let level_texture = self.level_texture(ui.ctx());
                let facts = self.game_facts();
                let mut view = crate::enhance_dock::EnhanceCtx {
                    widescreen_decisions: self.widescreen_decisions,
                    hd_summary: self.hd_summary.as_deref(),
                    hd_unsatisfied: &self.hd_unsatisfied,
                    hd_report: self.hd_report,
                    level_texture: level_texture.as_ref(),
                    profile_title: self
                        .level_session
                        .as_ref()
                        .map(|s| s.profile.meta.title.clone()),
                    profile_capabilities: self.level_session.as_ref().map_or_else(Vec::new, |s| {
                        let c = &s.profile.capabilities;
                        // The profile's OWN declared capabilities, not a
                        // list this file invents — the honesty contract
                        // says the UI cannot offer what the profile does
                        // not claim.
                        vec![
                            ("full_level", c.full_level),
                            (
                                "widescreen",
                                c.widescreen != rf_profiles::schema::WidescreenMode::None,
                            ),
                            ("hud_separation", c.hud_separation),
                            ("entity_overlay", c.entity_overlay),
                            ("fast_load", c.fast_load),
                            ("smooth_camera", c.smooth_camera),
                        ]
                    }),
                    level_camera: self.level_camera,
                    // The original viewport, which is what the outline
                    // outlines. Taken from the live frame rather than
                    // hardcoded to 256x240 so a PAL or hires frame is
                    // outlined at its real size.
                    viewport_size: self.texture.as_ref().map_or((256.0, 240.0), |t| {
                        let s = t.size();
                        (s[0] as f32, s[1] as f32)
                    }),
                    settings: &mut self.current_game_settings,
                    facts,
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
        if let Some(on) = actions.deflicker_set {
            self.send_command(CoreCommand::SetDeflicker(on));
        }
        if let Some(on) = actions.full_level_set {
            // Ticket W16-13: OR'd with Diorama's own want — turning "Full-
            // level view" off must not disarm the probe out from under a
            // still-effective Diorama, which needs the identical data
            // (`Self::sync_diorama_subscription`'s own doc).
            self.set_level_probe(on || self.diorama_effective());
        }
        if actions.diorama_set.is_some() || actions.mode7_ground_set.is_some() {
            // Ticket W16-13/W16-14: react to the toggle within THIS frame
            // rather than waiting for the next repaint's
            // `sync_diorama_subscription` call — the enhance_dock checkbox
            // already wrote `ctx.settings.diorama`/`mode7_ground` directly
            // (`features_body`'s own arms).
            self.sync_diorama_subscription();
        }
        if let Some(on) = actions.widescreen_set {
            self.set_widescreen(on);
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

    /// Ticket W20-04: borderless fullscreen on/off.
    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        let on = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!on));
        self.last_fullscreen_request = Some(!on);
    }

    /// Ticket W20-04: remember the WINDOWED size once it settles (never
    /// the fullscreen one — leaving fullscreen and quitting must not make
    /// the next launch screen-sized).
    fn track_window_size(&mut self, ctx: &egui::Context) {
        let (fullscreen, size) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.fullscreen.unwrap_or(false),
                v.inner_rect
                    .map(|r| [r.width().round(), r.height().round()]),
            )
        });
        let Some(size) = size.filter(|_| !fullscreen) else {
            self.pending_window_size = None;
            return;
        };
        if self.settings.window.inner_size == Some(size) {
            self.pending_window_size = None;
            return;
        }
        match self.pending_window_size {
            Some((pending, since)) if pending == size => {
                if since.elapsed() >= WINDOW_SIZE_SETTLE {
                    self.settings.window.inner_size = Some(size);
                    self.pending_window_size = None;
                    self.save_settings();
                } else {
                    ctx.request_repaint_after(WINDOW_SIZE_SETTLE);
                }
            }
            _ => {
                self.pending_window_size = Some((size, std::time::Instant::now()));
                ctx.request_repaint_after(WINDOW_SIZE_SETTLE);
            }
        }
    }

    /// Ticket W20-01: hand Settings › Video's V-sync to the surface.
    ///
    /// Live, not at restart: eframe 0.35's `Frame::set_wgpu_surface_config`
    /// reconfigures the surface on the next paint (egui-wgpu
    /// `winit.rs`, "Apply any runtime changes requested via
    /// `RenderState::surface_config`"). `Auto*` rather than `Fifo`/
    /// `Immediate` so a backend lacking one mode degrades instead of
    /// failing surface configuration. Only called when the setting differs
    /// from what was last applied, so the surface is not reconfigured
    /// every frame.
    fn apply_vsync(&mut self, frame: &mut eframe::Frame) {
        let want = self.settings.video.vsync;
        if self.applied_vsync == Some(want) {
            return;
        }
        if let Some(mut config) = frame.wgpu_surface_config() {
            config.present_mode = if want {
                eframe::wgpu::PresentMode::AutoVsync
            } else {
                eframe::wgpu::PresentMode::AutoNoVsync
            };
            frame.set_wgpu_surface_config(config);
        }
        self.applied_vsync = Some(want);
    }

    fn video_panel(&mut self, ui: &mut egui::Ui) {
        let scale_mode = self.settings.video.scale_mode;
        let par = crate::play_view::pixel_aspect_ratio(self.settings.video.pixel_aspect);
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
            match enhanced_view::select_active_view(
                camera,
                self.ultrawide_render.as_ref(),
                self.diorama_render.as_ref(),
                self.peeking_original,
            ) {
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
                        // Ticket W20-01: sized by Settings › Video, from the
                        // core's own frame size (an HD pack's texture is
                        // N times larger but covers the same picture).
                        let [tw, th] = texture.size();
                        let (fw, fh) = self.core_frame_size.unwrap_or((tw, th));
                        #[allow(clippy::cast_precision_loss)]
                        let grid = crate::play_view::DisplayGrid::for_frame(fw as f32, fh as f32);
                        let response =
                            crate::play_view::show_frame(ui, texture, grid, par, scale_mode);
                        self.last_play_rect = Some(response.rect);
                        // Ticket W11-04: what the script asked to draw,
                        // painted OVER the frame and never into it. An
                        // overlay that modified the framebuffer would be
                        // an enhancement pretending to be an observer —
                        // the line ARCHITECTURE §2 draws.
                        self.draw_script_overlay(ui, response.rect);
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
                        // Console pixels at console line count, so the TV
                        // aspect and integer rule apply as for Original.
                        let [tw, th] = texture.size();
                        #[allow(clippy::cast_precision_loss)]
                        let grid = crate::play_view::DisplayGrid::for_frame(tw as f32, th as f32);
                        crate::play_view::show_frame(ui, texture, grid, par, scale_mode);
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
                // Ticket W16-13: input/hit-testing stay 2D even here
                // (acceptance criterion 2) — this paints the composited
                // RGBA the exact same way `Image::from_texture` paints
                // Original/Ultrawide above; nothing below hooks pointer
                // events into the 3D scene at all.
                enhanced_view::ActiveView::Diorama { .. } => {
                    if let Some(texture) = &self.diorama_texture {
                        // A rendered 3D image: its pixels are already
                        // display pixels, so no TV stretch.
                        let [tw, th] = texture.size();
                        #[allow(clippy::cast_precision_loss)]
                        let grid = crate::play_view::DisplayGrid::exact(tw as f32, th as f32);
                        crate::play_view::show_frame(ui, texture, grid, 1.0, scale_mode);
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("Diorama view: preparing texture\u{2026}");
                        });
                    }
                }
            }
        });
    }
}

impl eframe::App for RetroForgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);
        self.apply_vsync(frame);
        self.track_window_size(&ctx);
        // Ticket W15-04: drain any script load/manifest failure queued
        // since the last frame into a toast — see
        // `script_error_toast_pending`'s doc for why this can't happen
        // at the point the error is discovered.
        if let Some(msg) = self.script_error_toast_pending.take() {
            self.toasts.push(crate::toast::ToastKind::Error, msg, &ctx);
        }
        self.poll_input(&ctx);
        self.poll_app_hotkeys(&ctx);
        // Ticket W14-02: adopt a background library scan the moment it
        // lands, before anything draws the grid.
        self.poll_library_scan(&ctx);
        self.poll_art_fetch(&ctx);
        // Ticket W16-02: adopt a finished Upscale Studio run.
        self.poll_upscale_studio_run(&ctx);
        // Ticket W16-13: re-derive Diorama's effective state and arm/
        // disarm its data sources BEFORE draining this frame's core
        // events — so a toggle-off is already reflected by the time
        // `pump_core_events` below decides whether to refresh/clear the
        // Diorama texture, and `video_panel` paints the flat view this
        // SAME repaint (acceptance 1).
        self.sync_diorama_subscription();
        self.pump_core_events(&ctx);
        self.maybe_request_canvas_snapshot();
        self.sync_event_subscription();
        self.pump_trace();
        // Ticket W13-02f: whatever the annotations panel asked for last
        // frame (save, or export to the profile editor).
        self.pump_annotation_request();
        // Ticket W13-02d: one edit from the memory panel, if the user
        // committed one this frame.
        self.pump_memory_poke();
        self.pump_audio_scopes();

        self.menu_bar(ui);
        self.controls_bar(ui);
        self.video_panel(ui);
        self.crash_dialog(&ctx);
        self.layers_debug_window(&ctx);
        self.enhance_window(&ctx);
        self.controls_window(&ctx);
        self.settings_window(&ctx);
        self.game_settings_window(&ctx);
        self.upscale_studio_window(&ctx);
        self.hash_info_window(&ctx);
        self.overlay_menu(&ctx);
        self.states_modal(&ctx);
        // Ticket W20-10: drawn whenever a Save asked for it — from the
        // States window OR the Quick Menu — not only inside the former.
        self.overwrite_confirm_modal(&ctx);
        self.pump_authoring();
        self.author_window(&ctx);
        self.debug_panels_window(&ctx);
        // Ticket W15-04: quit confirmation, drawn wherever a Quit click
        // set `pending_quit` this frame or a prior one.
        self.quit_confirm_modal(&ctx);
        // Ticket W15-04: toasts render LAST, after every window/modal —
        // they are non-interactable (`ToastStack::show`'s own
        // `Area::interactable(false)`) and anchored independently of any
        // panel, so draw order only affects which layer paints over
        // which, never input. Painting them last is what keeps a toast
        // visible over a maximized window instead of tucked behind it.
        let tokens = crate::theme::Tokens::from_accessibility(&self.settings.accessibility);
        // Ticket W20-12: OSD cards inside the picture's top-left corner;
        // not over the Quick Menu (it is the thing being looked at then).
        if self.core.is_some() && !self.show_overlay_menu {
            let corner = self
                .last_play_rect
                .map_or(egui::pos2(16.0, 56.0), |r| r.min + egui::vec2(12.0, 12.0));
            self.osd.show_at(&ctx, &tokens, corner);
        }
        self.toasts.show(&ctx, &tokens);
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
/// Ticket W16-04 adds `rf_enhance::atmosphere::HEURISTIC_ID` — this is the
/// Game Settings window's own toggle for promoting the fog pass from
/// shadow to advisory to active (acceptance criterion 3), the same
/// mechanism anti-flicker already uses; no separate fog-specific control
/// is needed because [`Self::heuristics_panel`] is generic over the name.
pub(crate) const HEURISTICS: &[&str] = &["anti-flicker", rf_enhance::atmosphere::HEURISTIC_ID];

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
