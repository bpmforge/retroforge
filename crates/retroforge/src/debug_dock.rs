//! `egui_dock` wiring for the debug-viewer panels (ticket W4-06a criterion
//! 3, FR-FE-004) — the one module in this crate (besides `crate::app`)
//! allowed to depend on `egui_dock`. This is deliberately kept separate
//! from `crate::app` (rather than folded into it) so
//! [`capture_layout`]/[`restore_layout`] — the two functions that actually
//! matter for "layout persisted" — are unit-testable without a window:
//! `egui_dock::DockState`/`Tree`/`Node` are all plain data structures with
//! no rendering dependency, so building one, walking it, and round-
//! tripping it through TOML needs no `eframe`/GPU context at all.
//!
//! `rf_debugger::layout` owns the actual persisted *format*
//! ([`rf_debugger::layout::PersistedLayout`]) — see that module's doc for
//! why it is a small hand-rolled format rather than a serialized
//! `DockState` directly. This module is only the translation layer between
//! that format and a live `egui_dock::DockState<DebugTab>`.
use std::path::PathBuf;

use egui_dock::{DockState, LeafNode, Node, NodeIndex, Split, Tree};
use rf_debugger::layout::{self, DebugTab, PersistedLayout, PersistedNode, SplitAxis};

/// Placeholder tab momentarily written by [`graft`] to satisfy
/// `Tree::split`'s `new.tabs_count() != 0` assertion before the real
/// content is grafted in on top of it — never actually shown to a user
/// (every `graft` call immediately overwrites the position it creates).
/// Same "not a considered policy, just enough to make the real mechanism
/// work" caveat `crate::core_thread::canvas_cache_root` already uses for
/// an unrelated placeholder.
const PLACEHOLDER_TAB: DebugTab = DebugTab::Pattern;

/// Walk a live `Tree<DebugTab>` starting at `index`, building the
/// [`PersistedNode`] it corresponds to. `index.left()`/`index.right()`
/// (`egui_dock::NodeIndex`'s own doc) are always in-bounds for a
/// `Vertical`/`Horizontal` node — `Tree::split` grows the backing `Vec` to
/// fit before ever writing a split node, so this never indexes out of
/// range for any tree `egui_dock` itself produced or [`restore_layout`]
/// below built.
fn capture_node(tree: &Tree<DebugTab>, index: NodeIndex) -> PersistedNode {
    match &tree[index] {
        Node::Empty => PersistedNode::Leaf {
            tabs: Vec::new(),
            active: 0,
        },
        Node::Leaf(leaf) => PersistedNode::Leaf {
            tabs: leaf.tabs.clone(),
            active: leaf.active.0,
        },
        Node::Vertical(split) => PersistedNode::Split {
            axis: SplitAxis::Vertical,
            fraction: split.fraction,
            first: Box::new(capture_node(tree, index.left())),
            second: Box::new(capture_node(tree, index.right())),
        },
        Node::Horizontal(split) => PersistedNode::Split {
            axis: SplitAxis::Horizontal,
            fraction: split.fraction,
            first: Box::new(capture_node(tree, index.left())),
            second: Box::new(capture_node(tree, index.right())),
        },
    }
}

/// Capture `dock_state`'s current main-surface arrangement (tabs, split
/// axes, split fractions) into this crate's persisted format. Floating
/// windows/extra surfaces (`egui_dock::Surface::Window`) are out of this
/// ticket's scope — the debugger panel set is small enough that a single
/// main-surface tree covers it; only `main_surface()` is captured.
#[must_use]
pub fn capture_layout(dock_state: &DockState<DebugTab>) -> PersistedLayout {
    PersistedLayout {
        version: layout::LAYOUT_FORMAT_VERSION,
        root: capture_node(dock_state.main_surface(), NodeIndex::root()),
    }
}

/// Recursively rebuild `node` into `tree` at `at`, which must already hold
/// *some* valid node (true for `NodeIndex::root()` right after
/// `DockState::new`, and for every child index this function itself just
/// created via `Tree::split`).
fn graft(tree: &mut Tree<DebugTab>, at: NodeIndex, node: &PersistedNode) {
    match node {
        PersistedNode::Leaf { tabs, active } => {
            let mut leaf = LeafNode::new(tabs.clone());
            // Out-of-range `active` (e.g. a hand-edited or stale layout
            // file) degrades to the `LeafNode::new` default (tab 0) rather
            // than erroring — a corrupted layout file is not a reason to
            // refuse to open the debugger (`rf_debugger::layout::
            // from_toml_str`'s own doc makes the same call one level up).
            let _ = leaf.set_active_tab(*active);
            tree[at] = Node::Leaf(leaf);
        }
        PersistedNode::Split {
            axis,
            fraction,
            first,
            second,
        } => {
            // Any of the four `Split` variants works here — NOT just
            // `Below`/`Right` as an earlier version of this comment
            // (wrongly) claimed. `Tree::split`'s `index = ...` table (the
            // vendored source) does map `Above`/`Left` to a swapped
            // `[parent.right(), parent.left()]` vs. `Below`/`Right`'s
            // `[parent.left(), parent.right()]`, but the two `graft` calls
            // below unconditionally OVERWRITE both `at.left()` and
            // `at.right()` immediately afterward regardless of which
            // physical index briefly held which placeholder — so whatever
            // `split()` transiently assigned is always clobbered before it
            // could matter. **Verified by mutation, not assumed**: swapping
            // this match to `Above`/`Left` left every test in this module
            // passing unchanged. `Below`/`Right` is kept anyway as the more
            // intuitive reading (`first` conceptually comes "first", i.e.
            // stays put; `second` is the newly-added side) — a future
            // reader should not "fix" this thinking it's load-bearing.
            let split_dir = match axis {
                SplitAxis::Vertical => Split::Below,
                SplitAxis::Horizontal => Split::Right,
            };
            tree.split(
                at,
                split_dir,
                *fraction,
                Node::leaf_with(vec![PLACEHOLDER_TAB]),
            );
            graft(tree, at.left(), first);
            graft(tree, at.right(), second);
        }
    }
}

/// Rebuild a fresh `DockState<DebugTab>` from a persisted layout — the
/// inverse of [`capture_layout`]. Always succeeds (a malformed
/// [`PersistedLayout`] — e.g. an out-of-range `active` index — degrades
/// field-by-field rather than erroring; see [`graft`]'s doc).
#[must_use]
pub fn restore_layout(layout: &PersistedLayout) -> DockState<DebugTab> {
    let mut dock_state = DockState::new(vec![PLACEHOLDER_TAB]);
    graft(
        dock_state.main_surface_mut(),
        NodeIndex::root(),
        &layout.root,
    );
    dock_state
}

/// Placeholder persistence location — same "not a considered app-data-
/// directory policy, just a real writable location" caveat
/// `crate::core_thread::canvas_cache_root` already carries for the exact
/// same class of problem (no XDG/platform-data-dir convention exists
/// anywhere else in this workspace yet).
fn debug_layout_path() -> PathBuf {
    std::env::temp_dir().join("retroforge-debug-dock-layout.toml")
}

/// Write `dock_state`'s current layout to disk (module doc: TOML via
/// [`layout::to_toml_string`]). Best-effort: an unwritable placeholder
/// directory degrades to "layout not saved this session" rather than a
/// fatal error — same posture `crate::core_thread`'s canvas cache already
/// takes for its own `Cache::open` failure.
pub fn save_layout(dock_state: &DockState<DebugTab>) {
    let Ok(text) = layout::to_toml_string(&capture_layout(dock_state)) else {
        return;
    };
    let _ = std::fs::write(debug_layout_path(), text);
}

/// Load a previously-saved layout from disk, falling back to
/// [`layout::default_layout`] if none exists yet or the file is
/// unreadable/malformed (a corrupted layout file must never block opening
/// the debugger).
#[must_use]
pub fn load_layout() -> DockState<DebugTab> {
    let persisted = std::fs::read_to_string(debug_layout_path())
        .ok()
        .and_then(|text| layout::from_toml_str(&text).ok())
        .unwrap_or_else(layout::default_layout);
    restore_layout(&persisted)
}

// ---------------------------------------------------------------------
// Panel state + rendering (ticket W4-06a criteria 1-2). Everything above
// this line is decode-free plain-data wiring, already covered by the
// headless tests below; everything from here down is the egui-touching
// half the ticket brief accepts as human-only-verifiable (its "vacuity
// traps" section) — decode logic itself lives in `rf_debugger` and is
// tested there, so this half is intentionally thin: pull already-decoded
// structs, lay them out.
// ---------------------------------------------------------------------
use eframe::egui;
use rf_debugger::pattern::PatternTable;

/// Live data the debug panels read each repaint — a plain struct
/// `crate::app` fills in from whatever it can actually reach (see each
/// field's doc for its source, and `rf_debugger::lib`'s module-doc table
/// for which viewers have live data at all today).
pub struct PanelData {
    /// Ticket W4-06a: static CHR-ROM bytes sliced straight from the loaded
    /// ROM *file* (`rf_nes::NesRom::chr_rom`), extracted once in
    /// `crate::app::RetroForgeApp::open_rom` before the bytes move into
    /// `core_thread::spawn` — never read from the running core. `None`
    /// until a ROM is loaded, or if the cartridge uses CHR RAM
    /// (`rf_nes::NesRom::chr_is_ram`) — there is no static pattern data to
    /// show in that case (`rf_debugger::pattern`'s own module doc).
    pub chr_rom: Option<Vec<u8>>,
    /// Ticket W4-04: everything the Lua console tab renders, pre-computed
    /// by `crate::script_panel` (which is where its tests live). `None`
    /// when no plugin is loaded, so the tab can say so rather than
    /// showing an empty panel that reads as "not implemented".
    pub script: Option<Box<ScriptPanelData>>,
    /// Ticket W4-10a: the trace scrollback, its filters and the capture
    /// controls. `None` when the app has no session — the panel then says
    /// so rather than showing an empty list a user would read as "the
    /// trace is running and nothing happened".
    pub trace: Option<Box<TracePanelData>>,
    /// Ticket W4-10b: per-channel scope traces and the mute/solo state.
    /// `None` with no session.
    pub audio: Option<Box<AudioPanelData>>,
    /// Latest frame's OAM (`core_thread::FrameMsg::oam`) — genuinely live,
    /// unlike `chr_rom`/vram/cgram.
    pub oam: [u8; 256],
    /// Ticket W4-06c: the PREVIOUS frame's OAM, so the diff panel can say
    /// what moved. Kept here rather than fetched, because the panel is a
    /// read-only consumer — it must never reach into the core for a
    /// second sample (that is W3-05a's hazard class: a debug surface
    /// silently perturbing state, invisible to pixel comparison).
    pub previous_oam: [u8; 256],
    /// Ticket W4-06d: the PPU's nametable VRAM and palette RAM, live at
    /// last frame. Read through `NesBus::vram()`/`palette()`, which are
    /// plain non-observing borrows — see `Ppu::vram`'s doc for why that
    /// property is load-bearing rather than incidental.
    pub vram: [u8; 0x1000],
    pub palette_ram: [u8; 32],
    /// Which scanline the OAM diff panel's drop analysis is about.
    pub oam_diff_scanline: u16,
    /// Latest frame's event FIFO (`rf_core_api::FrameBundle::events`, via
    /// `CoreHandle::frame_bundle`) — genuinely live, gated by whatever mask
    /// `DebugPanels::wants_event_subscription` last asked for.
    pub events: Vec<rf_core_api::CoreEvent>,
    /// Ticket W4-06b: latest frame's 2 KiB WRAM snapshot
    /// (`core_thread::FrameMsg::wram`) — genuinely live, read-only,
    /// non-perturbing (`EmuStepper::wram_snapshot`'s own doc).
    pub wram: [u8; 0x0800],
    /// Ticket W4-06b: latest frame's 8 KiB PRG-RAM window
    /// (`core_thread::FrameMsg::prg_ram`) — the memory viewer's second
    /// live range (`EmuStepper::prg_ram`'s own doc for why WRAM alone
    /// isn't enough).
    pub prg_ram: [u8; 0x2000],
    /// Ticket W13-02d: the memory panel's goto/find/edit state.
    pub memory: MemoryPanelData,
    /// Ticket W13-02b: the SNES session's memories, or `None` on NES.
    /// **This is what makes the panels console-aware**: every viewer below
    /// checks it and draws the SNES column when it is `Some`.
    pub snes: Option<Box<crate::core_thread::SnesDebugFrame>>,
    /// Which BG layer the SNES tilemap viewer is showing.
    pub snes_bg: usize,
    /// Which memory space the SNES memory view is showing: 0 VRAM,
    /// 1 CGRAM, 2 ARAM.
    pub snes_space: usize,
    /// Which DSP voice the BRR preview is showing (ticket W13-02c).
    pub snes_voice: usize,
    /// The CPU register file this frame, typed per CPU family (ticket
    /// W13-02i). Drawn at the top of the Trace tab by [`registers_ui`]
    /// for both consoles through one match.
    pub cpu_regs: rf_core_api::CpuRegs,
}

impl Default for PanelData {
    fn default() -> Self {
        PanelData {
            chr_rom: None,
            script: None,
            trace: None,
            audio: None,
            oam: [0u8; 256],
            previous_oam: [0u8; 256],
            vram: [0u8; 0x1000],
            palette_ram: [0u8; 32],
            oam_diff_scanline: 0,
            events: Vec::new(),
            wram: [0u8; 0x0800],
            prg_ram: [0u8; 0x2000],
            memory: MemoryPanelData::default(),
            snes: None,
            snes_bg: 0,
            snes_space: 0,
            snes_voice: 0,
            cpu_regs: rf_core_api::CpuRegs::None,
        }
    }
}

/// The debug-viewer window's full state: the dock layout, its live data,
/// and the one piece of per-panel UI state (which pattern-table half is
/// selected) that doesn't belong in [`PanelData`] because it's a view
/// choice, not data.
pub struct DebugPanels {
    /// Whether the debug-viewer window is currently shown (ticket W4-06a
    /// criterion 2's "closed panels register no event subscriptions" reads
    /// this too — see [`Self::wants_event_subscription`]).
    pub visible: bool,
    pub dock_state: DockState<DebugTab>,
    pub data: PanelData,
    /// Ticket W13-02f: the annotation store, its forms and its pending
    /// request. Kept beside [`PanelData`] rather than inside it so the
    /// trace panel can borrow the labels (shared) while it borrows its own
    /// scrollback (mutable) — two fields, two borrows, no clone per
    /// repaint.
    pub annotations: AnnotationPanelData,
    pattern_table: PatternTable,
}

impl DebugPanels {
    /// Load the persisted layout (or [`layout::default_layout`] if none
    /// exists yet) and start with empty live data — `crate::app` fills
    /// [`PanelData`] in as frames/ROMs arrive.
    #[must_use]
    pub fn new() -> Self {
        DebugPanels {
            visible: false,
            dock_state: load_layout(),
            data: PanelData::default(),
            annotations: AnnotationPanelData::default(),
            pattern_table: PatternTable::Left,
        }
    }

    /// Whether the Event-timeline tab is currently docked anywhere in
    /// [`Self::dock_state`] — DEBUGGER.md §6's "closed panels register no
    /// event subscriptions" discipline, made mechanical: `crate::app`
    /// calls this every repaint to decide whether to send
    /// `CoreCommand::SetEventMask(EventMask::ALL)` (widen) or
    /// `EventMask::NONE` (narrow — `EmuStepper::set_event_mask` always
    /// re-asserts the camera baseline regardless, so this can never starve
    /// `crate::canvas_accum`).
    #[must_use]
    pub fn wants_event_subscription(&self) -> bool {
        self.dock_state
            .iter_all_tabs()
            .any(|(_, tab)| *tab == DebugTab::EventTimeline)
    }

    /// Whether a SNES-capable viewer is docked, so `crate::app` can turn
    /// the frame-side capture on (ticket W13-02b).
    ///
    /// Keyed on the panels being **open**, not on the console: the cost is
    /// 128 KiB per frame and a closed panel must not pay it (DEBUGGER.md
    /// §6). The core answers `None` on a NES session anyway, so asking
    /// while a NES game runs is free.
    #[must_use]
    pub fn wants_snes_capture(&self) -> bool {
        self.visible
            && self.dock_state.iter_all_tabs().any(|(_, tab)| {
                matches!(
                    tab,
                    DebugTab::Pattern
                        | DebugTab::Nametable
                        | DebugTab::Palette
                        | DebugTab::Oam
                        | DebugTab::Memory
                        // Ticket W13-02c: the HDMA lanes and the DSP
                        // voices ride on the same capture.
                        | DebugTab::EventTimeline
                        | DebugTab::Audio
                )
            })
    }

    /// Persist the current layout (ticket criterion 3) — `crate::app`
    /// calls this whenever the window is closed and from `eframe::App::
    /// save`, so a session that never cleanly exits still keeps whatever
    /// was last saved at close time.
    pub fn save(&self) {
        save_layout(&self.dock_state);
    }

    /// Draw the dock area into `ui` (ticket W4-06a criterion 3's actual
    /// egui wiring — everything it draws is data already computed by
    /// `rf_debugger`'s decode functions, called from the small `*_ui`
    /// helpers below).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.tab_picker_ui(ui);
        let style = egui_dock::Style::from_egui(ui.style().as_ref());
        let mut viewer = PanelTabViewer {
            data: &mut self.data,
            annotations: &mut self.annotations,
            pattern_table: &mut self.pattern_table,
        };
        egui_dock::DockArea::new(&mut self.dock_state)
            .style(style)
            .show_inside(ui, &mut viewer);
    }
}

impl DebugPanels {
    /// The "Add panel" menu — every [`DebugTab`] not currently docked.
    ///
    /// ## Why this exists, and why it is part of ticket W13-02f
    ///
    /// A tab that is in the enum but in no layout **cannot be opened**:
    /// `restore_layout` only ever shows what the persisted file (or
    /// [`layout::default_layout`]) names, and until now nothing could add
    /// one. Two panels were already in that state — `LuaConsole`
    /// (DEBUGGER.md §5, shipped by W4-04) and `OamDiff` (FR-DBG-006,
    /// shipped by W4-06c) — with working UI nobody could reach.
    ///
    /// It is in *this* ticket because the same wall applies to
    /// `Annotations`: putting it in `default_layout` alone would reach a
    /// fresh install and no one else, since anybody who has opened the
    /// debug window has a persisted layout that predates the variant. The
    /// ticket's criterion is that the workflow reaches a **user**, so the
    /// affordance is part of the criterion rather than an extra.
    /// Dock `tab` if it is not already open, and do nothing if it is.
    ///
    /// The picker's one action, as a method so the end-to-end test drives
    /// the same door a user does rather than a second one that could
    /// diverge from it.
    pub fn open_tab(&mut self, tab: DebugTab) {
        if self.dock_state.iter_all_tabs().any(|(_, t)| *t == tab) {
            return;
        }
        // `push_to_focused_leaf` verified against the vendored egui_dock
        // 0.20.1 source, not recall (`DockState::push_to_focused_leaf`,
        // dock_state/mod.rs:468) — the same discipline the layout
        // capture/restore in this module already follows.
        self.dock_state.push_to_focused_leaf(tab);
    }

    fn tab_picker_ui(&mut self, ui: &mut egui::Ui) {
        let open: Vec<DebugTab> = self
            .dock_state
            .iter_all_tabs()
            .map(|(_, tab)| *tab)
            .collect();
        let missing: Vec<DebugTab> = DebugTab::ALL
            .into_iter()
            .filter(|t| !open.contains(t))
            .collect();
        ui.horizontal(|ui| {
            ui.menu_button("Add panel", |ui| {
                if missing.is_empty() {
                    ui.label("Every panel is already open.");
                    return;
                }
                for tab in missing {
                    if ui.button(tab.label()).clicked() {
                        self.open_tab(tab);
                        ui.close();
                    }
                }
            });
        });
    }
}

impl Default for DebugPanels {
    fn default() -> Self {
        Self::new()
    }
}

/// OAM diff panel (ticket W4-06c; FR-DBG-006).
///
/// **Read-only by construction.** Both OAM snapshots arrive as copies the
/// frame already carried (`core_thread::FrameMsg::oam`), so this function
/// has no path to the core at all — it cannot perturb state even by
/// accident, which is the hazard W3-05a named: an enhancement silently
/// clocking MMC3's A12 counter, invisible to pixel comparison.
///
/// Both snapshots are the **PPU's own** OAM, never the CPU-side shadow at
/// `$0200`. W2-10a's gem defect was exactly those two disagreeing.
fn oam_diff_ui(ui: &mut egui::Ui, previous: &[u8; 256], current: &[u8; 256], scanline: u16) {
    let deltas = rf_debugger::oam::diff_oam(previous, current);
    ui.label(format!(
        "{} sprite(s) changed since the last frame",
        deltas.len()
    ));
    if deltas.is_empty() {
        // Said explicitly: a blank panel reads as "not implemented", and
        // "nothing moved" is a real and common answer.
        ui.label("No OAM changes this frame.");
    }
    egui::ScrollArea::vertical()
        .max_height(240.0)
        .show(ui, |ui| {
            egui::Grid::new("debug-oam-diff-grid")
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("#");
                    ui.strong("changed");
                    ui.strong("was (x,y,tile)");
                    ui.strong("now (x,y,tile)");
                    ui.end_row();
                    for d in &deltas {
                        ui.monospace(d.index.to_string());
                        ui.monospace(d.fields.summary());
                        ui.monospace(format!(
                            "{},{},{:#04X}",
                            d.before.x, d.before.y, d.before.tile
                        ));
                        ui.monospace(format!("{},{},{:#04X}", d.after.x, d.after.y, d.after.tile));
                        ui.end_row();
                    }
                });
        });

    ui.separator();
    // 8x8 is assumed: this panel has no PPUCTRL access of its own (it is
    // a read-only consumer), and `rf_debugger::oam` takes sprite height
    // from its caller for exactly that reason. Stated rather than hidden,
    // since an 8x16 game's drop list would be wrong.
    let sprites = rf_debugger::oam::decode_oam(current);
    let dropped = rf_debugger::oam::dropped_by_limit(&sprites, scanline, 8);
    ui.label(format!(
        "Scanline {scanline}: {} sprite(s) dropped by the 8-per-scanline limit (assumes 8x8)",
        dropped.len()
    ));
    if dropped.is_empty() {
        ui.label("None dropped on this scanline.");
    } else {
        ui.monospace(
            dropped
                .iter()
                .map(|i| format!("#{i}"))
                .collect::<Vec<_>>()
                .join("  "),
        );
    }
}

/// Paint the Lua console (ticket W4-04; DEBUGGER.md §5).
///
/// Every string shown here is computed by `crate::script_panel`, which is
/// tested headlessly — this function only lays them out, so there is no
/// logic here that could disagree with what the tests assert.
fn lua_console_ui(ui: &mut egui::Ui, script: Option<&ScriptPanelData>) {
    let Some(script) = script else {
        ui.label("No plugin loaded.");
        ui.label(
            "Load one from plugins/examples — see docs/PLUGIN_AUTHORING.md for the manifest \
             shape and the capability list.",
        );
        return;
    };
    ui.label(&script.status);
    ui.separator();
    egui::CollapsingHeader::new("Capabilities")
        .default_open(true)
        .show(ui, |ui| {
            for line in &script.capabilities {
                ui.label(line);
            }
        });
    egui::CollapsingHeader::new("Write ledger").show(ui, |ui| {
        for line in &script.ledger {
            ui.label(line);
        }
    });
    ui.separator();
    ui.label("Console:");
    egui::ScrollArea::vertical()
        .max_height(200.0)
        .show(ui, |ui| {
            for line in &script.console {
                ui.monospace(line);
            }
        });
}

/// Everything the Lua console tab renders, pre-computed by
/// `crate::script_panel` on the UI thread.
#[derive(Debug, Clone, Default)]
pub struct ScriptPanelData {
    pub status: String,
    pub capabilities: Vec<String>,
    pub ledger: Vec<String>,
    pub console: Vec<String>,
}

struct PanelTabViewer<'a> {
    annotations: &'a mut AnnotationPanelData,
    data: &'a mut PanelData,
    pattern_table: &'a mut PatternTable,
}

impl egui_dock::TabViewer for PanelTabViewer<'_> {
    type Tab = DebugTab;

    fn title(&mut self, tab: &mut DebugTab) -> egui::WidgetText {
        tab.label().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut DebugTab) {
        match tab {
            DebugTab::Pattern => match self.data.snes.as_deref() {
                Some(snes) => snes_pattern_ui(ui, snes, &mut self.data.snes_bg),
                None => pattern_ui(ui, self.data.chr_rom.as_deref(), self.pattern_table),
            },
            DebugTab::Nametable => match self.data.snes.as_deref() {
                // Mode 7 has no tilemap in the BGnSC sense — its map IS
                // the playfield — so it shares this tab rather than
                // getting one the NES would leave empty. Which view is
                // shown follows the live BG mode, so a game entering
                // mode 7 does not leave the user on a panel that no
                // longer describes it.
                Some(snes) => {
                    if snes.ppu_regs.get(0x05).copied().unwrap_or(0) & 0x07 == 7 {
                        snes_mode7_ui(ui, snes);
                    } else {
                        snes_tilemap_ui(ui, snes, &mut self.data.snes_bg);
                    }
                }
                None => nametable_ui(ui, &self.data.vram),
            },
            DebugTab::Palette => match self.data.snes.as_deref() {
                Some(snes) => snes_palette_ui(ui, snes),
                None => palette_ui(ui, &self.data.palette_ram),
            },
            DebugTab::Oam => match self.data.snes.as_deref() {
                Some(snes) => snes_oam_ui(ui, snes, self.data.oam_diff_scanline),
                None => oam_ui(ui, &self.data.oam),
            },
            DebugTab::EventTimeline => {
                event_timeline_ui(ui, &self.data.events);
                if let Some(snes) = self.data.snes.as_deref() {
                    ui.separator();
                    snes_hdma_lanes_ui(ui, snes);
                }
            }
            DebugTab::Memory => match self.data.snes.as_deref() {
                Some(snes) => snes_memory_ui(ui, snes, &mut self.data.snes_space),
                None => memory_ui(
                    ui,
                    &self.data.wram,
                    &self.data.prg_ram,
                    &mut self.data.memory,
                    &self.annotations.store.ram_labels(),
                ),
            },
            DebugTab::OamDiff => oam_diff_ui(
                ui,
                &self.data.previous_oam,
                &self.data.oam,
                self.data.oam_diff_scanline,
            ),
            DebugTab::LuaConsole => lua_console_ui(ui, self.data.script.as_deref()),
            DebugTab::Trace => {
                // Ticket W13-02i: the register readout lives with the
                // trace, where bsnes and Mesen both put it, and it is
                // the one panel that reads BOTH cores through a single
                // typed path — no `snes.as_deref()` branch here.
                registers_ui(ui, &self.data.cpu_regs);
                ui.separator();
                trace_ui(
                    ui,
                    self.data.trace.as_deref_mut(),
                    &self.annotations.store.ram_labels(),
                );
            }
            DebugTab::Annotations => annotations_ui(ui, self.annotations),
            DebugTab::Audio => {
                audio_ui(ui, self.data.audio.as_deref_mut());
                if let Some(snes) = self.data.snes.as_deref() {
                    ui.separator();
                    snes_dsp_ui(ui, snes, &mut self.data.snes_voice);
                }
            }
        }
    }
}

/// Build an egui `ColorImage` from [`rf_debugger::pattern`]'s decoded
/// tiles, laid out in a 16x16 grid (256 tiles x 8px = 128x128), using a
/// fixed 4-step grayscale ramp — there is no live CGRAM to color it with
/// (`rf_debugger::palette`'s module doc), and a grayscale index ramp is
/// the standard "no palette selected yet" fallback real pattern-table
/// viewers (Mesen included) use.
const GRAYSCALE_RAMP: [[u8; 3]; 4] = [[0, 0, 0], [85, 85, 85], [170, 170, 170], [255, 255, 255]];
const TILES_PER_ROW: usize = 16;
const TABLE_PIXELS: usize = TILES_PER_ROW * 8;

fn pattern_table_image(chr: &[u8], table: PatternTable) -> egui::ColorImage {
    let tiles = rf_debugger::pattern::decode_pattern_table(chr, table);
    let mut rgba = vec![0u8; TABLE_PIXELS * TABLE_PIXELS * 4];
    for (i, tile) in tiles.iter().enumerate() {
        let tile_col = i % TILES_PER_ROW;
        let tile_row = i / TILES_PER_ROW;
        let tile_rgba = rf_debugger::pattern::tile_to_rgba(tile, GRAYSCALE_RAMP);
        for py in 0..8 {
            for px in 0..8 {
                let src = (py * 8 + px) * 4;
                let dst_x = tile_col * 8 + px;
                let dst_y = tile_row * 8 + py;
                let dst = (dst_y * TABLE_PIXELS + dst_x) * 4;
                rgba[dst..dst + 4].copy_from_slice(&tile_rgba[src..src + 4]);
            }
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([TABLE_PIXELS, TABLE_PIXELS], &rgba)
}

fn pattern_ui(ui: &mut egui::Ui, chr: Option<&[u8]>, table: &mut PatternTable) {
    ui.horizontal(|ui| {
        ui.selectable_value(table, PatternTable::Left, "Left ($0000)");
        ui.selectable_value(table, PatternTable::Right, "Right ($1000)");
    });
    let Some(chr) = chr else {
        ui.label("No CHR data (no ROM loaded, or this cartridge uses CHR RAM — see rf_debugger::pattern's module doc).");
        return;
    };
    let image = pattern_table_image(chr, *table);
    if image.pixels.iter().all(|p| p.a() == 0) {
        ui.label("This ROM's CHR data is present but this half decoded to zero tiles (short/empty slice).");
        return;
    }
    let texture =
        ui.ctx()
            .load_texture("debug-pattern-table", image, egui::TextureOptions::NEAREST);
    ui.add(egui::Image::from_texture(&texture).fit_to_original_size(2.0));
}

/// Live nametable viewer (ticket W4-06d closed W4-06a's "no live data"
/// gap by adding `NesBus::vram()`).
///
/// Shows the first 1 KiB nametable; `$2400`/`$2800`/`$2C00` are mirrors of
/// it or of the second physical table depending on cartridge mirroring,
/// which the viewer does not yet resolve — stated rather than implied by
/// showing one grid and calling it "the nametable".
fn nametable_ui(ui: &mut egui::Ui, vram: &[u8; 0x1000]) {
    let Some(grid) = rf_debugger::nametable::decode_nametable(vram) else {
        ui.label("No nametable data yet — load a ROM and run a frame.");
        return;
    };
    ui.label("Nametable 0 ($2000) — tile index per cell, palette in brackets");
    egui::ScrollArea::both().show(ui, |ui| {
        for row in grid.iter() {
            let line: String = row
                .iter()
                .map(|c| format!("{:02X}[{}]", c.tile_index, c.palette))
                .collect::<Vec<_>>()
                .join(" ");
            ui.monospace(line);
        }
    });
}

/// Live palette viewer (ticket W4-06d).
///
/// Shows the RAW stored bytes — `NesBus::palette()` deliberately does not
/// apply the `$3F10/$14/$18/$1C` backdrop mirroring the PPU uses on read,
/// because a difference between what is stored and what renders is
/// exactly what a palette viewer exists to reveal.
fn palette_ui(ui: &mut egui::Ui, cgram: &[u8; 32]) {
    let Some(groups) = rf_debugger::palette::decode_palette(cgram) else {
        ui.label("No palette data yet — load a ROM and run a frame.");
        return;
    };
    ui.label("Raw palette RAM (unmirrored) — BG 0-3 then sprite 0-3");
    for (i, group) in groups.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.monospace(format!("{} {}:", if i < 4 { "BG" } else { "SP" }, i % 4));
            for swatch in group.iter() {
                let [r, g, b] = swatch.rgb;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, 2.0, egui::Color32::from_rgb(r, g, b));
                ui.monospace(format!("{:02X}", swatch.raw_index));
            }
        });
    }
}

fn oam_ui(ui: &mut egui::Ui, oam: &[u8; 256]) {
    let sprites = rf_debugger::oam::decode_oam(oam);
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("debug-oam-grid")
            .striped(true)
            .show(ui, |ui| {
                ui.strong("#");
                ui.strong("X");
                ui.strong("Y");
                ui.strong("Tile");
                ui.strong("Pal");
                ui.strong("Prio");
                ui.strong("FlipH");
                ui.strong("FlipV");
                ui.end_row();
                for s in &sprites {
                    ui.monospace(s.index.to_string());
                    ui.monospace(s.x.to_string());
                    ui.monospace(s.y.to_string());
                    ui.monospace(format!("{:#04X}", s.tile));
                    ui.monospace(s.palette.to_string());
                    ui.monospace(if s.priority_behind_bg { "BG" } else { "FG" });
                    ui.monospace(if s.flip_h { "H" } else { "-" });
                    ui.monospace(if s.flip_v { "V" } else { "-" });
                    ui.end_row();
                }
            });
    });
}

/// Ticket W4-06b (FR-DBG-002, DEBUGGER.md §3 "Memory hex" row): a
/// read-only hex dump of the two live ranges `PanelData` carries — WRAM
/// (`$0000-$07FF`) and cartridge PRG-RAM (`$6000-$7FFF`),
/// `rf_debugger::memory_view::build_rows`'d separately (they're not
/// contiguous). Purely a rendering of already-decoded rows: nothing here
/// reads the core itself (that already happened on the core thread, into
/// `PanelData`, non-perturbingly — `EmuStepper::wram_snapshot`/`prg_ram`'s
/// own docs), which is what keeps this panel "read-only and
/// non-perturbing" even though it repaints every frame.
/// What the memory panel needs beyond the bytes themselves (ticket
/// W13-02d): goto, find, the live-edit box, and the annotation lookup that
/// colours what is already labelled.
#[derive(Debug, Default)]
pub struct MemoryPanelData {
    pub goto: String,
    pub find: String,
    /// Address the last goto/find resolved to, highlighted in the dump.
    pub cursor: Option<u32>,
    /// The address being edited and the text typed into it, if any.
    pub editing: Option<(u32, String)>,
    /// Whether the core is paused. Live edit is gated on it — see
    /// [`memory_ui`].
    pub paused: bool,
    /// A write the app should perform: `(addr, value)`.
    pub poke: Option<(u32, u8)>,
    pub status: Option<String>,
}

/// The memory hex panel (DEBUGGER.md section 3's memory-hex row; ticket
/// W13-02d).
///
/// ## Live edit is gated on pause, and that is a correctness rule
///
/// The core runs on its own thread (project law 4: nothing mutates core
/// state off the core thread). A poke sent while the machine is running
/// lands at whatever cycle the command queue happens to drain on, so the
/// same edit produces a different machine every time — and the whole
/// project rests on runs being reproducible. Paused, the write happens at
/// a boundary the user can see, and a replay of the session lands it in
/// the same place.
fn memory_ui(
    ui: &mut egui::Ui,
    wram: &[u8; 0x0800],
    prg_ram: &[u8; 0x2000],
    panel: &mut MemoryPanelData,
    labels: &[rf_debugger::annotation::RamLabel<'_>],
) {
    use rf_debugger::memory_view::{build_rows, find_bytes, parse_addr, parse_bytes};

    let wram_rows = build_rows(0x0000, wram);
    let prg_rows = build_rows(0x6000, prg_ram);

    ui.horizontal(|ui| {
        ui.label("goto $");
        let goto = ui.add(egui::TextEdit::singleline(&mut panel.goto).desired_width(64.0));
        if goto.changed() {
            panel.cursor = parse_addr(&panel.goto);
        }
        ui.separator();
        ui.label("find");
        ui.add(egui::TextEdit::singleline(&mut panel.find).desired_width(110.0))
            .on_hover_text("hex bytes, spaced or run together: 4C 00 80");
        let needle = parse_bytes(&panel.find);
        if ui
            .add_enabled(needle.is_some(), egui::Button::new("next"))
            .on_disabled_hover_text("an odd number of hex digits is a half-typed byte")
            .clicked()
        {
            if let Some(needle) = needle {
                // Search from just past the cursor so repeated presses
                // walk the matches instead of finding the same one.
                let from = panel.cursor.map_or(0, |c| c.saturating_add(1));
                let found = find_bytes(&wram_rows, &needle, from)
                    .or_else(|| find_bytes(&prg_rows, &needle, from))
                    // Wrap: a search that stops at the end of the range
                    // and says nothing looks like "not present".
                    .or_else(|| find_bytes(&wram_rows, &needle, 0))
                    .or_else(|| find_bytes(&prg_rows, &needle, 0));
                match found {
                    Some(addr) => {
                        panel.cursor = Some(addr);
                        panel.goto = format!("{addr:04X}");
                        panel.status = Some(format!("found at ${addr:04X}"));
                    }
                    None => panel.status = Some("not found".to_string()),
                }
            }
        }
    });
    if let Some(status) = &panel.status {
        ui.label(egui::RichText::new(status).small().weak());
    }
    if !panel.paused {
        ui.label(
            egui::RichText::new("Editing is available while paused.")
                .small()
                .weak(),
        );
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label("WRAM ($0000-$07FF)");
        memory_rows_ui(ui, "debug-memory-wram", &wram_rows, panel, labels);
        ui.separator();
        ui.label("PRG-RAM ($6000-$7FFF)");
        memory_rows_ui(ui, "debug-memory-prgram", &prg_rows, panel, labels);
    });
}

/// Colour for a byte covered by an annotation. Deliberately a tint rather
/// than a background block: the hex has to stay readable, and the question
/// the colour answers is "is this labelled", not "what is it".
const ANNOTATED: egui::Color32 = egui::Color32::from_rgb(0x7F, 0xC8, 0xFF);
/// Colour for the goto/find cursor.
const CURSOR: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xC8, 0x50);

fn memory_rows_ui(
    ui: &mut egui::Ui,
    grid_id: &str,
    rows: &[rf_debugger::memory_view::MemoryRow],
    panel: &mut MemoryPanelData,
    labels: &[rf_debugger::annotation::RamLabel<'_>],
) {
    let mut begin_edit: Option<u32> = None;
    let mut commit: Option<(u32, u8)> = None;
    let mut cancel = false;
    egui::Grid::new(grid_id).striped(true).show(ui, |ui| {
        for row in rows {
            ui.monospace(format!("{:04X}", row.addr));
            ui.horizontal(|ui| {
                for (i, b) in row.bytes.iter().enumerate() {
                    let addr = row.addr + i as u32;
                    if let Some((editing_addr, text)) = panel.editing.as_mut() {
                        if *editing_addr == addr {
                            let response = ui.add(
                                egui::TextEdit::singleline(text)
                                    .desired_width(22.0)
                                    .font(egui::TextStyle::Monospace),
                            );
                            if response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                match u8::from_str_radix(text.trim(), 16) {
                                    Ok(value) => commit = Some((addr, value)),
                                    // A bad byte cancels rather than
                                    // writing something the user did not
                                    // type. Writing "whatever parsed" into
                                    // live memory is not a recoverable
                                    // mistake.
                                    Err(_) => cancel = true,
                                }
                            } else if response.lost_focus() {
                                cancel = true;
                            }
                            continue;
                        }
                    }
                    let label = labels
                        .iter()
                        .find(|l| addr >= l.addr && addr < l.addr.saturating_add(l.len));
                    let mut text = egui::RichText::new(format!("{b:02X}")).monospace();
                    if panel.cursor == Some(addr) {
                        text = text.color(CURSOR).strong();
                    } else if label.is_some() {
                        text = text.color(ANNOTATED);
                    }
                    let cell = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                    let cell = match label {
                        Some(l) => cell.on_hover_text(l.label),
                        None => cell,
                    };
                    if cell.clicked() && panel.paused {
                        begin_edit = Some(addr);
                    }
                }
            });
            ui.end_row();
        }
    });
    if let Some(addr) = begin_edit {
        panel.editing = Some((addr, String::new()));
    }
    if let Some((addr, value)) = commit {
        panel.poke = Some((addr, value));
        panel.editing = None;
        panel.status = Some(format!("wrote ${value:02X} to ${addr:04X}"));
    }
    if cancel {
        panel.editing = None;
    }
}

/// Scanline range plotted along the timeline's X axis (NES: 262 total,
/// 0-241 visible+post-render before vblank) — `rf_debugger::event_timeline`
/// doesn't fix this (it is console-agnostic data), so the panel picks it.
const TIMELINE_SCANLINES: f32 = 262.0;
/// One labeled row per `CoreEvent` category (DEBUGGER.md's "frame
/// timeline: dot/scanline scatter... IRQ/NMI, DMA").
const TIMELINE_ROWS: [&str; 8] = [
    "FrameStart",
    "FrameEnd",
    "VblankStart",
    "Scanline",
    "OamRewrite",
    "ScrollWrite",
    "MapperIrq",
    "DmaStart",
];

fn timeline_row(event: &rf_core_api::CoreEvent) -> usize {
    match event {
        rf_core_api::CoreEvent::FrameStart => 0,
        rf_core_api::CoreEvent::FrameEnd => 1,
        rf_core_api::CoreEvent::VblankStart => 2,
        rf_core_api::CoreEvent::Scanline(_) => 3,
        rf_core_api::CoreEvent::OamRewrite => 4,
        rf_core_api::CoreEvent::ScrollWrite { .. } => 5,
        rf_core_api::CoreEvent::MapperIrq => 6,
        rf_core_api::CoreEvent::DmaStart { .. } => 7,
        // MemWatch has no fixed row on this MVP timeline (DEBUGGER.md's
        // row list doesn't name it either) — plotted on the DmaStart row
        // is wrong, so give it its own off-chart marker instead: row 8,
        // one past the last labeled row, drawn without a label.
        rf_core_api::CoreEvent::MemWatch { .. } => 8,
    }
}

fn event_timeline_ui(ui: &mut egui::Ui, events: &[rf_core_api::CoreEvent]) {
    if events.is_empty() {
        ui.label(
            "No events this frame (or the event viewer was just opened — the core thread \
             subscribes on the next command poll).",
        );
        return;
    }
    let timeline = rf_debugger::event_timeline::build_timeline(events);
    let row_height = 18.0;
    let label_width = 90.0;
    let (rect, _response) = ui.allocate_exact_size(
        egui::vec2(
            ui.available_width(),
            row_height * (TIMELINE_ROWS.len() + 1) as f32,
        ),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let plot_width = (rect.width() - label_width).max(1.0);
    for (i, label) in TIMELINE_ROWS.iter().enumerate() {
        let y = rect.top() + row_height * i as f32 + row_height / 2.0;
        painter.text(
            egui::pos2(rect.left(), y),
            egui::Align2::LEFT_CENTER,
            *label,
            egui::FontId::monospace(11.0),
            ui.visuals().text_color(),
        );
    }
    for point in &timeline {
        let row = timeline_row(&point.event);
        if row >= TIMELINE_ROWS.len() {
            continue; // MemWatch (module doc) has no labeled row to plot on.
        }
        let x_fraction = point
            .scanline
            .map_or(0.0, |sl| f32::from(sl) / TIMELINE_SCANLINES);
        let x = rect.left() + label_width + plot_width * x_fraction.clamp(0.0, 1.0);
        let y = rect.top() + row_height * row as f32 + row_height / 2.0;
        painter.circle_filled(egui::pos2(x, y), 3.0, egui::Color32::from_rgb(220, 120, 40));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nontrivial layout `default_layout` builds (two horizontal
    /// leaves nested under a vertical split) round-trips through capture
    /// and restore alone, in-memory — the baseline sanity check before the
    /// stronger file-backed test below.
    #[test]
    fn capture_then_restore_reproduces_the_default_layout_structure() {
        let original = layout::default_layout();
        let dock_state = restore_layout(&original);
        let captured = capture_layout(&dock_state);
        assert_eq!(captured, original);
    }

    /// A single leaf with several tabs and a non-zero active index round-
    /// trips its tab ORDER and active selection, not just membership.
    #[test]
    fn capture_then_restore_preserves_tab_order_and_active_index() {
        let original = PersistedLayout {
            version: layout::LAYOUT_FORMAT_VERSION,
            root: PersistedNode::Leaf {
                tabs: vec![DebugTab::Oam, DebugTab::EventTimeline, DebugTab::Palette],
                active: 1,
            },
        };
        let dock_state = restore_layout(&original);
        assert_eq!(capture_layout(&dock_state), original);
    }

    /// Deeply nested splits (3 levels) round-trip their axis and fraction
    /// at every level, not just the top one.
    #[test]
    fn capture_then_restore_preserves_a_deeply_nested_split_tree() {
        let original = PersistedLayout {
            version: layout::LAYOUT_FORMAT_VERSION,
            root: PersistedNode::Split {
                axis: SplitAxis::Horizontal,
                fraction: 0.25,
                first: Box::new(PersistedNode::Leaf {
                    tabs: vec![DebugTab::Pattern],
                    active: 0,
                }),
                second: Box::new(PersistedNode::Split {
                    axis: SplitAxis::Vertical,
                    fraction: 0.6,
                    first: Box::new(PersistedNode::Leaf {
                        tabs: vec![DebugTab::Nametable],
                        active: 0,
                    }),
                    second: Box::new(PersistedNode::Split {
                        axis: SplitAxis::Horizontal,
                        fraction: 0.9,
                        first: Box::new(PersistedNode::Leaf {
                            tabs: vec![DebugTab::Oam],
                            active: 0,
                        }),
                        second: Box::new(PersistedNode::Leaf {
                            tabs: vec![DebugTab::EventTimeline],
                            active: 0,
                        }),
                    }),
                }),
            },
        };
        let dock_state = restore_layout(&original);
        assert_eq!(capture_layout(&dock_state), original);
    }

    /// The full vacuity-trap-required proof (ticket brief (b)): capture a
    /// non-default `DockState`, serialize it, write the bytes to a REAL
    /// file on disk, drop every in-memory value, read the file back,
    /// deserialize, rebuild a `DockState`, and confirm the reconstructed
    /// structure matches. A round trip that only exercised in-memory
    /// `Clone`/equality (never touching the actual TOML text or a real
    /// file) could not catch a broken `Serialize`/`Deserialize` derive or a
    /// broken [`save_layout`]/[`load_layout`] file path — this test goes
    /// through both.
    #[test]
    fn save_layout_then_load_layout_round_trips_through_a_real_file_on_disk() {
        let original_layout = PersistedLayout {
            version: layout::LAYOUT_FORMAT_VERSION,
            root: PersistedNode::Split {
                axis: SplitAxis::Vertical,
                fraction: 0.42,
                first: Box::new(PersistedNode::Leaf {
                    tabs: vec![DebugTab::Pattern, DebugTab::Nametable],
                    active: 1,
                }),
                second: Box::new(PersistedNode::Leaf {
                    tabs: vec![DebugTab::EventTimeline],
                    active: 0,
                }),
            },
        };
        let dock_state = restore_layout(&original_layout);

        // A unique path per test run (parallel `cargo test` threads must
        // not clobber each other's file) — same reasoning any test writing
        // to `std::env::temp_dir()` needs, just inlined here rather than
        // touching the production `debug_layout_path()` (which every
        // caller of `save_layout`/`load_layout` shares).
        let path = std::env::temp_dir().join(format!(
            "retroforge-debug-dock-layout-test-{}-{}.toml",
            std::process::id(),
            "save_layout_then_load_layout_round_trips_through_a_real_file_on_disk"
        ));
        let text = layout::to_toml_string(&capture_layout(&dock_state))
            .expect("a freshly restored DockState must serialize");
        std::fs::write(&path, &text).expect("must be able to write the temp layout file");

        // Drop everything in-memory; read only from disk from here on.
        drop(dock_state);
        drop(original_layout);

        let read_back = std::fs::read_to_string(&path).expect("must be able to read it back");
        let _ = std::fs::remove_file(&path);
        let restored_persisted =
            layout::from_toml_str(&read_back).expect("the written TOML must parse back");
        let restored_dock_state = restore_layout(&restored_persisted);

        let PersistedNode::Split {
            fraction,
            first,
            second,
            ..
        } = &restored_persisted.root
        else {
            panic!("expected the root to still be a Split node");
        };
        assert!((*fraction - 0.42).abs() < f32::EPSILON);
        let PersistedNode::Leaf { tabs, active } = first.as_ref() else {
            panic!("expected `first` to be a Leaf");
        };
        assert_eq!(*tabs, vec![DebugTab::Pattern, DebugTab::Nametable]);
        assert_eq!(*active, 1);
        let PersistedNode::Leaf { tabs, .. } = second.as_ref() else {
            panic!("expected `second` to be a Leaf");
        };
        assert_eq!(*tabs, vec![DebugTab::EventTimeline]);

        // And the live DockState rebuilt from the file-round-tripped data
        // captures back to the exact same structure — proving the
        // capture/restore functions themselves, not just the plain-data
        // format, survive the full path.
        assert_eq!(capture_layout(&restored_dock_state), restored_persisted);
    }

    /// `load_layout` must degrade to the default layout, not panic, when
    /// there is nothing on disk yet (a fresh install) or the file is
    /// garbage — exercised via the plain-data functions directly since
    /// `load_layout`/`save_layout` share one fixed production path across
    /// the whole test binary (parallel tests would race on it).
    #[test]
    fn register_rows_render_both_cpus_through_one_match_and_none_as_empty() {
        use rf_core_api::{CpuRegs, Mos6502Regs, Wdc65816Regs};
        assert!(register_rows(&CpuRegs::None).is_empty());

        let nes = register_rows(&CpuRegs::Mos6502(Mos6502Regs {
            a: 0x12,
            x: 0x34,
            y: 0x56,
            s: 0xFD,
            pc: 0xC000,
            p: 0x24,
        }));
        assert_eq!(nes[0], ("PC", "$C000".to_string()));
        assert_eq!(nes[5], ("P", "$24 ..-..I..".to_string()));
        assert_eq!(nes.len(), 6);

        let snes = register_rows(&CpuRegs::Wdc65816(Wdc65816Regs {
            a: 0x1234,
            x: 0,
            y: 0,
            sp: 0x01FF,
            d: 0,
            dbr: 0x7E,
            pbr: 0x80,
            pc: 0x8000,
            p: 0x30,
            e: true,
        }));
        // bsnes convention: bank:offset for the PC, and E shown as its
        // own row because it is not a bit of P.
        assert_eq!(snes[0], ("PC", "$80:8000".to_string()));
        assert_eq!(snes[6], ("DB", "$7E".to_string()));
        assert_eq!(snes[7], ("P", "$30 ..1B....".to_string()));
        assert_eq!(snes[8].1, "1 (emulation)");
    }

    #[test]
    fn from_toml_str_on_garbage_falls_back_to_default_without_panicking() {
        let fallback = layout::from_toml_str("not valid toml {{{")
            .ok()
            .unwrap_or_else(layout::default_layout);
        assert_eq!(fallback, layout::default_layout());
    }
}

/// What the Trace tab needs (ticket W4-10a). Owned by
/// [`crate::app::RetroForgeApp`]; the panel only reads and edits it.
pub struct TracePanelData {
    pub scrollback: rf_debugger::trace::TraceScrollback,
    pub filter: rf_debugger::trace::TraceFilter,
    /// Set by the panel, acted on by the app on the next update — the
    /// panel cannot send `CoreCommand`s itself (it has no channel), and
    /// giving it one would put core-thread wiring inside a draw function.
    pub request: Option<TraceRequest>,
    /// Whether a capture is currently armed, so the button can say
    /// "Start"/"Stop" truthfully rather than toggling a local bool that
    /// could drift from the core thread's actual state.
    pub armed: bool,
    /// Path currently being written, if any.
    pub file: Option<std::path::PathBuf>,
    /// Filter text boxes' raw contents. Kept as strings rather than
    /// parsed `u16`s so a half-typed "C0" is not silently read as $00C0
    /// and applied while the user is still typing.
    pub pc_from: String,
    pub pc_to: String,
}

/// A control the user pressed, for the app to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceRequest {
    Start,
    Stop,
    Clear,
    StartFile,
    StopFile,
}

/// The Trace tab (DEBUGGER.md §2-3's "Trace viewer | scrollback of ring
/// buffer w/ filters").
/// One `(name, value)` row per register, in the order the console's own
/// documentation lists them, for [`registers_ui`]. Separate from the
/// drawing so it can be asserted without an egui context.
///
/// The shell knows each CPU's *register set* — that is the instruction
/// set architecture, public since 1975 and 1983 respectively — and
/// nothing about how either core stores it. `CpuRegs::None` yields no
/// rows, which the panel reports as "no CPU" rather than as zeros.
fn register_rows(regs: &rf_core_api::CpuRegs) -> Vec<(&'static str, String)> {
    use rf_core_api::CpuRegs;
    match regs {
        CpuRegs::None => Vec::new(),
        CpuRegs::Mos6502(r) => vec![
            ("PC", format!("${:04X}", r.pc)),
            ("A", format!("${:02X}", r.a)),
            ("X", format!("${:02X}", r.x)),
            ("Y", format!("${:02X}", r.y)),
            ("S", format!("${:02X}", r.s)),
            ("P", format!("${:02X} {}", r.p, r.flags())),
        ],
        CpuRegs::Wdc65816(r) => vec![
            ("PC", format!("${:02X}:{:04X}", r.pbr, r.pc)),
            ("A", format!("${:04X}", r.a)),
            ("X", format!("${:04X}", r.x)),
            ("Y", format!("${:04X}", r.y)),
            ("S", format!("${:04X}", r.sp)),
            ("D", format!("${:04X}", r.d)),
            ("DB", format!("${:02X}", r.dbr)),
            ("P", format!("${:02X} {}", r.p, r.flags())),
            (
                "E",
                if r.e { "1 (emulation)" } else { "0 (native)" }.to_string(),
            ),
        ],
    }
}

/// The CPU register readout (ticket W13-02i): one grid, both consoles.
fn registers_ui(ui: &mut egui::Ui, regs: &rf_core_api::CpuRegs) {
    let rows = register_rows(regs);
    if rows.is_empty() {
        ui.label("Registers: no CPU reported.");
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (name, value) in rows {
            ui.label(egui::RichText::new(name).strong());
            ui.monospace(value);
            ui.add_space(8.0);
        }
    });
}

fn trace_ui(
    ui: &mut egui::Ui,
    data: Option<&mut TracePanelData>,
    labels: &[rf_debugger::annotation::RamLabel<'_>],
) {
    let Some(data) = data else {
        ui.label("No session — open a ROM to trace.");
        return;
    };

    ui.horizontal(|ui| {
        if ui
            .button(if data.armed {
                "Stop trace"
            } else {
                "Start trace"
            })
            .clicked()
        {
            data.request = Some(if data.armed {
                TraceRequest::Stop
            } else {
                TraceRequest::Start
            });
        }
        if ui.button("Clear").clicked() {
            data.request = Some(TraceRequest::Clear);
        }
        match &data.file {
            Some(path) => {
                if ui.button("Stop writing").clicked() {
                    data.request = Some(TraceRequest::StopFile);
                }
                ui.label(format!("writing {}", path.display()));
            }
            None => {
                if ui.button("Trace to file\u{2026}").clicked() {
                    data.request = Some(TraceRequest::StartFile);
                }
            }
        }
    });

    let stats = data.scrollback.stats();
    ui.horizontal(|ui| {
        ui.label(format!("{} entries", stats.held));
        if stats.dropped_from_scrollback > 0 {
            ui.label(format!(
                "\u{2022} {} scrolled past",
                stats.dropped_from_scrollback
            ));
        }
        // DEBUGGER.md §2's "truncation flag visible in the UI". Coloured
        // and worded as a hole rather than as a count, because the number
        // matters less than the fact that the trace is no longer
        // contiguous.
        if stats.truncated() {
            ui.colored_label(
                egui::Color32::from_rgb(0xE0, 0x80, 0x30),
                format!(
                    "\u{26a0} TRUNCATED — {} entries lost; the trace has gaps",
                    stats.dropped_in_transport
                ),
            );
        }
    });

    // DEBUGGER.md §2's size warning, shown next to the control it is
    // about rather than only after the file has grown.
    if let Some(warning) = rf_debugger::trace::size_warning(60, 73, 500_000) {
        ui.label(egui::RichText::new(warning).small().weak());
    }

    ui.separator();
    ui.horizontal_wrapped(|ui| {
        for kind in rf_debugger::trace::TraceKind::ALL {
            let mut on = data.filter.kinds.contains(&kind);
            if ui.checkbox(&mut on, kind.label()).changed() {
                if on {
                    data.filter.kinds.push(kind);
                } else {
                    data.filter.kinds.retain(|k| *k != kind);
                }
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("PC range $");
        ui.add(egui::TextEdit::singleline(&mut data.pc_from).desired_width(48.0));
        ui.label("\u{2013} $");
        ui.add(egui::TextEdit::singleline(&mut data.pc_to).desired_width(48.0));
        // Both boxes must parse before a range is applied: applying a
        // half-typed range would filter the view out from under someone
        // mid-keystroke.
        data.filter.pc_range = match (
            u16::from_str_radix(data.pc_from.trim(), 16),
            u16::from_str_radix(data.pc_to.trim(), 16),
        ) {
            (Ok(lo), Ok(hi)) => Some((lo, hi)),
            _ => None,
        };
        ui.label("find");
        ui.add(egui::TextEdit::singleline(&mut data.filter.contains).desired_width(120.0));
    });

    ui.separator();
    let rows: Vec<String> = data
        .scrollback
        .filtered(&data.filter)
        .map(|e| {
            format!(
                "{:<10} {}",
                e.kind.label(),
                // DEBUGGER.md §4's cross-link: `LDA $0086` renders as
                // `LDA $0086 {player_x_screen}` once that address is
                // annotated. Free when nothing is annotated —
                // `label_operands` returns early on an empty lookup.
                rf_debugger::annotation::label_operands(&e.text, labels)
            )
        })
        .collect();
    if rows.is_empty() {
        ui.label(if data.armed {
            "No entries match the current filters."
        } else {
            "Not tracing. Press Start trace."
        });
        return;
    }
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .show_rows(
            ui,
            ui.text_style_height(&egui::TextStyle::Monospace),
            rows.len(),
            |ui, range| {
                for row in &rows[range] {
                    ui.label(egui::RichText::new(row).monospace());
                }
            },
        );
}

/// What the Audio tab needs (ticket W4-10b).
pub struct AudioPanelData {
    /// One trace per channel, in `rf_nes::apu::CHANNEL_NAMES` order.
    pub traces: Vec<rf_debugger::audio_scope::ScopeTrace>,
    pub mute: rf_debugger::audio_scope::MuteState<{ rf_nes::apu::CHANNEL_COUNT }>,
    /// Set by the panel, acted on by the app — the panel has no channel
    /// of its own, exactly like `TracePanelData::request`.
    pub request: Option<AudioRequest>,
    pub capturing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRequest {
    Start,
    Stop,
    ClearMutes,
}

/// The Audio tab: one scope per channel with mute/solo
/// (DEBUGGER.md §3's "Audio | channel scopes, mute/solo per channel").
fn audio_ui(ui: &mut egui::Ui, data: Option<&mut AudioPanelData>) {
    let Some(data) = data else {
        ui.label("No session — open a ROM to see the audio channels.");
        return;
    };

    ui.horizontal(|ui| {
        if ui
            .button(if data.capturing {
                "Stop scopes"
            } else {
                "Start scopes"
            })
            .clicked()
        {
            data.request = Some(if data.capturing {
                AudioRequest::Stop
            } else {
                AudioRequest::Start
            });
        }
        if ui.button("Clear mutes").clicked() {
            data.request = Some(AudioRequest::ClearMutes);
        }
        // Silence has to be explained, or it reads as a broken emulator.
        if !data.mute.anything_audible() {
            let _ = ui.selectable_label(
                false,
                egui::RichText::new("\u{26a0} everything is muted")
                    .color(egui::Color32::from_rgb(0xE0, 0x80, 0x30)),
            );
        }
    });
    ui.separator();

    for (i, name) in rf_nes::apu::CHANNEL_NAMES.iter().enumerate() {
        ui.horizontal(|ui| {
            let _ = ui.selectable_label(false, *name);
            let mut muted = data.mute.muted[i];
            if ui.checkbox(&mut muted, "M").changed() {
                data.mute.toggle_mute(i);
            }
            let mut soloed = data.mute.soloed[i];
            if ui.checkbox(&mut soloed, "S").changed() {
                data.mute.toggle_solo(i);
            }
            let trace = data.traces.get(i);
            let _ = ui.selectable_label(
                false,
                match trace {
                    Some(t) if t.active => "signal",
                    Some(_) => "silent",
                    None => "no data",
                },
            );
            if !data.mute.audible(i) {
                let _ = ui.selectable_label(false, "(inaudible)");
            }
        });
        if let Some(t) = data.traces.get(i) {
            scope_plot(ui, t);
        }
    }
}

/// Draw one channel's min/max envelope.
///
/// Min and max as a filled band rather than a line through the mean: an
/// averaged square wave is a flat line at its DC offset, which is the
/// single most misleading thing an audio scope can show
/// (`rf_debugger::audio_scope::trace`'s own doc).
fn scope_plot(ui: &mut egui::Ui, trace: &rf_debugger::audio_scope::ScopeTrace) {
    let height = 32.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(20));
    if trace.min.is_empty() {
        return;
    }
    let mid = rect.center().y;
    let scale = height / 2.0 / f32::from(i16::MAX);
    let step = rect.width() / trace.min.len() as f32;
    for (i, (lo, hi)) in trace.min.iter().zip(&trace.max).enumerate() {
        let x = rect.left() + i as f32 * step;
        let y0 = mid - f32::from(*hi) * scale;
        let y1 = mid - f32::from(*lo) * scale;
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x, y0), egui::pos2(x + step.max(1.0), y1)),
            0.0,
            egui::Color32::from_rgb(0x60, 0xC0, 0x80),
        );
    }
}

// ---------------------------------------------------------------------
// Annotations panel (ticket W13-02f; DEBUGGER.md §4).
//
// W13-01's grading found the whole §4 workflow reachable only from tests:
// `AnnotationStore`, `datacrystal::parse_tsv` and
// `profile_export::export_skeleton` were called from
// `crates/retroforge/tests/**` and from nowhere in `src`. VISION §3 sells
// "Debugger → annotation → profile export" as the differentiator against
// Mesen's tiles-only pack builder, so a pipeline only a test can drive is
// the differentiator not shipping. This panel is that pipeline's front
// door.
// ---------------------------------------------------------------------

/// What the app must do on the panel's behalf, because the panel has no
/// filesystem and no session.
///
/// A request rather than a direct call: this module draws, `crate::app`
/// owns the config root, the ROM identity and the profile editor. The same
/// split every other panel here uses (`TraceRequest`, and the audio
/// panel's mute state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationRequest {
    /// Persist the store for the open game (`crate::annotation_store`).
    Save,
    /// Push the armed watchpoints into the running core (ticket W13-02e).
    InstallWatches,
    /// Build a profile skeleton from the store and open it in the profile
    /// editor — GAME_PROFILES.md §3 step 2.
    ExportSkeleton,
}

/// The identity an export needs, filled by `crate::app` when a ROM opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationIdentity {
    pub title: String,
    pub normalized_sha256: String,
    pub console: rf_debugger::profile_export::Console,
}

/// The add/edit form's raw text. Strings rather than parsed values because
/// a half-typed address is a normal state of a form, not an error to
/// report on every keystroke.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnnotationForm {
    pub space_is_rom: bool,
    pub addr: String,
    pub len: String,
    pub ty: String,
    pub label: String,
    pub source: String,
    pub notes: String,
    pub count: String,
}

impl AnnotationForm {
    /// Fill the form from an existing annotation, for editing.
    fn from_annotation(a: &rf_debugger::annotation::Annotation) -> Self {
        use rf_debugger::annotation::AddressSpace;
        AnnotationForm {
            space_is_rom: a.space == AddressSpace::Rom,
            addr: format!("{:X}", a.addr),
            len: a.len.to_string(),
            ty: a.ty.clone(),
            label: a.label.clone(),
            source: a.source.clone(),
            notes: a.notes.clone().unwrap_or_default(),
            count: a.count.map(|c| c.to_string()).unwrap_or_default(),
        }
    }

    /// Parse the form, or say which field is not usable yet.
    ///
    /// Public because it is the panel's contract with the app: the
    /// end-to-end test drives the same parse the Add button does, rather
    /// than a second construction path that could diverge from it.
    ///
    /// `source` and `label` are checked here **and** by
    /// `AnnotationStore::add`. That is not redundancy for its own sake: the
    /// store's check is the structural guarantee, this one exists so the
    /// button can be disabled with a reason instead of the user pressing it
    /// and being told no.
    pub fn parse(&self) -> Result<rf_debugger::annotation::Annotation, String> {
        use rf_debugger::annotation::{AddressSpace, Annotation};
        let addr = u32::from_str_radix(self.addr.trim(), 16)
            .map_err(|_| "address must be hexadecimal".to_string())?;
        let len = match self.len.trim() {
            "" => 1,
            other => other
                .parse::<u32>()
                .map_err(|_| "length must be a number".to_string())?,
        };
        if self.label.trim().is_empty() {
            return Err("a label is required".to_string());
        }
        if self.source.trim().is_empty() {
            // FR-DBG-005 and CONSTRAINTS §2: provenance is not optional.
            return Err("a source is required (FR-DBG-005)".to_string());
        }
        let count = match self.count.trim() {
            "" => None,
            other => Some(
                other
                    .parse::<u32>()
                    .map_err(|_| "count must be a number".to_string())?,
            ),
        };
        Ok(Annotation {
            space: if self.space_is_rom {
                AddressSpace::Rom
            } else {
                AddressSpace::Ram
            },
            addr,
            len,
            ty: match self.ty.trim() {
                "" => "u8".to_string(),
                other => other.to_string(),
            },
            label: self.label.trim().to_string(),
            notes: match self.notes.trim() {
                "" => None,
                other => Some(other.to_string()),
            },
            source: self.source.trim().to_string(),
            count: if self.space_is_rom { count } else { None },
        })
    }
}

/// Everything the annotations tab draws and the app acts on.
#[derive(Debug, Default)]
pub struct AnnotationPanelData {
    pub store: rf_debugger::annotation::AnnotationStore,
    /// `None` until a ROM is open. The panel then refuses to author,
    /// because an annotation with no game to belong to has nowhere to be
    /// saved and no identity to export against.
    pub identity: Option<AnnotationIdentity>,
    pub form: AnnotationForm,
    /// Index being edited, or `None` when the form is an "add".
    pub editing: Option<usize>,
    pub import_text: String,
    pub import_is_rom: bool,
    /// Last thing that went right, shown until the next action.
    pub status: Option<String>,
    /// Last thing that went wrong. Separate from `status` so a success
    /// message cannot quietly overwrite an error the user has not read.
    pub problem: Option<String>,
    pub request: Option<AnnotationRequest>,
    /// Whether the store has changed since the last successful save.
    pub dirty: bool,
    /// Ticket W13-02e: the armed watchpoints, in the order they were
    /// added. Lives beside the annotations because promotion is the
    /// point — a watch is how a ROM hacker finds the address an
    /// annotation is eventually about (DEBUGGER.md §1's "watchpoints
    /// double as profile probes").
    pub watches: Vec<rf_core_api::MemWatch>,
    pub watch_form: WatchForm,
    /// How many times each watch has fired since it was armed, by id.
    /// A count rather than a log: the useful question at the bench is
    /// "does this address get touched at all", and a per-hit log would
    /// grow without bound in exactly the sessions where the answer is
    /// obviously yes.
    pub watch_hits: std::collections::BTreeMap<u32, u64>,
    /// Next id to hand out. Monotonic and never reused, so a stale hit
    /// report can never be attributed to a different watch.
    pub next_watch_id: u32,
}

/// The add-a-watchpoint form's raw text (ticket W13-02e).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchForm {
    pub space_is_ppu: bool,
    /// 0 = read, 1 = write, 2 = any.
    pub access: u8,
    pub start: String,
    /// Blank means a one-byte watch — `start == end`, since the range is
    /// inclusive at both ends.
    pub end: String,
    /// Blank means "any value".
    pub value_mask: String,
    pub value_equals: String,
}

impl WatchForm {
    /// Parse the form into a watch with `id`, or say what is unusable.
    ///
    /// # Errors
    /// A human-readable reason, shown on the disabled button rather than
    /// after a rejected press.
    pub fn parse(&self, id: u32) -> Result<rf_core_api::MemWatch, String> {
        let start = u32::from_str_radix(self.start.trim(), 16)
            .map_err(|_| "start address must be hexadecimal".to_string())?;
        let end = match self.end.trim() {
            "" => start,
            other => u32::from_str_radix(other, 16)
                .map_err(|_| "end address must be hexadecimal".to_string())?,
        };
        if end < start {
            return Err("the end address is below the start".to_string());
        }
        let hex_byte = |text: &str, what: &str| -> Result<u8, String> {
            match text.trim() {
                "" => Ok(0),
                other => {
                    u8::from_str_radix(other, 16).map_err(|_| format!("{what} must be a hex byte"))
                }
            }
        };
        Ok(rf_core_api::MemWatch {
            id,
            space: if self.space_is_ppu {
                rf_core_api::WatchSpace::Ppu
            } else {
                rf_core_api::WatchSpace::Cpu
            },
            start,
            end,
            access: match self.access {
                0 => rf_core_api::WatchAccess::Read,
                1 => rf_core_api::WatchAccess::Write,
                _ => rf_core_api::WatchAccess::Any,
            },
            value_mask: hex_byte(&self.value_mask, "the value mask")?,
            value_equals: hex_byte(&self.value_equals, "the value")?,
        })
    }
}

impl AnnotationPanelData {
    /// Adopt a freshly loaded store for a newly opened game.
    pub fn adopt(
        &mut self,
        store: rf_debugger::annotation::AnnotationStore,
        identity: Option<AnnotationIdentity>,
        problem: Option<String>,
    ) {
        self.store = store;
        self.identity = identity;
        self.problem = problem;
        self.status = None;
        self.editing = None;
        self.form = AnnotationForm::default();
        self.dirty = false;
        // A new game is a new machine: watches armed against the last
        // game's addresses would fire on unrelated memory and their hit
        // counts would be someone else's session.
        self.watches.clear();
        self.watch_hits.clear();
        self.request = Some(AnnotationRequest::InstallWatches);
    }

    /// Whether anything is watched — the app reads this to decide whether
    /// the core needs a `MEM_WATCH` subscription at all (DEBUGGER.md §6's
    /// pay-for-use: no watches, no events).
    #[must_use]
    pub fn has_watches(&self) -> bool {
        !self.watches.is_empty()
    }

    /// Fold this frame's events into the per-watch hit counts.
    pub fn record_watch_hits(&mut self, events: &[rf_core_api::CoreEvent]) {
        if self.watches.is_empty() {
            return;
        }
        for id in rf_debugger::breakpoint::watch_hits(events) {
            *self.watch_hits.entry(id).or_insert(0) += 1;
        }
    }
}

fn annotations_ui(ui: &mut egui::Ui, data: &mut AnnotationPanelData) {
    let Some(identity) = data.identity.clone() else {
        ui.label("No game open — open a ROM to annotate it.");
        ui.label(
            egui::RichText::new(
                "Annotations are stored per normalized ROM hash, so they need a game to belong to.",
            )
            .small()
            .weak(),
        );
        return;
    };

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(&identity.title).strong());
        ui.label(
            egui::RichText::new(format!("{} annotations", data.store.len()))
                .small()
                .weak(),
        );
        if data.dirty {
            ui.label(egui::RichText::new("• unsaved").small());
        }
    });

    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            data.request = Some(AnnotationRequest::Save);
        }
        if ui
            .add_enabled(
                !data.store.is_empty(),
                egui::Button::new("Export profile skeleton\u{2026}"),
            )
            .on_hover_text("Builds a schema-v0 profile from these annotations and opens it in the profile editor")
            .clicked()
        {
            data.request = Some(AnnotationRequest::ExportSkeleton);
        }
    });

    if let Some(problem) = &data.problem {
        ui.colored_label(egui::Color32::from_rgb(0xE0, 0x80, 0x30), problem);
    }
    if let Some(status) = &data.status {
        ui.label(egui::RichText::new(status).small().weak());
    }

    ui.separator();
    // The panel stacks a form, a list and an import box; on a short pane
    // the import box is the first thing to fall off the bottom, and it is
    // the half of the workflow a new author reaches for. The list keeps
    // its own bounded scroll inside this one so the Save/Export controls
    // stay put no matter how many labels exist.
    egui::ScrollArea::vertical().show(ui, |ui| {
        annotation_form_ui(ui, data);
        ui.separator();
        annotation_list_ui(ui, data);
        ui.separator();
        watchpoint_ui(ui, data);
        ui.separator();
        annotation_import_ui(ui, data);
    });
}

/// Watchpoints and their promotion (ticket W13-02e; DEBUGGER.md section 1).
///
/// In *this* panel rather than a separate one because promotion is the
/// point: section 1's last bullet has a watchpoint becoming a `memory_map`
/// annotation "with one click", and a click that has to cross two panels
/// is not that. A watch is how a ROM hacker *finds* the address an
/// annotation is eventually about.
fn watchpoint_ui(ui: &mut egui::Ui, data: &mut AnnotationPanelData) {
    egui::CollapsingHeader::new(format!("Watchpoints ({})", data.watches.len()))
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("space");
                ui.selectable_value(&mut data.watch_form.space_is_ppu, false, "CPU");
                ui.selectable_value(&mut data.watch_form.space_is_ppu, true, "PPU");
                ui.separator();
                ui.selectable_value(&mut data.watch_form.access, 0, "read");
                ui.selectable_value(&mut data.watch_form.access, 1, "write");
                ui.selectable_value(&mut data.watch_form.access, 2, "any");
            });
            ui.horizontal(|ui| {
                ui.label("$");
                ui.add(egui::TextEdit::singleline(&mut data.watch_form.start).desired_width(64.0));
                ui.label("to $");
                ui.add(egui::TextEdit::singleline(&mut data.watch_form.end).desired_width(64.0))
                    .on_hover_text("blank watches a single byte");
                ui.label("value &");
                ui.add(
                    egui::TextEdit::singleline(&mut data.watch_form.value_mask).desired_width(36.0),
                );
                ui.label("==");
                ui.add(
                    egui::TextEdit::singleline(&mut data.watch_form.value_equals)
                        .desired_width(36.0),
                );
            });

            let parsed = data.watch_form.parse(data.next_watch_id);
            let (enabled, hint) = match &parsed {
                Ok(_) => (true, String::new()),
                Err(why) => (false, why.clone()),
            };
            ui.horizontal(|ui| {
                let button = ui.add_enabled(enabled, egui::Button::new("Arm"));
                let button = if hint.is_empty() {
                    button
                } else {
                    button.on_disabled_hover_text(hint.clone())
                };
                if button.clicked() {
                    if let Ok(watch) = parsed {
                        data.watches.push(watch);
                        // Ids are never reused, so a hit reported for an id
                        // that has been disarmed is ignored rather than
                        // credited to whatever took its slot.
                        data.next_watch_id = data.next_watch_id.wrapping_add(1);
                        data.watch_form = WatchForm::default();
                        data.request = Some(AnnotationRequest::InstallWatches);
                    }
                }
                if !enabled {
                    ui.label(egui::RichText::new(hint).small().weak());
                }
                if data.watches.len() > rf_core_api::MAX_WATCHES {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xE0, 0x80, 0x30),
                        format!("only the first {} are armed", rf_core_api::MAX_WATCHES),
                    );
                }
            });

            if data.watches.is_empty() {
                ui.label(
                    egui::RichText::new("Nothing watched - the core pays no cost.")
                        .small()
                        .weak(),
                );
                return;
            }

            // Same collect-then-apply discipline as the annotation list:
            // mutating while iterating is what makes a list panel panic on
            // the frame someone presses a button.
            let mut disarm: Option<usize> = None;
            let mut promote: Option<usize> = None;
            for (i, w) in data.watches.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(match w.space {
                            rf_core_api::WatchSpace::Cpu => "CPU",
                            rf_core_api::WatchSpace::Ppu => "PPU",
                        })
                        .small()
                        .weak(),
                    );
                    ui.monospace(if w.start == w.end {
                        format!("${:04X}", w.start)
                    } else {
                        format!("${:04X}-${:04X}", w.start, w.end)
                    });
                    ui.label(
                        egui::RichText::new(match w.access {
                            rf_core_api::WatchAccess::Read => "read",
                            rf_core_api::WatchAccess::Write => "write",
                            rf_core_api::WatchAccess::Any => "any",
                        })
                        .small()
                        .weak(),
                    );
                    let hits = data.watch_hits.get(&w.id).copied().unwrap_or(0);
                    ui.label(format!("{hits} hits"));
                    if ui
                        .add_enabled(
                            w.space == rf_core_api::WatchSpace::Cpu,
                            egui::Button::new("promote").small(),
                        )
                        .on_hover_text("turn this watch into a labelled annotation")
                        .on_disabled_hover_text(
                            "a profile's memory_map is CPU-bus addressed, so a PPU watch has no \
                             row to become",
                        )
                        .clicked()
                    {
                        promote = Some(i);
                    }
                    if ui.small_button("disarm").clicked() {
                        disarm = Some(i);
                    }
                });
            }
            if let Some(i) = promote {
                let watch = data.watches[i];
                // Promotion FILLS THE FORM rather than storing straight
                // away: section 1 gives the click address, size and label,
                // and only the first two are things a watch knows. The
                // source FR-DBG-005 requires is a human's to write, and
                // inventing one would be the exact provenance failure
                // CONSTRAINTS section 2 exists to prevent.
                data.form = AnnotationForm {
                    space_is_rom: false,
                    addr: format!("{:X}", watch.start),
                    len: (watch.end - watch.start + 1).to_string(),
                    ty: if watch.end > watch.start { "u16" } else { "u8" }.to_string(),
                    label: String::new(),
                    source: String::new(),
                    notes: String::new(),
                    count: String::new(),
                };
                data.editing = None;
                data.status = Some(format!(
                    "promoted ${:04X} - give it a label and a source",
                    watch.start
                ));
            }
            if let Some(i) = disarm {
                let removed = data.watches.remove(i);
                data.watch_hits.remove(&removed.id);
                data.request = Some(AnnotationRequest::InstallWatches);
            }
        });
}

fn annotation_form_ui(ui: &mut egui::Ui, data: &mut AnnotationPanelData) {
    ui.label(egui::RichText::new(match data.editing {
        Some(i) => format!("Editing #{i}"),
        None => "New annotation".to_string(),
    }));
    egui::Grid::new("annotation_form")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("space");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut data.form.space_is_rom, false, "RAM");
                ui.selectable_value(&mut data.form.space_is_rom, true, "ROM");
            });
            ui.end_row();
            ui.label("address $");
            ui.add(egui::TextEdit::singleline(&mut data.form.addr).desired_width(80.0));
            ui.end_row();
            ui.label("length");
            ui.add(egui::TextEdit::singleline(&mut data.form.len).desired_width(60.0));
            ui.end_row();
            ui.label("type");
            ui.add(egui::TextEdit::singleline(&mut data.form.ty).desired_width(80.0));
            ui.end_row();
            ui.label("label");
            ui.add(egui::TextEdit::singleline(&mut data.form.label).desired_width(200.0));
            ui.end_row();
            ui.label("source");
            ui.add(egui::TextEdit::singleline(&mut data.form.source).desired_width(280.0));
            ui.end_row();
            ui.label("notes");
            ui.add(egui::TextEdit::multiline(&mut data.form.notes).desired_rows(2));
            ui.end_row();
            if data.form.space_is_rom {
                ui.label("count");
                ui.add(egui::TextEdit::singleline(&mut data.form.count).desired_width(60.0));
                ui.end_row();
            }
        });

    // The parse runs every repaint so the button can carry the reason it
    // is disabled — CONSTRAINTS §2's required provenance shown as a
    // condition of the control, not as a rejection after the fact.
    let parsed = data.form.parse();
    ui.horizontal(|ui| {
        let (enabled, hint) = match &parsed {
            Ok(_) => (true, String::new()),
            Err(why) => (false, why.clone()),
        };
        let button = ui.add_enabled(
            enabled,
            egui::Button::new(if data.editing.is_some() {
                "Apply"
            } else {
                "Add"
            }),
        );
        let button = if hint.is_empty() {
            button
        } else {
            button.on_disabled_hover_text(hint.clone())
        };
        if button.clicked() {
            if let Ok(annotation) = parsed {
                let outcome = match data.editing {
                    Some(i) => data.store.replace(i, annotation),
                    None => data.store.add(annotation),
                };
                match outcome {
                    Ok(()) => {
                        data.dirty = true;
                        data.problem = None;
                        data.status = Some(match data.editing {
                            Some(i) => format!("updated #{i}"),
                            None => "added".to_string(),
                        });
                        data.editing = None;
                        data.form = AnnotationForm::default();
                    }
                    Err(e) => data.problem = Some(e.to_string()),
                }
            }
        }
        if data.editing.is_some() && ui.button("Cancel").clicked() {
            data.editing = None;
            data.form = AnnotationForm::default();
        }
        if !enabled {
            ui.label(egui::RichText::new(hint).small().weak());
        }
    });
}

fn annotation_list_ui(ui: &mut egui::Ui, data: &mut AnnotationPanelData) {
    use rf_debugger::annotation::AddressSpace;
    if data.store.is_empty() {
        ui.label("No annotations yet.");
        return;
    }
    // Actions are collected and applied after the loop: mutating the store
    // while iterating it is the classic shape that makes a list panel
    // panic on the frame someone presses delete.
    let mut edit: Option<usize> = None;
    let mut delete: Option<usize> = None;
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .show(ui, |ui| {
            for (i, a) in data.store.entries().iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(match a.space {
                            AddressSpace::Ram => "RAM",
                            AddressSpace::Rom => "ROM",
                        })
                        .small()
                        .weak(),
                    );
                    ui.monospace(format!("${:04X}", a.addr));
                    ui.label(&a.label);
                    ui.label(egui::RichText::new(&a.ty).small().weak());
                    if ui.small_button("edit").clicked() {
                        edit = Some(i);
                    }
                    if ui.small_button("delete").clicked() {
                        delete = Some(i);
                    }
                });
            }
        });
    if let Some(i) = edit {
        if let Some(a) = data.store.get(i) {
            data.form = AnnotationForm::from_annotation(a);
            data.editing = Some(i);
        }
    }
    if let Some(i) = delete {
        if data.store.remove(i).is_some() {
            data.dirty = true;
            data.status = Some(format!("deleted #{i}"));
            // An edit targeting the row that just vanished must not carry
            // on pointing at whatever slid into its index.
            if data.editing == Some(i) {
                data.editing = None;
                data.form = AnnotationForm::default();
            }
        }
    }
}

fn annotation_import_ui(ui: &mut egui::Ui, data: &mut AnnotationPanelData) {
    use rf_debugger::annotation::AddressSpace;
    egui::CollapsingHeader::new("Import DataCrystal TSV")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    "Paste address/len/label rows. Notes are never imported — CONSTRAINTS §2 \
                     requires prose to be written fresh, not transcribed.",
                )
                .small()
                .weak(),
            );
            ui.horizontal(|ui| {
                ui.label("space");
                ui.selectable_value(&mut data.import_is_rom, false, "RAM");
                ui.selectable_value(&mut data.import_is_rom, true, "ROM");
            });
            ui.add(
                egui::TextEdit::multiline(&mut data.import_text)
                    .desired_rows(4)
                    .code_editor(),
            );
            if ui
                .add_enabled(
                    !data.import_text.trim().is_empty(),
                    egui::Button::new("Import"),
                )
                .clicked()
            {
                let space = if data.import_is_rom {
                    AddressSpace::Rom
                } else {
                    AddressSpace::Ram
                };
                match rf_debugger::datacrystal::parse_tsv(&data.import_text, space) {
                    Ok(rows) => {
                        let mut added = 0usize;
                        let mut refused = 0usize;
                        for row in rows {
                            match data.store.add(row) {
                                Ok(()) => added += 1,
                                Err(_) => refused += 1,
                            }
                        }
                        data.dirty |= added > 0;
                        data.problem = None;
                        data.status = Some(if refused == 0 {
                            format!("imported {added}")
                        } else {
                            // Refusals are counted, never silent: an
                            // import that quietly drops rows is how a
                            // profile ends up missing facts its author
                            // believes it has.
                            format!("imported {added}, refused {refused}")
                        });
                        data.import_text.clear();
                    }
                    Err(e) => data.problem = Some(e.to_string()),
                }
            }
        });
}

// ---------------------------------------------------------------------
// The SNES viewer column (ticket W13-02b; DEBUGGER.md §3's SNES cells).
//
// Every panel here is the SNES arm of a tab the NES already had, chosen
// by whether `PanelData::snes` is `Some` — one DebugTab, two consoles,
// which is criterion 3. The decode itself lives in `rf_snes::debug`
// (see that module's doc for why it cannot live in `rf-debugger`).
// ---------------------------------------------------------------------

use crate::core_thread::SnesDebugFrame;

/// Bit depths per BG for the current mode, from `$2105`.
fn snes_bg_depths(snes: &SnesDebugFrame) -> [u8; 4] {
    let mode = snes.ppu_regs.get(0x05).copied().unwrap_or(0) & 0x07;
    rf_snes::ppu::bg::bit_depths(mode)
}

/// Character base for a BG, from `$210B`/`$210C`, in VRAM words.
fn snes_char_base(snes: &SnesDebugFrame, bg: usize) -> u16 {
    let reg = snes.ppu_regs.get(0x0B + bg / 2).copied().unwrap_or(0);
    let nibble = if bg.is_multiple_of(2) {
        reg & 0x0F
    } else {
        reg >> 4
    };
    u16::from(nibble) << 12
}

/// Tilemap base and size code for a BG, from `$2107`-`$210A`.
fn snes_tilemap_reg(snes: &SnesDebugFrame, bg: usize) -> (u16, u8) {
    let reg = snes.ppu_regs.get(0x07 + bg).copied().unwrap_or(0);
    ((u16::from(reg >> 2)) << 10, reg & 0x03)
}

fn snes_bg_selector(ui: &mut egui::Ui, snes: &SnesDebugFrame, bg: &mut usize) {
    let depths = snes_bg_depths(snes);
    ui.horizontal(|ui| {
        for (i, depth) in depths.iter().enumerate() {
            // A layer the current mode does not have is shown disabled
            // rather than hidden, so the mode's shape is visible: mode 1
            // having no BG4 is a fact about the game, not a missing
            // feature of the viewer.
            let exists = *depth > 0;
            let label = if exists {
                format!("BG{} ({depth}bpp)", i + 1)
            } else {
                format!("BG{}", i + 1)
            };
            if ui
                .add_enabled(exists, egui::Button::selectable(*bg == i, label))
                .clicked()
            {
                *bg = i;
            }
        }
    });
    if depths[*bg] == 0 {
        // Fall back rather than render a layer this mode does not have.
        *bg = depths.iter().position(|d| *d > 0).unwrap_or(0);
    }
}

/// CHR viewer: a page of tiles decoded at the selected BG's depth and
/// character base (DEBUGGER.md §3: "VRAM char data per BG char-base,
/// 2/4/8bpp decode").
fn snes_pattern_ui(ui: &mut egui::Ui, snes: &SnesDebugFrame, bg: &mut usize) {
    snes_bg_selector(ui, snes, bg);
    let depth = snes_bg_depths(snes)[*bg].max(2);
    let char_base = snes_char_base(snes, *bg);
    ui.label(
        egui::RichText::new(format!(
            "char base ${:04X} words · {}bpp · first 256 tiles",
            char_base, depth
        ))
        .small()
        .weak(),
    );

    // 16x16 tiles of 8x8 pixels, drawn through the game's own palette for
    // this layer so the page reads as the artwork rather than as indices.
    const TILES: usize = 16;
    let side = TILES * 8;
    let mut img = egui::ColorImage::new([side, side], vec![egui::Color32::BLACK; side * side]);
    let palette_base = u16::from(rf_snes::ppu::bg::palette_base(
        snes.ppu_regs.get(0x05).copied().unwrap_or(0) & 0x07,
        *bg,
    ));
    for ty in 0..TILES {
        for tx in 0..TILES {
            let character = (ty * TILES + tx) as u16;
            for py in 0..8u16 {
                for px in 0..8u16 {
                    let index =
                        rf_snes::debug::tile_pixel(&snes.vram, char_base, character, px, py, depth);
                    // Index 0 is transparent on every SNES layer, so it is
                    // drawn as the backdrop rather than as palette entry 0
                    // — otherwise every tile sits on a coloured block that
                    // the game never draws.
                    let colour = if index == 0 {
                        egui::Color32::from_gray(0x18)
                    } else {
                        let entry = (palette_base as usize + index as usize).min(255) as u8;
                        let [r, g, b] = rf_snes::debug::cgram_rgb(&snes.cgram, entry);
                        egui::Color32::from_rgb(r, g, b)
                    };
                    img.pixels[(ty * 8 + py as usize) * side + tx * 8 + px as usize] = colour;
                }
            }
        }
    }
    let texture = ui
        .ctx()
        .load_texture("snes-chr", img, egui::TextureOptions::NEAREST);
    ui.add(egui::Image::new(&texture).fit_to_original_size(2.0));
}

/// Tilemap viewer: the selected BG's map with its flip and priority flags
/// (DEBUGGER.md §3: "per-BG tilemaps w/ tile flip/prio flags").
fn snes_tilemap_ui(ui: &mut egui::Ui, snes: &SnesDebugFrame, bg: &mut usize) {
    snes_bg_selector(ui, snes, bg);
    let (base, size) = snes_tilemap_reg(snes, *bg);
    let (w, h) = rf_snes::debug::tilemap_dimensions(size);
    ui.label(
        egui::RichText::new(format!("base ${base:04X} words · {w}x{h} tiles"))
            .small()
            .weak(),
    );
    egui::ScrollArea::both().show(ui, |ui| {
        egui::Grid::new("snes-tilemap")
            .striped(true)
            .show(ui, |ui| {
                // The first 16x16 corner: a full 64x64 map is 4096 cells
                // and a hex grid of that size is unreadable anyway.
                for ty in 0..16u16.min(h) {
                    for tx in 0..16u16.min(w) {
                        let cell = rf_snes::debug::tilemap_entry(&snes.vram, base, size, tx, ty);
                        let mut text =
                            egui::RichText::new(format!("{:03X}", cell.character)).monospace();
                        if cell.priority {
                            text = text.strong();
                        }
                        let flips = match (cell.flip_x, cell.flip_y) {
                            (false, false) => "",
                            (true, false) => "H",
                            (false, true) => "V",
                            (true, true) => "HV",
                        };
                        ui.add(egui::Label::new(text)).on_hover_text(format!(
                            "tile {:03X} · palette {} · priority {} · flip {}",
                            cell.character,
                            cell.palette,
                            u8::from(cell.priority),
                            if flips.is_empty() { "none" } else { flips }
                        ));
                    }
                    ui.end_row();
                }
            });
    });
}

/// CGRAM viewer: 256 entries, plus the colour-math state that decides how
/// they combine (DEBUGGER.md §3: "CGRAM 256, color-math preview").
fn snes_palette_ui(ui: &mut egui::Ui, snes: &SnesDebugFrame) {
    ui.label(
        egui::RichText::new("CGRAM — 256 entries, BGR555")
            .small()
            .weak(),
    );
    let cell = egui::vec2(14.0, 14.0);
    egui::Grid::new("snes-cgram")
        .spacing([1.0, 1.0])
        .show(ui, |ui| {
            for row in 0..16 {
                for col in 0..16 {
                    let index = (row * 16 + col) as u8;
                    let [r, g, b] = rf_snes::debug::cgram_rgb(&snes.cgram, index);
                    let (rect, response) = ui.allocate_exact_size(cell, egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 0.0, egui::Color32::from_rgb(r, g, b));
                    response.on_hover_text(format!("${index:02X} — #{r:02X}{g:02X}{b:02X}"));
                }
                ui.end_row();
            }
        });
    // The colour-math preview §3 asks for: what $2130/$2131 would do to
    // these colours, stated rather than simulated, because the arithmetic
    // is per-pixel and depends on which layers a pixel came from.
    let cgwsel = snes.ppu_regs.get(0x30).copied().unwrap_or(0);
    let cgadsub = snes.ppu_regs.get(0x31).copied().unwrap_or(0);
    ui.separator();
    ui.label(format!(
        "colour math: {} · {}{} · layers {:05b}",
        if cgadsub & 0x80 != 0 {
            "subtract"
        } else {
            "add"
        },
        if cgadsub & 0x40 != 0 { "half" } else { "full" },
        if cgwsel & 0x01 != 0 {
            " · direct colour"
        } else {
            ""
        },
        cgadsub & 0x1F,
    ));
}

/// OAM viewer: 128 entries with the high table decoded, plus the
/// 32-per-line occupancy (DEBUGGER.md §3: "128 entries, 32/line ...
/// size/base decode").
fn snes_oam_ui(ui: &mut egui::Ui, snes: &SnesDebugFrame, scanline: u16) {
    let sprites = rf_snes::debug::decode_oam(&snes.oam);
    let obsel = snes.ppu_regs.get(0x01).copied().unwrap_or(0);
    let obj_size = (obsel >> 5) & 0x07;
    let (small, big) = rf_snes::debug::obj_sizes(obj_size);
    let on_line = rf_snes::debug::line_occupancy(&sprites, scanline, obj_size);

    ui.horizontal(|ui| {
        ui.label(format!("sizes {small:?} / {big:?}"));
        ui.separator();
        let over = on_line > rf_snes::debug::OBJ_PER_LINE_LIMIT;
        let text = format!(
            "line {scanline}: {on_line}/{}",
            rf_snes::debug::OBJ_PER_LINE_LIMIT
        );
        if over {
            // The same honesty the NES panel's 8-per-line bar has: the
            // hardware drops sprites past the limit, and a viewer that
            // did not say so would show sprites the player cannot see.
            ui.colored_label(egui::Color32::from_rgb(0xE0, 0x80, 0x30), text);
        } else {
            ui.label(text);
        }
    });
    egui::ScrollArea::vertical()
        .max_height(260.0)
        .show(ui, |ui| {
            egui::Grid::new("snes-oam").striped(true).show(ui, |ui| {
                for s in sprites.iter().filter(|s| s.y != 0xF0) {
                    ui.monospace(format!("{:3}", s.index));
                    ui.monospace(format!("{:4},{:3}", s.x, s.y));
                    ui.monospace(format!("t{:03X}", s.tile));
                    ui.monospace(format!("p{}", s.palette));
                    ui.monospace(format!("pr{}", s.priority));
                    ui.label(if s.large { "large" } else { "small" });
                    ui.label(match (s.flip_x, s.flip_y) {
                        (false, false) => "",
                        (true, false) => "H",
                        (false, true) => "V",
                        (true, true) => "HV",
                    });
                    ui.end_row();
                }
            });
        });
}

/// Memory view over the three SNES-only spaces (DEBUGGER.md §3:
/// "+VRAM/CGRAM/ARAM/DMA regs spaces, 24-bit addressing").
fn snes_memory_ui(ui: &mut egui::Ui, snes: &SnesDebugFrame, space: &mut usize) {
    ui.horizontal(|ui| {
        ui.selectable_value(space, 0, "VRAM");
        ui.selectable_value(space, 1, "CGRAM");
        ui.selectable_value(space, 2, "ARAM");
    });
    let (bytes, label): (&[u8], &str) = match space {
        1 => (&snes.cgram, "CGRAM ($00-$FF, 2 bytes per entry)"),
        2 => (&snes.aram, "ARAM ($0000-$FFFF)"),
        _ => (&snes.vram, "VRAM ($0000-$FFFF)"),
    };
    ui.label(egui::RichText::new(label).small().weak());
    egui::ScrollArea::vertical().show(ui, |ui| {
        // The first 2 KiB. These spaces are 64 KiB and a hex dump of that
        // is 4096 rows — the goto/find controls W13-02d added are what
        // reach the rest, and wiring them to these spaces is W13-02c's.
        let shown = &bytes[..bytes.len().min(0x800)];
        let rows = rf_debugger::memory_view::build_rows(0, shown);
        egui::Grid::new("snes-memory").striped(true).show(ui, |ui| {
            for row in &rows {
                ui.monospace(format!("{:04X}", row.addr));
                ui.monospace(
                    row.bytes
                        .iter()
                        .map(|b| format!("{b:02X} "))
                        .collect::<String>(),
                );
                ui.end_row();
            }
        });
    });
}

// ---------------------------------------------------------------------
// Mode 7, HDMA lanes and the DSP voice view (ticket W13-02c).
// ---------------------------------------------------------------------

/// The mode-7 playfield with the camera trapezoid drawn over it
/// (DEBUGGER.md §3: "Mode 7 view (1024x1024 playfield + camera
/// trapezoid)").
fn snes_mode7_ui(ui: &mut egui::Ui, snes: &SnesDebugFrame) {
    let mode = snes
        .ppu_regs
        .first()
        .map_or(0, |_| snes.ppu_regs.get(0x05).copied().unwrap_or(0) & 0x07);
    if mode != 7 {
        // Shown, not hidden: the registers are real whatever mode is
        // live, and a game that has just left mode 7 still has the matrix
        // that put it where it is. Saying which mode is running is the
        // honest version of both.
        ui.label(
            egui::RichText::new(format!("BG mode {mode} — the matrix below is not in use"))
                .small()
                .weak(),
        );
    }
    let m = &snes.mode7;
    ui.monospace(format!(
        "A {:6}  B {:6}   X0 {:5}  HOFS {:5}",
        m.a, m.b, m.x0, m.hofs
    ));
    ui.monospace(format!(
        "C {:6}  D {:6}   Y0 {:5}  VOFS {:5}",
        m.c, m.d, m.y0, m.vofs
    ));
    ui.label(
        egui::RichText::new(format!(
            "screen-over {} · flip {}{}",
            m.screen_over,
            if m.flip_x { "H" } else { "" },
            if m.flip_y { "V" } else { "-" }
        ))
        .small()
        .weak(),
    );

    // The playfield at 1/4 scale: 1024 square is far past any pane, and a
    // 256-square thumbnail is what makes the trapezoid's SHAPE readable,
    // which is the whole point of the view.
    const SHOWN: usize = 256;
    const STEP: u16 = (rf_snes::debug::MODE7_SIDE / SHOWN) as u16;
    let mut img = egui::ColorImage::new([SHOWN, SHOWN], vec![egui::Color32::BLACK; SHOWN * SHOWN]);
    for y in 0..SHOWN {
        for x in 0..SHOWN {
            let index =
                rf_snes::debug::mode7_pixel(&snes.vram, (x as u16) * STEP, (y as u16) * STEP);
            let [r, g, b] = rf_snes::debug::cgram_rgb(&snes.cgram, index);
            img.pixels[y * SHOWN + x] = egui::Color32::from_rgb(r, g, b);
        }
    }
    let texture = ui
        .ctx()
        .load_texture("snes-mode7", img, egui::TextureOptions::NEAREST);
    let response = ui
        .add(egui::Image::new(&texture).fit_to_exact_size(egui::vec2(SHOWN as f32, SHOWN as f32)));

    // The trapezoid, in the same 1/4 scale, from the projection the
    // renderer itself uses.
    let corners = rf_snes::debug::mode7_camera_corners(m, 256, 224);
    let origin = response.rect.min;
    let scale = SHOWN as f32 / rf_snes::debug::MODE7_SIDE as f32;
    let points: Vec<egui::Pos2> = corners
        .iter()
        .map(|(x, y)| {
            // Wrapped into the playfield, because that is where those
            // samples actually read from — a corner at -50 is reading
            // 974, not off the edge.
            let wx = (x.rem_euclid(rf_snes::debug::MODE7_SIDE as i32)) as f32;
            let wy = (y.rem_euclid(rf_snes::debug::MODE7_SIDE as i32)) as f32;
            origin + egui::vec2(wx * scale, wy * scale)
        })
        .collect();
    let painter = ui.painter_at(response.rect);
    let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(0xFF, 0xC8, 0x50));
    for i in 0..4 {
        painter.line_segment([points[i], points[(i + 1) % 4]], stroke);
    }
}

/// HDMA channel lanes: one row per channel, one column per scanline
/// (DEBUGGER.md §3's "+ HDMA channel lanes per scanline").
fn snes_hdma_lanes_ui(ui: &mut egui::Ui, snes: &SnesDebugFrame) {
    ui.label(
        egui::RichText::new("HDMA — a mark where a channel moved bytes on that line")
            .small()
            .weak(),
    );
    let lines = snes.hdma_lanes.len().min(262);
    let width = ui.available_width().min(lines as f32 * 2.0).max(64.0);
    let row_h = 10.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(width, row_h * 8.0 + 4.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(0x14));
    let x_of = |line: usize| rect.min.x + (line as f32 / lines.max(1) as f32) * rect.width();
    for ch in 0..8usize {
        let y = rect.min.y + ch as f32 * row_h + 2.0;
        for (line, mask) in snes.hdma_lanes.iter().take(lines).enumerate() {
            if mask & (1 << ch) != 0 {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(x_of(line), y),
                        egui::vec2((rect.width() / lines.max(1) as f32).max(1.0), row_h - 2.0),
                    ),
                    0.0,
                    egui::Color32::from_rgb(0x60, 0xC0, 0xF0),
                );
            }
        }
    }
    // A frame in which nothing ran is a real answer, and a blank strip
    // alone would read as a broken viewer rather than as "no HDMA".
    if snes.hdma_lanes.iter().all(|m| *m == 0) {
        ui.label(
            egui::RichText::new("no HDMA transfers this frame")
                .small()
                .weak(),
        );
    }
}

/// DSP voice states and a BRR preview (DEBUGGER.md §3: "+DSP voice
/// states, BRR source view").
fn snes_dsp_ui(ui: &mut egui::Ui, snes: &SnesDebugFrame, selected: &mut usize) {
    egui::Grid::new("snes-dsp").striped(true).show(ui, |ui| {
        ui.label("v");
        ui.label("srcn");
        ui.label("pitch");
        ui.label("env");
        ui.label("vol L/R");
        ui.label("out");
        ui.end_row();
        for v in &snes.voices {
            let on = v.keyed_on;
            let tag = |t: String| {
                let text = egui::RichText::new(t).monospace();
                if on {
                    text.strong()
                } else {
                    text.weak()
                }
            };
            if ui
                .add(egui::Button::selectable(
                    *selected == usize::from(v.index),
                    format!("{}", v.index),
                ))
                .clicked()
            {
                *selected = usize::from(v.index);
            }
            ui.label(tag(format!("{:02X}", v.srcn)));
            // $1000 is 1.0 — showing the ratio as well as the raw value,
            // because "4096" means nothing and "1.00x" is the thing a
            // musician is looking for.
            ui.label(tag(format!(
                "{:04X} {:.2}x",
                v.pitch,
                f32::from(v.pitch) / 4096.0
            )));
            ui.label(tag(format!("{:4}", v.envelope_level)));
            ui.label(tag(format!("{:4}/{:4}", v.vol_left, v.vol_right)));
            ui.label(tag(format!("{:6}", v.last_output)));
            ui.end_row();
        }
    });

    let Some(voice) = snes.voices.get(*selected) else {
        return;
    };
    ui.separator();
    ui.label(
        egui::RichText::new(format!(
            "voice {} BRR at ${:04X} (loop ${:04X})",
            voice.index, voice.start, voice.loop_addr
        ))
        .small()
        .weak(),
    );

    // Decode a short run from the sample's start. Sequential, because
    // filters 1-3 are recursive — a random-access decoder would draw a
    // different waveform than the DSP plays.
    const BLOCKS: usize = 8;
    let mut prev = (0i16, 0i16);
    let mut samples: Vec<i16> = Vec::with_capacity(BLOCKS * 16);
    let mut addr = voice.start;
    let mut ended = false;
    for _ in 0..BLOCKS {
        let block = rf_snes::debug::decode_brr_block(&snes.aram, addr, prev);
        samples.extend_from_slice(&block.samples);
        prev = (block.samples[14], block.samples[15]);
        if block.end {
            ended = true;
            break;
        }
        addr = addr.wrapping_add(9);
    }
    if ended {
        ui.label(egui::RichText::new("end flag reached").small().weak());
    }

    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(320.0), 60.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(0x14));
    let mid = rect.center().y;
    let stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(0x80, 0xE0, 0x90));
    for (i, pair) in samples.windows(2).enumerate() {
        let x0 = rect.min.x + (i as f32 / samples.len().max(1) as f32) * rect.width();
        let x1 = rect.min.x + ((i + 1) as f32 / samples.len().max(1) as f32) * rect.width();
        let scale = rect.height() / 2.0 / f32::from(i16::MAX);
        painter.line_segment(
            [
                egui::pos2(x0, mid - f32::from(pair[0]) * scale),
                egui::pos2(x1, mid - f32::from(pair[1]) * scale),
            ],
            stroke,
        );
    }
}
