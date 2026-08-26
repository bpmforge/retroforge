//! **The Map tab is fed without flipping an unrelated toggle**
//! (ticket W10-05).
//!
//! W10-02 shipped a Map tab that read "Nothing stitched yet" while the
//! canvas demonstrably existed: `maybe_request_canvas_snapshot` returned
//! early unless `camera == Ultrawide`, so a tab called Map did nothing
//! until the user found a View-menu entry nobody had told them about.
//! Found by photographing the workspace at frame 308 of a live session
//! against a canvas the same tour rendered in full at frame 1200 — no
//! test in the repo could see it, because the widget tree is identical
//! either way.
//!
//! This asserts the feeding, not the drawing: that opening the workspace
//! is enough to make a stitched canvas exist.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn opening_the_enhance_workspace_is_enough_to_feed_the_map() {
    let dir = std::env::temp_dir().join(format!("retroforge_maptab_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());
    let rom: PathBuf = dir.join("rf-scroller.nes");
    std::fs::copy(&fixture, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        // A real GPU: the stitched map is composited on it, and without
        // one this test would pass by way of "no GPU device" rather than
        // by way of the fix.
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });

    // Play a while WITHOUT opening the workspace and WITHOUT touching the
    // camera. Nothing should be stitched: law 6 says an enhancement costs
    // nothing until asked for, and `current_canvas` clones the whole
    // canvas per snapshot.
    run_frames(&mut harness, 300, Duration::from_secs(20));
    assert!(
        !harness.state().has_stitched_map_for_test(),
        "a canvas was composited before anyone asked to see one — the snapshot request is \
         no longer gated at all, which trades this bug for a more expensive one"
    );

    // Open the workspace. That alone must be enough.
    harness.state_mut().show_enhance_for_test(true);
    run_frames(&mut harness, 900, Duration::from_secs(30));
    harness.run_steps(3);

    assert!(
        harness.state().has_stitched_map_for_test(),
        "opening the Enhance workspace did not feed the Map tab. It is still waiting on the \
         View-menu camera toggle, which is the bug W10-05 exists to fix."
    );
}
