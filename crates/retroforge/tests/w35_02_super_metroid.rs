//! **W35-02: Super Metroid, settings found by observation.** ROM-gated; run one at a time, release:
//! `cargo test --release -p retroforge --test w35_02_super_metroid -- --ignored --nocapture <name>`

#[allow(dead_code)]
mod w33_07_common;

use std::path::Path;
use w33_07_common::*;

/// Explore: from a snapshot at free play, each `RF_SUFFIXES` script (`;` separated), RAM + shot.
#[test]
#[ignore = "explore"]
fn explore_branch() {
    let Some(mut rig) = Rig::open(SM.0, SM.1, None) else {
        return;
    };
    let prefix = std::env::var("RF_PREFIX").unwrap_or_else(|_| sm_to_free_play());
    rig.script(&prefix);
    let snap = rig.st.snapshot().unwrap();
    let dir = std::env::var("RF_SHOTS").unwrap_or_else(|_| "/tmp/sm".into());
    std::fs::create_dir_all(&dir).unwrap();
    let suffixes = std::env::var("RF_SUFFIXES").unwrap_or_default();
    for (i, suf) in suffixes.split(';').enumerate() {
        rig.st.restore(&snap).unwrap();
        let mut line = String::new();
        for tok in suf.split_whitespace() {
            rig.script(tok);
            let r = |a: u32| rig.st.peek_bus(0x7E_0000 + a);
            let w = |a: u32| u32::from(r(a)) | u32::from(r(a + 1)) << 8;
            line += &format!(
                "\n  {tok}: gs {:04x} room {:04x} cam {:04x},{:04x} samus {:04x},{:04x}",
                w(0x0998),
                w(0x079B),
                w(0x0911),
                w(0x0915),
                w(0x0AF6),
                w(0x0AFA)
            );
        }
        println!("BRANCH {i} [{suf}]{line}");
        rig.shot(&Path::new(&dir).join(format!("b{i:02}.png")));
    }
}

/// Explore: `RF_SCRIPT` tokens, a shot + RAM line after each token.
#[test]
#[ignore = "explore"]
fn explore_menu() {
    let Some(mut rig) = Rig::open(SM.0, SM.1, None) else {
        return;
    };
    let script = std::env::var("RF_SCRIPT").unwrap_or_else(|_| "w300".into());
    let dir = std::env::var("RF_SHOTS").unwrap_or_else(|_| "/tmp/sm".into());
    std::fs::create_dir_all(&dir).unwrap();
    for (i, tok) in script.split_whitespace().enumerate() {
        rig.script(tok);
        let r = |a: u32| rig.st.peek_bus(0x7E_0000 + a);
        let w = |a: u32| u32::from(r(a)) | u32::from(r(a + 1)) << 8;
        println!(
            "{i} {tok} f{} gamestate {:04x} room {:04x} area {:04x} cam {:04x},{:04x} samus {:04x},{:04x} hp {}",
            rig.st.frame_count(), w(0x0998), w(0x079B), w(0x079F), w(0x0911), w(0x0915), w(0x0AF6), w(0x0AFA), w(0x09C2)
        );
        rig.shot(&Path::new(&dir).join(format!("{i:03}.png")));
    }
}

/// Free play to the corridor room (two screens wide): down the shaft, through the door.
pub fn to_corridor() -> String {
    format!("{} {SM_SHAFT}", sm_to_free_play())
}

/// In the corridor (the entry door is on the left: never walk left from the entry): right over the step, left a little, six standing jumps, right with jumps, then stand still.
#[test]
#[ignore = "needs Super Metroid"]
fn observe_sm() {
    observe(
        SM,
        &to_corridor(),
        SM_PLAY,
        &[
            0x0911, 0x0912, 0x0915, 0x0916, 0x0AF6, 0x0AF7, 0x0AFA, 0x0AFB, 0x0998, 0x079B,
        ],
    );
}

#[test]
#[ignore = "needs Super Metroid"]
fn discover_sm() {
    let _ = discover_game(SM, &to_corridor(), SM_PLAY, false);
}

use retroforge::scout::{LiveView, ScoutDriver, ScoutPlan, ScoutSetup, ScoutTelemetry};
use std::sync::atomic::Ordering::Relaxed;

/// Scout the machine the rig is at with `plan` through the real driver; the first canvas layer's
/// size afterwards, and the telemetry line.
pub fn scout_dims(
    rig: &mut Rig,
    rom: &[u8],
    plan: ScoutPlan,
    secs: u64,
) -> (usize, usize, String, Option<u64>, u64) {
    let scene = rig.live.current_scene().expect("a scene");
    let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
    let mut driver = ScoutDriver::default();
    driver.set(Some(ScoutSetup {
        plan,
        telemetry: telemetry.clone(),
    }));
    let t0 = std::time::Instant::now();
    // Bounded: `secs` seconds of 10 ms ticks.
    while t0.elapsed() < std::time::Duration::from_secs(secs) {
        driver.tick(LiveView {
            stepper: &rig.st,
            rom,
            canvases: &mut rig.live,
            scene,
            size: (256, 224),
            scrolls: retroforge::core_thread::snes_layer_scrolls,
        });
        if telemetry.finished.load(Relaxed) + telemetry.gave_up.load(Relaxed) >= 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (w, h) = rig.live.scene_canvases(scene.0).map_or((0, 0), |set| {
        (
            set.iter().map(|c| c.width()).max().unwrap_or(0),
            set.iter().map(|c| c.height()).max().unwrap_or(0),
        )
    });
    let dirs: Vec<String> = telemetry
        .last_report
        .lock()
        .ok()
        .and_then(|r| r.clone())
        .map(|r| r.passes.iter().map(|p| format!("{:?}", p.dir)).collect())
        .unwrap_or_default();
    let line = format!(
        "passes {dirs:?}; started {} finished {} gave_up {} aborted {} frames {} cells {} agreement {:?}\u{2030}; ends {:?}; {}",
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
    (
        w,
        h,
        line,
        telemetry.agreement_permille(),
        telemetry.shared_cells.load(Relaxed),
    )
}

/// Explore: the shaft scouted vertically with hand-typed rows (what the finder named).
#[test]
#[ignore = "explore"]
fn explore_shaft() {
    let Some(mut rig) = Rig::open(SM.0, SM.1, None) else {
        return;
    };
    let rom = rig.rom.clone();
    rig.script(&sm_to_free_play());
    rig.script("R40 w60 R40 w60 L40 w60 R40 w60 D10 w60");
    let toml = "[scout]\nstrategy = \"player_move\"\nstep_px = 4\nframe_budget = 3000\nstall_frames = 60\nsettle_frames = 0\ndirs = [\"right\", \"left\", \"down\", \"up\"]\n\
[[scout.addr]]\nrole = \"camera_x\"\naddr = 0x7E00B1\nsource = \"observed: t\"\n\
[[scout.addr]]\nrole = \"camera_y\"\naddr = 0x7E00B3\nsource = \"observed: t\"\n\
[[scout.addr]]\nrole = \"player_x\"\naddr = 0x7E0AF6\nsource = \"observed: t\"\n\
[[scout.addr]]\nrole = \"player_y\"\naddr = 0x7E0AFA\nsource = \"observed: t\"\n\
[[scout.addr]]\nrole = \"player_y_hi\"\naddr = 0x7E0AFB\nsource = \"observed: t\"\n";
    let doc = format!(
        "[meta]\nprofile_version = \"0.1\"\ntitle = \"T\"\nconsole = \"snes\"\nregion = \"ntsc\"\nauthors = [\"t\"]\nsources = []\n[[identity]]\nsha256 = \"{}\"\nrevision = \"t\"\n[capabilities]\nfull_level = false\nhud_separation = false\nentity_overlay = false\nwidescreen = \"decoded\"\n\n{toml}",
        "0".repeat(64)
    );
    let outcome = rf_profiles::load_str(&doc).expect("table");
    let mut plan = ScoutPlan::from_profile(outcome.profile.scout.as_ref().unwrap());
    plan.settle_frames = 0;
    let (w, h, line, _, _) = scout_dims(&mut rig, &rom, plan, 300);
    println!("SHAFT scouted: {w}x{h}; {line}");
}

#[test]
#[ignore = "needs Super Metroid"]
fn find_sm() {
    let Some(mut rig) = Rig::open(SM.0, SM.1, None) else {
        return;
    };
    rig.script(&sm_to_free_play());
    let t0 = std::time::Instant::now();
    let sk = sm_find_table(&mut rig, 600, &mut |_| {});
    describe(&rig.finder.observed());
    println!(
        "find took {:.1}s\n{}",
        t0.elapsed().as_secs_f32(),
        sk.to_toml()
    );
}

/// The acceptance, headless: no hand-made `[scout]` table. One scripted session finds the table; the
/// rooms split into scenes; a scout with the found table fills the corridor at >= 98% agreement.
#[test]
#[ignore = "needs Super Metroid"]
fn sm_acceptance() {
    use retroforge::scout_find::{Out, ScoutFind};
    let Some(mut rig) = Rig::open(SM.0, SM.1, None) else {
        return;
    };
    let rom = rig.rom.clone();
    rig.script(&sm_to_free_play());
    rig.find = Some(ScoutFind::new(true));
    // Scenes against rooms, sampled at the end of every script token.
    let mut seen: Vec<(u64, u16)> = Vec::new();
    let mut sample = |rig: &Rig| {
        if let Some(scene) = rig.last_scene {
            let room =
                u16::from(rig.st.peek_bus(0x7E_079B)) | u16::from(rig.st.peek_bus(0x7E_079C)) << 8;
            seen.push((scene, room));
        }
    };
    for tok in SM_SHAFT
        .split_whitespace()
        .chain(SM_PLAY.split_whitespace())
    {
        rig.script(tok);
        sample(&rig);
    }
    let mut found = None;
    let t0 = std::time::Instant::now();
    // Bounded: ten minutes of idle frames.
    while t0.elapsed() < std::time::Duration::from_secs(600) && found.is_none() {
        rig.script("w30");
        for out in rig.find_out.drain(..) {
            if let Out::Done(r) = out {
                found = Some(r);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let sk = found.expect("an answer").expect("a table, not an error");
    println!("{}", sk.to_toml());
    let addr_of = |role: &str| sk.rows.iter().find(|r| r.role == role).map(|r| r.addr);
    assert_eq!(addr_of("camera_x"), Some(0x7E_00B1));
    assert_eq!(addr_of("camera_y"), Some(0x7E_00B3));
    assert_eq!(addr_of("player_x"), Some(0x7E_0AF6));
    assert_eq!(addr_of("player_y"), Some(0x7E_0AFA));
    assert_eq!(addr_of("player_y_hi"), Some(0x7E_0AFB));
    assert!(addr_of("start_when").is_some(), "no start gate");

    // Rooms are scenes, and no scene spans two rooms.
    let mut by_scene: std::collections::BTreeMap<u64, std::collections::BTreeSet<u16>> =
        std::collections::BTreeMap::new();
    let mut by_room: std::collections::BTreeMap<u16, std::collections::BTreeSet<u64>> =
        std::collections::BTreeMap::new();
    for (scene, room) in &seen {
        by_scene.entry(*scene).or_default().insert(*room);
        by_room.entry(*room).or_default().insert(*scene);
    }
    println!("SCENES by room: {by_room:#x?}");
    println!("ROOMS by scene: {by_scene:#x?}");
    // A door's own scroll is a few frames of the old room's id with the new room's picture: a
    // sampled scene may carry the room it was entered from, never a third.
    assert!(
        by_room.len() >= 2 && by_room.values().all(|s| !s.is_empty()),
        "expected the shaft and the corridor"
    );
    let corridor: std::collections::BTreeSet<u64> =
        by_room.get(&0xDF8D).cloned().unwrap_or_default();
    let shaft: std::collections::BTreeSet<u64> = by_room.get(&0xDF45).cloned().unwrap_or_default();
    assert!(
        corridor.is_disjoint(&shaft),
        "the corridor and the shaft share a scene: {corridor:x?} {shaft:x?}"
    );

    // The scout, with the found table, from the corridor's door.
    let mut rig2 = Rig::open(SM.0, SM.1, None).expect("rig");
    rig2.script(&sm_to_free_play());
    rig2.script(SM_SHAFT);
    rig2.script("w60");
    let mut plan = retroforge::scout_discovery::plan_of(&sk);
    plan.settle_frames = 0;
    let (w, h, line, agreement, shared) = scout_dims(&mut rig2, &rom, plan, 600);
    println!("CORRIDOR scouted: {w}x{h}; agreement {agreement:?} over {shared}; {line}");
    assert!(w >= 512, "the corridor is 512 wide: {w}");
    assert!(
        agreement.is_some_and(|a| a >= 980),
        "agreement {agreement:?}"
    );

    // The shaft, a vertical room: the same table scouts it down and up once the live model has
    // seen the screen scroll that way.
    let mut rig3 = Rig::open(SM.0, SM.1, None).expect("rig");
    rig3.script(&sm_to_free_play());
    rig3.script("R40 w60 R40 w60 L40 w60 R40 w60 D10 w60 L40 w60 L40 w60");
    let mut plan = retroforge::scout_discovery::plan_of(&sk);
    plan.settle_frames = 0;
    let (w, h, line, agreement, shared) = scout_dims(&mut rig3, &rom, plan, 600);
    println!("SHAFT scouted: {w}x{h}; agreement {agreement:?} over {shared}; {line}");
    assert!(h >= 600, "the shaft is three screens tall: {h}");
    assert!(
        agreement.is_some_and(|a| a >= 980),
        "agreement {agreement:?}"
    );
}

/// Explore: the shaft scouted from a saved table (`RF_SK`), with and without its gate.
#[test]
#[ignore = "explore"]
fn explore_shaft_table() {
    let Some(mut rig) = Rig::open(SM.0, SM.1, None) else {
        return;
    };
    let rom = rig.rom.clone();
    let toml = std::fs::read_to_string(std::env::var("RF_SK").expect("RF_SK")).expect("table");
    let doc = format!(
        "[meta]\nprofile_version = \"0.1\"\ntitle = \"T\"\nconsole = \"snes\"\nregion = \"ntsc\"\nauthors = [\"t\"]\nsources = []\n[[identity]]\nsha256 = \"{}\"\nrevision = \"t\"\n[capabilities]\nfull_level = false\nhud_separation = false\nentity_overlay = false\nwidescreen = \"decoded\"\n\n{toml}",
        "0".repeat(64)
    );
    let outcome = rf_profiles::load_str(&doc).expect("table");
    let plan = ScoutPlan::from_profile(outcome.profile.scout.as_ref().unwrap());
    rig.script(&sm_to_free_play());
    rig.script("R40 w60 R40 w60 L40 w60 R40 w60 D10 w60 L40 w60 L40 w60");
    for (a, v) in &plan.start_when {
        println!("GATE {a:#x} wants {v} live {}", rig.st.peek_bus(*a));
    }
    for gated in [true, false] {
        let mut p = plan.clone();
        p.settle_frames = 0;
        if !gated {
            p.start_when.clear();
        }
        let snap = rig.st.snapshot().unwrap();
        let (w, h, line, ag, shared) = scout_dims(&mut rig, &rom, p, 60);
        println!("SHAFT gated {gated}: {w}x{h} agreement {ag:?} over {shared}; {line}");
        rig.st.restore(&snap).unwrap();
    }
}
