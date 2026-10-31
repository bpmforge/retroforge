//! **W42-01: one overlay stack.** Esc pops exactly one layer: a framed player window if one is
//! open, else the Quick Menu. Settings opened from the Quick Menu draws over it and returns to it.
//!
//! One test function: `RETROFORGE_CONFIG_DIR` is process-global.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::quick_menu::Section;

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(60) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Esc as a press + release pair, a frame each.
fn esc(h: &mut Harness<'_, RetroForgeApp>) {
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        h.run_steps(2);
    }
}

#[test]
fn esc_pops_one_layer_at_a_time() {
    let dir = std::env::temp_dir().join(format!("retroforge_w42_01_{}", std::process::id()));
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

    // (a) Quick Menu > Settings > Open Settings...: the sheet opens OVER the menu.
    h.state_mut().open_quick_menu_for_test(Section::Settings);
    h.run_steps(3);
    assert!(h.state().quick_menu_open_for_test(), "self-test: menu open");
    h.get_by_label("Open Settings\u{2026}").click();
    h.run_steps(3);
    assert!(h.state().settings_open_for_test(), "the sheet is open");
    assert!(h.state().quick_menu_open_for_test(), "the menu stays open");
    assert!(
        !h.state().running_and_menu_for_test().0,
        "game stays paused"
    );
    // The sheet is the topmost layer where it is drawn.
    let probe = egui::pos2(WINDOW_SIZE[0] - 40.0, WINDOW_SIZE[1] * 0.5);
    let top = h.ctx.layer_id_at(probe).map(|l| l.id);
    assert_eq!(
        top,
        Some(egui::Id::new("settings-sheet")),
        "sheet above menu"
    );

    esc(&mut h);
    assert!(!h.state().settings_open_for_test(), "Esc closed the sheet");
    assert!(h.state().quick_menu_open_for_test(), "back on the menu");
    assert_eq!(h.state().quick_section_for_test(), Section::Settings);
    assert!(!h.state().running_and_menu_for_test().0, "still paused");

    esc(&mut h);
    assert!(
        !h.state().quick_menu_open_for_test(),
        "second Esc closes the menu"
    );
    assert!(h.state().running_and_menu_for_test().0, "game resumed");

    // Settings with no menu behind it: Esc closes the sheet only.
    h.state_mut().show_settings_for_test(true);
    h.run_steps(3);
    esc(&mut h);
    assert!(!h.state().settings_open_for_test());
    assert!(
        !h.state().quick_menu_open_for_test(),
        "Esc did not open the menu"
    );

    // (b) Game settings window: Esc closes it and does not open the menu.
    h.state_mut().open_game_settings_for_test();
    h.run_steps(3);
    assert!(h.state().show_game_settings_for_test(), "self-test");
    esc(&mut h);
    assert!(!h.state().show_game_settings_for_test(), "window closed");
    assert!(!h.state().quick_menu_open_for_test(), "menu not opened");
    esc(&mut h);
    assert!(
        h.state().quick_menu_open_for_test(),
        "next Esc opens the menu"
    );
    esc(&mut h);
    assert!(!h.state().quick_menu_open_for_test());

    // (c) W42-04: Quick Menu > Controls > "All controls...": Settings opens on its
    // Controls tab OVER the menu; Esc returns to the menu.
    h.state_mut().open_quick_menu_for_test(Section::Controls);
    h.run_steps(3);
    h.get_by_label("All controls (player 2, shortcuts)\u{2026}")
        .click();
    h.run_steps(3);
    assert!(h.state().settings_open_for_test(), "Settings opened");
    assert!(
        h.state().settings_on_controls_for_test(),
        "on the Controls tab"
    );
    assert!(h.state().quick_menu_open_for_test(), "the menu stays up");
    assert!(
        !h.state().running_and_menu_for_test().0,
        "game stays paused"
    );
    esc(&mut h);
    assert!(!h.state().settings_open_for_test(), "Esc closed the sheet");
    assert!(h.state().quick_menu_open_for_test(), "back on the menu");
    assert_eq!(h.state().quick_section_for_test(), Section::Controls);
    esc(&mut h);
    assert!(!h.state().quick_menu_open_for_test());
    let _ = std::fs::remove_dir_all(&dir);
}
