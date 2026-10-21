//! **W35-07: The Legend of Zelda (NES) at 32:9.** ROM-gated probes; run one at a time, release:
//! `cargo test --release -p retroforge --test w35_07_zelda1 -- --ignored --nocapture <name>`

#[allow(dead_code)]
mod w33_07_common;

use std::path::Path;
use w33_07_common::*;

/// Dump a canvas as a PNG: index -> a colour.
fn dump(c: &rf_enhance::stitcher::Canvas, path: &Path) {
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

#[test]
#[ignore = "probe"]
fn live_walk() {
    let Some(mut rig) = Rig::open(ZELDA1.0, ZELDA1.1, None) else {
        return;
    };
    rig.script(Z1_TO_PLAY);
    let script = std::env::var("RF_SCRIPT").unwrap_or_else(|_| "R60 R60 R60 w100".into());
    for tok in script.split_whitespace() {
        for _ in 0..tok_len(tok) {
            rig.script(&format!("{}1", tok.trim_end_matches(char::is_numeric)));
            let r = |a: u32| rig.st.peek_bus(a);
            let cs = rig.live.current_scene().map(|s| s.0 % 10000);
            let wd = rig
                .live
                .current_scene()
                .and_then(|s| rig.live.scene_canvases(s.0))
                .map(|c| (c[0].width(), c[0].origin().0));
            let (bands, _) = rf_enhance::scroll_tracker::compute_bands(&rig.last_events, (0, 0));
            let bd: Vec<_> = bands
                .iter()
                .map(|b| (b.start, b.end, b.base_scroll))
                .collect();
            println!("   bands {bd:?}");
            println!(
                "sc {cs:?} cv {wd:?} f{} map {:02x} x {:02x} y {:02x} 12 {:02x} fd {:02x} fc {:02x} scroll {:?}",
                rig.st.frame_count(), r(0xEB), r(0x70), r(0x84), r(0x12), r(0xFD), r(0xFC), rig.last_scroll
            );
        }
    }
    let scene = rig.live.current_scene().expect("scene");
    let set = rig.live.scene_canvases(scene.0).expect("canvases");
    for (i, c) in set.iter().enumerate() {
        println!(
            "layer {i}: {}x{} origin {:?}",
            c.width(),
            c.height(),
            c.origin()
        );
        if c.width() > 0 {
            dump(c, Path::new(&format!("/tmp/z1_live_{i}.png")));
        }
    }
}

fn tok_len(t: &str) -> u32 {
    t.trim_start_matches(char::is_alphabetic)
        .parse()
        .unwrap_or(1)
}

/// Walk the live game to the screens either side of the first one, and compare what the scout's
/// table put in the canvas with what the game itself showed there (cells both know).
#[test]
#[ignore = "needs The Legend of Zelda"]
fn z1_scout_vs_walked() {
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
    // The scout's canvas.
    rig.st.restore(&start).expect("restore");
    rig.reset();
    rig.script("w60");
    let rom = rig.rom.clone();
    let (w, line) = scout_width(&mut rig, &rom, plan, 600);
    println!("scouted width {w}; {line}");
    let scene = rig.live.current_scene().expect("scene");
    let scouted = rig.live.scene_canvases(scene.0).expect("set")[0].clone();
    dump(&scouted, Path::new("/tmp/z1_scouted.png"));
    // The game's own: walk right to the next screen and back, then left to the one before.
    let mut truth = Rig::open(ZELDA1.0, ZELDA1.1, None).expect("rig");
    truth.st.restore(&start).expect("restore");
    truth.reset();
    truth.script("w60 R60 R60 R60 w100 L60 L60 L60 L60 L60 w100 L60 L60 L60 L60 L60 L60 w100");
    let tscene = truth.live.current_scene().expect("scene");
    let walked = truth.live.scene_canvases(tscene.0).expect("set")[0].clone();
    dump(&walked, Path::new("/tmp/z1_walked.png"));
    println!(
        "walked canvas {}x{} origin {:?}; scouted {}x{} origin {:?}",
        walked.width(),
        walked.height(),
        walked.origin(),
        scouted.width(),
        scouted.height(),
        scouted.origin()
    );
    // Per 256-wide screen column, for the playfield rows (below the status bar).
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
    assert_eq!(
        (scouted.width(), scouted.origin().0),
        (768, -256),
        "the start screen and the one each side"
    );
}
