//! RetroForge application shell entry point (ticket W1-06). The only place
//! [`eframe::run_native`] is ever called — never from a test, since a
//! winit window cannot open headlessly in CI (see `crates::app`'s module
//! doc and this ticket's commit body for the manual verification
//! checklist that stands in for automated coverage here).
use eframe::egui;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([768.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "RetroForge",
        native_options,
        Box::new(|cc| Ok(Box::new(retroforge::app::RetroForgeApp::new(cc)))),
    )
}
