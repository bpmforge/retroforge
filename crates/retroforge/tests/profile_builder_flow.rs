//! **Make a profile, then find an address in play** (tickets W24-02..04;
//! design https://claude.ai/artifact/6a89VhM41WY7Ryqaw5pt7k). A copy of
//! the RF-Scroller fixture with one changed byte has no profile; the
//! player makes one, the builder opens, and the finder narrows the work
//! RAM by "It went up" until a few candidates are left — the fixture's
//! frame counters climb every frame — and "Use this" writes the camera.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn frames(h: &mut Harness<'_, RetroForgeApp>, n: u64) {
    let target = h.state().frame_count_for_test() + n;
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(20) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn make_a_profile_then_find_and_use_an_address() {
    let dir = std::env::temp_dir().join(format!("retroforge_builder_{}", std::process::id()));
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
    let mut bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)).expect("fixture");
    // The last CHR byte: a different dump that still runs the same code.
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    let rom = dir.join("rf-other.nes");
    std::fs::write(&rom, &bytes).expect("write");

    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().launch_rom(&rom);
    frames(&mut h, 20);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.get_by_label(&Section::Enhancements.rail_text()).click();
    h.run_steps(3);
    h.get_by_label("Make a profile for this game").click();
    h.run_steps(2);
    // Back to playing: the builder sits beside the running game.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(
        h.state().running_and_menu_for_test().0,
        "the game runs with the builder open"
    );

    h.get_by_label("Take the first look").click();
    h.run_steps(1);
    frames(&mut h, 3);
    assert_eq!(
        h.state().finder_remaining_for_test(),
        Some(0x800),
        "every NES WRAM offset"
    );
    for _ in 0..6 {
        if h.state()
            .finder_remaining_for_test()
            .is_some_and(|n| n <= 12)
        {
            break; // "Use this" is showing.
        }
        frames(&mut h, 5);
        h.get_by_label("It went up").click();
        h.run_steps(1);
        frames(&mut h, 3);
    }
    let left = h.state().finder_remaining_for_test().expect("a finder");
    assert!(left > 0 && left <= 12, "narrowed to a few: {left}");
    h.query_all_by_label("Use this")
        .next()
        .expect("a candidate")
        .click();
    h.run_steps(2);
    let text = std::fs::read_to_string(dir.join("retroforge/profiles/nes/rf-other/profile.toml"))
        .expect("the profile");
    assert!(text.contains("[camera"), "camera written: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}
