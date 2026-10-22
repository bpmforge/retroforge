//! **W35-11: the start gate (`start_when`) of Zelda 1, SMB3, MM2 and MMX is found by the finder, and
//! the real `ScoutDriver` never starts on a title, an attract, a menu or a transition, and does in
//! play.** ROM-gated; run one at a time, release:
//! `cargo test --release -p retroforge --test w35_11_start_gates -- --ignored --nocapture <name>`

#[allow(dead_code)]
mod w33_07_common;

use retroforge::scout::{LiveView, ScoutDriver, ScoutPlan, ScoutSetup, ScoutTelemetry};
use retroforge::scout_discovery::plan_of;
use rf_enhance::scene_tracker::SETTLING;
use std::sync::atomic::Ordering::Relaxed;
use w33_07_common::*;

/// One game's runs. `menus`: from power-on to the last frame before play (a long title so the
/// attract/demo shows, the menus, the select); `enter`: the press that starts play and a wait;
/// `play`: the finder's session after it.
struct Case {
    /// A second area (another level/stage/dungeon): the script from power-on to it, and the play
    /// there. `None`: not reached.
    second: Option<(String, String)>,
    /// The script the finder watches (from power-on to play; the game's own).
    session: String,
    game: (&'static str, &'static str, &'static str),
    menus: String,
    enter: &'static str,
    play: String,
}

/// The legend of Zelda: a long title (the story scrolls, the attract plays), the name entry and the
/// select screen, then the first overworld screen. (`Z1_TO_PLAY` with a longer first wait.)
fn zelda() -> Case {
    Case {
        session: Z1_TO_PLAY.into(),
        // The old man's cave on the first screen: another scene, another room.
        second: Some((
            format!("{Z1_TO_PLAY} U30 w10 L40 w10 U30 w10 L20 w10 U20 w10 U20 w60"),
            "L20 R20 L20 R20 L20 R20 L20 R20 L20 R20 L20 R20 L20 R20 L20 R20 L20 R20 L20 R20 w100"
                .into(),
        )),
        game: ZELDA1,
        menus: "w2400 S6 w300 S6 w300 A6 w30 E6 w20 E6 w20 E6 w20 S6 w200 E6 w20 E6 w20 E6 w20"
            .into(),
        enter: "S6 w400",
        play: Z1_WALK.into(),
    }
}

fn smb3() -> Case {
    Case {
        session: String::new(),
        second: None,
        game: SMB3,
        menus: "w2400 S6 w120 S6 w400 R8 w60 U8 w60".into(),
        enter: "A8 w300",
        play: SMB3_PLAY.into(),
    }
}

fn mm2() -> Case {
    Case {
        session: String::new(),
        // Another stage from the select screen.
        second: Some((
            "w400 S6 w120 S6 w300 S6 w300 R8 w20 S6 w800".into(),
            format!(
                "L30 w5 {} S6 w80 S6 w40 {}",
                "R25 R+A15 ".repeat(14),
                "R25 R+A15 ".repeat(8)
            ),
        )),
        game: MM2,
        menus: "w2400 S6 w120 S6 w300 S6 w300 U8 w20".into(),
        enter: "S6 w800",
        play: MM2_PLAY.into(),
    }
}

fn mmx() -> Case {
    Case {
        session: String::new(),
        second: None,
        game: MMX,
        menus: "w2400 S6 w114 S6 w114 S6 w114 S6 w114".into(),
        enter: "S6 w400",
        play: MMX_PLAY.into(),
    }
}

/// Explore: each token of the case's menus and entry, then the scene and the frame.
fn explore(c: &Case) {
    let Some(mut rig) = Rig::open(c.game.0, c.game.1, Some(c.game.2)) else {
        return;
    };
    let mut last = None;
    // `RF_FROM=session`: the game's own session instead of the long title; `RF_EXTRA`: more.
    let head = if std::env::var_os("RF_FROM").is_some() {
        c.session.clone()
    } else {
        format!("{} {}", c.menus, c.enter)
    };
    let extra = std::env::var("RF_EXTRA").unwrap_or_default();
    for tok in head.split_whitespace().chain(extra.split_whitespace()) {
        rig.script(tok);
        let s = rig.last_scene.map(|s| s % 100_000);
        if last != Some(s) {
            println!("{tok:>6} f{} scene {s:?}", rig.st.frame_count());
        }
        last = Some(s);
        if let Ok(dir) = std::env::var("RF_SHOTS") {
            let _ = std::fs::create_dir_all(&dir);
            rig.shot(&std::path::Path::new(&dir).join(format!("{:06}.png", rig.st.frame_count())));
        }
    }
}

/// Explore: `RF_GAME` (z1, smb3, mm2, mmx) and `RF_SCRIPT` from power-on, a shot after every token.
#[test]
#[ignore = "explore"]
fn explore_script() {
    let c = match std::env::var("RF_GAME").as_deref() {
        Ok("smb3") => smb3(),
        Ok("mm2") => mm2(),
        Ok("mmx") => mmx(),
        _ => zelda(),
    };
    let Some(mut rig) = Rig::open(c.game.0, c.game.1, Some(c.game.2)) else {
        return;
    };
    let dir = std::env::var("RF_SHOTS").unwrap_or_else(|_| "/tmp/rf_explore".into());
    let _ = std::fs::create_dir_all(&dir);
    let script = std::env::var("RF_SCRIPT").unwrap_or_default();
    for (i, tok) in script.split_whitespace().enumerate() {
        rig.script(tok);
        println!(
            "{i:03} {tok:>6} f{} scene {:?}",
            rig.st.frame_count(),
            rig.last_scene.map(|s| s % 100_000)
        );
        rig.shot(&std::path::Path::new(&dir).join(format!("{i:03}.png")));
    }
}

#[test]
#[ignore = "explore"]
fn explore_z1() {
    explore(&zelda());
}

#[test]
#[ignore = "explore"]
fn explore_smb3() {
    explore(&smb3());
}

#[test]
#[ignore = "explore"]
fn explore_mm2() {
    explore(&mm2());
}

#[test]
#[ignore = "explore"]
fn explore_mmx() {
    explore(&mmx());
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
    for tok in unit_tokens(&c.menus) {
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
    for tok in unit_tokens(c.enter).into_iter().chain(unit_tokens(&c.play)) {
        rig.script(&tok);
        tick(&mut rig);
        play_frames += 1;
        open_frames += u32::from(retroforge::scout::start_gate_open(&rows, |a| {
            rig.st.peek_bus(a)
        }));
    }
    let (mut second_frames, mut second_open) = (0, 0);
    if let Some((snap, play)) = &second {
        rig.st.restore(snap).expect("restore");
        for tok in unit_tokens(play) {
            rig.script(&tok);
            tick(&mut rig);
            second_frames += 1;
            second_open += u32::from(retroforge::scout::start_gate_open(&rows, |a| {
                rig.st.peek_bus(a)
            }));
        }
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

#[test]
#[ignore = "needs The Legend of Zelda"]
fn z1_start_gate() {
    check(&zelda());
}

#[test]
#[ignore = "needs Super Mario Bros. 3"]
fn smb3_start_gate() {
    check(&smb3());
}

#[test]
#[ignore = "needs Mega Man 2"]
fn mm2_start_gate() {
    check(&mm2());
}

#[test]
#[ignore = "needs Mega Man X"]
fn mmx_start_gate() {
    check(&mmx());
}

// ---- the real core thread (the path the app takes) -----------------------------------------------

use retroforge::core_thread::{self, CoreCommand, CoreEvent, WidescreenRequest};

/// The app's core thread, fed one frame of input at a time in real time (the pacer on), the way the
/// UI thread feeds it: the input is stored the moment a frame arrives, before the next one is run.
struct App {
    h: core_thread::CoreHandle,
    frames: u64,
    found: Option<Result<Box<rf_enhance::scout_finder::Skeleton>, String>>,
}

fn nes_mask(tok: &str) -> u16 {
    use rf_input::NesButton as B;
    tok.split('+').fold(0, |m, c| {
        m | match c {
            "A" => 1 << B::A.bit(),
            "B" => 1 << B::B.bit(),
            "E" => 1 << B::Select.bit(),
            "S" => 1 << B::Start.bit(),
            "U" => 1 << B::Up.bit(),
            "D" => 1 << B::Down.bit(),
            "L" => 1 << B::Left.bit(),
            "R" => 1 << B::Right.bit(),
            _ => 0,
        }
    })
}

impl App {
    fn open(c: &Case) -> Option<Self> {
        let path = rom_file(c.game.0, c.game.1)?;
        let rom = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
        let h = core_thread::spawn(rom).expect("core");
        h.cmd_tx.send(CoreCommand::Resume).expect("send");
        // Widescreen on, as it is for anyone who scouts (the finder watches from the first frame).
        h.cmd_tx
            .send(CoreCommand::SetWidescreen(Some(WidescreenRequest {
                width: 340,
                policies: rf_enhance::widescreen::WidescreenPolicies::default(),
                convention: false,
            })))
            .expect("send");
        Some(Self {
            h,
            frames: 0,
            found: None,
        })
    }

    fn pump(&mut self) {
        match self
            .h
            .evt_rx
            .recv_timeout(std::time::Duration::from_secs(10))
        {
            Ok(CoreEvent::Frame(_)) => {
                self.h
                    .pending_frames
                    .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                self.frames += 1;
            }
            Ok(CoreEvent::ScoutFound(r)) => self.found = Some(r),
            Ok(_) => {}
            Err(e) => panic!("the core thread went quiet: {e}"),
        }
    }

    /// `script` one frame at a time, `each` after every frame.
    fn run(&mut self, script: &str, mut each: impl FnMut(&mut Self)) {
        for tok in unit_tokens(script) {
            let mask = if tok.starts_with('w') {
                0
            } else {
                nes_mask(&tok[..tok.len() - 1])
            };
            let mut input = rf_core_api::InputFrame::empty();
            input.ports[0] = mask;
            self.h.input.store(input);
            let before = self.frames;
            while self.frames == before {
                self.pump();
            }
            each(self);
        }
    }
}

/// Zelda through the app's own path: the game is played from power-on through the menus into the
/// field with the finder watching from the first frame; "Find scout settings" is pressed in the
/// field; what it answers is the table the app would save. Then a fresh core with that table
/// runs the long title, the attract and the menus (no scout may start), and the field (one must).
#[test]
#[ignore = "needs The Legend of Zelda; real time, about five minutes"]
fn z1_in_app_start_gate() {
    let c = zelda();
    let Some(mut app) = App::open(&c) else { return };
    app.run(Z1_TO_PLAY, |_| {});
    app.run("w60", |_| {});
    app.h
        .cmd_tx
        .send(CoreCommand::FindScout(true))
        .expect("send");
    app.run(&c.play, |_| {});
    // Standing still until the trials answer (bounded: 90 s of frames).
    for _ in 0..180 {
        if app.found.is_some() {
            break;
        }
        app.run("w30", |_| {});
    }
    let skeleton = app
        .found
        .take()
        .expect("an answer")
        .expect("a table, not an error");
    let rows: Vec<(u32, u8)> = plan_of(&skeleton).start_when;
    println!(
        "IN-APP FIND saved start_when rows {rows:x?}\n{}",
        skeleton.to_toml()
    );
    drop(app);
    assert!(!rows.is_empty(), "the app's finder found no gate");

    let mut app = App::open(&c).expect("app");
    let mut plan = plan_of(&skeleton);
    plan.scene_key = None;
    let telemetry = std::sync::Arc::new(ScoutTelemetry::default());
    app.h
        .cmd_tx
        .send(CoreCommand::SetScout(Some(Box::new(ScoutSetup {
            plan,
            telemetry: telemetry.clone(),
        }))))
        .expect("send");
    app.run(&c.menus, |_| {});
    let in_menus = telemetry.started.load(Relaxed);
    app.run(c.enter, |_| {});
    app.run(&c.play, |_| {});
    app.run("w120", |_| {});
    let in_play = telemetry.started.load(Relaxed);
    println!("IN-APP START GATE: rows {rows:x?}; scouts started in the menus {in_menus}, by the end of play {in_play}");
    assert_eq!(in_menus, 0, "the scout started on a title, attract or menu");
    assert!(in_play >= 1, "the scout never started in play");
}
