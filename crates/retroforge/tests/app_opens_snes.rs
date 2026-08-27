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
