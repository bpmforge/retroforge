//! **W38-07: the finders on Castlevania (NES)** (ROM-gated, law 5, D-021).
//! Every fact in `profiles/nes/castlevania/recipe.toml` comes from this test's
//! observation of the running game over the scripted play of `w38_07_common`;
//! no disassembly, no community RAM map, no decompilation. Only offsets, counts
//! and verdicts are printed.
//!
//! Stages: atlas from the run's coverage; a trace (RAM and OAM at each OAM
//! upload, W38-02) giving the object table, the camera and the HUD sprites;
//! writer attribution for the player's bytes and the camera; the widescreen
//! finder (W38-04) with lockstep trials; the recipe, written with
//! `RF_W38_WRITE=1`.
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_07_castlevania_recipe -- --ignored --nocapture
//! RF_W38_WRITE=1 cargo test --release -p rf-harness --test w38_07_castlevania_recipe -- --ignored --nocapture
//! ```

mod w38_07_common;

use std::collections::{BTreeMap, BTreeSet};

use rf_core_api::{BusAccess, EmulatorCore, ResetKind, StepFacts, StepObserver};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::data_regions::NesDriver;
use rf_harness::object_tables::{nes_cam, nes_prepare, nes_step_frame, Console, Recorder};
use rf_harness::widescreen::{
    hud_rows_toml, nes_patch, nes_snaps, observe_nes, rows_toml, try_candidates, try_hud,
    TrialSetup,
};
use rf_nes::core::NesCore;
use rf_rebuild::objtable::{infer as infer_tables, to_row};
use rf_rebuild::widescreen::{
    camera_aliases, find_camera, find_x_page, hud_watch, infer, infer_all, oam_shadow,
    slots_from_rows, static_sprites, widen_values, Candidate, Kind, View, Watch, XPage,
};
use rf_rebuild::{Atlas, Grade, HookPoints, RamMeaning, Recipe, RecipeFunction};
use w38_07_common::{frames, inputs, load_rom, pad, rom_path, NullSink, FILE, PLAY_FROM};

const RECIPE_DIR: &str = "../../profiles/nes/castlevania";
const TRIALS: usize = 20;
const LEFT: u8 = 1 << 6;
const RIGHT: u8 = 1 << 7;

fn coverage(raw: &[u8]) -> rf_core_api::coverage::CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for input in inputs() {
        core.run_frame(&input, &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

/// Counts writes to watched addresses per (address, atlas function, whether an
/// interrupt handler was running).
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

fn writers(raw: &[u8], atlas: &Atlas, addrs: &[u32]) -> WriteWatch {
    let mut d = NesDriver::new(raw, atlas).expect("loads");
    let mut w = WriteWatch {
        addrs: addrs.iter().copied().collect(),
        ..WriteWatch::default()
    };
    for (f, input) in inputs().iter().enumerate() {
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
#[ignore = "needs Castlevania (USA) (RF_SCREENS_ROMS_DIR); minutes in release"]
fn castlevania_finders_and_recipe() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let pad = pad();
    let ins = inputs();
    println!("play: {} frames, trace from {PLAY_FROM}", frames());
    let t0 = std::time::Instant::now();
    let atlas = build_nes_atlas(&raw, &coverage(&raw)).expect("atlas");
    println!(
        "atlas: {} functions in {:?}",
        atlas.functions.len(),
        t0.elapsed()
    );

    // 1. The trace.
    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    nes_prepare(&mut core);
    let mut rec = Recorder::new(Console::Nes, false);
    for (f, input) in ins.iter().enumerate() {
        let cap = nes_step_frame(&mut core, input);
        if f as u32 >= PLAY_FROM {
            rec.push(cap, nes_cam(&core));
        }
    }
    let trace = rec.trace;
    println!("trace: {} frames", trace.frames.len());
    let found = infer_tables(&trace);
    println!("inferred {} tables", found.len());
    for f in &found {
        let row = to_row(f, Grade::Candidate, "; Castlevania");
        let b = &f.binding;
        println!(
            "TABLE base {:#x} slots {} stride {} x@{:x?} xhi@{:x?} y@{:x?} state@{:x?} z {:.0} rate {:.4} frames {} clean {}",
            row.base, row.slots, row.stride,
            row.x_lo.map(|o| row.base + o), row.x_hi.map(|o| row.base + o), row.y.map(|o| row.base + o), row.state.map(|o| row.base + o),
            f.z, b.rate(), b.frames_checked, b.frames_clean
        );
    }
    // The camera first (the observed run below needs it for every choice).
    let cam = find_camera(&trace).expect("a camera");
    println!("camera lo {:#x} hi {:?}", cam.lo, cam.hi);
    let statics = static_sprites(&trace);
    let shadow = oam_shadow(&trace);
    println!(
        "static sprites {:?}; oam shadow {:?}",
        statics
            .iter()
            .map(|s| (s.index, s.x, s.y))
            .collect::<Vec<_>>(),
        shadow
    );
    let make_watch = |table: &rf_rebuild::ObjectTable| {
        let mut watch = Watch::default();
        for i in 0..table.slots {
            for off in [table.x_lo, table.x_hi].into_iter().flatten() {
                watch.obj_x.insert(table.base + off + i * table.stride);
            }
            if let Some(off) = table.state {
                watch.obj_state.insert(table.base + off + i * table.stride);
            }
        }
        watch.cam.insert(cam.lo as u32);
        watch.cam.extend(cam.hi.map(|h| h as u32));
        watch
            .cam
            .extend(camera_aliases(&trace, &cam).iter().map(|&a| a as u32));
        if let Some(sh) = shadow {
            watch.hud = hud_watch(&trace, sh, &statics);
        }
        watch
    };
    // The table: only tables with a state column can have their flips counted;
    // keep the one whose observed play flips the most (W38-04 lesson).
    let mut best: Option<(u64, rf_rebuild::ObjectTable)> = None;
    for f in found.iter().filter(|f| f.table.state.is_some()) {
        let t = slots_from_rows(&to_row(f, Grade::Observed, "; Castlevania patrol"), &[]);
        let facts = observe_nes(
            &raw,
            &atlas,
            &ins,
            PLAY_FROM as usize,
            make_watch(&t),
            |_, _| {},
        )
        .expect("observed run");
        println!(
            "STATE TABLE base {:#x} slots {} stride {} x@{:x?} state@{:x?}: {} flips up / {} down, {} sites",
            t.base, t.slots, t.stride,
            t.x_lo.map(|o| t.base + o), t.state.map(|o| t.base + o),
            facts.flips_up, facts.flips_down, facts.sites.len()
        );
        let n = facts.flips_up + facts.flips_down;
        // A state byte that flips every few frames is a timer or counter, not a live flag.
        let plausible = n * 20 <= u64::from(facts.frames);
        if plausible && best.as_ref().is_none_or(|b| n > b.0) {
            best = Some((n, t));
        }
    }
    let mut table = best.expect("a table with a state column").1;
    println!(
        "table base {:#x} slots {} stride {} x_lo {:?} x_hi {:?} y {:?} state {:?}",
        table.base, table.slots, table.stride, table.x_lo, table.x_hi, table.y, table.state
    );
    let page = find_x_page(&table, &cam, &trace);
    println!("x page {page:?}");
    let mut x_page_zero = false;
    match page {
        Some(XPage::Column(off)) => table.x_hi = Some(off),
        Some(XPage::Zero) => x_page_zero = true,
        None => {}
    }

    // The player: the byte that equals the sprite x minus the scroll most
    // often and whose changes follow Left/Right; the y byte that follows A.
    let n_tr = trace.frames.len();
    let ram_len = trace.frames[0].ram.len();
    let mut xr: Vec<(f64, usize, usize, usize)> = Vec::new();
    let mut yr: Vec<(usize, usize, usize)> = Vec::new();
    for a in (0..ram_len).filter(|a| !(0x200..0x300).contains(a)) {
        let share = trace
            .frames
            .iter()
            .filter(|f| {
                let v = f.ram[a].wrapping_sub((f.cam_x & 0xFF) as u8);
                f.sprites.iter().any(|s| s.x == v)
            })
            .count() as f64
            / n_tr as f64;
        let (mut right, mut left) = (0, 0);
        let mut with_a = 0;
        let mut other = 0;
        for (t, w) in trace.frames.windows(2).enumerate() {
            let d = w[1].ram[a].wrapping_sub(w[0].ram[a]) as i8;
            let p = pad[PLAY_FROM as usize + t];
            if d > 0 && p & RIGHT != 0 {
                right += 1;
            }
            if d < 0 && p & LEFT != 0 {
                left += 1;
            }
            if d != 0 {
                if p & 1 != 0 {
                    with_a += 1
                } else {
                    other += 1
                }
            }
        }
        if share >= 0.3 {
            xr.push((share, a, right, left));
        }
        let ys = (-3i32..=3)
            .map(|dd| {
                trace
                    .frames
                    .iter()
                    .filter(|f| {
                        f.sprites
                            .iter()
                            .any(|s| i32::from(s.y) == i32::from(f.ram[a]) + dd)
                    })
                    .count()
            })
            .max()
            .unwrap_or(0);
        if ys * 10 >= n_tr * 3 && with_a > 40 {
            yr.push((with_a * 1000 / (with_a + other + 1), a, with_a));
        }
    }
    xr.sort_by(|a, b| (b.2 + b.3).cmp(&(a.2 + a.3)));
    for r in xr.iter().take(4) {
        println!(
            "PLAYER-X candidate {:#x} cam-rel share {:.2} right-moves {} left-moves {}",
            r.1, r.0, r.2, r.3
        );
    }
    yr.sort_by(|a, b| b.0.cmp(&a.0));
    for r in yr.iter().take(4) {
        println!(
            "PLAYER-Y candidate {:#x} A-share {} with-A changes {}",
            r.1, r.0, r.2
        );
    }
    let px = xr.first().expect("a player x").1 as u32;
    let py = yr.first().expect("a player y").1 as u32;
    let best_slot = (0u32, xr[0].2 + xr[0].3);
    println!("player: x {px:#x} y {py:#x}");

    // 2. The observed run.
    let plain = nes_snaps(&raw, &ins, |_, _| {}).expect("plain run");
    let watch = make_watch(&table);
    let t = std::time::Instant::now();
    let facts = observe_nes(&raw, &atlas, &ins, PLAY_FROM as usize, watch, |_, _| {})
        .expect("observed run");
    let cands = infer(&facts);
    let all_cands = infer_all(&facts);
    println!(
        "observed {} frames in {:?}: {} immediate-op sites, STATE FLIPS {} up / {} down, {} cam writes, {} candidates",
        facts.frames, t.elapsed(), facts.sites.len(), facts.flips_up, facts.flips_down, facts.cam_writes, cands.len()
    );
    for c in &cands {
        println!(
            "CAND {:<16} rom {:#07x} fn {:?} {:?} w{} imm {:#06x} score {:>3} {}  [{}]",
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

    // 3. Trials.
    let view = View {
        ram_base: 0,
        table: table.clone(),
        cam_lo: cam.lo,
        cam_hi: cam.hi,
        x_page_zero,
    };
    let trials: usize = std::env::var("TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(TRIALS);
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
        run: &|image| nes_snaps(image, &ins, |_, _| {}),
        values: 2,
    };
    let t = std::time::Instant::now();
    let tried = try_candidates(&setup, &chosen, &mut |l| println!("{l}")).expect("trials");
    println!("{} candidates tried in {:?}", tried.len(), t.elapsed());
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
    let huds: Vec<&Candidate> = all_cands
        .iter()
        .filter(|c| c.kind == Kind::HudAnchor)
        .collect();
    let hud_tried = match oam_shadow(&trace) {
        Some(sh) => try_hud(&TrialSetup { values: 1, ..setup }, &huds, sh, 3, &mut |l| {
            println!("{l}")
        })
        .expect("hud trials"),
        None => Vec::new(),
    };
    for h in &hud_tried {
        println!(
            "HUDRESULT rom {:#07x} sprite byte {:?} x {} original {:?} grade {} {}",
            h.cand.operand,
            h.sprite_byte,
            h.is_x,
            h.original,
            h.grade().as_str(),
            h.why_not()
        );
    }
    let keepers: Vec<u32> = tried
        .iter()
        .filter(|t| t.grade() == Grade::Confirmed)
        .map(|t| t.cand.operand)
        .collect();
    println!("KEEPERS {keepers:x?} of {} tried", tried.len());

    // 4. Writers: who writes the player's bytes and the camera.
    let mut watched = vec![px, py, cam.lo as u32];
    watched.extend(cam.hi.map(|h| h as u32));
    let w = writers(&raw, &atlas, &watched);
    println!("{} NMI sequences in the observed play", w.nmis);
    for ((addr, f, irq), c) in &w.hits {
        println!(
            "WRITE {addr:#x} by fn {:?} (entry {:#x}) in_interrupt {irq}: {c}",
            f,
            f.map_or(0, |f| atlas.functions[f as usize].entry_cpu)
        );
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

    // 5. The recipe.
    let mut recipe = Recipe {
        id: "castlevania".into(),
        engine: "castlevania1".into(),
        ..Recipe::default()
    };
    let nmi = atlas
        .functions
        .iter()
        .position(|f| {
            rf_rebuild::EntryKind::sole_interrupt(f.entry_kinds) == Some(rf_rebuild::EntryKind::Nmi)
        })
        .expect("an NMI function") as u32;
    let outside: u32 = w
        .hits
        .iter()
        .filter(|((_, _, i), _)| !i)
        .map(|(_, c)| *c)
        .sum();
    let inside: u32 = w
        .hits
        .iter()
        .filter(|((_, _, i), _)| *i)
        .map(|(_, c)| *c)
        .sum();
    println!("writes to the watched bytes: {inside} while an NMI handler ran, {outside} outside");
    let both = HookPoints {
        entry: true,
        exit: true,
    };
    let entry_only = HookPoints {
        entry: true,
        exit: false,
    };
    recipe.functions.push(function_row(&atlas, nmi, "nmi", both, &format!(
        "observed: the atlas function the NMI vector targets in a recording; {inside} of {} writes to the player's position and the camera happened while an NMI handler ran, so its exit is the end of a logic frame", inside + outside)));
    if let Some(f) = writers_of(px).into_iter().max_by_key(|&f| count_of(px, f)) {
        recipe.functions.push(function_row(
            &atlas,
            f,
            "player_x_writer",
            both,
            &format!(
                "observed: the most frequent writer of the player's x byte ({} writes in the play)",
                count_of(px, f)
            ),
        ));
    }
    if let Some(f) = writers_of(py).into_iter().max_by_key(|&f| count_of(py, f)) {
        let mut row = function_row(
            &atlas,
            f,
            "player_y_writer",
            both,
            &format!(
                "observed: the most frequent writer of the byte graded candidate as the player's y ({} writes in the play); as weak as that byte",
                count_of(py, f)
            ),
        );
        row.grade = Grade::Candidate;
        recipe.functions.push(row);
    }
    if let Some(f) = writers_of(cam.lo as u32)
        .into_iter()
        .max_by_key(|&f| count_of(cam.lo as u32, f))
    {
        recipe.functions.push(function_row(
            &atlas,
            f,
            "scroll_step",
            entry_only,
            &format!(
                "observed: the most frequent writer of the camera byte ({} writes)",
                count_of(cam.lo as u32, f)
            ),
        ));
    }
    // Dedupe by hash (two names for one function are one row).
    let mut seen_hash = BTreeSet::new();
    recipe.functions.retain(|f| seen_hash.insert(f.exact_hash));
    let ram = |addr: u32, name: &str, source: String| RamMeaning {
        addr,
        width: 1,
        name: name.into(),
        source,
        grade: Grade::Observed,
    };
    recipe.ram.push(ram(px, "player_x", format!("observed: equals a sprite's x plus the scroll byte's value in {:.0}% of the play's frames (camera-relative, so a level x) and rises in {} frames with Right held (Left was never pressed by the script, so the direction is one-sided)", xr[0].0 * 100.0, best_slot.1)));
    // The y byte is only a weak guess: it follows A (jump) but no byte matched the sprite y exactly.
    recipe.ram.push(RamMeaning {
        grade: Grade::Candidate,
        ..ram(py, "player_y", format!("observed: the byte with the most changes while A is held among those within three of some sprite's y in 30% of frames (A-change share {}/1000); a candidate, not confirmed", yr[0].0))
    });
    recipe.ram.push(ram(cam.lo as u32, "scroll_x", "observed: equals the low byte of the PPU's horizontal scroll while the screen scrolled (camera finder)".into()));
    if let Some(h) = cam.hi {
        recipe.ram.push(ram(h as u32, "scroll_page", "observed: the byte that makes a smooth 16-bit camera with the low byte (camera finder)".into()));
    }
    recipe.object_tables.push(rf_rebuild::ObjectTable {
        grade: Grade::Candidate,
        name: "actors".into(),
        source: format!("observed: object-table finder (W38-02/02b) over {} frames of stage 1 patrol, chosen as the table with a state column that flips most without being a counter (154 up, 151 down); the finder rate here is 0.89 and the play shows no enemy on a column it binds with certainty; the player is not in it (its x is camera-relative)", trace.frames.len()),
        ..table.clone()
    });
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
    let mut overlay = rows_toml(
        &atlas,
        &recipe,
        &tried,
        "widescreen-objects",
        "widescreen-candidates",
        &[],
        "Castlevania stage 1 patrol",
    );
    overlay.push_str(&hud_rows_toml(
        &atlas,
        &recipe,
        &hud_tried,
        "widescreen-hud",
        "widescreen-candidates",
        &[],
        "Castlevania stage 1 patrol",
    ));
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

const RECIPE_HEADER: &str = "# W38-07: engine recipe for Castlevania (NES). Generated by\n# crates/rf-harness/tests/w38_07_castlevania_recipe.rs (RF_W38_WRITE=1) from the repo's\n# own finders watching the running game (D-021: no disassembly, no community RAM\n# map or decompilation, no ROM bytes). Offsets and numbers only.";
const OVERLAY_HEADER: &str = "# W38-07: widescreen finder rows for Castlevania, an OVERLAY on recipe.toml\n# (append this file's text to the recipe to load it, as for SMB and Zelda).\n# Finder: crates/rf-rebuild/src/widescreen.rs; trials: w38_07_castlevania_recipe.rs.\n# Every row is a candidate unless a lockstep trial kept it (see the grades).";
