//! Ticket W16-02: the Upscale Studio must still capture tiles when it was
//! opened BEFORE any ROM was loaded.
//!
//! **Own binary, own test function** — same `RETROFORGE_CONFIG_DIR`
//! process-global reasoning `context_menu_and_game_settings.rs` and
//! `upscale_studio_ui.rs` both give, and kept separate from
//! `upscale_studio_ui.rs` specifically because that scenario seeds its
//! own captures directly and must not also have a live core feeding real
//! ones into the same session.
//!
//! `set_upscale_studio_open` sends `CoreCommand::SetStudioCapture`
//! through `Self::send_command`, which silently no-ops when no core
//! exists yet (`send_command`'s own doc) — opening the window with
//! nothing running is exactly that case. If `open_rom_path` did not
//! re-assert studio capture on the freshly spawned core the way it
//! already does for `SetLayerExtraction`/`SetSpriteOverlay`, this
//! scenario would sit at "0 tile(s) captured" for the entire session.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

/// Repaint until the CORE has produced `target` frames, or `deadline`
/// elapses — same shape and reasoning as `capture_tour.rs`'s helper of
/// the same name: the core runs on its OWN thread in real time, so
/// spinning `run_steps` as fast as possible advances the emulator by
/// almost nothing. The sleep is the point: it yields to that thread.
fn run_emulated_frames(harness: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while harness.state().frame_count_for_test() < target && start.elapsed() < deadline {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// A ROM that turns rendering on and otherwise loops — same shape as
/// `context_menu_and_game_settings.rs`'s fixture, which is enough to
/// make the PPU draw a real (if trivial) nametable every frame.
fn write_fixture_rom(dir: &Path, name: &str) -> PathBuf {
    let mut data = vec![0u8; 16 + 0x4000 + 0x2000];
    data[0..4].copy_from_slice(b"NES\x1a");
    data[4] = 1;
    data[5] = 1;
    let prg = &mut data[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x1E, 0x8D, 0x01, 0x20, // LDA #$1E ; STA $2001 (rendering on)
        0xEE, 0x00, 0x00, // INC $0000
        0x4C, 0x05, 0x80, // JMP $8005
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    let path = dir.join(name);
    std::fs::write(&path, &data).expect("write fixture rom");
    path
}

fn app() -> Harness<'static, RetroForgeApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();
    harness
}

#[test]
fn opening_the_studio_before_a_rom_still_captures_once_one_loads() {
    let config_dir = std::env::temp_dir().join(format!(
        "retroforge_upscale_studio_capture_arms_config_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&config_dir);
    std::fs::create_dir_all(&config_dir).expect("create scratch config dir");
    // SAFETY: first statement of the only test in this binary, before any
    // app, core thread or harness exists — no concurrent reader. Same
    // isolated-config-dir reasoning as the sibling scenarios: an empty
    // library means no real ROM-folder scan to wait out.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &config_dir);
    }

    let roms_dir = config_dir.join("roms");
    std::fs::create_dir_all(&roms_dir).expect("create roms dir");
    let rom_path = write_fixture_rom(&roms_dir, "Fixture.nes");

    let mut harness = app();

    // ---- open the studio FIRST, before any ROM — the order that used
    // to leave capture permanently off for the whole session -----------
    assert!(!harness.state_mut().show_upscale_studio_for_test());
    harness
        .query_by_label("Enhance")
        .expect("Enhance menu")
        .click();
    harness.run_steps(2);
    harness
        .query_by_label("Upscale Studio\u{2026}")
        .expect("Enhance menu's Upscale Studio… command")
        .click();
    harness.run_steps(2);
    assert!(harness.state_mut().show_upscale_studio_for_test());
    assert_eq!(
        harness.state_mut().upscale_studio_capture_len_for_test(),
        0,
        "nothing has run yet, so nothing should be captured yet"
    );

    // ---- THEN load a ROM — a fresh core thread that starts with
    // studio capture off unless `open_rom_path` re-asserts it -----------
    harness.state_mut().open_rom_path(&rom_path);
    harness.run_steps(2);
    harness.get_by_label("Run").click();
    run_emulated_frames(&mut harness, 20, Duration::from_secs(20));
    assert!(
        harness.state().has_presented_frame(),
        "no frame reached the screen, so capture never had anything to see"
    );

    assert!(
        harness.state_mut().upscale_studio_capture_len_for_test() > 0,
        "the studio, opened before the ROM, must still capture tiles once one is running"
    );

    std::fs::remove_dir_all(&config_dir).ok();
}
