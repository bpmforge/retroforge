//! **"Apply to" for video settings** (ticket W21-04;
//! `docs/design/UX_WAVE_21.md` §4): a change made for "This game" applies
//! to that game only, survives closing and reopening it, and leaves the
//! library's (everything's) settings alone. Real app, real core, driven
//! through the Quick Menu's Display section.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::quick_menu::Section;
use retroforge::settings::ScaleMode;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn open(harness: &mut Harness<'_, RetroForgeApp>, rom: &Path) {
    harness.state_mut().launch_rom(rom);
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 10 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn this_game_settings_stay_with_the_game() {
    let dir = std::env::temp_dir().join(format!("retroforge_video_scope_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let rom = dir.join("rf-scroller.nes");
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE), &rom).expect("fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 200.0, WINDOW_SIZE[1] + 200.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    open(&mut harness, &rom);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    harness.get_by_label(&Section::Display.rail_text()).click();
    harness.run_steps(2);
    harness.get_by_label("This game").click();
    harness.run_steps(2);
    harness.get_by_label("Fit").click();
    harness.run_steps(2);
    assert_eq!(
        harness.state_mut().video_settings_mut_for_test().scale_mode,
        ScaleMode::Fit
    );

    // Back on the library: everything's setting, untouched.
    harness.state_mut().close_rom_for_test();
    harness.run_steps(2);
    assert_eq!(
        harness.state_mut().video_settings_mut_for_test().scale_mode,
        ScaleMode::Integer,
        "a This-game change must not leak into the global settings"
    );

    // The game again: its own setting is back.
    open(&mut harness, &rom);
    assert_eq!(
        harness.state_mut().video_settings_mut_for_test().scale_mode,
        ScaleMode::Fit,
        "the game's own setting is restored when it is reopened"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
