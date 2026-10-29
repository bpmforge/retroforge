//! **W38-01: the data-region finder on Super Mario World** (ROM-gated, law 5).
//!
//! Yoshi's Island 1 by the W36-08 menu-and-play script, observed from the
//! first frame, then more levels entered by test-side RAM writes on the
//! overworld (the SMB experiment's SNES counterpart; see the research note
//! `docs/research/W38-01-data-regions.md` for what was tried). Only offsets,
//! sizes, strides and counts are printed (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_01_data_regions_smw -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::data_regions::SnesDriver;
use rf_harness::movie::{self, Movie};
use rf_harness::movie_replay::{plan, Plan};
use rf_input::SnesButton;

mod smw_drive;
use rf_rebuild::dataregion::{associate, grade_rows, infer, DataFacts};
use rf_snes::core::SnesCore;

const FILE: &str = "Super Mario World (USA).zip";
const MODE: u32 = 0x7E_0100;
const TRANSLEVEL: u32 = 0x7E_13BF;
const IN_LEVEL: u8 = 0x14;

/// Menus to Yoshi's Island 1 and play: the token scripts of
/// `crates/retroforge/tests/w33_07_common/mod.rs`, as `w36_08_atlas_smw`
/// plays them. `wN` idles N frames, `<buttons>N` holds them
/// (`A B X Y S E U D L R`).
const PLAY: &str = "L2 w3 B8 w52 B8 w52 B8 w52 B8 w52 R+B14 R22 R+B14 R22 S6 w100 S6 w60 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22";

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("snes").join(FILE);
    p.exists().then_some(p)
}

fn load_rom(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).ok()?;
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Snes { .. })
        ) {
            return Some(buf);
        }
    }
    None
}

fn frames_of(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0u16, n.parse::<usize>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
            let (b, n) = tok.split_at(split);
            let mask = b.split('+').fold(0u16, |m, c| {
                let bit = match c {
                    "A" => SnesButton::A,
                    "B" => SnesButton::B,
                    "X" => SnesButton::X,
                    "Y" => SnesButton::Y,
                    "S" => SnesButton::Start,
                    "E" => SnesButton::Select,
                    "U" => SnesButton::Up,
                    "D" => SnesButton::Down,
                    "L" => SnesButton::Left,
                    "R" => SnesButton::Right,
                    other => panic!("button {other}"),
                }
                .bit();
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

/// Menus to the level, then play until the level has seen 1,500 frames.
fn session(raw: &[u8]) -> Vec<u16> {
    let mut pad = smw_drive::level_pad(raw);
    let start = pad.len();
    pad.extend(frames_of(PLAY));
    let more = frames_of("R+B14 R22");
    while pad.len() < start + 1500 {
        pad.extend(&more);
    }
    pad
}

fn input(mask: u16) -> InputFrame {
    let mut f = InputFrame::empty();
    f.ports[0] = mask;
    f
}

struct Null;
impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn baseline(raw: &[u8]) -> CoverageLog {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for mask in session(raw) {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&InputFrame::empty(), &mut Null);
    }
    core.take_coverage().expect("armed")
}

/// Observe `pad` and cut one [`DataFacts`] per level: from the frame the game
/// leaves a level (or the start) to the frame it leaves the next one, keyed by
/// the translevel number when the level started.
fn observe_levels(d: &mut SnesDriver, pad: &[u16], from: usize) -> Vec<(u32, DataFacts)> {
    let mut runs = Vec::new();
    let mut key: Option<u32> = None;
    for (f, &mask) in pad.iter().enumerate() {
        d.frame(&input(mask), f >= from).expect("frame");
        let mode = d.core.peek(MODE);
        if mode == IN_LEVEL && key.is_none() {
            key = Some(u32::from(d.core.peek(TRANSLEVEL)));
        } else if mode != IN_LEVEL {
            if let Some(k) = key.take() {
                runs.push((k, d.take_facts()));
            }
        }
    }
    runs
}

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR)"]
fn smw_probe_yoshis_island_1() {
    let Some(raw) = rom_path().and_then(|p| load_rom(&p)) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let atlas = build_snes_atlas(&raw, &baseline(&raw)).expect("atlas");
    let mut d = SnesDriver::new(&raw, &atlas).expect("loads");
    let pad = session(&raw);
    let t = Instant::now();
    let runs = observe_levels(&mut d, &pad, 0);
    println!(
        "{} frames observed in {:?}; {} level runs",
        pad.len(),
        t.elapsed(),
        runs.len()
    );
    let mut merged = DataFacts::default();
    for (k, f) in &runs {
        println!(
            "RUN translevel {k:#x}: {} sites, {} links, stats {:?}",
            f.sites.len(),
            f.links.len(),
            f.stats
        );
        merged.merge(f);
    }
    let refs: Vec<(u32, &DataFacts)> = runs.iter().map(|(k, f)| (*k, f)).collect();
    for l in associate(&refs) {
        println!(
            "FEED lo {:#x} hi {:#x} bank {:?}: {} runs, own {}, entries {}, streams {}, tracks {}",
            l.link.lo,
            l.link.hi,
            l.link.bank,
            l.runs,
            l.own_entry,
            l.distinct_entries,
            l.stream_found,
            l.tracks_level()
        );
        for r in &l.per_run {
            println!(
                "  run {:#x}: entry idx {:?} target {:?} stream {:?}",
                r.key,
                r.index,
                r.target.map(|t| format!("{t:#x}")),
                r.stream
            );
        }
    }
    for (k, l) in &merged.links {
        let site = &merged.sites[&k.lo];
        println!(
            "LINK lo {:#x} hi {:#x} bank {:?}: {} uses, {} entries, deltas {:?}, index_source {:?}, index set {:?}",
            k.lo, k.hi, k.bank, l.count, l.entries.len(), l.deltas, site.index_source, l.entries.values().map(|e| e.index).collect::<Vec<_>>()
        );
    }
    let _ = grade_rows(&[], &[], "");
    for r in infer(&merged) {
        if r.kind == rf_rebuild::dataregion::RegionKind::PointerTable {
            println!("{} {:?} {:?} start {:#x} size {} stride {} entries {} conf {} idx_ram {:?} ptr_ram {:?}", r.name, r.kind, r.part, r.start, r.size, r.stride, r.entries, r.confidence, r.index_ram, r.pointer_ram);
        }
    }
}

fn tas_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("RF_SCREENS_ROMS_DIR")
            .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string()),
    )
    .join("tas")
}

fn movie_file(var: &str, default: &str) -> Option<PathBuf> {
    let p = std::env::var(var).map_or_else(|_| tas_dir().join(default), PathBuf::from);
    p.exists().then_some(p)
}

/// Run a movie plan on a fresh driver: resets are applied as the movie says
/// and `observe_levels`' cut rule is applied after every frame. `from..cap`
/// frames; `observe` false runs plain (the alignment sweep).
fn run_plan(
    d: &mut SnesDriver,
    pl: &Plan,
    cap: usize,
    observe: bool,
    runs: &mut Vec<(u32, DataFacts)>,
) -> usize {
    let mut key: Option<u32> = None;
    let mut entered = std::collections::BTreeSet::new();
    for f in pl.frames.iter().take(cap) {
        if let Some(r) = f.reset {
            d.core.reset(match r {
                movie::Reset::Soft => ResetKind::Soft,
                movie::Reset::Hard => ResetKind::Hard,
            });
        }
        let mut i = InputFrame::empty();
        i.ports[0] = f.pads[0];
        i.ports[1] = f.pads[1];
        d.frame(&i, observe).expect("frame");
        let mode = d.core.peek(MODE);
        if mode == IN_LEVEL {
            entered.insert(d.core.peek(TRANSLEVEL));
            if key.is_none() {
                key = Some(u32::from(d.core.peek(TRANSLEVEL)));
            }
        } else if let Some(k) = key.take() {
            if observe {
                runs.push((k, d.take_facts()));
            }
        }
    }
    entered.len()
}

/// `runs` with the facts of repeated visits to one translevel merged.
fn by_level(runs: Vec<(u32, DataFacts)>) -> Vec<(u32, DataFacts)> {
    let mut out: Vec<(u32, DataFacts)> = Vec::new();
    for (k, f) in runs {
        match out.iter_mut().find(|(ok, _)| *ok == k) {
            Some((_, of)) => of.merge(&f),
            None => out.push((k, f)),
        }
    }
    out
}

#[test]
#[ignore = "needs Super Mario World (USA) and the SMW movies (RF_SCREENS_ROMS_DIR/tas); about ten minutes in release"]
fn smw_level_pointer_tables_are_found_without_hints() {
    let Some(raw) = rom_path().and_then(|p| load_rom(&p)) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let atlas = build_snes_atlas(&raw, &baseline(&raw)).expect("atlas");
    let t = Instant::now();
    let mut runs: Vec<(u32, DataFacts)> = Vec::new();

    // 1. The W36-08 menus-and-play script.
    let mut d = SnesDriver::new(&raw, &atlas).expect("loads");
    runs.extend(observe_levels(&mut d, &session(&raw), 0));
    println!("script: {} level runs in {:?}", runs.len(), t.elapsed());

    // 2. The W37-00 movies, as far as they get.
    // (name, movie, idle frames before it, frames to run): the offsets are
    // W37-00's sweep winners (docs/research/W37-00-movie-import.md); both
    // movies stall after three levels, so the caps stop shortly after that.
    let mut sources: Vec<(&str, Movie, usize, usize)> = Vec::new();
    if let Some(p) = movie_file("RF_W37_SMW_LSMV", "4144/x/brunovisnadi-smw-warps.lsmv") {
        let m = movie::lsmv::parse_archive(&std::fs::read(p).expect("reads")).expect("lsmv");
        sources.push(("warps (lsnes)", m, 3, 8_000));
    }
    if let Some(p) = movie_file(
        "RF_W37_SMW_SMV",
        "2307/x/bahamete,kaizoman666,masterjun-supermarioworld-96exit.smv",
    ) {
        let m = movie::smv::parse(&std::fs::read(p).expect("reads")).expect("smv");
        sources.push(("96 exits (Snes9x)", m, 4, 14_000));
    }
    for (name, m, delay, cap) in &sources {
        let before = runs.len();
        let mut d = SnesDriver::new(&raw, &atlas).expect("loads");
        let n = run_plan(&mut d, &plan(&[], *delay, 0, m), *cap, true, &mut runs);
        println!(
            "movie {name}: offset {delay}, {n} translevels entered, {} level runs, {:?}",
            runs.len() - before,
            t.elapsed()
        );
    }

    let runs = by_level(runs);
    let keys: Vec<String> = runs.iter().map(|(k, _)| format!("{k:#x}")).collect();
    println!(
        "{} levels reached (translevel numbers): {}",
        runs.len(),
        keys.join(" ")
    );
    let mut merged = DataFacts::default();
    for (_, f) in &runs {
        merged.merge(f);
    }
    let refs: Vec<(u32, &DataFacts)> = runs.iter().map(|(k, f)| (*k, f)).collect();
    let links = associate(&refs);
    let tracking: Vec<_> = links.iter().filter(|l| l.tracks_level()).collect();
    println!(
        "{} feeds seen in 2+ levels, {} follow the level",
        links.len(),
        tracking.len()
    );
    for l in &tracking {
        println!(
            "feed lo {:#x} hi {:#x} bank {:?}: {} runs, {} entries, {} targets, {} streams",
            l.link.lo,
            l.link.hi,
            l.link.bank,
            l.runs,
            l.distinct_entries,
            l.distinct_targets,
            l.stream_found
        );
        println!("level  entry-index  target       stream(start,size)");
        for r in &l.per_run {
            println!(
                "{:#04x}   {:<11}  {:<11}  {:?}",
                r.key,
                r.index.map_or(String::new(), |i| i.to_string()),
                r.target.map_or(String::new(), |t| format!("{t:#x}")),
                r.stream
            );
        }
        // Same entry, same target (a fact of the table); different entries may
        // alias one target (shared background data), which is reported.
        let mut aliased = 0;
        for (i, a) in l.per_run.iter().enumerate() {
            for b in l.per_run.iter().skip(i + 1) {
                let (Some(ea), Some(eb)) = (a.entry, b.entry) else {
                    continue;
                };
                if ea == eb {
                    assert_eq!(a.target, b.target, "same entry, same target");
                } else if a.target == b.target {
                    aliased += 1;
                }
            }
        }
        println!("{aliased} pair(s) of levels reach one target through different entries");
    }
    assert!(
        tracking
            .iter()
            .any(|l| l.distinct_targets == l.runs && l.stream_found == l.runs),
        "a pointer table whose entry per level leads to a distinct stream per level"
    );

    let regions = infer(&merged);
    let rows = grade_rows(
        &regions,
        &links,
        "levels entered by the W36-08 script and the W37-00 movies",
    );
    for (r, row) in regions.iter().zip(&rows) {
        if r.kind == rf_rebuild::dataregion::RegionKind::PointerTable
            || row.grade == rf_rebuild::Grade::Confirmed
        {
            println!(
                "{} {:?} {:?} rom {:#x} size {} stride {} ptr {} entries {} conf {} grade {}",
                r.name,
                r.kind,
                r.part,
                r.start,
                r.size,
                r.stride,
                r.pointer_bytes,
                r.entries,
                r.confidence,
                row.grade.as_str()
            );
        }
    }
    let confirmed = rows
        .iter()
        .filter(|r| r.grade == rf_rebuild::Grade::Confirmed)
        .count();
    println!("{} regions, {confirmed} confirmed", rows.len());
    let mut by_kind = std::collections::BTreeMap::new();
    for (r, row) in regions.iter().zip(&rows) {
        let e = by_kind
            .entry((r.kind, row.grade.as_str()))
            .or_insert((0u32, 0u32));
        e.0 += 1;
        e.1 += u32::from(r.confidence >= 60);
    }
    println!("by kind and grade (count, conf>=60): {by_kind:?}");
    assert!(
        rows.iter().any(
            |r| r.kind == rf_rebuild::dataregion::RegionKind::PointerTable
                && r.pointer_bytes == Some(3)
                && r.grade == rf_rebuild::Grade::Confirmed
        ),
        "a confirmed pointer table of 3-byte pointers (the level pointer table)"
    );
    if std::env::var("RF_W38_WRITE").is_ok() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/super-mario-world/recipe.toml");
        let text = std::fs::read_to_string(&path).expect("recipe");
        let keep = rf_rebuild::dataregion::select_rows(&rows, 150);
        let block = rf_profiles::recipe::region_rows_block(&keep);
        std::fs::write(
            &path,
            rf_profiles::recipe::splice_region_block(&text, &block),
        )
        .expect("writes");
        rf_profiles::recipe::load_recipe_file(&path).expect("the recipe still loads");
        println!("wrote {} rows to {}", keep.len(), path.display());
    }
}
