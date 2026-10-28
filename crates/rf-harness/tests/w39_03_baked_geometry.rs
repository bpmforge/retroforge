//! **W39-03: baked level geometry for SMW and A Link to the Past**
//! (ROM-gated, law 5: only offsets, ids and counts are printed or stored).
//!
//! * SMW: the W38-03 finder's collision facts (rows over the RAM Map16
//!   buffer ids it found) become the solidity of the level read from that
//!   buffer, over the footprint of the session's scouted map (W39-03b); the stage set is baked, cached under the ROM's sha256 in an
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
    bake_canvas, bake_screen, load_stage_set, save_stage_set, visited_cells, Kind, RuleClass,
    ScreenInput, Solidity, StageSet, TileRule,
};
use rf_enhance::stitcher::Canvas;
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
/// The player's world position words, as the harness `Watch` above reads them.
const PLAYER_X: u32 = 0x7E_0094;
const PLAYER_Y: u32 = 0x7E_0096;

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

/// Collects the last frame's background rows (sprites left out: a level scout only keeps
/// the map).
#[derive(Default)]
struct RowSink {
    rows: Vec<Vec<Option<PpuPixel>>>,
}

impl CoreSink for RowSink {
    fn video_scanline(&mut self, y: u16, p: &[PpuPixel]) {
        let y = usize::from(y);
        if self.rows.len() <= y {
            self.rows.resize(y + 1, Vec::new());
        }
        self.rows[y] = p
            .iter()
            .take(256)
            .map(|q| (q.layer != rf_core_api::PixelLayer::Sprite).then_some(*q))
            .collect();
    }
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// The profile's observed `[[scout.addr]]` address for `role`.
fn scout_addr(profile: &str, role: &str) -> u32 {
    let v: toml::Value = toml::from_str(profile).expect("profile parses");
    v["scout"]["addr"]
        .as_array()
        .expect("scout.addr")
        .iter()
        .find(|a| a["role"].as_str() == Some(role))
        .and_then(|a| a["addr"].as_integer())
        .map(|a| a as u32)
        .expect("role present")
}

/// The scouted map of the session: each in-level frame's background rows placed at the
/// camera the game reports (the profile's `camera_x` / `camera_y`), as the level scout
/// stitches them. Returns the canvas and the in-level frame count.
/// The scouted canvas, in-level frame count, camera x range, and the player's position per frame.
type LevelScout = (Canvas, u32, (i64, i64), Vec<(i64, i64)>);

fn level_canvas(raw: &[u8], cam_x: u32, cam_y: u32) -> LevelScout {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut canvas = Canvas::new();
    let mut in_level = 0;
    let mut cam_xs = (i64::MAX, i64::MIN);
    let mut feet = Vec::new();
    for mask in session(raw) {
        // As `baseline` drives the session (the joypad latch, not the frame's input).
        core.system_mut().bus.joypads.ports[0] = mask;
        let mut sink = RowSink::default();
        core.run_frame(&InputFrame::empty(), &mut sink);
        if core.peek(MODE) != IN_LEVEL {
            continue;
        }
        in_level += 1;
        let word = |a: u32| i64::from(core.peek(a)) | (i64::from(core.peek(a + 1)) << 8);
        let (cx, cy) = (word(cam_x), word(cam_y));
        cam_xs = (cam_xs.0.min(cx), cam_xs.1.max(cx));
        feet.push((word(PLAYER_X), word(PLAYER_Y)));
        for (y, row) in sink.rows.iter().enumerate() {
            canvas.blit_row(cx, cy + y as i64, row);
        }
    }
    (canvas, in_level, cam_xs, feet)
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

/// Blocks per screen in the Map16 RAM buffer (screens are stored one after another, each
/// `BUF_COLS` wide and `BUF_ROWS` tall). This lays the buffer's bytes out in world cells; how
/// far the level reaches is the scout canvas's footprint, not this.
const BUF_COLS: u32 = 16;
const BUF_ROWS: u32 = 27;

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
    // The footprint: the scout's map of the session, placed at the camera the profile's
    // observed rows name. The baked grid is that footprint (its origin and extent on the
    // 16 px world grid), and a cell is baked only where the map has pixels. The buffer's
    // bytes are read as they stand at the end of play, laid out as stacked screens.
    let profile = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/super-mario-world/profile.toml"),
    )
    .expect("profile");
    let (canvas, in_level, _, feet) = level_canvas(
        &raw,
        scout_addr(&profile, "camera_x"),
        scout_addr(&profile, "camera_y"),
    );
    let fp = visited_cells(&canvas, 16).expect("the session scouted some map");
    // Where the buffer's first cell sits is observed, not assumed: the read set also holds
    // stray addresses far below the buffer (so its first address is no base). The base is the
    // offset, within a screen-and-a-bit before the first dense run of reads, under which the
    // cells at the player's feet are bodies and the cell he stands in is not, over the most
    // in-level frames (the way W39-04 found its layout).
    let mut runs: Vec<(u32, u32, usize)> = Vec::new();
    for &a in &site.addrs {
        match runs.last_mut() {
            Some(r) if a - r.1 <= 0x100 => {
                r.1 = a;
                r.2 += 1;
            }
            _ => runs.push((a, a, 1)),
        }
    }
    let dense_first = runs.iter().find(|r| r.2 >= 10).expect("a dense run").0;
    let stride = BUF_COLS * BUF_ROWS;
    let id_for = |base: u32, c: i64, r: i64| -> Option<u32> {
        if c < 0 || !(0..i64::from(BUF_ROWS)).contains(&r) {
            return None;
        }
        let (screen, col) = (c / i64::from(BUF_COLS), c % i64::from(BUF_COLS));
        let a = base + (screen as u32) * stride + (r as u32) * BUF_COLS + col as u32;
        site.addrs.contains(&a).then(|| u32::from(d.core.peek(a)))
    };
    let is_solid =
        |id: Option<u32>| id.is_some_and(|i| rules.iter().any(|r| r.first <= i && i <= r.last));
    let score_of = |base: u32| -> usize {
        feet.iter()
            .filter(|(px, py)| {
                let (x, y) = (px + 8, *py);
                let at = |dy: i64| {
                    let (wx, wy) = (x, y + dy);
                    if wx < 0 || wy < 0 {
                        return None;
                    }
                    id_for(base, wx / 16, wy / 16)
                };
                !is_solid(at(8)) && is_solid(at(16)) | is_solid(at(32))
            })
            .count()
    };
    let mut scores: Vec<(usize, u32)> = (dense_first.saturating_sub(0x400)..=dense_first)
        .map(|b| (score_of(b), b))
        .collect();
    scores.sort_unstable_by(|a, b| b.cmp(a));
    println!(
        "SMW buffer base search: best {:#x} scores {} of {} frames (next {:?}, first dense run {:#x}, first read addr {:#x})",
        scores[0].1,
        scores[0].0,
        feet.len(),
        &scores[1..4],
        dense_first,
        site.addrs.first().expect("addrs")
    );
    let base = scores[0].1;
    let id_at = |c: i64, r: i64| id_for(base, c, r);
    let screen = bake_canvas(
        1,
        &canvas,
        16,
        id_at,
        &Solidity {
            rules: &rules,
            painted: None,
        },
    )
    .expect("bakes");
    let known = screen
        .cells
        .iter()
        .filter(|c| c.kind != Kind::Unknown)
        .count();
    let bodies = screen.body_count();
    println!(
        "SMW footprint after {in_level} level frames: {} x {} cells at {:?} ({} visited), {} rule ranges, {known} cells judged, {bodies} solid/one-way cells, {} quads",
        fp.cols,
        fp.rows,
        fp.origin,
        fp.visited.iter().filter(|v| **v).count(),
        rules.len(),
        screen.quads.len()
    );
    assert_eq!(
        (screen.cols, screen.rows),
        (fp.cols, fp.rows),
        "the canvas footprint"
    );
    assert!(
        screen.cols > BUF_COLS,
        "the scouted level is wider than one screen ({} cols)",
        screen.cols
    );
    assert!(known > 0 && bodies > 0, "solid cells baked");
    assert!(!screen.quads.is_empty(), "walls extruded");
    assert!(
        screen.cells.iter().any(|c| c.kind == Kind::Unknown),
        "cells the scout never reached or the buffer lacks stay unknown"
    );
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

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR)"]
fn smw_footprint_is_the_scouted_camera_range() {
    let path = rom_dir().join("snes").join(FILE);
    let Some(raw) = load_rom(&path) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let profile = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/super-mario-world/profile.toml"),
    )
    .expect("profile");
    let (canvas, in_level, (lo, hi), _) = level_canvas(
        &raw,
        scout_addr(&profile, "camera_x"),
        scout_addr(&profile, "camera_y"),
    );
    let fp = visited_cells(&canvas, 16).expect("a scouted map");
    // The picture is 256 px wide, so the map spans the camera's range plus one screen.
    let want = (hi + 255).div_euclid(16) - lo.div_euclid(16) + 1;
    println!(
        "SMW: {in_level} level frames, camera x {lo}..={hi}, footprint {} x {} cells at {:?}",
        fp.cols, fp.rows, fp.origin
    );
    assert!(in_level > 0);
    assert_eq!(
        i64::from(fp.cols),
        want,
        "footprint width follows the scouted range"
    );
    assert_eq!(fp.origin.0, lo.div_euclid(16) * 16);
}
