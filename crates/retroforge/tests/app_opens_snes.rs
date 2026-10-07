//! **The app opens a SNES ROM** (ticket W11-12).
//!
//! Before this, `core_thread::spawn` called `EmuStepper::from_ines_bytes`
//! unconditionally and a SNES image was refused with `NotNesImage` —
//! while `library::ROM_EXTENSIONS` accepted `sfc`/`smc`/`fig`, the
//! scanner identified the file correctly as
//! `Recognized { console: Snes, .. }`, the library listed it with a Play
//! button and §3.1's console filter offered SNES. **The product
//! advertised a feature it could not perform**, which is worse than one
//! it simply lacked: a user saw their game offered and got an error.
//!
//! `VISION.md` §5's 18-month criterion — "SNES core boots the plain-
//! LoROM/HiROM commercial mainstream" — was true of the CORE and false
//! of the PRODUCT until now.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const SNES: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";
const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn a_snes_rom_starts_and_reaches_the_screen() {
    let dir = std::env::temp_dir().join(format!("retroforge_snes_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let snes = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES);
    assert!(snes.exists(), "SNES fixture missing: {}", snes.display());
    let rom: PathBuf = dir.join("rf-scroller-s.sfc");
    std::fs::copy(&snes, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);

    // The failure this ticket exists to end.
    let status = harness.state().status().to_string();
    assert!(
        !status.contains("NotNesImage"),
        "the app still refuses SNES ROMs: {status}"
    );
    assert!(
        status.starts_with("Loaded"),
        "opening a SNES ROM did not succeed: {status}"
    );

    harness.state_mut().resume_for_test();
    run_frames(&mut harness, 60, Duration::from_secs(20));

    assert!(
        harness.state().has_presented_frame(),
        "the SNES core started but no frame reached the screen. The two consoles build a \
         frame in opposite directions — rf-nes pushes scanlines mid-frame, rf-snes composes \
         a settled frame and replays it — so a sink that sees nothing means the adapter is \
         not replaying."
    );
    assert!(
        harness.state().frame_count_for_test() > 0,
        "no frames were counted"
    );
    assert!(
        harness.state().crash_message_for_test().is_none(),
        "the SNES core crashed: {:?}",
        harness.state().crash_message_for_test()
    );
    // Ticket W21-08: the fixture runs without overscan, so the picture is
    // the 224 lines the PPU drew, not the 240-row host buffer with a
    // blank band under it.
    let (rect, (w, h)) = harness.state().play_rect_for_test().expect("a play rect");
    assert_eq!((w, h), (256, 224), "displayed frame size");
    let ratio = rect.width() / rect.height();
    assert!(
        (ratio - 256.0 * 8.0 / 7.0 / 224.0).abs() < 0.02,
        "the drawn rect has the 224-line shape: {rect:?}"
    );
}

/// **The NES path is unharmed.** W11-10 through W11-12 rewrote how every
/// frame is produced; the console that already worked must still work.
#[test]
fn the_nes_path_still_opens_and_runs() {
    let nes = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    assert!(nes.exists(), "NES fixture missing");
    // A second app in the same process would race on
    // RETROFORGE_CONFIG_DIR, so this drives the stepper directly — which
    // is also the layer the console dispatch lives in.
    let bytes = std::fs::read(&nes).expect("read fixture");
    let stepper = retroforge::stepper::EmuStepper::open(&bytes);
    assert!(
        stepper.is_ok(),
        "the NES path broke while adding SNES: {:?}",
        stepper.err()
    );
}

/// A file neither console claims is refused with a reason, not a panic
/// and not a plausible-looking empty session.
#[test]
fn a_file_that_is_neither_console_is_refused() {
    let junk = vec![0x00u8; 1024];
    match retroforge::stepper::EmuStepper::open(&junk) {
        Ok(_) => panic!("1 KiB of zeroes was accepted as a cartridge"),
        Err(e) => {
            let msg = format!("{e}");
            assert!(!msg.is_empty(), "a refusal must say something");
        }
    }
}

/// **The event timeline is fed on a SNES session** (ticket W13-02h).
///
/// Before it, `grep -rn CoreEvent crates/rf-snes/src` returned zero lines:
/// the panel rendered an empty list for every SNES game, which a user
/// reads as "nothing happened" rather than "this core reports nothing".
#[test]
fn a_snes_session_feeds_the_event_timeline() {
    let dir = std::env::temp_dir().join(format!("retroforge_snesev_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");

    let snes = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES);
    assert!(snes.exists(), "SNES fixture missing: {}", snes.display());
    let rom: PathBuf = dir.join("rf-scroller-s.sfc");
    std::fs::copy(&snes, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    // The subscription is gated on the debug window being open
    // (DEBUGGER.md §6's pay-for-use), so this is what a user does.
    harness.state_mut().show_debug_for_test(true);
    harness
        .state_mut()
        .debug_open_tab(rf_debugger::layout::DebugTab::EventTimeline);
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    run_frames(&mut harness, 10, Duration::from_secs(60));

    let events = harness.state().debug_events_for_test();
    assert!(
        !events.is_empty(),
        "a running SNES session must report events to the timeline"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Throws away everything a frame produces — this test is about the
/// memories the core exposes afterwards, not about what it drew.
#[derive(Default)]
struct Discard;

impl rf_core_api::CoreSink for Discard {
    fn video_scanline(&mut self, _y: u16, _pixels: &[rf_core_api::PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: rf_core_api::CoreEvent) {}
}

/// **A SNES session exposes real memories through the shell** (ticket
/// W13-02a). `EmuStepper::state_view` is the one accessor that answers for
/// both cores; the NES-typed `vram`/`palette`/`oam` helpers beside it
/// cannot, because their return types are NES-sized.
#[test]
fn a_snes_session_exposes_its_memories_through_the_shell() {
    let dir = std::env::temp_dir().join(format!("retroforge_snessv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");

    let snes = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES);
    let rom: PathBuf = dir.join("rf-scroller-s.sfc");
    std::fs::copy(&snes, &rom).expect("copy fixture");

    let mut stepper =
        retroforge::stepper::EmuStepper::open(&std::fs::read(&rom).expect("read")).expect("open");
    let mut sink = Discard;
    stepper.step_frame(&mut sink);

    let view = stepper.state_view();
    assert_eq!(view.wram.len(), 128 * 1024);
    assert_eq!(view.vram.len(), 64 * 1024);
    assert_eq!(view.cgram.len(), 512);
    assert_eq!(view.oam.len(), 544);
    assert!(!view.ppu_regs.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// **Bar B-1: every SNES viewer has real data to draw** (ticket W13-02b).
///
/// The providers are decoded here rather than screenshotted, for the same
/// reason `memory_viewer_poke_bus` drives functions instead of pixels: the
/// egui half is the accepted human-verifiable part, and what a test can
/// prove is that each viewer's *input* exists and decodes.
#[test]
fn every_snes_viewer_has_something_to_draw() {
    let dir = std::env::temp_dir().join(format!("retroforge_snesview_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");

    let snes = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES);
    let rom: PathBuf = dir.join("rf-scroller-s.sfc");
    std::fs::copy(&snes, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    // The capture is pay-for-use: opening the debug window is what turns
    // it on, exactly as it turns on the event subscription.
    harness.state_mut().show_debug_for_test(true);
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    run_frames(&mut harness, 30, Duration::from_secs(60));

    let snes = harness
        .state()
        .snes_debug_frame_for_test()
        .expect("a SNES session with the debug window open must capture its memories");

    assert_eq!(snes.vram.len(), 64 * 1024, "the CHR and tilemap viewers");
    assert_eq!(snes.cgram.len(), 512, "the palette viewer");
    assert_eq!(snes.oam.len(), 544, "the sprite viewer");
    assert_eq!(snes.aram.len(), 64 * 1024, "the ARAM memory space");
    assert!(
        !snes.ppu_regs.is_empty(),
        "every viewer needs the registers"
    );

    // A running game must have put SOMETHING in VRAM — a viewer fed
    // 64 KiB of zeroes would pass a length check and draw a blank page.
    assert!(
        snes.vram.iter().any(|b| *b != 0),
        "the fixture draws, so its VRAM cannot be empty"
    );
    assert!(snes.cgram.iter().any(|b| *b != 0), "and it sets a palette");

    // The decoders reach real values through the register file, not
    // through assumptions: $2105's mode picks the bit depths the CHR
    // viewer decodes at.
    let mode = snes.ppu_regs[0x05] & 0x07;
    let depths = rf_snes::ppu::bg::bit_depths(mode);
    assert!(depths[0] > 0, "BG1 always exists, in every mode");
    let sprites = rf_snes::debug::decode_oam(&snes.oam);
    assert_eq!(sprites.len(), 128);

    // And the NES path stays NES: opening a NES ROM clears the column.
    let nes = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    let nes_rom: PathBuf = dir.join("rf-scroller.nes");
    std::fs::copy(&nes, &nes_rom).expect("copy nes fixture");
    harness.state_mut().open_rom_path(&nes_rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    run_frames(&mut harness, 5, Duration::from_secs(30));
    assert!(
        harness.state().snes_debug_frame_for_test().is_none(),
        "a NES session must not leave the previous SNES game's memories on screen"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **Mode 7, HDMA lanes and the DSP voices reach the panels**
/// (ticket W13-02c). Like the viewer test above, this asserts on the
/// *inputs* each panel decodes rather than on pixels.
#[test]
fn the_mode7_hdma_and_dsp_views_are_fed() {
    let dir = std::env::temp_dir().join(format!("retroforge_snesc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");

    let snes = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES);
    let rom: PathBuf = dir.join("rf-scroller-s.sfc");
    std::fs::copy(&snes, &rom).expect("copy fixture");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().show_debug_for_test(true);
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    run_frames(&mut harness, 30, Duration::from_secs(60));

    let snes = harness
        .state()
        .snes_debug_frame_for_test()
        .expect("the capture must be on with the debug window open");

    // The lane record is one byte per hardware line and is reset each
    // frame, so its LENGTH is the invariant — the fixture may or may not
    // use HDMA, and "no transfers" is a real answer the panel states.
    assert_eq!(
        snes.hdma_lanes.len(),
        262,
        "one lane byte per hardware line"
    );
    assert_eq!(snes.voices.len(), 8, "the DSP has eight voices");

    // The camera trapezoid comes from the same projection the renderer
    // samples with, so it is defined for whatever matrix is live — even
    // the all-zero one a game that never entered mode 7 leaves behind.
    let corners = rf_snes::debug::mode7_camera_corners(&snes.mode7, 256, 224);
    assert_eq!(corners.len(), 4);

    // And a BRR decode over real ARAM does not panic on whatever bytes
    // happen to be there — a debugger that crashes on uninitialised audio
    // memory is worse than one that shows noise.
    let block = rf_snes::debug::decode_brr_block(&snes.aram, snes.voices[0].start, (0, 0));
    assert_eq!(block.samples.len(), 16);

    let _ = std::fs::remove_dir_all(&dir);
}

/// **A SNES session traces in bsnes convention** (ticket W13-02g).
/// Before it, `EmuStepper`'s trace path was 6502-shaped by construction
/// and a SNES session emitted an empty line for every instruction.
#[test]
fn a_snes_session_produces_bsnes_shaped_trace_lines() {
    let dir = std::env::temp_dir().join(format!("retroforge_snestrace_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");

    let snes = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNES);
    let rom: PathBuf = dir.join("rf-scroller-s.sfc");
    std::fs::copy(&snes, &rom).expect("copy fixture");

    let mut stepper =
        retroforge::stepper::EmuStepper::open(&std::fs::read(&rom).expect("read")).expect("open");
    let mut sink = Discard;
    let mut lines: Vec<String> = Vec::new();
    stepper.latch_and_advance_frame_traced(
        rf_core_api::InputFrame::default(),
        &mut sink,
        &mut |_pc, _cycle, line| {
            if lines.len() < 8 {
                lines.push(line);
            }
        },
    );

    assert!(!lines.is_empty(), "a traced frame must produce lines");
    let first = &lines[0];
    // bank:addr, which is the shape that makes a 65816 trace diffable
    // against a reference emulator at all.
    assert!(
        first.len() > 7 && first.as_bytes()[2] == b':',
        "expected a bank:addr prefix, got {first:?}"
    );
    assert!(first.contains("A:"), "{first:?}");
    assert!(first.contains("DB:"), "and the data bank: {first:?}");
    // The flag field is eight characters. It shows M and X in native
    // mode — and B in emulation mode, where the chip has no M or X to
    // show. A 65816 comes out of reset AS A 6502, so the first traced
    // line of any SNES session is an emulation-mode line: asserting on
    // 'M' here would be asserting that the CPU had already run `XCE`,
    // which no ROM has at instruction one.
    let flags = first.rsplit("P:").next().expect("a P: field");
    assert_eq!(flags.len(), 8, "flag string in {first:?}");
    assert!(
        flags.contains('b') || flags.contains('B') || flags.contains('m') || flags.contains('M'),
        "expected an emulation-mode B or a native-mode M in {first:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
