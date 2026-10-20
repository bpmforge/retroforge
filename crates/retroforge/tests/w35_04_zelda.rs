//! **W35-04: Zelda (A Link to the Past, SNES; Legend of Zelda, NES), settings found by observation.**
//! ROM-gated; run one at a time, release:
//! `cargo test --release -p retroforge --test w35_04_zelda -- --ignored --nocapture <name>`

#[allow(dead_code)]
mod w33_07_common;

use std::path::Path;
use w33_07_common::*;

/// Explore: `RF_SCRIPT` tokens, a shot + RAM line after each token.
#[test]
#[ignore = "explore"]
fn explore_menu() {
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    let script = std::env::var("RF_SCRIPT").unwrap_or_else(|_| "w300".into());
    let dir = std::env::var("RF_SHOTS").unwrap_or_else(|_| "/tmp/alttp".into());
    std::fs::create_dir_all(&dir).unwrap();
    for (i, tok) in script.split_whitespace().enumerate() {
        rig.script(tok);
        let r = |a: u32| rig.st.peek_bus(0x7E_0000 + a);
        println!(
            "{i} {tok} f{} scene {:?} 10:{:02x} 11:{:02x} 1b:{:02x} 1a:{:02x} 22:{:02x}{:02x} 20:{:02x}{:02x}",
            rig.st.frame_count(), rig.last_scene.map(|s| s % 10000), r(0x10), r(0x11), r(0x1B), r(0x1A), r(0x23), r(0x22), r(0x21), r(0x20)
        );
        let v = rig.st.bg_layer_views().expect("snes");
        println!(
            "   bg1 {:04x},{:04x} bg2 {:04x},{:04x}",
            v[0].hofs, v[0].vofs, v[1].hofs, v[1].vofs
        );
        rig.shot(&Path::new(&dir).join(format!("{i:03}.png")));
    }
}

/// Explore: from free play, each `;`-separated script of `RF_SCRIPTS` from the same snapshot, a
/// line with where Link ended and the camera layers.
#[test]
#[ignore = "explore"]
fn explore_paths() {
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    rig.script(&alttp_to_free_play());
    let snap = rig.st.snapshot().expect("snap");
    let dir = std::env::var("RF_SHOTS").unwrap_or_else(|_| "/tmp/alttp".into());
    std::fs::create_dir_all(&dir).unwrap();
    let scripts = std::env::var("RF_SCRIPTS").unwrap_or_default();
    for (i, text) in scripts.split(';').enumerate() {
        rig.st.restore(&snap).unwrap();
        rig.script(text);
        let r = |a: u32| rig.st.peek_bus(0x7E_0000 + a);
        let v = rig.st.bg_layer_views().expect("snes");
        println!(
            "PATH {i} [{text}] f{} link x {:02x}{:02x} y {:02x}{:02x} 10:{:02x} bg2 {:04x},{:04x}",
            rig.st.frame_count(),
            r(0x23),
            r(0x22),
            r(0x21),
            r(0x20),
            r(0x10),
            v[1].hofs,
            v[1].vofs
        );
        rig.shot(&Path::new(&dir).join(format!("p{i:02}.png")));
    }
}

#[test]
#[ignore = "needs A Link to the Past"]
fn observe_alttp() {
    observe(
        ALTTP,
        &alttp_to_free_play(),
        &alttp_play(),
        &[
            0x20, 0x21, 0x22, 0x23, 0x10, 0x11, 0x1B, 0xE0, 0xE1, 0xE2, 0xE3,
        ],
    );
}

/// Explore: per-frame series for `RF_SCRIPT` after free play (L/R/U/D tokens one frame at a time).
#[test]
#[ignore = "explore"]
fn explore_series() {
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    rig.script(&alttp_to_free_play());
    rig.script("D80");
    let v = rig.st.bg_layer_views().expect("snes");
    for (i, l) in v.iter().enumerate() {
        println!(
            "LAYER {i}: enabled {} map {}x{}",
            l.enabled, l.tilemap_width, l.tilemap_height
        );
    }
    let script = std::env::var("RF_SCRIPT").unwrap_or_else(|_| "L1".into());
    for _ in 0..std::env::var("RF_N")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(40)
    {
        rig.script(&script);
        let r = |a: u32| {
            u16::from(rig.st.peek_bus(0x7E_0000 + a))
                | u16::from(rig.st.peek_bus(0x7E_0001 + a)) << 8
        };
        let v = rig.st.bg_layer_views().expect("snes");
        println!(
            "S f{} link {:04x},{:04x} bg1 {:04x},{:04x} bg2 {:04x},{:04x} e0 {:04x} e2 {:04x} en {}{}{}{} 120 {:04x}",
            rig.st.frame_count(), r(0x22), r(0x20), v[0].hofs, v[0].vofs, v[1].hofs, v[1].vofs, r(0xE0), r(0xE2), u8::from(v[0].enabled), u8::from(v[1].enabled), u8::from(v[2].enabled), u8::from(v[3].enabled), r(0x120)
        );
    }
}

/// Explore: the byte at `RF_OFF` against the finder's camera delta (the first fed layer whose x
/// changed) from the house door on; every frame the camera moved at a walk's pace (<= 4) and the
/// byte did not follow is printed with its token.
#[test]
#[ignore = "explore"]
fn explore_session_misses() {
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    let pre = alttp_to_free_play();
    let at = pre.find("R32").unwrap();
    rig.script(&pre[..at]);
    let off: u32 = std::env::var("RF_OFF")
        .ok()
        .and_then(|v| u32::from_str_radix(&v, 16).ok())
        .unwrap_or(0x22);
    let axis_y = std::env::var("RF_AXIS").is_ok_and(|v| v == "y");
    let cam = |rig: &Rig| -> Vec<Option<u8>> {
        retroforge::scout_find::finder_snes_scroll(&rig.st)
            .unwrap()
            .iter()
            .map(|(x, y)| {
                if axis_y {
                    y.last().copied()
                } else {
                    x.last().copied()
                }
            })
            .collect()
    };
    let mut prev_reg = cam(&rig);
    let mut prev_b = rig.st.peek_bus(0x7E_0000 + off);
    let (mut slow, mut hit) = (0, 0);
    let play = if std::env::var_os("RF_ROAD").is_some() {
        alttp_play_road()
    } else {
        alttp_play()
    };
    let toks: Vec<String> = pre[at..]
        .split_whitespace()
        .chain(play.split_whitespace())
        .map(String::from)
        .collect();
    for tok in &toks {
        let split = tok.find(|c: char| c.is_ascii_digit()).unwrap();
        let (b, n) = tok.split_at(split);
        let n: u32 = n.parse().unwrap();
        let one = format!("{b}1");
        let mut misses = Vec::new();
        for _ in 0..n {
            rig.script(&one);
            let reg = cam(&rig);
            let v = rig.st.peek_bus(0x7E_0000 + off);
            let cd = reg
                .iter()
                .zip(&prev_reg)
                .find_map(|(n, p)| match (n, p) {
                    (Some(n), Some(p)) if n != p => Some(n.wrapping_sub(*p) as i8),
                    _ => None,
                })
                .unwrap_or(0);
            let d = v.wrapping_sub(prev_b) as i8;
            if cd != 0 && cd.abs() <= 4 {
                slow += 1;
                if d == cd {
                    hit += 1;
                } else {
                    misses.push(format!("f{} cd{cd} d{d}", rig.st.frame_count()));
                }
            }
            prev_reg = reg;
            prev_b = v;
        }
        println!(
            "TOK {tok}: mode {:02x}/{:02x} link {:02x}{:02x} scene {:?} misses {} {:?}",
            rig.st.peek_bus(0x7E_0010),
            rig.st.peek_bus(0x7E_0011),
            rig.st.peek_bus(0x7E_0023),
            rig.st.peek_bus(0x7E_0022),
            rig.last_scene.map(|s| s % 1000),
            misses.len(),
            &misses[..misses.len().min(2)]
        );
    }
    println!("SESSION slow {slow} hit {hit}");
}

#[test]
#[ignore = "needs A Link to the Past"]
fn discover_alttp() {
    let to_road = alttp_to_free_play() + " D80 w30";
    let _ = discover_game(ALTTP, &to_road, &alttp_play_road(), false);
}

/// Explore: one trial scout stepped by hand from the road, extent and width every 10 frames.
#[test]
#[ignore = "explore"]
fn explore_trial() {
    use retroforge::scout::{Scout, ScoutDir, ScoutPlan, ScoutStatus, ScoutStrategy};
    use rf_enhance::scene_identity::SceneId;
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    let rom = rig.rom.clone();
    rig.script(&alttp_to_free_play());
    if std::env::var_os("RF_END").is_some() {
        rig.script(&alttp_play());
    } else {
        rig.script("D80 w60");
    }
    println!(
        "START link {:04x},{:04x} bg2 {:04x},{:04x} frame {}",
        rig.st.peek_bus(0x7E_0022) as u16 | (rig.st.peek_bus(0x7E_0023) as u16) << 8,
        rig.st.peek_bus(0x7E_0020) as u16 | (rig.st.peek_bus(0x7E_0021) as u16) << 8,
        rig.st.bg_layer_views().unwrap()[1].hofs,
        rig.st.bg_layer_views().unwrap()[1].vofs,
        rig.st.frame_count()
    );
    println!(
        "MODE 10 {:02x} 11 {:02x} 1b {:02x}",
        rig.st.peek_bus(0x7E_0010),
        rig.st.peek_bus(0x7E_0011),
        rig.st.peek_bus(0x7E_001B)
    );
    rig.shot(Path::new("/private/tmp/claude-502/-Users-bmatthews-Code/45f96156-bb1b-4b20-b97d-bbd589d3a745/scratchpad/end.png"));
    let snap = rig.st.snapshot().expect("snap");
    let seed = rig.live.fork_for_scout();
    let scene = SceneId(rig.last_scene.expect("scene"));
    let dir = match std::env::var("RF_DIR").as_deref() {
        Ok("left") => ScoutDir::Left,
        Ok("down") => ScoutDir::Down,
        Ok("up") => ScoutDir::Up,
        _ => ScoutDir::Right,
    };
    let plan = ScoutPlan {
        strategy: ScoutStrategy::PlayerMove,
        camera: vec![0x7E_011E],
        player_x: vec![0x7E_0022],
        player_x_hi: vec![0x7E_0023],
        camera_y: vec![0x7E_0122],
        player_y: vec![0x7E_0020],
        player_y_hi: vec![0x7E_0021],
        step_px: std::env::var("RF_STEP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4),
        frame_budget: 300,
        stall_frames: 90,
        settle_frames: 0,
        dirs: vec![dir],
        hold: Vec::new(),
        start_when: Vec::new(),
        pin: true,
        scene_key: None,
    };
    let mut scout = Scout::start(&rom, snap, plan, &seed, scene, (256, 224)).expect("scout");
    for i in 0..300 {
        let st = scout.step(&retroforge::core_thread::snes_layer_scrolls);
        if i % 10 == 0 || matches!(st, ScoutStatus::Finished(_)) {
            println!(
                "TR {i}: {st:?} extent {} width {} layers {:?} link {:04x},{:04x} cam {:04x},{:04x} e2 {:04x}",
                scout.extent(),
                scout.width(),
                scout.layer_dims(),
                scout.peek_word(0x7E_0022),
                scout.peek_word(0x7E_0020),
                scout.peek_word(0x7E_011E),
                scout.peek_word(0x7E_0122),
                scout.peek_word(0x7E_00E2)
            );
        }
        if matches!(st, ScoutStatus::Finished(_)) {
            break;
        }
    }
}

/// Explore: the live model's canvases (per layer: width x height) along the session.
#[test]
#[ignore = "explore"]
fn explore_live() {
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    rig.script(&alttp_to_free_play());
    let dims = |rig: &Rig| -> Vec<String> {
        rig.live
            .current_scene()
            .and_then(|s| rig.live.scene_canvases(s.0))
            .map(|set| {
                set.iter()
                    .map(|c| format!("{}x{}", c.width(), c.height()))
                    .collect()
            })
            .unwrap_or_default()
    };
    println!("LIVE at spawn: {:?}", dims(&rig));
    for tok in ["w60", "U60"] {
        rig.script(tok);
    }
    for i in 0..30 {
        rig.script("w10");
        println!(
            "LIVE f{} +{}: {:?}",
            rig.st.frame_count(),
            i * 10,
            dims(&rig)
        );
    }
    rig.script("D60");
    for i in 0..30 {
        rig.script("w10");
        println!(
            "LIVE f{} out +{}: {:?}",
            rig.st.frame_count(),
            i * 10,
            dims(&rig)
        );
    }
}

#[test]
#[ignore = "needs A Link to the Past"]
fn observe_alttp_road() {
    let to_road = alttp_to_free_play() + " D80 w30";
    observe(
        ALTTP,
        &to_road,
        &alttp_play_road(),
        &[0x20, 0x21, 0x22, 0x23, 0x1B],
    );
}

/// The acceptance, headless: no hand-made `[scout]` table. One scripted session outside Link's
/// house (the core thread's `ScoutFind`) finds the table, the house and the road are separate
/// scenes, and a scout with the found table fills the road at >= 98% agreement.
#[test]
#[ignore = "needs A Link to the Past"]
fn alttp_acceptance() {
    use std::collections::{BTreeMap, BTreeSet};
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    let rom = rig.rom.clone();
    rig.script(&alttp_to_free_play());
    // Scenes against the game's indoors flag (the finder's top gate candidate: 1 inside).
    let mut by_flag: BTreeMap<u8, BTreeSet<u64>> = BTreeMap::new();
    let flag_at = |rig: &Rig| rig.st.peek_bus(0x7E_001B);
    rig.find = Some(retroforge::scout_find::ScoutFind::new(true));
    let play = alttp_play();
    for tok in play.split_whitespace() {
        rig.script(tok);
        if let Some(scene) = rig.last_scene {
            by_flag.entry(flag_at(&rig)).or_default().insert(scene);
        }
    }
    let mut found = None;
    let t0 = std::time::Instant::now();
    // Bounded: ten minutes of idle frames.
    while t0.elapsed() < std::time::Duration::from_secs(600) && found.is_none() {
        rig.script("w30");
        for out in rig.find_out.drain(..) {
            // The app takes the first answer (the finder goes on running in this rig).
            if let retroforge::scout_find::Out::Done(r) = out {
                found.get_or_insert(r);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let sk = found.expect("an answer").expect("a table, not an error");
    println!("{}", sk.to_toml());
    println!("SCENES by indoors flag: {by_flag:#x?}");
    let indoors = by_flag.get(&1).cloned().unwrap_or_default();
    let outdoors = by_flag.get(&0).cloned().unwrap_or_default();
    assert!(
        !indoors.is_empty() && !outdoors.is_empty() && indoors.is_disjoint(&outdoors),
        "the door is not a scene cut: {indoors:x?} {outdoors:x?}"
    );
    // The scout, with the found table, from the road.
    let mut rig2 = Rig::open(ALTTP.0, ALTTP.1, None).expect("rig");
    rig2.script(&alttp_to_free_play());
    rig2.script("D80 L120 w60");
    let mut plan = retroforge::scout_discovery::plan_of(&sk);
    plan.settle_frames = 0;
    let (w, line) = scout_width(&mut rig2, &rom, plan, 600);
    println!("ROAD scouted: width {w}; {line}");
    assert!(
        w >= 512,
        "the road scouted to two screens wide at least: {w}"
    );
    assert!(
        line.contains("agreement Some(99") || line.contains("agreement Some(1000"),
        "agreement under 98%: {line}"
    );
}

/// Explore (NES Zelda): `RF_SCRIPT` tokens, a shot + RAM line after each token.
#[test]
#[ignore = "explore"]
fn explore_z1() {
    let Some(mut rig) = Rig::open(ZELDA1.0, ZELDA1.1, None) else {
        return;
    };
    let script = std::env::var("RF_SCRIPT").unwrap_or_else(|_| "w300".into());
    let dir = std::env::var("RF_SHOTS").unwrap_or_else(|_| "/tmp/z1".into());
    std::fs::create_dir_all(&dir).unwrap();
    for (i, tok) in script.split_whitespace().enumerate() {
        rig.script(tok);
        let r = |a: u32| rig.st.peek_bus(a);
        println!(
            "{i} {tok} f{} scene {:?} level {:02x} map {:02x} x {:02x} y {:02x} 12 {:02x} 13 {:02x} 11 {:02x}",
            rig.st.frame_count(), rig.last_scene.map(|s| s % 10000), r(0x10), r(0xEB), r(0x70), r(0x84), r(0x12), r(0x13), r(0x11)
        );
        rig.shot(&Path::new(&dir).join(format!("{i:03}.png")));
    }
}

/// Explore (NES Zelda): from the name screen with an `A` typed, each `;`-separated script of
/// `RF_SCRIPTS`, then the mode bytes and a shot.
#[test]
#[ignore = "explore"]
fn explore_z1_try() {
    let Some(mut rig) = Rig::open(ZELDA1.0, ZELDA1.1, None) else {
        return;
    };
    rig.script("w300 S6 w300 S6 w300 A6 w30");
    let snap = rig.st.snapshot().expect("snap");
    let dir = std::env::var("RF_SHOTS").unwrap_or_else(|_| "/tmp/z1t".into());
    std::fs::create_dir_all(&dir).unwrap();
    for (i, text) in std::env::var("RF_SCRIPTS")
        .unwrap_or_default()
        .split(';')
        .enumerate()
    {
        rig.st.restore(&snap).unwrap();
        rig.script(text);
        let r = |a: u32| rig.st.peek_bus(a);
        println!(
            "TRY {i} [{text}] f{} 12 {:02x} 13 {:02x} level {:02x} x {:02x} y {:02x}",
            rig.st.frame_count(),
            r(0x12),
            r(0x13),
            r(0x10),
            r(0x70),
            r(0x84)
        );
        rig.shot(&Path::new(&dir).join(format!("t{i:02}.png")));
    }
}

/// Walk across the first screen's right and left edges, back and forth.
const Z1_PLAY: &str = "R60 R60 R60 w100 L60 L60 L60 L60 w100 R60 R60 R60 w100 L60 L60 L60 L60 w100 R60 R60 R60 w100 L60 L60 L60 L60 w100 R60 R60 R60 w100 L60 L60 L60 L60 w100";

#[test]
#[ignore = "needs The Legend of Zelda"]
fn observe_z1() {
    observe(ZELDA1, Z1_TO_PLAY, Z1_PLAY, &[0x70, 0x84, 0xEB, 0x12, 0x13]);
}

#[test]
#[ignore = "needs The Legend of Zelda"]
fn discover_z1() {
    let _ = discover_game(ZELDA1, Z1_TO_PLAY, Z1_PLAY, false);
}
