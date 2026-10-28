//! **W38-05: the finders on The Legend of Zelda (NES)** (ROM-gated, law 5,
//! D-021). Every fact in `profiles/nes/legend-of-zelda/recipe.toml` comes from
//! this test's observation of the running game over the scripted play of
//! `w38_05_zelda_common`; no disassembly, no community RAM map, no
//! decompilation. Only offsets, counts and verdicts are printed.
//!
//! Stages:
//! 1. atlas from the run's coverage;
//! 2. a trace (RAM and OAM at each OAM upload, as W38-02): the object columns
//!    (x bytes that equal a sprite's x, y bytes that equal one's y plus a
//!    constant, grouped into runs), the scroll byte, the screen-index byte
//!    (the one byte that only ever moves by 1 or 16, at the screen changes);
//! 3. an observed run that attributes writes to those bytes to atlas
//!    functions, and to the NMI or the main thread;
//! 4. the W36 naming pass;
//! 5. the W38-01 data-region finder, W38-03 collision finder and W38-04
//!    widescreen finder over the same play;
//! 6. the recipe, written with `RF_W38_WRITE=1`.
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_05_zelda_recipe -- --ignored --nocapture
//! RF_W38_WRITE=1 cargo test --release -p rf-harness --test w38_05_zelda_recipe -- --ignored --nocapture
//! ```

mod w38_05_zelda_common;

use std::collections::{BTreeMap, BTreeSet};

use rf_core_api::{BusAccess, EmulatorCore, ResetKind, StepFacts, StepObserver};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::collision_tables::{run_nes as run_collision, Watch as CollisionWatch};
use rf_harness::data_regions::NesDriver;
use rf_harness::object_tables::{nes_cam, nes_prepare, nes_step_frame, Console, Recorder};
use rf_harness::widescreen::{
    nes_patch, nes_snaps, observe_nes, rows_toml, try_candidates, TrialSetup,
};
use rf_nes::core::NesCore;
use rf_rebuild::collision::infer as infer_collision;
use rf_rebuild::dataregion::{
    associate, grade_rows, infer as infer_regions, select_rows, DataFacts, RegionKind,
};
use rf_rebuild::objtable::{Frame, Trace};
use rf_rebuild::widescreen::{
    hud_watch, infer as infer_wide, oam_shadow, static_sprites, widen_values, Candidate, Kind,
    View, Watch as WideWatch,
};
use rf_rebuild::TileClass;
use rf_rebuild::{Atlas, Grade, HookPoints, ObjectTable, RamMeaning, Recipe, RecipeFunction};
use w38_05_zelda_common::{
    inputs, load_rom, rom_path, script, NullSink, DOWN, FILE, FRAMES, LEFT, PLAY_FROM, RIGHT, UP,
};

const RECIPE_DIR: &str = "../../profiles/nes/legend-of-zelda";

// ---------------------------------------------------------------------------
// Stage 1-2: atlas and trace.
// ---------------------------------------------------------------------------

fn coverage(raw: &[u8]) -> rf_core_api::coverage::CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for input in inputs(FRAMES) {
        core.run_frame(&input, &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

fn trace_of(raw: &[u8]) -> Trace {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.reset(ResetKind::Hard);
    nes_prepare(&mut core);
    let mut rec = Recorder::new(Console::Nes, false);
    for (f, input) in inputs(FRAMES).iter().enumerate() {
        let cap = nes_step_frame(&mut core, input);
        if f as u32 >= PLAY_FROM {
            rec.push(cap, nes_cam(&core));
        }
    }
    rec.trace
}

/// Share of `frames` for which `pred` holds.
fn share(frames: &[Frame], pred: impl Fn(&Frame) -> bool) -> f64 {
    frames.iter().filter(|f| pred(f)).count() as f64 / frames.len().max(1) as f64
}

/// A run of consecutive RAM bytes (stride 1) that line up with the picture.
#[derive(Debug, Clone)]
struct Column {
    start: usize,
    /// Per byte: share of frames it matched a sprite coordinate.
    shares: Vec<f64>,
    /// Per byte: the constant added to the byte to give the sprite coordinate.
    deltas: Vec<i32>,
}

/// Runs of at least three bytes, each matching some sprite's `coord` (plus a
/// small constant) in at least `min` of the frames. RAM that mirrors the OAM
/// (a sprite buffer) is skipped.
fn columns(t: &Trace, y: bool, min: f64) -> Vec<Column> {
    let n = t.frames[0].ram.len();
    let mirror = |a: usize| {
        share(&t.frames, |f| {
            f.oam.get(a & 0xFF).copied() == Some(f.ram[a])
        }) > 0.9
    };
    let best = |a: usize| -> (f64, i32) {
        let deltas: &[i32] = if y { &[-3, -2, -1, 0, 1, 2, 3] } else { &[0] };
        deltas
            .iter()
            .map(|&d| {
                let s = share(&t.frames, |f| {
                    f.sprites.iter().any(|sp| {
                        let c = if y { sp.y } else { sp.x };
                        i32::from(c) == i32::from(f.ram[a]) + d
                    })
                });
                (s, d)
            })
            .fold((0.0, 0), |b, c| if c.0 > b.0 { c } else { b })
    };
    let scored: Vec<(f64, i32)> = (0..n)
        .map(|a| if mirror(a) { (0.0, 0) } else { best(a) })
        .collect();
    let mut out = Vec::new();
    let mut a = 0;
    while a < n {
        // Progress: `a` advances on every path below.
        if scored[a].0 < min {
            a += 1;
            continue;
        }
        let start = a;
        while a < n && scored[a].0 >= min {
            a += 1;
        }
        if a - start >= 3 {
            out.push(Column {
                start,
                shares: scored[start..a].iter().map(|s| s.0).collect(),
                deltas: scored[start..a].iter().map(|s| s.1).collect(),
            });
        }
    }
    out
}

/// The byte that only ever moves by 1 or 16 (mod 256), at least three times,
/// in both sizes: the screen index of a grid map.
type Change = (usize, u8, u8);

fn screen_index(t: &Trace) -> Vec<(usize, Vec<Change>)> {
    let n = t.frames[0].ram.len();
    let mut out = Vec::new();
    for a in 0..n {
        let ch: Vec<Change> = t
            .frames
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0].ram[a] != w[1].ram[a])
            .map(|(i, w)| (i + 1, w[0].ram[a], w[1].ram[a]))
            .collect();
        let step = |c: &Change| c.2.wrapping_sub(c.1);
        if ch.len() >= 3
            && ch.len() <= 24
            && ch.iter().all(|c| matches!(step(c), 1 | 255 | 16 | 240))
            && ch.iter().any(|c| matches!(step(c), 1 | 255))
            && ch.iter().any(|c| matches!(step(c), 16 | 240))
        {
            out.push((a, ch));
        }
    }
    out
}

/// The byte that equals the low eight bits of the playfield scroll in at least
/// 90% of the frames in which the scroll moved horizontally.
fn scroll_x_bytes(t: &Trace) -> Vec<(usize, usize, usize)> {
    let moved: Vec<usize> = (1..t.frames.len())
        .filter(|&i| t.frames[i].cam_x != t.frames[i - 1].cam_x)
        .collect();
    if moved.len() < 20 {
        return Vec::new();
    }
    let n = t.frames[0].ram.len();
    (0..n)
        .filter_map(|a| {
            let hit = moved
                .iter()
                .filter(|&&i| {
                    let d = t.frames[i].ram[a].wrapping_sub((t.frames[i].cam_x & 0xFF) as u8);
                    d <= 9 || d >= 247
                })
                .count();
            (hit * 10 >= moved.len() * 9).then_some((a, hit, moved.len()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stage 3: who writes the bytes.
// ---------------------------------------------------------------------------

/// Counts writes to watched addresses per (address, atlas function, whether an
/// interrupt handler was running). An interrupt handler is running from an
/// interrupt step until an `RTI` (`$40`, the opcode fetch is the step's first
/// access).
#[derive(Default)]
struct WriteWatch {
    addrs: BTreeSet<u32>,
    in_interrupt: bool,
    hits: BTreeMap<(u32, Option<u32>, bool), u32>,
    /// Frames in which an NMI sequence started.
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

fn writers(raw: &[u8], atlas: &Atlas, addrs: &[u32]) -> WriteWatch {
    let mut d = NesDriver::new(raw, atlas).expect("loads");
    let mut w = WriteWatch {
        addrs: addrs.iter().copied().collect(),
        ..WriteWatch::default()
    };
    for (f, input) in inputs(FRAMES).iter().enumerate() {
        if (f as u32) < PLAY_FROM {
            d.frame(input, false).expect("frame");
        } else {
            d.frame_with(input, &mut w).expect("frame");
        }
    }
    w
}

/// Frames at which `addr` changes value in a plain run of the script.
fn change_frames(raw: &[u8], addr: u32) -> Vec<u32> {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut prev = core.peek(addr);
    let mut out = Vec::new();
    for (f, input) in inputs(FRAMES).iter().enumerate() {
        core.run_frame(input, &mut NullSink);
        let now = core.peek(addr);
        if now != prev && f as u32 >= PLAY_FROM {
            out.push(f as u32);
        }
        prev = now;
    }
    out
}

fn observe_data(raw: &[u8], atlas: &Atlas, from: u32, to: u32) -> DataFacts {
    let mut d = NesDriver::new(raw, atlas).expect("loads");
    for (f, input) in (0u32..).zip(inputs(FRAMES)).take(to as usize) {
        d.frame(&input, f >= from).expect("frame");
    }
    d.take_facts()
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
#[ignore = "needs The Legend of Zelda (USA) (RF_SCREENS_ROMS_DIR); minutes in release"]
fn zelda_finders_and_recipe() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let atlas = build_nes_atlas(&raw, &coverage(&raw)).expect("atlas");
    println!("atlas: {} functions", atlas.functions.len());
    let trace = trace_of(&raw);
    let n = trace.frames.len();
    println!("trace: {n} frames");

    // The object columns.
    let xs = columns(&trace, false, 0.35);
    let ys = columns(&trace, true, 0.35);
    for c in xs.iter().chain(ys.iter()) {
        println!(
            "COLUMN start {:#x} len {} shares {:.2?} deltas {:?}",
            c.start,
            c.shares.len(),
            c.shares,
            c.deltas
        );
    }
    let scroll = scroll_x_bytes(&trace);
    println!("scroll x bytes {scroll:x?}");
    let idx = screen_index(&trace);
    for (a, ch) in &idx {
        println!("SCREEN-INDEX candidate {a:#x} changes {ch:?}");
    }
    let (px, py) = (xs[0].start as u32, ys[0].start as u32);
    let mut watched: Vec<u32> = vec![px, py, idx[0].0 as u32, scroll[0].0 as u32];
    watched.extend((1..xs[0].shares.len() as u32).map(|i| px + i));
    watched.extend((1..ys[0].shares.len() as u32).map(|i| py + i));
    let w = writers(&raw, &atlas, &watched);
    println!("{} NMI sequences in the observed play", w.nmis);
    for ((addr, f, irq), c) in &w.hits {
        println!(
            "WRITE {addr:#x} by fn {:?} (entry {:#x}) in_interrupt {irq}: {c}",
            f,
            f.map_or(0, |f| atlas.functions[f as usize].entry_cpu)
        );
    }
    let t = std::time::Instant::now();
    let named = rf_harness::understand::run_nes(&raw, &atlas, inputs(FRAMES)).expect("observes");
    for l in rf_harness::understand::summary_lines(&named, &atlas) {
        println!("{l}");
    }
    println!("naming in {:?}", t.elapsed());
    for l in rf_harness::understand::sample_lines(&named).iter().take(60) {
        println!("{l}");
    }

    // Stage 5a: data regions, one observed window around each screen load. The
    // screen index byte is the observed one; a window runs from 30 frames
    // before the byte changes to 150 after, so one screen load is inside.
    let eb = idx[0].0 as u32;
    let chg = change_frames(&raw, eb);
    println!("screen index changes at frames {chg:?}");
    // Changes within 40 frames of each other are one screen change (the byte
    // flickers while the game sets the new screen up): keep the last of each.
    let mut settled: Vec<u32> = Vec::new();
    for &c in &chg {
        match settled.last_mut() {
            Some(l) if c - *l < 40 => *l = c,
            _ => settled.push(c),
        }
    }
    // key, from, to: the first screen is loaded when the game starts; each
    // other window ends shortly after the settled change.
    let mut windows: Vec<(u32, u32, u32)> = vec![(
        u32::from(eb_value_at(&raw, eb, PLAY_FROM)),
        560,
        PLAY_FROM + 20,
    )];
    for &c in &settled {
        windows.push((u32::from(eb_value_at(&raw, eb, c + 5)), c - 60, c + 120));
    }
    let t = std::time::Instant::now();
    let mut runs: Vec<(u32, DataFacts)> = Vec::new();
    for &(key, from, to) in windows
        .iter()
        .filter(|_| std::env::var("SKIP_DATA").is_err())
    {
        let f = observe_data(&raw, &atlas, from, to);
        println!(
            "window screen {key} frames {from}..{to}: {} sites, {} data reads, {} unknown",
            f.sites.len(),
            f.stats.data_reads,
            f.stats.unknown
        );
        runs.push((key, f));
    }
    let mut merged = DataFacts::default();
    for (_, f) in &runs {
        merged.merge(f);
    }
    println!("observed in {:?}", t.elapsed());
    let refs: Vec<(u32, &DataFacts)> = runs.iter().map(|(k, f)| (*k, f)).collect();
    let links = associate(&refs);
    for l in &links {
        println!("feed lo {:#x} hi {:#x}: {} runs, {} own, {} entries, {} targets, {} streams, tracks {}", l.link.lo, l.link.hi, l.runs, l.own_entry, l.distinct_entries, l.distinct_targets, l.stream_found, l.tracks_level());
        for r in &l.per_run {
            println!(
                "   screen {} index {:?} target {:?} stream {:?}",
                r.key, r.index, r.target, r.stream
            );
        }
    }
    let regions = infer_regions(&merged);
    let rows = grade_rows(&regions, &links, "scripted overworld walk across screens");
    let region_rows = rows.clone();
    let confirmed = rows.iter().filter(|r| r.grade == Grade::Confirmed).count();
    let mut by_kind: BTreeMap<(RegionKind, &str), u32> = BTreeMap::new();
    for r in &rows {
        *by_kind.entry((r.kind, r.grade.as_str())).or_default() += 1;
    }
    println!(
        "{} regions, {} confirmed; by kind {by_kind:?}",
        rows.len(),
        confirmed
    );
    for r in rows
        .iter()
        .filter(|r| r.kind == RegionKind::PointerTable || r.grade == Grade::Confirmed)
    {
        println!(
            "REGION {} {:?} rom {:#x} size {} stride {} conf {:?} grade {}",
            r.name,
            r.kind,
            r.rom,
            r.size,
            r.stride,
            r.confidence,
            r.grade.as_str()
        );
    }

    // Stage 5b: the collision finder, watching Link.
    let cw = CollisionWatch {
        y: vec![py],
        x: vec![px],
    };
    let mut d = NesDriver::new(&raw, &atlas).expect("loads");
    let pad = |f: usize| script(f as u32) & (LEFT | RIGHT | UP | DOWN) != 0;
    let cf = run_collision(
        &mut d,
        &inputs(FRAMES),
        PLAY_FROM as usize,
        &cw,
        pad,
        |_, _| {},
    )
    .expect("runs");
    let csites = infer_collision(&cf);
    println!(
        "collision: {} frames, {} load sites, {} candidates",
        cf.frames.len(),
        cf.sites.len(),
        csites.len()
    );
    for s in csites.iter().take(4) {
        let by_class: BTreeMap<&str, usize> =
            s.verdicts.iter().fold(BTreeMap::new(), |mut m, v| {
                *m.entry(v.class.as_str()).or_default() += 1;
                m
            });
        println!("COLLISION site rom {:#x} fn {:?} active {} frames, {} verdicts {by_class:?}, solid ids {:x?}", s.site, s.function, s.active_frames, s.verdicts.len(), s.ids(TileClass::Solid));
    }

    // Stage 5c: widescreen candidates and trials.
    let ins = inputs(FRAMES);
    let table = ObjectTable {
        name: "actors".into(),
        base: px,
        slots: xs[0].shares.len() as u32,
        stride: 1,
        x_lo: Some(0),
        y: Some(py - px),
        grade: Grade::Observed,
        ..ObjectTable::default()
    };
    let mut watch = WideWatch::default();
    for i in 0..table.slots {
        watch.obj_x.insert(px + i);
    }
    watch.cam.insert(scroll[0].0 as u32);
    let statics = static_sprites(&trace);
    let shadow = oam_shadow(&trace);
    println!(
        "static sprites {:?}; oam shadow {shadow:?}",
        statics
            .iter()
            .map(|s| (s.index, s.x, s.y))
            .collect::<Vec<_>>()
    );
    if let Some(sh) = shadow {
        watch.hud = hud_watch(&trace, sh, &statics);
    }
    let t = std::time::Instant::now();
    let facts = observe_nes(&raw, &atlas, &ins, PLAY_FROM as usize, watch, |_, _| {})
        .expect("observed run");
    let cands = infer_wide(&facts);
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
    let plain = nes_snaps(&raw, &ins, |_, _| {}).expect("plain run");
    let view = View {
        ram_base: 0,
        table: table.clone(),
        cam_lo: scroll[0].0,
        cam_hi: None,
        x_page_zero: true,
    };
    let chosen: Vec<&Candidate> = cands
        .iter()
        .filter(|c| c.kind != Kind::ScreenConst && !widen_values(c).is_empty())
        .take(14)
        .collect();
    let t = std::time::Instant::now();
    let tried = try_candidates(
        &TrialSetup {
            raw: &raw,
            view: &view,
            plain: &plain,
            patch: &|r, at, b| nes_patch(r, at, b),
            run: &|image| nes_snaps(image, &ins, |_, _| {}),
            values: 2,
        },
        &chosen,
        &mut |l| println!("{l}"),
    )
    .expect("trials");
    println!("{} candidates tried in {:?}", tried.len(), t.elapsed());
    for t in &tried {
        println!(
            "RESULT {:<16} rom {:#07x} grade {} value {:?} partners {:x?}",
            t.kind().name(),
            t.cand.operand,
            t.grade().as_str(),
            t.best.as_ref().map(|b| b.0),
            t.partners
        );
    }

    // Stage 6: the recipe.
    let mut recipe = Recipe {
        id: "legend-of-zelda".into(),
        engine: "zelda1".into(),
        ..Recipe::default()
    };
    let top_writer = |addr: u32| -> Option<u32> {
        w.hits
            .iter()
            .filter(|((a, f, _), _)| *a == addr && f.is_some())
            .max_by_key(|(_, c)| **c)
            .and_then(|((_, f, _), _)| *f)
    };
    let writers_of = |addr: u32| -> BTreeSet<u32> {
        w.hits
            .keys()
            .filter(|(a, f, _)| *a == addr && f.is_some())
            .filter_map(|(_, f, _)| *f)
            .collect()
    };
    let nmi = atlas
        .functions
        .iter()
        .position(|f| {
            rf_rebuild::EntryKind::sole_interrupt(f.entry_kinds) == Some(rf_rebuild::EntryKind::Nmi)
        })
        .expect("an NMI function") as u32;
    let outside_nmi: u32 = w
        .hits
        .iter()
        .filter(|((_, _, irq), _)| !irq)
        .map(|(_, c)| *c)
        .sum();
    let inside_nmi: u32 = w
        .hits
        .iter()
        .filter(|((_, _, irq), _)| *irq)
        .map(|(_, c)| *c)
        .sum();
    println!(
        "writes to the watched bytes: {inside_nmi} while an NMI handler ran, {outside_nmi} outside"
    );
    let both = HookPoints {
        entry: true,
        exit: true,
    };
    let entry_only = HookPoints {
        entry: true,
        exit: false,
    };
    recipe.functions.push(function_row(&atlas, nmi, "nmi", both, &format!(
        "observed: the atlas function the NMI vector targets in a recording; the per-frame game tick: {inside_nmi} of {} writes to Link, the actor slots, the scroll and the screen index happened while an NMI handler ran, so its exit is the end of a logic frame", inside_nmi + outside_nmi)));
    // The writer of Link's x that does not write the next slot's x.
    let player_fn = writers_of(px)
        .into_iter()
        .filter(|f| !writers_of(px + 1).contains(f))
        .max_by_key(|f| {
            w.hits
                .iter()
                .filter(|((a, g, _), _)| *a == px && *g == Some(*f))
                .map(|(_, c)| *c)
                .sum::<u32>()
        });
    if let Some(f) = player_fn {
        let c: u32 = w
            .hits
            .iter()
            .filter(|((a, g, _), _)| *a == px && *g == Some(f))
            .map(|(_, c)| *c)
            .sum();
        recipe.functions.push(function_row(&atlas, f, "player_move", both, &format!("observed: writes Link's x and y ({c} x writes in the play) and never a following slot's, with the pad pressed")));
    }
    let all_slots = |f: &u32| {
        (0..table.slots).all(|i| writers_of(px + i).contains(f) && writers_of(py + i).contains(f))
    };
    let slot_fn = writers_of(px)
        .into_iter()
        .filter(all_slots)
        .max_by_key(|f| {
            w.hits
                .iter()
                .filter(|((_, g, _), _)| *g == Some(*f))
                .map(|(_, c)| *c)
                .sum::<u32>()
        });
    if let Some(f) = slot_fn {
        recipe.functions.push(function_row(&atlas, f, "actor_move", both, "observed: writes the x and y of every actor slot (Link's and each following one), the shared per-slot mover"));
    }
    if let Some(f) = top_writer(scroll[0].0 as u32) {
        recipe.functions.push(function_row(&atlas, f, "scroll_step", entry_only, "observed: the most frequent writer of the scroll byte (64 writes over four screen changes), which also moves Link while the screen scrolls"));
    }
    for (k, f) in writers_of(idx[0].0 as u32).into_iter().enumerate() {
        recipe.functions.push(function_row(&atlas, f, &format!("screen_index_writer_{}", (b'a' + k as u8) as char), entry_only, "observed: writes the screen-index byte (the byte that only moves by 1 or 16, at screen changes)"));
    }
    let own_names = recipe.functions.len();
    let named_n = rf_rebuild::naming::merge_name_rows(&mut recipe, &atlas, &named.proposals);
    println!("{own_names} observed function rows, {named_n} rule-named rows");

    let ram = |addr: u32, name: &str, source: String| RamMeaning {
        addr,
        width: 1,
        name: name.into(),
        source,
        grade: Grade::Observed,
    };
    let col = |c: &Column, i: usize| format!("{:.0}% of the play's frames", c.shares[i] * 100.0);
    recipe.ram.push(ram(px, "player_x", format!("observed: equals the x of a sprite in {} of the frames (object-column finder, W38-02 method); changes by the pad's left and right", col(&xs[0], 0))));
    recipe.ram.push(ram(
        py,
        "player_y",
        format!(
            "observed: equals the y of a sprite plus {} in {}",
            xs.first().map_or(0, |_| ys[0].deltas[0]),
            col(&ys[0], 0)
        ),
    ));
    recipe.ram.push(ram(idx[0].0 as u32, "screen_index", format!("observed: the only byte that moves by exactly 1 (sideways) or 16 (up or down) at screen changes: {} changes in the play, e.g. {:?}", idx[0].1.len(), &idx[0].1[..3.min(idx[0].1.len())])));
    recipe.ram.push(ram(scroll[0].0 as u32, "scroll_x", format!("observed: equals the low byte of the PPU's horizontal scroll in {} of {} frames in which it moved", scroll[0].1, scroll[0].2)));
    recipe.object_tables.push(ObjectTable {
        source: format!("observed: x bytes {px:#x}.. equal sprite x in {:.2?} of the frames (slot 0 always: Link; the others when an actor is drawn), y bytes {py:#x}.. equal a sprite y plus {:?}; slot 0's sprite y is two below its RAM y, the others equal; no state or type column was found (the W38-02 finder did not bind this table: its slots do not share one sprite offset)", xs[0].shares, ys[0].deltas),
        ..table.clone()
    });
    recipe.data_regions = select_rows(&region_rows, 60);
    recipe.tiles = Vec::new();
    let text = rf_profiles::recipe::recipe_to_toml(&recipe);
    let back = rf_profiles::recipe::load_recipe_str(&text).expect("the recipe round-trips");
    assert_eq!(back, recipe);
    let report = rf_rebuild::match_recipe(&atlas, &recipe);
    println!(
        "recipe: {} functions ({} unmatched), {} ram, {} tables, {} regions",
        recipe.functions.len(),
        report.unmatched.len(),
        recipe.ram.len(),
        recipe.object_tables.len(),
        recipe.data_regions.len()
    );
    assert!(report.unmatched.is_empty());
    let overlay = rows_toml(
        &atlas,
        &recipe,
        &tried,
        "widescreen-objects",
        "widescreen-candidates",
        &[],
        "Zelda overworld play",
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

const RECIPE_HEADER: &str = "# W38-05: engine recipe for The Legend of Zelda (NES). Generated by\n# crates/rf-harness/tests/w38_05_zelda_recipe.rs (RF_W38_WRITE=1) from the repo's\n# own finders watching the running game (D-021: no disassembly, no community RAM\n# map or decompilation, no ROM bytes). Offsets and numbers only.";
const OVERLAY_HEADER: &str = "# W38-05: widescreen finder rows for The Legend of Zelda, an OVERLAY on\n# recipe.toml (append this file's text to the recipe to load it, as for SMB).\n# Finder: crates/rf-rebuild/src/widescreen.rs; trials: w38_05_zelda_recipe.rs.\n# Every row is a candidate unless a lockstep trial kept it (see the grades).";

fn eb_value_at(raw: &[u8], addr: u32, frame: u32) -> u8 {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.reset(ResetKind::Hard);
    for input in inputs(FRAMES).iter().take(frame as usize + 1) {
        core.run_frame(input, &mut NullSink);
    }
    core.peek(addr)
}
