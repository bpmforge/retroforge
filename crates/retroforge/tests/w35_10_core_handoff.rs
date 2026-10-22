//! **Opening a ROM stops the previous core thread first** (ticket W35-10).
//!
//! `open_rom_path` used to overwrite `self.core`, dropping the old handle
//! without a `Shutdown`: the old thread kept running (its command loop does
//! not notice a hung-up sender) holding an in-memory `rf-cache` index, and a
//! late `put` from it persisted an index without the scene the new thread
//! had cached. Now the old thread is told to stop and joined (its final
//! flush included) before the new one is spawned.
//!
//! One test function: `RETROFORGE_CONFIG_DIR` is process-global.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::core_thread::CoreCommand;

const SNES: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";
const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

#[test]
fn opening_a_second_rom_ends_the_first_core_thread() {
    let dir = std::env::temp_dir().join(format!("retroforge_w35_10_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let nes = root.join(NES);
    let snes = root.join(SNES);
    assert!(nes.exists() && snes.exists(), "fixtures not built");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();

    harness.state_mut().open_rom_path(&nes);
    harness.run_steps(2);
    let first = harness
        .state()
        .core_cmd_sender_for_test()
        .expect("first core running");
    assert!(first.send(CoreCommand::Pause).is_ok(), "first core alive");

    harness.state_mut().open_rom_path(&snes);
    // No waiting: the join happened inside open_rom_path. A send fails only
    // when the old thread's receiver is gone, i.e. the thread has exited.
    assert!(
        first.send(CoreCommand::Pause).is_err(),
        "the first core thread must have exited before the second opens"
    );
    let second = harness
        .state()
        .core_cmd_sender_for_test()
        .expect("second core running");
    assert!(second.send(CoreCommand::Pause).is_ok(), "second core alive");
    let took = harness.state().last_core_join_for_test();
    assert!(
        took < Duration::from_secs(2),
        "ROM switch stalled: {took:?}"
    );
    eprintln!("w35-10: core join took {took:?}");

    // Dropping the app (exit) also ends the thread.
    drop(harness);
    let start = Instant::now();
    assert!(
        second.send(CoreCommand::Pause).is_err(),
        "exit left a core thread"
    );
    assert!(start.elapsed() < Duration::from_secs(1));
}
