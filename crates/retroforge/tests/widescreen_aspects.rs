//! **Widescreen aspects up to 32:9 reach the app** (ticket W30-03).
//!
//! Drives `RetroForgeApp`, not the library: picks 32:9, turns widescreen
//! on, and checks the frame the app draws is the aspect's width - wider
//! than the 512 dots the PPU path can draw - with the margins beyond the
//! VRAM-drawn width black and the VRAM-drawn width equal to the core's own
//! output when no world source is in use. One test per binary: the
//! `set_var` race note in `widescreen_reaches_the_app.rs`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WIDESCREEN_WIDTH, WINDOW_SIZE};
use rf_enhance::widescreen::{Aspect, MarginSource};

const SNES: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// One test, not two, and the reason is in the `unsafe` block below:
/// `set_var` is process-global, so two tests in one binary racing to set
/// `RETROFORGE_CONFIG_DIR` is a data race that shows up as one of them
/// mysteriously never receiving a frame. The existing SNES app test has
/// the same "only test in this binary" note for the same reason.
fn boot() -> (Harness<'static, RetroForgeApp>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("retroforge_wsa_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        // Ticket W20-09: the reopen check goes through the real gate,
        // which needs the fixture's profile to MATCH — and the default
        // `profiles/` is relative to the process cwd (the crate dir).
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
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
    // The app boots PAUSED; without this no frame is ever produced and
    // every assertion below fails for a reason that has nothing to do
    // with widescreen.
    harness.state_mut().resume_for_test();
    run_frames(&mut harness, 30, Duration::from_secs(30));
    (harness, rom)
}

#[test]
fn thirty_two_by_nine_widens_past_the_ppu_and_vram_margins_are_exact() {
    // The 16:9 default is the long-standing constant.
    assert_eq!(Aspect::Wide16x9.output_width(224, 1.0), WIDESCREEN_WIDTH);

    let (mut harness, _rom) = boot();
    harness.state_mut().force_vram_margins_for_test(true);
    harness
        .state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    harness.state_mut().set_widescreen(true);
    let target = harness.state().frame_count_for_test() + 10;
    run_frames(&mut harness, target, Duration::from_secs(30));

    let (w, h) = harness
        .state()
        .frame_size_for_test()
        .expect("frames arrive");
    let want = Aspect::Wide32x9.output_width(h, 1.0);
    assert_eq!(w, want, "32:9 at {h} lines");
    assert!(w > 512, "wider than the PPU path can draw, got {w}");

    let rgba = harness.state().last_frame_rgba_for_test().expect("a frame");
    assert_eq!(rgba.len(), w * h * 4);
    let core = harness
        .state()
        .core_frame_for_test()
        .expect("the core frame is kept while VRAM-only")
        .to_vec();
    let native_w = core.len() / (h * 4);
    assert_eq!(native_w, 512, "the core is capped at the PPU's maximum");
    assert_eq!(
        harness.state().widescreen_source_for_test(),
        MarginSource::Vram
    );

    let pad = (w - native_w) / 2;
    for y in 0..h {
        for x in 0..w {
            let px = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
            if x < pad || x >= pad + native_w {
                assert_eq!(px, [0, 0, 0, 255], "beyond the VRAM width at ({x},{y})");
            } else {
                let c = &core[(y * native_w + (x - pad)) * 4..][..4];
                assert_eq!(&px[..3], &c[..3], "within the VRAM width at ({x},{y})");
            }
        }
    }

    // Narrowing the aspect re-asks the core: 16:9 is the old constant again.
    harness
        .state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide16x9);
    let target = harness.state().frame_count_for_test() + 10;
    run_frames(&mut harness, target, Duration::from_secs(30));
    assert_eq!(
        harness.state().frame_size_for_test().map(|s| s.0),
        Some(WIDESCREEN_WIDTH)
    );

    // With world sources allowed again, the stitched canvas of the scene
    // takes over the margins once a snapshot of THIS scene has landed.
    harness.state_mut().force_vram_margins_for_test(false);
    harness
        .state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    // The fixture scrolls when the D-pad moves the player.
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    let start = Instant::now();
    while harness.state().widescreen_source_for_test() != MarginSource::StitchedCanvas
        && start.elapsed() < Duration::from_secs(60)
    {
        let target = harness.state().frame_count_for_test() + 10;
        run_frames(&mut harness, target, Duration::from_secs(10));
    }
    assert_eq!(
        harness.state().widescreen_source_for_test(),
        MarginSource::StitchedCanvas,
        "a scrolling fixture stitches a canvas and the margins use it"
    );
    let (w2, h2) = harness.state().frame_size_for_test().unwrap();
    assert_eq!((w2, h2), (w, h));
}
