//! **Watchpoints fire on a running game, and promote to an annotation**
//! (ticket W13-02e; `docs/design/DEBUGGER.md` §1, bar B-4/B-5).
//!
//! `Condition::MemoryEquals` tested what memory *held* at an instruction
//! boundary. A watchpoint tests what the machine *did*, at the cycle it did
//! it — which only the bus can see. So the evaluation lives in the core
//! (`rf_core_api::WatchTable`) and the hit comes back as
//! `CoreEvent::MemWatch`, a variant that has been in the contract since
//! W4-00 with **no producer**. This test is the proof it has one.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use rf_core_api::{MemWatch, WatchAccess, WatchSpace};

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn an_armed_watchpoint_reports_hits_and_promotes_to_an_annotation() {
    let dir = std::env::temp_dir().join(format!("retroforge_watch_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let nes = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    assert!(nes.exists(), "NES fixture missing: {}", nes.display());
    let rom: PathBuf = dir.join("rf-scroller.nes");
    std::fs::copy(&nes, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();

    // **$2002 is PPUSTATUS**, which every NES program polls in its vblank
    // wait — so a read watch on it is guaranteed to fire on any running
    // game, without this test needing to know anything about the fixture's
    // own RAM map. Picking a game-specific address would make a failure
    // ambiguous between "watchpoints are broken" and "the game does not
    // touch that byte".
    {
        let panel = harness.state_mut().debug_annotations_mut();
        panel.watches.push(MemWatch::unconditional(
            1,
            WatchSpace::Cpu,
            0x2002,
            WatchAccess::Read,
        ));
        panel.next_watch_id = 2;
        panel.request = Some(retroforge::debug_dock::AnnotationRequest::InstallWatches);
    }
    // The panel asks for a subscription as soon as anything is armed, even
    // with the debug window closed — a watch is armed until it is disarmed.
    harness.run_steps(2);

    let target = harness.state().frame_count_for_test() + 20;
    run_frames(&mut harness, target, Duration::from_secs(60));

    let hits = harness
        .state_mut()
        .debug_annotations_mut()
        .watch_hits
        .get(&1)
        .copied()
        .unwrap_or(0);
    assert!(
        hits > 0,
        "a read watch on PPUSTATUS must fire on a running game; got {hits} hits"
    );

    // **Promotion** (bar B-5): one action fills the annotation form with
    // what the watch knows, and leaves what it cannot know blank.
    {
        let panel = harness.state_mut().debug_annotations_mut();
        let watch = panel.watches[0];
        let promoted = rf_debugger::annotation::promote_watch(
            watch,
            "ppu_status",
            "u8",
            "https://www.nesdev.org/wiki/PPU_registers",
        )
        .expect("a CPU-space watch promotes");
        assert_eq!(promoted.addr, 0x2002);
        assert_eq!(promoted.len, 1);
        panel.store.add(promoted).expect("and the store accepts it");
    }

    // Disarming stops the cost: nothing armed, nothing subscribed.
    {
        let panel = harness.state_mut().debug_annotations_mut();
        panel.watches.clear();
        panel.request = Some(retroforge::debug_dock::AnnotationRequest::InstallWatches);
        assert!(!panel.has_watches());
    }
    harness.run_steps(2);

    let _ = std::fs::remove_dir_all(&dir);
}
