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
use std::time::Duration;

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::core_thread::CoreCommand;
use retroforge::game_settings::Mode;
use rf_enhance::widescreen::Aspect;
use w36_06b_common::*;

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
    let scene = find_scene(&image, &session, 45, false, 0..=i32::MAX);
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
    // The committed pair is W36-06c's (the same scene over the level scout's scenery); this one has no
    // canvas in its margins, so it stays in the scratch directory.
    let out = dir.clone();
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
