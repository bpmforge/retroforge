//! **W38-11 (D-032): presentation margins, learned from play.** The core thread reads the recipe's
//! rows without changing the picture, and the app offers the option, labelled, for a recipe game.
//! Runs on the repo's own NES fixture (no ROM); the ROM-gated renders of the four games are in
//! `crates/rf-harness/tests/w38_11_margins.rs`.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::core_thread::{self, CoreCommand, CoreEvent};
use retroforge::game_settings::Mode;
use retroforge::presentation::Spec;
use retroforge::quick_menu::Section;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn recipe_spec() -> Spec {
    let r = rf_profiles::load_recipe_str(
        "[recipe]\nformat = 1\nid = \"t\"\nengine = \"t\"\n\
         [[ram]]\naddr = 0x10\nwidth = 1\nname = \"scroll_x\"\ngrade = \"observed\"\nsource = \"t\"\n\
         [[ram]]\naddr = 0x11\nwidth = 1\nname = \"scroll_page\"\ngrade = \"observed\"\nsource = \"t\"\n\
         [[object_table]]\nname = \"o\"\nbase = 0x200\nslots = 8\nstride = 1\nx_lo = 0x10\ny = 0x20\nstate = 0\ngrade = \"observed\"\nsource = \"t\"\n",
    )
    .expect("recipe");
    Spec::from_recipe(&r).expect("spec")
}

/// Frames' pictures with the reading `on` or off; the picture must not differ.
fn run(on: bool) -> (Vec<Vec<u8>>, Vec<bool>) {
    let rom = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)).expect("fixture");
    let core = core_thread::spawn(rom).expect("spawns");
    if on {
        core.cmd_tx
            .send(CoreCommand::SetPresentation(Some(Box::new(recipe_spec()))))
            .expect("accepts");
    }
    let (mut pics, mut got) = (Vec::new(), Vec::new());
    while pics.len() < 8 {
        core.cmd_tx.send(CoreCommand::StepFrame).expect("accepts");
        match core
            .evt_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a frame")
        {
            CoreEvent::Frame(msg) => {
                core.pending_frames
                    .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                got.push(msg.presentation.as_ref().is_some_and(|p| {
                    p.sprite.len() == msg.width * msg.height
                        && p.facts.objects.as_ref().map(Vec::len) == Some(8)
                }));
                pics.push(msg.rgba.clone());
            }
            CoreEvent::Crashed(r) => panic!("crashed: {}", r.message),
            _ => {}
        }
    }
    let _ = core.cmd_tx.send(CoreCommand::Shutdown);
    let _ = core.join_handle.join();
    (pics, got)
}

#[test]
fn reading_the_recipe_never_changes_the_picture() {
    let (off, got_off) = run(false);
    let (on, got_on) = run(true);
    assert!(got_off.iter().all(|g| !g), "nothing is read while off");
    assert!(
        got_on.iter().skip(1).all(|g| *g),
        "readings ride on every later frame"
    );
    assert_eq!(off, on, "byte-identical pictures with the reading on");
}

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
fn the_option_is_selectable_for_a_recipe_game_and_says_where_it_comes_from() {
    let dir = std::env::temp_dir().join(format!("retroforge_w38_11_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary that touches the environment.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 200.0, WINDOW_SIZE[1] + 600.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run_steps(2);
    harness.state_mut().launch_rom(&fixture);
    let start = Instant::now();
    while harness.state().frame_count_for_test() < 30 && start.elapsed() < Duration::from_secs(30) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    harness
        .get_by_label(&Section::Enhancements.rail_text())
        .click();
    harness.run_steps(3);

    let label = format!("Margins: {}", rf_enhance::presentation_margins::LABEL);
    assert_eq!(label, "Margins: learned from play");
    // No recipe: the label is there, the option is not offered.
    assert!(
        texts(&harness).iter().any(|t| t == &label),
        "{:?}",
        texts(&harness)
    );
    assert!(!harness.state().presentation_active());

    // A recipe: selectable, and the blurb repeats the origin.
    harness
        .state_mut()
        .set_presentation_spec_for_test(Some(recipe_spec()));
    harness.run_steps(3);
    harness
        .get_by_role_and_label(egui::accesskit::Role::CheckBox, &label)
        .scroll_to_me();
    harness.run_steps(3);
    harness
        .get_by_role_and_label(egui::accesskit::Role::CheckBox, &label)
        .click();
    harness.run_steps(8);
    assert!(harness.state().presentation_active());
    assert!(
        texts(&harness)
            .iter()
            .any(|t| t.contains("level and objects, learned from play")),
        "{:?}",
        texts(&harness)
    );
}
