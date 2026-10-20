//! **W33-07: scout tables found by observation.**
//!
//! ROM-gated (law 5): every test is `#[ignore]` and skips with a message when
//! the ROM is absent. Run ONE at a time, release, from a cold canvas cache:
//!
//! ```text
//! cargo test --release -p retroforge --test w33_07_discovery -- --ignored --nocapture <name>
//! ```
//!
//! * `observe_*` -- one scripted session, the finder's `Observed` printed.
//! * `discover_*` -- the acceptance: the session, then the trials
//!   (`retroforge::scout_discovery`), then a scout with the DISCOVERED table
//!   against a scout with the hand-made one, widths compared.
//! * `explore_*` -- probes used to design the scripted sessions.

#[allow(dead_code)]
mod w33_07_common;

use std::path::Path;

use retroforge::scout::ScoutPlan;
use rf_enhance::scene_identity::SceneId;
use w33_07_common::*;

#[test]
#[ignore = "needs SMW"]
fn observe_smw() {
    observe(
        SMW,
        SMW_TO_LEVEL,
        SMW_PLAY,
        &[
            0x94, 0x95, 0x1A, 0x1B, 0x96, 0x97, 0x1462, 0xD1, 0x100, 0x101, 0x13D4, 0x71, 0x9D,
        ],
    );
}

#[test]
#[ignore = "needs SMB3"]
fn observe_smb3() {
    observe(
        SMB3,
        SMB3_TO_LEVEL,
        SMB3_PLAY,
        &[0x90, 0x75, 0xA2, 0xFD, 0x0F],
    );
}

#[test]
#[ignore = "needs MMX"]
fn observe_mmx() {
    observe(
        MMX,
        MMX_TO_LEVEL,
        MMX_PLAY,
        &[0xB4, 0xBAD, 0xBAE, 0x1F7A, 0xBCF],
    );
}

#[test]
#[ignore = "needs MM2"]
fn observe_mm2() {
    observe(
        MM2,
        MM2_TO_LEVEL,
        MM2_PLAY,
        &[0x04A0, 0x0460, 0x0440, 0x06C0, 0x1B],
    );
}

/// Explore: any game, print the probe bytes as a pattern repeats.
/// `RF_GAME=smw|smb3|mm2|mmx`, `RF_PATTERN`, `RF_PROBES=0x94,0x1A`, `RF_CYCLES`.
#[test]
#[ignore = "explore"]
fn explore_walk() {
    let game = std::env::var("RF_GAME").unwrap_or_else(|_| "smw".into());
    let (g, to_level) = match game.as_str() {
        "smb3" => (SMB3, SMB3_TO_LEVEL),
        "mm2" => (MM2, MM2_TO_LEVEL),
        "mmx" => (MMX, MMX_TO_LEVEL),
        _ => (SMW, SMW_TO_LEVEL),
    };
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    rig.script(to_level);
    if let Ok(prefix) = std::env::var("RF_PREFIX") {
        rig.script(&prefix);
    }
    let pattern = std::env::var("RF_PATTERN").unwrap_or_else(|_| "R20".to_string());
    let probes: Vec<u32> = std::env::var("RF_PROBES")
        .unwrap_or_else(|_| "0x94,0x95,0x1A,0x96".into())
        .split(',')
        .map(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).expect("hex"))
        .collect();
    let cycles: u32 = std::env::var("RF_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    for cycle in 0..cycles {
        rig.script(&pattern);
        let base = if rig.snes { 0x7E_0000 } else { 0 };
        let vals: Vec<String> = probes
            .iter()
            .map(|&o| format!("{:02x}", rig.st.peek_bus(base + o)))
            .collect();
        println!(
            "cycle {cycle}: {} scene {:?}",
            vals.join(" "),
            rig.last_scene.map(|s| s % 1000)
        );
        if let Ok(dir) = std::env::var("RF_SHOTS") {
            std::fs::create_dir_all(&dir).expect("dir");
            rig.shot(&Path::new(&dir).join(format!("{game}-{cycle:02}.png")));
        }
    }
}

/// Explore: from a snapshot at the level start, how far does a script get?
/// `RF_GAME`, `RF_PROBES=lo,hi` (the player X bytes), `RF_PREFIXES` (`;`
/// separated scripts), `RF_PATTERN` repeated `RF_CYCLES` times after each.
#[test]
#[ignore = "explore"]
fn explore_search() {
    let game = std::env::var("RF_GAME").unwrap_or_else(|_| "smw".into());
    let (g, to_level) = match game.as_str() {
        "smb3" => (SMB3, SMB3_TO_LEVEL),
        "mm2" => (MM2, MM2_TO_LEVEL),
        "mmx" => (MMX, MMX_TO_LEVEL),
        _ => (SMW, SMW_TO_LEVEL),
    };
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    rig.script(to_level);
    let snap = rig.st.snapshot().expect("snapshot");
    let probes: Vec<u32> = std::env::var("RF_PROBES")
        .unwrap_or_else(|_| "0x94,0x95".into())
        .split(',')
        .map(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).expect("hex"))
        .collect();
    let base = if rig.snes { 0x7E_0000 } else { 0 };
    let pattern = std::env::var("RF_PATTERN").unwrap_or_else(|_| "R+B14 R22".into());
    let cycles: u32 = std::env::var("RF_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(14);
    let prefixes = std::env::var("RF_PREFIXES").unwrap_or_else(|_| "w0".into());
    for prefix in prefixes.split(';') {
        rig.st.restore(&snap).expect("restore");
        rig.script(prefix);
        let (mut best, mut died_at) = (0u32, None);
        for cycle in 0..cycles {
            rig.script(&pattern);
            let x = u32::from(rig.st.peek_bus(base + probes[0]))
                | u32::from(rig.st.peek_bus(base + probes[1])) << 8;
            if x > best {
                best = x;
            } else if died_at.is_none() && x < best {
                died_at = Some(cycle);
            }
        }
        println!("PREFIX [{prefix}] best x {best:#x} died_at {died_at:?}");
    }
}

#[test]
#[ignore = "needs SMW"]
fn discover_smw() {
    accept(SMW, SMW_TO_LEVEL, SMW_PLAY);
}

#[test]
#[ignore = "needs SMB3"]
fn discover_smb3() {
    accept(SMB3, SMB3_TO_LEVEL, SMB3_PLAY);
}

#[test]
#[ignore = "needs MM2"]
fn discover_mm2() {
    accept(MM2, MM2_TO_LEVEL, MM2_PLAY);
}

#[test]
#[ignore = "needs MMX"]
fn discover_mmx() {
    let _ = discover_game(MMX, MMX_TO_LEVEL, MMX_PLAY, false);
}

/// Explore: scout trials with the hand-made table, with and without its hold.
#[test]
#[ignore = "explore"]
fn explore_trials_smb3() {
    let hints = std::env::var("RF_HINTS").map_or(true, |v| v != "0");
    let mut rig = Rig::open(SMB3.0, SMB3.1, hints.then_some(SMB3.2)).expect("rig");
    let rom = retroforge::rom_open::load_rom_bytes(&rom_file(SMB3.0, SMB3.1).unwrap()).unwrap();
    rig.script(SMB3_TO_LEVEL);
    rig.script("w60");
    let scene = SceneId(rig.last_scene.expect("scene"));
    let seed = rig.live.fork_for_scout();
    let snap = rig.st.snapshot().unwrap();
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(SMB3.2);
    let outcome = rf_profiles::load_file(&p).unwrap();
    let hand = ScoutPlan::from_profile(outcome.profile.scout.as_ref().unwrap());
    let mut scout =
        retroforge::scout::Scout::start(&rom, snap, hand.clone(), &seed, scene, (256, 240))
            .unwrap();
    for (name, hold, start_when_dir) in [("hand with hold", true, 0), ("hand no hold", false, 0)] {
        let mut plan = hand.clone();
        plan.settle_frames = 0;
        plan.scene_key = rig.scene_key.clone();
        plan.dirs = vec![retroforge::scout::ScoutDir::Right];
        plan.stall_frames = 90;
        plan.frame_budget = 400;
        if !hold {
            plan.hold.clear();
        }
        let _ = start_when_dir;
        scout.restart_with(plan).unwrap();
        let mut line = String::new();
        for f in 0..400u32 {
            let st = scout.step(&no_scrolls);
            if f % 40 == 0 {
                line += &format!(" {f}:{}/{:?}", scout.extent(), scout.words());
            }
            if st != retroforge::scout::ScoutStatus::Running {
                line += &format!(" END@{f} {st:?}");
                break;
            }
        }
        println!("{name}:{line}");
    }
}

// ---- the player finder against camera mirrors -----------------------------------

const SMB1: (&str, &str, &str) = (
    "nes",
    "Super Mario Bros. + Duck Hunt (USA).zip",
    "../../profiles/nes/super-mario-bros/profile.toml",
);
const CONTRA: (&str, &str, &str) = (
    "nes",
    "Contra (USA).zip",
    "../../profiles/nes/contra/profile.toml",
);

/// How often does the player finder name a camera mirror (a byte that holds the
/// scroll register's low byte on 70 % of the moving frames) as the player, with
/// the wall test and without it? One scripted session per game.
#[test]
#[ignore = "needs ROMs"]
fn mirror_false_positives() {
    let walk = |n: usize| " ".join_n(["R+B14 R+B+A24"], n);
    type Game = (
        &'static str,
        (&'static str, &'static str, &'static str),
        String,
        String,
    );
    let games: Vec<Game> = vec![
        ("smw", SMW, SMW_TO_LEVEL.into(), SMW_PLAY.into()),
        ("smb3", SMB3, SMB3_TO_LEVEL.into(), SMB3_PLAY.into()),
        ("mm2", MM2, MM2_TO_LEVEL.into(), MM2_PLAY.into()),
        ("mmx", MMX, MMX_TO_LEVEL.into(), MMX_PLAY.into()),
        (
            "smb1",
            SMB1,
            "w200 S6 w500".into(),
            format!("L10 w5 {}", walk(12)),
        ),
        (
            "contra",
            CONTRA,
            "w300 S6 w120 S6 w400".into(),
            format!("R+B40 R+B+A30 {}", walk(10)),
        ),
    ];
    let (
        mut mirrors_total,
        mut passing_no_wall,
        mut mirrors_no_wall,
        mut reported,
        mut reported_mirrors,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for (tag, game, to_level, play) in games {
        let Some(mut rig) = Rig::open(game.0, game.1, Some(game.2)) else {
            continue;
        };
        rig.script(&to_level);
        rig.script(&play);
        let mirrors = rig.finder.camera_mirrors();
        let no_wall = rig.finder.players_without_wall_test();
        let obs = rig.finder.observed();
        let named: Vec<usize> = obs.players.iter().map(|p| p.lo).collect();
        let m_no_wall: Vec<usize> = no_wall
            .iter()
            .copied()
            .filter(|o| mirrors.contains(o))
            .collect();
        let m_named: Vec<usize> = named
            .iter()
            .copied()
            .filter(|o| mirrors.contains(o))
            .collect();
        println!(
            "MIRRORS {tag}: camera+mirrors {} {:#x?}; pass step+direction without the wall test: {} of which mirrors {} {:#x?}; reported players {:#x?}, of which mirrors {} {:#x?}",
            mirrors.len(),
            mirrors,
            no_wall.len(),
            m_no_wall.len(),
            m_no_wall,
            named,
            m_named.len(),
            m_named
        );
        mirrors_total += mirrors.len();
        passing_no_wall += no_wall.len();
        mirrors_no_wall += m_no_wall.len();
        reported += named.len();
        reported_mirrors += m_named.len();
    }
    println!(
        "MIRROR-FP TOTAL: camera mirrors seen {mirrors_total}; names without the wall test {passing_no_wall} (mirrors among them {mirrors_no_wall}); reported players with the wall test {reported} (mirrors among them {reported_mirrors})"
    );
}

trait JoinN {
    fn join_n(self, parts: [&str; 1], n: usize) -> String;
}

impl JoinN for &str {
    fn join_n(self, parts: [&str; 1], n: usize) -> String {
        vec![parts[0]; n].join(self)
    }
}

// ---- the core thread's half of the app flow ---------------------------------------

/// The finder as the core thread runs it (`retroforge::scout_find`): fed live
/// frames, it starts its trials on a thread of its own when the player stands
/// still and reports a table. Super Mario Bros. 3 with its own `[scout]`
/// table ignored.
#[test]
#[ignore = "needs SMB3"]
fn scout_find_core_half_smb3() {
    use retroforge::scout_find::{Out, ScoutFind};
    let Some(mut rig) = Rig::open(SMB3.0, SMB3.1, Some(SMB3.2)) else {
        return;
    };
    rig.script(SMB3_TO_LEVEL);
    rig.find = Some(ScoutFind::new(false));
    rig.script(SMB3_PLAY);
    // The player stands still; the trials run beside the live game.
    let t0 = std::time::Instant::now();
    let mut found = None;
    // Bounded: 3 minutes of idle frames.
    while t0.elapsed() < std::time::Duration::from_secs(180) && found.is_none() {
        rig.script("w30");
        for out in rig.find_out.drain(..) {
            if let Out::Done(r) = out {
                found = Some(r);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    describe(&rig.finder.observed());
    let sk = found.expect("an answer").expect("a table, not an error");
    println!("{}", sk.to_toml());
    let addr_of = |role: &str| {
        sk.rows
            .iter()
            .find(|r| r.role == role)
            .map(|r| (r.addr, r.value))
    };
    assert_eq!(addr_of("player_x").map(|a| a.0), Some(0x90));
    assert_eq!(addr_of("player_x_hi").map(|a| a.0), Some(0x75));
    assert_eq!(addr_of("hold").map(|a| a.0), Some(0xA2));
    assert_eq!(sk.strategy, "player_move");
}

/// Ticket W33-07 (the fix the discovery needed): a NES scout whose profile has
/// a `[scene]` key is fed the same level bytes as the live model. Without them
/// the first scout frame is a "key change" and every pass ended at frame 0.
#[test]
#[ignore = "needs SMB3"]
fn nes_scout_with_a_scene_key_runs() {
    let Some(mut rig) = Rig::open(SMB3.0, SMB3.1, Some(SMB3.2)) else {
        return;
    };
    let rom = rig.rom.clone();
    rig.script(SMB3_TO_LEVEL);
    rig.script("w60");
    let scene = SceneId(rig.last_scene.expect("a scene"));
    let seed = rig.live.fork_for_scout();
    let snap = rig.st.snapshot().expect("snapshot");
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(SMB3.2);
    let outcome = rf_profiles::load_file(&p).expect("profile");
    let mut plan = ScoutPlan::from_profile(outcome.profile.scout.as_ref().expect("[scout]"));
    plan.scene_key = rig.scene_key.clone();
    assert!(plan.scene_key.is_some(), "SMB3's profile has [scene] rows");
    plan.settle_frames = 0;
    let mut scout =
        retroforge::scout::Scout::start(&rom, snap, plan, &seed, scene, (256, 240)).expect("scout");
    let no_scrolls = |_: &retroforge::stepper::EmuStepper| None;
    for _ in 0..60 {
        assert_eq!(
            scout.step(&no_scrolls),
            retroforge::scout::ScoutStatus::Running,
            "the pass ended in its first frames"
        );
    }
    assert!(scout.extent() > 0);
}
