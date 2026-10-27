//! **W38-03: the collision-table finder on Super Mario Bros.** (ROM-gated,
//! law 5).
//!
//! World 1-1 by the W38-01 script (two Starts, then Right with A pressed 12
//! frames in every 60), observed from the first frame of play. The finder
//! watches RAM loads that feed compares or table reads, labels frames by the
//! player's motion (y at `$00CE`, level x at `$006D:$0086`, both named by the
//! recipe/RAM map) and classifies the tile ids a loading site reads by what
//! happened to the player. Only offsets, ids and counts are printed (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_03_collision_smb -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::collision_tables::{painted_heights, run_nes, Watch};
use rf_harness::data_regions::NesDriver;
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_rebuild::collision::{buffer_span, infer, to_rows};
use rf_rebuild::{Grade, TileClass};

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const RIGHT: u8 = 1 << 7;
const LEAD_IN: u32 = 500;
const FRAMES: u32 = LEAD_IN + 1200;
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

fn record(raw: &[u8], level: Option<(u8, u8)>) -> CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for (f, input) in (0u32..).zip(inputs(script_one)) {
        force_level(f, &mut core, level);
        core.run_frame(&input, &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR); about a minute in release"]
fn smb_collision_routine_and_solid_ids_are_found_without_hints() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let atlas = build_nes_atlas(&raw, &record(&raw, None)).expect("atlas");
    let watch = Watch {
        y: vec![0x00CE],
        x: vec![0x0086, 0x006D],
    };
    let ins = inputs(script_one);
    let t = std::time::Instant::now();
    // 1-1 (ground, bricks, pipes) and 2-2 (underwater), forced the way the
    // W38-01 experiment forces levels.
    let mut facts = rf_rebuild::collision::CollisionFacts::default();
    for level in [(0u8, 0u8), (1, 1)] {
        let mut d = NesDriver::new(&raw, &atlas).expect("loads");
        let f = run_nes(
            &mut d,
            &ins,
            LEAD_IN as usize,
            &watch,
            |f| script_one(f as u32) & RIGHT != 0,
            |f, d| force_level(f as u32, &mut d.core, Some(level)),
        )
        .expect("runs");
        println!(
            "level {:?}: {} frames, {} sites",
            level,
            f.frames.len(),
            f.sites.len()
        );
        facts.merge(&f);
    }
    println!(
        "{} frames observed in {:?}; {} load sites",
        facts.frames.len(),
        t.elapsed(),
        facts.sites.len()
    );
    for (site, f) in facts.sites.iter().filter(|(_, f)| f.addrs.len() >= 12) {
        println!(
            "WALKER rom {site:#x} fn {:?} consumers {:?}: {} addrs {:#x}..{:#x}",
            f.function,
            f.consumers,
            f.addrs.len(),
            f.addrs.keys().next().unwrap(),
            f.addrs.keys().next_back().unwrap()
        );
    }
    let sites = infer(&facts);
    println!("{} collision-site candidates", sites.len());
    for s in sites.iter().take(6) {
        println!(
            "site rom {:#x} fn {:?} consumers {:?} active {} frames, span {:?}",
            s.site,
            s.function,
            s.consumers,
            s.active_frames,
            buffer_span(s)
        );
        for v in &s.verdicts {
            println!("   id {:#04x} {:?} {:?}", v.id, v.class, v.tally);
        }
        for row in to_rows(
            s,
            None,
            None,
            "the RAM level buffer ids",
            "1-1 play",
            Grade::Candidate,
        ) {
            println!("   row {:?} {:#x}..={:#x}", row.class, row.first, row.last);
        }
    }
    assert!(!sites.is_empty(), "a collision site is found");
    assert!(
        sites.iter().any(|s| !s.ids(TileClass::Solid).is_empty()),
        "some ids classified Solid"
    );
    // A per-screen solidity map of the buffer the routine walked: its RAM span
    // read mid-1-1, 16 cells to a row, through the rows just found.
    let site = &sites[0];
    let (lo, hi, _) = buffer_span(site).expect("span");
    let mut d = NesDriver::new(&raw, &atlas).expect("loads");
    for (f, input) in ins.iter().enumerate().take(LEAD_IN as usize + 600) {
        force_level(f as u32, &mut d.core, Some((0, 0)));
        d.frame(input, false).expect("frame");
    }
    let rows = to_rows(
        site,
        None,
        None,
        "RAM level buffer ids",
        "1-1",
        Grade::Candidate,
    );
    let ids: Vec<Option<u32>> = (lo..=hi)
        .map(|a| Some(u32::from(d.core.bus().peek(a as u16))))
        .collect();
    let grid = rf_rebuild::collision::found_grid(&ids, 16, &rows);
    let solid = grid.cells.iter().filter(|c| **c == Some(true)).count();
    println!(
        "buffer {lo:#x}..={hi:#x}: {} rows of 16, {solid} solid cells of {}",
        grid.h,
        grid.cells.len()
    );
    assert!(solid > 0, "the found rows mark some buffer cells solid");
    // The painted side: none ships for this ROM (heights are painted for SNES
    // tilemaps only), so the agreement is not computable.
    let profile = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/nes/super-mario-bros/profile.toml"),
    )
    .expect("profile");
    match painted_heights(&profile) {
        Some(h) => println!("painted table: {} tiles", h.len()),
        None => println!("no painted/learned height table for this ROM; agreement not computable"),
    }
}
