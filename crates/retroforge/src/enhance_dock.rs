//! The Enhance workspace: `docs/design/FRONTEND_UI.md` §3.3's
//! `[Compare] [Features] [Map]` (ticket W10-02).
//!
//! ## Why this module exists at all
//!
//! §3.3 specifies three tabs. What shipped was 94 lines implementing the
//! **Features** third of it: Compare lived in a bottom-bar menu, and Map
//! did not exist as a surface anywhere — the ultrawide camera was a
//! toggle in the View menu with the stitched canvas drawn into the play
//! area. So this is not a re-housing of existing UI, it is two thirds of
//! a specified workspace.
//!
//! ## Why the tab type lives here and not in `rf-debugger`
//!
//! The debug workspace's tab type is `rf_debugger::layout::DebugTab`, and
//! reusing it would have been the shortest path. It is the wrong one:
//! `rf-debugger` is a lower layer, and giving it `Compare`/`Map` variants
//! would make it carry enhancement concepts it has no business knowing.
//! Worse, `DebugTab` is what `rf_debugger::layout::PersistedLayout`
//! serialises — a versioned format with files already sitting in users'
//! config directories — so non-debug tabs would leak into a migration
//! story that has nothing to do with them. [`EnhanceTab`] is a separate
//! type in this crate, and the two workspaces share only `egui_dock`.
//!
//! **`crate::app` and `crate::debug_dock` are not the only modules
//! permitted to import `egui_dock`** — `debug_dock`'s module doc names
//! itself as "the one module in this crate (besides `crate::app`)", which
//! this module now joins. It is worth saying plainly because the opposite
//! claim ("only `debug_dock.rs` may depend on `egui_dock`") was written
//! into three commit messages and a ticket during W10-01 before anyone
//! checked; `scripts/validate-arch.sh` has never enforced anything of the
//! kind.
//!
//! ## The rendering/state split
//!
//! No `&mut RetroForgeApp` reaches this module. Tabs read what they need
//! through [`EnhanceCtx`] and report what the user did through
//! [`EnhanceActions`], which `crate::app` then acts on. That keeps
//! command sending, persistence and GPU work on the app side where the
//! rest of it lives, and it is what makes the tab bodies testable
//! without a running emulator.

use eframe::egui;
use egui_dock::{DockState, NodeIndex};

/// §3.3's three tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnhanceTab {
    /// Original vs enhanced, side by side or A/B.
    Compare,
    /// Per-feature rows with scope, provenance and the trust ladder.
    Features,
    /// The stitched canvas, with fog over what has not been visited.
    Map,
}

/// What a tab needs to draw itself.
pub struct EnhanceCtx<'a> {
    pub settings: &'a mut crate::game_settings::GameSettings,
    /// Ticket W20-09: what is true about the running game
    /// (`crate::enhance_ui::GameFacts`) — profile match, decoded level,
    /// collision, mode 7, widescreen support, fog renderer — replacing
    /// three loose bools that every caller had to pass in the same order.
    pub facts: crate::enhance_ui::GameFacts,
    pub compare_mode: &'a mut rf_renderer::CompareMode,
    pub compare_divider: &'a mut f32,
    /// The stitched-canvas texture, when one has been composited.
    pub map_texture: Option<&'a egui::TextureHandle>,
    /// Ticket W11-02: the DECODED level texture, when a profile matched
    /// and the full-level view is on. §3.3's Map is "stitched/decoded
    /// canvas" — both, and this is the second one.
    pub level_texture: Option<&'a egui::TextureHandle>,
    /// Where the live camera is in level space, and how big the original
    /// viewport is, for §3.3's viewport outline.
    pub level_camera: Option<(i64, i64)>,
    pub viewport_size: (f32, f32),
    /// Ticket W11-02: the matched profile's title and declared
    /// capabilities, for the inspector. It was hardcoded to `None` and an
    /// empty capability list, so the inspector said "no profile matched"
    /// on the same screen as a status bar reading "profile" — a
    /// contradiction a user could see and nobody had.
    pub profile_title: Option<String>,
    pub profile_capabilities: Vec<(&'static str, bool)>,
    /// Ticket W11-03: why each background did or did not widen. `Some` is
    /// a refusal with its reason.
    pub widescreen_decisions: [Option<&'static str>; 4],
    /// Ticket W32-03: "352 wide (patch)" while a widescreen patch decides
    /// the picture width.
    pub patch_aspect_label: Option<String>,
    /// Ticket W11-05: what the loaded HD pack's import said, what it
    /// could not satisfy, and what the last frame actually replaced.
    pub hd_summary: Option<&'a str>,
    pub hd_unsatisfied: &'a [String],
    pub hd_report: Option<rf_enhance::hd_render::CompositeReport>,
    /// Ticket W31-03: the Super FX HD compositor's last frame, as one line
    /// (score, offset, job age, masked share); `None` when it is off.
    pub superfx_hd_readout: Option<String>,
    /// FM-13's "view too large for GPU, reduced" message, if any.
    pub fm13_message: Option<&'a str>,
    /// Whether a GPU compositor exists at all. Without one there is
    /// nothing to render the map with, and saying so beats an empty pane.
    pub has_compositor: bool,
    /// Ticket W30-08: each governed chip's live status, from the last frame.
    pub governors: crate::clock_governor::GovernorReport,
}

/// What the user did, for `crate::app` to act on.
///
/// Out-parameters rather than callbacks: every one of these needs
/// something this module deliberately cannot reach — the core-thread
/// command channel, per-game settings persistence, the GPU compositor —
/// and threading closures for all of it would put the app's borrow graph
/// inside a `TabViewer`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnhanceActions {
    /// Per-game settings were edited and should be saved.
    pub settings_changed: bool,
    /// The sprite-limit-bypass overlay was toggled; the core must be told.
    pub sprite_overlay_set: Option<bool>,
    /// Ticket W11-01: temporal de-flicker was toggled; the core must be
    /// told. Before W11-01 this row wrote a bool to disk and nothing
    /// read it — the checkbox was the whole feature.
    pub deflicker_set: Option<bool>,
    /// Ticket W11-03: decoded widescreen was toggled; the core must be
    /// told, with the profile's policies.
    pub widescreen_set: Option<bool>,
    /// Ticket W30-03: the widescreen aspect picker changed; the core must
    /// be re-asked for the new width.
    pub widescreen_aspect_changed: bool,
    /// Ticket W11-02: the full-level view was toggled; the level probe
    /// must be armed or disarmed.
    pub full_level_set: Option<bool>,
    /// Ticket W16-13: Diorama was toggled — the level probe and sprite-
    /// layer extraction must be (re)synced (`RetroForgeApp::
    /// sync_diorama_subscription`); `ctx.settings.diorama` itself is
    /// already written directly by `features_body` below (the same
    /// pattern `widescreen_decoded`/`full_level_view` use), so this only
    /// needs to be `Some` to trigger the sync, not carry the new value.
    pub diorama_set: Option<bool>,
    /// Ticket W16-14: "Mode 7 as 3D" was toggled — same "trigger a sync,
    /// the field itself is already written directly" shape as
    /// `diorama_set` above (`RetroForgeApp::sync_diorama_subscription`
    /// picks it up alongside the walls toggle).
    pub mode7_ground_set: Option<bool>,
    /// Ticket W20-17.
    pub loading_fast_forward_set: Option<bool>,
    /// The Compare tab asked for a both-buffers screenshot.
    pub screenshot_requested: bool,
}

/// The workspace itself.
pub struct EnhanceWorkspace {
    dock_state: DockState<EnhanceTab>,
}

impl EnhanceWorkspace {
    /// Compare and Features side by side, Map below.
    ///
    /// Not all three as one tab strip: Compare and Features are used
    /// *together* — you toggle a feature and look at what it did to the
    /// picture — and a layout that makes you flip between them hides the
    /// only thing the pairing is for. Map is the odd one out (a whole
    /// level, not a frame), so it gets the lower half. The user can drag
    /// any of it anywhere; this is only where it starts.
    #[must_use]
    pub fn new() -> Self {
        let mut dock_state = DockState::new(vec![EnhanceTab::Compare]);
        let surface = dock_state.main_surface_mut();
        let [left, _right] =
            surface.split_right(NodeIndex::root(), 0.55, vec![EnhanceTab::Features]);
        let _ = surface.split_below(left, 0.55, vec![EnhanceTab::Map]);
        Self { dock_state }
    }

    /// Which tabs are currently open, in tree order. Exists so a test can
    /// assert §3.3's three tabs are all present without a window — the
    /// alternative is asserting on rendered tab titles, which passes just
    /// as well when two of them are stacked invisibly behind the third.
    #[must_use]
    pub fn open_tabs(&self) -> Vec<EnhanceTab> {
        self.dock_state
            .iter_all_tabs()
            .map(|(_, tab)| *tab)
            .collect()
    }

    /// Draw the workspace, returning what the user did.
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut EnhanceCtx<'_>) -> EnhanceActions {
        let mut viewer = Viewer {
            ctx,
            actions: EnhanceActions::default(),
        };
        egui_dock::DockArea::new(&mut self.dock_state)
            .style(egui_dock::Style::from_egui(ui.style().as_ref()))
            .show_inside(ui, &mut viewer);
        viewer.actions
    }
}

impl Default for EnhanceWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

struct Viewer<'a, 'c> {
    ctx: &'a mut EnhanceCtx<'c>,
    actions: EnhanceActions,
}

impl egui_dock::TabViewer for Viewer<'_, '_> {
    type Tab = EnhanceTab;

    fn title(&mut self, tab: &mut EnhanceTab) -> egui::WidgetText {
        match tab {
            EnhanceTab::Compare => "Compare",
            EnhanceTab::Features => "Features",
            EnhanceTab::Map => "Map",
        }
        .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut EnhanceTab) {
        match tab {
            EnhanceTab::Compare => compare_ui(ui, self.ctx, &mut self.actions),
            EnhanceTab::Features => features_ui(ui, self.ctx, &mut self.actions),
            EnhanceTab::Map => map_ui(ui, self.ctx),
        }
    }
}

/// §3.3's Compare tab (ticket W3-04, FR-REND-005/FR-FE-005).
///
/// Off by default: it is a comparison instrument, and law 6's "a fresh
/// install boots in Accuracy Mode" reads the same way here — what you see
/// by default is the emulator's own output, not a reading of it.
fn compare_ui(ui: &mut egui::Ui, ctx: &mut EnhanceCtx<'_>, actions: &mut EnhanceActions) {
    scrolled(ui, |ui| compare_body(ui, ctx, actions));
}

fn compare_body(ui: &mut egui::Ui, ctx: &mut EnhanceCtx<'_>, actions: &mut EnhanceActions) {
    let mut mode = *ctx.compare_mode;
    ui.radio_value(&mut mode, rf_renderer::CompareMode::Off, "Off");
    ui.radio_value(
        &mut mode,
        rf_renderer::CompareMode::Split {
            divider: *ctx.compare_divider,
        },
        "Split screen",
    );
    ui.radio_value(
        &mut mode,
        rf_renderer::CompareMode::Blink { period_frames: 30 },
        "A/B blink",
    );
    if let rf_renderer::CompareMode::Split { .. } = mode {
        // The "draggable divider" RENDERER.md §5 asks for. A slider
        // rather than a hit-tested drag handle: same control, and it
        // works with a keyboard.
        ui.add(egui::Slider::new(ctx.compare_divider, 0.0..=1.0).text("Divider"));
        mode = rf_renderer::CompareMode::Split {
            divider: *ctx.compare_divider,
        };
    }
    *ctx.compare_mode = mode;
    ui.separator();
    if ui.button("Screenshot (both buffers)").clicked() {
        actions.screenshot_requested = true;
    }
}

/// §3.3's Features tab: one row per feature, with scope and provenance.
fn features_ui(ui: &mut egui::Ui, ctx: &mut EnhanceCtx<'_>, actions: &mut EnhanceActions) {
    // **A dock pane is a container the user can shrink to nothing.** This
    // tab's content is unbounded — a feature row per registry entry, a
    // report card that grows with every contradiction recorded, and a
    // profile inspector listing every precedence layer — so without a
    // scroll area the bottom of it is simply unreachable. It shipped that
    // way in W10-02's first build: the rendered frame cut the profile
    // inspector off at "1. base profile" with no way to see line 2, and
    // no assertion noticed, because the widget tree is identical whether
    // or not the pixels are reachable.
    scrolled(ui, |ui| features_body(ui, ctx, actions));
}

/// A pane's content, in a vertical scroll area that fills the pane.
///
/// `auto_shrink` off in both axes: left on, the scroll area shrinks to
/// its content and the pane's background stops where the text stops,
/// which reads as a rendering fault rather than as empty space.
fn scrolled<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, body)
        .inner
}

/// Ticket W20-18: turn feature `id` on or off — the settings write and
/// the action the app applies (`RetroForgeApp::apply_enhance_actions`).
/// Shared by this workspace's Features tab and the Quick Menu's player
/// panel, so a toggle is wired one way only.
pub fn apply_toggle(
    settings: &mut crate::game_settings::GameSettings,
    id: &str,
    enabled: bool,
    actions: &mut EnhanceActions,
) {
    let ctx_settings = settings;
    match id {
        "sprite_overlay" => {
            ctx_settings.sprite_overlay = enabled;
            actions.sprite_overlay_set = Some(enabled);
        }
        "deflicker" => {
            ctx_settings.deflicker = enabled;
            actions.deflicker_set = Some(enabled);
        }
        "widescreen_decoded" => {
            // Until W11-03 this ONLY set the field: the toggle
            // round-tripped to disk and changed nothing anyone
            // could see, because no command existed to carry it
            // to the core.
            ctx_settings.widescreen_decoded = enabled;
            actions.widescreen_set = Some(enabled);
        }
        "full_level_view" => {
            ctx_settings.full_level_view = enabled;
            actions.full_level_set = Some(enabled);
        }
        "diorama" => {
            // Ticket W16-13: this arm was missing entirely
            // (fell into `_ => {}` below) — the checkbox
            // toggled its own LOCAL `enabled` copy and
            // `ctx_settings.diorama` was never actually
            // written, so Diorama could never be turned on
            // through this UI at all (the same class of "row
            // exists, wiring doesn't" bug W16-06 already found
            // once for `profile_matched`).
            ctx_settings.diorama = enabled;
            actions.diorama_set = Some(enabled);
        }
        "mode7_ground" => {
            ctx_settings.mode7_ground = enabled;
            actions.mode7_ground_set = Some(enabled);
        }
        "loading_fast_forward" => {
            ctx_settings.loading_fast_forward = enabled;
            actions.loading_fast_forward_set = Some(enabled);
        }
        // Ticket W30-01: the app's `sync_clocks` reads the
        // setting every frame, so no dedicated action is needed.
        "superfx_overclock" => ctx_settings.superfx_overclock = enabled,
        // Ticket W30-08: likewise read every frame by `sync_clocks`. The
        // NES row reads on at 1+ lines, so switching it on starts at a
        // moderate amount.
        "cpu_overclock" => ctx_settings.cpu_overclock = enabled,
        "sa1_overclock" => ctx_settings.sa1_overclock = enabled,
        "nes_extra_lines" => {
            ctx_settings.nes_extra_lines = if !enabled {
                0
            } else if ctx_settings.nes_extra_lines == 0 {
                crate::game_settings::NES_EXTRA_LINES_ON
            } else {
                ctx_settings.nes_extra_lines
            };
        }
        // Ticket W30-07: likewise read every frame by `sync_run_ahead`. The
        // row reads on at 1 or 2 frames, so switching it on starts at 1.
        "run_ahead" => ctx_settings.run_ahead = u8::from(enabled),
        // Ticket W30-02: likewise read every frame by `sync_hd_mode7`.
        "hd_mode7" => ctx_settings.hd_mode7 = enabled,
        // Ticket W31-03: read every frame by `sync_superfx_hd`.
        "superfx_hd" => ctx_settings.superfx_hd = enabled,
        // Ticket W30-09: likewise read every frame by `sync_snes_interpolation`.
        "snes_interpolation" => ctx_settings.snes_interpolation = enabled,
        // Ticket W30-04: the row reads on when any effect is, so its own
        // switch turns the whole set on (a starting look) or off. The app
        // reads the settings every frame; no command exists to send.
        "hd2d_lens" => {
            if enabled {
                ctx_settings.lens_start();
            } else {
                ctx_settings.lens_clear();
            }
        }
        _ => {}
    }
    actions.settings_changed = true;
}

/// Ticket W30-03: the widescreen aspect choices (16:9 / 21:9 / 32:9) as a
/// row of selectable labels. `true` when the choice changed. Shared by the
/// Enhance workspace and the Enhancements cards so the two cannot drift.
pub fn aspect_picker(ui: &mut egui::Ui, aspect: &mut rf_enhance::widescreen::Aspect) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Aspect");
        for choice in rf_enhance::widescreen::Aspect::ALL {
            if ui
                .selectable_label(*aspect == choice, choice.label())
                .clicked()
                && *aspect != choice
            {
                *aspect = choice;
                changed = true;
            }
        }
    });
    changed
}

/// Ticket W30-07: the run-ahead choice (1 or 2 frames) as a pair of
/// selectable labels. `true` when the choice changed. Shared by the Enhance
/// workspace and the Enhancements cards so the two cannot drift.
pub fn run_ahead_picker(ui: &mut egui::Ui, frames: &mut u8) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Frames ahead");
        for (choice, label) in [(1u8, "1 frame"), (2, "2 frames")] {
            if ui.selectable_label(*frames == choice, label).clicked() && *frames != choice {
                *frames = choice;
                changed = true;
            }
        }
    });
    changed
}

/// Ticket W31-05: the Super FX HD scale slider (2x-8x) and the edge-smoothing
/// choice (off / 4x / 8x multisampling on the GPU path), under the toggle.
/// `true` when either changed. Shared by the Enhance workspace and the
/// Enhancements cards.
pub fn superfx_hd_controls(ui: &mut egui::Ui, s: &mut crate::game_settings::GameSettings) -> bool {
    use crate::game_settings::{SUPERFX_HD_SCALE_MAX, SUPERFX_HD_SCALE_MIN};
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        changed |= ui
            .add(
                egui::Slider::new(
                    &mut s.superfx_hd_scale,
                    SUPERFX_HD_SCALE_MIN..=SUPERFX_HD_SCALE_MAX,
                )
                .suffix("x"),
            )
            .changed();
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Edge smoothing");
        for (samples, label) in [(1u8, "Off"), (4, "4x"), (8, "8x")] {
            changed |= ui
                .selectable_value(&mut s.superfx_hd_msaa, samples, label)
                .changed();
        }
    });
    changed
}

/// Ticket W30-09: the audio-interpolation filter choice (cubic, linear or
/// none), under its toggle. `true` when it changed. Shared by the Enhance
/// workspace and the Enhancements cards like [`diorama_controls`].
pub fn interpolation_controls(
    ui: &mut egui::Ui,
    s: &mut crate::game_settings::GameSettings,
) -> bool {
    use rf_snes::apu::dsp::Interpolation;
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        for (mode, label) in [
            (Interpolation::Cubic, "Cubic"),
            (Interpolation::Linear, "Linear"),
            (Interpolation::Nearest, "None"),
        ] {
            changed |= ui
                .selectable_value(&mut s.snes_interpolation_mode, mode, label)
                .changed();
        }
    });
    changed
}

/// Ticket W30-05: the diorama camera controls -- tilt, turn and zoom
/// sliders, plus (for the walls diorama only) a "Follow the player" switch.
/// The Mode 7 ground has no level to scroll across, so `show_follow` is
/// `false` there. `true` when anything changed. Shared by the Enhance
/// workspace and the Enhancements cards so the two cannot drift.
pub fn diorama_controls(
    ui: &mut egui::Ui,
    s: &mut crate::game_settings::GameSettings,
    show_follow: bool,
) -> bool {
    use crate::game_settings::{
        DIORAMA_PITCH_MAX, DIORAMA_PITCH_MIN, DIORAMA_YAW_LIMIT, DIORAMA_ZOOM_DEFAULT,
        DIORAMA_ZOOM_MAX,
    };
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label("Tilt");
        changed |= ui
            .add(
                egui::Slider::new(&mut s.diorama_pitch, DIORAMA_PITCH_MIN..=DIORAMA_PITCH_MAX)
                    .suffix("\u{b0}"),
            )
            .changed();
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Turn");
        changed |= ui
            .add(
                egui::Slider::new(&mut s.diorama_yaw, -DIORAMA_YAW_LIMIT..=DIORAMA_YAW_LIMIT)
                    .suffix("\u{b0}"),
            )
            .changed();
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Zoom");
        changed |= ui
            .add(
                egui::Slider::new(&mut s.diorama_zoom, DIORAMA_ZOOM_DEFAULT..=DIORAMA_ZOOM_MAX)
                    .suffix("%"),
            )
            .changed();
    });
    if show_follow {
        changed |= ui
            .checkbox(&mut s.diorama_follow, "Follow the player")
            .changed();
    }
    changed
}

/// Ticket W30-04: the HD-2D lens controls -- a checkbox and a strength
/// slider per effect, the weather picker, and the time of day with its
/// "follow my clock" switch. `true` when anything changed. Shared by the
/// Enhance workspace and the Enhancements cards so the two cannot drift.
pub fn lens_controls(ui: &mut egui::Ui, s: &mut crate::game_settings::GameSettings) -> bool {
    let mut changed = false;
    for (label, on, pct) in [
        ("Glow (bloom)", &mut s.lens_bloom, &mut s.lens_bloom_pct),
        (
            "Miniature focus (tilt-shift)",
            &mut s.lens_tiltshift,
            &mut s.lens_tiltshift_pct,
        ),
        (
            "Soft shadows under sprites",
            &mut s.lens_shadows,
            &mut s.lens_shadows_pct,
        ),
        (
            "Day and night light",
            &mut s.lens_grade,
            &mut s.lens_grade_pct,
        ),
    ] {
        ui.horizontal_wrapped(|ui| {
            changed |= ui.checkbox(on, label).changed();
            changed |= ui
                .add_enabled(*on, egui::Slider::new(pct, 0..=100).suffix("%"))
                .changed();
        });
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Weather");
        for choice in rf_renderer::lens::Weather::ALL {
            if ui
                .selectable_label(s.lens_weather == choice, choice.label())
                .clicked()
                && s.lens_weather != choice
            {
                s.lens_weather = choice;
                changed = true;
            }
        }
        changed |= ui
            .add_enabled(
                s.lens_weather != rf_renderer::lens::Weather::None,
                egui::Slider::new(&mut s.lens_weather_pct, 0..=100).suffix("%"),
            )
            .changed();
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Time of day");
        changed |= ui
            .add_enabled(
                s.lens_grade && !s.lens_follow_clock,
                egui::Slider::new(&mut s.lens_time_of_day, 0..=100)
                    .custom_formatter(|n, _| crate::lens_view::clock_label(n as u8)),
            )
            .changed();
        changed |= ui
            .add_enabled(
                s.lens_grade,
                egui::Checkbox::new(&mut s.lens_follow_clock, "Follow my clock"),
            )
            .changed();
    });
    changed
}

fn features_body(ui: &mut egui::Ui, ctx: &mut EnhanceCtx<'_>, actions: &mut EnhanceActions) {
    let rows = crate::enhance_ui::feature_rows(ctx.settings, &ctx.facts);
    for row in &rows {
        // **`horizontal_wrapped`, and the opposite call from the status
        // bar.** W10-01 refused wrapping for the bottom bar because three
        // rows of mixed buttons and combos is uglier than one clipped row
        // and hides the spec gap. The trade is reversed here: this is a
        // dock pane the user can make as narrow as they like, its content
        // is a checkbox followed by prose (scope, ladder chip, the reason
        // a row is unavailable), and prose that wraps is normal while
        // prose cut off at "(requires pro" is not. Rendered at the
        // default pane width, these rows clipped exactly there.
        ui.horizontal_wrapped(|ui| {
            let available = row.availability == crate::enhance_ui::Availability::Available;
            let mut enabled = row.enabled;
            // Unavailable rows are shown DISABLED and explained, never
            // hidden — §3.3's honesty contract. Hiding them would tell a
            // user the feature does not exist.
            if ui
                .add_enabled(available, egui::Checkbox::new(&mut enabled, row.label))
                .changed()
            {
                apply_toggle(ctx.settings, row.id, enabled, actions);
                actions.settings_changed = true;
            }
            ui.label(format!("({})", row.scope));
            if let Some(h) = row.heuristic {
                ui.label(format!(
                    "[{}]",
                    crate::enhance_ui::ladder_chip(&ctx.settings.trust, h)
                ));
            }
            if let Some(why) = row.availability.explanation() {
                ui.label(format!("\u{2014} {why}"));
            }
        });
        // **Ticket W11-03: a refusal says so, right under the toggle that
        // caused it.** bsnes-hd's lesson is that widening the wrong layer
        // looks like a bug in the GAME — a status bar smeared across the
        // margins — so the policy declines those layers. A user who is
        // not told why simply sees a picture narrower than they asked for
        // and concludes the feature is broken.
        if row.id == "widescreen_decoded" {
            if let Some(label) = &ctx.patch_aspect_label {
                ui.label(format!("    Picture: {label}"));
            }
        }
        if row.id == "widescreen_decoded" && row.enabled {
            for (i, reason) in ctx.widescreen_decisions.iter().enumerate() {
                if let Some(why) = reason {
                    ui.label(format!("    BG{}: kept 4:3 \u{2014} {why}", i + 1));
                }
            }
            // Ticket W30-03: the screen shape to aim for.
            if aspect_picker(ui, &mut ctx.settings.widescreen_aspect) {
                actions.settings_changed = true;
                actions.widescreen_aspect_changed = true;
            }
        }
        // Ticket W30-08: each clock's ceiling, mode and live status, and the
        // NES line count, right under their toggles.
        if row.enabled {
            match row.id {
                "cpu_overclock" => {
                    if crate::overclock_ui::clock_controls(
                        ui,
                        &mut ctx.settings.cpu_overclock_pct,
                        &mut ctx.settings.cpu_overclock_mode,
                        ctx.facts.cpu_max,
                        ctx.governors.cpu.as_ref(),
                        None,
                    ) {
                        actions.settings_changed = true;
                    }
                }
                "superfx_overclock" => {
                    // Ticket W30-01c: the Super FX shares the clock card.
                    if crate::overclock_ui::clock_controls(
                        ui,
                        &mut ctx.settings.superfx_overclock_pct,
                        &mut ctx.settings.superfx_overclock_mode,
                        ctx.facts.superfx_max,
                        ctx.governors.gsu.as_ref(),
                        None,
                    ) {
                        actions.settings_changed = true;
                    }
                }
                "sa1_overclock" => {
                    if crate::overclock_ui::clock_controls(
                        ui,
                        &mut ctx.settings.sa1_overclock_pct,
                        &mut ctx.settings.sa1_overclock_mode,
                        ctx.facts.sa1_max,
                        ctx.governors.sa1.as_ref(),
                        None,
                    ) {
                        actions.settings_changed = true;
                    }
                }
                "nes_extra_lines" => {
                    if crate::overclock_ui::nes_lines_controls(
                        ui,
                        &mut ctx.settings.nes_extra_lines,
                        ctx.facts.nes_lines_max,
                        None,
                    ) {
                        actions.settings_changed = true;
                    }
                }
                _ => {}
            }
        }
        // Ticket W30-07: how many frames ahead, right under its toggle.
        if row.id == "run_ahead" && row.enabled && run_ahead_picker(ui, &mut ctx.settings.run_ahead)
        {
            actions.settings_changed = true;
        }
        // Ticket W30-04: the lens options, whenever the row can act (so a
        // player can tick the first effect, which is what turns it on).
        if row.id == "hd2d_lens"
            && row.availability == crate::enhance_ui::Availability::Available
            && lens_controls(ui, ctx.settings)
        {
            actions.settings_changed = true;
        }
        // Ticket W30-05: the camera, under both rows that use it. The
        // Mode 7 ground has no level to scroll, so no "follow" there.
        if (row.id == "diorama" || row.id == "mode7_ground")
            && row.enabled
            && diorama_controls(ui, ctx.settings, row.id == "diorama")
        {
            actions.settings_changed = true;
        }
        // Ticket W30-09: which smoother filter, right under its toggle.
        if row.id == "snes_interpolation" && row.enabled && interpolation_controls(ui, ctx.settings)
        {
            actions.settings_changed = true;
        }
        // Ticket W31-03: the Super FX HD scale, right under its toggle.
        if row.id == "superfx_hd" && row.enabled {
            if superfx_hd_controls(ui, ctx.settings) {
                actions.settings_changed = true;
            }
            if let Some(line) = &ctx.superfx_hd_readout {
                ui.weak(line);
            }
        }
        // Ticket W30-02: the HD Mode 7 options, right under its toggle.
        if row.id == "hd_mode7" && row.enabled {
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Slider::new(&mut ctx.settings.hd_mode7_scale, 2..=4).suffix("x"))
                    .changed()
                {
                    actions.settings_changed = true;
                }
            });
            if ui
                .checkbox(
                    &mut ctx.settings.hd_mode7_supersample,
                    "Supersample (smoother edges)",
                )
                .changed()
            {
                actions.settings_changed = true;
            }
            if ui
                .checkbox(
                    &mut ctx.settings.hd_mode7_perspective,
                    "Perspective smoothing",
                )
                .changed()
            {
                actions.settings_changed = true;
            }
        }
    }

    // **Ticket W11-05: an HD pack reports itself, in the UI.**
    //
    // Three different facts, and conflating them would hide the one
    // that matters: what the pack CLAIMS (rules parsed), what could
    // not be honoured (missing images, conditions this build cannot
    // evaluate), and what the last frame actually REPLACED. A pack
    // can import cleanly and still replace nothing, which looks
    // identical to "the feature is broken" without the third line.
    if let Some(summary) = ctx.hd_summary {
        ui.separator();
        egui::CollapsingHeader::new("HD pack")
            .default_open(true)
            .show(ui, |ui| {
                ui.label(summary);
                if let Some(r) = ctx.hd_report {
                    ui.label(r.summary());
                }
                // (W11-05 printed a "background tiles only" limitation
                // here. W11-14 implemented sprite replacement, so the
                // notice is GONE rather than left to mislead — a stale
                // limitation notice is its own kind of dishonesty.)
                for u in ctx.hd_unsatisfied {
                    ui.label(format!("    {u}"));
                }
            });
    }

    ui.separator();
    // Ticket W3-05c (FR-ENH-012): the per-game report card, surfaced
    // locally and only locally — NFR-005 forbids telemetry, and
    // `rf_enhance::trust` has no I/O of any kind, so there is nowhere for
    // this to leak to even by accident.
    egui::CollapsingHeader::new("Report card (D-004)").show(ui, |ui| {
        let card = ctx.settings.trust.report_card();
        if card.is_empty() {
            ui.label("No contradictions recorded this session.");
        }
        for c in card {
            ui.label(format!(
                "[{}] {} \u{2014} {}",
                c.scene, c.heuristic, c.detail
            ));
        }
    });

    ui.separator();
    egui::CollapsingHeader::new("Profile inspector")
        .default_open(true)
        .show(ui, |ui| {
            for line in crate::enhance_ui::profile_inspector_lines(
                ctx.profile_title.as_deref(),
                &ctx.profile_capabilities,
                &[
                    "base profile".to_string(),
                    "user overrides (profiles.d)".to_string(),
                    "per-session toggles".to_string(),
                ],
            ) {
                ui.label(line);
            }
        });
}

/// §3.3's Map tab: the stitched canvas, fog over what has not been
/// visited.
///
/// **Every empty case says which empty case it is.** A map pane can be
/// blank for three unrelated reasons — no GPU compositor, no ROM running,
/// nothing stitched yet — and they send the user to three different
/// places. This is the same rule design review G-21 imposed on the
/// library's empty states, and the reason it is worth repeating is that a
/// blank rectangle is indistinguishable from a bug.
fn map_ui(ui: &mut egui::Ui, ctx: &EnhanceCtx<'_>) {
    // §3.3's Map is "stitched/decoded canvas" — BOTH. The decoded level
    // comes first when it exists, because it is the authored truth about
    // the level while the stitched canvas is what play happened to
    // reveal.
    if let Some(level) = ctx.level_texture {
        let response = ui.add(egui::Image::from_texture(level).shrink_to_fit());
        draw_viewport_outline(ui, response.rect, ctx);
        ui.add_space(4.0);
        ui.add(crate::app::readout(
            egui::RichText::new(
                "Decoded from the game's own level data via its profile. The outline is where \
                 the original 256x240 viewport is right now.",
            )
            .weak(),
        ));
        return;
    }
    // The image branch below is deliberately NOT inside a scroll area:
    // `shrink_to_fit` sizes the canvas to the pane, so it cannot overflow
    // — and inside a scroll area `available_size` is effectively
    // unbounded, which would make `shrink_to_fit` grow the image without
    // limit instead of fitting it.
    if !ctx.has_compositor {
        ui.label("No GPU device, so no stitched map can be composited.");
        return;
    }
    if let Some(msg) = ctx.fm13_message {
        // FM-13 criterion 3: surfaced plainly, never swallowed.
        ui.colored_label(egui::Color32::from_rgb(230, 180, 40), msg);
    }
    match ctx.map_texture {
        Some(texture) => {
            ui.add(egui::Image::from_texture(texture).shrink_to_fit());
        }
        None => {
            ui.label("Nothing stitched yet \u{2014} play for a moment and the map fills in.");
            ui.add_space(4.0);
            ui.weak(
                "Unvisited areas stay fogged rather than guessed (FR-ENH-004): the map shows \
                 where you have been, not where the game might go.",
            );
        }
    }
}

/// §3.3's "original-viewport outline", over the decoded level.
///
/// Recorded as impossible in W10-02's notes — correctly, at the time: the
/// canvas-space position of the live viewport never reached the UI
/// thread. W11-02's bounded probe supplies it for the LEVEL view, where
/// the camera's own address is declared by the profile. It is still
/// unavailable over the STITCHED canvas, which is a different coordinate
/// space with no profile behind it.
fn draw_viewport_outline(ui: &egui::Ui, image: egui::Rect, ctx: &EnhanceCtx<'_>) {
    let Some((cam_x, cam_y)) = ctx.level_camera else {
        return;
    };
    let Some(level) = ctx.level_texture else {
        return;
    };
    let size = level.size();
    let (lw, lh) = (size[0] as f32, size[1] as f32);
    if lw <= 0.0 || lh <= 0.0 || image.width() <= 0.0 {
        return;
    }
    // Level pixels -> screen pixels. The image was drawn with
    // `shrink_to_fit`, so one scale applies to both axes.
    let scale = image.width() / lw;
    let (vw, vh) = ctx.viewport_size;
    let origin = egui::pos2(
        image.left() + (cam_x as f32).clamp(0.0, lw) * scale,
        image.top() + (cam_y as f32).clamp(0.0, lh) * scale,
    );
    let rect = egui::Rect::from_min_size(origin, egui::vec2(vw * scale, vh * scale));
    ui.painter().rect_stroke(
        rect.intersect(image),
        0.0,
        egui::Stroke::new(1.0, ui.visuals().selection.stroke.color),
        egui::StrokeKind::Inside,
    );
}

#[cfg(test)]
mod overclock_toggle_tests {
    use super::*;
    use crate::game_settings::{GameSettings, NES_EXTRA_LINES_ON};

    /// Ticket W30-08: the three rows' switches write their settings; the
    /// NES row reads as on at 1+ lines, so switching it on starts at a
    /// moderate amount and keeps a slider value already chosen.
    #[test]
    fn the_overclock_switches_write_their_settings() {
        let mut s = GameSettings::default();
        let mut actions = EnhanceActions::default();
        apply_toggle(&mut s, "cpu_overclock", true, &mut actions);
        apply_toggle(&mut s, "sa1_overclock", true, &mut actions);
        assert!(s.cpu_overclock && s.sa1_overclock);
        apply_toggle(&mut s, "cpu_overclock", false, &mut actions);
        assert!(!s.cpu_overclock && s.sa1_overclock);

        apply_toggle(&mut s, "nes_extra_lines", true, &mut actions);
        assert_eq!(s.nes_extra_lines, NES_EXTRA_LINES_ON);
        s.nes_extra_lines = 100;
        apply_toggle(&mut s, "nes_extra_lines", true, &mut actions);
        assert_eq!(s.nes_extra_lines, 100, "an existing amount is kept");
        apply_toggle(&mut s, "nes_extra_lines", false, &mut actions);
        assert_eq!(s.nes_extra_lines, 0);
        assert!(actions.settings_changed);
    }
}
