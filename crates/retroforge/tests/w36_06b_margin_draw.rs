//! **W36-06b: the margin redraw** (ROM-gated, law 5). See `retroforge::margin_draw`.
//!
//! ```text
//! cargo test --release -p retroforge --test w36_06b_margin_draw -- --ignored --nocapture
//! ```

mod w36_06b_common;

use retroforge::margin_draw::{MarginDrawer, Shown};
use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, PpuPixel};
use w36_06b_common::*;

struct Discard;

impl CoreSink for Discard {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// Slots as `(rel, y)` of `st` (rel = camera-relative x).
fn slot_table(st: &EmuStepper) -> Vec<(i32, i32)> {
    let cam = i32::from(st.peek_bus(0x071A)) * 256 + i32::from(st.peek_bus(0x071C));
    (0..5u32)
        .filter(|k| st.peek_bus(0x0F + k) != 0)
        .map(|k| {
            let x = i32::from(st.peek_bus(0x6E + k)) * 256 + i32::from(st.peek_bus(0x87 + k));
            (x - cam, i32::from(st.peek_bus(0xCF + k)))
        })
        .collect()
}

/// Whether strip pixel (`col`, `row`) of `side` (`right`: false = left) is within tolerance of a slot of
/// `table`. The strip column of an object is its camera-relative x moved by a page.
fn near_a_slot(table: &[(i32, i32)], right: bool, col: i32, row: i32) -> bool {
    table.iter().any(|&(rel, y)| {
        let object = if right { rel - 256 } else { rel + 256 };
        (object - 16..object + 28).contains(&col) && (y - 32..y + 36).contains(&row)
    })
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn strips_appear_for_margin_enemies_and_only_there_and_sit_at_the_slots_spot() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let image = mod_image(&raw);
    let mut st = EmuStepper::open(&image).expect("opens");
    let mut drawer = MarginDrawer::new(&image, &recipe()).expect("the recipe has an object table");
    let (mut deep, mut deep_drawn, mut drawn, mut drawn_without_margin_slot) =
        (0u32, 0u32, 0u32, 0u32);
    let (mut pixels, mut pixels_off_spot) = (0u64, 0u64);
    let mut history: Vec<Vec<(i32, i32)>> = Vec::new();
    let mut cost = [0f64; 4];
    let started = std::time::Instant::now();
    for f in 0..3200u32 {
        let inp = input(script_two(f));
        if st.peek_bus(0x0770) == 1 {
            st.poke_bus(0x079E, 0x30);
        }
        let t0 = std::time::Instant::now();
        drawer.before_frame(&st, inp);
        let t1 = std::time::Instant::now();
        st.latch_and_advance_frame(inp, &mut Discard);
        let t2 = std::time::Instant::now();
        let oam = drawer.oam_of_current_picture();
        let md = drawer.frame(
            &st,
            inp,
            &Shown {
                ahead: 0,
                oam: &oam,
                ids: None,
                bg_under: &[],
            },
        );
        let t3 = std::time::Instant::now();
        cost[0] += (t1 - t0).as_secs_f64();
        cost[1] += (t2 - t1).as_secs_f64();
        cost[2] += (t3 - t2).as_secs_f64();
        cost[3] = cost[3].max((t3 - t2).as_secs_f64());
        history.push(slot_table(&st));
        if history.len() > 4 {
            history.remove(0);
        }
        if f < 500 || st.peek_bus(0x0770) != 1 {
            continue;
        }
        let table = history.concat();
        let is_deep = history
            .last()
            .is_some_and(|t| t.iter().any(|&(r, _)| r >= 336 || (-300..-80).contains(&r)));
        let margin_slot = table.iter().any(|&(r, _)| !(16..240).contains(&r));
        let has = md.as_ref().is_some_and(|m| !m.is_empty());
        deep += u32::from(is_deep);
        deep_drawn += u32::from(is_deep && has);
        drawn += u32::from(has);
        drawn_without_margin_slot += u32::from(has && !margin_slot);
        if let Some(m) = &md {
            for (right, strip) in [(false, &m.left), (true, &m.right)] {
                for (i, p) in strip.iter().enumerate() {
                    if *p != rf_enhance::margin_draw::TRANSPARENT {
                        pixels += 1;
                        let off = !near_a_slot(&table, right, (i % 256) as i32, (i / 256) as i32);
                        pixels_off_spot += u64::from(off);
                        if off && pixels_off_spot % 400 == 1 {
                            println!(
                                "f{f} right={right} col {} row {} slots {table:?}",
                                i % 256,
                                i / 256
                            );
                        }
                    }
                }
            }
        }
    }
    println!(
        "deep-margin frames {deep}, drawn {deep_drawn}; strips non-empty {drawn} (without a margin slot in \
         the last 3 frames: {drawn_without_margin_slot}); strip pixels {pixels}, off the slot's spot \
         {pixels_off_spot}; {} frames replayed in {:?} (whole 3200-frame run)",
        drawer.frames_replayed,
        started.elapsed()
    );
    println!(
        "per frame over 3200: snapshot {:.3} ms, the real frame {:.3} ms, margin redraw {:.3} ms (worst frame {:.2} ms)",
        cost[0] * 1000.0 / 3200.0,
        cost[1] * 1000.0 / 3200.0,
        cost[2] * 1000.0 / 3200.0,
        cost[3] * 1000.0
    );
    assert!(deep > 500, "the run exercised the deep margins: {deep}");
    assert!(
        deep_drawn * 100 >= deep * 97,
        "deep slots are drawn: {deep_drawn}/{deep}"
    );
    assert_eq!(
        drawn_without_margin_slot, 0,
        "nothing is drawn when no object is outside the picture"
    );
    assert_eq!(
        pixels_off_spot, 0,
        "every drawn pixel is at a slot's expected spot"
    );
}

/// A machine's drawn tiles in a comparable order.
fn sorted(mut t: Vec<rf_enhance::margin_draw::Tile>) -> Vec<(u8, i16, i16, u8)> {
    let mut v: Vec<_> = t
        .drain(..)
        .map(|t| (t.oam_index, t.x, t.y, t.tile))
        .collect();
    v.sort_unstable();
    v
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn the_unmoved_replay_reproduces_the_real_pictures_sprites() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let image = mod_image(&raw);
    let mut st = EmuStepper::open(&image).expect("opens");
    st.set_tile_capture(true);
    let mut drawer = MarginDrawer::new(&image, &recipe()).expect("object table");
    let (mut frames, mut same, mut nonempty) = (0u32, 0u32, 0u32);
    for f in 0..1500u32 {
        let inp = input(script_two(f));
        if st.peek_bus(0x0770) == 1 {
            st.poke_bus(0x079E, 0x30);
        }
        drawer.before_frame(&st, inp);
        st.latch_and_advance_frame(inp, &mut Discard);
        if f < 600 {
            continue;
        }
        let Some(replayed) = drawer.replay_unmoved(&st) else {
            continue;
        };
        let real = retroforge::margin_draw::tiles_of(&st);
        frames += 1;
        nonempty += u32::from(!real.is_empty());
        same += u32::from(sorted(real) == sorted(replayed));
    }
    println!("{frames} frames compared ({nonempty} with sprites): identical {same}");
    assert!(frames > 800 && nonempty > 800, "the comparison ran");
    assert_eq!(
        same, frames,
        "the replay reproduces the real picture's sprites exactly"
    );
}

/// The 256-px picture centred in a `out_w`-wide black frame, as `widen_margins_layered` starts from it.
fn centred(rgba: &[u8], out_w: usize) -> Vec<u8> {
    let pad = (out_w - 256) / 2;
    let mut out = vec![0u8; out_w * 240 * 4];
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    for y in 0..240 {
        out[(y * out_w + pad) * 4..][..256 * 4].copy_from_slice(&rgba[y * 256 * 4..][..256 * 4]);
    }
    out
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn layer_on_changes_the_pad_and_exactly_the_wrapped_dots_of_the_real_picture() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let image = mod_image(&raw);
    let mut st = EmuStepper::open(&image).expect("opens");
    st.set_bg_under_capture(true);
    let mut sink = rf_renderer::FrameBuffer::new();
    sink.set_sprite_id_capture(true);
    let mut drawer = MarginDrawer::new(&image, &recipe()).expect("object table");
    let resolve = |i: u8| rf_renderer::resolve_index(i, None);
    let out_w = 856; // Aspect::Wide32x9 at 240 lines
    let pad = (out_w - 256) / 2;
    let (mut frames, mut drawn_frames, mut ghost_frames, mut ghost_dots) = (0u32, 0u32, 0u32, 0u32);
    let (mut centre_changed, mut centre_changed_outside_ghosts, mut pad_painted) =
        (0u64, 0u64, 0u64);
    let mut pad_changed_but_not_by_a_strip = 0u64;
    for f in 0..3200u32 {
        let inp = input(script_two(f));
        if st.peek_bus(0x0770) == 1 {
            st.poke_bus(0x079E, 0x30);
        }
        drawer.before_frame(&st, inp);
        st.latch_and_advance_frame(inp, &mut sink);
        let oam = drawer.oam_of_current_picture();
        let md = drawer.frame(
            &st,
            inp,
            &Shown {
                ahead: 0,
                oam: &oam,
                ids: sink.sprite_ids(),
                bg_under: st.bg_under(),
            },
        );
        if f < 500 || st.peek_bus(0x0770) != 1 {
            continue;
        }
        frames += 1;
        let off = centred(&sink.to_vec(), out_w);
        let mut on = off.clone();
        if let Some(md) = &md {
            drawn_frames += 1;
            pad_painted += md.paint_pad(&mut on, out_w, 240, &resolve) as u64;
            let removed = md.remove_ghosts(&mut on, out_w, 240, &resolve);
            ghost_frames += u32::from(removed > 0);
            ghost_dots += removed as u32;
            // Every removed dot shows the background colour the PPU computed under it.
            for &(x, y, bg) in &md.ghosts {
                let at = (usize::from(y) * out_w + pad + usize::from(x)) * 4;
                assert_eq!(on[at..at + 3], resolve(bg), "frame {f}: ({x},{y})");
            }
        }
        let ghosts: std::collections::HashSet<(usize, usize)> = md
            .iter()
            .flat_map(|m| {
                m.ghosts
                    .iter()
                    .map(|&(x, y, _)| (usize::from(x), usize::from(y)))
            })
            .collect();
        for y in 0..240 {
            for x in 0..out_w {
                let i = (y * out_w + x) * 4;
                if on[i..i + 4] == off[i..i + 4] {
                    continue;
                }
                if (pad..pad + 256).contains(&x) {
                    centre_changed += 1;
                    centre_changed_outside_ghosts += u64::from(!ghosts.contains(&(x - pad, y)));
                } else {
                    // Outside the picture the only change is a strip painting black margin.
                    pad_changed_but_not_by_a_strip += u64::from(off[i..i + 3] != [0, 0, 0]);
                }
            }
        }
    }
    println!(
        "{frames} frames in play; the layer drew on {drawn_frames}; {ghost_frames} frames had a wrapped copy \
         taken off the real picture ({ghost_dots} dots); {pad_painted} pad dots painted; real-picture dots \
         changed {centre_changed}, outside the wrapped copies {centre_changed_outside_ghosts}"
    );
    assert!(drawn_frames > 500, "the layer drew: {drawn_frames}");
    assert!(pad_painted > 10_000, "pad dots: {pad_painted}");
    assert_eq!(
        centre_changed_outside_ghosts, 0,
        "the 256-px picture is untouched except the wrapped copies"
    );
    assert_eq!(pad_changed_but_not_by_a_strip, 0);
    // Step 1 (w36_06b_oam_observation) found no wrapped copy of a margin enemy on the real picture in
    // this run, so the wrapped-copy removal has nothing to remove here; its pure parts are unit-tested
    // in rf_enhance::margin_draw. What the real OAM held at the entries the redraw drew:
    println!("real OAM at the redraw's entries: {:?}", drawer.tally);
    assert_eq!(ghost_frames, 0, "no wrapped copies were found by step 1");
    assert!(
        drawer.tally.hidden > 1000,
        "the game hides the margin objects: {:?}",
        drawer.tally
    );
}

/// Plain play of 1-1 (no test-side help), `ahead` frames of run-ahead: per frame the picture shown and the
/// margin draw for it.
fn play_with_run_ahead(
    image: &[u8],
    ahead: u8,
    frames: u32,
) -> Vec<(Vec<u8>, Option<rf_enhance::margin_draw::MarginDraw>)> {
    let mut st = EmuStepper::open(image).expect("opens");
    st.set_bg_under_capture(true);
    let mut sink = rf_renderer::FrameBuffer::new();
    sink.set_sprite_id_capture(true);
    let mut drawer = MarginDrawer::new(image, &recipe()).expect("object table");
    let mut speculator = retroforge::run_ahead::Speculator::new();
    let mut out = Vec::new();
    for f in 0..frames {
        let inp = input(script_one(f));
        drawer.before_frame(&st, inp);
        st.latch_and_advance_frame(inp, &mut sink);
        let real_oam = drawer.oam_of_current_picture();
        if ahead == 0 {
            let md = drawer.frame(
                &st,
                inp,
                &Shown {
                    ahead: 0,
                    oam: &real_oam,
                    ids: sink.sprite_ids(),
                    bg_under: st.bg_under(),
                },
            );
            out.push((sink.to_vec(), md.map(|m| *m)));
        } else {
            let spec = speculator
                .run(&st, image, inp, ahead, (256, 240), false, true)
                .expect("a speculative frame");
            let md = drawer.frame(
                &st,
                inp,
                &Shown {
                    ahead,
                    oam: &spec.margin.oam,
                    ids: spec.margin.sprite_ids.as_deref(),
                    bg_under: &spec.margin.bg_under,
                },
            );
            out.push((spec.rgba, md.map(|m| *m)));
        }
    }
    out
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn run_ahead_with_the_mod_predicts_the_real_frame_and_its_margin_draw() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let image = mod_image(&raw);
    let frames = 1700;
    let off = play_with_run_ahead(&image, 0, frames);
    for ahead in [1u8, 2] {
        let on = play_with_run_ahead(&image, ahead, frames);
        let n = usize::from(ahead);
        let (mut compared, mut drawn, mut same_picture, mut same_draw) = (0u32, 0u32, 0u32, 0u32);
        for k in 600..frames as usize - n {
            // The speculative frames assume the input is held: skip windows where it changes.
            if (k..=k + n).any(|i| script_one(i as u32) != script_one(k as u32)) {
                continue;
            }
            compared += 1;
            drawn += u32::from(off[k + n].1.is_some());
            same_picture += u32::from(on[k].0 == off[k + n].0);
            same_draw += u32::from(on[k].1 == off[k + n].1);
        }
        println!(
            "run-ahead +{ahead}: {compared} windows; shown picture is the real frame +{ahead}: {same_picture}; \
             its margin draw equal: {same_draw} (the real frame had one in {drawn})"
        );
        assert!(
            compared > 500 && drawn > 100,
            "the comparison ran: {compared}/{drawn}"
        );
        assert_eq!(
            same_picture, compared,
            "+{ahead}: prediction matches with the mod on"
        );
        assert_eq!(
            same_draw, compared,
            "+{ahead}: the margin draw is the shown frame's"
        );
    }
}
