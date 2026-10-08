//! **Game info on screen** (tickets W27-04/05; design
//! https://claude.ai/artifact/6NGLfLgWmt8FGfvNsutDLB). RF-Scroller's
//! profile lists `player_x`; pinning it in the Quick Menu's Game info puts
//! a chip over the game whose value climbs as the player walks right.

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
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(30) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn value(h: &Harness<'_, RetroForgeApp>) -> i64 {
    let chips = h.state().game_info_chips_for_test();
    let chip = chips.first().expect("a chip");
    chip.rsplit(' ')
        .next()
        .and_then(|v| v.parse().ok())
        .expect("a number")
}

#[test]
fn a_pinned_item_shows_and_follows_the_game() {
    let dir = std::env::temp_dir().join(format!("retroforge_gameinfo_{}", std::process::id()));
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
    let rom = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().launch_rom(&rom);
    frames(&mut h, 20);
    assert!(
        h.state().game_info_chips_for_test().is_empty(),
        "nothing pinned yet"
    );

    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.get_by_label(&Section::GameInfo.rail_text()).click();
    h.run_steps(3);
    h.get_by_label("Player X").click();
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    frames(&mut h, 5);
    let before = value(&h);

    h.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    frames(&mut h, 120);
    let after = value(&h);
    assert!(
        after > before,
        "player X climbs while walking right: {before} -> {after}"
    );

    // The pin is this game's, and it is saved.
    let saved: String = std::fs::read_dir(dir.join("retroforge"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten().flatten())
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect();
    assert!(saved.contains("pinned_items"), "saved: {saved}");
    let _ = std::fs::remove_dir_all(&dir);
}
