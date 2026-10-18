//! Ticket W33-08: vertical and 8-way scouting. ROM-gated (law 5): every test
//! is `#[ignore]` and skips with a message when the ROM is absent. The
//! evidence is written up in `docs/research/W33-08-vertical.md`.
//!
//! * `mm2_shaft_is_scouted_vertically`: the acceptance run. Air Man's first
//!   horizontal section, then the shaft it ends in; the real driver (worker
//!   thread, ladder, self-check, merge) scouts it DOWN and the live canvas
//!   gets a screen taller.
//! * `probe_scout_y`: one Y pass by hand (`RF_STEP`, `RF_HEALTH`, `RF_NOHOLD`).
//! * `axis_at_level_start`: which axis the tracker names at the start of
//!   SMW, SMB3, MMX and MM2 (none: a standing player has no axis).
//! * `smw_up_pass_releases_the_hold_and_moves_the_player_y_word`: the SNES half of
//!   the Y machinery in Yoshi's Island 1 (no vertical SMW level is reachable).
//!
//! ```text
//! cargo test --release -p retroforge --test w33_08_vertical -- --ignored --nocapture
//! ```
#[allow(dead_code, unused_imports)]
mod w33_07_common;
use w33_07_common::*;

use retroforge::scout::{LiveView, ScoutDriver, ScoutPlan, ScoutSetup, ScoutTelemetry};
use std::sync::atomic::Ordering::Relaxed;

const RIGHT: u16 = 1 << 7;

/// The MM2 plan the profile makes (its `[scout]` table), settle 0.
fn mm2_plan(rig: &Rig) -> ScoutPlan {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(MM2.2);
    let outcome = rf_profiles::load_file(&p).expect("profile");
    let mut plan = ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"));
    plan.settle_frames = 0;
    plan.scene_key = rig.scene_key.clone();
    plan
}

/// Scout the scene the rig stands in with `plan`, through the real driver.
/// Returns `(width, height)` of the scene's biggest layer canvas before and
/// after, the passes' directions, and the telemetry line.
fn scout_dims(
    rig: &mut Rig,
    rom: &[u8],
    plan: ScoutPlan,
) -> ((usize, usize), (usize, usize), Vec<String>, String) {
    let scene = rig.live.current_scene().expect("a scene");
    let dims = |rig: &Rig| {
        rig.live.scene_canvases(scene.0).map_or((0, 0), |set| {
            (
                set.iter().map(|c| c.width()).max().unwrap_or(0),
                set.iter().map(|c| c.height()).max().unwrap_or(0),
            )
        })
    };
    let before = dims(rig);
    let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
    let mut driver = ScoutDriver::default();
    driver.set(Some(ScoutSetup {
        plan,
        telemetry: telemetry.clone(),
    }));
    let t0 = std::time::Instant::now();
    // Bounded: 600 s of 10 ms ticks.
    while t0.elapsed() < std::time::Duration::from_secs(600) {
        driver.tick(LiveView {
            stepper: &rig.st,
            rom,
            canvases: &mut rig.live,
            scene,
            size: (256, 240),
            scrolls: no_scrolls,
        });
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let report = telemetry.last_report.lock().ok().and_then(|r| r.clone());
    let dirs = report
        .as_ref()
        .map(|r| r.passes.iter().map(|p| format!("{:?}", p.dir)).collect())
        .unwrap_or_default();
    let line = format!(
        "started {} finished {} gave_up {} aborted {} frames {} cells {} agreement {:?}\u{2030}; ends {:?}; {}",
        telemetry.started.load(Relaxed),
        telemetry.finished.load(Relaxed),
        telemetry.gave_up.load(Relaxed),
        telemetry.aborted.load(Relaxed),
        telemetry.frames.load(Relaxed),
        telemetry.cells.load(Relaxed),
        telemetry.agreement_permille(),
        telemetry.last_ends.lock().map(|e| e.clone()).unwrap_or_default(),
        telemetry.last_summary().unwrap_or_default(),
    );
    (before, dims(rig), dirs, line)
}

/// Air Man: the way the scout walks the horizontal section (Right held, Mega
/// Man's Y held high) to the shaft, released at the end of it (he falls in),
/// then `after` more frames in the shaft.
fn mm2_to_shaft(rig: &mut Rig, after: u32) {
    rig.script(MM2_TO_LEVEL);
    for f in 0..(2150 + after) {
        if f < 2150 {
            rig.st.poke_bus(0x4A0, 0x20);
        }
        rig.st.poke_bus(0x6C0, 0x1C);
        rig.frame(RIGHT);
    }
}

/// W33-08 acceptance: the scene after the first horizontal section is
/// vertical, the tracker says so, and the scout goes down it: the canvas gets
/// TALLER, with no change of width, by the real driver (worker thread, ladder,
/// self-check, merge).
#[test]
#[ignore = "needs Mega Man 2"]
fn mm2_shaft_is_scouted_vertically() {
    let Some(mut rig) = Rig::open(MM2.0, MM2.1, Some(MM2.2)) else {
        return;
    };
    let path = rom_file(MM2.0, MM2.1).unwrap();
    let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    mm2_to_shaft(&mut rig, 300);
    assert_eq!(
        rig.live.scene_scrolls_vertically(),
        Some(true),
        "the tracker names the shaft's axis"
    );
    let plan = mm2_plan(&rig);
    let (before, after, dirs, line) = scout_dims(&mut rig, &rom, plan);
    println!("MM2 SHAFT before {before:?} after {after:?} passes {dirs:?}; {line}");
    assert_eq!(dirs.first().map(String::as_str), Some("Down"), "{line}");
    assert!(
        after.1 >= before.1 + 200,
        "the canvas grew taller by a screen: {before:?} -> {after:?}; {line}"
    );
    // Screen 0b scrolls a few pixels sideways by itself (camera x 5); the
    // scout does not walk sideways.
    assert!(
        after.0 <= before.0 + 16,
        "no growth sideways: {before:?} -> {after:?}"
    );
}

/// Debug: one Y pass by hand, printing what the scout machine does.
#[test]
#[ignore = "needs Mega Man 2"]
fn probe_scout_y() {
    use retroforge::scout::{Scout, ScoutDir, ScoutStatus, ScoutStrategy};
    let Some(mut rig) = Rig::open(MM2.0, MM2.1, Some(MM2.2)) else {
        return;
    };
    let path = rom_file(MM2.0, MM2.1).unwrap();
    let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    mm2_to_shaft(&mut rig, 300);
    let scene = rig.live.current_scene().unwrap();
    let mut plan = mm2_plan(&rig);
    if std::env::var("RF_HEALTH").is_ok() {
        plan.hold.push((0x6C0, 0x1C));
    }
    if std::env::var("RF_NOHOLD").is_ok() {
        plan.hold.clear();
    }
    let step: u16 = std::env::var("RF_STEP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    let snap = rig.st.snapshot().unwrap();
    let seed = rig.live.fork_for_scout();
    let mut sc = Scout::start(&rom, snap, plan, &seed, scene, (256, 240)).unwrap();
    sc.restart(ScoutStrategy::PlayerMove, step, 3000, ScoutDir::Down)
        .unwrap();
    for f in 0..700u32 {
        let st = sc.step(&no_scrolls);
        if f % 10 == 0 || st != ScoutStatus::Running {
            let fr = sc.frames().and_then(|fr| fr.origins[0]);
            println!(
                "f{f} {st:?} origin {fr:?} y4a0={:02x} scr20={:02x} fd={:02x} extent {}",
                sc.peek_word(0x4A0) & 0xff,
                sc.peek_word(0x20) & 0xff,
                sc.peek_word(0xFD) & 0xff,
                sc.extent()
            );
        }
        if st != ScoutStatus::Running {
            break;
        }
    }
}

/// Debug: which axis does the tracker name at the start of each level?
#[test]
#[ignore = "needs SMW, SMB3, MMX, MM2"]
fn axis_at_level_start() {
    for (g, to_level) in [
        (SMW, SMW_TO_LEVEL),
        (SMB3, SMB3_TO_LEVEL),
        (MMX, MMX_TO_LEVEL),
        (MM2, MM2_TO_LEVEL),
    ] {
        let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
            continue;
        };
        rig.script(to_level);
        let mut line = String::new();
        for i in 0..12 {
            rig.script("w30");
            line += &format!("{:?} ", rig.live.scene_scrolls_vertically());
            let _ = i;
        }
        println!("AXIS {}: {line}", g.1);
    }
}

/// Super Mario World has no vertical level a script here reaches (only
/// Yoshi's Island 1 and its pipe room), so the SNES half of the Y machinery
/// is checked in Yoshi's Island 1 itself: an UP pass with the hand-made table
/// (its Y rows: `camera_y` $1C, `player_y` $96/$97, and the two `hold` rows on
/// the same bytes, which a Y pass releases). The recipe has to move Mario's Y
/// word where the hold would pin it at 150, and the pass has to end cleanly.
/// (Yoshi's Island 1 does not scroll vertically: the camera's Y word stays at
/// 192 with Mario at the top of the level, so the canvas does not get taller
/// here; that is the level's, not the scout's.)
#[test]
#[ignore = "needs SMW"]
fn smw_up_pass_releases_the_hold_and_moves_the_player_y_word() {
    use retroforge::scout::{Scout, ScoutDir, ScoutStatus};
    let Some(mut rig) = Rig::open(SMW.0, SMW.1, Some(SMW.2)) else {
        return;
    };
    let path = rom_file(SMW.0, SMW.1).unwrap();
    let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    rig.script(SMW_TO_LEVEL);
    rig.script("w60");
    let scene = rig.live.current_scene().unwrap();
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SMW.2);
    let outcome = rf_profiles::load_file(&p).expect("profile");
    let mut plan = ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"));
    plan.settle_frames = 0;
    plan.scene_key = rig.scene_key.clone();
    plan.dirs = vec![ScoutDir::Up];
    let snap = rig.st.snapshot().unwrap();
    let seed = rig.live.fork_for_scout();
    let mut sc = Scout::start(&rom, snap, plan, &seed, scene, (256, 224)).unwrap();
    let cam = |sc: &Scout| sc.peek_word(0x7E_001C);
    let start_cam = cam(&sc);
    let mut min_cam = start_cam;
    let mut min_player = u16::MAX;
    let mut last = ScoutStatus::Running;
    for f in 0..1500u32 {
        last = sc.step(&retroforge::core_thread::snes_layer_scrolls);
        min_cam = min_cam.min(cam(&sc));
        min_player = min_player.min(sc.peek_word(0x7E_0096));
        if f % 50 == 0 || last != ScoutStatus::Running {
            println!(
                "f{f} {last:?} cam_y {} player_y {} extent {}",
                cam(&sc),
                sc.peek_word(0x7E_0096),
                sc.extent()
            );
        }
        if last != ScoutStatus::Running {
            break;
        }
    }
    let dims = sc.canvases().map(|set| {
        set.iter()
            .map(|c| (c.width(), c.height()))
            .collect::<Vec<_>>()
    });
    println!("SMW UP start cam_y {start_cam}, lowest {min_cam}, ended {last:?}, canvases {dims:?}");
    assert!(
        min_player < 150,
        "the hold on Mario's Y was not released: lowest Y {min_player}"
    );
    assert!(matches!(last, ScoutStatus::Finished(_)), "the pass ended");
}
