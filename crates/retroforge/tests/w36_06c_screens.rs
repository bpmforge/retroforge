//! **W36-06c: Super Mario Bros. 1-1 at 32:9 with the level scout's scenery in the margins and the
//! live enemies drawn over it** (ROM-gated, law 5: only the rendered PNGs are committed).
//!
//! The path is the shipped app's: widescreen 32:9 on with the scout, a `[scout]` table found by
//! watching the game (this cart's shipped profile has none; the player gets one from "Find scout
//! settings"), the scout filling the canvas of 1-1 on a cold cache, then the live-enemies image
//! playing that level. Two captures of one game frame: the margin objects off (the canvas alone)
//! and on. Nothing but enemy dots may differ, and the 256-px picture is identical.
//!
//! ```text
//! cargo test --release -p retroforge --test w36_06c_screens -- --ignored --nocapture
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
use retroforge::game_settings::Mode;
use retroforge::stepper::EmuStepper;
use rf_enhance::widescreen::Aspect;
use w36_06b_common::*;

/// The cart's shipped profile (it has no `[scout]` table).
const PROFILE: &str = "../../profiles/nes/super-mario-bros/profile.toml";

/// The pad as the keyboard holds it: Right, B (Z) and A (X).
#[derive(Default)]
struct Keys {
    held: u8,
}

impl Keys {
    fn set(&mut self, h: &mut Harness<'_, RetroForgeApp>, pad: u8) {
        for (bit, key) in [
            (RIGHT, egui::Key::ArrowRight),
            (B, egui::Key::Z),
            (A, egui::Key::X),
        ] {
            match ((self.held & bit) != 0, (pad & bit) != 0) {
                (false, true) => h.key_down(key),
                (true, false) => h.key_up(key),
                _ => {}
            }
        }
        self.held = pad;
    }

    /// One console frame with `pad` held, as the Presenter's input latch sees the keys (the game is paused).
    fn step(&mut self, h: &mut Harness<'_, RetroForgeApp>, pad: u8) {
        self.set(h, pad);
        h.run_steps(2);
        let before = h.state().frame_count_for_test();
        h.state().send_core_command_for_test(CoreCommand::StepFrame);
        let start = Instant::now();
        while h.state().frame_count_for_test() <= before && start.elapsed() < Duration::from_secs(5)
        {
            h.run_steps(1);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR) and a GPU"]
fn smb_1_1_at_32_9_scenery_with_enemies_over_it() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let dir =
        std::env::temp_dir().join(format!("retroforge_w36_06c_screens_{}", std::process::id()));
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

    // The table the finder reports for this game (what "Find scout settings" would offer).
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
    // The finder also reported a stack byte ($01F7 = 175) as a start condition. It read 175 on every
    // level frame of the finder's own session and on none of 2,600 level frames of the "Mario unhurt"
    // script, so with it the scout would hardly ever start for another player (a finder gate
    // problem, not this ticket's: noted in docs/design/REBUILD_LAB.md). The level-state rows stay.
    plan.start_when.retain(|(addr, _)| *addr != 0x01F7);
    assert!(
        !plan.start_when.is_empty(),
        "the finder named no level-state row"
    );
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
    let badge = h.state().status_badge();
    println!("badge: {badge}");
    assert!(badge.contains("mod live-enemies"), "{badge}");
    let telemetry = h.state().scout_telemetry_for_test();

    // 1-1 just entered (Mario standing at the start of the level, injury timer run up: the game idles
    // while the scout takes its snapshot, and he must be alive then), as the app's own state.
    let image = mod_image(&bytes);
    let session = retroforge::mod_chunk::ModSession {
        mods: vec![MOD.to_string()],
        recipe: recipe(),
    };
    let mut st = EmuStepper::open(&image).expect("opens");
    // (Start pressed twice, the intro screen waited out, then the pad idle until the level runs.)
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
    let pad_at = |t: u32| script_two(500 + t);
    let mut keys = Keys::default();
    // The player walks right. The finder's level-state row only reads as in a level while Mario moves,
    // so the scout starts once he does, and takes its snapshot from where he is by then.
    keys.set(&mut h, RIGHT);
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    h.state().send_core_command_for_test(CoreCommand::Resume);
    let t0 = Instant::now();
    while telemetry.started.load(Relaxed) == 0 && t0.elapsed() < Duration::from_secs(60) {
        run_n(&mut h, 4);
    }
    keys.set(&mut h, 0);
    h.state().send_core_command_for_test(CoreCommand::Pause);
    run_n(&mut h, 4);
    while t0.elapsed() < Duration::from_secs(400)
        && telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) == 0
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    let ends = telemetry
        .last_ends
        .lock()
        .map(|e| e.clone())
        .unwrap_or_default();
    println!(
        "scout: started {} finished {} aborted {} gave_up {} cells {} frames {}; passes ended {ends:?}",
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.frames.load(Relaxed),
    );
    assert!(
        telemetry.finished.load(Relaxed) >= 1 && telemetry.aborted.load(Relaxed) == 0,
        "the scout did not finish cleanly on the mod session"
    );
    assert!(
        telemetry.cells.load(Relaxed) > 20_000,
        "the scout merged only {} cells: its player was killed on the way",
        telemetry.cells.load(Relaxed)
    );

    // Where the live game stands now: the app writes its state to a slot file.
    let slots = dir.join("slots");
    std::fs::create_dir_all(&slots).expect("slots dir");
    h.state()
        .send_core_command_for_test(CoreCommand::SaveStateToSlot {
            dir: slots.clone(),
            stem: "slot1".to_string(),
        });
    let file = slots.join("slot1.rfstate");
    let t1 = Instant::now();
    while !file.exists() && t1.elapsed() < Duration::from_secs(20) {
        run_n(&mut h, 2);
    }
    run_n(&mut h, 2);
    let (here, _) =
        rf_state::Container::decode_default(&std::fs::read(&file).expect("the slot file"))
            .expect("decodes");

    // The player's run from there (the unhurt script of W36-06, one pad per frame), played offline on
    // the same machine to find a frame with an enemy deep in the right margin; the state eight frames
    // before it is what the captures start from.
    let mut st = EmuStepper::open(&image).expect("opens");
    st.load_state(&here).expect("loads");
    let cam_of =
        |st: &EmuStepper| i32::from(st.peek_bus(0x071A)) * 256 + i32::from(st.peek_bus(0x071C));
    println!(
        "live game at the scout's end: camera {}, mario state {:02x}, injury timer {:02x}",
        cam_of(&st),
        st.peek_bus(0x000E),
        st.peek_bus(0x079E)
    );
    assert_eq!(st.peek_bus(0x0770), 1, "the game is not in play");
    let mut ring: std::collections::VecDeque<rf_state::Container> =
        std::collections::VecDeque::new();
    let mut pick: Option<(u32, rf_state::Container, (usize, usize, usize))> = None;
    for t in 0..500u32 {
        ring.push_back(st.save_state_with_mods(0, Some(&session)).expect("saves"));
        if ring.len() > 8 {
            ring.pop_front();
        }
        st.latch_and_advance_frame(input(pad_at(t)), &mut Discard);
        let (right, left, inside) = census(&st);
        // Enemies deep in the right margin and on the picture, while both margins still have canvas.
        if t >= 10
            && st.peek_bus(0x0770) == 1
            && st.peek_bus(0x000E) == 8
            && right > 0
            && inside > 0
            && cam_of(&st) <= 220
        {
            pick = Some((t + 1, ring[0].clone(), (right, left, inside)));
            println!("candidate frame {}: camera {}", t + 1, cam_of(&st));
            break;
        }
    }
    let (target, from_state, census_at) =
        pick.expect("a frame with an enemy deep in the right margin");
    println!(
        "frame {target} of the run: enemies deep right {}, deep left {}, in the picture {}",
        census_at.0, census_at.1, census_at.2
    );

    // The player plays those frames with the real keys, one frame at a time (as the Presenter's input
    // latch sees them), so the live canvas holds what he has walked past.
    for t in 0..target.saturating_sub(8) {
        keys.step(&mut h, pad_at(t));
    }
    // The same eight frames three times from one state (a rewind of eight frames is no scene cut): the
    // margin objects on, off (the canvas alone), on. The first pass warms the redraw's history.
    let mut shots = Vec::new();
    for on in [true, false, true] {
        h.state()
            .send_core_command_for_test(CoreCommand::SetModSession(
                on.then(|| Box::new(session.clone())),
            ));
        h.state()
            .send_core_command_for_test(CoreCommand::ApplyState(Box::new(from_state.clone())));
        run_n(&mut h, 4);
        for k in 0..8 {
            keys.step(&mut h, pad_at(target - 8 + k));
        }
        run_n(&mut h, 4);
        assert_eq!(
            h.state().widescreen_source_for_test(),
            rf_enhance::widescreen::MarginSource::StitchedCanvas,
            "the margins are not filled from the scout's canvas"
        );
        shots.push(grab(&h));
    }
    keys.set(&mut h, 0);
    let (off, on) = (&shots[1], &shots[2]);
    assert_eq!((off.0, off.1), (on.0, on.1));
    let (w, hh) = (off.0, off.1);
    assert_eq!(w, 856, "32:9 over the NES's 240 lines");
    let pad = (w - 256) / 2;

    // Where the game keeps its enemies on that frame (the same machine run offline from the same state).
    let mut st = EmuStepper::open(&image).expect("opens");
    st.load_state(&from_state).expect("loads");
    for k in 0..8 {
        st.latch_and_advance_frame(input(pad_at(target - 8 + k)), &mut Discard);
    }
    let cam = i32::from(st.peek_bus(0x071A)) * 256 + i32::from(st.peek_bus(0x071C));
    let boxes: Vec<(i32, i32)> = (0..5u32)
        .filter(|k| st.peek_bus(0x0F + k) != 0)
        .map(|k| {
            let rel =
                i32::from(st.peek_bus(0x6E + k)) * 256 + i32::from(st.peek_bus(0x87 + k)) - cam;
            (pad as i32 + rel, i32::from(st.peek_bus(0xCF + k)))
        })
        .filter(|(x, _)| *x < pad as i32 || *x >= (pad + 256) as i32)
        .collect();
    println!("margin enemies at (x, y): {boxes:?}");
    assert!(!boxes.is_empty(), "no enemy in a margin on the frame");

    // The pad outside the enemies' boxes is the canvas-only picture; the 256-px picture is the same.
    let (mut centre_diff, mut stray, mut dots) = (0usize, 0usize, 0usize);
    let (mut scenery_left, mut scenery_right) = (0usize, 0usize);
    let sky = [
        off.2[(2 * w + pad + 2) * 4],
        off.2[(2 * w + pad + 2) * 4 + 1],
        off.2[(2 * w + pad + 2) * 4 + 2],
    ];
    for y in 0..hh {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let in_pad = !(pad..pad + 256).contains(&x);
            if in_pad && off.2[i..i + 3] != sky {
                if x < pad {
                    scenery_left += 1;
                } else {
                    scenery_right += 1;
                }
            }
            if off.2[i..i + 4] == on.2[i..i + 4] {
                continue;
            }
            if !in_pad {
                centre_diff += 1;
                continue;
            }
            // Enemy sprites are at most 16 wide and 32 tall; allow a tile either side.
            let near = boxes.iter().any(|(bx, by)| {
                (*bx - 12..*bx + 28).contains(&(x as i32))
                    && (*by - 8..*by + 40).contains(&(y as i32))
            });
            if near {
                dots += 1;
            } else {
                stray += 1;
            }
        }
    }
    println!(
        "centre dots that differ {centre_diff}; margin dots outside the enemy boxes that differ {stray}; enemy dots {dots}; scenery dots left {scenery_left}, right {scenery_right}"
    );
    let out = std::env::var("RF_06C_OUT").map_or_else(
        |_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens/w36"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&out).expect("out dir");
    save(
        &out.join("smb-1-1-32x9-scenery-margin-objects-off.png"),
        w,
        hh,
        &off.2,
    );
    save(
        &out.join("smb-1-1-32x9-scenery-margin-objects-on.png"),
        w,
        hh,
        &on.2,
    );
    assert_eq!(
        centre_diff, 0,
        "the 256-px picture is identical with the layer on or off"
    );
    assert_eq!(
        stray, 0,
        "the margin differs from the canvas-only capture away from the enemies"
    );
    assert!(dots > 100, "the enemies drew {dots} dots over the canvas");
    assert!(
        scenery_left > 1000 && scenery_right > 1000,
        "no scenery in a margin (left {scenery_left}, right {scenery_right})"
    );
}
