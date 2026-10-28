//! **W39-01: no-latency extrapolation on real Super Mario World, Yoshi's Island 1** (ROM-gated, law 5).
//!
//! The tick hook (`frame_interp::Ticks`) reads the recipe's camera and player rows after every frame; the
//! Presenter shows each console frame on arrival and moves it on. Three things are checked on a headless
//! machine playing the W37-06 script:
//!
//! * **simulation untouched** — pictures, all of WRAM and the frame count are identical with the whole
//!   chain (captures, tick reads, Presenter) off and on;
//! * **zero added latency** — Right is pressed after Mario stood still; the first picture that differs
//!   from the standing one is shown on the SAME arrival in no-latency mode and one arrival later with
//!   the W37-06 delay (the same layers, the verdict removed);
//! * **fidelity** — an extrapolated picture is nearer the console frame that really followed than
//!   simply repeating the current one.
//!
//! Only counts are printed (D-021). PNGs go to `W39_01_SAMPLE_DIR` when it is set.
//!
//! ```text
//! cargo test --release -p retroforge --test w39_01_extrapolation -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use retroforge::frame_interp::{capture_snes, Captured, Presenter, Shown, Ticks};
use retroforge::stepper::EmuStepper;
use rf_core_api::InputFrame;
use rf_enhance::extrapolation::Verdict;
use rf_input::SnesButton;
use rf_renderer::FrameBuffer;

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

fn recipe() -> rf_rebuild::Recipe {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/snes/super-mario-world/recipe.toml");
    rf_profiles::load_recipe_file(&p).expect("the committed SMW recipe loads")
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

fn tap(st: &mut EmuStepper, sink: &mut FrameBuffer, b: SnesButton, hold: u32) {
    run(st, sink, &[b], hold);
    run(st, sink, &[], 8);
}

/// Boot, menus, the map, Yoshi's Island 1, the fade-in (the script of `w37_06_interpolation_snes`).
fn reach_yi1(st: &mut EmuStepper, sink: &mut FrameBuffer) {
    run(st, sink, &[], 200);
    for i in 0..10 {
        let b = if i % 2 == 0 {
            SnesButton::Start
        } else {
            SnesButton::A
        };
        tap(st, sink, b, 6);
        run(st, sink, &[], 100);
    }
    run(st, sink, &[], 300);
    tap(st, sink, SnesButton::A, 8);
    run(st, sink, &[], 400);
    tap(st, sink, SnesButton::Left, 8);
    run(st, sink, &[], 150);
    tap(st, sink, SnesButton::A, 8);
    run(st, sink, &[], 150);
}

fn play(f: u32) -> Vec<SnesButton> {
    let mut keys = vec![SnesButton::Right, SnesButton::Y];
    if (f / 20).is_multiple_of(3) {
        keys.push(SnesButton::B);
    }
    keys
}

/// A runner that hops in long arcs (the plain `play` falls into the first pit and never scrolls the camera).
fn hop(f: u32) -> Vec<SnesButton> {
    let mut keys = vec![SnesButton::Right, SnesButton::Y];
    if f % 40 < 28 {
        keys.push(SnesButton::B);
    }
    keys
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The frame the machine just ran, as the core thread would capture it, tick verdict attached when
/// `ticks` is given.
fn grab(st: &EmuStepper, sink: &FrameBuffer, ticks: Option<&mut Ticks>) -> Option<Captured> {
    let mut c = *capture_snes(
        st,
        sink.layer_codes(),
        sink.sprite_ids(),
        sink.line_regs(),
        sink.width(),
        sink.height(),
    )?;
    c.set_rgba(&sink.rgba()[..W * c.dims().1 * 4]);
    if let Some(t) = ticks {
        t.attach(st, &mut c);
    }
    Some(c)
}

fn word(st: &EmuStepper, addr: u32) -> i32 {
    i32::from(st.peek_bus(addr)) | i32::from(st.peek_bus(addr + 1)) << 8
}

/// A print of the sprite dots around the player only (other sprites and the scenery animate by
/// themselves). The player's place on the picture is the recipe's player row less its camera row.
fn player_print(st: &EmuStepper, c: &Captured) -> u64 {
    let Captured::Snes(l) = c else {
        unreachable!("an SNES machine")
    };
    let sx = word(st, 0x7E_0094) - word(st, 0x7E_001A);
    let sy = word(st, 0x7E_0096) - word(st, 0x7E_001C);
    let mut masked = Vec::new();
    for (i, (p, code)) in l.rgba.chunks_exact(4).zip(&l.codes).enumerate() {
        let (x, y) = ((i % W) as i32, (i / W) as i32);
        let near = (sx - 16..sx + 32).contains(&x) && (sy - 16..sy + 48).contains(&y);
        if near && code & 7 == rf_enhance::interpolate_snes::CODE_SPRITE {
            masked.extend_from_slice(&[p[0], p[1], p[2], x as u8, y as u8]);
        }
    }
    fnv(&masked)
}

fn wram_print(st: &EmuStepper) -> u64 {
    let ram: Vec<u8> = (0..0x2_0000u32)
        .map(|a| st.peek_bus(0x7E_0000 + a))
        .collect();
    fnv(&ram)
}

/// **Mode-invariant:** the machine is the same with the whole no-latency chain on as off.
#[test]
#[ignore = "needs the SMW ROM"]
fn the_no_latency_chain_changes_nothing_the_machine_does() {
    let recipe = recipe();
    let results: Vec<_> = [false, true]
        .iter()
        .map(|&on| {
            let (mut st, mut sink) = machine(on)?;
            let mut ticks = Ticks::from_recipe(&recipe).expect("the recipe has the rows");
            let mut presenter = Presenter::new();
            presenter.alpha_override = Some(128);
            reach_yi1(&mut st, &mut sink);
            let mut frames = Vec::new();
            let t0 = Instant::now();
            for f in 0..900u32 {
                run(&mut st, &mut sink, &play(f), 1);
                frames.push(fnv(sink.rgba()));
                if on {
                    let c = grab(&st, &sink, Some(&mut ticks)).expect("captures");
                    let at = t0 + Duration::from_micros(16_639 * u64::from(f));
                    let _ = presenter.arrive(c, at);
                    let _ = presenter.intermediate(at + Duration::from_millis(8));
                }
            }
            Some((frames, wram_print(&st), st.frame_count()))
        })
        .collect();
    let (Some(off), Some(on)) = (&results[0], &results[1]) else {
        return;
    };
    assert_eq!(off.0, on.0, "every console frame identical");
    assert_eq!(off.1, on.1, "all of WRAM identical");
    assert_eq!(off.2, on.2, "frame count identical");
    println!(
        "{} frames: pictures, WRAM (128 KiB) and frame count identical with captures, tick reads and the Presenter on and off",
        off.0.len()
    );
}

/// Counts what became of every tick of a run and checks the two claims about pictures.
#[test]
#[ignore = "needs the SMW ROM; run with --release"]
fn smw_yi1_ticks_latency_and_fidelity() {
    let Some((mut st, mut sink)) = machine(true) else {
        return;
    };
    let mut ticks = Ticks::from_recipe(&recipe()).expect("the recipe has the rows");
    reach_yi1(&mut st, &mut sink);

    // ---- latency: stand still, then press Right ----
    let mut nolat = Presenter::new();
    let mut delayed = Presenter::new();
    let t0 = Instant::now();
    let (mut shown_nl, mut shown_dl): (Vec<u64>, Vec<u64>) = (Vec::new(), Vec::new());
    // Picture print -> the print of its sprite dots, for the frames of this stretch.
    let mut sprites_of: BTreeMap<u64, u64> = BTreeMap::new();
    let mut press_at = None;
    let mut player_x = Vec::new();
    for f in 0..120u32 {
        let buttons = if f >= 60 {
            vec![SnesButton::Right]
        } else {
            vec![]
        };
        if f == 60 {
            press_at = Some(shown_nl.len());
        }
        run(&mut st, &mut sink, &buttons, 1);
        player_x.push(u16::from(st.peek_bus(0x7E_0094)) | u16::from(st.peek_bus(0x7E_0095)) << 8);
        let c = grab(&st, &sink, Some(&mut ticks)).expect("captures");
        let mut d = c.clone();
        if let Captured::Snes(l) = &mut d {
            l.tick = None; // the W37-06 path: no verdict, the frame is held back
        }
        let at = t0 + Duration::from_micros(16_639 * u64::from(f));
        sprites_of.insert(fnv(c.rgba()), player_print(&st, &c));
        shown_nl.push(sprites_of[&fnv(&nolat.arrive(c, at))]);
        shown_dl.push(sprites_of[&fnv(&delayed.arrive(d, at))]);
    }
    assert!(nolat.is_no_latency() && !delayed.is_no_latency());
    let press = press_at.expect("pressed");
    let first_change = |shown: &[u64]| (press..shown.len()).find(|&i| shown[i] != shown[press - 1]);
    let (nl, dl) = (first_change(&shown_nl), first_change(&shown_dl));
    let moved_tick = (press..player_x.len()).find(|&i| player_x[i] != player_x[press]);
    println!(
        "pressed at arrival {press}: Mario's RAM x first changes at arrival {moved_tick:?}; \
         first changed picture shown: no-latency {nl:?}, W37-06 delay {dl:?}"
    );
    let (nl, dl) = (
        nl.expect("no-latency shows motion"),
        dl.expect("delay shows motion"),
    );
    assert_eq!(dl, nl + 1, "the delayed path is exactly one arrival later");

    // ---- the run: tick verdicts and fidelity ----
    let mut verdicts: BTreeMap<String, u32> = BTreeMap::new();
    let mut presenter = Presenter::new();
    presenter.alpha_override = Some(255);
    let mut prev: Option<Captured> = None;
    let (mut ex_dots_wrong, mut rep_dots_wrong, mut pairs, mut cuts) = (0usize, 0usize, 0u32, 0u32);
    let (mut ex_scroll, mut rep_scroll, mut scroll_pairs) = (0usize, 0usize, 0u32);
    let (mut in_level_prev, mut moving_prev, mut last_hofs) = (false, false, 0u16);
    let mut modes: BTreeMap<u8, u32> = BTreeMap::new();
    let mut extrapolated = 0u32;
    let mut sample: Option<(Vec<u8>, Vec<u8>, Vec<u8>)> = None;
    let mut pending: Option<(Vec<u8>, Vec<u8>)> = None; // (extrapolated, current) awaiting the next frame
    let t0 = Instant::now();
    for f in 0..3000u32 {
        run(&mut st, &mut sink, &hop(f), 1);
        let Some(c) = grab(&st, &sink, Some(&mut ticks)) else {
            prev = None;
            pending = None;
            continue;
        };
        if let Captured::Snes(l) = &c {
            let key = match &l.tick {
                Some(Verdict::Extrapolate(_)) => "extrapolate".to_string(),
                Some(Verdict::Snap(r)) => format!("snap:{r:?}"),
                None => "none".to_string(),
            };
            *verdicts.entry(key).or_default() += 1;
        }
        // The previous gap's extrapolation, judged against the frame that actually followed it.
        if let Some((ex, cur)) = pending.take() {
            let truth = c.rgba();
            let wrong = |a: &[u8]| {
                a.chunks_exact(4)
                    .zip(truth.chunks_exact(4))
                    .filter(|(p, q)| p[..3] != q[..3])
                    .count()
            };
            // Pairs judged: not a cut the game made (a full-screen change nothing could predict).
            let (er, rr) = (wrong(&ex), wrong(&cur));
            if rr > 30_000 {
                cuts += 1;
            } else {
                ex_dots_wrong += er;
                rep_dots_wrong += rr;
                if in_level_prev && moving_prev {
                    scroll_pairs += 1;
                    ex_scroll += er;
                    rep_scroll += rr;
                }
            }
            pairs += 1;
            if sample.is_none() && wrong(&cur) > 3000 && wrong(&ex) < wrong(&cur) / 2 {
                sample = Some((cur, ex, truth.to_vec()));
            }
        }
        let mode = st.peek_bus(0x7E_0100);
        *modes.entry(mode).or_default() += 1;
        let hofs = word(&st, 0x7E_001A) as u16;
        in_level_prev = mode == 0x14;
        moving_prev = hofs != last_hofs;
        last_hofs = hofs;
        let at = t0 + Duration::from_micros(16_639 * u64::from(f));
        let cur_rgba = presenter.arrive(c.clone(), at);
        if let Some((px, shown)) = presenter.intermediate(at + Duration::from_millis(16)) {
            assert!(matches!(shown, Shown::Extrapolated { .. }));
            extrapolated += 1;
            pending = Some((px.to_vec(), cur_rgba));
        }
        prev = Some(c);
    }
    let _ = prev;
    println!("tick verdicts over 3000 frames: {verdicts:?}");
    println!(
        "{extrapolated} frames extrapolated; against the console frame that really followed, over {pairs} pairs: \
         extrapolated {ex_dots_wrong} wrong dots, repeating the current frame {rep_dots_wrong} wrong dots ({:.1}% of the repeat error)",
        100.0 * ex_dots_wrong as f64 / rep_dots_wrong.max(1) as f64
    );
    println!("pairs the game cut on (not judged): {cuts}; game modes seen: {modes:?}");
    println!(
        "level frames with the camera moving: {scroll_pairs} pairs, extrapolated {ex_scroll} wrong dots, repeat {rep_scroll} wrong dots"
    );
    println!("refusals: {:?}", presenter.refusals);
    assert!(
        extrapolated > 300,
        "the run was mostly extrapolated ({extrapolated})"
    );
    assert!(
        ex_dots_wrong < rep_dots_wrong,
        "extrapolation is nearer the truth than a repeat"
    );
    assert!(
        scroll_pairs > 200,
        "the camera scrolled in the run ({scroll_pairs})"
    );
    assert!(
        ex_scroll * 2 < rep_scroll,
        "while the camera scrolls, extrapolating halves the error of repeating the frame"
    );
    if let (Some(dir), Some((cur, ex, truth))) = (std::env::var_os("W39_01_SAMPLE_DIR"), sample) {
        for (name, px) in [
            ("a-current", &cur),
            ("b-extrapolated", &ex),
            ("c-next", &truth),
        ] {
            let mut opaque = px.clone();
            opaque.chunks_exact_mut(4).for_each(|q| q[3] = 255);
            image::save_buffer(
                Path::new(&dir).join(format!("smw-{name}.png")),
                &opaque,
                W as u32,
                (px.len() / 4 / W) as u32,
                image::ColorType::Rgba8,
            )
            .expect("png");
        }
    }
}
