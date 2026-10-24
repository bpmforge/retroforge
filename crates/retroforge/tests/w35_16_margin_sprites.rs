//! **W35-16: sprites in the 32:9 margins.** ROM-gated, `#[ignore]`; one at a time, release:
//! `cargo test --release -p retroforge --test w35_16_margin_sprites -- --ignored --nocapture <name>`
//!
//! `dump_*` write one line per frame of every object the game placed (screen px) to
//! `RF_OUT` (default the scratchpad), for the measurements in docs/research/W35-16-margin-sprites.md.

#[allow(dead_code)]
mod w33_07_common;

use std::io::Write;
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

/// Every object the game placed this frame: `x y w h tile slot`, screen px (x may be negative).
fn objects(rig: &Rig) -> Vec<(i32, i32, i32, i32, u16, u8)> {
    let mut out = Vec::new();
    if let Some((oam, size)) = rig.st.snes_oam_view() {
        let (small, big) = rf_snes::debug::obj_sizes(size);
        for e in rf_snes::debug::decode_oam(oam) {
            let (w, h) = if e.large { big } else { small };
            let mut y = i32::from(e.y) + 1;
            if y + i32::from(h) > 256 {
                y -= 256;
            }
            if y + i32::from(h) <= 0 || y >= 224 {
                continue;
            }
            out.push((
                i32::from(e.x),
                y,
                i32::from(w),
                i32::from(h),
                e.tile,
                e.index,
            ));
        }
    } else {
        let hgt = i32::from(rig.st.sprite_height_px());
        for e in &rf_debugger::oam::decode_oam(rig.st.oam()) {
            if e.y >= 240 {
                continue;
            }
            out.push((
                i32::from(e.x),
                i32::from(e.y) + 1,
                8,
                hgt,
                u16::from(e.tile),
                e.index,
            ));
        }
    }
    out
}

fn dump(g: (&str, &str, &str), head: &str, play: &str, tag: &str) {
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    rig.script(head);
    let dir =
        std::env::var("RF_OUT").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let mut f = std::fs::File::create(format!("{dir}/w3516_{tag}.tsv")).expect("out");
    for (i, u) in units(play).iter().enumerate() {
        rig.script(u);
        let objs = objects(&rig);
        write!(f, "{i}").unwrap();
        for (x, y, w, h, t, s) in objs {
            write!(f, "\t{x},{y},{w},{h},{t},{s}").unwrap();
        }
        writeln!(f).unwrap();
    }
    println!("DUMP {tag}");
}

#[test]
#[ignore = "needs SMW"]
fn dump_smw() {
    dump(SMW, &format!("{SMW_TO_LEVEL} w60"), SMW_PLAY, "smw");
}

#[test]
#[ignore = "needs SMB3"]
fn dump_smb3() {
    dump(SMB3, &format!("{SMB3_TO_LEVEL} w60"), SMB3_PLAY, "smb3");
}

#[test]
#[ignore = "needs Castlevania"]
fn dump_cv() {
    dump(CV, &format!("{CV_TO_STAGE} w60"), CV_PLAY, "cv");
}

// ---- the widened SNES frame through the app's own mask (SMW) -------------------------------------

/// The widened frame the app's core thread asks for: 512 dots, pad 128 each side.
const WIDE: usize = 512;
const PAD: usize = (WIDE - 256) / 2;

/// One frame under the policies the app uses (a default profile: `auto` per background, sprites
/// `safe`), then the app's mask over it.
fn wide_frame(rig: &mut Rig, unit: &str) -> Vec<u8> {
    let views = rig.st.bg_layer_views().expect("snes");
    let policies = rf_enhance::widescreen::WidescreenPolicies {
        enabled: true,
        ..Default::default()
    };
    let bg = std::array::from_fn(|i| policies.decide_all(views)[i].widens());
    rig.st.set_widescreen(WIDE, bg, true);
    rig.script(unit);
    let wide = rig.sink.wide.as_ref().expect("wide sink");
    retroforge::core_thread::margin_sprite_mask(&rig.st, wide)
}

fn wide_rig() -> Option<Rig> {
    let mut rig = Rig::open(SMW.0, SMW.1, Some(SMW.2))?;
    rig.script(&format!("{SMW_TO_LEVEL} w60"));
    let mut wide = rf_renderer::FrameBuffer::with_size(WIDE, 224);
    wide.set_sprite_id_capture(true);
    rig.sink.wide = Some(wide);
    Some(rig)
}

/// This process's resident set, MB (`ps`).
fn rss_mb_now() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
        / 1024
}

#[derive(Default, Debug)]
struct Episode {
    start: usize,
    frames: usize,
    first_overhang: i32,
    speed: f32,
    last: (i32, i32, i32),
}

/// SMW YI1 with the app's capture on: the painted dots against the game's own table.
/// Prints (and asserts) the numbers in docs/research/W35-16-margin-sprites.md section 7.
#[test]
#[ignore = "needs SMW"]
fn measure_smw() {
    let Some(mut rig) = wide_rig() else {
        return;
    };
    let mut rss_mb: Vec<u64> = Vec::new();
    let (mut frames, mut painted_frames, mut painted_dots) = (0usize, 0usize, 0usize);
    let (mut outside_box, mut pixel_straddler_missed, mut empty_when_no_entry) =
        (0usize, 0usize, 0usize);
    let mut sprite_dots_in_margin_unpainted = 0usize;
    let mut episodes: Vec<Episode> = Vec::new();
    // The frame with the most painted dots, for the cost measure.
    let mut busiest: (usize, Vec<u8>, Vec<u8>) = (0, Vec::new(), Vec::new());
    let mut open: std::collections::HashMap<u8, Episode> = std::collections::HashMap::new();
    for (i, u) in units(SMW_PLAY).iter().enumerate() {
        let mask = wide_frame(&mut rig, u);
        frames += 1;
        if i % 200 == 100 {
            rss_mb.push(rss_mb_now());
        }
        let ids = rig
            .sink
            .wide
            .as_ref()
            .unwrap()
            .sprite_ids()
            .unwrap()
            .to_vec();
        let objs = objects(&rig);
        let (small, big) = rf_snes::debug::obj_sizes(rig.st.snes_oam_view().unwrap().1);
        let _ = (small, big);
        let straddlers: Vec<_> = objs
            .iter()
            .filter(|o| rf_enhance::margin_sprites::straddles(o.0, o.2, 256))
            .collect();
        let mut n = 0usize;
        // Dots: painted ones must lie in their entry's box; margin sprite dots that are not painted
        // belong to entries that do not straddle.
        let mut pixel_edge: std::collections::HashSet<u8> = Default::default();
        let (mut inside_ids, mut margin_ids) = (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        );
        for (m, &id) in ids.iter().enumerate() {
            if id == rf_renderer::frame::NO_SPRITE {
                continue;
            }
            let (x, y) = (m % WIDE, m / WIDE);
            let _ = y;
            if (PAD..PAD + 256).contains(&x) {
                inside_ids.insert(id);
            } else {
                margin_ids.insert(id);
            }
            if mask[m] == 1 {
                n += 1;
                let sx = x as i32 - PAD as i32;
                let ok = objs
                    .iter()
                    .any(|o| o.5 == id && sx >= o.0 && sx < o.0 + o.2);
                outside_box += usize::from(!ok);
            } else if !(PAD..PAD + 256).contains(&x) {
                sprite_dots_in_margin_unpainted += 1;
            }
        }
        for id in inside_ids.intersection(&margin_ids) {
            pixel_edge.insert(*id);
            if !straddlers.iter().any(|o| o.5 == *id) {
                pixel_straddler_missed += 1;
            }
        }
        painted_dots += n;
        if n > busiest.0 {
            busiest = (
                n,
                mask.clone(),
                rig.sink.wide.as_ref().unwrap().rgba().to_vec(),
            );
        }
        painted_frames += usize::from(n > 0);
        if straddlers.is_empty() && n > 0 {
            empty_when_no_entry += 1;
        }
        // Episodes by slot: a straddling entry that continues (<= 24 px) is one object.
        let live: std::collections::HashSet<u8> = straddlers.iter().map(|o| o.5).collect();
        for o in &straddlers {
            let over = if o.0 < 0 { -o.0 } else { o.0 + o.2 - 256 };
            match open.get_mut(&o.5) {
                Some(e) if (e.last.0 - o.0).abs() <= 24 && (e.last.1 - o.1).abs() <= 24 => {
                    e.frames += 1;
                    e.speed = (o.0 - e.last.0) as f32;
                    e.last = (o.0, o.1, o.2);
                }
                _ => {
                    open.insert(
                        o.5,
                        Episode {
                            start: i,
                            frames: 1,
                            first_overhang: over,
                            speed: 0.0,
                            last: (o.0, o.1, o.2),
                        },
                    );
                }
            }
        }
        let gone: Vec<u8> = open.keys().copied().filter(|k| !live.contains(k)).collect();
        for k in gone {
            episodes.extend(open.remove(&k));
        }
    }
    episodes.extend(open.into_values());
    println!("MEASURE RSS (MB) at play frames 100, 300, 500, 700, 900: {rss_mb:?}");
    println!("MEASURE frames {frames}; frames with painted margin dots {painted_frames}; dots {painted_dots}");
    println!("MEASURE painted dots outside their entry's box: {outside_box}");
    println!("MEASURE sprites whose dots span both sides but are not in the OAM straddler set: {pixel_straddler_missed}");
    println!("MEASURE frames with painted dots and no straddling entry: {empty_when_no_entry}");
    println!("MEASURE margin sprite dots left unpainted (entries wholly off the picture): {sprite_dots_in_margin_unpainted}");
    println!("MEASURE episodes {}:", episodes.len());
    for e in &episodes {
        println!(
            "  frame {} lasts {} first overhang {} px, speed {:+.0} px/frame",
            e.start, e.frames, e.first_overhang, e.speed
        );
    }
    // Per-frame cost with the toggle on: the core thread's mask (OAM decode + ids to flags) and the
    // UI thread's paint onto a 32:9 picture, on a frame that has margin sprites.
    {
        let mut out = vec![0u8; 796 * 224 * 4];
        let native = busiest.2.clone();
        let reps = 500u32;
        let t = std::time::Instant::now();
        let mut fresh = 0;
        for _ in 0..reps {
            fresh = retroforge::core_thread::margin_sprite_mask(
                &rig.st,
                rig.sink.wide.as_ref().unwrap(),
            )
            .len();
        }
        let mask_us = t.elapsed().as_secs_f64() * 1e6 / f64::from(reps);
        let t = std::time::Instant::now();
        let mut n = 0;
        for _ in 0..reps {
            n = rf_enhance::margin_sprites::paint(
                &mut out, 796, 224, &native, WIDE, &busiest.1, 256,
            );
        }
        let paint_us = t.elapsed().as_secs_f64() * 1e6 / f64::from(reps);
        println!("COST mask {mask_us:.0} us, paint {paint_us:.0} us per frame ({n} dots painted in the busiest frame; a fresh mask is {fresh} flags)");
        // The mask and paint on their own for 20,000 frames' worth: resident memory must not move.
        let rss_before = rss_mb_now();
        for _ in 0..20_000 {
            std::hint::black_box(retroforge::core_thread::margin_sprite_mask(
                &rig.st,
                rig.sink.wide.as_ref().unwrap(),
            ));
            n = rf_enhance::margin_sprites::paint(
                &mut out, 796, 224, &native, WIDE, &busiest.1, 256,
            );
        }
        println!(
            "MEASURE RSS around 20,000 mask+paint frames: {rss_before} MB -> {} MB ({n} dots)",
            rss_mb_now()
        );
    }
    assert_eq!(outside_box, 0);
    assert_eq!(
        pixel_straddler_missed, 0,
        "the OAM read after the frame must match what was drawn"
    );
    assert_eq!(empty_when_no_entry, 0);
}

/// Explore: `RF_GAME` (smw, default, or alttp) frames `RF_FRAMES` (play-frame indices, space separated)
/// as the widened native picture (512 wide) to `RF_OUT`, margin dots the mask paints tinted red.
#[test]
#[ignore = "explore"]
fn explore_frames() {
    let alttp = std::env::var("RF_GAME").as_deref() == Ok("alttp");
    let (g, head, play) = if alttp {
        (ALTTP, alttp_to_free_play(), alttp_play())
    } else {
        (SMW, format!("{SMW_TO_LEVEL} w60"), SMW_PLAY.to_string())
    };
    let Some(mut rig) = Rig::open(g.0, g.1, Some(g.2)) else {
        return;
    };
    rig.script(&head);
    let mut wide = rf_renderer::FrameBuffer::with_size(WIDE, 224);
    wide.set_sprite_id_capture(true);
    rig.sink.wide = Some(wide);
    let want: Vec<usize> = std::env::var("RF_FRAMES")
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    let dir =
        std::env::var("RF_OUT").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    for (i, u) in units(&play).iter().enumerate() {
        let mask = wide_frame(&mut rig, u);
        if want.contains(&i) {
            let wide = rig.sink.wide.as_ref().unwrap();
            let mut px = wide.rgba().to_vec();
            for (m, &f) in mask.iter().enumerate() {
                if f == 1 {
                    px[m * 4] = px[m * 4].saturating_add(90);
                }
            }
            image::save_buffer(
                format!(
                    "{dir}/w3516_{}_{i}.png",
                    if alttp { "alttp" } else { "smw" }
                ),
                &px,
                WIDE as u32,
                224,
                image::ColorType::Rgba8,
            )
            .expect("png");
        }
        if i > want.iter().max().copied().unwrap_or(0) {
            break;
        }
    }
}

#[test]
#[ignore = "needs ALttP"]
fn dump_alttp() {
    dump(ALTTP, &alttp_to_free_play(), &alttp_play(), "alttp");
}

#[test]
#[ignore = "needs Super Metroid"]
fn dump_sm() {
    dump(SM, &sm_to_free_play(), SM_PLAY, "sm");
}
