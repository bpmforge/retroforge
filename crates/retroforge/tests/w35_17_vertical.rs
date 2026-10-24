//! **W35-17: up/down scouting for top-down games (The Legend of Zelda, A Link to the Past).**
//!
//! ROM-gated (law 5), one at a time, release:
//! `cargo test --release -p retroforge --test w35_17_vertical -- --ignored --nocapture <name>`
//!
//! What is measured is the 32:9 margin of the live canvas AFTER the player has walked a screen up:
//! the scout runs first (the real driver, worker thread and all), the live rig then walks, and the
//! margin cells the canvas knows at the picture's new place are counted
//! ([`rf_enhance::canvas::margin_known`]'s rule, per layer).

#[allow(dead_code)]
mod w33_07_common;

use retroforge::scout::{ScoutDir, ScoutPlan, ScoutStrategy};
use w33_07_common::*;

/// The table the finder gives Zelda 1 (W35-07), as the app runs it at 32:9 (`reach_px` 300).
fn z1_plan(dirs: &[ScoutDir]) -> ScoutPlan {
    ScoutPlan {
        strategy: ScoutStrategy::Walk,
        camera: vec![],
        player_x: vec![],
        player_x_hi: vec![],
        camera_y: vec![],
        player_y: vec![],
        player_y_hi: vec![],
        step_px: 1,
        frame_budget: 3000,
        stall_frames: 260,
        settle_frames: 0,
        dirs: dirs.to_vec(),
        hold: vec![],
        start_when: vec![],
        pin: false,
        scene_key: None,
        reach_px: 300,
    }
}

const TWO_AXES: [ScoutDir; 4] = [
    ScoutDir::Right,
    ScoutDir::Left,
    ScoutDir::Down,
    ScoutDir::Up,
];

/// `RF_DIRS=right,left` runs the baseline (one axis) instead.
fn dirs_from_env() -> Vec<ScoutDir> {
    std::env::var("RF_DIRS").map_or_else(
        |_| TWO_AXES.to_vec(),
        |v| v.split(',').filter_map(ScoutDir::parse).collect(),
    )
}

/// Margin cells of layer `layer` (`None`: the level layer the app picks, the first with content
/// that is not HUD) at the live picture's present place: `(known, total)` over all `rows` picture
/// rows, and over the rows from `skip_top` down (Zelda 1's status bar is cut out of the margins on
/// purpose).
fn margins_of(
    rig: &Rig,
    pad: i64,
    rows: i64,
    skip_top: i64,
    layer: Option<usize>,
) -> ((usize, usize), (usize, usize)) {
    let (Some(scene), Some(frames)) = (rig.live.current_scene(), rig.live.frames()) else {
        return ((0, 0), (0, 0));
    };
    let Some(set) = rig.live.scene_canvases(scene.0) else {
        return ((0, 0), (0, 0));
    };
    let hud = rig.live.hud_layers(scene.0);
    let n = layer.unwrap_or_else(|| {
        (0..set.len())
            .find(|&i| set[i].width() > 0 && !hud[i] && frames.origins[i].is_some())
            .unwrap_or(0)
    });
    let Some(origin) = frames.origins[n] else {
        return ((0, 0), (0, 0));
    };
    let canvas = &set[n];
    let all = rf_enhance::canvas::margin_known(canvas, origin, pad, rows);
    let (mut known, mut total) = (0, 0);
    for y in skip_top..rows {
        for dx in (-pad..0).chain(256..256 + pad) {
            total += 1;
            known += usize::from(canvas.get(origin.0 + dx, origin.1 + y).is_some());
        }
    }
    (all, (known, total))
}

fn pct(k: usize, t: usize) -> f64 {
    100.0 * k as f64 / t.max(1) as f64
}

/// Zelda 1 scouted from its first screen with `RF_DIRS` (default: both axes), then walked
/// `RF_WALK` (default: one screen up); the margins are counted after each leg. Acceptance (both
/// axes): the playfield rows of the margins are 95% known one screen up (the status bar's rows are
/// black on purpose, W35-07), and the first screen's own margins were full before.
#[test]
#[ignore = "needs The Legend of Zelda"]
fn z1_margins_after_walking_up() {
    let Some(mut rig) = Rig::open(ZELDA1.0, ZELDA1.1, None) else {
        return;
    };
    rig.script(Z1_TO_PLAY);
    rig.script("w60");
    let rom = rig.rom.clone();
    let dirs = dirs_from_env();
    let two_axes = dirs.contains(&ScoutDir::Up);
    let t0 = std::time::Instant::now();
    let (w, line) = scout_width(&mut rig, &rom, z1_plan(&dirs), 900);
    println!(
        "SCOUT {:.1}s dirs {dirs:?} width {w}; {}",
        t0.elapsed().as_secs_f32(),
        &line[..line.len().min(260)]
    );
    let ((k, t), (pk, pt)) = margins_of(&rig, 300, 240, 64, None);
    println!(
        "START margins: all rows {k}/{t} ({:.1}%), playfield rows {pk}/{pt} ({:.1}%)",
        pct(k, t),
        pct(pk, pt)
    );
    assert_eq!(pk, pt, "the first screen's playfield margins are full");
    let walk = std::env::var("RF_WALK").unwrap_or_else(|_| "U70 w100".into());
    let mut last = (0, 0);
    for phase in walk.split(';') {
        rig.script(phase.trim());
        let ((k, t), (pk, pt)) = margins_of(&rig, 300, 240, 64, None);
        let scene = rig.live.current_scene().expect("scene");
        let c = &rig.live.scene_canvases(scene.0).expect("set")[0];
        println!(
            "AFTER {phase:?}: map {:02x}; all rows {k}/{t} ({:.1}%), playfield rows {pk}/{pt} ({:.1}%); canvas {}x{}",
            rig.st.peek_bus(0xEB),
            pct(k, t),
            pct(pk, pt),
            c.width(),
            c.height()
        );
        last = (pk, pt);
    }
    if two_axes {
        assert!(
            last.0 * 100 >= last.1 * 95,
            "one screen up, the playfield margins are {}/{} known",
            last.0,
            last.1
        );
    }
}

/// The scouted canvas against the picture the game shows at rest (the truth of the screen it
/// shows) on each screen a second rig WALKS to: the screen above the first, and the one beside it
/// (the status bar's rows left out, and the twenty rows above the first screen that the live
/// canvas holds from the game's first frames).
#[test]
#[ignore = "needs The Legend of Zelda"]
fn z1_scouted_screens_against_what_the_game_shows() {
    let Some(mut rig) = Rig::open(ZELDA1.0, ZELDA1.1, None) else {
        return;
    };
    rig.script(Z1_TO_PLAY);
    rig.script("w60");
    let start = rig.st.snapshot().expect("snapshot");
    let rom = rig.rom.clone();
    let (w, _) = scout_width(&mut rig, &rom, z1_plan(&dirs_from_env()), 900);
    println!("SCOUT width {w}");
    let scene = rig.live.current_scene().expect("scene");
    let scouted = rig.live.scene_canvases(scene.0).expect("set")[0].clone();
    let mut truth = Rig::open(ZELDA1.0, ZELDA1.1, None).expect("rig");
    truth.st.restore(&start).expect("restore");
    truth.reset();
    truth.script("w60");
    // Up one screen, then right round the trees into the next one.
    let route = std::env::var("RF_UP")
        .unwrap_or_else(|_| "U70 w100;U60;R100;D30;R100;U30;R100;R100 R100".into());
    for phase in route.split(';') {
        truth.script(phase.trim());
        truth.script("w30");
        let Some(o) = truth.live.frames().and_then(|f| f.origins[0]) else {
            continue;
        };
        let (mut both, mut agree, mut unknown) = (0u64, 0u64, 0u64);
        // (picture colour, scouted colour) -> cells: how much of a disagreement is one palette
        // read for another (a screen whose own palette the game loads as its flip ends).
        let mut pairs = std::collections::BTreeMap::<(u8, u8), u64>::new();
        for r in 64..240usize {
            for c in 0..256usize {
                let Some(px) = truth.video.get(r * 256 + c) else {
                    continue;
                };
                if matches!(px.layer, rf_core_api::PixelLayer::Sprite) {
                    continue;
                }
                let (x, y) = (o.0 + c as i64, o.1 + r as i64);
                // The twenty rows above the first screen (the game's first frames) are not the
                // scout's: they sit in the live canvas, in the first screen's column.
                if (-20..0).contains(&y) && (0..256).contains(&x) {
                    continue;
                }
                match scouted.get(x, y) {
                    Some(cell) => {
                        both += 1;
                        agree += u64::from(cell.palette_index == px.palette_index);
                        *pairs
                            .entry((px.palette_index, cell.palette_index))
                            .or_default() += 1;
                    }
                    None => unknown += 1,
                }
            }
        }
        // The best one-to-one reading: each picture colour to the scouted colour it meets most.
        let mut best = std::collections::BTreeMap::<u8, u64>::new();
        for (&(p, _), &n) in &pairs {
            let e = best.entry(p).or_default();
            *e = (*e).max(n);
        }
        let same_shapes: u64 = best.values().sum();
        println!(
            "SCREEN after {phase:?}: map {:02x} origin {o:?}: scouted knows {both}, agrees {agree} ({:.2}%), unknown {unknown}; same shapes under a palette swap {:.2}%",
            truth.st.peek_bus(0xEB),
            100.0 * agree as f64 / both.max(1) as f64,
            100.0 * same_shapes as f64 / both.max(1) as f64
        );
        if both > 0 && truth.st.peek_bus(0xEB) == 0x67 && o.0 == 0 {
            assert!(
                agree * 100 >= both * 98,
                "the screen above: {agree} of {both}"
            );
        }
    }
}

/// The finder's discovery, from the first screen (where `discover_game` snapshots it), puts Down
/// and Up in the table of a game whose walker goes up from there and back down from where that
/// left it.
#[test]
#[ignore = "needs The Legend of Zelda"]
fn z1_discovery_adds_down_and_up() {
    let Some((skeleton, ..)) = discover_game(ZELDA1, Z1_TO_PLAY, Z1_WALK, false) else {
        return;
    };
    println!("DISCOVERED dirs {:?}", skeleton.dirs);
    assert_eq!(skeleton.dirs, ["right", "left", "down", "up"]);
}

/// A phase of `TPY<dy>` moves Link `dy` pixels vertically, 3 px a frame, by poking his Y word (as
/// the scout does: the game's own walls and camera window may refuse), then waits 60 frames; any
/// other phase is a pad script.
fn alttp_phase(rig: &mut Rig, phase: &str) {
    let Some(dy) = phase.strip_prefix("TPY") else {
        rig.script(phase);
        return;
    };
    let dy: i32 = dy.trim().parse().expect("TPY<dy>");
    let word = |rig: &Rig| {
        i32::from(rig.st.peek_bus(0x7E0020)) | (i32::from(rig.st.peek_bus(0x7E0021)) << 8)
    };
    for _ in 0..dy.abs() / 3 {
        let y = (word(rig) + 3 * dy.signum()).clamp(0, 0xFFFF);
        rig.st.poke_ram(0x7E0020, y as u8);
        rig.st.poke_ram(0x7E0021, (y >> 8) as u8);
        rig.frame(0);
    }
    rig.script("w60");
}

/// A Link to the Past: the finder's table (the session of `alttp_find_table`, vertical legs
/// included) must carry Down and Up; the road is scouted with it at 32:9 (`reach_px` 272), Link
/// walks `RF_WALK` and the margins are counted per layer; a second machine that only WALKED the
/// same legs is the truth the scouted canvases are compared with (palette indices, cells both
/// know). Scenarios `prefix|walk;walk` separated by `;;` (default: the road, up and down).
///
/// What this game gives on its first area, honestly: the camera's vertical window there is about
/// 160 px (`0x0a7e..0x0b1e`, W35-04) and the road is walled, so Link cannot walk a screen up or
/// down, and a poked Link is put back; the margins are what the road's X passes made them.
#[test]
#[ignore = "needs A Link to the Past"]
fn alttp_two_axes_on_the_road() {
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    rig.script(&alttp_to_free_play());
    let mut road = None;
    let t0 = std::time::Instant::now();
    let skeleton = alttp_find_table(&mut rig, 900, &mut |r| {
        road = Some(r.st.snapshot().expect("snapshot"));
    });
    let road = road.expect("the road");
    println!(
        "FIND {:.0}s dirs {:?} strategy {}",
        t0.elapsed().as_secs_f32(),
        skeleton.dirs,
        skeleton.strategy
    );
    assert!(
        skeleton.dirs.contains(&"down") && skeleton.dirs.contains(&"up"),
        "a top-down game's table carries both axes: {:?}",
        skeleton.dirs
    );
    let mut plan = retroforge::scout_discovery::plan_of(&skeleton);
    plan.settle_frames = 0;
    plan.reach_px = 272;
    if let Ok(d) = std::env::var("RF_DIRS") {
        plan.dirs = d.split(',').filter_map(ScoutDir::parse).collect();
    }
    let rom = rig.rom.clone();
    let scen = std::env::var("RF_SCEN").unwrap_or_else(|_| "|TPY-224;TPY448;TPY-224".into());
    for sc in scen.split(";;") {
        let (prefix, walks) = sc.split_once('|').unwrap_or(("", sc));
        println!(
            "SCENARIO prefix {prefix:?} walks {walks:?}; plan dirs {:?}",
            plan.dirs
        );
        rig.st.restore(&road).expect("restore");
        rig.reset();
        rig.script("w60");
        rig.script(prefix);
        rig.script("w60");
        let at = rig.st.snapshot().expect("snapshot");
        let t0 = std::time::Instant::now();
        let (w, line) = scout_width(&mut rig, &rom, plan.clone(), 1200);
        println!(
            "SCOUT {:.1}s width {w}; {}",
            t0.elapsed().as_secs_f32(),
            &line[..line.len().min(300)]
        );
        let scene = rig.live.current_scene().expect("scene");
        let scouted: Vec<_> = rig.live.scene_canvases(scene.0).expect("set").to_vec();
        for (i, c) in scouted.iter().enumerate().filter(|(_, c)| c.width() > 0) {
            println!(
                "CANVAS layer {i} {}x{} origin {:?}",
                c.width(),
                c.height(),
                c.origin()
            );
        }
        let all = |rig: &Rig| {
            (0..3)
                .map(|l| {
                    let ((k, t), _) = margins_of(rig, 272, 224, 0, Some(l));
                    format!("L{l} {k}/{t} ({:.1}%)", pct(k, t))
                })
                .collect::<Vec<_>>()
                .join("  ")
        };
        println!("START margins {}", all(&rig));
        for phase in walks.split(';') {
            alttp_phase(&mut rig, phase.trim());
            println!("AFTER {phase:?}: margins {}", all(&rig));
        }
        let mut truth = Rig::open(ALTTP.0, ALTTP.1, None).expect("rig");
        truth.st.restore(&at).expect("restore");
        truth.reset();
        truth.script("w60");
        for phase in walks.split(';') {
            alttp_phase(&mut truth, phase.trim());
        }
        let ts = truth.live.current_scene().expect("scene");
        let walked: Vec<_> = truth.live.scene_canvases(ts.0).expect("set").to_vec();
        for (l, (walked, scouted)) in walked.iter().zip(&scouted).enumerate() {
            let (mut both, mut agree) = (0u64, 0u64);
            let (ox, oy) = walked.origin();
            for y in oy..oy + walked.height() as i64 {
                for x in ox..ox + walked.width() as i64 {
                    if let (Some(a), Some(b)) = (walked.get(x, y), scouted.get(x, y)) {
                        both += 1;
                        agree += u64::from(a.palette_index == b.palette_index);
                    }
                }
            }
            println!(
                "AGREE layer {l}: walked {}x{} both known {both}, agree {agree} ({:.2}%)",
                walked.width(),
                walked.height(),
                100.0 * agree as f64 / both.max(1) as f64
            );
            assert!(agree * 100 >= both * 98, "layer {l}: {agree} of {both}");
        }
    }
}
