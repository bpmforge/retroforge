//! **W35-12: the shipped Mega Man 2 profile scouts right AND left, and its start gate holds in play.**
//! ROM-gated; run one at a time, release:
//! `cargo test --release -p retroforge --test w35_12_mm2_profile_dirs -- --ignored --nocapture <name>`

#[allow(dead_code)]
mod w33_07_common;

use retroforge::scout::{start_gate_open, ScoutPlan};
use w33_07_common::*;

fn shipped_plan() -> ScoutPlan {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(MM2.2);
    let outcome = rf_profiles::load_file(&path).expect("the profile loads");
    ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"))
}

/// Two screens right of the stage start (Mega Man walked there), then the shipped table's scout
/// (settle 0): the canvas it fills left of the live picture agrees with what a second rig sees
/// when it walks left from the same snapshot, and the shipped gate is open there.
#[test]
#[ignore = "needs Mega Man 2"]
fn mm2_shipped_left_vs_walked() {
    let Some(mut rig) = Rig::open(MM2.0, MM2.1, Some(MM2.2)) else {
        return;
    };
    rig.script(MM2_TO_LEVEL);
    rig.script("w60");
    let plan0 = shipped_plan();
    assert!(
        start_gate_open(&plan0.start_when, |a| rig.st.peek_bus(a)),
        "the shipped gate is shut at the stage start"
    );
    rig.script(&"R25 R+A15 ".repeat(6));
    rig.script("w30");
    println!(
        "mid: health {:x} $36D {:x} $2A {:x}",
        rig.st.peek_bus(0x6C0),
        rig.st.peek_bus(0x36D),
        rig.st.peek_bus(0x2A)
    );
    assert!(
        start_gate_open(&plan0.start_when, |a| rig.st.peek_bus(a)),
        "the shipped gate is shut mid-stage"
    );
    let mid = rig.st.snapshot().expect("snapshot");
    let mut plan = shipped_plan();
    plan.settle_frames = 0;
    plan.scene_key = rig.scene_key.clone();
    let rom = rig.rom.clone();
    rig.reset();
    rig.script("w60");
    println!(
        "before scout: {:?} gate {}",
        plan.start_when,
        start_gate_open(&plan.start_when, |a| rig.st.peek_bus(a))
    );
    let (w, line) = scout_width(&mut rig, &rom, plan, 120);
    println!("scouted width {w}; {line}");
    assert!(
        line.contains("passes [\"Right\", \"Left\"]"),
        "both passes ran: {line}"
    );
    let scene = rig.live.current_scene().expect("scene");
    let scouted = rig.live.scene_canvases(scene.0).expect("set")[0].clone();
    // The game's own: from the same snapshot, walk left (Mega Man stays alive on the floor).
    let mut truth = Rig::open(MM2.0, MM2.1, Some(MM2.2)).expect("rig");
    truth.st.restore(&mid).expect("restore");
    truth.reset();
    truth.script("w60");
    truth.script(&"L25 ".repeat(30));
    truth.script("w60");
    let tscene = truth.live.current_scene().expect("scene");
    let walked = truth.live.scene_canvases(tscene.0).expect("set")[0].clone();
    let (wx0, wy0) = walked.origin();
    let (mut both, mut agree, mut walked_only) = (0u64, 0u64, 0u64);
    for y in wy0..wy0 + walked.height() as i64 {
        for x in wx0..wx0 + walked.width() as i64 {
            match (walked.get(x, y), scouted.get(x, y)) {
                (Some(a), Some(b)) => {
                    both += 1;
                    agree += u64::from(a.palette_index == b.palette_index);
                }
                (Some(_), None) => walked_only += 1,
                _ => {}
            }
        }
    }
    println!(
        "walked {}x{} origin {:?}; scouted {}x{} origin {:?}; LEFT both {both} agree {agree} ({:.2}%) walked-only {walked_only}",
        walked.width(), walked.height(), walked.origin(),
        scouted.width(), scouted.height(), scouted.origin(),
        100.0 * agree as f64 / both.max(1) as f64
    );
    assert!(
        both >= 30_000,
        "the walked left side is known to both: {both}"
    );
    assert!(agree * 100 >= both * 98, "{agree} of {both}");
}

/// The gate is open on stage 1 and stage 2 play, shut at the title and the menus.
#[test]
#[ignore = "needs Mega Man 2"]
fn mm2_shipped_gate_stages() {
    let Some(mut rig) = Rig::open(MM2.0, MM2.1, Some(MM2.2)) else {
        return;
    };
    let rows = shipped_plan().start_when;
    let open = |rig: &Rig| start_gate_open(&rows, |a| rig.st.peek_bus(a));
    rig.script("w400");
    assert!(!open(&rig), "open on the title");
    rig.script("S6 w120 S6 w300");
    assert!(!open(&rig), "open on the menus");
    rig.script("S6 w300 U8 w20 S6 w800 w60");
    assert!(open(&rig), "shut in stage 1");
    let mut rig2 = Rig::open(MM2.0, MM2.1, Some(MM2.2)).expect("rig");
    rig2.script("w400 S6 w120 S6 w300 S6 w300 R8 w20 S6 w800 w60");
    assert!(open(&rig2), "shut in stage 2");
}

/// `frames N` and `cells N` out of a `scout_width` line.
fn field(line: &str, key: &str) -> u64 {
    let rest = line.split(&format!("{key} ")).nth(1).expect(key);
    rest.split_whitespace().next().unwrap().parse().expect(key)
}

/// From the stage's start the camera is at its left stop: the Left pass of the shipped table ends
/// cleanly (stalled, nothing added) and costs at most one stall window of extra frames.
#[test]
#[ignore = "needs Mega Man 2"]
fn mm2_shipped_left_at_stage_start() {
    let run = |dirs: &[retroforge::scout::ScoutDir]| {
        let mut rig = Rig::open(MM2.0, MM2.1, Some(MM2.2))?;
        rig.script(MM2_TO_LEVEL);
        rig.script("w90");
        let mut plan = shipped_plan();
        plan.settle_frames = 0;
        plan.scene_key = rig.scene_key.clone();
        plan.dirs = dirs.to_vec();
        let rom = rig.rom.clone();
        Some(scout_width(&mut rig, &rom, plan, 120))
    };
    use retroforge::scout::ScoutDir::{Left, Right};
    let (Some((w1, right)), Some((w2, both))) = (run(&[Right]), run(&[Right, Left])) else {
        return;
    };
    println!("RIGHT ONLY width {w1}: {right}\nBOTH       width {w2}: {both}");
    assert!(both.contains("passes [\"Right\", \"Left\"]"), "{both}");
    assert!(both.contains("passes ended [Stalled, Stalled]"), "{both}");
    assert_eq!(w1, w2, "the Left pass grew the canvas from the stage start");
    assert_eq!(
        field(&right, "cells"),
        field(&both, "cells"),
        "cells merged"
    );
    let extra = field(&both, "frames") - field(&right, "frames");
    println!("LEFT PASS from the stage start cost {extra} scout frames");
    assert!(extra <= 120, "{extra} extra scout frames");
}
