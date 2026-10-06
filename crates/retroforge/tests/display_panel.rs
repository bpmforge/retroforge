//! **Display panel: shader preview tiles and the ambient-glow letterbox**
//! (ticket W20-19; `docs/design/UX_WAVE_20.md` §6).
//!
//! Real GPU (`WgpuTestRenderer`, the `shader_chain_applies.rs` convention;
//! skips without an adapter): the Quick Menu's Display section shows the
//! paused frame through all six shaders, and clicking one selects it. The
//! glow setting makes the play view track the picture's edge colours.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn preview_tiles_pick_a_shader_and_glow_tracks_the_edges() {
    let dir = std::env::temp_dir().join(format!("retroforge_display_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 200.0, WINDOW_SIZE[1] + 200.0))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    if !harness.state().gpu_available_for_test() {
        eprintln!("SKIP: no GPU adapter");
        return;
    }
    harness.state_mut().launch_rom(&fixture);
    harness
        .state_mut()
        .video_settings_mut_for_test()
        .ambient_glow = true;
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 60 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    assert!(
        harness.state().edge_colours_for_test().is_some(),
        "glow on: edge colours tracked"
    );

    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    harness.get_by_label(&Section::Display.rail_text()).click();
    harness.run_steps(3);
    assert_eq!(
        harness.state().shader_previews_for_test(),
        6,
        "one tile per shader"
    );
    harness.get_by_label("Scanlines preview").click();
    harness.run_steps(2);
    assert_eq!(
        harness
            .state_mut()
            .video_settings_mut_for_test()
            .shader
            .as_deref(),
        Some("scanlines"),
        "clicking a tile selects that shader"
    );
}
