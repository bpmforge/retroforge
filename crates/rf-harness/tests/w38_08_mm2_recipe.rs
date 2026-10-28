//! **W38-08: the finders on Mega Man 2 (NES)** (ROM-gated, law 5, D-021).
//! Every fact in `profiles/nes/mega-man-2/recipe.toml` comes from this test's
//! observation of the running game over the scripted play of
//! `w38_08_mm2_common`; no disassembly, no community RAM map, no
//! decompilation, no address read from the repo's older profile. Only offsets,
//! counts and verdicts are printed.
//!
//! Stages:
//! 1. atlas from the run's coverage;
//! 2. a trace (RAM and OAM at each OAM upload, as W38-02);
//! 3. the player's x byte (the byte that steps with Right and Left and idles
//!    otherwise, minus the byte that is the scroll), the object table around it
//!    (the period of the "slot in use" pattern, then the role of each array:
//!    x high steps by one when x wraps, y matches a sprite's y, state), the
//!    camera and its page;
//! 4. an observed run that attributes writes to those bytes to atlas
//!    functions, and to the NMI or the main thread;
//! 5. the W38-04 widescreen finder and its lockstep trials over the same play;
//! 6. the recipe, written with `RF_W38_WRITE=1`.
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_08_mm2_recipe -- --ignored --nocapture
//! RF_W38_WRITE=1 cargo test --release -p rf-harness --test w38_08_mm2_recipe -- --ignored --nocapture
//! ```

mod w38_08_mm2_common;

use std::collections::{BTreeMap, BTreeSet};

use rf_core_api::{BusAccess, EmulatorCore, ResetKind, StepFacts, StepObserver};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::data_regions::NesDriver;
use rf_harness::object_tables::{nes_cam, nes_prepare, nes_step_frame, Console, Recorder};
use rf_harness::widescreen::{
    nes_patch, nes_snaps, observe_nes, rows_toml, try_candidates, try_windows, TrialSetup,
};
use rf_nes::core::NesCore;
use rf_rebuild::objtable::{Frame, Trace};
use rf_rebuild::widescreen::{
    camera_aliases, find_camera, hud_watch, infer as infer_wide, infer_all, oam_shadow,
    static_sprites, widen_values, window_pairs, Candidate, Kind, View, Watch as WideWatch,
    WideFacts,
};
use rf_rebuild::{Atlas, Grade, HookPoints, ObjectTable, RamMeaning, Recipe, RecipeFunction};
use w38_08_mm2_common::{
    forest_script, inputs_of, metal_script, load_rom, poke, rom_path, NullSink, FILE, PLAY_FROM,
};

const RECIPE_DIR: &str = "../../profiles/nes/mega-man-2";
const TRIALS: usize = 16;

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One power-on run.
struct Seg {
    name: &'static str,
    pads: Vec<u8>,
}

fn session() -> Vec<Seg> {
    vec![
        Seg {
            name: "forest-patrol",
            pads: forest_script(),
        },
        Seg {
            name: "conveyor-stage",
            pads: metal_script(),
        },
    ]
}

fn ins_of(seg: &Seg) -> Vec<rf_core_api::InputFrame> {
    inputs_of(&seg.pads)
}

// ---------------------------------------------------------------------------
// Stage 1-2: atlas and trace.
// ---------------------------------------------------------------------------

fn coverage(raw: &[u8], segs: &[Seg]) -> rf_core_api::coverage::CoverageLog {
    let mut all: Option<rf_core_api::coverage::CoverageLog> = None;
    for seg in segs {
        let mut core = NesCore::from_ines_bytes(raw).expect("loads");
        core.set_coverage_recording(true);
        core.reset(ResetKind::Hard);
        for (f, input) in ins_of(seg).iter().enumerate() {
            poke(f, &mut core);
            core.run_frame(input, &mut NullSink);
        }
        let log = core.take_coverage().expect("armed");
        match all.as_mut() {
            None => all = Some(log),
            Some(a) => assert!(a.merge(&log), "coverage logs of one cartridge merge"),
        }
    }
    all.expect("a segment")
}

fn trace_of(raw: &[u8], seg: &Seg) -> Trace {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.reset(ResetKind::Hard);
    nes_prepare(&mut core);
    let mut rec = Recorder::new(Console::Nes, false);
    for (f, input) in ins_of(seg).iter().enumerate() {
        poke(f, &mut core);
        let cap = nes_step_frame(&mut core, input);
        if f as u32 >= PLAY_FROM {
            rec.push(cap, nes_cam(&core));
        }
    }
    rec.trace
}

/// Plain snaps of every segment, concatenated.
fn snaps_of(image: &[u8], segs: &[Seg]) -> Result<Vec<rf_rebuild::widescreen::Snap>, String> {
    let mut out = Vec::new();
    for seg in segs {
        out.extend(nes_snaps(image, &ins_of(seg), poke)?);
    }
    Ok(out)
}

/// Share of `frames` for which `pred` holds.
fn share(frames: &[Frame], pred: impl Fn(&Frame) -> bool) -> f64 {
    frames.iter().filter(|f| pred(f)).count() as f64 / frames.len().max(1) as f64
}

// ---------------------------------------------------------------------------
// Stage 3: the player, the table, the camera.
// ---------------------------------------------------------------------------

/// Bytes that step by +1..+3 while Right is held and by -1..-3 while Left is
/// held, best first, and that hold still when no direction is held. `pad` is
/// the pad byte of each trace frame.
fn walkers(t: &Trace, pad: &[u8]) -> Vec<(usize, f64, f64, f64)> {
    let n = t.frames.len();
    let mut out = Vec::new();
    for a in 0..0x800usize {
        let (mut r_up, mut r_n, mut l_dn, mut l_n, mut idle_ch, mut idle_n) = (0, 0, 0, 0, 0, 0);
        for i in 1..n {
            let pd = pad[i];
            let d = t.frames[i].ram[a].wrapping_sub(t.frames[i - 1].ram[a]);
            if pd & 128 != 0 && pd & 64 == 0 {
                r_n += 1;
                r_up += i32::from((1..=3).contains(&d));
            } else if pd & 64 != 0 && pd & 128 == 0 {
                l_n += 1;
                l_dn += i32::from((253..=255).contains(&d));
            } else if pd & 0xC0 == 0 {
                idle_n += 1;
                idle_ch += i32::from(d != 0);
            }
        }
        let rs = f64::from(r_up) / f64::from(r_n.max(1));
        let ls = f64::from(l_dn) / f64::from(l_n.max(1));
        let is = f64::from(idle_ch) / f64::from(idle_n.max(1));
        if rs > 0.4 && ls > 0.4 && is < 0.2 {
            out.push((a, rs, ls, is));
        }
    }
    out.sort_by(|x, y| (y.1 + y.2 - 3.0 * y.3).total_cmp(&(x.1 + x.2 - 3.0 * x.3)));
    out
}

/// The facts about the object table around the player's x byte.
#[derive(Debug, Clone)]
struct Table {
    /// First byte of the array that starts the table (the lowest role array).
    base: usize,
    pitch: usize,
    x_hi: usize,
    x_lo: usize,
    y: usize,
    state: usize,
    y_share: f64,
    y_dy: i32,
    hi_steps: (usize, usize),
    state_agree: f64,
    period_corr: f64,
}

fn table_around(t: &Trace, px: usize) -> Table {
    let usage: Vec<f64> = (0..0x800usize)
        .map(|a| share(&t.frames, |f| f.ram[a] != 0))
        .collect();
    let corr = |pp: usize| -> f64 {
        let (lo, hi) = (px.saturating_sub(0x60), (px + 0x60).min(0x7ff - pp));
        let n = (hi - lo) as f64;
        let (mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for a in lo..hi {
            let (x, y) = (usage[a], usage[a + pp]);
            sx += x;
            sy += y;
            sxx += x * x;
            syy += y * y;
            sxy += x * y;
        }
        let cov = sxy / n - sx / n * sy / n;
        let vx = sxx / n - (sx / n).powi(2);
        let vy = syy / n - (sy / n).powi(2);
        cov / (vx * vy).sqrt().max(1e-9)
    };
    let (pitch, period_corr) = [8usize, 16, 32, 64]
        .into_iter()
        .map(|p| (p, corr(p)))
        .fold((8, f64::MIN), |b, c| if c.1 > b.1 { c } else { b });
    let wraps: Vec<usize> = (1..t.frames.len())
        .filter(|&i| {
            (i32::from(t.frames[i - 1].ram[px]) - i32::from(t.frames[i].ram[px])).abs() > 128
        })
        .collect();
    // The x-high array steps by one at the player's wraps.
    let mut hi_best = (0usize, 0usize);
    let mut y_best = (0usize, 0.0f64, 0i32);
    for k in -6i32..=6 {
        let a = px as i32 + k * pitch as i32;
        if k == 0 || a < 0 || a as usize >= 0x800 {
            continue;
        }
        let a = a as usize;
        let ok = wraps
            .iter()
            .filter(|&&i| {
                let d = i32::from(t.frames[i].ram[a]) - i32::from(t.frames[i - 1].ram[a]);
                d.abs() == 1
            })
            .count();
        if ok > hi_best.1 {
            hi_best = (a, ok);
        }
        // y: the array whose slot-0 byte plus a constant is a sprite's y.
        let mut best = (0.0f64, 0i32);
        for dy in -40i32..=40 {
            let s = share(&t.frames, |f| {
                f.sprites
                    .iter()
                    .any(|sp| i32::from(sp.y) == i32::from(f.ram[a]) + dy)
            });
            if s > best.0 {
                best = (s, dy);
            }
        }
        // Reject arrays that match because they hold a constant.
        let varies = share(&t.frames, |f| f.ram[a] != t.frames[0].ram[a]) > 0.2;
        if varies && best.0 > y_best.1 {
            y_best = (a, best.0, best.1);
        }
    }
    let x_hi = hi_best.0;
    // The state array: not a position array, and in use (non-zero) exactly in
    // the slot-frames where the x-high array is.
    let mut state_best = (0usize, -1.0f64);
    for k in -6i32..=6 {
        let a = px as i32 + k * pitch as i32;
        if a < 0 || a as usize + pitch > 0x800 {
            continue;
        }
        let a = a as usize;
        if [px, x_hi, y_best.0].contains(&a) {
            continue;
        }
        let agree = t
            .frames
            .iter()
            .step_by(7)
            .map(|f| {
                (0..pitch)
                    .filter(|&i| (f.ram[a + i] != 0) == (f.ram[x_hi + i] != 0))
                    .count() as f64
                    / pitch as f64
            })
            .sum::<f64>()
            / (t.frames.len().div_ceil(7)) as f64;
        // The play-time "in use" byte is non-zero for player slot 0 nearly always.
        if agree > state_best.1 + 1e-9 {
            state_best = (a, agree);
        }
    }
    let first = [px, x_hi, y_best.0, state_best.0]
        .into_iter()
        .min()
        .unwrap_or(px);
    Table {
        base: first,
        pitch,
        x_hi,
        x_lo: px,
        y: y_best.0,
        state: state_best.0,
        y_share: y_best.1,
        y_dy: y_best.2,
        hi_steps: (hi_best.1, wraps.len()),
        state_agree: state_best.1,
        period_corr,
    }
}

/// Bytes that equal the right edge's page: `(cam + 255) >> 8` in 95% of frames
/// and differ from the left page in at least a tenth of them.
fn right_page_bytes(t: &Trace, lo: usize, hi: usize) -> Vec<usize> {
    let cam = |f: &Frame| i32::from(f.ram[hi]) * 256 + i32::from(f.ram[lo]);
    (0..0x800usize)
        .filter(|&a| a != lo && a != hi)
        .filter(|&a| {
            share(&t.frames, |f| {
                i32::from(f.ram[a]) == ((cam(f) + 255) >> 8) & 0xFF
            }) >= 0.95
                && share(&t.frames, |f| f.ram[a] != f.ram[hi]) >= 0.1
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stage 4: who writes the bytes.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct WriteWatch {
    addrs: BTreeSet<u32>,
    in_interrupt: bool,
    hits: BTreeMap<(u32, Option<u32>, bool), u32>,
    nmis: u32,
}

impl StepObserver for WriteWatch {
    fn step(&mut self, facts: StepFacts, accesses: &[BusAccess]) {
        if facts.interrupt {
            self.in_interrupt = true;
            self.nmis += 1;
        }
        for a in accesses.iter().filter(|a| a.write) {
            if self.addrs.contains(&a.addr) {
                *self
                    .hits
                    .entry((a.addr, facts.function, self.in_interrupt))
                    .or_default() += 1;
            }
        }
        if !facts.interrupt && accesses.first().is_some_and(|a| a.value == 0x40) {
            self.in_interrupt = false;
        }
    }
}

fn writers(raw: &[u8], atlas: &Atlas, seg: &Seg, addrs: &[u32]) -> WriteWatch {
    let mut d = NesDriver::new(raw, atlas).expect("loads");
    let mut w = WriteWatch {
        addrs: addrs.iter().copied().collect(),
        ..WriteWatch::default()
    };
    for (f, input) in ins_of(seg).iter().enumerate() {
        poke(f, &mut d.core);
        if (f as u32) < PLAY_FROM {
            d.frame(input, false).expect("frame");
        } else {
            d.frame_with(input, &mut w).expect("frame");
        }
    }
    w
}

fn function_row(
    atlas: &Atlas,
    id: u32,
    name: &str,
    hooks: HookPoints,
    source: &str,
) -> RecipeFunction {
    let f = &atlas.functions[id as usize];
    RecipeFunction {
        name: name.to_string(),
        shape_hash: f.shape_hash,
        exact_hash: Some(f.byte_hash),
        address_hint: Some(f.entry_cpu),
        entry: rf_rebuild::EntryKind::sole_interrupt(f.entry_kinds),
        insn_count: Some(f.insn_count),
        hooks,
        source: source.to_string(),
        grade: Grade::Observed,
        ..RecipeFunction::default()
    }
}

#[test]
#[ignore = "needs Mega Man 2 (USA) (RF_SCREENS_ROMS_DIR); many minutes in release"]
fn mm2_finders_and_recipe() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let segs = session();
    println!(
        "session: {} frames in {} power-on segments {:?}",
        segs.iter().map(|s| s.pads.len()).sum::<usize>(),
        segs.len(),
        segs.iter()
            .map(|s| (s.name, s.pads.len()))
            .collect::<Vec<_>>()
    );
    let t0 = std::time::Instant::now();
    let atlas = build_nes_atlas(&raw, &coverage(&raw, &segs)).expect("atlas");
    println!(
        "atlas: {} functions in {:?}",
        atlas.functions.len(),
        t0.elapsed()
    );
    let trace = trace_of(&raw, &segs[0]);
    println!("trace: {} frames", trace.frames.len());

    // The player's x byte.
    let pad: Vec<u8> = segs[0].pads[PLAY_FROM as usize..].to_vec();
    let walk = walkers(&trace, &pad);
    for w in walk.iter().take(6) {
        println!(
            "WALKER {:#x} right-steps {:.2} left-steps {:.2} idle-change {:.3}",
            w.0, w.1, w.2, w.3
        );
    }
    // The camera: the scroll byte (the one that equals the PPU scroll's low
    // byte) and its page, from the camera finder.
    let cam = find_camera(&trace).expect("a camera");
    let (cam_lo, cam_hi) = (cam.lo, cam.hi.expect("a camera page"));
    println!("camera lo {:#x} hi {:#x}", cam_lo, cam_hi);
    let px = walk
        .iter()
        .map(|w| w.0)
        .find(|&a| a != cam_lo && Some(a) != cam.hi)
        .expect("a player x byte");
    let tb = table_around(&trace, px);
    println!(
        "TABLE pitch {} (period corr {:.3}) x_lo {:#x} x_hi {:#x} ({} of {} wraps step by one) y {:#x} ({:.2} of frames, dy {}) state {:#x} (agrees {:.3})",
        tb.pitch, tb.period_corr, tb.x_lo, tb.x_hi, tb.hi_steps.0, tb.hi_steps.1, tb.y, tb.y_share, tb.y_dy, tb.state, tb.state_agree
    );
    let rights = right_page_bytes(&trace, cam_lo, cam_hi);
    println!("right-edge page bytes {rights:x?}");
    // The player slot's x page equals the camera page plus a constant? Report.
    let rel = |f: &Frame| {
        (i32::from(f.ram[tb.x_hi]) * 256 + i32::from(f.ram[px]))
            - (i32::from(f.ram[cam_hi]) * 256 + i32::from(f.ram[cam_lo]))
    };
    let on = share(&trace.frames, |f| (0..256).contains(&rel(f)));
    println!("player x minus camera is in 0..256 in {on:.3} of frames");

    let table = ObjectTable {
        name: "objects".into(),
        base: tb.base as u32,
        slots: tb.pitch as u32,
        stride: 1,
        x_lo: Some((tb.x_lo - tb.base) as u32),
        x_hi: Some((tb.x_hi - tb.base) as u32),
        y: Some((tb.y - tb.base) as u32),
        state: Some((tb.state - tb.base) as u32),
        grade: Grade::Observed,
        ..ObjectTable::default()
    };
    let statics = static_sprites(&trace);
    let shadow = oam_shadow(&trace);
    println!(
        "static sprites {:?}; oam shadow {shadow:?}",
        statics
            .iter()
            .map(|s| (s.index, s.x, s.y))
            .collect::<Vec<_>>()
    );

    // Stage 4: writers.
    let (pxa, pya) = (tb.x_lo as u32, tb.y as u32);
    let mut watched: Vec<u32> = vec![pxa, pya, cam_lo as u32, cam_hi as u32];
    watched.extend((1..tb.pitch as u32).map(|i| pxa + i));
    watched.extend((1..tb.pitch as u32).map(|i| pya + i));
    let w = writers(&raw, &atlas, &segs[0], &watched);
    println!("{} NMI sequences in the observed play", w.nmis);
    let inside_nmi: u32 = w
        .hits
        .iter()
        .filter(|((_, _, irq), _)| *irq)
        .map(|(_, c)| *c)
        .sum();
    let outside_nmi: u32 = w
        .hits
        .iter()
        .filter(|((_, _, irq), _)| !irq)
        .map(|(_, c)| *c)
        .sum();
    println!(
        "writes to the watched bytes: {inside_nmi} while an NMI handler ran, {outside_nmi} outside"
    );
    for ((addr, f, irq), c) in &w.hits {
        if *addr == pxa || *addr == cam_lo as u32 {
            println!(
                "WRITE {addr:#x} by fn {:?} (entry {:#x}) in_interrupt {irq}: {c}",
                f,
                f.map_or(0, |f| atlas.functions[f as usize].entry_cpu)
            );
        }
    }
    let writers_of = |addr: u32| -> BTreeSet<u32> {
        w.hits
            .keys()
            .filter(|(a, f, _)| *a == addr && f.is_some())
            .filter_map(|(_, f, _)| *f)
            .collect()
    };
    let count_of = |addr: u32, f: u32| -> u32 {
        w.hits
            .iter()
            .filter(|((a, g, _), _)| *a == addr && *g == Some(f))
            .map(|(_, c)| *c)
            .sum()
    };

    // Stage 5: widescreen candidates and trials.
    let mut watch = WideWatch::default();
    for i in 0..table.slots {
        watch.obj_x.insert(table.base + table.x_lo.unwrap() + i);
        watch.obj_x.insert(table.base + table.x_hi.unwrap() + i);
        watch
            .obj_state
            .insert(table.base + table.state.unwrap() + i);
    }
    watch.cam.insert(cam_lo as u32);
    watch.cam.insert(cam_hi as u32);
    let aliases = camera_aliases(&trace, &cam);
    println!("camera aliases {aliases:x?}");
    watch.cam.extend(aliases.iter().map(|&a| a as u32));
    if let Some(sh) = shadow {
        watch.hud = hud_watch(&trace, sh, &statics);
    }
    let t = std::time::Instant::now();
    let mut facts = WideFacts::default();
    for seg in &segs {
        let f = observe_nes(
            &raw,
            &atlas,
            &ins_of(seg),
            PLAY_FROM as usize,
            watch.clone(),
            poke,
        )
        .expect("observed run");
        println!(
            "segment {}: {} frames, {} flips up / {} down",
            seg.name,
            seg.pads.len(),
            f.flips_up,
            f.flips_down
        );
        facts.merge(&f);
    }
    let cands = infer_wide(&facts);
    let all_cands = infer_all(&facts);
    println!(
        "widescreen observed {} frames in {:?}: {} sites, {} flips up/{} down, {} cam writes, {} candidates",
        facts.frames, t.elapsed(), facts.sites.len(), facts.flips_up, facts.flips_down, facts.cam_writes, cands.len()
    );
    for c in &cands {
        println!(
            "CAND {:<16} rom {:#07x} fn {:?} {:?} w{} imm {:#06x} score {:>3} {} [{}]",
            c.kind.name(),
            c.site,
            c.function,
            c.op.map(|o| o.name()),
            c.width,
            c.imm,
            c.score,
            c.band.name(),
            c.evidence
        );
    }
    let plain = snaps_of(&raw, &segs).expect("plain run");
    let view = View {
        ram_base: 0,
        table: table.clone(),
        cam_lo,
        cam_hi: Some(cam_hi),
        x_page_zero: false,
    };
    let trials = env_usize("TRIALS", TRIALS);
    let chosen: Vec<&Candidate> = cands
        .iter()
        .filter(|c| c.kind != Kind::ScreenConst && !widen_values(c).is_empty())
        .take(trials)
        .collect();
    let setup = TrialSetup {
        raw: &raw,
        view: &view,
        plain: &plain,
        patch: &|r, at, b| nes_patch(r, at, b),
        run: &|image| snaps_of(image, &segs),
        values: 2,
    };
    let t = std::time::Instant::now();
    let mut tried = try_candidates(&setup, &chosen, &mut |l| println!("{l}")).expect("trials");
    println!("{} candidates tried in {:?}", tried.len(), t.elapsed());
    let pairs = window_pairs(&all_cands);
    println!(
        "{} adc/cmp window pairs among {} candidates",
        pairs.len(),
        all_cands.len()
    );
    let windows = try_windows(
        &TrialSetup { values: 1, ..setup },
        &pairs,
        &[0x20u16, 0x40, 0x80],
        &mut |l| println!("{l}"),
    )
    .expect("window trials");
    for wt in windows {
        match tried.iter().position(|t| t.cand.operand == wt.cand.operand) {
            Some(i) if wt.best.is_some() => tried[i] = wt,
            Some(_) => {}
            None => tried.push(wt),
        }
    }
    for t in &tried {
        println!(
            "RESULT {:<16} (reads said {:<16}) rom {:#07x} grade {} value {:?} partners {:x?}",
            t.kind().name(),
            t.cand.kind.name(),
            t.cand.operand,
            t.grade().as_str(),
            t.best.as_ref().map(|b| b.0),
            t.partners
        );
    }
    for t in tried.iter().filter(|t| t.grade() != Grade::Confirmed) {
        for (v, tr) in &t.alone {
            let d = tr.first.as_ref();
            println!(
                "NOTKEPT rom {:#07x} {:<16} {:#06x}->{:#06x}: {} (first divergence frame {:?}, off {} on {} other {} oam {} cam {}; reach R{} L{})",
                t.cand.operand,
                t.cand.kind.name(),
                t.cand.imm,
                v,
                tr.why_not(),
                d.map(|d| d.frame),
                d.map_or(0, |d| d.off_screen.len()),
                d.map_or(0, |d| d.on_screen.len()),
                d.map_or(0, |d| d.bookkeeping.len()),
                d.is_some_and(|d| d.oam),
                d.is_some_and(|d| d.camera),
                tr.gain_right,
                tr.gain_left
            );
        }
    }
    println!(
        "KEEPERS {:x?}",
        tried
            .iter()
            .filter(|t| t.grade() == Grade::Confirmed)
            .map(|t| t.cand.operand)
            .collect::<Vec<_>>()
    );

    // Stage 6: the recipe.
    let mut recipe = Recipe {
        id: "mega-man-2".into(),
        engine: "mega-man-2".into(),
        ..Recipe::default()
    };
    let nmis: Vec<u32> = atlas
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            rf_rebuild::EntryKind::sole_interrupt(f.entry_kinds) == Some(rf_rebuild::EntryKind::Nmi)
        })
        .map(|(i, _)| i as u32)
        .collect();
    assert!(!nmis.is_empty(), "an NMI function");
    println!("{} atlas function(s) with the NMI entry kind", nmis.len());
    let both = HookPoints {
        entry: true,
        exit: true,
    };
    let entry_only = HookPoints {
        entry: true,
        exit: false,
    };
    for (k, &id) in nmis.iter().enumerate() {
        let name = if k == 0 {
            "nmi".to_string()
        } else {
            format!("nmi_{}", (b'a' + k as u8) as char)
        };
        recipe.functions.push(function_row(&atlas, id, &name, both, &format!(
            "observed: an atlas function the NMI vector targets in a recording (the NMI may be entered in several decode modes, so every one is hooked); {inside_nmi} of {} writes to the player and object bytes and the scroll happened while an NMI handler ran, {outside_nmi} outside it",
            inside_nmi + outside_nmi)));
    }
    let player_fn = writers_of(pxa)
        .into_iter()
        .filter(|f| !writers_of(pxa + 1).contains(f))
        .max_by_key(|&f| count_of(pxa, f));
    if let Some(f) = player_fn {
        recipe.functions.push(function_row(
            &atlas,
            f,
            "player_move",
            both,
            &format!(
                "observed: writes the player's x ({} writes in the play) and never the next slot's",
                count_of(pxa, f)
            ),
        ));
    }
    let all_slots = |f: &u32| {
        (0..tb.pitch as u32).all(|i| i == 0 || writers_of(pxa + i).contains(f))
            || (1..tb.pitch as u32)
                .filter(|&i| writers_of(pxa + i).contains(f))
                .count()
                >= 3
    };
    let slot_fn = writers_of(pxa + 1)
        .into_iter()
        .filter(all_slots)
        .max_by_key(|&f| {
            (1..tb.pitch as u32)
                .map(|i| count_of(pxa + i, f))
                .sum::<u32>()
        });
    if let Some(f) = slot_fn {
        recipe.functions.push(function_row(&atlas, f, "object_move", both, "observed: writes the x of several object slots after the player's, the shared per-slot mover"));
    }
    let top_cam = w
        .hits
        .iter()
        .filter(|((a, f, _), _)| *a == cam_lo as u32 && f.is_some())
        .max_by_key(|(_, c)| **c)
        .and_then(|((_, f, _), _)| *f);
    if let Some(f) = top_cam {
        recipe.functions.push(function_row(
            &atlas,
            f,
            "scroll_step",
            entry_only,
            &format!(
                "observed: the most frequent writer of the scroll byte ({} writes in the play)",
                count_of(cam_lo as u32, f)
            ),
        ));
    }
    let tuples = recipe.functions.len();
    println!("{tuples} observed function rows");

    let ram = |addr: u32, width: u8, name: &str, source: String| RamMeaning {
        addr,
        width,
        name: name.into(),
        source,
        grade: Grade::Observed,
    };
    let walk0 = &walk[0];
    recipe.ram.push(ram(pxa, 1, "player_x", format!("observed: steps by 1 to 3 with Right in {:.0}% and by -1 to -3 with Left in {:.0}% of the frames, changes in {:.1}% of frames with no direction held (walker finder); its screen position is this plus the page minus the camera", walk0.1 * 100.0, walk0.2 * 100.0, walk0.3 * 100.0)));
    recipe.ram.push(ram(
        tb.x_hi as u32,
        1,
        "player_x_page",
        format!(
            "observed: steps by one at {} of the {} wraps of the player's x byte",
            tb.hi_steps.0, tb.hi_steps.1
        ),
    ));
    recipe.ram.push(ram(pya, 1, "player_y", format!("observed: plus {} equals a sprite's y in {:.0}% of the frames; the player is slot 0 of the object table (period {} found from the slot-in-use pattern, corr {:.2})", tb.y_dy, tb.y_share * 100.0, tb.pitch, tb.period_corr)));
    recipe.ram.push(ram(
        cam_lo as u32,
        1,
        "scroll_x",
        "observed: equals the low byte of the PPU's horizontal scroll (camera finder)".into(),
    ));
    recipe.ram.push(ram(cam_hi as u32, 1, "scroll_page", "observed: the camera's high byte (camera finder): the player's page minus this and its x minus the low byte put the player on the picture".into()));
    // The margin vocabulary of `retroforge::margin_draw` (the SMB names), as
    // far as observation reaches.
    recipe.ram.push(ram(table.base + table.state.unwrap(), tb.pitch as u8, "enemy_flag", format!("observed: the array (one byte per slot) whose non-zero pattern matches the x-high array's in {:.0}% of the slot-frames", tb.state_agree * 100.0)));
    recipe.ram.push(ram(
        tb.x_hi as u32,
        tb.pitch as u8,
        "enemy_x_page",
        "observed: the x-high array of the object table (steps by one when x wraps)".into(),
    ));
    recipe.ram.push(ram(
        pxa,
        tb.pitch as u8,
        "enemy_x",
        "observed: the x-low array of the object table (slot 0 is the player)".into(),
    ));
    recipe.ram.push(ram(
        pya,
        tb.pitch as u8,
        "enemy_y",
        "observed: the y array of the object table".into(),
    ));
    recipe.ram.push(ram(
        cam_hi as u32,
        1,
        "screen_left_page",
        "observed: the camera's high byte".into(),
    ));
    recipe.ram.push(ram(
        cam_lo as u32,
        1,
        "screen_left_x",
        "observed: the camera's low byte".into(),
    ));
    if let Some(&r) = rights.first() {
        recipe.ram.push(ram(r as u32, 1, "screen_right_page", "observed: equals the page of the picture's right edge ((camera + 255) >> 8) in 95% of the frames and differs from the left page in a tenth of them".into()));
    }
    recipe.object_tables.push(ObjectTable {
        source: format!("observed: the player's x byte {pxa:#x} (walker finder); the table is {} slots of one byte per field (the period of the slot-in-use pattern, corr {:.2}), x high steps by one at x wraps ({} of {}), y plus {} equals a sprite's y in {:.0}% of frames, state agrees with x high in {:.0}% of slot-frames; the W38-02 finder does not bind this table (slots of different objects draw at different sprite offsets)", tb.pitch, tb.period_corr, tb.hi_steps.0, tb.hi_steps.1, tb.y_dy, tb.y_share * 100.0, tb.state_agree * 100.0),
        ..table.clone()
    });
    recipe.tiles = Vec::new();
    let text = rf_profiles::recipe::recipe_to_toml(&recipe);
    let back = rf_profiles::recipe::load_recipe_str(&text).expect("the recipe round-trips");
    assert_eq!(back, recipe);
    let report = rf_rebuild::match_recipe(&atlas, &recipe);
    println!(
        "recipe: {} functions ({} unmatched), {} ram, {} tables",
        recipe.functions.len(),
        report.unmatched.len(),
        recipe.ram.len(),
        recipe.object_tables.len()
    );
    assert!(report.unmatched.is_empty());
    let overlay = rows_toml(
        &atlas,
        &recipe,
        &tried,
        "widescreen-objects",
        "widescreen-candidates",
        &[],
        "Mega Man 2 forest-stage play",
    );
    if std::env::var("RF_W38_WRITE").is_ok() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(RECIPE_DIR);
        std::fs::write(dir.join("recipe.toml"), format!("{RECIPE_HEADER}\n{text}"))
            .expect("writes");
        std::fs::write(
            dir.join("recipe-widescreen.toml"),
            format!("{OVERLAY_HEADER}\n{overlay}"),
        )
        .expect("writes");
        println!("wrote the recipe and the overlay to {}", dir.display());
    }
}

const RECIPE_HEADER: &str = "# W38-08: engine recipe for Mega Man 2 (NES). Generated by\n# crates/rf-harness/tests/w38_08_mm2_recipe.rs (RF_W38_WRITE=1) from the repo's\n# own finders watching the running game (D-021: no disassembly, no community RAM\n# map or decompilation, no ROM bytes, no address from the older profile.toml).\n# Offsets and numbers only.";
const OVERLAY_HEADER: &str = "# W38-08: widescreen finder rows for Mega Man 2, an OVERLAY on recipe.toml\n# (append this file's text to the recipe to load it, as for SMB and Zelda).\n# Finder: crates/rf-rebuild/src/widescreen.rs; trials: w38_08_mm2_recipe.rs.\n# Every row is a candidate unless a lockstep trial kept it (see the grades).";
