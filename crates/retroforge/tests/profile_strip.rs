//! **The profile strip** (ticket W22-02; `docs/design/UX_WAVE_22.md`).
//! Quick Menu › Enhancements says, in plain words, whether a profile
//! matched this game and what that means. RF-Scroller ships a profile;
//! a copy with a changed byte does not match it.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn enhancements_texts(rom: &Path) -> Vec<String> {
    let mut h = Harness::builder()
        .with_size(egui::vec2(980.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().launch_rom(rom);
    let start = Instant::now();
    while h.state().frame_count_for_test() < 10 && start.elapsed() < Duration::from_secs(20) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.get_by_label(&Section::Enhancements.rail_text()).click();
    h.run_steps(3);
    h.get_by_label("What's a profile?").click();
    h.run_steps(3);
    h.root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            a.label().or_else(|| a.value())
        })
        .collect()
}

#[test]
fn the_strip_says_whether_a_profile_matched() {
    let dir = std::env::temp_dir().join(format!("retroforge_profile_strip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let matched = enhancements_texts(&fixture);
    assert!(
        matched.iter().any(|t| t.starts_with("Profile found: ")),
        "the fixture's own profile matches: {matched:?}"
    );
    assert!(
        matched.iter().any(|t| t.contains("dump")),
        "the explainer opens"
    );

    // The same game with one PRG byte changed: a different "dump".
    let mut bytes = std::fs::read(&fixture).expect("fixture");
    bytes[16 + 0x100] ^= 0xFF;
    let other = dir.join("rf-scroller-other-dump.nes");
    std::fs::write(&other, &bytes).expect("write");
    let unmatched = enhancements_texts(&other);
    assert!(
        unmatched
            .iter()
            .any(|t| t == "No profile matches this copy of the game"),
        "{unmatched:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
