//! **W38-01: the data-region finder on Super Mario Bros.** (ROM-gated, law 5).
//!
//! The finder watches every ROM *data* read of the running game (no hints, no
//! disassembly) and proposes pointer tables, element tables, level streams and
//! graphics feeds. The level-switch experiment then forces each of the 32
//! levels the way the W36-04 lockstep test does (test-side RAM write of the
//! observed world/level bytes into the title screen's Start frames) and
//! checks that the found pointer table's entry follows the level and that the
//! stream read for each level is a distinct region.
//!
//! Only offsets, sizes, strides and counts are printed (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_01_data_regions_smb -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::data_regions::NesDriver;
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_rebuild::dataregion::{associate, grade_rows, infer, DataFacts, RegionKind};
use rf_rebuild::Atlas;

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const RIGHT: u8 = 1 << 7;
/// 500 frames to get through both title screens plus the intro, then play.
const LEAD_IN: u32 = 500;
const PLAY: u32 = 1200;
const FRAMES: u32 = LEAD_IN + PLAY;
/// Frames on which the harness writes the level into RAM (module doc).
const FORCE_FROM: u32 = 380;
const FORCE_TO: u32 = 404;

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("nes").join(FILE);
    p.exists().then_some(p)
}

/// The single NES image inside the zip (content-sniffed, like the app).
fn load_rom(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("reads the zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("entry");
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("reads");
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Nes { .. })
        ) {
            return buf;
        }
    }
    panic!("no NES image in the zip");
}

/// Two Starts (game select, then SMB's title), then Right with A pressed 12
/// frames in every 60. Same as `w36_03_rebuild_smb`'s script one.
fn script_one(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => RIGHT | A,
        _ => RIGHT,
    }
}

fn inputs(script: fn(u32) -> u8) -> Vec<InputFrame> {
    (0..FRAMES)
        .map(|f| {
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(script(f));
            i
        })
        .collect()
}

/// Test-harness level entry (module doc): world and level, zero-indexed.
fn force_level(frame: u32, core: &mut NesCore, level: Option<(u8, u8)>) {
    if let Some((w, l)) = level {
        if (FORCE_FROM..=FORCE_TO).contains(&frame) {
            core.bus_mut().write(0x075F, w);
            core.bus_mut().write(0x0760, l);
        }
    }
}

struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// Record a run of `script` (entering `level`) and return its coverage.
fn record(raw: &[u8], script: fn(u32) -> u8, level: Option<(u8, u8)>) -> CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for (f, input) in (0u32..).zip(inputs(script)) {
        force_level(f, &mut core, level);
        core.run_frame(&input, &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

/// Observe `script` for frames `from..to` of a run that forces `level`.
fn observe(
    raw: &[u8],
    atlas: &Atlas,
    script: fn(u32) -> u8,
    level: Option<(u8, u8)>,
    from: u32,
    to: u32,
) -> DataFacts {
    let mut d = NesDriver::new(raw, atlas).expect("loads");
    for (f, input) in (0u32..).zip(inputs(script)).take(to as usize) {
        force_level(f, &mut d.core, level);
        d.frame(&input, f >= from).expect("frame");
    }
    d.take_facts()
}

/// Frames observed per level: from just before the forced Start to well into
/// play, so the level load and a screen or two of scrolling are inside.
const OBSERVE_FROM: u32 = 380;
const OBSERVE_TO: u32 = FRAMES;

fn all_levels() -> Vec<(u8, u8)> {
    (0..8).flat_map(|w| (0..4).map(move |l| (w, l))).collect()
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR); about two minutes in release"]
fn smb_level_pointer_table_and_area_streams_are_found_without_hints() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    // The atlas only attributes steps (interrupts, functions); union the
    // coverage of a few levels so level code is in it.
    let mut union: Option<CoverageLog> = None;
    for lv in [(0, 0), (1, 1), (3, 3), (5, 2), (7, 3)] {
        let log = record(&raw, script_one, Some(lv));
        match union.as_mut() {
            Some(u) => {
                u.merge(&log);
            }
            None => union = Some(log),
        }
    }
    let atlas = build_nes_atlas(&raw, &union.expect("recorded")).expect("atlas");

    let t = Instant::now();
    let levels = all_levels();
    let mut runs: Vec<(u32, DataFacts)> = Vec::new();
    for &(w, l) in &levels {
        let f = observe(
            &raw,
            &atlas,
            script_one,
            Some((w, l)),
            OBSERVE_FROM,
            OBSERVE_TO,
        );
        assert_eq!(f.stats.unknown, 0, "every ROM data read classified");
        assert_eq!(f.stats.suspect_index, 0, "8-bit indices on the NES");
        runs.push((u32::from(w) * 4 + u32::from(l), f));
    }
    let mut merged = DataFacts::default();
    for (_, f) in &runs {
        merged.merge(f);
    }
    println!(
        "32 levels observed in {:?}: {} data reads, {} sites, {} pointer uses",
        t.elapsed(),
        merged.stats.data_reads,
        merged.sites.len(),
        merged.stats.links
    );

    let refs: Vec<(u32, &DataFacts)> = runs.iter().map(|(k, f)| (*k, f)).collect();
    let links = associate(&refs);
    let tracking: Vec<_> = links.iter().filter(|l| l.tracks_level()).collect();
    println!(
        "{} pointer feeds seen in 2+ levels, {} follow the level",
        links.len(),
        tracking.len()
    );
    for l in &links {
        println!(
            "feed lo {:#x} hi {:#x}: {} runs, {} own entry, {} entries, {} targets, {} streams found, tracks {}",
            l.link.lo, l.link.hi, l.runs, l.own_entry, l.distinct_entries, l.distinct_targets,
            l.stream_found, l.tracks_level()
        );
    }
    assert!(!tracking.is_empty(), "some pointer table follows the level");
    for l in &tracking {
        println!("level  entry-index  target       stream(start,size)");
        for r in &l.per_run {
            println!(
                "{}-{}    {:<11}  {:<11}  {:?}",
                r.key / 4 + 1,
                r.key % 4 + 1,
                r.index.map_or(String::new(), |i| i.to_string()),
                r.target.map_or(String::new(), |t| format!("{t:#x}")),
                r.stream
            );
        }
        // Different entry => different target and stream; same entry => same.
        for (i, a) in l.per_run.iter().enumerate() {
            for b in l.per_run.iter().skip(i + 1) {
                let (Some(ea), Some(eb)) = (a.entry, b.entry) else {
                    continue;
                };
                if ea == eb {
                    assert_eq!(a.target, b.target, "same entry, same target");
                } else {
                    assert_ne!(
                        a.target, b.target,
                        "levels {}/{}: different entry, same target",
                        a.key, b.key
                    );
                    if let (Some(sa), Some(sb)) = (a.stream, b.stream) {
                        assert_ne!(sa.0, sb.0, "levels {}/{}: one stream region", a.key, b.key);
                    }
                }
            }
        }
        let shared = l.runs - l.distinct_entries.min(l.runs);
        println!("{} level(s) share an entry with another level", shared);
    }

    let regions = infer(&merged);
    let rows = grade_rows(
        &regions,
        &links,
        "forced world/level RAM $075F/$0760 over 32 levels",
    );
    for (r, row) in regions.iter().zip(&rows) {
        if matches!(r.kind, RegionKind::PointerTable) || row.grade == rf_rebuild::Grade::Confirmed {
            println!(
                "{} {:?} {:?} rom {:#x} size {} stride {} entries {} conf {} grade {}",
                r.name,
                r.kind,
                r.part,
                r.start,
                r.size,
                r.stride,
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
    println!("{} regions, {} confirmed", rows.len(), confirmed);
    let mut by_kind = std::collections::BTreeMap::new();
    for (r, row) in regions.iter().zip(&rows) {
        let e = by_kind
            .entry((r.kind, row.grade.as_str()))
            .or_insert((0u32, 0u32));
        e.0 += 1;
        e.1 += u32::from(r.confidence >= 60);
    }
    println!("by kind and grade (count, conf>=60): {by_kind:?}");
    assert!(confirmed >= 2, "a pointer table and a stream confirmed");
    if std::env::var("RF_W38_WRITE").is_ok() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/nes/super-mario-bros/recipe.toml");
        let text = std::fs::read_to_string(&path).expect("recipe");
        // Keep the rows worth a recipe: confirmed ones and the finder's
        // better candidates (the rest are in the research note's counts).
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
