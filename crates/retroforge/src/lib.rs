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
//! `egui`/`eframe` — the "two modules touch egui" count above stays
//! exactly two.

pub mod app;
pub mod canvas_accum;
pub mod core_thread;
pub mod enhanced_view;
pub mod hash;
pub mod input_map;
pub mod mode_invariant;
pub mod pacer;
pub mod rom_open;
pub mod stepper;

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "retroforge";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "retroforge");
    }
}
