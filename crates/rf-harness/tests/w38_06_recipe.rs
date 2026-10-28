//! **W38-06: the finders on Super Metroid (SNES)** (ROM-gated, law 5, D-021).
//! Every fact in `profiles/snes/super-metroid/recipe.toml` comes from this
//! test's observation of the running game over the scripted play of
//! `w38_06_common`; no disassembly, no community RAM map, no decompilation.
//! Only offsets, counts and verdicts are printed.
//!
//! Stages (each can be skipped with an environment variable; the later ones
//! need what the earlier ones found, so the test stops at the first skipped
//! stage it needs):
//! 1. atlas from the run's coverage;
//! 2. a trace (RAM and OAM at each OAM upload, as W38-02): the object table the
//!    W38-02 finder binds, the camera, and the player's position words found
//!    the way the NES recipe found its columns, as 16-bit words whose value
//!    minus the scroll equals a sprite coordinate plus a constant;
//! 3. an observed run that attributes writes to the position words to atlas
//!    functions, and to the NMI, an IRQ or the main thread, and says whether the
//!    positions are final at the NMI's entry or its exit;
//! 4. the W36 naming pass;
//! 5. the W38-01 data-region finder (one window per room, keyed by the room
//!    word found in the trace), the W38-03 collision finder and the W38-04
//!    widescreen finder over the same play;
//! 6. the recipe, written with `RF_W38_WRITE=1`.
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_06_recipe -- --ignored --nocapture
//! RF_W38_WRITE=1 cargo test --release -p rf-harness --test w38_06_recipe -- --ignored --nocapture
//! ```

mod w38_06_common;

use std::collections::{BTreeMap, BTreeSet};

use rf_core_api::{BusAccess, EmulatorCore, ResetKind, StepFacts, StepObserver};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::collision_tables::{run_snes as run_collision, Watch as CollisionWatch};
use rf_harness::data_regions::SnesDriver;
use rf_harness::object_tables::{snes_cam, snes_step_frame, Console, Recorder};
use rf_harness::widescreen::{
    observe_snes, rows_toml, snes_patch, snes_snaps, try_candidates, TrialSetup,
};
use rf_input::SnesButton;
use rf_rebuild::collision::infer as infer_collision;
use rf_rebuild::dataregion::{
    associate, grade_rows, infer as infer_regions, select_rows, DataFacts, RegionKind,
};
use rf_rebuild::objtable::{infer as infer_tables, to_row, Frame, Trace};
use rf_rebuild::widescreen::{
    camera_aliases, find_camera, find_x_page, hud_watch, infer as infer_wide, oam_shadow,
    slots_from_rows, static_sprites, widen_values, Candidate, Kind, View, Watch as WideWatch,
    XPage,
};
use rf_rebuild::TileClass;
use rf_rebuild::{Atlas, Grade, HookPoints, ObjectTable, RamMeaning, Recipe, RecipeFunction};
use rf_snes::core::SnesCore;
use w38_06_common::*;

const RECIPE_DIR: &str = "../../profiles/snes/super-metroid";

fn skip(stage: &str) -> bool {
    std::env::var(format!("SKIP_{stage}")).is_ok()
}

// ---------------------------------------------------------------------------
// Stage 1-2: atlas and trace.
// ---------------------------------------------------------------------------

fn coverage(raw: &[u8]) -> rf_core_api::coverage::CoverageLog {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for mask in script() {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

/// The trace, and the script frame each of its frames is (a frame in which the
/// OAM never changed leaves no entry).
fn trace_of(raw: &[u8]) -> (Trace, Vec<usize>) {
    let ins = inputs_of(&script());
    let from = play_from();
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut rec = Recorder::new(Console::Snes, false);
    let mut frame_of = Vec::new();
    for (f, input) in ins.iter().enumerate() {
        if f >= from {
            let cap = snes_step_frame(&mut core, input);
            if cap.is_some() {
                frame_of.push(f);
            }
            rec.push(cap, snes_cam(&core));
        } else {
            core.system_mut().bus.joypads.ports[0] = input.ports[0];
            core.run_frame(input, &mut NullSink);
        }
    }
    (rec.trace, frame_of)
}

/// A 16-bit word of work RAM that lines up with the picture.
#[derive(Debug, Clone, Copy)]
struct Word {
    addr: usize,
    /// Share of the trace's frames in which some sprite's coordinate equals the
    /// word minus the scroll plus `delta` (mod 256).
    share: f64,
    delta: i32,
    /// Frames in which the word changed.
    moved: usize,
}

/// Words that follow the picture: for each candidate address the constant that
/// puts the word's on-screen position on some sprite in the most frames. Words
/// that never move, or move in nearly every frame (counters), are skipped.
fn position_words(
    t: &Trace,
    frame_of: &[usize],
    y: bool,
    skip: std::ops::Range<usize>,
) -> Vec<Word> {
    let n = t.frames.len();
    let r = t.frames[0].ram.len();
    let word = |f: &Frame, a: usize| u16::from(f.ram[a]) | u16::from(f.ram[a + 1]) << 8;
    let cam_of = |f: &Frame| if y { f.cam_y } else { f.cam_x };
    let coord = |s: &rf_rebuild::objtable::Sprite| if y { s.y } else { s.x };
    let mut out = Vec::new();
    for a in (0..r - 1).filter(|a| !skip.contains(a) && !skip.contains(&(a + 1))) {
        let moved = t
            .frames
            .windows(2)
            .filter(|w| word(&w[0], a) != word(&w[1], a))
            .count();
        if moved < 100 || moved * 10 > n * 6 {
            continue;
        }
        // The scroll itself (or a copy of it) is not a position on the screen:
        // the word minus the scroll must take several values.
        let rel: BTreeSet<u16> = t
            .frames
            .iter()
            .map(|f| word(f, a).wrapping_sub(cam_of(f)))
            .collect();
        if rel.len() < 8 {
            continue;
        }
        // Motion match: in consecutive frames in which the word moved a few
        // pixels and the scroll did not, a sprite (the same OAM entry) moved by
        // the same amount. A pose change moves the sprite entries about, so
        // this is a share, not all.
        let (mut pairs, mut hit) = (0u32, 0u32);
        let mut steps: BTreeSet<i16> = BTreeSet::new();
        for (i, w) in t.frames.windows(2).enumerate() {
            if frame_of[i + 1] != frame_of[i] + 1 || cam_of(&w[0]) != cam_of(&w[1]) {
                continue;
            }
            let dw = word(&w[1], a).wrapping_sub(word(&w[0], a)) as i16;
            if dw == 0 || dw.abs() > 8 {
                continue;
            }
            pairs += 1;
            steps.insert(dw);
            let moved_same = w[1].sprites.iter().any(|s1| {
                w[0].sprites
                    .iter()
                    .any(|s0| s0.index == s1.index && i16::from(coord(s1).wrapping_sub(coord(s0)) as i8) == dw)
            });
            hit += u32::from(moved_same);
        }
        if pairs < 100 || steps.len() < 3 {
            continue;
        }
        let share = f64::from(hit) / f64::from(pairs);
        if share < 0.6 {
            continue;
        }
        // The constant that puts the word's screen position on a sprite most.
        let mut votes = [0u32; 256];
        for f in &t.frames {
            let screen = word(f, a).wrapping_sub(cam_of(f)) as u8;
            let mut seen = [false; 256];
            for s in &f.sprites {
                seen[usize::from(coord(s).wrapping_sub(screen))] = true;
            }
            for (v, s) in votes.iter_mut().zip(seen) {
                *v += u32::from(s);
            }
        }
        let d = votes
            .iter()
            .enumerate()
            .max_by_key(|(_, v)| **v)
            .map_or(0, |(d, _)| d);
        out.push(Word {
            addr: a,
            share,
            delta: i32::from(d as u8 as i8),
            moved,
        });
    }
    out.sort_by(|a, b| b.share.total_cmp(&a.share).then(b.moved.cmp(&a.moved)));
    out
}

/// The player's word: of the words whose motion matches a sprite's in at least
/// 65% of the pairs, the one that moves in the most frames (the player moves
/// whenever the pad is held; a copy or another object does not), the lowest
/// address within 1% of the most.
fn pick(ws: &[Word]) -> Word {
    let ok: Vec<&Word> = ws.iter().filter(|w| w.share >= 0.65).collect();
    let most = ok.iter().map(|w| w.moved).max().expect("a position word");
    **ok.iter()
        .filter(|w| w.moved * 100 >= most * 99)
        .min_by_key(|w| w.addr)
        .expect("one")
}

/// 16-bit words that change a few times, each time to a value that then stays:
/// the room-sized state of the game (which room, and so which data).
fn room_words(t: &Trace) -> Vec<(usize, Vec<(usize, u16)>)> {
    let r = t.frames[0].ram.len();
    let word = |f: &Frame, a: usize| u16::from(f.ram[a]) | u16::from(f.ram[a + 1]) << 8;
    let mut out = Vec::new();
    for a in 0..r - 1 {
        let mut changes: Vec<(usize, u16)> = Vec::new();
        for (i, w) in t.frames.windows(2).enumerate() {
            if word(&w[0], a) != word(&w[1], a) {
                changes.push((i + 1, word(&w[1], a)));
            }
        }
        if std::env::var("DBG_ADDR").is_ok_and(|v| usize::from_str_radix(&v, 16) == Ok(a)) {
            println!("DBG {a:#x}: {} changes {:x?}", changes.len(), &changes[..changes.len().min(40)]);
        }
        // Few changes, each value held for a while, several distinct values.
        let pointer_like = changes.iter().all(|c| c.1 >= 0x8000);
        let held = changes
            .windows(2)
            .all(|c| c[1].0 - c[0].0 >= 60 || c[1].0 - c[0].0 <= 4);
        let distinct: BTreeSet<u16> = changes.iter().map(|c| c.1).collect();
        if (6..=80).contains(&changes.len()) && distinct.len() >= 5 && held && pointer_like {
            out.push((a, changes));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Stage 3: who writes the position words.
// ---------------------------------------------------------------------------

/// What kind of interrupt an interrupt step began: the vector it read
/// (fullsnes "CPU Interrupts": native NMI `$00FFEA`, IRQ `$00FFEE`; emulation
/// NMI `$00FFFA`, IRQ `$00FFFE`).
fn interrupt_is_nmi(accesses: &[BusAccess]) -> bool {
    accesses
        .iter()
        .any(|a| !a.write && matches!(a.addr & 0xFFFF, 0xFFEA | 0xFFEB | 0xFFFA | 0xFFFB))
}

/// The work-RAM offset (below 8 KiB) a bus address names: bank `$7E`, or the
/// low-RAM mirror in the system banks (fullsnes "Memory Map").
fn wram_offset(addr: u32) -> Option<u32> {
    let (bank, low) = (addr >> 16, addr & 0xFFFF);
    let mirrored = (bank & 0x7F) < 0x40 && low < 0x2000;
    ((bank == 0x7E && low < 0x2000) || mirrored).then_some(low)
}

#[derive(Default)]
struct WriteWatch {
    addrs: BTreeSet<u32>,
    /// Nested handlers: true for an NMI.
    stack: Vec<bool>,
    /// (RAM offset, function, handler: 0 none, 1 NMI, 2 IRQ) -> writes.
    hits: BTreeMap<(u32, Option<u32>, u8), u32>,
    nmis: u32,
    irqs: u32,
    /// Per frame: the NMI began / ended; a watched write came after.
    nmi_began: bool,
    nmi_ended: bool,
    late_after_entry: bool,
    late_after_exit: bool,
    frames: u32,
    frames_with_nmi: u32,
    /// Frames in which a watched write came at or after the NMI's entry /
    /// strictly after its exit.
    final_not_at_entry: u32,
    final_not_at_exit: u32,
}

impl StepObserver for WriteWatch {
    fn step(&mut self, facts: StepFacts, accesses: &[BusAccess]) {
        if facts.interrupt {
            let nmi = interrupt_is_nmi(accesses);
            self.stack.push(nmi);
            if nmi {
                self.nmis += 1;
                self.nmi_began = true;
            } else {
                self.irqs += 1;
            }
        }
        let who = match self.stack.last() {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        };
        for a in accesses.iter().filter(|a| a.write) {
            let Some(off) = wram_offset(a.addr) else {
                continue;
            };
            if self.addrs.contains(&off) {
                *self.hits.entry((off, facts.function, who)).or_default() += 1;
                if self.nmi_began {
                    self.late_after_entry = true;
                }
                if self.nmi_ended {
                    self.late_after_exit = true;
                }
            }
        }
        // `RTI` (`$40`) is the first access of its step.
        if !facts.interrupt
            && accesses
                .first()
                .is_some_and(|a| a.value == 0x40 && !a.write)
        {
            if let Some(nmi) = self.stack.pop() {
                if nmi {
                    self.nmi_ended = true;
                }
            }
        }
    }

    fn frame_end(&mut self) {
        self.frames += 1;
        self.frames_with_nmi += u32::from(self.nmi_began);
        self.final_not_at_entry += u32::from(self.late_after_entry);
        self.final_not_at_exit += u32::from(self.late_after_exit);
        self.nmi_began = false;
        self.nmi_ended = false;
        self.late_after_entry = false;
        self.late_after_exit = false;
        self.stack.clear();
    }
}

fn writers(raw: &[u8], atlas: &Atlas, from: usize, addrs: &[u32]) -> WriteWatch {
    let mut d = SnesDriver::new(raw, atlas).expect("loads");
    let mut w = WriteWatch {
        addrs: addrs.iter().copied().collect(),
        ..WriteWatch::default()
    };
    for (f, input) in inputs_of(&script()).iter().enumerate() {
        if f < from {
            d.frame(input, false).expect("frame");
        } else {
            d.frame_with(input, &mut w).expect("frame");
        }
    }
    w
}

/// One observed pass; `windows` are (key, from, to) frame ranges, in order and
/// not overlapping. The facts of each are taken when its range ends.
fn observe_windows(
    raw: &[u8],
    atlas: &Atlas,
    windows: &[(u32, usize, usize)],
) -> Vec<(u32, DataFacts)> {
    let mut d = SnesDriver::new(raw, atlas).expect("loads");
    let mut out = Vec::new();
    let last = windows.last().map_or(0, |w| w.2);
    for (f, input) in inputs_of(&script()).iter().enumerate().take(last) {
        let open = windows.iter().find(|w| (w.1..w.2).contains(&f));
        d.frame(input, open.is_some()).expect("frame");
        if let Some(w) = windows.iter().find(|w| w.2 == f + 1) {
            out.push((w.0, d.take_facts()));
        }
    }
    out
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
        mode: Some(f.entry_mode),
        hooks,
        source: source.to_string(),
        grade: Grade::Observed,
        ..RecipeFunction::default()
    }
}

#[test]
#[ignore = "needs Super Metroid (RF_SCREENS_ROMS_DIR); many minutes in release"]
fn super_metroid_finders_and_recipe() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let ins = inputs_of(&script());
    let from = play_from();
    let t0 = std::time::Instant::now();
    let (trace, frame_of) = trace_of(&raw);
    println!("trace: {} frames in {:?}", trace.frames.len(), t0.elapsed());

    // Stage 2: table, camera, position words.
    let found = infer_tables(&trace);
    for f in found.iter().take(4) {
        let tb = &f.table;
        println!(
            "TABLE base {:#x} slots {} stride {} x_lo {:?} x_hi {:?} y {:?} state {:?} z {:.0} rate {:.3} expected {} live_from_state {}",
            tb.base, tb.slots, tb.stride, tb.x_lo, tb.x_hi, tb.y, tb.state, f.z, f.binding.rate(), f.binding.expected, f.binding.live_from_state
        );
    }
    let cam = find_camera(&trace).expect("a camera");
    println!("camera lo {:#x} hi {:?}", cam.lo, cam.hi);
    // The sprite buffer (a RAM copy of the OAM) is not a position either.
    let shadow_at = oam_shadow(&trace).unwrap_or(0);
    let shadow = shadow_at..shadow_at + 544;
    let xs = position_words(&trace, &frame_of, false, shadow.clone());
    let ys = position_words(&trace, &frame_of, true, shadow);
    for w in xs.iter().take(10) {
        println!(
            "XWORD {:#x} motion share {:.2} delta {} moved {}",
            w.addr, w.share, w.delta, w.moved
        );
    }
    for w in ys.iter().take(10) {
        println!(
            "YWORD {:#x} motion share {:.2} delta {} moved {}",
            w.addr, w.share, w.delta, w.moved
        );
    }
    let mut rooms = room_words(&trace);
    rooms.sort_by_key(|(a, ch)| {
        (
            std::cmp::Reverse(ch.iter().map(|c| c.1).collect::<BTreeSet<_>>().len()),
            *a,
        )
    });
    for (a, ch) in rooms.iter().take(12) {
        println!(
            "ROOM-WORD candidate {a:#x}: {} changes, e.g. {:x?}",
            ch.len(),
            &ch[..4.min(ch.len())]
        );
    }
    if skip("ATLAS") {
        return;
    }
    let t1 = std::time::Instant::now();
    let atlas = build_snes_atlas(&raw, &coverage(&raw)).expect("atlas");
    println!(
        "atlas: {} functions in {:?}",
        atlas.functions.len(),
        t1.elapsed()
    );
    let (xw, yw) = (pick(&xs), pick(&ys));
    println!("PICK x {:#x} y {:#x}", xw.addr, yw.addr);
    let (px, py) = (xw.addr as u32, yw.addr as u32);
    let cam_lo = cam.lo as u32;
    let mut watched: Vec<u32> = vec![px, px + 1, py, py + 1, cam_lo];
    watched.extend(cam.hi.map(|h| h as u32));
    let w = writers(&raw, &atlas, from, &watched);
    println!(
        "{} NMI and {} IRQ sequences in {} observed frames ({} with an NMI)",
        w.nmis, w.irqs, w.frames, w.frames_with_nmi
    );
    println!(
        "position writes after the NMI's entry in {} frames, after its exit in {}",
        w.final_not_at_entry, w.final_not_at_exit
    );
    for ((addr, f, who), c) in &w.hits {
        println!(
            "WRITE {addr:#x} by fn {:?} (entry {:#x}) in {}: {c}",
            f,
            f.map_or(0, |f| atlas.functions[f as usize].entry_cpu),
            ["main", "nmi", "irq"][usize::from(*who)]
        );
    }
    if skip("NAMING") {
        return;
    }
    let t = std::time::Instant::now();
    let named = rf_harness::understand::run_snes(&raw, &atlas, ins.clone()).expect("observes");
    for l in rf_harness::understand::summary_lines(&named, &atlas) {
        println!("{l}");
    }
    println!("naming in {:?}", t.elapsed());

    // Stage 5a: data regions, one window around each room change.
    let (rw, changes) = rooms.first().expect("a room word");
    println!("room word {rw:#x}");
    let mut settled: Vec<(usize, u16)> = Vec::new();
    for &(f, v) in changes {
        match settled.last_mut() {
            Some(l) if f - l.0 < 40 => *l = (f, v),
            _ => settled.push((f, v)),
        }
    }
    let mut runs: Vec<(u32, DataFacts)> = Vec::new();
    if !skip("DATA") {
        // Windows from 60 frames before each change to 200 after, cut where
        // the next one starts so the passes never overlap.
        let mut windows: Vec<(u32, usize, usize)> = settled
            .iter()
            .map(|&(f, v)| (
                    u32::from(v),
                    frame_of[f.saturating_sub(1)].saturating_sub(60),
                    (frame_of[f] + 200).min(ins.len()),
                ))
            .collect();
        for i in 1..windows.len() {
            windows[i - 1].2 = windows[i - 1].2.min(windows[i].1);
        }
        windows.retain(|w| w.2 > w.1);
        let t = std::time::Instant::now();
        runs = observe_windows(&raw, &atlas, &windows);
        for (k, f) in &runs {
            println!(
                "window room {k:#06x}: {} sites, {} data reads",
                f.sites.len(),
                f.stats.data_reads
            );
        }
        println!("observed {} windows in {:?}", runs.len(), t.elapsed());
    }
    let mut merged = DataFacts::default();
    for (_, f) in &runs {
        merged.merge(f);
    }
    let refs: Vec<(u32, &DataFacts)> = runs.iter().map(|(k, f)| (*k, f)).collect();
    let links = associate(&refs);
    for l in &links {
        println!(
            "feed lo {:#x} hi {:#x}: {} runs, {} entries, {} targets, {} streams, tracks {}",
            l.link.lo,
            l.link.hi,
            l.runs,
            l.distinct_entries,
            l.distinct_targets,
            l.stream_found,
            l.tracks_level()
        );
    }
    let regions = infer_regions(&merged);
    let rows = grade_rows(&regions, &links, "scripted play through the first rooms");
    let region_rows = rows.clone();
    let mut by_kind: BTreeMap<(RegionKind, &str), u32> = BTreeMap::new();
    for r in &rows {
        *by_kind.entry((r.kind, r.grade.as_str())).or_default() += 1;
    }
    println!("{} regions; by kind {by_kind:?}", rows.len());

    // Stage 5b: collision.
    if !skip("COLLISION") {
        let cw = CollisionWatch {
            y: vec![RAM_BASE + py, RAM_BASE + py + 1],
            x: vec![RAM_BASE + px, RAM_BASE + px + 1],
        };
        let mut d = SnesDriver::new(&raw, &atlas).expect("loads");
        let side = (1u16 << SnesButton::Left.bit()) | (1u16 << SnesButton::Right.bit());
        let pad = script();
        let cf =
            run_collision(&mut d, &ins, from, &cw, |f| pad[f] & side != 0, |_| true).expect("runs");
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
            println!(
                "COLLISION site rom {:#x} fn {:?} active {} frames, {} verdicts {by_class:?}, solid ids {:x?}",
                s.site, s.function, s.active_frames, s.verdicts.len(), s.ids(TileClass::Solid)
            );
        }
    }

    // Stage 5c: widescreen candidates and trials.
    let table = found
        .iter()
        .find(|f| rf_rebuild::objtable::grade_of(f) == Grade::Observed)
        .map_or_else(
        || ObjectTable {
            name: "samus".into(),
            base: px,
            slots: 1,
            stride: 2,
            x_lo: Some(0),
            y: Some(py.wrapping_sub(px)),
            grade: Grade::Candidate,
            ..ObjectTable::default()
        },
        |f| {
            to_row(
                f,
                rf_rebuild::objtable::grade_of(f),
                "; Super Metroid first rooms",
            )
        },
    );
    let mut table = table;
    let page = find_x_page(&table, &cam, &trace);
    let mut x_page_zero = false;
    match page {
        Some(XPage::Column(off)) => table.x_hi = Some(off),
        Some(XPage::Zero) => x_page_zero = true,
        None => {}
    }
    println!("x page {page:?}");
    let mut watch = WideWatch::default();
    for i in 0..table.slots {
        for off in [table.x_lo, table.x_hi].into_iter().flatten() {
            watch.obj_x.insert(table.base + off + i * table.stride);
        }
        if let Some(off) = table.state {
            watch.obj_state.insert(table.base + off + i * table.stride);
        }
    }
    watch.cam.insert(RAM_BASE + cam.lo as u32);
    watch.cam.extend(cam.hi.map(|h| RAM_BASE + h as u32));
    watch.cam.extend(
        camera_aliases(&trace, &cam)
            .iter()
            .map(|&a| RAM_BASE + a as u32),
    );
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
    let facts = observe_snes(&raw, &atlas, &ins, from, watch).expect("observed run");
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
    let trials: usize = std::env::var("TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let chosen: Vec<&Candidate> = cands
        .iter()
        .filter(|c| c.kind != Kind::ScreenConst && !widen_values(c).is_empty())
        .take(trials)
        .collect();
    let mut tried = Vec::new();
    if !skip("TRIALS") {
        let plain = snes_snaps(&raw, &ins, |_, _| {}).expect("plain run");
        let view = View {
            ram_base: RAM_BASE,
            table: table.clone(),
            cam_lo: cam.lo,
            cam_hi: cam.hi,
            x_page_zero,
        };
        let t = std::time::Instant::now();
        tried = try_candidates(
            &TrialSetup {
                raw: &raw,
                view: &view,
                plain: &plain,
                patch: &|r, at, b| snes_patch(r, at, b),
                run: &|image| snes_snaps(image, &ins, |_, _| {}),
                values: 1,
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
    }

    // Stage 6: the recipe.
    let mut recipe = Recipe {
        id: "super-metroid".into(),
        engine: "supermetroid".into(),
        ..Recipe::default()
    };
    let writers_of = |addr: u32| -> BTreeSet<u32> {
        w.hits
            .keys()
            .filter(|(a, f, _)| *a == addr && f.is_some())
            .filter_map(|(_, f, _)| *f)
            .collect()
    };
    let by_who = |who: u8| -> u32 {
        w.hits
            .iter()
            .filter(|((_, _, k), _)| *k == who)
            .map(|(_, c)| *c)
            .sum()
    };
    let (in_nmi, in_irq, in_main) = (by_who(1), by_who(2), by_who(0));
    println!("position and camera writes: {in_nmi} in the NMI, {in_irq} in an IRQ, {in_main} in the main thread");
    let nmi = atlas.functions.iter().position(|f| {
        rf_rebuild::EntryKind::sole_interrupt(f.entry_kinds) == Some(rf_rebuild::EntryKind::Nmi)
    });
    let both = HookPoints {
        entry: true,
        exit: true,
    };
    let entry_only = HookPoints {
        entry: true,
        exit: false,
    };
    if let Some(nmi) = nmi {
        recipe.functions.push(function_row(&atlas, nmi as u32, "nmi", both, &format!(
            "observed: the atlas function the NMI vector targets in a recording; {} NMI sequences in {} observed frames; of the {} writes to the player's position words and the camera, {in_nmi} came while an NMI handler ran, {in_irq} in an IRQ handler and {in_main} in the main thread; a position write came after the NMI's entry in {} frames and after its exit in {}",
            w.nmis, w.frames, in_nmi + in_irq + in_main, w.final_not_at_entry, w.final_not_at_exit)));
    }
    let top_writer = |addr: u32| -> Option<u32> {
        w.hits
            .iter()
            .filter(|((a, f, _), _)| *a == addr && f.is_some())
            .max_by_key(|(_, c)| **c)
            .and_then(|((_, f, _), _)| *f)
    };
    for (name, addr) in [("player_x_writer", px), ("player_y_writer", py)] {
        if let Some(f) = top_writer(addr) {
            if recipe
                .functions
                .iter()
                .all(|r| r.address_hint != Some(atlas.functions[f as usize].entry_cpu))
            {
                let c: u32 = w
                    .hits
                    .iter()
                    .filter(|((a, g, _), _)| *a == addr && *g == Some(f))
                    .map(|(_, c)| *c)
                    .sum();
                recipe.functions.push(function_row(&atlas, f, name, both, &format!(
                    "observed: the most frequent writer of the word the picture follows ({c} writes in the play)")));
            }
        }
    }
    if let Some(f) = top_writer(cam_lo) {
        if recipe
            .functions
            .iter()
            .all(|r| r.address_hint != Some(atlas.functions[f as usize].entry_cpu))
        {
            recipe.functions.push(function_row(
                &atlas,
                f,
                "camera_step",
                entry_only,
                "observed: the most frequent writer of the camera's low byte",
            ));
        }
    }
    let _ = writers_of;
    let own_names = recipe.functions.len();
    let named_n = rf_rebuild::naming::merge_name_rows(&mut recipe, &atlas, &named.proposals);
    println!("{own_names} observed function rows, {named_n} rule-named rows");

    let ram = |addr: u32, width: u8, name: &str, source: String| RamMeaning {
        addr: RAM_BASE + addr,
        width,
        name: name.into(),
        source,
        grade: Grade::Observed,
    };
    recipe.ram.push(ram(px, 2, "player_x", format!("observed: in {:.0}% of the consecutive frames in which it moved a few pixels and the scroll stood still, a sprite entry moved by the same amount (the 16-bit position-word search of this test); word minus scroll is {} below a sprite x; it changed in {} frames", xw.share * 100.0, -xw.delta, xw.moved)));
    recipe.ram.push(ram(py, 2, "player_y", format!("observed: a sprite entry moved by the same amount in {:.0}% of the frames in which it moved a few pixels and the scroll stood still; word minus scroll is {} below a sprite y; it changed in {} frames", yw.share * 100.0, -yw.delta, yw.moved)));
    recipe.ram.push(ram(cam.lo as u32, 1, "camera_x_lo", "observed: equals the low byte of the BG1 horizontal scroll in nine frames of ten (W38-04 camera finder)".into()));
    if let Some(h) = cam.hi {
        recipe.ram.push(ram(h as u32, 1, "camera_x_hi", "observed: with the low byte it makes a 16-bit position that moves smoothly with the scroll".into()));
    }
    recipe.ram.push(ram(*rw as u32, 2, "room", format!("observed: a word that changed {} times in the play, each value held until the next door; the data windows are keyed by it", changes.len())));
    if let Some(f) = found
        .iter()
        .find(|f| rf_rebuild::objtable::grade_of(f) == Grade::Observed)
    {
        recipe.object_tables.push(to_row(
            f,
            rf_rebuild::objtable::grade_of(f),
            "; Super Metroid first rooms",
        ));
    }
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
    let _ = slots_from_rows;
    let overlay = rows_toml(
        &atlas,
        &recipe,
        &tried,
        "widescreen-objects",
        "widescreen-candidates",
        &[],
        "Super Metroid first rooms",
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

const RECIPE_HEADER: &str = "# W38-06: engine recipe for Super Metroid (SNES). Generated by\n# crates/rf-harness/tests/w38_06_recipe.rs (RF_W38_WRITE=1) from the repo's\n# own finders watching the running game (D-021: no disassembly, no community RAM\n# map or decompilation, no ROM bytes). Offsets and numbers only.";
const OVERLAY_HEADER: &str = "# W38-06: widescreen finder rows for Super Metroid, an OVERLAY on\n# recipe.toml (append this file's text to the recipe to load it, as for SMW).\n# Finder: crates/rf-rebuild/src/widescreen.rs; trials: w38_06_recipe.rs.\n# Every row is a candidate unless a lockstep trial kept it (see the grades).";
