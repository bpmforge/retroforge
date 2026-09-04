//! **The memory view is an editor, not a dump** (ticket W13-02d;
//! `docs/design/DEBUGGER.md` §3's memory-hex row, bar B-3).
//!
//! W13-01's grading found `memory_view` was `build_rows` + `byte_at` and
//! nothing else — no goto, no find, no live edit, no annotation colouring;
//! the "find" box in `debug_dock` was the *trace* filter. This drives
//! `RetroForgeApp` and asserts the edit reaches the running machine.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn an_edit_committed_while_paused_reaches_the_machine_and_one_sent_running_does_not() {
    let dir = std::env::temp_dir().join(format!("retroforge_memedit_{}", std::process::id()));
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
    run_frames(&mut harness, 20, Duration::from_secs(60));

    // **An edit while RUNNING is refused, not queued.** A poke that landed
    // at whatever cycle the command queue drained on would make the same
    // session unreproducible, which the whole project rests on not being.
    {
        let mem = harness.state_mut().debug_memory_mut();
        mem.poke = Some((0x0010, 0xAB));
    }
    harness.run_steps(2);
    let status = harness
        .state_mut()
        .debug_memory_mut()
        .status
        .clone()
        .unwrap_or_default();
    assert!(
        status.contains("running"),
        "a running core must refuse the write and say so, got {status:?}"
    );

    // Paused, the same edit lands.
    harness.state_mut().pause_for_test();
    harness.run_steps(2);
    {
        let mem = harness.state_mut().debug_memory_mut();
        assert!(mem.paused, "the panel must know the core is paused");
        mem.poke = Some((0x0010, 0xAB));
        mem.status = None;
    }
    // A paused core sends no frames, so the panel's snapshot only
    // refreshes when the session advances — step one scanline, the
    // smallest advance a paused debugger makes.
    harness.state_mut().step_scanline_for_test();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = 0u8;
    while Instant::now() < deadline {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
        seen = harness.state().wram_for_test()[0x0010];
        if seen == 0xAB {
            break;
        }
    }
    assert_eq!(
        seen, 0xAB,
        "the byte the editor committed must be in the machine"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
