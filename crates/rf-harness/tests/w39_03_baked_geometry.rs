//! **W39-03: baked level geometry for SMW and A Link to the Past**
//! (ROM-gated, law 5: only offsets, ids and counts are printed or stored).
//!
//! * SMW: the W38-03 finder's collision facts (rows over the RAM Map16
//!   buffer ids it found) become the solidity of a screen read from that
//!   buffer; the stage set is baked, cached under the ROM's sha256 in an
//!   `rf-cache` root and reloaded.
//! * Zelda: there is no collision recipe, so painted heights are the
//!   solidity source. The repo carries no `[heights]` for it (checked), so
//!   the test bakes what it has and says so.
//!
//! ```text
//! cargo test --release -p rf-harness --test w39_03_baked_geometry -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;

use rf_cache::Cache;
use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_enhance::baked::{
    bake_screen, load_stage_set, save_stage_set, Kind, RuleClass, ScreenInput, Solidity, StageSet,
    TileRule,
};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::collision_tables::{painted_heights, run_snes, Watch};
use rf_harness::data_regions::SnesDriver;
use rf_input::SnesButton;

mod smw_drive;
use rf_rebuild::collision::{infer, to_rows};
use rf_rebuild::{Grade, TileClass};
use rf_snes::core::SnesCore;

const FILE: &str = "Super Mario World (USA).zip";
const ZELDA: &str = "Legend of Zelda, The - A Link to the Past (USA).zip";
const MODE: u32 = 0x7E_0100;
const IN_LEVEL: u8 = 0x14;

/// Menus to Yoshi's Island 1 and play: the token scripts of
/// `crates/retroforge/tests/w33_07_common/mod.rs`, as `w36_08_atlas_smw`
/// plays them. `wN` idles N frames, `<buttons>N` holds them
/// (`A B X Y S E U D L R`).
const PLAY: &str = "L2 w3 B8 w52 B8 w52 B8 w52 B8 w52 R+B14 R22 R+B14 R22 S6 w100 S6 w60 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22";

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

fn rom_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("RF_SCREENS_ROMS_DIR")
            .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string()),
    )
}

fn temp_cache(label: &str) -> (Cache, PathBuf) {
    let root = std::env::temp_dir().join(format!("rf-w39-03-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    (Cache::open(&root, 256 << 20).expect("cache"), root)
}

fn sha256(raw: &[u8]) -> String {
    rf_cart::hash::hash_all(raw).sha256
}

/// SMW's Map16 screen is 16 blocks wide (the buffer is stacked screens).
const SCREEN_COLS: u32 = 16;
/// Most buffer bytes read back as one stage screen.
const MAX_BUFFER: u32 = 64 * 1024;

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR)"]
fn smw_stage_set_bakes_caches_and_reloads() {
    let path = rom_dir().join("snes").join(FILE);
    let Some(raw) = load_rom(&path) else {
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
    let facts = run_snes(
        &mut d,
        &ins,
        0,
        &watch,
        |f| pad[f] & right != 0,
        |d| d.core.peek(MODE) == IN_LEVEL,
    )
    .expect("runs");
    let sites = infer(&facts);
    let site = sites
        .iter()
        .find(|s| !s.ids(TileClass::Solid).is_empty())
        .expect("a site with solid ids");
    let rows = to_rows(
        site,
        None,
        None,
        "the RAM Map16 buffer ids",
        "W39-03",
        Grade::Candidate,
    );
    let rules: Vec<TileRule> = rows
        .iter()
        .map(|r| {
            let class = match r.class {
                TileClass::Solid => RuleClass::Solid,
                TileClass::OneWay => RuleClass::OneWay,
                TileClass::Water | TileClass::Hazard => RuleClass::Open,
            };
            TileRule {
                first: r.first,
                last: r.last,
                class,
            }
        })
        .collect();
    // The screen: the bytes of the buffer the finder found, read as they
    // stand at the end of play; unread addresses are unobserved.
    let lo = *site.addrs.first().expect("addrs");
    let hi = (*site.addrs.last().expect("addrs")).min(lo + MAX_BUFFER - 1);
    let n = (hi - lo + 1).div_ceil(SCREEN_COLS) * SCREEN_COLS;
    let ids: Vec<Option<u32>> = (0..n)
        .map(|i| {
            let a = lo + i;
            site.addrs.contains(&a).then(|| u32::from(d.core.peek(a)))
        })
        .collect();
    let input = ScreenInput {
        id: 1,
        origin: (0, 0),
        cols: SCREEN_COLS,
        rows: n / SCREEN_COLS,
        cell_px: 16,
        ids: &ids,
        visited: None,
    };
    let screen = bake_screen(
        &input,
        &Solidity {
            rules: &rules,
            painted: None,
        },
    )
    .expect("bakes");
    let observed = ids.iter().flatten().count();
    let bodies = screen.body_count();
    println!(
        "SMW buffer {lo:#x}..={hi:#x}: {observed} ids observed of {n} cells, {} rule ranges, {bodies} solid/one-way cells, {} quads",
        rules.len(),
        screen.quads.len()
    );
    assert!(observed > 0 && bodies > 0, "solid cells baked");
    assert!(!screen.quads.is_empty(), "walls extruded");
    assert!(screen.cells.iter().any(|c| c.kind == Kind::Unknown) || observed as u32 == n);
    let set = StageSet {
        screens: vec![screen],
    };
    let (mut cache, root) = temp_cache("smw");
    let sha = sha256(&raw);
    let stored = save_stage_set(&mut cache, &sha, &set).expect("saves");
    println!(
        "SMW stage set: {} cells, {stored} bytes stored",
        set.cell_count()
    );
    drop(cache);
    let mut again = Cache::open(&root, 256 << 20).expect("reopen");
    assert_eq!(load_stage_set(&mut again, &sha).expect("loads"), Some(set));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
#[ignore = "needs A Link to the Past (USA) (RF_SCREENS_ROMS_DIR)"]
fn zelda_stage_set_uses_painted_heights_and_caches() {
    let path = rom_dir().join("snes").join(ZELDA);
    let Some(raw) = load_rom(&path) else {
        println!("SKIP: {ZELDA} not found");
        return;
    };
    let profile = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/the-legend-of-zelda-a-link-to-the-past/profile.toml"),
    )
    .expect("profile");
    let painted = painted_heights(&profile);
    println!(
        "Zelda: no collision recipe; painted [heights] tiles: {}",
        painted.as_ref().map_or(0, |p| p.len())
    );
    // The scout supplies the screen footprint and the tilemap character
    // numbers; without a painted table every cell is unknown and the set is
    // empty rather than invented.
    let mut set = StageSet::default();
    if let Some(p) = painted.as_ref() {
        let ids: Vec<Option<u32>> = p.keys().map(|&k| Some(k)).collect();
        let cols = ids.len() as u32;
        let screen = bake_screen(
            &ScreenInput {
                id: 1,
                origin: (0, 0),
                cols,
                rows: 1,
                cell_px: 8,
                ids: &ids,
                visited: None,
            },
            &Solidity {
                rules: &[],
                painted: Some(p),
            },
        )
        .expect("bakes");
        set.screens.push(screen);
    }
    let (mut cache, root) = temp_cache("zelda");
    let sha = sha256(&raw);
    save_stage_set(&mut cache, &sha, &set).expect("saves");
    drop(cache);
    let mut again = Cache::open(&root, 256 << 20).expect("reopen");
    assert_eq!(load_stage_set(&mut again, &sha).expect("loads"), Some(set));
    let _ = std::fs::remove_dir_all(root);
}
