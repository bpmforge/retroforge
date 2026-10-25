//! **W36-06b: by-eye screenshot of Super Mario Bros. 1-1 at 32:9 with live enemies in the margins**
//! (ROM-gated, law 5: only the rendered PNGs are committed).
//!
//! The scene is found on the headless machine (the live-enemies image, Mario kept unhurt by a test-side
//! poke, a frame where enemies stand in both margins and keep standing there while Mario idles), saved
//! as a state, and loaded into the real app. The app renders it twice from the same state and frame
//! count: the margin objects off (`SetModSession(None)`) and on. The 256-px picture must be identical
//! in the two.
//!
//! ```text
//! cargo test --release -p retroforge --test w36_06b_screens -- --ignored --nocapture
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
use retroforge::game_settings::Mode;
use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, PpuPixel};
use rf_enhance::widescreen::Aspect;
use w36_06b_common::*;

struct Discard;

impl CoreSink for Discard {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// Camera-relative x of each active enemy slot.
fn rels(st: &EmuStepper) -> Vec<i32> {
    let cam = i32::from(st.peek_bus(0x071A)) * 256 + i32::from(st.peek_bus(0x071C));
    (0..5u32)
        .filter(|k| st.peek_bus(0x0F + k) != 0)
        .map(|k| i32::from(st.peek_bus(0x6E + k)) * 256 + i32::from(st.peek_bus(0x87 + k)) - cam)
        .collect()
}

/// Enemies deep in the right margin, deep in the left, and in the picture.
fn census(st: &EmuStepper) -> (usize, usize, usize) {
    let r = rels(st);
    (
        r.iter().filter(|r| (300..480).contains(*r)).count(),
        r.iter().filter(|r| (-250..-100).contains(*r)).count(),
        r.iter().filter(|r| (24..232).contains(*r)).count(),
    )
}

/// Play the unhurt script on the live-enemies image to a frame where enemies stand in both margins and
/// still do 45 frames later with the pad idle and Mario alive; return the state container there.
fn find_scene(image: &[u8], session: &retroforge::mod_chunk::ModSession) -> rf_state::Container {
    let mut st = EmuStepper::open(image).expect("opens");
    let mut best: Option<((usize, usize, usize), rf_state::Container)> = None;
    for f in 0..3200u32 {
        let inp = input(script_two(f));
        if st.peek_bus(0x0770) == 1 {
            st.poke_bus(0x079E, 0x30);
        }
        st.latch_and_advance_frame(inp, &mut Discard);
        if f < 600 || st.peek_bus(0x0770) != 1 || f % 4 != 0 {
            continue;
        }
        let (right, left, inside) = census(&st);
        if right == 0
            || best.as_ref().is_some_and(|(c, _)| {
                (c.0 > 0) as usize + (c.1 > 0) as usize + (c.2 > 0) as usize
                    >= (right > 0) as usize + (left > 0) as usize + (inside > 0) as usize
            })
        {
            continue;
        }
        // Does the scene hold with the pad idle?
        let snap = st.snapshot().expect("snapshots");
        let mut later = EmuStepper::open(image).expect("opens");
        later.restore(&snap).expect("restores");
        for _ in 0..45 {
            later.latch_and_advance_frame(input(0), &mut Discard);
        }
        let after = census(&later);
        if later.peek_bus(0x0770) == 1 && after.0 > 0 {
            best = Some((
                (right, left, inside),
                st.save_state_with_mods(0, Some(session)).expect("saves"),
            ));
            if left > 0 && inside > 0 {
                break;
            }
        }
    }
    let (c, container) = best.expect("a scene with margin enemies");
    println!(
        "scene: enemies deep right {}, deep left {}, in the picture {}",
        c.0, c.1, c.2
    );
    container
}

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn run_n(h: &mut Harness<'_, RetroForgeApp>, n: usize) {
    for _ in 0..n {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// The picture the app composed for the last frame.
fn grab(h: &Harness<'_, RetroForgeApp>) -> (usize, usize, Vec<u8>) {
    let rgba = h.state().last_frame_rgba_for_test().expect("a frame");
    let (w, hh) = h.state().frame_size_for_test().expect("a size");
    assert_eq!(rgba.len(), w * hh * 4);
    (w, hh, rgba)
}

fn save(path: &Path, w: usize, h: usize, rgba: &[u8]) {
    let mut opaque = rgba.to_vec();
    for px in opaque.chunks_exact_mut(4) {
        px[3] = 255;
    }
    image::save_buffer(path, &opaque, w as u32, h as u32, image::ColorType::Rgba8).expect("png");
    println!("SHOT {}", path.display());
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR) and a GPU"]
fn smb_1_1_at_32_9_with_enemies_in_the_margins() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let dir =
        std::env::temp_dir().join(format!("retroforge_w36_06b_screens_{}", std::process::id()));
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

    // The mod needs the atlas of the unpatched image in the per-ROM cache (the app's Rebuild panel
    // stores it); this is that step.
    let bytes = retroforge::rom_open::load_rom_bytes(&path).expect("the zip opens");
    let raw_sha = retroforge::hash::sha256_hex(&bytes);
    retroforge::rebuild::store_atlas_for_test(&raw_sha, &atlas_of(&bytes));
    let base = match rf_cart::Cartridge::load(&bytes).expect("loads") {
        rf_cart::Cartridge::Nes { identity, .. } => identity.normalized.sha256,
        rf_cart::Cartridge::Snes { .. } => unreachable!("a NES image"),
    };
    let mut settings = retroforge::game_settings::GameSettings::default();
    settings.mode = Mode::Enhanced;
    settings.mods_enabled = vec![MOD.to_string()];
    settings.widescreen_aspect = Aspect::Wide32x9;
    retroforge::game_settings::save(&dir, &base, &settings).expect("settings");

    let mut h = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().open_rom_path(&path);
    run_n(&mut h, 3);
    let badge = h.state().status_badge();
    println!("badge: {badge}");
    assert!(badge.contains("mod live-enemies"), "{badge}");
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    h.state_mut().resume_for_test();
    run_frames(&mut h, 20, Duration::from_secs(60));

    // The scene, from the same image the app runs (patched bytes, so the same machine).
    let image = mod_image(&bytes);
    let session = retroforge::mod_chunk::ModSession {
        mods: vec![MOD.to_string()],
        recipe: recipe(),
    };
    let scene = find_scene(&image, &session);
    let mut shots = Vec::new();
    // The first pass only fills the margins' canvas (the scene's first render has none yet); the two
    // that are kept are rendered from the same warm canvas.
    for on in [true, false, true] {
        h.state().send_core_command_for_test(CoreCommand::Pause);
        h.state()
            .send_core_command_for_test(CoreCommand::SetModSession(
                on.then(|| Box::new(session.clone())),
            ));
        h.state()
            .send_core_command_for_test(CoreCommand::ApplyState(Box::new(scene.clone())));
        // Same game frame both times: the state's own count plus 8 stepped frames (the first
        // four fill the redraw's history of states).
        for _ in 0..8 {
            h.state().send_core_command_for_test(CoreCommand::StepFrame);
            run_n(&mut h, 8);
        }
        shots.push(grab(&h));
    }
    let (off, on) = (&shots[1], &shots[2]);
    assert_eq!((off.0, off.1), (on.0, on.1));
    let (w, hh) = (off.0, off.1);
    assert_eq!(w, 856, "32:9 over the NES's 240 lines");
    let pad = (w - 256) / 2;
    // The 256-px picture is identical (the two renders can differ by a few frames if the app's pacing
    // let one run longer; compare the central dots only when the frame counts agree, and say so).
    let (mut centre_diff, mut pad_diff) = (0usize, 0usize);
    for y in 0..hh {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if off.2[i..i + 4] != on.2[i..i + 4] {
                if (pad..pad + 256).contains(&x) {
                    centre_diff += 1;
                } else {
                    pad_diff += 1;
                }
            }
        }
    }
    println!(
        "dots that differ with the margin objects on: real picture {centre_diff}, pad {pad_diff}"
    );
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens/w36");
    std::fs::create_dir_all(&out).expect("docs/screens/w36");
    save(
        &out.join("smb-1-1-32x9-margin-objects-off.png"),
        w,
        hh,
        &off.2,
    );
    save(
        &out.join("smb-1-1-32x9-margin-objects-on.png"),
        w,
        hh,
        &on.2,
    );
    assert!(
        pad_diff > 500,
        "the margin objects drew something: {pad_diff}"
    );
    assert_eq!(
        centre_diff, 0,
        "the 256-px picture is identical with the layer on or off"
    );
}
