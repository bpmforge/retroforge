//! **W35-05: the start gate is the game's mode byte.** ROM-gated; run one at a time, release:
//! `cargo test --release -p retroforge --test w35_05_mode_gate -- --ignored --nocapture <name>`

#[allow(dead_code)]
mod w33_07_common;

use w33_07_common::*;

/// The game a name stands for: its triple, the script to the level and the play.
fn game(name: &str) -> ((&'static str, &'static str, &'static str), String, String) {
    match name {
        "smw" => (SMW, SMW_TO_LEVEL.into(), SMW_PLAY.into()),
        "smb3" => (SMB3, SMB3_TO_LEVEL.into(), SMB3_PLAY.into()),
        "mm2" => (MM2, MM2_TO_LEVEL.into(), MM2_PLAY.into()),
        "mmx" => (MMX, MMX_TO_LEVEL.into(), MMX_PLAY.into()),
        "sm" => (
            SM,
            format!("{} {SM_SHAFT}", sm_to_free_play()),
            SM_PLAY.into(),
        ),
        "cv" => (CV, CV_TO_STAGE.into(), CV_PLAY.into()),
        "alttp" => (ALTTP, alttp_to_free_play(), alttp_play()),
        other => panic!("unknown game {other}"),
    }
}

/// Explore: `RF_GAME`, `RF_SCRIPT` (default: the game's own to-level + play), `RF_PROBES`
/// (hex offsets): after each token the scene and the probe bytes.
#[test]
#[ignore = "explore"]
fn explore_session() {
    let name = std::env::var("RF_GAME").unwrap_or_else(|_| "smw".into());
    let (g, to_level, play) = game(&name);
    let script = std::env::var("RF_SCRIPT").unwrap_or_else(|_| format!("{to_level} {play}"));
    let probes: Vec<u32> = std::env::var("RF_PROBES")
        .unwrap_or_default()
        .split(',')
        .filter_map(|p| u32::from_str_radix(p.trim().trim_start_matches("0x"), 16).ok())
        .collect();
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    let mut last: Option<(Option<u64>, Vec<u8>)> = None;
    for tok in script.split_whitespace() {
        rig.script(tok);
        let vals: Vec<u8> = probes
            .iter()
            .map(|&o| rig.st.peek_bus(if rig.snes { 0x7E_0000 + o } else { o }))
            .collect();
        let now = (rig.last_scene.map(|s| s % 100_000), vals.clone());
        if last.as_ref() != Some(&now) {
            println!(
                "{tok:>8} f{} scene {:?} {:02x?}",
                rig.st.frame_count(),
                now.0,
                vals
            );
        }
        last = Some(now);
    }
}

// ---- the ranking, per game ----------------------------------------------------------------------

/// Run the game's session and print the gate ranking with the values the top bytes take in each
/// state; `RF_EXPECT=0xOFFSET=0xVALUE` makes the first gate an assertion.
fn rank_game(name: &str, expect: Option<(usize, u8)>) {
    let (g, to_level, play) = game(name);
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    rig.script(&to_level);
    rig.script(&play);
    let gates = rig.finder.gates_ranked(6);
    for (i, c) in gates.iter().enumerate() {
        println!(
            "{name} #{i}: {:#06x} = {:#04x} mode {:.3} contrast {:.2} pause apart {:.2}/{} speech apart {:.2}/{} confidence {:.2}",
            c.offset, c.value, c.mode_score, c.contrast, c.freeze_apart, c.frozen_frames,
            c.speech_apart, c.speech_frames, c.confidence
        );
        println!("{}", rig.finder.state_probe(c.offset));
    }
    if let Some((offset, value)) = expect {
        let first = gates.first().expect("a gate");
        assert_eq!((first.offset, first.value), (offset, value), "{gates:?}");
    }
}

#[test]
#[ignore = "needs Super Mario World"]
fn rank_smw() {
    rank_game("smw", Some((0x0100, 0x14)));
}

#[test]
#[ignore = "needs Super Metroid"]
fn rank_sm() {
    rank_game("sm", Some((0x0998, 0x08)));
}

#[test]
#[ignore = "needs Castlevania"]
fn rank_cv() {
    rank_game("cv", Some((0x0018, 0x05)));
}

#[test]
#[ignore = "needs A Link to the Past"]
fn rank_alttp() {
    rank_game("alttp", Some((0x0010, 0x09)));
}

#[test]
#[ignore = "needs Super Mario Bros. 3"]
fn rank_smb3() {
    rank_game("smb3", None);
}

#[test]
#[ignore = "needs Mega Man 2"]
fn rank_mm2() {
    rank_game("mm2", None);
}

#[test]
#[ignore = "needs Mega Man X"]
fn rank_mmx() {
    rank_game("mmx", None);
}

// ---- the scout never starts where the game is not in control -------------------------------------

use retroforge::scout::{LiveView, ScoutDriver, ScoutPlan, ScoutSetup, ScoutTelemetry};
use retroforge::scout_discovery::bus_address;
use std::sync::atomic::Ordering::Relaxed;

/// The `start_when` rows the discovery would write from what the finder saw: the first two gates
/// it is sure of (`scout_discovery` keeps those the scouting run leaves true; a pause or a door
/// does not happen in one).
fn start_rows(rig: &Rig) -> Vec<(u32, u8)> {
    rig.finder
        .observed()
        .gates
        .iter()
        .filter(|g| g.confidence >= 0.7)
        .take(2)
        .map(|g| (bus_address(rig.snes, g.offset), g.value))
        .collect()
}

fn gate_open(rig: &Rig, rows: &[(u32, u8)]) -> bool {
    rows.iter().all(|&(a, v)| rig.st.peek_bus(a) == v)
}

/// A script as one-frame tokens (`R+A3` becomes `R+A1 R+A1 R+A1`).
fn unit_tokens(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in script.split_whitespace() {
        let split = tok.find(|c: char| c.is_ascii_digit()).expect("N");
        let (b, n) = tok.split_at(split);
        for _ in 0..n.parse::<u32>().expect("N") {
            out.push(format!("{b}1"));
        }
    }
    out
}

/// Super Mario World: the level's mode byte holds in play and its pause flag is the second row; with
/// the game paused the scout never starts (150 frames of it), and the moment it is unpaused it does.
#[test]
#[ignore = "needs Super Mario World"]
fn smw_scout_never_starts_while_paused() {
    let (g, to_level, play) = game("smw");
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    rig.script(&to_level);
    rig.script("w60");
    rig.script(&play);
    let rows = start_rows(&rig);
    println!("start_when rows {rows:x?}");
    assert!(
        rows.contains(&(0x7E_0100, 0x14)),
        "the mode byte is a row: {rows:x?}"
    );
    assert!(
        rows.contains(&(0x7E_13D4, 0)),
        "so is the pause flag: {rows:x?}"
    );

    // A second machine at the level's start, with the hand profile's plan and these rows.
    let mut rig = Rig::open(g.0, g.1, Some(g.2)).expect("rig");
    let rom = rig.rom.clone();
    rig.script(&to_level);
    rig.script("w60");
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(g.2);
    let outcome = rf_profiles::load_file(&p).expect("profile");
    let mut plan = ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"));
    plan.settle_frames = 0;
    plan.scene_key = rig.scene_key.clone();
    plan.start_when = rows;
    let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
    let mut driver = ScoutDriver::default();
    driver.set(Some(ScoutSetup {
        plan,
        telemetry: telemetry.clone(),
    }));
    let scene = rig.live.current_scene().expect("a scene");
    let mut tick = |rig: &mut Rig| {
        driver.tick(LiveView {
            stepper: &rig.st,
            rom: &rom,
            canvases: &mut rig.live,
            scene,
            size: (256, 224),
            scrolls: retroforge::core_thread::snes_layer_scrolls,
        });
    };
    // Start: the game pauses. The driver is ticked on every frame, as the core thread does.
    rig.script("S1");
    tick(&mut rig);
    for _ in 0..150 {
        rig.script("w1");
        tick(&mut rig);
        assert_eq!(
            telemetry.started.load(Relaxed),
            0,
            "the scout started in a pause (frame {})",
            rig.st.frame_count()
        );
    }
    // Start again: play resumes and the scout starts after its thirty frames.
    rig.script("S1");
    for _ in 0..120 {
        rig.script("w1");
        tick(&mut rig);
    }
    assert_eq!(
        telemetry.started.load(Relaxed),
        1,
        "and starts once the game runs"
    );
}

/// Super Metroid: the shaft's door (the fade between the two rooms) is not a time to start: the
/// rows are false on the frames the picture is cut (96 % of them or more), and the gate is open
/// for most of the rooms' frames.
#[test]
#[ignore = "needs Super Metroid"]
fn sm_gate_is_shut_in_the_door() {
    let (g, to_level, play) = game("sm");
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    rig.script(&to_level);
    rig.script(&play);
    let rows = start_rows(&rig);
    println!("start_when rows {rows:x?}");
    assert!(rows.contains(&(0x7E_0998, 8)), "{rows:x?}");

    let mut rig = Rig::open(g.0, g.1, Some(g.2)).expect("rig");
    rig.script(&sm_to_free_play());
    let (mut door, mut open_in_door, mut room, mut open_in_room) = (0u32, 0u32, 0u32, 0u32);
    for tok in unit_tokens(SM_SHAFT) {
        rig.script(&tok);
        let open = gate_open(&rig, &rows);
        if rig.last_scene.is_none() {
            door += 1;
            open_in_door += u32::from(open);
        } else {
            room += 1;
            open_in_room += u32::from(open);
        }
    }
    println!("door frames {door}, gate open on {open_in_door}; room frames {room}, open on {open_in_room}");
    assert!(door >= 20, "the shaft's door was not met ({door} frames)");
    assert!(
        open_in_door * 25 <= door,
        "the gate was open on {open_in_door} of {door} door frames"
    );
    assert!(
        open_in_room * 10 >= room * 3,
        "and shut on {} of {room} frames of the rooms",
        room - open_in_room
    );
}

/// A Link to the Past: a speech box holds the pad while the picture stands still; the gate is
/// shut on those frames (95 % of them or more) and open for a good part of the rest.
#[test]
#[ignore = "needs A Link to the Past"]
fn alttp_gate_is_shut_in_a_speech() {
    let (g, to_level, play) = game("alttp");
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    rig.script(&to_level);
    rig.script(&play);
    let rows = start_rows(&rig);
    println!("start_when rows {rows:x?}");
    assert!(rows.contains(&(0x7E_0010, 9)), "{rows:x?}");

    // The same session again, frame by frame: a speech is a direction held on a picture that has
    // not moved for 30 frames (the game ignores the pad).
    let mut rig = Rig::open(g.0, g.1, Some(g.2)).expect("rig");
    let (mut last, mut still, mut held) = (0u64, 0u32, 0u32);
    let (mut speech, mut open_in_speech, mut other, mut open_in_other) = (0u32, 0u32, 0u32, 0u32);
    for tok in unit_tokens(&format!("{to_level} {play}")) {
        rig.script(&tok);
        let hash = rig.shown;
        still = if hash == last { still + 1 } else { 0 };
        last = hash;
        held = if tok.starts_with(['L', 'R', 'U', 'D']) {
            held + 1
        } else {
            0
        };
        let open = gate_open(&rig, &rows);
        if held >= 30 && still >= 30 {
            speech += 1;
            open_in_speech += u32::from(open);
        } else {
            other += 1;
            open_in_other += u32::from(open);
        }
    }
    println!("speech frames {speech}, gate open on {open_in_speech}; other {other}, open on {open_in_other}");
    assert!(speech >= 60, "no speech met ({speech} frames)");
    assert!(
        open_in_speech * 20 <= speech,
        "the gate was open on {open_in_speech} of {speech} frames of a speech"
    );
    // (The boot, the file screen and the story take most of the others.)
    assert!(
        open_in_other * 5 >= other * 2,
        "and open on {open_in_other} of {other} others"
    );
}
