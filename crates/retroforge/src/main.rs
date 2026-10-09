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
    // Ticket W20-04: reopen at the size the window was last left at
    // (`settings::WindowSettings::startup_size` clamps it to at least
    // `MIN_WINDOW_SIZE`, and falls back to `WINDOW_SIZE`).
    let startup_size = retroforge::bindings_store::config_root()
        .map_or(retroforge::app::WINDOW_SIZE, |root| {
            retroforge::settings::load(&root).0.window.startup_size()
        });
    let mut native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(startup_size)
            .with_min_inner_size(retroforge::app::MIN_WINDOW_SIZE),
        ..Default::default()
    };
    let chosen = apply_graphics_backend(&mut native_options);
    let fallback_options = native_options.clone();
    let result = eframe::run_native(
        "RetroForge",
        native_options,
        Box::new(|cc| Ok(Box::new(retroforge::app::RetroForgeApp::new(cc)))),
    );
    // Ticket W29-01: a backend this machine cannot start (Vulkan with no
    // Vulkan driver) must not lock the player out — the only other way
    // back would be editing the settings file. Start again on Auto.
    match result {
        Err(e) if chosen => {
            log::warn!("graphics backend failed to start ({e}); retrying with Auto");
            let mut options = fallback_options;
            options.wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
            eframe::run_native(
                "RetroForge",
                options,
                Box::new(|cc| Ok(Box::new(retroforge::app::RetroForgeApp::new(cc)))),
            )
        }
        other => other,
    }
}

/// Ticket W29-01: honor the saved graphics backend. `WGPU_BACKEND` in the
/// environment wins, and a saved choice this OS cannot offer is ignored.
/// Returns whether a backend other than Auto was applied.
fn apply_graphics_backend(options: &mut eframe::NativeOptions) -> bool {
    use retroforge::settings::GraphicsBackend;
    if std::env::var_os("WGPU_BACKEND").is_some() {
        return false;
    }
    let choice = retroforge::bindings_store::config_root().map_or(GraphicsBackend::Auto, |root| {
        retroforge::settings::load(&root).0.video.graphics_backend
    });
    if !GraphicsBackend::available().contains(&choice) {
        return false;
    }
    let Some(backends) = choice.wgpu_backends() else {
        return false;
    };
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = backends;
    options.wgpu_options.wgpu_setup = eframe::egui_wgpu::WgpuSetup::CreateNew(setup);
    true
}
