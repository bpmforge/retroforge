//! **W37-06: Smoother motion with 32:9 widescreen and the level scout's canvas on, Super Mario Bros.
//! 1-1** (ROM-gated, law 5: only the rendered PNGs are committed).
//!
//! The path is the shipped app's, as in W36-06c: widescreen 32:9 with the scout, a `[scout]` table found
//! by watching the game, the scout filling the canvas of 1-1, the live-enemies image playing the level
//! (enemies drawn into the margins). Then "Smoother motion" goes on, the Presenter's alpha is pinned to the
//! middle of each gap (the 120 Hz case) and the player runs right for a while. Reported: how many gaps
//! got an intermediate, why the others did not, and the cost; saved: console frame N, the intermediate
//! and N+1 as the player sees them (856 wide).
//!
//! ```text
//! cargo test --release -p retroforge --test w37_06_widescreen -- --ignored --nocapture
//! ```
//!
//! One test per binary: `set_var` is process-global.

#[allow(dead_code, unused_imports)]
#[path = "w33_07_common/mod.rs"]
mod discovery;
mod w36_06b_common;

use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::core_thread::CoreCommand;
use retroforge::frame_interp::Shown;
use retroforge::game_settings::Mode;
use retroforge::stepper::EmuStepper;
use rf_enhance::widescreen::Aspect;
use w36_06b_common::*;

/// The cart's shipped profile (it has no `[scout]` table).
const PROFILE: &str = "../../profiles/nes/super-mario-bros/profile.toml";

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR) and a GPU"]
fn smb_1_1_at_32_9_with_smoother_motion() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let dir = std::env::temp_dir().join(format!("retroforge_w37_06_wide_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let profiles = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    // SAFETY: first statement of the only test in this binary. TMPDIR is the cold canvas cache.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var("TMPDIR", &dir);
        std::env::set_var("RETROFORGE_PROFILES_DIR", &profiles);
    }

    let walk = ["R+B14 R+B+A24"; 12].join(" ");
    let (skeleton, ..) = discovery::discover_game(
        ("nes", FILE, PROFILE),
        "w200 S6 w500",
        &format!("L10 w5 {walk}"),
        false,
    )
    .expect("the finder names a scout table for Super Mario Bros.");
    let mut plan = retroforge::scout_discovery::plan_of(&skeleton);
    plan.settle_frames = 30;
    // See W36-06c: the stack byte the finder also reported is not a start condition for another player.
    plan.start_when.retain(|(addr, _)| *addr != 0x01F7);
    assert!(!plan.start_when.is_empty(), "no level-state row");
    plan.scene_key = rf_profiles::load_file(&Path::new(env!("CARGO_MANIFEST_DIR")).join(PROFILE))
        .expect("profile")
        .profile
        .scene
        .as_ref()
        .map(retroforge::scene_key::SceneKeySpec::from_profile);

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
    let telemetry = h.state().scout_telemetry_for_test();

    let image = mod_image(&bytes);
    let session = retroforge::mod_chunk::ModSession {
        mods: vec![MOD.to_string()],
        recipe: recipe(),
    };
    let mut st = EmuStepper::open(&image).expect("opens");
    for f in 0..620u32 {
        let pad = if f < 500 { script_two(f) } else { 0 };
        st.latch_and_advance_frame(input(pad), &mut Discard);
    }
    st.poke_bus(0x079E, 0xFF);
    let level_start = st.save_state_with_mods(0, Some(&session)).expect("saves");

    h.state_mut().set_scout_plan_for_test(Some(plan));
    h.state_mut()
        .set_widescreen_aspect_for_test(Aspect::Wide32x9);
    h.state_mut().set_widescreen(true);
    h.state_mut().resume_for_test();
    h.state()
        .send_core_command_for_test(CoreCommand::ApplyState(Box::new(level_start)));
    h.key_down(egui::Key::ArrowRight);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    h.state().send_core_command_for_test(CoreCommand::Resume);
    let t0 = Instant::now();
    while telemetry.started.load(Relaxed) == 0 && t0.elapsed() < Duration::from_secs(60) {
        run_n(&mut h, 4);
    }
    h.key_up(egui::Key::ArrowRight);
    h.state().send_core_command_for_test(CoreCommand::Pause);
    run_n(&mut h, 4);
    while t0.elapsed() < Duration::from_secs(400)
        && telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) == 0
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "scout: started {} finished {} cells {} frames {}",
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.frames.load(Relaxed),
    );
    assert!(
        telemetry.finished.load(Relaxed) >= 1 && telemetry.cells.load(Relaxed) > 20_000,
        "the scout did not fill the canvas"
    );

    // Smoother motion on, real time, one intermediate per console frame; Mario kept unhurt.
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
    h.state_mut().frame_interp_alpha_for_test(Some(128));
    h.state_mut().log_presented_for_test();
    h.state_mut().set_frame_interpolation_for_test(true);
    h.state().send_core_command_for_test(CoreCommand::Resume);
    h.key_down(egui::Key::ArrowRight);
    h.key_down(egui::Key::Z);
    let t1 = Instant::now();
    let mut n = 0u32;
    while t1.elapsed() < Duration::from_secs(16) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
        n += 1;
        if n.is_multiple_of(16) {
            h.state().send_core_command_for_test(CoreCommand::PokeBus {
                addr: 0x079E,
                value: 0x30,
            });
        }
    }
    println!("{}", h.state().frame_interp_debug_for_test());
    let tally = h.state().frame_interp_tally_for_test();
    println!("gaps between console frames: {tally:?}");
    println!(
        "refusals by name (pictures tried): {:?}",
        h.state().frame_interp_refusals_for_test()
    );
    let share = tally.interpolated as f64 / (tally.interpolated + tally.repeated).max(1) as f64;
    println!(
        "32:9 share of gaps interpolated: {}/{} = {:.1}%",
        tally.interpolated,
        tally.interpolated + tally.repeated,
        share * 100.0
    );
    let log = h.state().presented_for_test();
    let (w, rows) = h.state().frame_size_for_test().expect("a size");
    println!("{} pictures presented, {w}x{rows}", log.len());
    assert_eq!(w, 856, "32:9 over the NES's 240 lines");

    // The best triple: console a, intermediate from a, console a + 1, scrolling, HUD rows unchanged.
    let hud_bytes = 24 * w * 4;
    let mut best: Option<(usize, usize)> = None;
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
        assert_eq!(&pm[..hud_bytes], &pa[..hud_bytes], "HUD rows are frame N's");
        let moved = |x: &[u8], y: &[u8]| x.iter().zip(y).filter(|(p, q)| p != q).count();
        let score = moved(pm, pa).min(moved(pm, pb));
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((i, score));
        }
    }
    let (i, score) = best.expect("a console / intermediate / console triple while scrolling");
    println!("chosen triple at log index {i}; the intermediate differs from both neighbours by {score} bytes at least");
    assert!(
        score > 400,
        "the intermediate is neither neighbour: {score}"
    );
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens/w37");
    std::fs::create_dir_all(&out).expect("docs/screens/w37");
    for (k, name) in ["a-console-n", "b-intermediate", "c-console-n-plus-1"]
        .iter()
        .enumerate()
    {
        save(
            &out.join(format!("smb-1-1-32x9-120hz-{name}.png")),
            w,
            rows,
            &log[i + k].1,
        );
    }
    assert!(share > 0.5, "steady play interpolates at 32:9: {tally:?}");
}
