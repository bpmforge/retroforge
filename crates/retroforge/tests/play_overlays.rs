//! **Performance overlay and input display** (ticket W20-15;
//! `docs/design/UX_WAVE_20.md` §5).
//!
//! Both are off by default; turned on in Settings › Video they appear over
//! the picture, and the input display shows exactly the buttons sent to
//! the core this frame.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::NodeT as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

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
fn overlays_are_off_by_default_and_show_when_turned_on() {
    let dir = std::env::temp_dir().join(format!("retroforge_overlays_{}", std::process::id()));
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
    run(&mut harness, 40);
    let before = texts(&harness);
    assert!(
        !before.iter().any(|t| t.contains(" ms")),
        "off by default: {before:?}"
    );

    {
        let v = harness.state_mut().video_settings_mut_for_test();
        v.perf_overlay = true;
        v.input_display = true;
    }
    // Hold Right so the input display has something lit.
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    run(&mut harness, 30);
    let after = texts(&harness);
    assert!(
        after.iter().any(|t| t.ends_with(" fps")),
        "FPS readout: {after:?}"
    );
    assert!(
        after.iter().any(|t| t.ends_with(" ms")),
        "frame-time readout: {after:?}"
    );
    assert!(
        after.iter().any(|t| t.starts_with("audio")),
        "audio readout: {after:?}"
    );
    for name in ["A", "B", "Select", "Start", "Up", "Down", "Left", "Right"] {
        assert!(
            after.iter().any(|t| t == name),
            "input display button {name}: {after:?}"
        );
    }
    assert!(
        harness.state().frame_times_for_test() > 10,
        "the sparkline has history"
    );
}
