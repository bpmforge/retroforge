//! **The play view frames a widescreen picture at its own aspect, at every
//! aspect, and the Diorama fills that rect** (ticket W34-09, D2).
//!
//! The core's frame is capped at the PPU's 512 dots, so at 21:9 and 32:9 its
//! width is not the picture's (and 512 dots is what the shell folds as hires):
//! sizing the play rect from it showed the picture at 4:3, squeezed. The rect
//! now follows the widened picture. With Diorama on (and a GPU), the 3D image
//! is made at that rect's aspect, so toggling does not jump or letterbox.
//! One test per binary: `set_var` is process-global (`widescreen_aspects.rs`).

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use rf_enhance::widescreen::Aspect;

const SNES: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";
/// The Tv pixel shape (`play_view::pixel_aspect_ratio`).
const PAR: f32 = 8.0 / 7.0;

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn the_play_rect_and_the_diorama_follow_the_widened_pictures_aspect_at_every_aspect() {
    let dir = std::env::temp_dir().join(format!("retroforge_w34_09_wide_{}", std::process::id()));
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
        .with_size(egui::vec2(WINDOW_SIZE[0] * 2.0, WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().open_rom_path(&rom);
    h.run_steps(2);
    h.state_mut().resume_for_test();
    run_frames(&mut h, 20, Duration::from_secs(30));
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_diorama_for_test(true);

    for aspect in [Aspect::Wide16x9, Aspect::Wide21x9, Aspect::Wide32x9] {
        h.state_mut().set_widescreen_aspect_for_test(aspect);
        h.state_mut().set_widescreen(true);
        let from = h.state().frame_count_for_test();
        run_frames(&mut h, from + 40, Duration::from_secs(60));
        let (w, rows) = h.state().frame_size_for_test().expect("a frame");
        let want = (w as f32 * PAR) / rows as f32;
        let (rect, _) = h.state().play_rect_for_test().expect("a play rect");
        let got = rect.width() / rect.height();
        assert!(
            (got - want).abs() / want < 0.02,
            "{aspect:?}: the play rect is {got:.2}:1, the {w}x{rows} picture is {want:.2}:1"
        );
        // The 3D image, when the Diorama runs here, is the rect's shape.
        if let Some((iw, ih)) = h.state().diorama_size_for_test() {
            let image = iw as f32 / ih as f32;
            assert!(
                (image - got).abs() / got < 0.02,
                "{aspect:?}: the Diorama image is {image:.2}:1, the play rect {got:.2}:1"
            );
        }
        h.state_mut().set_widescreen(false);
        let from = h.state().frame_count_for_test();
        run_frames(&mut h, from + 10, Duration::from_secs(30));
    }
}
