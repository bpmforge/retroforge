//! **The Quick Menu** (ticket W20-10; `docs/design/UX_WAVE_20.md` §5).
//!
//! Esc opens it over the paused game; every rail section shows its own
//! content; the header carries the honesty badge exactly as the status
//! bar would; Save stores a state from the menu; Quit to library closes
//! the game and the menu together. Real `RetroForgeApp`, real core.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn texts(harness: &Harness<'_, RetroForgeApp>) -> Vec<String> {
    harness
        .root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            a.label().or_else(|| a.value())
        })
        .collect()
}

#[test]
fn every_section_shows_its_content_and_the_header_is_honest() {
    let dir = std::env::temp_dir().join(format!("retroforge_quick_menu_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let rom = dir.join("rf-scroller.nes");
    std::fs::copy(&fixture, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 200.0, WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().launch_rom(&rom);
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 60 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }

    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert_eq!(harness.state().running_and_menu_for_test(), (false, true));

    // The header shows the badge exactly as the status bar computes it.
    let badge = harness.state().status_badge();
    assert!(
        texts(&harness).iter().any(|t| t == &badge),
        "the Quick Menu header must carry the honesty badge {badge:?}"
    );

    // Every section: clicking its rail entry shows its heading.
    for section in Section::ALL {
        harness
            .query_by_label(&section.rail_text())
            .unwrap_or_else(|| panic!("rail entry for {section:?}"))
            .click();
        harness.run_steps(2);
        let all = texts(&harness);
        assert!(
            all.iter().filter(|t| t.as_str() == section.label()).count() >= 1,
            "{section:?} must show its heading; saw {all:?}"
        );
        assert!(
            harness.state().running_and_menu_for_test().1,
            "{section:?} closed the menu"
        );
    }

    // Save from the menu stores a state in Slot 1.
    harness
        .query_by_label(&Section::Save.rail_text())
        .unwrap()
        .click();
    harness.run_steps(2);
    harness.get_by_label("Save Slot 1").click();
    harness.run_steps(3);
    // The Save section re-reads the slot directory itself (the core
    // thread writes the file asynchronously), so the new screenshot shows
    // up in the menu without opening any other window.
    let start = Instant::now();
    while harness.query_all_by_label("Slot 1 screenshot").count() == 0
        && start.elapsed() < Duration::from_secs(10)
    {
        harness.run_steps(2);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        harness.query_all_by_label("Slot 1 screenshot").count() > 0,
        "Save from the menu wrote a slot, and the menu shows it"
    );

    // Quit to library closes the game and the menu together.
    harness
        .query_by_label(&Section::Quit.rail_text())
        .unwrap()
        .click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Quit to library")
        .click();
    harness.run_steps(3);
    assert_eq!(harness.state().running_and_menu_for_test(), (false, false));
    assert!(
        !harness.state().has_presented_frame(),
        "back on the library"
    );
}
