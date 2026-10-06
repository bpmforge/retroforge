//! **A photographed tour of the shipped product** (2026-08-26).
//!
//! Not a test of correctness — `renders.rs` owns the "did it draw
//! anything" assertion. This exists so a claim like "the debugger is
//! daily-drivable" or "the map stitches" can be answered with a picture
//! instead of a paragraph. Every frame lands in `target/ui-tour/`.
//!
//! It is `#[ignore]`d for the reason the other artefact-writing tests in
//! this repo are (`demo_capture.rs`, `unprofiled_scroller_replay.rs`): it
//! writes files and runs a real core for hundreds of frames. Run it
//! deliberately:
//!
//! ```text
//! cargo test -p retroforge --test capture_tour -- --ignored --nocapture
//! ```
//!
//! **The harness gets a real GPU here**, unlike `renders.rs`. Passing a
//! `WgpuTestRenderer` sets `cc.wgpu_render_state`, which is what
//! `RetroForgeApp::new` builds its `EnhancedCompositor` from — so the
//! ultrawide/map path is exercised for real rather than reporting "no GPU
//! device". Without it the most interesting surface in the product
//! photographs as an apology.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn out_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-tour");
    std::fs::create_dir_all(&d).expect("create tour dir");
    d
}

/// Hold a key down for the rest of the tour.
///
/// One press event, no release: egui keeps a key in `keys_down` until it
/// sees the release, and `RetroForgeApp::poll_input` reads exactly that.
/// **The stitcher needs this.** RF-Scroller's player does not move on its
/// own, so an unattended run visits one screen — and the ultrawide
/// camera over a one-screen canvas photographs as an ordinary play view,
/// which is a truthful picture of nothing happening. The map is the
/// product's differentiator; showing it requires actually playing.
fn hold(harness: &mut Harness<'_, RetroForgeApp>, key: egui::Key) {
    harness.input_mut().events.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
}

/// Repaint until the CORE has produced `target` frames, or `deadline`
/// elapses. The sleep is the point: it yields to the core thread, which
/// is where emulation actually happens.
fn run_emulated_frames(harness: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while harness.state().frame_count_for_test() < target && start.elapsed() < deadline {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
    println!(
        "  ran to frame {} in {:.1}s",
        harness.state().frame_count_for_test(),
        start.elapsed().as_secs_f32()
    );
}

fn shot(harness: &mut Harness<'_, RetroForgeApp>, name: &str) {
    match harness.render() {
        Ok(img) => {
            let p = out_dir().join(format!("{name}.png"));
            img.save(&p).expect("save frame");
            println!("SHOT {name} -> {}", p.display());
        }
        Err(e) => println!("SHOT {name} FAILED: {e}"),
    }
}

#[test]
#[ignore = "writes a screenshot tour under target/ui-tour/ and runs a real core; run deliberately"]
fn photograph_every_major_surface() {
    let dir = std::env::temp_dir().join(format!("retroforge_tour_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        // W11-02: without this the tour photographs "no profile" against
        // a ROM that has one — which is how the bug hid.
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
    }

    // A library the home screen can actually show. The fixture is copied
    // rather than pointed at, so the tour never depends on the shape of
    // the repo checkout around it.
    let games = dir.join("games");
    std::fs::create_dir_all(&games).expect("games dir");
    let games_dir = games.clone();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    assert!(
        fixture.exists(),
        "fixture ROM missing at {} — the tour photographs a RUNNING emulator, and without \
         it every frame below would be chrome around an empty rectangle",
        fixture.display()
    );
    let rom = games.join("RF-Scroller.nes");
    std::fs::copy(&fixture, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().set_library_roots_for_test(vec![games]);
    harness.run();
    harness.run();
    shot(&mut harness, "01-library-home");

    // ---- the emulator, actually running -----------------------------
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    // W20-07: Run moved to the Debug viewers; start the core directly.
    harness.state_mut().resume_for_test();
    // **Wall-clock, not repaints.** The first version of this loop called
    // `run_steps` 900 times and photographed a flat grey rectangle with
    // `f3 sl239` in the status bar. `run_steps` repaints the UI; the core
    // runs on its OWN thread in real time, so spinning the UI as fast as
    // possible advances the emulator by almost nothing. The tour was
    // photographing the boot screen and calling it the product.
    //
    // Bounded by a deadline as well as a frame target, because a hang is
    // a denial of service and not a failing test (project law 8).
    // Walk right, so there is a level to stitch and a picture worth
    // taking. Held from here to the end of the tour.
    hold(&mut harness, egui::Key::ArrowRight);
    run_emulated_frames(&mut harness, 300, Duration::from_secs(20));
    assert!(
        harness.state().has_presented_frame(),
        "no frame reached the screen, so every shot below is of an empty player"
    );
    assert!(
        harness.state().frame_count_for_test() >= 120,
        "only {} emulated frames — this is a picture of a boot screen, not of the product",
        harness.state().frame_count_for_test()
    );
    shot(&mut harness, "02-play-view");

    // ---- Wave 20: the Quick Menu over the paused game ----------------
    // Save first (F5) so the Save/Load sections have a screenshot to show.
    harness.key_press(egui::Key::F5);
    // W20-12: the save's OSD card, over the picture — shot at once, since
    // every harness step advances egui's clock by a whole step's dt.
    harness.run_steps(1);
    shot(&mut harness, "17-osd-saved");
    let target = harness.state().frame_count_for_test() + 20;
    run_emulated_frames(&mut harness, target, Duration::from_secs(5));
    // Enhanced, with the sprite-limit bypass on, so the Enhancements
    // panel (W20-18) has a before/after pair and a counted feature.
    harness
        .state_mut()
        .set_mode_for_test(retroforge::game_settings::Mode::Enhanced);
    harness.state_mut().set_sprite_overlay_for_test(true);
    let target = harness.state().frame_count_for_test() + 20;
    run_emulated_frames(&mut harness, target, Duration::from_secs(5));
    harness.key_press(egui::Key::Escape);
    harness.run_steps(4);
    shot(&mut harness, "12-quick-menu");
    for (section, name) in [
        (retroforge::quick_menu::Section::Save, "13-quick-menu-save"),
        (
            retroforge::quick_menu::Section::Display,
            "14-quick-menu-display",
        ),
        (
            retroforge::quick_menu::Section::Enhancements,
            "15-quick-menu-enhancements",
        ),
        (
            retroforge::quick_menu::Section::Controls,
            "16-quick-menu-controls",
        ),
    ] {
        harness.get_by_label(&section.rail_text()).click();
        // The Save section re-reads slots on a 500 ms cadence.
        for _ in 0..8 {
            harness.run_steps(2);
            std::thread::sleep(Duration::from_millis(80));
        }
        shot(&mut harness, name);
    }
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    harness.state_mut().set_sprite_overlay_for_test(false);
    harness
        .state_mut()
        .set_mode_for_test(retroforge::game_settings::Mode::Accuracy);

    // W20-11: in play, with the pointer still, the bars slide away.
    let centre = egui::pos2(WINDOW_SIZE[0] / 2.0, WINDOW_SIZE[1] / 2.0);
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(centre));
    for _ in 0..40 {
        harness.step();
    }
    shot(&mut harness, "18-play-chrome-hidden");

    // W20-15: the optional performance overlay and input display.
    {
        let v = harness.state_mut().video_settings_mut_for_test();
        v.perf_overlay = true;
        v.input_display = true;
    }
    let target = harness.state().frame_count_for_test() + 30;
    run_emulated_frames(&mut harness, target, Duration::from_secs(5));
    shot(&mut harness, "19-perf-and-input-overlay");
    {
        let v = harness.state_mut().video_settings_mut_for_test();
        v.perf_overlay = false;
        v.input_display = false;
    }

    // W20-13: rewind's scrub bar while Backspace is held.
    harness.state_mut().set_rewind_setting_for_test(true);
    let target = harness.state().frame_count_for_test() + 200;
    run_emulated_frames(&mut harness, target, Duration::from_secs(10));
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Backspace,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    for _ in 0..12 {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(10));
    }
    shot(&mut harness, "20-rewind-scrub");
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Backspace,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    harness.run_steps(2);
    harness.state_mut().set_rewind_setting_for_test(false);

    // W20-19: the ambient-glow letterbox, in a window wider than the
    // picture so the side bars show.
    harness
        .state_mut()
        .video_settings_mut_for_test()
        .ambient_glow = true;
    let target = harness.state().frame_count_for_test() + 20;
    run_emulated_frames(&mut harness, target, Duration::from_secs(5));
    shot(&mut harness, "21-ambient-glow");
    harness
        .state_mut()
        .video_settings_mut_for_test()
        .ambient_glow = false;

    // W20-20: back on the library after playing — the Continue hero (with
    // Resume from the save taken above), and the grid's chips and spines.
    harness.state_mut().close_rom_for_test();
    harness
        .state_mut()
        .set_library_roots_for_test(vec![games_dir.clone()]);
    harness
        .state_mut()
        .set_library_view_for_test(retroforge::library::LibraryView::Grid);
    harness.run_steps(4);
    shot(&mut harness, "22-library-continue");
    harness.state_mut().launch_rom(&rom);
    harness.run_steps(2);

    // And back, so the rest of the tour has its menus.
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(centre.x, 8.0)));
    harness.run_steps(4);

    // Ticket W11-04: the shipped Lua overlay, running in the app and
    // drawing over the live frame. FR-PLUG-001 and an MVP checklist item
    // — proven since W4-04 against the LIBRARY, and unreachable in the
    // product until now.
    let plugin =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/examples/player-overlay");
    harness.state_mut().load_script_for_test(&plugin);
    run_emulated_frames(&mut harness, 600, Duration::from_secs(20));
    shot(&mut harness, "10-lua-overlay");

    // ---- the workspaces, over a live session ------------------------
    harness.state_mut().show_enhance_for_test(true);
    harness.run_steps(3);
    shot(&mut harness, "03-enhance-workspace");

    // Ticket W11-02: the decoded level, with the live viewport outline
    // over it. The promise VISION §2 calls "whole levels on one screen",
    // photographed because until now nothing could turn a decoded level
    // into a picture at all.
    harness.state_mut().set_full_level_view_for_test(true);
    run_emulated_frames(&mut harness, 900, Duration::from_secs(30));
    shot(&mut harness, "09-full-level-view");
    harness.state_mut().show_enhance_for_test(false);

    harness.state_mut().show_debug_panels_for_test(true);
    harness.run_steps(3);
    shot(&mut harness, "04-debug-workspace");
    harness.state_mut().show_debug_panels_for_test(false);

    harness.state_mut().show_settings_for_test(true);
    harness.run_steps(3);
    shot(&mut harness, "05-settings");
    harness.state_mut().show_settings_for_test(false);

    harness.state_mut().show_controls_for_test(true);
    harness.run_steps(3);
    shot(&mut harness, "06-controls");
    harness.state_mut().show_controls_for_test(false);

    // ---- the ultrawide camera, over a stitched canvas ---------------
    //
    // Left until last on purpose: the canvas has to be BUILT by playing,
    // and the frames above are what built it. Photographed early it would
    // honestly report an empty canvas, which is true and useless.
    harness.state_mut().set_ultrawide_for_test();
    run_emulated_frames(&mut harness, 1200, Duration::from_secs(30));
    shot(&mut harness, "07-ultrawide-camera");

    // Ticket W11-12: the SNES, in the app. VISION §5's "SNES core boots
    // the plain-LoROM/HiROM commercial mainstream" was true of the CORE
    // and false of the PRODUCT until now.
    let snes_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc");
    if snes_src.exists() {
        let snes = games_dir.join("RF-Scroller-S.sfc");
        std::fs::copy(&snes_src, &snes).expect("copy snes fixture");
        harness.state_mut().open_rom_path(&snes);
        harness.run_steps(2);
        harness.state_mut().resume_for_test();
        // Relative to THIS session: the app's frame readout still holds
        // the NES run's count until a new frame lands, so an absolute
        // target is already satisfied and the shot catches the
        // core-up-no-frame-yet state instead of the game.
        let from = harness.state().frame_count_for_test();
        run_emulated_frames(&mut harness, from + 120, Duration::from_secs(25));
        shot(&mut harness, "11-snes-play-view");
    }

    println!("TOUR COMPLETE -> {}", out_dir().display());
}
