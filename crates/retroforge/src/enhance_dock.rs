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
    pub profile_matched: bool,
    pub compare_mode: &'a mut rf_renderer::CompareMode,
    pub compare_divider: &'a mut f32,
    /// The stitched-canvas texture, when one has been composited.
    pub map_texture: Option<&'a egui::TextureHandle>,
    /// FM-13's "view too large for GPU, reduced" message, if any.
    pub fm13_message: Option<&'a str>,
    /// Whether a GPU compositor exists at all. Without one there is
    /// nothing to render the map with, and saying so beats an empty pane.
    pub has_compositor: bool,
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
    let rows = crate::enhance_ui::feature_rows(ctx.settings, ctx.profile_matched);
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
                match row.id {
                    "sprite_overlay" => {
                        ctx.settings.sprite_overlay = enabled;
                        actions.sprite_overlay_set = Some(enabled);
                    }
                    "deflicker" => ctx.settings.deflicker = enabled,
                    "widescreen_decoded" => ctx.settings.widescreen_decoded = enabled,
                    "full_level_view" => ctx.settings.full_level_view = enabled,
                    _ => {}
                }
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
                None,
                &[],
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
