//! **W38-03: the collision-table finder on Super Mario World** (ROM-gated,
//! law 5).
//!
//! Yoshi's Island 1 by the W36-08 menu-and-play script. The player's y/x
//! words (`$7E0096` / `$7E0094`) are the recipe's `player_y` / `player_x`
//! rows; frames are sampled only while the game mode (`$7E0100`) says a level
//! is running. Only offsets, ids and counts are printed (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_03_collision_smw -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::collision_tables::{painted_heights, run_snes, Watch};
use rf_harness::data_regions::SnesDriver;
use rf_input::SnesButton;

mod smw_drive;
use rf_rebuild::collision::{buffer_span, infer, to_rows};
use rf_rebuild::{Grade, TileClass};
use rf_snes::core::SnesCore;

const FILE: &str = "Super Mario World (USA).zip";
const MODE: u32 = 0x7E_0100;
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

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR)"]
fn smw_collision_routine_and_solid_ids_are_found_without_hints() {
    let Some(raw) = rom_path().and_then(|p| load_rom(&p)) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let atlas = build_snes_atlas(&raw, &baseline(&raw)).expect("atlas");
    let mut d = SnesDriver::new(&raw, &atlas).expect("loads");
    let pad = session(&raw);
    let ins: Vec<InputFrame> = pad.iter().map(|&m| input(m)).collect();
    let right = 1u16 << SnesButton::Right.bit();
    let watch = Watch {
        y: vec![0x7E_0096, 0x7E_0097],
        x: vec![0x7E_0094, 0x7E_0095],
    };
    let t = std::time::Instant::now();
    let facts = run_snes(
        &mut d,
        &ins,
        0,
        &watch,
        |f| pad[f] & right != 0,
        |d| d.core.peek(MODE) == IN_LEVEL,
    )
    .expect("runs");
    let sampled = facts.frames.iter().filter(|f| f.sampled).count();
    println!(
        "{} frames observed ({sampled} in a level) in {:?}; {} load sites",
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
            println!("   id {:#06x} {:?} {:?}", v.id, v.class, v.tally);
        }
        for row in to_rows(
            s,
            None,
            None,
            "the RAM Map16 buffer ids",
            "Yoshi's Island 1 play",
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
    let profile = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/super-mario-world/profile.toml"),
    )
    .expect("profile");
    match painted_heights(&profile) {
        Some(h) => println!("painted table: {} tiles", h.len()),
        None => println!("no painted/learned height table for this ROM; agreement not computable"),
    }
}
