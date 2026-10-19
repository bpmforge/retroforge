//! **W35-03: Castlevania (NES), settings found by observation.** ROM-gated; run one at a time, release:
//! `cargo test --release -p retroforge --test w35_03_castlevania -- --ignored --nocapture <name>`

#[allow(dead_code)]
mod w33_07_common;

use std::path::Path;
use w33_07_common::*;

/// Explore: `RF_SCRIPT` tokens, a shot + RAM line after each token.
#[test]
#[ignore = "explore"]
fn explore_menu() {
    let Some(mut rig) = Rig::open(CV.0, CV.1, None) else {
        return;
    };
    let script = std::env::var("RF_SCRIPT").unwrap_or_else(|_| "w300".into());
    let dir = std::env::var("RF_SHOTS").unwrap_or_else(|_| "/tmp/cv".into());
    std::fs::create_dir_all(&dir).unwrap();
    for (i, tok) in script.split_whitespace().enumerate() {
        rig.script(tok);
        let r = |a: u32| rig.st.peek_bus(a);
        println!(
            "{i} {tok} f{} scene {:?} stage {:02x} cam {:02x} px {:02x}{:02x} py {:02x} hp {:02x} lives {:02x} 0x18 {:02x} mode? {:02x}/{:02x}",
            rig.st.frame_count(), rig.last_scene.map(|s| s % 10000), r(0x28), r(0xFD), r(0x41), r(0x40), r(0x3F), r(0x45), r(0x2A), r(0x18), r(0x12), r(0x13)
        );
        rig.shot(&Path::new(&dir).join(format!("{i:03}.png")));
    }
}

#[test]
#[ignore = "needs Castlevania"]
fn observe_cv() {
    observe(
        CV,
        CV_TO_STAGE,
        CV_PLAY,
        &[0xFD, 0x40, 0x41, 0x3F, 0x28, 0x18, 0x45],
    );
}

#[test]
#[ignore = "needs Castlevania"]
fn discover_cv() {
    let _ = discover_game(CV, CV_TO_STAGE, CV_PLAY, false);
}

/// Explore: the scroll writes of a few frames of walking.
#[test]
#[ignore = "explore"]
fn explore_scroll() {
    let Some(mut rig) = Rig::open(CV.0, CV.1, None) else {
        return;
    };
    rig.script(CV_TO_STAGE);
    rig.script("R90");
    for _ in 0..8 {
        rig.script("R1");
        println!(
            "f{} cam2e {:02x} writes {:02x?}",
            rig.st.frame_count(),
            rig.st.peek_bus(0x2E),
            rig.last_scroll
        );
    }
}

/// Explore: the found table scouted with one thing changed at a time.
#[test]
#[ignore = "explore"]
fn explore_variants() {
    let Some((sk, _, _)) = discover_game(CV, CV_TO_STAGE, CV_PLAY, false) else {
        return;
    };
    for variant in ["asis", "nogate", "stall90", "pin", "nohold"] {
        let mut rig = Rig::open(CV.0, CV.1, None).expect("rig");
        let rom = rig.rom.clone();
        rig.script(CV_TO_STAGE);
        rig.script("w60");
        let mut plan = retroforge::scout_discovery::plan_of(&sk);
        plan.settle_frames = 0;
        match variant {
            "nogate" => plan.start_when.clear(),
            "stall90" => plan.stall_frames = 90,
            "pin" => plan.pin = true,
            "nohold" => plan.hold.clear(),
            _ => {}
        }
        let (w, line) = scout_width(&mut rig, &rom, plan, 300);
        println!(
            "VARIANT {variant}: width {w}; {}",
            &line[..line.len().min(400)]
        );
    }
}

/// Explore: scout the found table from the middle of block 1, width of the canvases.
#[test]
#[ignore = "explore"]
fn explore_mid() {
    let Some((sk, _, _)) = discover_game(CV, CV_TO_STAGE, CV_PLAY, false) else {
        return;
    };
    let mut rig = Rig::open(CV.0, CV.1, None).expect("rig");
    let rom = rig.rom.clone();
    rig.script(CV_TO_STAGE);
    rig.script("w60");
    rig.script(CV_PLAY_HEAD);
    rig.script("w60");
    let mut plan = retroforge::scout_discovery::plan_of(&sk);
    plan.settle_frames = 0;
    let (w, line) = scout_width(&mut rig, &rom, plan, 300);
    println!("MID: width {w}; {}", &line[..line.len().min(300)]);
}

/// Explore: a Left-only scout from the middle of block 1, stepped by hand.
#[test]
#[ignore = "explore"]
fn explore_left() {
    use retroforge::scout::{Scout, ScoutDir};
    use rf_enhance::scene_identity::SceneId;
    let Some((sk, _, _)) = discover_game(CV, CV_TO_STAGE, CV_PLAY, false) else {
        return;
    };
    let mut rig = Rig::open(CV.0, CV.1, None).expect("rig");
    let rom = rig.rom.clone();
    rig.script(CV_TO_STAGE);
    rig.script("w60");
    rig.script(CV_PLAY_HEAD);
    rig.script("w60");
    let snap = rig.st.snapshot().expect("snap");
    let seed = rig.live.fork_for_scout();
    let scene = SceneId(rig.last_scene.expect("scene"));
    let mut plan = retroforge::scout_discovery::plan_of(&sk);
    plan.settle_frames = 0;
    let which = std::env::var("RF_DIR").unwrap_or_default();
    plan.dirs = vec![if which == "right" {
        ScoutDir::Right
    } else {
        ScoutDir::Left
    }];
    if std::env::var("RF_NOHOLD").is_ok() {
        plan.hold.clear();
    }
    let mut scout = Scout::start(&rom, snap, plan, &seed, scene, (256, 240)).expect("scout");
    for i in 0..300 {
        let st = scout.step(&no_scrolls);
        if i % 20 == 0 || i < 12 {
            println!(
                "LEFT {i}: {st:?} px {:04x} py {:02x} cam {:02x}{:02x} fd {:02x} extent {}",
                scout.peek_word(0x40),
                scout.peek_word(0x3F) & 0xFF,
                scout.peek_word(0x2F) & 0xFF,
                scout.peek_word(0x2E) & 0xFF,
                scout.peek_word(0xFD) & 0xFF,
                scout.extent()
            );
        }
    }
}

/// Explore: which bytes DRIVE Simon. From the middle of block 1: the bytes that stepped +1 on
/// most frames of a Right walk; each is poked +24 and the screen position of Simon read back.
#[test]
#[ignore = "explore"]
fn explore_drivers() {
    let Some(mut rig) = Rig::open(CV.0, CV.1, None) else {
        return;
    };
    rig.script(CV_TO_STAGE);
    rig.script("w60");
    rig.script(CV_PLAY_HEAD);
    rig.script("w60");
    let snap = rig.st.snapshot().expect("snap");
    // Frame-by-frame changes during a walk.
    let mut prev: Vec<u8> = (0..0x800u32).map(|a| rig.st.peek_bus(a)).collect();
    let mut plus1 = vec![0u32; 0x800];
    for _ in 0..40 {
        rig.script("R1");
        for a in 0..0x800usize {
            let now = rig.st.peek_bus(a as u32);
            if now.wrapping_sub(prev[a]) == 1 {
                plus1[a] += 1;
            }
            prev[a] = now;
        }
    }
    let cands: Vec<usize> = (0..0x800).filter(|&a| plus1[a] >= 25).collect();
    println!("DRIVERS candidates (+1 on >= 25 of 40 frames): {cands:#x?}");
    for &c in &cands {
        rig.st.restore(&snap).unwrap();
        let before = rig.st.peek_bus(0x40);
        let v = rig.st.peek_bus(c as u32);
        for _ in 0..3 {
            rig.st.poke_ram(c as u32, v.wrapping_add(24));
            rig.frame(0);
        }
        let after = rig.st.peek_bus(0x40);
        let cam = rig.st.peek_bus(0xFD);
        println!(
            "DRIVERS poke {c:#05x} +24: $40 {before:#04x} -> {after:#04x}, cam $FD {cam:#04x}"
        );
    }
}

/// Explore: poke every work-RAM byte +24 from the middle of block 1 and report those after which
/// the camera ($2E) moves by 8 or more within 6 frames (the bytes the camera follows).
#[test]
#[ignore = "explore"]
fn explore_followed() {
    let Some(mut rig) = Rig::open(CV.0, CV.1, None) else {
        return;
    };
    rig.script(CV_TO_STAGE);
    rig.script("w60");
    rig.script(CV_PLAY_HEAD);
    rig.script("w60");
    let snap = rig.st.snapshot().expect("snap");
    let delta = std::env::var("RF_DELTA")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(24);
    for a in 0..0x800u32 {
        rig.st.restore(&snap).unwrap();
        let cam0 = rig.st.peek_bus(0x2E);
        let v = rig.st.peek_bus(a);
        rig.st.poke_ram(a, v.wrapping_add(delta));
        for _ in 0..6 {
            rig.frame(0);
        }
        let cam = rig.st.peek_bus(0x2E);
        let d = cam.wrapping_sub(cam0);
        if (3..=0x80).contains(&d) && a != 0x2E && a != 0xFD {
            println!(
                "FOLLOWED poke {a:#05x} {v:#04x}+{delta}: cam {cam0:#04x} -> {cam:#04x}, $40 {:#04x}",
                rig.st.peek_bus(0x40)
            );
        }
    }
}

/// The acceptance, headless: no hand-made `[scout]` table. One scripted session finds the table
/// (the core thread's `ScoutFind`), the courtyard and the entrance hall are separate scenes, and a
/// scout with the found table fills the courtyard (3 screens) at >= 98% agreement.
#[test]
#[ignore = "needs Castlevania"]
fn cv_acceptance() {
    use retroforge::scout_find::{Out, ScoutFind};
    use std::collections::{BTreeMap, BTreeSet};
    let Some(mut rig) = Rig::open(CV.0, CV.1, None) else {
        return;
    };
    let rom = rig.rom.clone();
    rig.script(CV_TO_STAGE);
    rig.script("w60");
    rig.find = Some(ScoutFind::new(false));
    // Scenes against the game's block counter ($28: 0 the courtyard, 1 the entrance hall).
    let mut by_block: BTreeMap<u8, BTreeSet<u64>> = BTreeMap::new();
    for tok in CV_PLAY_HEAD
        .split_whitespace()
        .chain(CV_PLAY_TAIL.split_whitespace())
    {
        rig.script(tok);
        if let Some(scene) = rig.last_scene {
            by_block
                .entry(rig.st.peek_bus(0x28))
                .or_default()
                .insert(scene);
        }
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
    println!("SCENES by block: {by_block:#x?}");
    assert!(by_block.len() >= 2, "the session met both blocks");
    let courtyard = by_block.get(&0).cloned().unwrap_or_default();
    let hall = by_block.get(&1).cloned().unwrap_or_default();
    assert!(
        !courtyard.is_empty() && !hall.is_empty() && courtyard.is_disjoint(&hall),
        "the door is not a scene cut: {courtyard:x?} {hall:x?}"
    );
    // The scout, with the found table, from the start of the courtyard.
    let mut rig2 = Rig::open(CV.0, CV.1, None).expect("rig");
    rig2.script(CV_TO_STAGE);
    rig2.script("w60");
    let mut plan = retroforge::scout_discovery::plan_of(&sk);
    plan.settle_frames = 0;
    let (w, line) = scout_width(&mut rig2, &rom, plan, 600);
    println!("COURTYARD scouted: width {w}; {line}");
    assert!(w >= 768, "the courtyard is 3 screens wide: {w}");
    assert!(
        line.contains("agreement Some(99") || line.contains("agreement Some(1000"),
        "agreement under 98%: {line}"
    );
}
