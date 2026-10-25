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

/// Camera-relative x of each active slot of `st`.
fn rels(st: &EmuStepper) -> Vec<(u32, i32)> {
    let cam = i32::from(st.peek_bus(0x071A)) * 256 + i32::from(st.peek_bus(0x071C));
    (0..5u32)
        .filter(|k| st.peek_bus(0x0F + k) != 0)
        .map(|k| {
            let x = i32::from(st.peek_bus(0x6E + k)) * 256 + i32::from(st.peek_bus(0x87 + k));
            (k, x - cam)
        })
        .collect()
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
        let margin_slot = table.iter().any(|&(r, _)| r >= 240 || r < 16);
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
