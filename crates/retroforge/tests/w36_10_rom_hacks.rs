//! **W36-10: the SMW recipe on five Lunar Magic hacks** (ROM-gated, law 5;
//! the patches are the user's downloads and are only read, never committed).
//!
//! Per hack (and once on the unpatched game as the control):
//! 1. apply the `.bps` in memory to the base ROM (the patch's own source size
//!    and CRC32 must match, otherwise the hack is reported and skipped);
//! 2. boot it and drive into gameplay generically: Start and A presses (and a
//!    few directions on the map) until the game-mode fact `$7E0100 == $14`
//!    holds, then jump-and-run for 1,500 frames, recording coverage;
//! 3. build the hack's own atlas and match `profiles/snes/super-mario-world/
//!    recipe.toml` on it: rows exact / by shape / unmatched;
//! 4. lockstep rebuilt versus plain over the drive script, zero divergence;
//! 5. hook the recipe's `nmi` row read-only and compare the count with the NMI
//!    sequences a plain core took into that function's entry, counted an
//!    instruction at a time.
//!
//! Only counts and names are printed (D-021). `RF_HACKS_DIR` overrides
//! `<roms>/snes/hacks`; `RF_W36_10_ONLY=<substring>` picks hacks;
//! `RF_W36_10_BUNDLES=<dir>` is where a divergence's repro bundle goes.
//!
//! ```text
//! cargo test --release -p retroforge --test w36_10_rom_hacks -- --ignored --nocapture
//! ```

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use rf_core_api::{CoreSink, EmulatorCore, InputFrame, ResetKind, Step};
use rf_harness::lockstep::Options;
use rf_plugin_sdk::{Budget, Capabilities, HookHost, Manifest};
use rf_rebuild::{match_recipe, Confidence};
use rf_snes::core::SnesCore;

mod w36_10_common;
use w36_10_common::*;

const CAMERA_X: u32 = 0x7E_001A;
/// Game mode on the overworld map (profile fact: `0x0e` on the map).
const MAP: u8 = 0x0E;
/// Frames the boot drive may take before the hack is called unreachable.
const BOOT_LIMIT: usize = 4000;

/// (label, folder under the hacks dir, patch path within it).
const HACKS: &[(&str, &str)] = &[
    (
        "Super Skyler World",
        "Super Skyler World/Super Skyler World.bps",
    ),
    (
        "Return to Dinosaur Land",
        "SMW- RDL/English/Return to Dinosaur Land.bps",
    ),
    (
        "Kaizo Mario World",
        "Kaizo Mario World/Kaizo Mario (English).bps",
    ),
    ("A Simple Story", "A Simple Story/A Simple Story.bps"),
    (
        "Brutal Mario Demo 7",
        "Brutal Mario Demo 7/Brutal Mario Demo 7/English/Brutal Mario Demo 7 [English].bps",
    ),
];

fn hacks_dir() -> PathBuf {
    std::env::var("RF_HACKS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(&roms_root()).join("snes").join("hacks"))
}

fn recipe_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/snes/super-mario-world/recipe.toml")
}

/// The overworld: on visit `k` push one direction (cycling), wait for the walk,
/// press A to enter whatever level Mario stands on, then wait.
fn map_mask(frame: usize, visit: usize) -> u16 {
    const DIRS: [&str; 4] = ["L", "R", "U", "D"];
    match frame {
        1..=40 => button_mask(DIRS[visit % DIRS.len()]),
        161..=168 => button_mask("A"),
        _ => 0,
    }
}

fn word(core: &SnesCore, addr: u32) -> u16 {
    u16::from(core.peek(addr)) | (u16::from(core.peek(addr + 1)) << 8)
}

/// What the generic drive did.
struct Drive {
    pad: Vec<u16>,
    log: rf_core_api::CoverageLog,
    reached_at: Option<usize>,
    in_level: usize,
    cam_span: u16,
    modes_seen: Vec<u8>,
}

/// Buttons for boot frame `f` outside a level and the map: Start and A
/// alternating every 120 frames. `f` only picks the phase.
fn boot_mask(f: usize) -> u16 {
    let (slot, p) = (f / 120, f % 120);
    let hold = |m: &str, n: usize| if p < n { button_mask(m) } else { 0 };
    if f < 200 {
        return 0;
    }
    if slot % 2 == 0 {
        hold("S", 6)
    } else {
        hold("A", 6)
    }
}

/// Drive a plain core into play, recording coverage the whole way. Bounded:
/// at most `BOOT_LIMIT` boot frames then `PLAY_FRAMES` play frames.
fn drive(raw: &[u8]) -> Drive {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    let mut pad: Vec<u16> = Vec::new();
    let play = frames_of("R200 R+B14 R22 R+B14 R22 L40 R+B14 R22 R+B14 R22 R120 B8 w30");
    let (mut reached_at, mut in_level) = (None, 0usize);
    let (mut cam_min, mut cam_max) = (u16::MAX, 0u16);
    let mut modes_seen: Vec<u8> = Vec::new();
    // Law 8: every pass appends one frame; the loop ends at a fixed length or
    // once the level has scrolled and been played for `PLAY_FRAMES`.
    let mut visit = 0usize;
    let (mut map_frame, mut map_visits) = (0usize, 0usize);
    let (mut last_mode, mut changes): (Option<u8>, usize) = (None, 0);
    while pad.len() < BOOT_LIMIT + PLAY_FRAMES {
        let mode = core.peek(GAME_MODE);
        if last_mode != Some(mode) && (mode == MAP || mode == IN_LEVEL) && changes < 16 {
            changes += 1;
            print!(" {}:{mode:02x}", pad.len());
        }
        if !modes_seen.contains(&mode) && modes_seen.len() < 64 {
            modes_seen.push(mode);
        }
        if mode == IN_LEVEL && pad.len() > 300 {
            reached_at.get_or_insert(pad.len());
        } else {
            visit = 0;
        }
        if mode == MAP && last_mode != Some(MAP) {
            map_visits += 1;
            map_frame = 0;
        }
        last_mode = Some(mode);
        if in_level >= PLAY_FRAMES && cam_max.saturating_sub(cam_min) >= 200 {
            break;
        }
        let mask = if mode == IN_LEVEL && reached_at.is_some() {
            visit += 1;
            play[visit % play.len()]
        } else {
            {
                if mode == MAP {
                    map_frame += 1;
                    map_mask(map_frame, map_visits)
                } else {
                    boot_mask(pad.len())
                }
            }
        };
        pad.push(mask);
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&InputFrame::empty(), &mut Null);
        if reached_at.is_some() && core.peek(GAME_MODE) == IN_LEVEL {
            in_level += 1;
            let cam = word(&core, CAMERA_X);
            (cam_min, cam_max) = (cam_min.min(cam), cam_max.max(cam));
        }
    }
    modes_seen.sort_unstable();
    Drive {
        log: core.take_coverage().expect("armed"),
        pad,
        reached_at,
        in_level,
        cam_span: cam_max.saturating_sub(cam_min),
        modes_seen,
    }
}

/// The same measurements for a fixed pad script (the W36-08b session: menus,
/// the map, Yoshi's Island 1 as the SMW profile's facts describe them). Used
/// when the generic drive never scrolls a level.
fn drive_script(raw: &[u8], pad: Vec<u16>) -> Drive {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    let (mut reached_at, mut in_level) = (None, 0usize);
    let (mut cam_min, mut cam_max) = (u16::MAX, 0u16);
    let mut modes_seen: Vec<u8> = Vec::new();
    for (f, &mask) in pad.iter().enumerate() {
        let mode = core.peek(GAME_MODE);
        if !modes_seen.contains(&mode) && modes_seen.len() < 64 {
            modes_seen.push(mode);
        }
        if mode == IN_LEVEL && f > 300 {
            reached_at.get_or_insert(f);
            in_level += 1;
            let cam = word(&core, CAMERA_X);
            (cam_min, cam_max) = (cam_min.min(cam), cam_max.max(cam));
        }
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&InputFrame::empty(), &mut Null);
    }
    modes_seen.sort_unstable();
    Drive {
        log: core.take_coverage().expect("armed"),
        pad,
        reached_at,
        in_level,
        cam_span: cam_max.saturating_sub(cam_min),
        modes_seen,
    }
}

/// How control reached an address, counted on a plain core.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Arrivals {
    /// An interrupt dispatch landing there in the function's decode mode.
    dispatch: u64,
    /// An interrupt dispatch landing there in another decode mode.
    dispatch_other_mode: u64,
    /// Any other way of getting there (a call or a jump).
    other: u64,
}

/// Arrivals at `entry24` by a native or emulation interrupt dispatch (it
/// pushes 4 bytes in native mode, 3 in emulation) versus anything else,
/// counted on a plain core an instruction at a time.
fn count_arrivals(raw: &[u8], pad: &[u16], entry24: u32, mode: u8) -> Arrivals {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut n = Arrivals::default();
    for &mask in pad {
        core.system_mut().bus.joypads.ports[0] = mask;
        // Law 8: a frame is a few tens of thousands of instructions; the
        // step reports the frame boundary, and the cap is a backstop.
        for _ in 0..4_000_000 {
            let (sp0, e0, pc0) = {
                let c = &core.system().cpu;
                (c.sp, c.e, c.pc24())
            };
            let r = core.step(Step::Instruction, &mut Null);
            let c = &core.system().cpu;
            if c.pc24() == entry24 && pc0 != entry24 {
                let dispatched = sp0.wrapping_sub(c.sp) == if e0 { 3 } else { 4 };
                let landed = rf_snes::coverage::mode_index(c.e, c.p);
                match (dispatched, landed == mode) {
                    (true, true) => n.dispatch += 1,
                    (true, false) => n.dispatch_other_mode += 1,
                    _ => n.other += 1,
                }
            }
            if r.frame_complete {
                break;
            }
        }
    }
    n
}

fn manifest() -> Manifest {
    Manifest {
        id: "w36-10-test".into(),
        version: "0.1.0".into(),
        api: "0.1".into(),
        license: "MIT".into(),
        provenance: "RetroForge W36-10 test".into(),
        capabilities: Capabilities {
            function_hooks: true,
            ..Capabilities::default()
        },
    }
}

struct Row {
    label: String,
    patch_bytes: Option<usize>,
    rom_bytes: usize,
    reached: String,
    atlas_fns: usize,
    exact: usize,
    shape: usize,
    unmatched: Vec<String>,
    matched_names: Vec<String>,
    lockstep: String,
    fallback: String,
    nmi: String,
    nmi_ok: bool,
    /// Rows the pre-W38-00 matcher (no entry kinds) leaves unmatched.
    unmatched_before: Vec<String>,
    /// Why each unmatched row is unmatched (W38-00): removed / not_reached /
    /// changed by a superset scan, or `unscanned` without an insn_count.
    reasons: Vec<(String, &'static str)>,
}

/// The unmatched `row`s of `report` classified by a superset scan of the
/// hack's own image (`raw`, copier header and all) against what `log` ran.
fn unmatched_reasons(
    raw: &[u8],
    log: &rf_core_api::CoverageLog,
    recipe: &rf_rebuild::Recipe,
    unmatched: &[String],
) -> Vec<(String, &'static str)> {
    let rf_cart::Cartridge::Snes { header, .. } = rf_cart::Cartridge::load(raw).expect("snes")
    else {
        panic!("not a SNES image");
    };
    let rom = if header.had_copier_header {
        &raw[512..]
    } else {
        raw
    };
    let isa = rf_snes::rebuild::Wdc65816::new(header.map_mode, rom.len());
    let executed = |o: usize| log.prg_flags.get(o).copied().unwrap_or(0) != 0;
    unmatched
        .iter()
        .map(|name| {
            let row = recipe
                .functions
                .iter()
                .find(|f| &f.name == name)
                .expect("row");
            let why = rf_rebuild::classify_unmatched(&isa, rom, &executed, row)
                .map_or("unscanned", |r| r.as_str());
            (name.clone(), why)
        })
        .collect()
}

impl Row {
    fn empty(label: &str) -> Self {
        Self {
            label: label.to_string(),
            patch_bytes: None,
            rom_bytes: 0,
            reached: String::new(),
            atlas_fns: 0,
            exact: 0,
            shape: 0,
            unmatched: vec![],
            matched_names: vec![],
            lockstep: "zero divergence (drive only)".into(),
            fallback: String::new(),
            nmi: String::new(),
            nmi_ok: true,
            unmatched_before: vec![],
            reasons: vec![],
        }
    }
}

fn analyse(
    label: &str,
    patch_bytes: Option<usize>,
    raw: &[u8],
    recipe: &rf_rebuild::Recipe,
) -> Row {
    let t = Instant::now();
    let mut d = drive(raw);
    let mut how = "generic Start/A drive".to_string();
    if d.cam_span < 200 {
        // Never scrolled a level: also try the SMW session script.
        let s = drive_script(raw, smw_session());
        println!(
            "[{label}] generic drive: in level {} frames, camera span {}; SMW script: in level {} frames, camera span {}",
            d.in_level, d.cam_span, s.in_level, s.cam_span
        );
        if s.cam_span > d.cam_span {
            d = s;
            how = "the SMW session script (the generic drive never scrolled a level)".into();
        }
    }
    if std::env::var("RF_W36_10_DRIVE_ONLY").is_ok() {
        println!(
            "[{label}] drive only: reached {:?}, in level {}, camera span {}",
            d.reached_at, d.in_level, d.cam_span
        );
        return Row::empty(label);
    }
    let reached = match d.reached_at {
        Some(at) => format!(
            "yes ($7E0100 == $14 from frame {at}); {} in-level frames, camera span {}; {how}",
            d.in_level, d.cam_span
        ),
        None => format!("NO; modes seen {:02x?}; {how}", d.modes_seen),
    };
    println!(
        "[{label}] drive {:?}: {reached}; {} frames",
        t.elapsed(),
        d.pad.len()
    );
    let atlas = rf_harness::atlas::build_snes_atlas(raw, &d.log).expect("atlas");
    let report = match_recipe(&atlas, recipe);
    let exact = report
        .matched
        .iter()
        .filter(|m| m.confidence == Confidence::Exact)
        .count();
    let shape = report.matched.len() - exact;
    let mut row = Row {
        label: label.to_string(),
        patch_bytes,
        rom_bytes: raw.len(),
        reached,
        atlas_fns: atlas.functions.len(),
        exact,
        shape,
        unmatched: report.unmatched.clone(),
        matched_names: report.matched.iter().map(|m| m.name.clone()).collect(),
        lockstep: String::new(),
        fallback: String::new(),
        nmi: String::new(),
        nmi_ok: true,
        unmatched_before: match_recipe(&atlas, &without_entry_kinds(recipe)).unmatched,
        reasons: unmatched_reasons(raw, &d.log, recipe, &report.unmatched),
    };
    println!(
        "[{label}] before W38-00 (address-hint twins) unmatched {:?}; now unmatched with reasons {:?}",
        row.unmatched_before, row.reasons
    );
    // For a row that insists on an entry kind and found nothing: what the
    // atlas does hold with that entry kind (addresses, modes and sizes only).
    for name in &report.unmatched {
        let Some(want) = recipe.functions.iter().find(|f| &f.name == name) else {
            continue;
        };
        let Some(kind) = want.entry else { continue };
        let held: Vec<String> = atlas
            .functions
            .iter()
            .filter(|f| f.entry_kinds & kind.bit() != 0)
            .map(|f| {
                format!(
                    "{:#08x} mode {} {} insns {} bytes shape {}",
                    f.entry_cpu,
                    f.entry_mode,
                    f.insn_count,
                    f.byte_len,
                    if f.shape_hash == want.shape_hash {
                        "same"
                    } else {
                        "differs"
                    }
                )
            })
            .collect();
        println!(
            "[{label}]   `{name}` wants entry {} (mode {:?}, {:?} insns); the atlas holds: {held:?}",
            kind.as_str(),
            want.mode,
            want.insn_count
        );
    }
    println!(
        "[{label}] atlas {} functions; recipe rows: {exact} exact, {shape} by shape, {} unmatched {:?}",
        atlas.functions.len(),
        report.unmatched.len(),
        report.unmatched
    );
    for m in report.matched.iter().filter(|m| m.candidates > 1) {
        println!(
            "[{label}]   {} had {} candidates of that shape",
            m.name, m.candidates
        );
    }

    // Hooks: the recipe's matched rows, with a read-only counter on `nmi`.
    let counters: Rc<RefCell<Vec<Rc<Cell<u64>>>>> = Rc::default();
    let hosts: Rc<RefCell<Vec<HookHost>>> = Rc::default();
    let build = {
        let (counters, hosts, report) = (counters.clone(), hosts.clone(), report.clone());
        move || {
            let h = HookHost::new(manifest(), Budget::default());
            let c = Rc::new(Cell::new(0u64));
            let cc = c.clone();
            h.on_enter("nmi", move |_ev, _m| cc.set(cc.get() + 1))
                .expect("function_hooks granted");
            h.resolve(&|n| report.function_of(n));
            counters.borrow_mut().push(c);
            hosts.borrow_mut().push(h.clone());
            h
        }
    };
    let ids = build().function_ids();
    counters.borrow_mut().clear();
    hosts.borrow_mut().clear();
    let bundles = std::env::var("RF_W36_10_BUNDLES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("rf-w36-10-bundles"))
        .join(label.replace(' ', "_"));
    let opts = Options::<SnesCore> {
        function_hooks: ids.clone(),
        hook_sink: Some(Box::new(move || Box::new(build()) as Box<dyn CoreSink>)),
        bundle_dir: Some(bundles.clone()),
        ..Options::default()
    };
    let t = Instant::now();
    let r = rf_harness::lockstep_snes::run(raw, &atlas, inputs(&d.pad), opts).expect("runs");
    let s = &r.stats;
    row.fallback = format!(
        "{:.3}% ({} of {}; no node {}, width {}, wram {}, other {})",
        s.fallback_rate() * 100.0,
        s.fallback,
        s.instructions(),
        s.fallback_no_node,
        s.fallback_width,
        s.fallback_ram,
        s.fallback_other
    );
    row.lockstep = match &r.divergence {
        None => format!("zero divergence over {} frames", r.frames_run),
        Some(dv) => format!(
            "DIVERGED at frame {}: {:?}; bundle {:?}",
            dv.frame,
            dv.fields.iter().map(|f| f.field).collect::<Vec<_>>(),
            r.bundle
        ),
    };
    println!(
        "[{label}] lockstep {:?}: {}; fallback {}",
        t.elapsed(),
        row.lockstep,
        row.fallback
    );

    // The nmi hook against an independent count.
    row.nmi = match report.function_of("nmi") {
        None => {
            let events = hosts.borrow().first().map_or(0, |h| h.event_counts().0);
            assert_eq!(events, 0, "an unmatched row fires nothing");
            "row unmatched: hook inactive, 0 events".to_string()
        }
        Some(fid) => {
            let f = &atlas.functions[fid as usize];
            let hooked = counters.borrow().first().map_or(0, |c| c.get());
            let entry24 = f.entry_cpu & 0xFF_FFFF;
            let plain = count_arrivals(raw, &d.pad, entry24, f.entry_mode);
            row.nmi_ok = hooked == plain.dispatch;
            format!(
                "hook {hooked} vs plain dispatches into the entry {} ({plain:?}; function {entry24:#08x} mode {}) over {} frames",
                plain.dispatch, f.entry_mode, d.pad.len()
            )
        }
    };
    println!("[{label}] nmi: {}", row.nmi);
    row
}

/// `recipe` as the format read before entry kinds: the twin rule falls back to
/// address hints alone.
fn without_entry_kinds(recipe: &rf_rebuild::Recipe) -> rf_rebuild::Recipe {
    let mut r = recipe.clone();
    for f in &mut r.functions {
        f.entry = None;
    }
    r
}

fn print_table(rows: &[Row]) {
    println!("\n| hack | patch | ROM | gameplay | atlas fns | exact | shape | none | lockstep | fallback | nmi hook |");
    for r in rows {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.label,
            r.patch_bytes.map_or("-".into(), |b| b.to_string()),
            r.rom_bytes,
            r.reached,
            r.atlas_fns,
            r.exact,
            r.shape,
            r.unmatched.len(),
            r.lockstep,
            r.fallback,
            r.nmi
        );
    }
    for r in rows {
        println!(
            "{}: matched {:?}; unmatched {:?}; unmatched before entry kinds {:?}; reasons {:?}",
            r.label, r.matched_names, r.unmatched, r.unmatched_before, r.reasons
        );
    }
}

#[test]
#[ignore = "needs Super Mario World (USA) and the downloaded hacks (RF_SCREENS_ROMS_DIR / RF_HACKS_DIR); many minutes in release"]
fn the_smw_recipe_on_five_hacks_matches_runs_in_lockstep_and_fires_its_hooks() {
    let Some(path) = smw_path() else {
        println!("SKIP: {SMW_FILE} not found");
        return;
    };
    let base = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let recipe = rf_profiles::load_recipe_file(&recipe_path()).expect("the SMW recipe");
    let only = std::env::var("RF_W36_10_ONLY").ok();
    let wanted = |label: &str| only.as_deref().is_none_or(|o| label.contains(o));

    let mut rows: Vec<Row> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    if wanted("control") {
        rows.push(analyse("SMW (unpatched control)", None, &base, &recipe));
    }
    for &(label, rel) in HACKS.iter().filter(|(l, _)| wanted(l)) {
        let patch_path = hacks_dir().join(rel);
        let Ok(patch) = std::fs::read(&patch_path) else {
            println!("SKIP: {label}: {} not found", patch_path.display());
            continue;
        };
        let patched = match rf_enhance::patch::bps::apply_bps(&base, &patch) {
            Ok(p) => p,
            Err(e) => {
                println!("SKIP: {label}: the patch does not apply to this base ROM: {e}");
                failures.push(format!("{label}: {e}"));
                continue;
            }
        };
        rows.push(analyse(label, Some(patch.len()), &patched, &recipe));
    }
    print_table(&rows);
    for r in &rows {
        assert!(r.nmi_ok, "{}: {}", r.label, r.nmi);
        assert!(
            r.lockstep.starts_with("zero divergence"),
            "{}: {}",
            r.label,
            r.lockstep
        );
    }
    assert!(
        failures.is_empty(),
        "patches that did not apply: {failures:?}"
    );
}
