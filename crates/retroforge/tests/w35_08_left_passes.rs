//! **W35-08: left passes for the scout.** ROM-gated; run one at a time, release:
//! `cargo test --release -p retroforge --test w35_08_left_passes -- --ignored --nocapture <name>`

#[allow(dead_code)]
mod w33_07_common;

use w33_07_common::*;

/// Castlevania from the middle of the courtyard (Simon walked right ~2 screens first): the table
/// the finder writes says `left` too, and the canvas the scout fills left of the live picture
/// agrees with what a second rig sees when it walks there (cells both know).
#[test]
#[ignore = "needs Castlevania"]
fn cv_left_vs_walked() {
    let Some(mut rig) = Rig::open(CV.0, CV.1, None) else {
        return;
    };
    rig.script(CV_TO_STAGE);
    rig.script("w60");
    let mut mid = None;
    let sk = cv_find_table(&mut rig, 600, &mut |r| {
        r.script("R60 R+A30 R60 R+A30");
        mid = Some(r.st.snapshot().expect("snapshot"));
    });
    println!("{}", sk.to_toml());
    let toml = sk.to_toml();
    assert!(
        toml.contains("dirs = [\"right\", \"left\"]"),
        "the table does not say left:\n{toml}"
    );
    let mid = mid.expect("the mid-level snapshot");
    let mut plan = retroforge::scout_discovery::plan_of(&sk);
    plan.settle_frames = 0;
    let rom = rig.rom.clone();
    rig.st.restore(&mid).expect("restore");
    rig.reset();
    rig.script("w60");
    let (w, line) = scout_width(&mut rig, &rom, plan, 600);
    println!("scouted width {w}; {line}");
    let scene = rig.live.current_scene().expect("scene");
    let scouted = rig.live.scene_canvases(scene.0).expect("set")[0].clone();
    // The game's own: from the same snapshot, walk left to the wall.
    let mut truth = Rig::open(CV.0, CV.1, None).expect("rig");
    truth.st.restore(&mid).expect("restore");
    truth.reset();
    truth.script("w60 L60 L60 L60 L60 L60 L60 L60 L60 L60 L60 w100");
    let tscene = truth.live.current_scene().expect("scene");
    let walked = truth.live.scene_canvases(tscene.0).expect("set")[0].clone();
    println!(
        "walked {}x{} origin {:?}; scouted {}x{} origin {:?}",
        walked.width(),
        walked.height(),
        walked.origin(),
        scouted.width(),
        scouted.height(),
        scouted.origin()
    );
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
        "LEFT both known {both}, agree {agree} ({:.2}%), walked-only {walked_only}",
        100.0 * agree as f64 / both.max(1) as f64
    );
    assert!(
        both >= 100_000,
        "the walked left side is known to both: {both}"
    );
    assert!(agree * 100 >= both * 98, "{agree} of {both}");
    assert!(
        scouted.origin().0 <= walked.origin().0 + 64,
        "the scout did not reach as far left as the walk"
    );
}
