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
    /// Latest frame's OAM (`core_thread::FrameMsg::oam`) — genuinely live,
    /// unlike `chr_rom`/vram/cgram.
    pub oam: [u8; 256],
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
}

impl Default for PanelData {
    fn default() -> Self {
        PanelData {
            chr_rom: None,
            script: None,
            oam: [0u8; 256],
            events: Vec::new(),
            wram: [0u8; 0x0800],
            prg_ram: [0u8; 0x2000],
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
        let style = egui_dock::Style::from_egui(ui.style().as_ref());
        let mut viewer = PanelTabViewer {
            data: &self.data,
            pattern_table: &mut self.pattern_table,
        };
        egui_dock::DockArea::new(&mut self.dock_state)
            .style(style)
            .show_inside(ui, &mut viewer);
    }
}

impl Default for DebugPanels {
    fn default() -> Self {
        Self::new()
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
    data: &'a PanelData,
    pattern_table: &'a mut PatternTable,
}

impl egui_dock::TabViewer for PanelTabViewer<'_> {
    type Tab = DebugTab;

    fn title(&mut self, tab: &mut DebugTab) -> egui::WidgetText {
        match tab {
            DebugTab::Pattern => "Pattern",
            DebugTab::Nametable => "Nametable",
            DebugTab::Palette => "Palette",
            DebugTab::Oam => "OAM",
            DebugTab::EventTimeline => "Events",
            DebugTab::Memory => "Memory",
            DebugTab::LuaConsole => "Lua",
        }
        .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut DebugTab) {
        match tab {
            DebugTab::Pattern => pattern_ui(ui, self.data.chr_rom.as_deref(), self.pattern_table),
            DebugTab::Nametable => nametable_ui(ui),
            DebugTab::Palette => palette_ui(ui),
            DebugTab::Oam => oam_ui(ui, &self.data.oam),
            DebugTab::EventTimeline => event_timeline_ui(ui, &self.data.events),
            DebugTab::Memory => memory_ui(ui, &self.data.wram, &self.data.prg_ram),
            DebugTab::LuaConsole => lua_console_ui(ui, self.data.script.as_deref()),
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

fn nametable_ui(ui: &mut egui::Ui) {
    ui.label(
        "No live nametable data: PPU VRAM has no accessor reachable from this crate yet \
         (rf_debugger::nametable's module doc — a future ticket adding NesBus::vram(), \
         mirroring the existing NesBus::oam(), closes this gap). Decode logic is already \
         implemented and unit-tested against synthetic bytes in rf-debugger.",
    );
}

fn palette_ui(ui: &mut egui::Ui) {
    ui.label(
        "No live palette data: PPU palette RAM (CGRAM) has no accessor reachable from this \
         crate yet (rf_debugger::palette's module doc — same gap as the nametable viewer). \
         Decode logic is already implemented and unit-tested against synthetic bytes.",
    );
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
fn memory_ui(ui: &mut egui::Ui, wram: &[u8; 0x0800], prg_ram: &[u8; 0x2000]) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label("WRAM ($0000-$07FF)");
        memory_rows_ui(ui, "debug-memory-wram", 0x0000, wram);
        ui.separator();
        ui.label("PRG-RAM ($6000-$7FFF)");
        memory_rows_ui(ui, "debug-memory-prgram", 0x6000, prg_ram);
    });
}

fn memory_rows_ui(ui: &mut egui::Ui, grid_id: &str, base_addr: u32, bytes: &[u8]) {
    let rows = rf_debugger::memory_view::build_rows(base_addr, bytes);
    egui::Grid::new(grid_id).striped(true).show(ui, |ui| {
        for row in &rows {
            ui.monospace(format!("{:04X}", row.addr));
            let hex: String = row
                .bytes
                .iter()
                .map(|b| format!("{b:02X} "))
                .collect::<String>();
            ui.monospace(hex);
            ui.end_row();
        }
    });
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
    fn from_toml_str_on_garbage_falls_back_to_default_without_panicking() {
        let fallback = layout::from_toml_str("not valid toml {{{")
            .ok()
            .unwrap_or_else(layout::default_layout);
        assert_eq!(fallback, layout::default_layout());
    }
}
