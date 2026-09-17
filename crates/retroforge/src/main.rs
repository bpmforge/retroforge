//! RetroForge application shell entry point (ticket W1-06). The only place
//! [`eframe::run_native`] is ever called — never from a test, since a
//! winit window cannot open headlessly in CI (see `crates::app`'s module
//! doc and this ticket's commit body for the manual verification
//! checklist that stands in for automated coverage here).
use eframe::egui;

fn main() -> eframe::Result {
    // Ticket W14-20: an env-filtered logger, installed before anything
    // else runs, so `RUST_LOG=eframe=trace,egui_winit=debug` can be
    // captured on the next UI-freeze stall without a rebuild — this is
    // exactly the trace that would have shown WHY the main thread sat in
    // AppKit's `_DPSBlockUntilNextEventMatchingListInMode` on 2026-09-17
    // (see plan.json W14-20's notes). `env_logger::init()` reads RUST_LOG
    // and installs the global `log` logger; with no RUST_LOG set it
    // stays silent (its default filter is `Off` — `env_logger::Builder::
    // default`'s own doc), so a normal launch prints nothing new.
    env_logger::init();
    // Ticket W10-01: the size comes from `app::WINDOW_SIZE`, not a literal
    // here. `tests/hud_fits.rs` asserts nothing is clipped at exactly this
    // size, and a test with its own copy of the number would keep passing
    // after someone changed the window — which is how the bar came to
    // need 1539 px inside a 768 px window without a single red test.
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(retroforge::app::WINDOW_SIZE)
            .with_min_inner_size(retroforge::app::MIN_WINDOW_SIZE),
        ..Default::default()
    };
    eframe::run_native(
        "RetroForge",
        native_options,
        Box::new(|cc| Ok(Box::new(retroforge::app::RetroForgeApp::new(cc)))),
    )
}
