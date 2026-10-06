//! **Hold Backspace to rewind** (ticket W20-13; `docs/design/UX_WAVE_20.md`
//! §5).
//!
//! Off by default (it costs memory): holding the key then just says so.
//! On: holding pauses the game, walks back through the history (the frame
//! counter goes DOWN), and releasing resumes. The exact-restoration
//! property is W20-22's open defect in the NES save state — see
//! `core_thread`'s `#[ignore]`d known-red tests — so it is not asserted
//! here.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn backspace(harness: &mut Harness<'_, RetroForgeApp>, held: bool) {
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Backspace,
        physical_key: None,
        pressed: held,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
}

#[test]
fn rewind_is_off_by_default_and_when_on_walks_back_and_resumes() {
    let dir = std::env::temp_dir().join(format!("retroforge_rewind_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().launch_rom(&fixture);
    let run = |harness: &mut Harness<'_, RetroForgeApp>, n: u64| {
        let target = harness.state().frame_count_for_test() + n;
        let start = Instant::now();
        while harness.state().frame_count_for_test() < target
            && start.elapsed() < Duration::from_secs(30)
        {
            harness.run_steps(1);
            std::thread::sleep(Duration::from_millis(4));
        }
    };
    run(&mut harness, 30);

    // Off: holding Backspace explains, and nothing pauses.
    backspace(&mut harness, true);
    harness.run_steps(1);
    assert!(
        harness
            .state()
            .osd_texts_for_test()
            .iter()
            .any(|t| t.starts_with("Rewind is off")),
        "{:?}",
        harness.state().osd_texts_for_test()
    );
    assert!(
        harness.state().running_and_menu_for_test().0,
        "off: the game keeps running"
    );
    backspace(&mut harness, false);
    harness.run_steps(1);

    // On: build some history.
    harness.state_mut().set_rewind_setting_for_test(true);
    run(&mut harness, 200);
    let (_, status) = harness.state().rewind_for_test();
    let status = status.expect("the ring reports its extent every frame");
    assert!(status.len > 5, "{status:?}");
    let before = harness.state().frame_count_for_test();

    // Hold: paused, and the frame counter walks back.
    backspace(&mut harness, true);
    for _ in 0..20 {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(harness.state().rewind_for_test().0, "rewinding while held");
    assert!(
        !harness.state().running_and_menu_for_test().0,
        "paused while rewinding"
    );
    let during = harness.state().frame_count_for_test();
    assert!(during + 20 <= before, "went back: {before} -> {during}");

    // Release: resumes.
    backspace(&mut harness, false);
    harness.run_steps(2);
    assert!(!harness.state().rewind_for_test().0);
    assert!(
        harness.state().running_and_menu_for_test().0,
        "resumed on release"
    );
}
