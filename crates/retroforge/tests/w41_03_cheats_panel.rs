//! **W41-03: the Cheats panel** in the Game settings window. Paste a valid NES Game Genie code and
//! an invalid line: one entry (switched off) and one inline error; switch it on and the badge says
//! "Cheats: 1"; it persists with the game's settings.
//!
//! One test function: `RETROFORGE_CONFIG_DIR` is process-global.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(60) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Whether any file under `dir` holds `needle` (the settings file's exact place is the store's business).
fn any_file_contains(dir: &Path, needle: &str) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            let p = e.path();
            if p.is_dir() {
                any_file_contains(&p, needle)
            } else {
                std::fs::read_to_string(&p).is_ok_and(|t| t.contains(needle))
            }
        })
}

#[test]
fn paste_a_code_and_a_bad_line_switch_it_on_and_the_badge_says_so() {
    let dir = std::env::temp_dir().join(format!("retroforge_w41_03_panel_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let rom = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    let mut h = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run_steps(2);
    h.state_mut().launch_rom(&rom);
    run_frames(&mut h, 10);
    assert!(
        !h.state().status_badge().contains("Cheats"),
        "a fresh game has no cheats: {}",
        h.state().status_badge()
    );

    h.state_mut().open_game_settings_for_test();
    h.run_steps(3);
    h.get_by_label("Cheat codes").click();
    h.run_steps(2);
    h.get_by_label("Cheat codes")
        .type_text("SXIOPO\nnot a code");
    h.run_steps(2);
    // The section sits below the fold of the Game settings window: scroll first, then a real click.
    h.get_by_label("Add codes").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Add codes").click();
    h.run_steps(3);

    let cheats = &h.state().game_settings_for_test().cheats;
    assert_eq!(cheats.len(), 1, "only the valid code is stored: {cheats:?}");
    assert_eq!(cheats[0].code, "SXIOPO");
    assert!(!cheats[0].enabled, "added switched off");
    assert!(
        h.query_by_label_contains("line 2").is_some(),
        "the bad line is explained inline"
    );
    assert!(
        !h.state().status_badge().contains("Cheats"),
        "off is not a cheat"
    );

    h.get_by_label("SXIOPO").focus();
    h.run_steps(2);
    h.key_press(egui::Key::Space);
    h.run_steps(3);
    assert!(h.state().game_settings_for_test().cheats[0].enabled);
    assert!(
        h.state().status_badge().ends_with("Cheats: 1"),
        "badge: {}",
        h.state().status_badge()
    );

    // Persisted per game: the settings file on disk carries it.
    let saved = any_file_contains(&dir, "cheats=1|SXIOPO|");
    assert!(saved, "the enabled cheat is saved with the game's settings");

    h.get_by_label("Remove cheat SXIOPO").focus();
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(h.state().game_settings_for_test().cheats.is_empty());
    assert!(!h.state().status_badge().contains("Cheats"));
    let _ = std::fs::remove_dir_all(&dir);
}
