//! RetroForge application shell (ticket W1-06): a winit+egui window that
//! drives an `rf-nes` core, blits its indexed frames through
//! `rf-renderer`'s CPU-blit path, and offers pause/step-frame/
//! step-scanline debugger controls (FR-DBG-004) plus a ROM open dialog.
//!
//! Library/binary split exists purely for testability: [`stepper`] and
//! [`core_thread`] have zero `egui`/`eframe`/`winit` dependency and are
//! exercised headlessly by `cargo test --workspace` (no window, no GPU
//! device — see each module's doc for why). Two modules touch
//! `egui`/`eframe` (ticket W1-07 widened this from one): [`app`] is the
//! `eframe::App` implementation itself, and [`input_map`] is the
//! `egui::Key -> rf_input::Key` translation table — `egui::Key` is an
//! egui type, so naming it requires the dependency, but `input_map` is
//! otherwise a plain, headlessly-testable function. `rf-input` itself
//! (the crate `input_map` translates *into*) has no `egui` dependency at
//! all (`scripts/validate-arch.sh` + that crate's own doc). `src/main.rs`'s
//! `fn main` is the only place `eframe::run_native` is ever called — never
//! from a test.
//!
//! Ticket W4-03e adds [`canvas_accum`] (per-scene `Canvas` accumulation,
//! driven on the core thread — see [`core_thread`] for why every frame
//! must reach it) and [`enhanced_view`] (the `SceneGraph` ->
//! `CompositeLayer` mediator, `ARCHITECTURE.md` §3). Neither touches
//! `egui`/`eframe`.
//!
//! Ticket W4-06a adds [`debug_dock`] (the third and last module that
//! touches `egui`/`egui_dock`: `DockState<rf_debugger::layout::DebugTab>`
//! capture/restore + persistence). Its own doc explains why it stays
//! separate from [`app`] rather than folding in: `capture_layout`/
//! `restore_layout` are unit-testable without a window (plain data
//! structures, no rendering), and keeping them in their own module is what
//! makes that testable in practice rather than in principle.

pub mod accessibility;
pub mod annotation_store;
pub mod app;
pub mod app_bindings;
pub mod art;
pub mod audio_out;
pub mod authoring;
pub mod bindings_store;
pub mod canvas_accum;
pub mod core_thread;
pub mod debug_dock;
pub mod enhance_dock;
pub mod enhance_panel;
pub mod enhance_ui;
pub mod enhanced_view;
pub mod game_settings;
pub mod hash;
pub mod icons;
pub mod input_map;
pub mod level_view;
pub mod library;
pub mod library_cache;
pub mod library_roots;
pub mod mod_chunk;
pub mod mode_invariant;
pub mod pacer;
pub mod play_view;
pub mod profile_editor;
pub mod quick_menu;
pub mod recording;
pub mod reveal;
pub mod rom_open;
pub mod save_state;
pub mod script_panel;
pub mod settings;
pub mod shader_select;
pub mod slot_cards;
pub mod snes_save_state;
pub mod state_slots;
pub mod stepper;
pub mod theme;
pub mod thumbnail;
pub mod toast;
pub mod trace_capture;
pub mod ui_nav;
pub mod upscale_studio;

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "retroforge";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "retroforge");
    }
}
