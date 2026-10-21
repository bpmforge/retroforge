//! **W35-09: Zelda 1, a second neighbour screen each side at 32:9.** ROM-gated probes; one at a
//! time, release: `cargo test --release -p retroforge --test w35_09_zelda_reach -- --ignored --nocapture <name>`

#[allow(dead_code)]
mod w33_07_common;

use retroforge::scout::{Scout, ScoutDir, ScoutPlan, ScoutStatus, ScoutStrategy};
use w33_07_common::*;

/// Dump a canvas as a PNG: palette index -> a colour (probe).
fn dump(c: &rf_enhance::stitcher::Canvas, path: &str) {
    let (w, h) = (c.width().max(1), c.height().max(1));
    let (ox, oy) = c.origin();
    let mut img = image::RgbaImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let px = match c.get(ox + x as i64, oy + y as i64) {
                Some(p) => {
                    let v = p.palette_index.wrapping_mul(37);
                    image::Rgba([v, v.wrapping_mul(3), v.wrapping_mul(5), 255])
                }
                None => image::Rgba([255, 0, 255, 255]),
            };
            img.put_pixel(x as u32, y as u32, px);
        }
    }
    img.save(path).unwrap();
}

fn walk_plan(dirs: Vec<ScoutDir>) -> ScoutPlan {
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
        dirs,
        hold: vec![],
        start_when: vec![],
        pin: false,
        scene_key: None,
        reach_px: std::env::var("RF_REACH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
    }
}

#[test]
#[ignore = "probe"]
fn z1_pass_probe() {
    let Some(mut rig) = Rig::open(ZELDA1.0, ZELDA1.1, None) else {
        return;
    };
    rig.script(Z1_TO_PLAY);
    rig.script("w60");
    let snap = rig.st.snapshot().expect("snapshot");
    let scene = rig.live.current_scene().expect("scene");
    let dir = if std::env::var("RF_DIR").as_deref() == Ok("left") {
        ScoutDir::Left
    } else {
        ScoutDir::Right
    };
    let rom = rig.rom.clone();
    let mut s = Scout::start(
        &rom,
        snap,
        walk_plan(vec![dir]),
        &rig.live,
        scene,
        (256, 240),
    )
    .expect("scout");
    let no_scrolls = |_: &retroforge::stepper::EmuStepper| None;
    for f in 0..3000u32 {
        let st = s.step(&no_scrolls);
        if f % 10 == 0 || matches!(st, ScoutStatus::Finished(_)) {
            let r = |a: u32| s.peek_word(a) as u8;
            println!(
                "f{f} map {:02x} x {:02x} y {:02x} dims {:?} cam {:?}",
                r(0xEB),
                r(0x70),
                r(0x84),
                s.layer_dims(),
                s.camera_pose()
            );
        }
        if matches!(st, ScoutStatus::Finished(_)) {
            println!("finished {st:?}");
            if let Some(c) = s.canvases() {
                dump(&c[0], "/tmp/z1_reach_probe.png");
            }
            break;
        }
    }
}

/// Route probe: phases of a script separated by `;` (RF_ROUTE); prints the map byte and the live
/// canvas after each.
#[test]
#[ignore = "probe"]
fn z1_route() {
    let Some(mut rig) = Rig::open(ZELDA1.0, ZELDA1.1, None) else {
        return;
    };
    rig.script(Z1_TO_PLAY);
    rig.script("w60");
    let route = std::env::var("RF_ROUTE").expect("RF_ROUTE");
    for phase in route.split(';') {
        rig.script(phase.trim());
        let scene = rig.live.current_scene().expect("scene");
        let c = &rig.live.scene_canvases(scene.0).expect("set")[0];
        println!(
            "after {phase:?}: map {:02x} x {:02x} y {:02x} canvas {}x{} origin {:?}",
            rig.st.peek_bus(0xEB),
            rig.st.peek_bus(0x70),
            rig.st.peek_bus(0x84),
            c.width(),
            c.height(),
            c.origin()
        );
    }
}

/// The table the finder gives, scouted at 32:9 (`reach_px` 300 each side): the canvas reaches the
/// second screen each side, and every screen agrees with what a rig that WALKED there saw.
#[test]
#[ignore = "needs The Legend of Zelda"]
fn z1_two_screens_vs_walked() {
    let Some(mut rig) = Rig::open(ZELDA1.0, ZELDA1.1, None) else {
        return;
    };
    rig.script(Z1_TO_PLAY);
    rig.script("w60");
    let start = rig.st.snapshot().expect("snapshot");
    let skeleton = z1_find_table(&mut rig, 300);
    println!("{}", skeleton.to_toml());
    let mut plan = retroforge::scout_discovery::plan_of(&skeleton);
    plan.settle_frames = 0;
    // (856 - 256) / 2: what the app derives from the 32:9 aspect.
    plan.reach_px = 300;
    rig.st.restore(&start).expect("restore");
    rig.reset();
    rig.script("w60");
    let rom = rig.rom.clone();
    let (w, line) = scout_width(&mut rig, &rom, plan, 600);
    println!("scouted width {w}; {line}");
    let scene = rig.live.current_scene().expect("scene");
    let scouted = rig.live.scene_canvases(scene.0).expect("set")[0].clone();
    dump(&scouted, "/tmp/z1_two_scouted.png");
    // What the game itself showed: right two screens (round the trees at the sidestep row),
    // back, then left two.
    let mut truth = Rig::open(ZELDA1.0, ZELDA1.1, None).expect("rig");
    truth.st.restore(&start).expect("restore");
    truth.reset();
    for phase in [
        "R60 R60 R60 w100",
        "R80",
        "U12",
        "R60 R60 R60 w100",
        "L60 L60 L60 L60 L60 L60 w100",
        "D12",
        "L60 L60 L60 L60 w100",
        "L60 L60 L60 w100",
        "L60 L60",
        "U12",
        "L60 L60 L60 w100",
    ] {
        truth.script(phase);
        println!(
            "walked {phase:?}: map {:02x} x {:02x} y {:02x}",
            truth.st.peek_bus(0xEB),
            truth.st.peek_bus(0x70),
            truth.st.peek_bus(0x84)
        );
    }
    let tscene = truth.live.current_scene().expect("scene");
    let walked = truth.live.scene_canvases(tscene.0).expect("set")[0].clone();
    dump(&walked, "/tmp/z1_two_walked.png");
    println!(
        "walked canvas {}x{} origin {:?}; scouted {}x{} origin {:?}",
        walked.width(),
        walked.height(),
        walked.origin(),
        scouted.width(),
        scouted.height(),
        scouted.origin()
    );
    let (wx0, wy0) = walked.origin();
    for col in (wx0.div_euclid(256) * 256..wx0 + walked.width() as i64).step_by(256) {
        let (mut both, mut agree, mut walked_only, mut scouted_only) = (0u64, 0u64, 0u64, 0u64);
        for y in wy0 + 64..wy0 + walked.height() as i64 {
            for x in col..col + 256 {
                match (walked.get(x, y), scouted.get(x, y)) {
                    (Some(a), Some(b)) => {
                        both += 1;
                        agree += u64::from(a.palette_index == b.palette_index);
                    }
                    (Some(_), None) => walked_only += 1,
                    (None, Some(_)) => scouted_only += 1,
                    _ => {}
                }
            }
        }
        println!(
            "SCREEN x {col}..{}: both known {both}, agree {agree} ({:.2}%), walked-only {walked_only}, scouted-only {scouted_only}",
            col + 256,
            100.0 * agree as f64 / both.max(1) as f64
        );
        assert!(
            both >= 40_000,
            "the screen at {col} is known to both: {both}"
        );
        assert!(
            agree * 100 >= both * 98,
            "the screen at {col}: {agree} of {both}"
        );
    }
    assert!(
        scouted.origin().0 <= -512 && scouted.origin().0 + scouted.width() as i64 >= 768,
        "two screens each side of the first: {:?} {}",
        scouted.origin(),
        scouted.width()
    );
}
