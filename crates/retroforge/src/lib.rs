//! RetroForge application shell (ticket W1-06): a winit+egui window that
//! drives an `rf-nes` core, blits its indexed frames through
//! `rf-renderer`'s CPU-blit path, and offers pause/step-frame/
//! step-scanline debugger controls (FR-DBG-004) plus a ROM open dialog.
//!
//! Library/binary split exists purely for testability: [`stepper`] and
//! [`core_thread`] have zero `egui`/`eframe`/`winit` dependency and are
//! exercised headlessly by `cargo test --workspace` (no window, no GPU
//! device — see each module's doc for why). [`app`] is the only module
//! that touches `eframe`/`egui`, and `src/main.rs`'s `fn main` is the only
//! place `eframe::run_native` is ever called — never from a test.

pub mod app;
pub mod core_thread;
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
