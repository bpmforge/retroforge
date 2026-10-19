//! **Layer extraction follows the widened frame** (ticket W34-07).
//!
//! `CoreCommand::SetWidescreen` grew the frame buffer but not the debug
//! layer extraction beside it, so with widescreen on and the layers wanted
//! (Diorama asks for them) the core handed the UI a `sprite_rgba` /
//! `bg_rgba` of the old 256-wide shape next to a wider `rgba`. The UI's
//! layer textures asserted on the size and took the app down (and the layers
//! held the first 256 dots of each widened line, the left margin, not the
//! picture). This runs a SNES game with both on.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use rf_enhance::widescreen::Aspect;

const SNES: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn diorama_and_widescreen_together_run_a_snes_game_without_the_layers_disagreeing() {
    let dir = std::env::temp_dir().join(format!("retroforge_w34_07_layers_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var("TMPDIR", &dir);
    }
    let snes = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES);
    assert!(snes.exists(), "SNES fixture missing: {}", snes.display());
    let rom = dir.join("rf-scroller-s.sfc");
    std::fs::copy(&snes, &rom).expect("copy fixture");

    let mut h = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().open_rom_path(&rom);
    h.run_steps(2);
    h.state_mut().resume_for_test();
    run_frames(&mut h, 20, Duration::from_secs(30));
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    // The layer upload asserts on a size mismatch: surviving the frames IS
    // the check.
    let from = h.state().frame_count_for_test();
    run_frames(&mut h, from + 60, Duration::from_secs(60));
    assert!(
        h.state().frame_count_for_test() >= from + 60,
        "the game did not run on"
    );
    assert!(h.state().has_presented_frame());
}
