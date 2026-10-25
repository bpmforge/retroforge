//! **W36-07: three consecutive presented frames of Super Mario Bros. 1-1 at 120 Hz presentation** (ROM-
//! gated, law 5: only the rendered PNGs are committed).
//!
//! The real app runs 1-1 with Right and B held, "Smoother motion" on, and the Presenter's alpha pinned to
//! the middle of each gap (so one intermediate per console frame, the 120 Hz case). Every picture the
//! Presenter presents is logged; the three saved are console frame N, the intermediate between N and N+1,
//! and console frame N+1. The same run in Accuracy Mode must present nothing and send nothing.
//!
//! ```text
//! cargo test --release -p retroforge --test w36_07_screens -- --ignored --nocapture
//! ```
//!
//! One test per binary: `set_var` is process-global.

mod w36_06b_common;

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::core_thread::CoreCommand;
use retroforge::frame_interp::Shown;
use retroforge::game_settings::Mode;
use retroforge::stepper::EmuStepper;
use w36_06b_common::*;

fn run_n(h: &mut Harness<'_, RetroForgeApp>, n: usize) {
    for _ in 0..n {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn run_for(h: &mut Harness<'_, RetroForgeApp>, secs: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn save(path: &Path, w: usize, h: usize, rgba: &[u8]) {
    let mut opaque = rgba.to_vec();
    for px in opaque.chunks_exact_mut(4) {
        px[3] = 255;
    }
    image::save_buffer(path, &opaque, w as u32, h as u32, image::ColorType::Rgba8).expect("png");
    println!("SHOT {}", path.display());
}

fn open(path: &Path) -> Harness<'static, RetroForgeApp> {
    let mut h = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().open_rom_path(path);
    run_n(&mut h, 3);
    h
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR) and a GPU"]
fn three_presented_frames_at_120_hz() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let dir =
        std::env::temp_dir().join(format!("retroforge_w36_07_screens_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let profiles = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var("TMPDIR", &dir);
        std::env::set_var("RETROFORGE_PROFILES_DIR", &profiles);
    }
    let bytes = retroforge::rom_open::load_rom_bytes(&path).expect("the zip opens");
    let base = match rf_cart::Cartridge::load(&bytes).expect("loads") {
        rf_cart::Cartridge::Nes { identity, .. } => identity.normalized.sha256,
        rf_cart::Cartridge::Snes { .. } => unreachable!("a NES image"),
    };

    // The scene: 1-1 just after the title, from the same image the app runs.
    let raw = load_rom(&path);
    let mut st = EmuStepper::open(&raw).expect("opens");
    let mut sink = rf_renderer::FrameBuffer::new();
    for f in 0..520u32 {
        st.latch_and_advance_frame(input(script_two(f)), &mut sink);
    }
    let scene = st.save_state_with_mods(0, None).expect("saves");

    // --- Accuracy Mode: nothing presented, nothing sent. ---
    let mut settings = retroforge::game_settings::GameSettings::default();
    settings.mode = Mode::Accuracy;
    settings.frame_interpolation = true; // even a file that says so
    retroforge::game_settings::save(&dir, &base, &settings).expect("settings");
    {
        let mut h = open(&path);
        h.state_mut().log_presented_for_test();
        h.state_mut().resume_for_test();
        h.state()
            .send_core_command_for_test(CoreCommand::ApplyState(Box::new(scene.clone())));
        h.key_down(egui::Key::ArrowRight);
        run_for(&mut h, 4);
        assert!(
            h.state().presented_for_test().is_empty(),
            "Accuracy Mode presents nothing through the interpolator"
        );
        assert_eq!(h.state().frame_interp_tally_for_test().interpolated, 0);
        println!("Accuracy Mode: nothing presented by the interpolator, as required");
    }

    // --- Enhanced, Smoother motion on. ---
    settings.mode = Mode::Enhanced;
    retroforge::game_settings::save(&dir, &base, &settings).expect("settings");
    let mut h = open(&path);
    h.state_mut().frame_interp_alpha_for_test(Some(128));
    h.state_mut().log_presented_for_test();
    h.state_mut().resume_for_test();
    h.state()
        .send_core_command_for_test(CoreCommand::ApplyState(Box::new(scene)));
    h.key_down(egui::Key::ArrowRight);
    h.key_down(egui::Key::Z);
    run_for(&mut h, 14);
    println!("{}", h.state().frame_interp_debug_for_test());
    let tally = h.state().frame_interp_tally_for_test();
    println!("gaps between console frames: {tally:?}");
    let log = h.state().presented_for_test();
    println!("{} pictures presented", log.len());
    let (w, rows) = h.state().frame_size_for_test().expect("a size");

    // The best triple: console a, intermediate from a, console a + 1, with a scrolling picture and a
    // HUD that did not change.
    let hud_bytes = 24 * w * 4;
    let mut best: Option<(usize, usize)> = None;
    // The first pairs after a state load are not steady play (the loaded frame is part-way through).
    for i in 12..log.len().saturating_sub(2) {
        let (
            (Shown::Console(a), pa),
            (Shown::Intermediate { from, .. }, pm),
            (Shown::Console(b), pb),
        ) = (&log[i], &log[i + 1], &log[i + 2])
        else {
            continue;
        };
        if *from != *a || *b != *a + 1 || pa[..hud_bytes] != pb[..hud_bytes] {
            continue;
        }
        if let Some(k) = (0..hud_bytes).find(|k| pm[*k] != pa[*k]) {
            panic!(
                "HUD rows differ across N, intermediate, N+1: first at byte {k} (x {}, y {}), {} vs {} (log {i}, console {a} -> {b}, alpha {:?})",
                (k / 4) % w, k / 4 / w, pm[k], pa[k], log[i + 1].0
            );
        }
        let moved = |x: &[u8], y: &[u8]| x.iter().zip(y).filter(|(p, q)| p != q).count();
        let score = moved(pm, pa).min(moved(pm, pb));
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((i, score));
        }
    }
    let (i, score) = best.expect("a console / intermediate / console triple while scrolling");
    println!("chosen triple at log index {i}; the intermediate differs from both console frames by {score} bytes at least");
    assert!(
        score > 400,
        "the intermediate is neither neighbour: {score}"
    );
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens/w36");
    std::fs::create_dir_all(&out).expect("docs/screens/w36");
    for (k, name) in ["a-console-n", "b-intermediate", "c-console-n-plus-1"]
        .iter()
        .enumerate()
    {
        save(
            &out.join(format!("smb-1-1-120hz-{name}.png")),
            w,
            rows,
            &log[i + k].1,
        );
    }
    assert!(
        tally.interpolated > 100,
        "steady play interpolates: {tally:?}"
    );
}
