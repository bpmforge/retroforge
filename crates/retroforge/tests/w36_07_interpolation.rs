//! **W36-07: layer-level interpolation on real Super Mario Bros. 1-1 play** (ROM-gated, law 5).
//!
//! The same capture the core thread does (`frame_interp::capture`) is taken here from a headless
//! machine, and the pure composer is run on consecutive pairs at a 120 Hz presentation: one
//! intermediate per console frame, at the middle of the gap (alpha 128).
//!
//! ```text
//! cargo test --release -p retroforge --test w36_07_interpolation -- --ignored --nocapture
//! ```

mod w36_06b_common;

use std::time::Instant;

use retroforge::frame_interp::{capture, Captured, Objects};
use retroforge::stepper::EmuStepper;
use rf_enhance::interpolate::{self, Layers};
use rf_enhance::interpolation::Refusal;
use w36_06b_common::*;

/// One console frame's layers, taken the way the core thread takes them.
fn grab(
    st: &EmuStepper,
    sink: &rf_renderer::FrameBuffer,
    oam: &[u8; 256],
    objs: Option<&Objects>,
) -> Layers {
    let Captured::Nes(mut l) = *capture(
        st,
        sink.sprite_ids(),
        sink.width(),
        sink.height(),
        oam,
        objs,
    )
    .expect("the captures are all on") else {
        unreachable!("a NES machine");
    };
    l.rgba = sink.to_vec();
    l
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn camera(st: &EmuStepper) -> i32 {
    i32::from(st.peek_bus(0x071A)) * 256 + i32::from(st.peek_bus(0x071C))
}

/// One frame of the script, with Mario kept unhurt (test-side poke, as the other W36-06b tests do).
fn step(st: &mut EmuStepper, sink: &mut rf_renderer::FrameBuffer, f: u32) {
    if st.peek_bus(0x0770) == 1 {
        st.poke_bus(0x079E, 0x30);
    }
    st.latch_and_advance_frame(input(script_two(f)), sink);
}

fn machine(raw: &[u8], captures: bool) -> (EmuStepper, rf_renderer::FrameBuffer) {
    let mut st = EmuStepper::open(raw).expect("opens");
    let mut sink = rf_renderer::FrameBuffer::new();
    st.set_bg_under_capture(captures);
    sink.set_sprite_id_capture(captures);
    (st, sink)
}

/// **Mode-invariant:** the simulation and the console's own pictures are the same with the layer
/// captures on as with them off (the captures are presentation-only reads).
#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn the_captures_change_nothing_the_machine_does() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let run = |captures: bool| {
        let (mut st, mut sink) = machine(&raw, captures);
        let mut frames = Vec::new();
        for f in 0..1100u32 {
            step(&mut st, &mut sink, f);
            frames.push(fnv(&sink.to_vec()));
        }
        let ram: Vec<u8> = (0..0x800u32).map(|a| st.peek_bus(a)).collect();
        (frames, fnv(&ram), *st.oam(), *st.palette())
    };
    let (off, on) = (run(false), run(true));
    assert_eq!(off.0, on.0, "every console frame identical");
    assert_eq!(off.1, on.1, "RAM identical after 1100 frames");
    assert_eq!(off.2, on.2, "OAM identical");
    assert_eq!(off.3, on.3, "palette identical");
    println!("1100 frames: pictures, RAM, OAM and palette identical with the captures on and off");
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR); run with --release"]
fn smb_1_1_at_120_hz_how_many_frames_are_interpolated() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let objects = Objects::from_recipe(&recipe()).expect("the SMB recipe has an object table");
    let (mut st, mut sink) = machine(&raw, true);

    let mut oam = [0xF8u8; 256];
    let mut prev: Option<(Layers, i32, u8, u8)> = None;
    // (all pairs, steady pairs) x (ok, total), refusals, HUD checks.
    let (mut all_ok, mut all_n, mut steady_ok, mut steady_n) = (0u32, 0u32, 0u32, 0u32);
    let mut refusals: std::collections::BTreeMap<String, u32> = Default::default();
    let mut steady_refusals: std::collections::BTreeMap<String, u32> = Default::default();
    let (mut hud_pairs, mut hud_checked, mut hud_differs_n_n1) = (0u32, 0u32, 0u32);
    let mut hud_ends: std::collections::BTreeMap<u16, u32> = Default::default();
    let mut scroll_lag: std::collections::BTreeMap<i32, u32> = Default::default();
    let mut static_exact = (0u32, 0u32);
    let mut out = Vec::new();
    let mut sample: Option<(Layers, Layers)> = None;
    let mut sprites_moved = 0usize;
    let mut sprites_grouped = 0usize;
    let mut cams: Vec<i32> = Vec::new();
    let mut pal_prev = [0u8; 32];
    let mut pal_changed: std::collections::BTreeMap<usize, u32> = Default::default();

    for f in 0..2600u32 {
        step(&mut st, &mut sink, f);
        let cam = camera(&st);
        cams.push(cam);
        let l = grab(&st, &sink, &oam, Some(&objects));
        oam = *st.oam();
        // Which of the last frames' camera values is the picture's scroll? (the picture lags the logic)
        if l.scroll.len() == 240 && l.scroll[120].0 != 0 {
            for k in 0..4usize {
                if cams.len() > k && i32::from(l.scroll[120].0) == cams[cams.len() - 1 - k] & 0x1FF
                {
                    *scroll_lag.entry(k as i32).or_default() += 1;
                    break;
                }
            }
        }
        let gameplay = st.peek_bus(0x0770) == 1;
        if f % 200 == 0 {
            println!(
                "f {f}: opermode {} player_state {} cam {cam}",
                st.peek_bus(0x0770),
                st.peek_bus(0x000E)
            );
        }
        let player_ok = st.peek_bus(0x000E) == 8;
        if let Some((p, pcam, pgame, pplayer)) = prev.take() {
            if f > 600 {
                all_n += 1;
                let mut candidate = Vec::new();
                let verdict = interpolate::compose(&p, &l, 128, &mut candidate);
                let moving = (cam - pcam).abs() >= 1;
                let steady = pgame == 1 && gameplay && pplayer == 1 && player_ok && moving;
                if steady {
                    steady_n += 1;
                }
                match &verdict {
                    Ok(stats) => {
                        all_ok += 1;
                        if steady {
                            steady_ok += 1;
                        }
                        sprites_moved += stats.sprites_moved;
                        sprites_grouped += stats.sprites_grouped;
                        out = candidate;
                        // HUD rows: identical across N, the intermediate and N+1 (when N and N+1 agree).
                        let hud = interpolate::detect_hud(&p, &l);
                        if let Some(top) = hud.first().filter(|h| h.start == 0) {
                            hud_pairs += 1;
                            *hud_ends.entry(top.end).or_default() += 1;
                            let bytes = usize::from(top.end) * 256 * 4;
                            assert_eq!(
                                &out[..bytes],
                                &p.rgba[..bytes],
                                "the intermediate's HUD rows are frame N's (frame {f})"
                            );
                            if p.rgba[..bytes] == l.rgba[..bytes] {
                                hud_checked += 1;
                                assert_eq!(&out[..bytes], &l.rgba[..bytes], "and N+1's");
                            } else {
                                hud_differs_n_n1 += 1;
                            }
                        }
                        if steady && sample.is_none() && stats.sprites_moved >= 2 && f > 900 {
                            sample = Some((p.clone(), l.clone()));
                        }
                    }
                    Err(r) => {
                        if matches!(r, Refusal::PaletteChanged) {
                            for (k, (a, b)) in pal_prev.iter().zip(st.palette()).enumerate() {
                                if a != b {
                                    *pal_changed.entry(k).or_default() += 1;
                                }
                            }
                        }
                        *refusals.entry(format!("{r:?}")).or_default() += 1;
                        if steady {
                            *steady_refusals.entry(format!("{r:?}")).or_default() += 1;
                        }
                    }
                }
                // The layers rebuild frame N's own picture when nothing moves (anti-vacuity).
                if f % 7 == 0 {
                    let mut same = l.clone();
                    same.index += 1;
                    let mut o = Vec::new();
                    if interpolate::compose(&l, &same, 128, &mut o).is_ok() {
                        static_exact.1 += 1;
                        if o == l.rgba {
                            static_exact.0 += 1;
                        }
                    }
                }
            }
        }
        pal_prev = *st.palette();
        prev = Some((l, cam, u8::from(gameplay), u8::from(player_ok)));
    }
    println!(
        "all pairs: {all_ok}/{all_n} interpolated ({:.1}%)",
        100.0 * f64::from(all_ok) / f64::from(all_n.max(1))
    );
    println!(
        "steady running pairs (gameplay, Mario normal, camera moving): {steady_ok}/{steady_n} ({:.1}%)",
        100.0 * f64::from(steady_ok) / f64::from(steady_n.max(1))
    );
    println!("refusals, all pairs: {refusals:?}");
    println!("refusals, steady pairs: {steady_refusals:?}");
    println!("palette entries that changed on refused pairs (index: count): {pal_changed:?}");
    println!("picture scroll = camera of k frames ago (k: count): {scroll_lag:?}");
    println!("top HUD band end rows (end: count): {hud_ends:?}");
    println!(
        "HUD rows compared across N, intermediate, N+1: {hud_checked} of {hud_pairs} pairs ({hud_differs_n_n1} had a changing HUD)"
    );
    println!(
        "still pairs rebuilt byte-exact from the layers: {}/{}",
        static_exact.0, static_exact.1
    );
    println!("moved sprites drawn over all pairs: {sprites_moved}, of which moved with a recipe object group: {sprites_grouped}");
    assert!(
        all_n > 1000 && steady_n > 200,
        "the script played: {all_n} / {steady_n}"
    );
    assert!(
        hud_checked > 50,
        "enough HUD pairs were checked: {hud_checked}"
    );
    assert_eq!(
        static_exact.0, static_exact.1,
        "with nothing moving the layers rebuild the console's own picture"
    );
    assert!(!out.is_empty());

    // Cost of one intermediate frame, on a typical steady pair.
    let (p, n) = sample.expect("a steady pair with moving sprites");
    let mut buf = Vec::new();
    let _ = interpolate::compose(&p, &n, 128, &mut buf);
    let runs = 400u32;
    let t = Instant::now();
    for i in 0..runs {
        let _ = interpolate::compose(&p, &n, (i % 250 + 1) as u8, &mut buf);
    }
    let per_full = t.elapsed() / runs;
    let t = Instant::now();
    for i in 0..runs {
        let _ = interpolate::compose_verified(&p, &n, (i % 250 + 1) as u8, &mut buf);
    }
    let per = t.elapsed() / runs;
    let t = Instant::now();
    for _ in 0..runs {
        let _ = interpolate::describes_its_picture(&p);
    }
    let per_check = t.elapsed() / runs;
    println!("cost per console frame: layer check {per_check:?}; per intermediate frame: compose {per:?} (with both checks {per_full:?}); 256x240, compose only");

    let share = f64::from(steady_ok) / f64::from(steady_n.max(1));
    assert!(
        share >= 0.90,
        "on steady running sections at least 90% of console frames get an interpolated intermediate, got {:.1}%",
        share * 100.0
    );
    let _ = Refusal::LargeMotion;
}
