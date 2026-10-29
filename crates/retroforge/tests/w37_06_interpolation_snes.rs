//! **W37-06: layer-level interpolation on real Super Mario World, Yoshi's Island 1** (ROM-gated, law 5).
//!
//! The captures the core thread takes (`frame_interp::capture_snes`: the frame buffer's per-dot layer
//! codes, sprite ids and per-row registers, and the PPU's latched line facts) are taken here from a
//! headless machine, and the pure composer is run on consecutive pairs at a 120 Hz presentation: one
//! intermediate per console frame, at the middle of the gap (alpha 128).
//!
//! ```text
//! cargo test --release -p retroforge --test w37_06_interpolation_snes -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use retroforge::frame_interp::{capture_snes, Captured};
use retroforge::stepper::EmuStepper;
use rf_core_api::InputFrame;
use rf_enhance::interpolate_snes::{compose_snes, SnesLayers};
use rf_input::SnesButton;
use rf_renderer::FrameBuffer;

#[path = "../../rf-harness/tests/smw_drive/mod.rs"]
mod smw_drive;

const W: usize = 256;
const H: usize = 224;

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root)
        .join("snes")
        .join("Super Mario World (USA).zip");
    p.exists().then_some(p)
}

fn pad(buttons: &[SnesButton]) -> InputFrame {
    let mut f = InputFrame::empty();
    f.ports[0] = buttons.iter().fold(0u16, |a, b| a | (1 << b.bit()));
    f
}

fn machine(captures: bool) -> Option<(EmuStepper, FrameBuffer)> {
    let path = rom_path().or_else(|| {
        println!("SKIP: Super Mario World (USA) not found");
        None
    })?;
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let st = EmuStepper::open(&raw).expect("opens");
    let mut sink = FrameBuffer::with_size(W, H);
    if captures {
        sink.set_layer_capture(true);
        sink.set_sprite_id_capture(true);
        sink.set_line_register_capture(true);
    }
    Some((st, sink))
}

fn run(st: &mut EmuStepper, sink: &mut FrameBuffer, buttons: &[SnesButton], n: u32) {
    let input = pad(buttons);
    for _ in 0..n {
        st.latch_and_advance_frame(input, sink);
    }
}

/// Boot, menus, the map at Yoshi's House, left to Yoshi's Island 1, enter, wait for the fade-in (the script
/// of `w32_07_scout_smw`).
fn reach_yi1(st: &mut EmuStepper, sink: &mut FrameBuffer) {
    // W37-00c: closed-loop, waits on the game mode (`smw_drive::to_yi1_direct`).
    smw_drive::to_yi1_direct(&mut |mask| {
        let mut input = InputFrame::empty();
        input.ports[0] = mask;
        st.latch_and_advance_frame(input, sink);
        st.peek_bus(smw_drive::MODE)
    });
}

fn game_mode(st: &EmuStepper) -> u8 {
    st.peek_bus(0x7E_0100)
}

fn bg1_hofs(st: &EmuStepper) -> u16 {
    st.bg_layer_views().expect("snes")[0].hofs
}

/// The play: the W33-07 oracle's walk right with a hop (`R+B14 R22`, which clears the level's first
/// obstacle), and on the map (game mode `0x0E`) Right to walk on and A to enter the next level.
/// W37-00c: the earlier "run right with Y, hop every 60 frames" script stood at the first obstacle
/// of this level and got its scrolling frames from dying and starting over, which the W37-00b
/// timing fixes shifted (289 pairs became 131 against the 200 the assertion wants); this one reads
/// the game, so the session keeps scrolling through whatever the levels do.
fn play(st: &EmuStepper, f: u32) -> Vec<SnesButton> {
    if game_mode(st) == 0x0E {
        return vec![if (f / 30).is_multiple_of(2) {
            SnesButton::Right
        } else {
            SnesButton::A
        }];
    }
    let mut keys = vec![SnesButton::Right];
    if f % 36 < 14 {
        keys.push(SnesButton::B);
    }
    keys
}

fn grab(st: &EmuStepper, sink: &FrameBuffer) -> Option<SnesLayers> {
    let Captured::Snes(mut l) = *capture_snes(
        st,
        sink.layer_codes(),
        sink.sprite_ids(),
        sink.line_regs(),
        sink.width(),
        sink.height(),
    )?
    else {
        unreachable!("an SNES machine")
    };
    l.rgba = sink.rgba()[..W * l.height * 4].to_vec();
    Some(l)
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// **Mode-invariant:** the simulation and the console's own pictures are the same with the SNES captures
/// (layer codes, sprite ids, line registers) on as with them off.
#[test]
#[ignore = "needs the SMW ROM"]
fn the_snes_captures_change_nothing_the_machine_does() {
    let results: Vec<_> = [false, true]
        .iter()
        .map(|&on| {
            let (mut st, mut sink) = machine(on)?;
            reach_yi1(&mut st, &mut sink);
            let mut frames = Vec::new();
            for f in 0..900u32 {
                let keys = play(&st, f);
                run(&mut st, &mut sink, &keys, 1);
                frames.push(fnv(sink.rgba()));
            }
            let ram: Vec<u8> = (0..0x2000u32).map(|a| st.peek_bus(0x7E_0000 + a)).collect();
            Some((frames, fnv(&ram), st.frame_count()))
        })
        .collect();
    let (Some(off), Some(on)) = (&results[0], &results[1]) else {
        return;
    };
    assert_eq!(off.0, on.0, "every console frame identical");
    assert_eq!(off.1, on.1, "RAM identical");
    assert_eq!(off.2, on.2, "frame count identical");
    println!(
        "{} frames after the level started: pictures, RAM and frame count identical with the SNES captures on and off",
        off.0.len()
    );
}

#[test]
#[ignore = "needs the SMW ROM; run with --release"]
fn smw_yi1_at_120_hz_how_many_frames_are_interpolated() {
    let Some((mut st, mut sink)) = machine(true) else {
        return;
    };
    reach_yi1(&mut st, &mut sink);
    println!(
        "in level? game mode {:#04x}, bg1 hofs {}",
        game_mode(&st),
        bg1_hofs(&st)
    );

    let mut prev: Option<(SnesLayers, u8, u16)> = None;
    let (mut all_ok, mut all_n, mut steady_ok, mut steady_n) = (0u32, 0u32, 0u32, 0u32);
    let mut refusals: std::collections::BTreeMap<String, u32> = Default::default();
    let mut steady_refusals: std::collections::BTreeMap<String, u32> = Default::default();
    let mut hazard_frames: std::collections::BTreeMap<u8, u32> = Default::default();
    let (mut unclaimed, mut fixed_rows, mut rows_held, mut sprites_moved, mut sprites_held) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut layers_moved: std::collections::BTreeMap<u8, u32> = Default::default();
    let (mut bg3_dots, mut bg3_moved) = (0usize, 0usize);
    let mut sample: Option<(SnesLayers, SnesLayers)> = None;
    let mut sample_speed = 0u32;
    let mut out = Vec::new();
    // Fidelity: composed at alpha 255 (the full step) a pair should come out as frame N+1, dot for dot.
    let (mut fid_dots, mut fid_wrong, mut fid_worst) = (0usize, 0usize, 0usize);
    let mut full = Vec::new();
    let mut capture_cost = std::time::Duration::ZERO;
    let mut captures = 0u32;

    for f in 0..2400u32 {
        let keys = play(&st, f);
        run(&mut st, &mut sink, &keys, 1);
        let t = Instant::now();
        let Some(l) = grab(&st, &sink) else {
            prev = None;
            continue;
        };
        capture_cost += t.elapsed();
        captures += 1;
        let mode = game_mode(&st);
        let hofs = bg1_hofs(&st);
        let mut bits = 0u8;
        for line in &l.lines {
            bits |= line.hazards;
        }
        *hazard_frames.entry(bits).or_default() += 1;
        if f % 300 == 0 {
            println!("f {f}: game mode {mode:#04x} hofs {hofs} hazards {bits:#04x}");
        }
        if let Some((p, pmode, phofs)) = prev.take() {
            all_n += 1;
            let steady = pmode == 0x14 && mode == 0x14 && phofs != hofs;
            if steady {
                steady_n += 1;
            }
            match compose_snes(&p, &l, 128, &mut out) {
                Ok(stats) => {
                    all_ok += 1;
                    if steady {
                        steady_ok += 1;
                    }
                    unclaimed += stats.unclaimed;
                    fixed_rows += stats.fixed_rows;
                    rows_held += stats.rows_held;
                    sprites_moved += stats.sprites_moved;
                    sprites_held += stats.sprites_held;
                    *layers_moved.entry(stats.layers_moved).or_default() += 1;
                    // A layer that did not move (BG3's status bar) is the same dot in the intermediate.
                    for i in 0..W * p.height {
                        if p.codes[i] & 7 == 2 && l.codes[i] & 7 == 2 {
                            bg3_dots += 1;
                            if out[i * 4..i * 4 + 3] != p.rgba[i * 4..i * 4 + 3] {
                                bg3_moved += 1;
                            }
                        }
                    }
                    if steady && compose_snes(&p, &l, 255, &mut full).is_ok() {
                        let wrong = full
                            .chunks_exact(4)
                            .zip(l.rgba.chunks_exact(4))
                            .filter(|(a, b)| a[..3] != b[..3])
                            .count();
                        fid_dots += W * l.height;
                        fid_wrong += wrong;
                        fid_worst = fid_worst.max(wrong);
                    }
                    // The sample for the cost and the pictures: the fastest scroll with sprites moving too.
                    let speed = u32::from(hofs.abs_diff(phofs));
                    if steady && stats.sprites_moved >= 2 && f > 300 && speed >= sample_speed {
                        sample_speed = speed;
                        sample = Some((p.clone(), l.clone()));
                    }
                }
                Err(r) => {
                    *refusals.entry(format!("{r:?}")).or_default() += 1;
                    if steady {
                        *steady_refusals.entry(format!("{r:?}")).or_default() += 1;
                    }
                }
            }
        }
        prev = Some((l, mode, hofs));
    }
    println!(
        "all pairs: {all_ok}/{all_n} interpolated ({:.1}%)",
        100.0 * f64::from(all_ok) / f64::from(all_n.max(1))
    );
    println!(
        "steady pairs (level, camera moving): {steady_ok}/{steady_n} ({:.1}%)",
        100.0 * f64::from(steady_ok) / f64::from(steady_n.max(1))
    );
    println!("refusals, all pairs: {refusals:?}");
    println!("refusals, steady pairs: {steady_refusals:?}");
    println!("hazard bits seen on some line of a frame (bits: frames): {hazard_frames:?}");
    println!("layers moved per composed pair (bit set of BG1..BG4: pairs): {layers_moved:?}");
    println!(
        "per composed pair: unclaimed dots {:.1}, fixed rows {:.1}, held rows {:.2}, sprites moved {:.1}, sprites held {:.2}",
        unclaimed as f64 / f64::from(all_ok.max(1)),
        fixed_rows as f64 / f64::from(all_ok.max(1)),
        rows_held as f64 / f64::from(all_ok.max(1)),
        sprites_moved as f64 / f64::from(all_ok.max(1)),
        sprites_held as f64 / f64::from(all_ok.max(1)),
    );
    println!("BG3 (status bar) dots visible in both frames: {bg3_dots}, of which moved in the intermediate: {bg3_moved}");
    println!(
        "fidelity at alpha 255 on steady pairs: {fid_wrong} of {fid_dots} dots differ from frame N+1 ({:.3}%), worst pair {fid_worst} dots",
        100.0 * fid_wrong as f64 / fid_dots.max(1) as f64
    );
    println!(
        "capture cost per console frame: {:?} (over {captures} frames)",
        capture_cost / captures.max(1)
    );
    assert!(
        all_n > 1500 && steady_n > 200,
        "the script played: {all_n} / {steady_n}"
    );
    assert_eq!(
        bg3_moved, 0,
        "a layer whose scroll did not change did not move"
    );

    // Cost of one intermediate frame on a typical steady pair.
    let (p, n) = sample.expect("a steady pair with moving sprites");
    let _ = compose_snes(&p, &n, 128, &mut out);
    if let Some(dir) = std::env::var_os("W37_SNES_SAMPLE_DIR") {
        for (name, px) in [("a", &p.rgba), ("b", &out), ("c", &n.rgba)] {
            let mut opaque = px.clone();
            opaque.chunks_exact_mut(4).for_each(|q| q[3] = 255);
            image::save_buffer(
                Path::new(&dir).join(format!("smw-{name}.png")),
                &opaque,
                W as u32,
                p.height as u32,
                image::ColorType::Rgba8,
            )
            .expect("png");
        }
        let mut census = [0usize; 8];
        for c in &p.codes {
            census[usize::from(c & 7)] += 1;
        }
        println!("layer census of the sample (BG1..4, sprite, backdrop, margin): {census:?}");
        println!("line 100: {:?}", p.lines[100]);
    }
    let runs = 400u32;
    let t = Instant::now();
    for i in 0..runs {
        let _ = compose_snes(&p, &n, (i % 250 + 1) as u8, &mut out);
    }
    println!(
        "cost per intermediate frame: compose {:?} ({W}x{} dots)",
        t.elapsed() / runs,
        p.height
    );
    println!("sample pair scrolls {sample_speed} dots");
    let share = f64::from(steady_ok) / f64::from(steady_n.max(1));
    println!("steady share {:.1}%", share * 100.0);
}
