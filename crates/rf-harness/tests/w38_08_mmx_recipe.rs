//! **W38-08: the finders on Mega Man X (SNES)** (ROM-gated, law 5, D-021).
//! Every fact in `profiles/snes/mega-man-x/recipe.toml` comes from this test's
//! observation of the running game over the scripted play of
//! `w38_08_mmx_common`; no disassembly, no community RAM map, no
//! decompilation, no address read from the repo's older profile. Only offsets,
//! counts and verdicts are printed.
//!
//! What this recipe has and does not have is stated in the rows' sources: the
//! player's x (a 16-bit word that is on the picture in 99% of the frames), the
//! camera (the scroll bytes), the vblank handler(s) with hooks, and the
//! function that moves the player. No object table was bound (the W38-02
//! finder returns only chance fits here; see the test's printout), so no
//! widescreen trial can run and the overlay carries no rows.
//!
//! ```text
//! RF_W38_WRITE=1 cargo test --release -p rf-harness --test w38_08_mmx_recipe -- --ignored --nocapture
//! ```

// Frame-by-byte grids are indexed on both axes by design.
#![allow(clippy::needless_range_loop)]

mod w38_08_mmx_common;

use std::collections::{BTreeMap, BTreeSet};

use rf_core_api::{BusAccess, EmulatorCore, ResetKind, StepFacts, StepObserver};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::data_regions::SnesDriver;
use rf_harness::object_tables::{snes_cam, snes_step_frame, Console, Recorder};
use rf_rebuild::objtable::{Frame, Trace};
use rf_rebuild::widescreen::{find_camera, oam_shadow, static_sprites};
use rf_rebuild::{Atlas, Grade, HookPoints, RamMeaning, Recipe, RecipeFunction};
use rf_snes::core::SnesCore;
use w38_08_mmx_common::{
    highway_script, inputs_of, load_rom, rom_path, NullSink, FILE, PLAY_FROM, RAM_BASE,
};

const RECIPE_DIR: &str = "../../profiles/snes/mega-man-x";

fn coverage(raw: &[u8], ins: &[rf_core_api::InputFrame]) -> rf_core_api::coverage::CoverageLog {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for input in ins {
        core.system_mut().bus.joypads.ports[0] = input.ports[0];
        core.run_frame(&rf_core_api::InputFrame::empty(), &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

fn trace_of(raw: &[u8], ins: &[rf_core_api::InputFrame]) -> Trace {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut rec = Recorder::new(Console::Snes, false);
    for (f, input) in ins.iter().enumerate() {
        if f as u32 >= PLAY_FROM {
            let cap = snes_step_frame(&mut core, input);
            rec.push(cap, snes_cam(&core));
        } else {
            core.system_mut().bus.joypads.ports[0] = input.ports[0];
            core.run_frame(input, &mut NullSink);
        }
    }
    rec.trace
}

fn share(frames: &[Frame], pred: impl Fn(&Frame) -> bool) -> f64 {
    frames.iter().filter(|f| pred(f)).count() as f64 / frames.len().max(1) as f64
}

/// Bytes that step up with Right and down with Left and hold still otherwise
/// (best first): `(addr, right share, left share, idle share)`.
fn walkers(t: &Trace, pad: &[u16], right: u16, left: u16) -> Vec<(usize, f64, f64, f64)> {
    let n = t.frames.len();
    let mut out = Vec::new();
    for a in 0..t.frames[0].ram.len() {
        let (mut ru, mut rn, mut ld, mut ln, mut ic, mut inn) = (0, 0, 0, 0, 0, 0);
        for i in 1..n {
            let pd = pad[i];
            let d = t.frames[i].ram[a].wrapping_sub(t.frames[i - 1].ram[a]);
            if pd & right != 0 && pd & left == 0 {
                rn += 1;
                ru += i32::from((1..=8).contains(&d));
            } else if pd & left != 0 && pd & right == 0 {
                ln += 1;
                ld += i32::from((248..=255).contains(&d));
            } else if pd & (left | right) == 0 {
                inn += 1;
                ic += i32::from(d != 0);
            }
        }
        out.push((
            a,
            f64::from(ru) / f64::from(rn.max(1)),
            f64::from(ld) / f64::from(ln.max(1)),
            f64::from(ic) / f64::from(inn.max(1)),
        ));
    }
    out.sort_by(|x, y| (y.1 + y.2 - 3.0 * y.3).total_cmp(&(x.1 + x.2 - 3.0 * x.3)));
    out
}

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
                // Low work RAM is also seen in every bank's first 8 KB: key by the 16-bit address.
                *self
                    .hits
                    .entry((a.addr & 0xFFFF, facts.function, self.in_interrupt))
                    .or_default() += 1;
            }
        }
        if !facts.interrupt && accesses.first().is_some_and(|a| a.value == 0x40) {
            self.in_interrupt = false;
        }
    }
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
        // The 65816 decodes by the M and X flags at entry: one handler is a
        // different atlas function in each mode, told apart by `mode`.
        mode: Some(f.entry_mode),
        entry: rf_rebuild::EntryKind::sole_interrupt(f.entry_kinds),
        insn_count: Some(f.insn_count),
        hooks,
        source: source.to_string(),
        grade: Grade::Observed,
        ..RecipeFunction::default()
    }
}

#[test]
#[ignore = "needs Mega Man X (USA) (RF_SCREENS_ROMS_DIR); minutes in release"]
fn mmx_finders_and_recipe() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let pads = highway_script();
    let ins = inputs_of(&pads);
    println!("session: {} frames, one power-on segment", ins.len());
    let atlas = build_snes_atlas(&raw, &coverage(&raw, &ins)).expect("atlas");
    println!("atlas: {} functions", atlas.functions.len());
    let trace = trace_of(&raw, &ins);
    println!("trace: {} frames", trace.frames.len());

    let cam = find_camera(&trace).expect("a camera");
    let (cam_lo, cam_hi) = (cam.lo, cam.hi.expect("a camera high byte"));
    println!("camera lo {cam_lo:#x} hi {cam_hi:#x}");
    let camw = |f: &Frame| i32::from(f.ram[cam_lo]) + 256 * i32::from(f.ram[cam_hi]);
    let statics = static_sprites(&trace);
    println!(
        "static sprites {:?}; oam shadow {:?}",
        statics
            .iter()
            .map(|s| (s.index, s.x, s.y))
            .collect::<Vec<_>>(),
        oam_shadow(&trace)
    );
    let right = 1u16 << rf_input::SnesButton::Right.bit();
    let left = 1u16 << rf_input::SnesButton::Left.bit();
    let pad: Vec<u16> = pads[PLAY_FROM as usize..].to_vec();
    let walk = walkers(&trace, &pad, right, left);
    // The player's x: a walker byte that, as the low byte of a 16-bit word,
    // puts the word on the picture (0..256 from the camera) in 95% of frames.
    let on_screen = |lo: usize| {
        share(&trace.frames, |f| {
            let w = i32::from(f.ram[lo]) + 256 * i32::from(f.ram[lo + 1]);
            (0..256).contains(&(w - camw(f)))
        })
    };
    let (px, w0) = walk
        .iter()
        .take(12)
        .map(|w| (w.0, w))
        .find(|(a, _)| on_screen(*a) >= 0.95 && *a != cam_lo)
        .expect("a player x word");
    let on = on_screen(px);
    println!(
        "player x word {px:#x}: right-steps {:.2} left-steps {:.2} idle {:.3}; on the picture in {on:.3} of frames",
        w0.1, w0.2, w0.3
    );
    // Writers of the player's x, the camera and the object bytes near it.
    let watched: Vec<u32> = vec![px as u32, px as u32 + 1, cam_lo as u32, cam_hi as u32];
    let mut d = SnesDriver::new(&raw, &atlas).expect("loads");
    let mut w = WriteWatch {
        addrs: watched.iter().flat_map(|a| [RAM_BASE + a, *a]).collect(),
        ..WriteWatch::default()
    };
    for (f, input) in ins.iter().enumerate() {
        if (f as u32) < PLAY_FROM {
            d.frame(input, false).expect("frame");
        } else {
            d.frame_with(input, &mut w).expect("frame");
        }
    }
    let inside: u32 = w
        .hits
        .iter()
        .filter(|((_, _, i), _)| *i)
        .map(|(_, c)| *c)
        .sum();
    let outside: u32 = w
        .hits
        .iter()
        .filter(|((_, _, i), _)| !*i)
        .map(|(_, c)| *c)
        .sum();
    println!("{} NMI sequences; writes to the watched bytes: {inside} inside an NMI handler, {outside} outside", w.nmis);
    for ((addr, f, irq), c) in &w.hits {
        println!(
            "WRITE {addr:#x} by fn {:?} (entry {:#x}) in_interrupt {irq}: {c}",
            f,
            f.map_or(0, |f| atlas.functions[f as usize].entry_cpu)
        );
    }
    let count_of = |addr: u32, f: u32| -> u32 {
        w.hits
            .iter()
            .filter(|((a, g, _), _)| *a == addr && *g == Some(f))
            .map(|(_, c)| *c)
            .sum()
    };
    let writers_of = |addr: u32| -> BTreeSet<u32> {
        w.hits
            .keys()
            .filter(|(a, f, _)| *a == addr && f.is_some())
            .filter_map(|(_, f, _)| *f)
            .collect()
    };

    let mut recipe = Recipe {
        id: "mega-man-x".into(),
        engine: "mega-man-x".into(),
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
        let f = &atlas.functions[id as usize];
        let name = if k == 0 {
            "nmi".to_string()
        } else {
            format!("nmi_{:06x}_m{}", f.entry_cpu, f.entry_mode)
        };
        recipe.functions.push(function_row(&atlas, id, &name, both, &format!(
            "observed: an atlas function the NMI vector targets in a recording (the NMI may be entered in several decode modes, so every one is hooked); {inside} of {} writes to the player's x and the scroll happened while an NMI handler ran, {outside} outside it",
            inside + outside)));
    }
    let player_fn = writers_of(px as u32)
        .into_iter()
        .max_by_key(|&f| count_of(px as u32, f));
    if let Some(f) = player_fn {
        recipe.functions.push(function_row(
            &atlas,
            f,
            "player_move",
            both,
            &format!(
                "observed: the most frequent writer of the player's x word ({} writes in the play)",
                count_of(px as u32, f)
            ),
        ));
    }
    let top_cam = writers_of(cam_lo as u32)
        .into_iter()
        .max_by_key(|&f| count_of(cam_lo as u32, f));
    if let Some(f) = top_cam {
        recipe.functions.push(function_row(&atlas, f, "scroll_step", entry_only, &format!("observed: the most frequent writer of the camera's low byte ({} writes in the play)", count_of(cam_lo as u32, f))));
    }
    let ram = |addr: usize, width: u8, name: &str, source: String| RamMeaning {
        addr: RAM_BASE + addr as u32,
        width,
        name: name.into(),
        source,
        grade: Grade::Observed,
    };
    recipe.ram.push(ram(px, 2, "player_x", format!("observed: a 16-bit word that steps up with Right in {:.0}% and down with Left in {:.0}% of the frames, never moves with no direction held ({:.1}% of idle frames), and is on the picture (0..256 from the camera word) in {:.0}% of the frames", w0.1 * 100.0, w0.2 * 100.0, w0.3 * 100.0, on * 100.0)));
    recipe.ram.push(ram(cam_lo, 2, "camera_x", "observed: the camera finder's low byte and its high byte next to it: a 16-bit horizontal playfield position (W38-04 camera finder)".into()));
    recipe.tiles = Vec::new();
    let text = rf_profiles::recipe::recipe_to_toml(&recipe);
    let back = rf_profiles::recipe::load_recipe_str(&text).expect("the recipe round-trips");
    assert_eq!(back, recipe);
    let report = rf_rebuild::match_recipe(&atlas, &recipe);
    println!(
        "recipe: {} functions ({} unmatched), {} ram",
        recipe.functions.len(),
        report.unmatched.len(),
        recipe.ram.len()
    );
    assert!(report.unmatched.is_empty());
    if std::env::var("RF_W38_WRITE").is_ok() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(RECIPE_DIR);
        std::fs::write(dir.join("recipe.toml"), format!("{RECIPE_HEADER}\n{text}"))
            .expect("writes");
        std::fs::write(dir.join("recipe-widescreen.toml"), OVERLAY_HEADER).expect("writes");
        println!("wrote the recipe and the overlay to {}", dir.display());
    }
}

const RECIPE_HEADER: &str = "# W38-08: engine recipe for Mega Man X (SNES). Generated by\n# crates/rf-harness/tests/w38_08_mmx_recipe.rs (RF_W38_WRITE=1) from the repo's\n# own finders watching the running game (D-021: no disassembly, no community RAM\n# map or decompilation, no ROM bytes, no address from the older profile.toml).\n# Offsets and numbers only.";
const OVERLAY_HEADER: &str = "# W38-08: widescreen overlay for Mega Man X: no rows. No object table was\n# bound (the W38-02 finder returned only chance fits), so no widescreen trial\n# could be run; see crates/rf-harness/tests/w38_08_mmx_recipe.rs.\n";
