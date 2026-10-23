//! **W35-14: the start gate in a second area of SMB3 (1-2), Mega Man X (a stage) and Zelda
//! (dungeon 1).** ROM-gated, `#[ignore]`; one at a time, release:
//! `cargo test --release -p retroforge --test w35_14_gate_areas -- --ignored --nocapture <name>`
//!
//! The areas are reached by PLAYING (D-021: no RAM maps): the scripts below were found by searching
//! over input timings with in-memory snapshots (`search_*`), judged by what the picture and the
//! scroll registers show; the result is a fixed input string, replayed here frame for frame (the
//! machine is deterministic). No ROM bytes and no save states are in git.

#[allow(dead_code)]
mod w33_07_common;

use w33_07_common::*;

fn units(script: &str) -> Vec<String> {
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

fn game(name: &str) -> (&'static str, &'static str, &'static str) {
    match name {
        "smb3" => SMB3,
        "mmx" => MMX,
        "mm2" => MM2,
        _ => ZELDA1,
    }
}

/// Explore: `RF_GAME`, `RF_HEAD` (power-on script), `RF_SCRIPT`; prints every `RF_EVERY` frames
/// (default 30) the frame, scene and the frame's scroll writes; shots with `RF_SHOTS=dir`.
#[test]
#[ignore = "explore"]
fn trace() {
    let g = game(&std::env::var("RF_GAME").unwrap_or_default());
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    let every: usize = std::env::var("RF_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let head = std::env::var("RF_HEAD").unwrap_or_default();
    rig.script(&head);
    let script = std::env::var("RF_SCRIPT").unwrap_or_default();
    let dir = std::env::var("RF_SHOTS").ok();
    if let Some(d) = &dir {
        let _ = std::fs::create_dir_all(d);
    }
    for (i, u) in units(&script).iter().enumerate() {
        rig.script(u);
        if i % every == 0 {
            println!(
                "f{} scene {:?} scroll {:?} dot {:?}",
                rig.st.frame_count(),
                rig.last_scene.map(|s| s % 100_000),
                rig.last_scroll,
                dot(&rig)
            );
            if let Some(d) = &dir {
                rig.shot(&std::path::Path::new(d).join(format!("{:06}.png", rig.st.frame_count())));
            }
        }
    }
}

// ---- the search --------------------------------------------------------------------------------

use retroforge::stepper::MachineSnapshot;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*: any non-zero seed; fine for choosing inputs.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn env_num(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The picture hash's change over `script` from the machine as it is (a probe on a copy: the
/// caller restores afterwards).
fn changes(rig: &mut Rig, script: &str) -> bool {
    let before = rig.shown;
    rig.script(script);
    rig.shown != before
}

/// Has the scene left `level` (another scene, or none, for 20 frames running)?
struct Leave {
    level: u64,
    away: u32,
}
impl Leave {
    fn step(&mut self, scene: Option<u64>) -> bool {
        if scene.map(|s| s % 100_000) == Some(self.level) {
            self.away = 0;
        } else {
            self.away += 1;
        }
        self.away >= 20
    }
}

struct Chunk {
    script: String,
    frames: u32,
}

/// Random local search over input chunks in a level: play chunks; when the scene leaves the level,
/// say `cleared(rig)` whether that was the level's end; if not, cut back `LAG` frames (the
/// game's death takes about that long to show) and play other chunks from there. Capped by
/// `RF_BUDGET` frames. Returns the script found, from `start`.
fn search(
    rig: &mut Rig,
    start: &MachineSnapshot,
    level: u64,
    options: &[&str],
    cleared: &dyn Fn(&mut Rig, &std::collections::HashSet<u64>) -> bool,
    survive: u32,
) -> Option<String> {
    let budget = env_num("RF_BUDGET", 600_000);
    let lag = env_num("RF_LAG", 200) as u32;
    let mut rng = Rng(env_num("RF_SEED", 1) | 1);
    rig.st.restore(start).expect("restore");
    // chunks[i] was played from snaps[i]; snaps[0] = start.
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut snaps: Vec<MachineSnapshot> = vec![rig.st.snapshot().expect("snapshot")];
    let mut spent: u64 = 0;
    let mut best = 0usize;
    // Every scene the machine showed in the search (the level's sections, its fades).
    let mut scenes: std::collections::HashSet<u64> = std::collections::HashSet::new();
    while spent < budget {
        // Choose a chunk: one of the options held 4..30 frames.
        let o = options[rng.below(options.len() as u64) as usize];
        let n = 4 + rng.below(27) as u32;
        let mut leave = Leave { level, away: 0 };
        let mut left_at = None;
        // Play it, then keep playing the same chunk's tail out as `R+B` until the checkpoint:
        // the chunk is one unit, the leave test runs on each frame.
        for i in 0..n {
            rig.script(&format!("{o}1"));
            if let Some(sc) = rig.last_scene {
                scenes.insert(sc % 100_000);
            }
            if leave.step(rig.last_scene) {
                left_at = Some(i);
                break;
            }
        }
        spent += u64::from(n);
        chunks.push(Chunk {
            script: o.to_string(),
            frames: n,
        });
        snaps.push(rig.st.snapshot().expect("snapshot"));
        // A long stand-by after a chunk to see the scene go: only when the chunk left it.
        if left_at.is_none() {
            // Death shows after ~`lag` frames: keep an eye out by letting the *next* chunks
            // report it; but when the leave is seen, cut.
            if chunks.len() > best {
                best = chunks.len();
            }
            // Survive mode: stay in the level for `survive` frames.
            if survive > 0 && chunks.iter().map(|c| c.frames).sum::<u32>() >= survive {
                return Some(
                    chunks
                        .iter()
                        .map(|c| format!("{}{}", c.script, c.frames))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            continue;
        }
        // Left the level: was it the end?
        let here = rig.st.snapshot().expect("snapshot");
        let played: u32 = chunks.iter().map(|c| c.frames).sum();
        if played >= env_num("RF_MIN", 1200) as u32 && std::env::var_os("RF_QUIET").is_none() {
            let text: Vec<String> = chunks
                .iter()
                .map(|c| format!("{}{}", c.script, c.frames))
                .collect();
            println!(
                "leave after {played} frames (spent {spent}): {}",
                text.join(" ")
            );
        }
        // A level takes a while: an early leave is a death (saves the slow probe).
        if survive == 0 && played >= env_num("RF_MIN", 1200) as u32 && cleared(rig, &scenes) {
            let mut s = String::new();
            for c in &chunks {
                s.push_str(&format!("{}{} ", c.script, c.frames));
            }
            return Some(s.trim_end().to_string());
        }
        rig.st.restore(&here).expect("restore");
        // Cut back `lag` + a random margin of frames, to a chunk boundary.
        let back = lag + rng.below(120) as u32;
        let mut cut = 0u32;
        while let Some(c) = chunks.last() {
            if cut >= back || chunks.is_empty() {
                break;
            }
            cut += c.frames;
            chunks.pop();
            snaps.pop();
        }
        // `snaps` always holds one more than `chunks`: the last snapshot is where to resume.
        if chunks.is_empty() {
            snaps.truncate(1);
        }
        rig.st
            .restore(snaps.last().expect("a snapshot"))
            .expect("restore");
    }
    println!("search: budget spent; deepest {best} chunks");
    None
}

const SMB3_HEAD: &str = "w300 S6 w120 S6 w400 R8 w60 U8 w60 A8 w300";

/// SMB3 1-1 cleared: input found by `search_smb3` (random chunks, rolled back on a death), played
/// after the head and a settle of 60 frames.
const SMB3_CLEAR_11: &str = "R+B12 R+B10 R+B+A8 R+B+A23 R+B+A9 R+B29 B21 R+B19 R+B+A24 R+B+A16 R+B+A13 R+B+A19 B14 R+B+A9 R+B4 R+B+A5 R+B+A17 R+B8 R+B28 R+B28 R+B+A+U8 R+B19 R+B+A15 B27 R+B22 R+B5 B11 R+B+A+U11 R+B+A21 R+B12 R+B+A9 R+B16 R+B+A+U5 R+B+A22 R+B+A26 R+B22 R+B22 R+B5 R+B21 B20 R+B+A+U29 R+B+A23 R+B+A7 R+B5 R+B+A11 R+B+A12 R+B+A30 R+B22 R+B+A18 R+B+A20 R+B15 R+B19 R+B+A5 R+B28 R+B19 R+B6 B16 R+B+A6 R+B9 R+B+A+U25 R+B26 R+B20 R+B11 B29 R+B+A+U14 R+B+A17 R+B7 R+B17 R+B29 R+B+A29 R+B+A17 B17 R+B+A22 B18 R+B6 B24 R+B+A26 R+B+A+U11 R+B27 R+B5 R+B19 R+B+A26 R+B+A+U6 B23 R+B+A25 R+B+A14 B13 R+B25 R+B13 R+B+A29 R+B6 R+B+A+U27 R+B7 R+B+A7 R+B20 R+B+A27 R+B19 R+B20 R+B+A25 R+B13 R+B+A16 B30 R+B+A+U6 R+B16 R+B+A27 R+B19 R+B+A28 R+B27 R+B+A20 R+B+A18 R+B+A19 R+B7 R+B20";

/// Power-on to the first frames of 1-2: 1-1, the card, the map and two steps right.
fn smb3_to_second() -> String {
    format!("{SMB3_HEAD} w60 {SMB3_CLEAR_11} w600 R20 w60 R20 w60 A8 w300")
}

/// After the level ends: wait out the card, press right and enter; if the path was unlocked a
/// different level (1-2) starts, if not the same level starts again.
fn smb3_cleared(rig: &mut Rig, level: u64) -> bool {
    rig.script("w600");
    let map = rig.last_scene.map(|s| s % 100_000);
    rig.script("R20 w60 R20 w60 A8 w300");
    rig.last_scene
        .map(|s| s % 100_000)
        .is_some_and(|s| s != level && Some(s) != map)
}

#[test]
#[ignore = "search; slow"]
fn search_smb3() {
    let Some(mut rig) = Rig::open(SMB3.0, SMB3.1, Some(SMB3.2)) else {
        return;
    };
    rig.script(SMB3_HEAD);
    rig.script("w60");
    let level = rig.last_scene.expect("a level") % 100_000;
    println!("level scene {level}");
    let start = rig.st.snapshot().expect("snapshot");
    let found = search(
        &mut rig,
        &start,
        level,
        &["R+B", "R+B", "R+B+A", "R+B+A", "B", "R+B+A+U"],
        &|r, _| smb3_cleared(r, level),
        0,
    );
    println!("FOUND {found:?}");
    let _ = changes;
}

/// 1-1 cleared (from the long search in `search_smb3`) and the walk to 1-2; then a stretch of play
/// in 1-2 that stays alive.
#[test]
#[ignore = "search; slow"]
fn search_smb3_second() {
    let Some(mut rig) = Rig::open(SMB3.0, SMB3.1, Some(SMB3.2)) else {
        return;
    };
    rig.script(&smb3_to_second());
    let level = rig.last_scene.expect("a level") % 100_000;
    println!("second level scene {level}");
    let start = rig.st.snapshot().expect("snapshot");
    let found = search(
        &mut rig,
        &start,
        level,
        &["R+B", "R+B", "R+B+A", "R+B+A", "B", "R+B+A+U"],
        &|_, _| false,
        env_num("RF_SURVIVE", 900) as u32,
    );
    println!("FOUND {found:?}");
}

// ---- the W35-11 validation, in the second areas -----------------------------------------------

use retroforge::scout::{LiveView, ScoutDriver, ScoutPlan, ScoutSetup, ScoutTelemetry};
use retroforge::scout_discovery::plan_of;
use rf_enhance::scene_tracker::SETTLING;
use std::sync::atomic::Ordering::Relaxed;

/// One game's runs (as `w35_11_start_gates::Case`).
struct Case {
    second: Option<(String, String)>,
    session: String,
    game: (&'static str, &'static str, &'static str),
    menus: String,
    enter: &'static str,
    play: String,
}
/// What a run of the driver saw.
struct Run {
    /// Scouts started over the menus (title, attract, selects): must be 0.
    in_menus: u64,
    /// Scouts started by the end of play: at least 1.
    in_play: u64,
    /// The `start_when` rows the run used.
    rows: Vec<(u32, u8)>,
    /// Frames of play, and those the gate was open on.
    play_frames: u32,
    open_frames: u32,
    /// The same in the second area (`0, 0`: none).
    second_frames: u32,
    second_open: u32,
}

/// Find the table from the session (`menus` + `enter` + `play`, the finder watching from power-on),
/// then run the real driver with it from power-on over the same frames, ticked every frame as the
/// core thread does.
fn run(c: &Case) -> Option<Run> {
    // (a case with no session of its own is watched through the whole run, the long title included)
    let session = if std::env::var_os("RF_SHORT").is_some() {
        match c.game.1 {
            f if f.starts_with("Mega Man X") => MMX_TO_LEVEL.to_string(),
            f if f.starts_with("Mega Man 2") => MM2_TO_LEVEL.to_string(),
            _ => SMB3_TO_LEVEL.to_string(),
        }
    } else if c.session.is_empty() {
        format!("{} {}", c.menus, c.enter)
    } else {
        c.session.clone()
    };
    // The second area's machine, made by another run of the game.
    let second = c
        .second
        .as_ref()
        .filter(|_| std::env::var_os("RF_NOSECOND").is_none())
        .and_then(|(to, play)| {
            let mut r = Rig::open(c.game.0, c.game.1, Some(c.game.2))?;
            r.script(to);
            r.script("w60");
            Some((r.st.snapshot().expect("snapshot"), play.clone()))
        });
    let (skeleton, ..) = discover_game_with(
        c.game,
        &session,
        &c.play,
        false,
        second.as_ref().map(|(snap, play)| (snap, play.as_str())),
    )?;
    let mut plan: ScoutPlan = plan_of(&skeleton);
    let rows = plan.start_when.clone();
    let mut rig = Rig::open(c.game.0, c.game.1, Some(c.game.2))?;
    let rom = rig.rom.clone();
    plan.scene_key = rig.scene_key.clone();
    let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
    let mut driver = ScoutDriver::default();
    driver.set(Some(ScoutSetup {
        plan,
        telemetry: telemetry.clone(),
    }));
    let size = if rig.snes { (256, 224) } else { (256, 240) };
    let scrolls: retroforge::scout::ScrollReader = if rig.snes {
        retroforge::core_thread::snes_layer_scrolls
    } else {
        no_scrolls
    };
    let mut tick = |rig: &mut Rig| {
        let scene = rig
            .last_scene
            .map_or(SETTLING, rf_enhance::scene_identity::SceneId);
        driver.tick(LiveView {
            stepper: &rig.st,
            rom: &rom,
            canvases: &mut rig.live,
            scene,
            size,
            scrolls,
            blank: rig.flat,
        });
    };
    let mut first_start = None;
    for tok in units(&c.menus) {
        rig.script(&tok);
        tick(&mut rig);
        if first_start.is_none() && telemetry.started.load(Relaxed) > 0 {
            first_start = Some(rig.st.frame_count());
        }
    }
    let in_menus = telemetry.started.load(Relaxed);
    if let Some(f) = first_start {
        println!("a scout started in the menus at frame {f}");
    }
    let (mut play_frames, mut open_frames) = (0, 0);
    for tok in units(c.enter).into_iter().chain(units(&c.play)) {
        rig.script(&tok);
        tick(&mut rig);
        play_frames += 1;
        open_frames += u32::from(retroforge::scout::start_gate_open(&rows, |a| {
            rig.st.peek_bus(a)
        }));
    }
    let (mut second_frames, mut second_open) = (0, 0);
    // `RF_ALT=addr:val,..` (hex): another table's rows, measured on the same frames (what the
    // gate without the second area would have been).
    let alt_rows: Option<Vec<(u32, u8)>> = std::env::var("RF_ALT").ok().map(|v| {
        v.split(',')
            .filter_map(|r| r.split_once(':'))
            .filter_map(|(a, b)| {
                Some((
                    u32::from_str_radix(a, 16).ok()?,
                    u8::from_str_radix(b, 16).ok()?,
                ))
            })
            .collect()
    });
    let mut alt_open = 0u32;
    if let Some((snap, play)) = &second {
        rig.st.restore(snap).expect("restore");
        for tok in units(play) {
            rig.script(&tok);
            tick(&mut rig);
            second_frames += 1;
            second_open += u32::from(retroforge::scout::start_gate_open(&rows, |a| {
                rig.st.peek_bus(a)
            }));
            if let Some(alt) = &alt_rows {
                alt_open += u32::from(retroforge::scout::start_gate_open(alt, |a| {
                    rig.st.peek_bus(a)
                }));
            }
        }
    }
    if alt_rows.is_some() {
        println!("ALT ROWS: open on {alt_open} of {second_frames} frames of the second area");
    }
    Some(Run {
        second_frames,
        second_open,
        in_menus,
        in_play: telemetry.started.load(Relaxed),
        play_frames,
        open_frames,
        rows,
    })
}

fn check(c: &Case) {
    let Some(r) = run(c) else { return };
    println!(
        "START GATE {}: rows {:x?}; scouts started in the menus {}, by the end of play {}; gate open on {} of {} frames of play",
        c.game.1, r.rows, r.in_menus, r.in_play, r.open_frames, r.play_frames
    );
    println!(
        "SECOND AREA {}: gate open on {} of {} frames",
        c.game.1, r.second_open, r.second_frames
    );
    if r.rows.is_empty() {
        println!(
            "NO GATE SHIPPED for {}: every candidate was shut in an area the player played in",
            c.game.1
        );
        return;
    }
    assert!(
        r.second_open * 10 >= r.second_frames * 9,
        "the gate is shut on more than a tenth of the second area ({} of {})",
        r.second_open,
        r.second_frames
    );
    assert_eq!(
        r.in_menus, 0,
        "the scout started on a title, attract or menu"
    );
    assert!(r.in_play >= 1, "the scout never started in play");
    assert!(
        r.open_frames * 2 >= r.play_frames,
        "the gate was shut on most of play ({} of {} open)",
        r.open_frames,
        r.play_frames
    );
}

/// Alive in 1-2 for 900 frames: found by `search_smb3_second`.
const SMB3_PLAY_12: &str = "R+B12 R+B10 R+B+A8 R+B+A23 R+B+A9 R+B29 B21 R+B19 R+B+A24 R+B+A16 R+B+A13 R+B+A19 B14 R+B+A9 R+B4 R+B+A5 R+B+A17 R+B8 R+B28 R+B10 R+B+A+U8 R+B+A20 R+B+A+U28 R+B27 R+B+A9 R+B10 R+B21 R+B7 B26 R+B+A+U18 R+B+A25 B5 B30 R+B11 R+B+A19 R+B14 R+B23 R+B+A+U4 R+B+A+U11 B10 R+B+A+U7 R+B+A13 R+B17 R+B25 R+B23 R+B+A26 R+B+A+U22 B8 R+B30 B7 R+B+A+U22 R+B+A+U16 R+B+A16 B19 B6 B18 R+B+A+U9";

fn smb3() -> Case {
    Case {
        session: String::new(),
        second: Some((smb3_to_second(), SMB3_PLAY_12.into())),
        game: SMB3,
        menus: "w2400 S6 w120 S6 w400 R8 w60 U8 w60".into(),
        enter: "A8 w300",
        play: SMB3_PLAY.into(),
    }
}

#[test]
#[ignore = "needs Super Mario Bros. 3"]
fn smb3_second_area_gate() {
    check(&smb3());
}

const MMX_HEAD: &str = "w300 S6 w114 S6 w114 S6 w114 S6 w114 S6 w400";

/// After the scene leaves the intro stage: idle a while; X put back at a checkpoint after a death
/// shows a scene the search has shown before; a scene never seen is a candidate (stage select):
/// saved as a shot for a look.
fn mmx_cleared(rig: &mut Rig, scenes: &std::collections::HashSet<u64>) -> bool {
    rig.script("w500");
    let Some(s) = rig.last_scene.map(|s| s % 100_000) else {
        return false;
    };
    if scenes.contains(&s) {
        return false;
    }
    // A game over also shows a new scene (the password grid); its Start puts X back in the intro
    // stage (a scene seen). A stage select's Start enters a stage the search has not shown.
    rig.script("S6 w400");
    let entered = rig.last_scene.map(|s| s % 100_000);
    println!(
        "CANDIDATE at frame {} scene {s}, Start leads to {entered:?}",
        rig.st.frame_count()
    );
    if entered.is_none_or(|e| scenes.contains(&e)) {
        return false;
    }
    if let Ok(d) = std::env::var("RF_SHOTS") {
        let _ = std::fs::create_dir_all(&d);
        rig.shot(&std::path::Path::new(&d).join(format!("cand{:06}.png", rig.st.frame_count())));
    }
    true
}

#[test]
#[ignore = "search; slow"]
fn search_mmx() {
    let Some(mut rig) = Rig::open(MMX.0, MMX.1, Some(MMX.2)) else {
        return;
    };
    rig.script(MMX_HEAD);
    rig.script("w60");
    let level = rig.last_scene.expect("a level") % 100_000;
    println!("level scene {level}");
    let start = rig.st.snapshot().expect("snapshot");
    let found = search(
        &mut rig,
        &start,
        level,
        &[
            "R", "R+B", "R+B", "R+Y", "R+Y", "R+B+Y", "R+B+Y", "R+A", "Y",
        ],
        &mmx_cleared,
        0,
    );
    println!("FOUND {found:?}");
}

/// Zelda's HUD map shows Link's screen as a green square: its position (the cell on the map).
/// Read from the picture, as the player sees it.
fn dot(rig: &Rig) -> Option<(usize, usize)> {
    let (w, rgba) = (rig.sink.fb.width(), rig.sink.fb.rgba());
    let mut hit = None;
    for y in 22..58 {
        for x in 12..84 {
            let p = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
            if i32::from(p[1]) > i32::from(p[0]) + 40 && i32::from(p[1]) > i32::from(p[2]) + 40 {
                hit = Some((x, y));
            }
        }
    }
    hit
}

// ---- Zelda: the overworld, screen by screen ------------------------------------------------------

/// The first overworld screen with the sword taken from the old man's cave, and out again.
const Z1_ARMED: &str = "w300 S6 w300 S6 w300 A6 w30 E6 w20 E6 w20 E6 w20 S6 w200 E6 w20 E6 w20 E6 w20 S6 w400 U30 w10 L40 w10 U30 w10 L20 w10 U20 w10 U20 w60 w100 U150 R6 w10 D10 w200 D170 w60";

struct Node {
    base: u64,
    cell: (usize, usize),
    snap: MachineSnapshot,
    path: String,
}

/// One random walk from the machine as it is: chunks that mostly go `dir`, until Link's map cell
/// changes (a screen flip), the scene changes (an entrance) or `frames` are spent. Returns what
/// ended it and the script played.
fn z1_walk(
    rig: &mut Rig,
    from: (usize, usize),
    dir: &str,
    rng: &mut Rng,
    frames: u32,
    base: u64,
) -> (String, Option<(usize, usize)>, bool, String) {
    let from = Some(from);
    let mut script = String::new();
    let mut spent = 0;
    let mut away = 0;
    while spent < frames {
        let n = 6 + rng.below(20) as u32;
        let pick = rng.below(10);
        let chunk = match pick {
            0..=5 => dir.to_string(),
            6 => format!("{dir}+A"),
            7 => ["U", "D", "L", "R"][rng.below(4) as usize].to_string(),
            8 => ["U", "D"][rng.below(2) as usize].to_string(),
            _ => ["L", "R"][rng.below(2) as usize].to_string(),
        };
        for _ in 0..n {
            rig.script(&format!("{chunk}1"));
            // (the scene is none while the screen scrolls: that is not an entrance)
            match rig.last_scene.map(|s| s % 100_000) {
                Some(s) if s != base => away += 1,
                Some(_) => away = 0,
                None => {}
            }
        }
        script.push_str(&format!("{chunk}{n} "));
        spent += n;
        let now = dot(rig);
        if away >= 40 && black_share(rig) >= 0.25 && hud_present(rig) {
            return ("entrance".into(), now, true, script);
        }
        if now.is_some() && now != from {
            return ("flip".into(), now, false, script);
        }
    }
    ("stuck".into(), dot(rig), false, script)
}

/// A fixed sweep: shift sideways `a`, go `dir` 90, shift sideways `b`, go `dir` 300 (negative:
/// the other way). Same result checks as `z1_walk`.
fn z1_sweep(
    rig: &mut Rig,
    from: (usize, usize),
    dir: &str,
    a: i32,
    b: i32,
    base: u64,
) -> (String, Option<(usize, usize)>, bool, String) {
    let (neg, pos) = if dir == "U" || dir == "D" {
        ("L", "R")
    } else {
        ("U", "D")
    };
    let side = |v: i32| {
        if v == 0 {
            String::new()
        } else {
            format!("{}{} ", if v < 0 { neg } else { pos }, v.abs())
        }
    };
    let plan = format!("{}{dir}90 {}{dir}300 ", side(a), side(b));
    let mut played: Vec<String> = Vec::new();
    // What was played, as one token per run of equal frames (a stop early leaves the rest out).
    let text = |played: &[String]| {
        let mut out = String::new();
        let mut i = 0;
        while i < played.len() {
            let mut j = i;
            while j < played.len() && played[j] == played[i] {
                j += 1;
            }
            out.push_str(&format!("{}{} ", played[i], j - i));
            i = j; // j > i: the inner loop always takes the first frame
        }
        out
    };
    let mut away = 0;
    for tok in units(&plan) {
        rig.script(&tok);
        played.push(tok[..tok.len() - 1].to_string());
        match rig.last_scene.map(|s| s % 100_000) {
            Some(s) if s != base => away += 1,
            Some(_) => away = 0,
            None => {}
        }
        if away >= 40 && black_share(rig) >= 0.25 && hud_present(rig) {
            return ("entrance".into(), dot(rig), true, text(&played));
        }
        let now = dot(rig);
        if now.is_some() && now != Some(from) {
            return ("flip".into(), now, false, text(&played));
        }
    }
    ("stuck".into(), dot(rig), false, text(&played))
}

#[test]
#[ignore = "search; slow"]
fn explore_z1_overworld() {
    let Some(mut rig) = Rig::open(ZELDA1.0, ZELDA1.1, Some(ZELDA1.2)) else {
        return;
    };
    rig.script(Z1_ARMED);
    rig.script(&std::env::var("RF_PREFIX").unwrap_or_default());
    let base = rig.last_scene.expect("a scene") % 100_000;
    rig.script("w2");
    let start = Node {
        base,
        cell: dot(&rig).expect("dot"),
        snap: rig.st.snapshot().expect("snapshot"),
        path: String::new(),
    };
    let mut rng = Rng(env_num("RF_SEED", 7) | 1);
    let dir_of = |a: (usize, usize), b: (usize, usize)| -> char {
        if b.0 > a.0 {
            'R'
        } else if b.0 < a.0 {
            'L'
        } else if b.1 < a.1 {
            'U'
        } else {
            'D'
        }
    };
    let _ = dir_of;
    let tries = env_num("RF_TRIES", 12);
    let walk = env_num("RF_WALK", 500) as u32;
    let max_nodes = env_num("RF_NODES", 14) as usize;
    let mut seen: Vec<(usize, usize)> = vec![start.cell];
    let mut queue = std::collections::VecDeque::from([start]);
    let shots = std::env::var("RF_SHOTS").ok();
    let mut done = 0;
    while let Some(node) = queue.pop_front() {
        done += 1;
        if done > max_nodes {
            break;
        }
        println!("NODE {:?} path: {}", node.cell, node.path);
        for dir in ["U", "R", "L", "D"] {
            if std::env::var("RF_DIRS").is_ok_and(|d| !d.contains(dir)) {
                continue;
            }
            let offs = [0, -20, 20, -40, 40, -70, 70, -100, 100];
            let sweeps: Vec<(i32, i32)> = offs
                .iter()
                .flat_map(|&a| offs.iter().map(move |&b| (a, b)))
                .collect();
            for t in 0..(tries + sweeps.len() as u64) {
                rig.st.restore(&node.snap).expect("restore");
                let (what, now, entered, script) = if (t as usize) < sweeps.len() {
                    let (a, b) = sweeps[t as usize];
                    z1_sweep(&mut rig, node.cell, dir, a, b, node.base)
                } else {
                    z1_walk(&mut rig, node.cell, dir, &mut rng, walk, node.base)
                };
                if what == "stuck" {
                    continue;
                }
                if !entered {
                    rig.script("w60");
                }
                let path = format!(
                    "{}{}{} ",
                    node.path,
                    script,
                    if entered { "w320" } else { "w60" }
                );
                if entered {
                    // Link's death looks like an entrance for a while: wait for the game over.
                    rig.script("w320");
                    if !hud_present(&rig) {
                        continue;
                    }
                }
                if entered && node.cell == (47, 54) {
                    // (the old man's cave on the first screen, already known)
                    continue;
                }
                if entered {
                    println!(
                        "ENTRANCE from {:?} going {dir}: scene {:?} path: {path}",
                        node.cell,
                        rig.last_scene.map(|s| s % 100_000)
                    );
                    if let Some(d) = &shots {
                        let _ = std::fs::create_dir_all(d);
                        rig.shot(
                            &std::path::Path::new(d)
                                .join(format!("ent_{}_{}_{}.png", node.cell.0, node.cell.1, dir)),
                        );
                    }
                    break;
                }
                println!("  edge {:?} {dir} -> {now:?} ({what})", node.cell);
                let Some(c) = now else { continue };
                // Only the screen in the direction asked (a shift sideways can flip another way).
                let right_way = match dir {
                    "U" => c.1 < node.cell.1,
                    "D" => c.1 > node.cell.1,
                    "L" => c.0 < node.cell.0,
                    _ => c.0 > node.cell.0,
                };
                if !entered && !right_way {
                    continue;
                }
                if !seen.contains(&c) {
                    seen.push(c);
                    println!("  {dir} -> {c:?} path: {path}");
                    if let Some(d) = &shots {
                        let _ = std::fs::create_dir_all(d);
                        rig.shot(
                            &std::path::Path::new(d).join(format!("cell_{}_{}.png", c.0, c.1)),
                        );
                    }
                    queue.push_back(Node {
                        base: rig.last_scene.map_or(node.base, |s| s % 100_000),
                        cell: c,
                        snap: rig.st.snapshot().expect("snapshot"),
                        path,
                    });
                }
                break;
            }
        }
    }
}

/// The share of the playfield (below the HUD) that is black: a cave or a dungeon room, not the
/// overworld.
fn black_share(rig: &Rig) -> f32 {
    let (w, rgba) = (rig.sink.fb.width(), rig.sink.fb.rgba());
    let (mut black, mut all) = (0u32, 0u32);
    for y in 70..220 {
        for x in 0..w {
            let p = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
            all += 1;
            black += u32::from(p[0] < 16 && p[1] < 16 && p[2] < 16);
        }
    }
    black as f32 / all as f32
}

/// The HUD's red "LIFE" is on screen (a game over has no HUD).
fn hud_present(rig: &Rig) -> bool {
    let (w, rgba) = (rig.sink.fb.width(), rig.sink.fb.rgba());
    let mut red = 0;
    for y in 20..36 {
        for x in 176..240 {
            let p = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
            red += u32::from(p[0] > 150 && p[1] < 80 && p[2] < 80);
        }
    }
    red >= 20
}

/// Zelda dungeon 1: from the armed first screen, a route over screens found by `explore_z1_overworld`
/// (the HUD map's cell as the observation) to the screen with the hollow dead tree, then the
/// walk to its door. (`w2` is the explorer's settle before its start.)
const Z1_TO_TREE: &str = "L20 U90 R40 U110 w60 R90 U70 R189 w60 U90 R40 U122 w60 U10 U11 U20 U24 U7 R11 U10 U22 U16 U15 U20 U+A15 D7 U+A22 U+A24 U17 U17 R16 U+A21 R19 U16 U24 U+A18 U14 U7 U6 U14 U8 w60 U211 w60 U20 L90 U40 L193 w60";

fn zelda() -> Case {
    Case {
        session: Z1_TO_PLAY.into(),
        second: Some((
            format!("{Z1_ARMED} w2 {Z1_TO_TREE} L93 w5 U50 w200"),
            "w30 U40 L60 R60 R60 D20 w40 R100 w40 L100 w40 U30 w40 D30 w40".into(),
        )),
        game: ZELDA1,
        menus: "w2400 S6 w300 S6 w300 A6 w30 E6 w20 E6 w20 E6 w20 S6 w200 E6 w20 E6 w20 E6 w20"
            .into(),
        enter: "S6 w400",
        play: Z1_WALK.into(),
    }
}

#[test]
#[ignore = "needs The Legend of Zelda"]
fn z1_dungeon_gate() {
    check(&zelda());
}
