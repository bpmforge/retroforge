//! **W37-03: Rebuild as a per-game option with an honest card.**
//!
//! Through the real app: in Enhanced mode the Enhancements panel offers a
//! Rebuild switch (off by default); turning it on starts the generic tier on the
//! core thread and the card then shows coverage, map size, tier mix and the
//! verification state together; the choice is saved per game; and Original
//! mode neither offers it nor runs it.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use retroforge::quick_menu::Section;
use retroforge::rebuild::RebuildStatus;

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

fn press(harness: &mut Harness<'_, RetroForgeApp>, key: egui::Key) {
    for pressed in [true, false] {
        harness.input_mut().events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.run_steps(1);
    }
}

fn files_containing(dir: &Path, needle: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                files_containing(&p, needle)
            } else {
                usize::from(std::fs::read_to_string(&p).is_ok_and(|t| t.contains(needle)))
            }
        })
        .sum()
}

#[test]
fn the_switch_starts_the_generic_tier_and_the_card_is_honest() {
    let dir = std::env::temp_dir().join(format!("retroforge_w37_03_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 200.0, WINDOW_SIZE[1] + 300.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().launch_rom(&fixture);
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 30 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }

    // Original mode: the row is absent and nothing runs.
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    harness
        .get_by_label(&Section::Enhancements.rail_text())
        .click();
    harness.run_steps(3);
    assert_eq!(
        harness
            .query_all_by_role_and_label(egui::accesskit::Role::CheckBox, "Rebuild")
            .count(),
        0,
        "no Rebuild switch in Original mode"
    );
    assert!(matches!(
        harness.state().rebuild_status_for_test(),
        RebuildStatus::Off
    ));

    // Enhanced: offered, off by default, no card yet.
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    harness.run_steps(3);
    let all = texts(&harness);
    assert!(!all.iter().any(|t| t.starts_with("Tier mix")), "{all:?}");
    let switch = harness.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Rebuild");
    switch.scroll_to_me();
    harness.run_steps(3);
    harness
        .get_by_role_and_label(egui::accesskit::Role::CheckBox, "Rebuild")
        .click();
    // The menu pauses the game, and the map is installed at a frame boundary: resume to let it.
    harness.run_steps(2);
    press(&mut harness, egui::Key::Escape);
    let start = Instant::now();
    while !matches!(
        harness.state().rebuild_status_for_test(),
        RebuildStatus::On(_)
    ) && start.elapsed() < Duration::from_secs(60)
    {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(8));
    }
    harness.run_steps(3);
    assert!(
        matches!(
            harness.state().rebuild_status_for_test(),
            RebuildStatus::On(_)
        ),
        "{:?}",
        harness.state().rebuild_status_for_test()
    );

    // Open the menu again: the card shows what the core last reported.
    press(&mut harness, egui::Key::Escape);
    harness.run_steps(3);
    harness
        .get_by_label(&Section::Enhancements.rail_text())
        .click();
    harness.run_steps(3);
    // The card: coverage, map, tier mix, verification, together.
    let all = texts(&harness);
    let has = |p: &str| all.iter().any(|t| t.starts_with(p));
    assert!(has("Coverage: "), "{all:?}");
    assert!(has("Map: "), "{all:?}");
    assert!(has("Tier mix: "), "{all:?}");
    assert!(has("Verification: "), "{all:?}");
    assert!(
        !all.iter().any(|t| t == "Proven"),
        "a game nobody has checked is not proven: {all:?}"
    );

    // Saved per game, and the badge counts it.
    assert!(
        files_containing(&dir, "rebuild=true") >= 1,
        "the choice is written with the game's settings"
    );
    assert!(harness.state().status_badge().contains('\u{e2de}'));

    // Back to Original: the row goes, the core is asked to stop, the choice stays saved.
    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    let start = Instant::now();
    while !matches!(
        harness.state().rebuild_status_for_test(),
        RebuildStatus::Off
    ) && start.elapsed() < Duration::from_secs(10)
    {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(8));
    }
    assert!(matches!(
        harness.state().rebuild_status_for_test(),
        RebuildStatus::Off
    ));
    assert!(files_containing(&dir, "rebuild=true") >= 1);
}
