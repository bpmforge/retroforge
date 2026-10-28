//! **W39-04: the HD-2D render plugin** (extruded walls from the baked stage
//! set, billboard objects with per-class shadows, tilted camera, depth of
//! field).
//!
//! * `the_hd2d_plugin_*` (no ROM): the shipped plugin loads as a `render`
//!   plugin, marks recipe objects with class codes, and asks for nothing that
//!   acts.
//! * `original_mode_*` (the repo's own synthetic SNES fixture): the Accuracy
//!   buffers and RAM are byte-identical frame by frame whether or not the
//!   plugin runs and the HD-2D frame is rendered.
//! * `smw_demo_*` (ROM-gated, law 5): Yoshi's Island 1 is played to, the
//!   stage set baked from the W38-03 collision facts and reloaded from the
//!   cache, and a few frames rendered to PNGs written OUTSIDE the repo (set
//!   `RF_W39_DEMO_DIR`; default is under the system temp dir). Only offsets,
//!   strides and counts are printed. The same run is repeated without HD-2D
//!   and every frame's buffers compared.
//!
//! ```text
//! cargo test --release -p retroforge --test w39_04_hd2d_plugin -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;

use rf_cache::Cache;
use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_enhance::baked::{
    bake_screen, load_stage_set, save_stage_set, BakedScreen, Kind, RuleClass, ScreenInput,
    Solidity, StageSet, TileRule,
};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::collision_tables::{run_snes, Watch};
use rf_harness::data_regions::SnesDriver;
use rf_input::SnesButton;
use rf_plugin_sdk::sandbox::{MemoryWindow, OverlayCmd};
use rf_plugin_sdk::{enable, PluginKind, RenderPlugin, SurfaceManifest};
use rf_rebuild::collision::{infer, to_rows};
use rf_rebuild::{Grade, ObjectTable, RamMeaning, Recipe, TileClass};
use rf_renderer::diorama::DioramaPass;
use rf_renderer::gpu::GpuContext;
use rf_renderer::hd2d::{code, render, Hd2dLattice, Hd2dLook, Hd2dMarker, Hd2dStage};
use rf_renderer::layers::LayeredFrame;
use rf_renderer::lens::LensPass;
use rf_snes::core::SnesCore;

const FILE: &str = "Super Mario World (USA).zip";
const MODE: u32 = 0x7E_0100;
const IN_LEVEL: u8 = 0x14;
/// The `$7E0000..` window the plugin reads (WRAM bank 7E low half).
const WINDOW_LEN: u32 = 0x2000;
const WIDTH: usize = 256;
const HEIGHT: usize = 224;
const CELL: i64 = 16;
/// Map16 blocks per column of one horizontal-level screen in the RAM buffer
/// (16 columns x 27 rows per screen, screens consecutive). Verified below by
/// the foot-cell check, not assumed.
const SCREEN_ROWS: i64 = 27;

const TO_LEVEL: &str = "w200 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 w300 A8 w408 A8 w200 R40 w20 L40 w20 R40 w20 L40 w20 R40 w20 L40 w20 R60 w20 R200 w300 L8 w158 A8 w158";
const PLAY: &str = "L2 w3 B8 w52 B8 w52 B8 w52 B8 w52 R+B14 R22 R+B14 R22 S6 w100 S6 w60 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/hd2d")
}

fn manifest() -> SurfaceManifest {
    let text = std::fs::read_to_string(plugin_dir().join("plugin.toml")).expect("plugin.toml");
    SurfaceManifest::parse(&text).expect("the hd2d manifest validates")
}

fn load_plugin(recipe: &Recipe) -> RenderPlugin {
    let src = std::fs::read_to_string(plugin_dir().join("main.lua")).expect("main.lua");
    RenderPlugin::load(&manifest(), &src, recipe, MemoryWindow::default()).expect("loads")
}

fn markers_of(cmds: &[OverlayCmd]) -> Vec<Hd2dMarker> {
    cmds.iter()
        .filter_map(|c| match *c {
            OverlayCmd::Rect {
                x,
                y,
                width,
                height,
                color_index,
            } => Some(Hd2dMarker::from_rect(x, y, width, height, color_index)),
            OverlayCmd::Line { .. } => None,
        })
        .collect()
}

fn ram(addr: u32, name: &str) -> RamMeaning {
    RamMeaning {
        addr,
        width: 2,
        name: name.into(),
        source: "w39-04 test".into(),
        grade: Grade::Candidate,
    }
}

// ---------------------------------------------------------------- no ROM ---

#[test]
fn the_hd2d_plugin_is_a_render_plugin_that_cannot_act() {
    let m = manifest();
    assert_eq!(m.kind, PluginKind::Render);
    assert!(!m.manifest.capabilities.write_memory);
    assert!(!m.manifest.capabilities.function_hooks);
    assert_eq!(enable(&m, None), Ok(PluginKind::Render));
}

#[test]
fn the_hd2d_plugin_marks_the_player_and_object_table_slots_with_class_codes() {
    let recipe = Recipe {
        ram: vec![
            ram(0x0100, "camera_x"),
            ram(0x0102, "camera_y"),
            ram(0x0104, "player_x"),
            ram(0x0106, "player_y"),
        ],
        object_tables: vec![ObjectTable {
            name: "enemies".into(),
            base: 0x0200,
            slots: 3,
            stride: 1,
            x_lo: Some(0),
            y: Some(4),
            state: Some(8),
            ..ObjectTable::default()
        }],
        ..Recipe::default()
    };
    let mut bytes = vec![0u8; 0x300];
    let w16 = |b: &mut Vec<u8>, a: usize, v: u16| b[a..a + 2].copy_from_slice(&v.to_le_bytes());
    w16(&mut bytes, 0x100, 300); // camera_x
    w16(&mut bytes, 0x102, 20); // camera_y
    w16(&mut bytes, 0x104, 340); // player_x -> screen 40
    w16(&mut bytes, 0x106, 100); // player_y -> screen 80
                                 // Slot 0 active at (310, 60) -> screen (10, 40); slot 1 inactive; slot 2
                                 // active but off screen.
    bytes[0x200] = 54; // 310 = 256 + 54, no x_hi in this table: world 54
    bytes[0x204] = 60;
    bytes[0x208] = 1;
    bytes[0x202] = 250;
    bytes[0x206] = 60;
    bytes[0x20a] = 1;
    let mut p = load_plugin(&recipe);
    p.set_memory(MemoryWindow { base: 0, bytes });
    let m = markers_of(&p.draw(1));
    assert_eq!(m[0], Hd2dMarker::from_rect(40, 80, 16, 32, code::PLAYER));
    // Slot 0: world x 54 - camera 300 is off screen (negative); slot 2 too.
    assert_eq!(m.len(), 1, "{m:?}");
    // Draw data only: nothing it did is a write.
    assert!(p.host().ledger().is_empty());
}

// ---------------------------------------------------- Original-mode proof ---

struct Null;
impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h = (*h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
}

/// What the Accuracy side holds after a frame: both layer buffers and the
/// window of WRAM.
fn accuracy_hash(core: &SnesCore, layers: &LayeredFrame) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    fnv(&mut h, layers.bg_rgba());
    fnv(&mut h, layers.sprite_rgba());
    let wram: Vec<u8> = (0..WINDOW_LEN).map(|i| core.peek(0x7E_0000 + i)).collect();
    fnv(&mut h, &wram);
    h
}

fn window(core: &SnesCore) -> MemoryWindow {
    MemoryWindow {
        base: 0x7E_0000,
        bytes: (0..WINDOW_LEN).map(|i| core.peek(0x7E_0000 + i)).collect(),
    }
}

/// Runs the pad and returns one accuracy hash per frame. With `hd2d`, the
/// plugin runs and a frame is rendered every `every` frames from the same
/// buffers.
struct Rig<'a> {
    gpu: Option<&'a GpuContext>,
    plugin: RenderPlugin,
    every: usize,
    rendered: usize,
}

fn run_hashes(raw: &[u8], pad: &[u16], mut rig: Option<&mut Rig<'_>>) -> Vec<u64> {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut out = Vec::with_capacity(pad.len());
    let mut layers = LayeredFrame::with_size(WIDTH, HEIGHT);
    let (dpass, lens) = (rig.as_ref().and_then(|r| r.gpu)).map_or((None, None), |g| {
        (Some(DioramaPass::new(g)), Some(LensPass::new(g)))
    });
    for (f, &mask) in pad.iter().enumerate() {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&InputFrame::empty(), &mut layers);
        if let Some(r) = rig.as_deref_mut() {
            if f % r.every == 0 {
                r.plugin.set_memory(window(&core));
                let markers = markers_of(&r.plugin.draw(f as u64));
                if let (Some(g), Some(d), Some(l)) = (r.gpu, dpass.as_ref(), lens.as_ref()) {
                    let depth = vec![0u8; 17 * 15];
                    let stage = Hd2dStage {
                        picture_rgba: layers.bg_rgba(),
                        sprite_rgba: layers.sprite_rgba(),
                        size: (WIDTH as u32, HEIGHT as u32),
                        lattice: Hd2dLattice {
                            origin: (0.0, 0.0),
                            tile_px: CELL as f32,
                            cols: 17,
                            rows: 15,
                            depth: &depth,
                        },
                    };
                    let look = Hd2dLook {
                        out: (640, 360),
                        ..Hd2dLook::default()
                    };
                    render(g, d, l, &stage, &markers, &look).expect("renders");
                    r.rendered += 1;
                }
            }
        }
        out.push(accuracy_hash(&core, &layers));
    }
    out
}

#[test]
fn original_mode_accuracy_buffers_are_byte_identical_with_the_plugin_on() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc");
    let Ok(raw) = std::fs::read(&path) else {
        eprintln!("SKIP: fixture ROM not built");
        return;
    };
    let pad: Vec<u16> = (0..180)
        .map(|f| {
            if f % 40 < 30 {
                1 << SnesButton::Right.bit()
            } else {
                0
            }
        })
        .collect();
    let plain = run_hashes(&raw, &pad, None);
    let gpu = GpuContext::request_headless().ok();
    let mut rig = Rig {
        gpu: gpu.as_ref(),
        plugin: load_plugin(&Recipe::default()),
        every: 15,
        rendered: 0,
    };
    let with = run_hashes(&raw, &pad, Some(&mut rig));
    assert_eq!(plain.len(), 180);
    assert_eq!(plain, with, "Accuracy buffers diverged with the plugin on");
    println!(
        "fixture: 180 frames identical, {} HD-2D frames rendered (gpu: {})",
        rig.rendered,
        gpu.is_some()
    );
}

// ------------------------------------------------------------- SMW demo ---

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
                    "S" => SnesButton::Start,
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
fn session() -> (Vec<u16>, usize) {
    let mut pad = frames_of(TO_LEVEL);
    let start = pad.len();
    pad.extend(frames_of(PLAY));
    let more = frames_of("R+B14 R22");
    while pad.len() < start + 1500 {
        pad.extend(&more);
    }
    (pad, start)
}

fn input(mask: u16) -> InputFrame {
    let mut f = InputFrame::empty();
    f.ports[0] = mask;
    f
}

fn baseline(raw: &[u8], pad: &[u16]) -> CoverageLog {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    for &mask in pad {
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

/// What the collision finder found: rule ranges and the span of the RAM
/// buffer of block ids its observed addresses fall in.
struct Finding {
    rules: Vec<TileRule>,
    lo: u32,
    hi: u32,
}

/// The finder's facts for the level (as `w39_03_baked_geometry` runs it). The
/// observed addresses are sparse (only blocks Mario touched), so they are
/// split into runs and the densest run's span is what the demo reads whole
/// from RAM afterwards: block ids as they stand in the buffer, not bytes of
/// the ROM.
fn find_collision(raw: &[u8], pad: &[u16]) -> Finding {
    let atlas = build_snes_atlas(raw, &baseline(raw, pad)).expect("atlas");
    let mut d = SnesDriver::new(raw, &atlas).expect("loads");
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
        "W39-04",
        Grade::Candidate,
    );
    let rules: Vec<TileRule> = rows
        .iter()
        .map(|r| TileRule {
            first: r.first,
            last: r.last,
            class: match r.class {
                TileClass::Solid => RuleClass::Solid,
                TileClass::OneWay => RuleClass::OneWay,
                TileClass::Water | TileClass::Hazard => RuleClass::Open,
            },
        })
        .collect();
    let addrs: Vec<u32> = site.addrs.iter().copied().collect();
    // Runs: a gap over 0x100 bytes starts a new one.
    let mut runs: Vec<(u32, u32, usize)> = Vec::new();
    for &a in &addrs {
        match runs.last_mut() {
            Some(r) if a - r.1 <= 0x100 => {
                r.1 = a;
                r.2 += 1;
            }
            _ => runs.push((a, a, 1)),
        }
    }
    for r in &runs {
        println!("observed run {:#x}..={:#x}: {} addrs", r.0, r.1, r.2);
    }
    // Dense runs (a row of blocks Mario walked along); the buffer holds
    // whole screens around them, so read from a screen before the first to
    // a screen and a half after the last.
    let dense: Vec<&(u32, u32, usize)> = runs.iter().filter(|r| r.2 >= 10).collect();
    let first = dense.first().expect("a dense run").0;
    let last = dense.last().expect("a dense run").1;
    println!("{} rule ranges; {} dense runs", rules.len(), dense.len());
    Finding {
        rules,
        lo: first.saturating_sub(0x400),
        hi: last + 0x600,
    }
}

/// One frame the plugin and renderer are run on, and what the alignment
/// search samples.
struct Sample {
    cam: (i64, i64),
    player: (i64, i64),
}

struct Grab {
    frame: usize,
    sample: Sample,
    markers: Vec<Hd2dMarker>,
    bg: Vec<u8>,
    sprite: Vec<u8>,
}

/// The stage screen baked from the RAM span read from `off` (the buffer's
/// first cell).
fn bake_with(f: &Finding, ram: &[u8], off: u32) -> BakedScreen {
    let mut ids: Vec<Option<u32>> = ram[(off as usize).min(ram.len())..]
        .iter()
        .map(|&b| Some(u32::from(b)))
        .collect();
    let n = (ids.len() as u32).div_ceil(16) * 16;
    ids.resize(n as usize, None);
    bake_screen(
        &ScreenInput {
            id: 1,
            origin: (0, 0),
            cols: 16,
            rows: n / 16,
            cell_px: 16,
            ids: &ids,
            visited: None,
        },
        &Solidity {
            rules: &f.rules,
            painted: None,
        },
    )
    .expect("bakes")
}

/// The baked cell under world pixel `(wx, wy)` for a layout of
/// `rows_per_screen` rows per screen.
fn baked_cell(
    s: &BakedScreen,
    rows_per_screen: i64,
    wx: i64,
    wy: i64,
) -> Option<rf_enhance::baked::Cell> {
    if wx < 0 || wy < 0 {
        return None;
    }
    let col = (wx / CELL) % 16;
    let row = (wx / (16 * CELL)) * rows_per_screen + wy / CELL;
    s.cell(col as u32, u32::try_from(row).ok()?)
}

fn is_body(c: Option<rf_enhance::baked::Cell>) -> bool {
    c.is_some_and(|c| c.kind.is_body())
}

/// Find where the buffer's first cell sits and how tall a screen is, from
/// the samples alone: the layout under which the cells below Mario's feet
/// are bodies and the cell he stands in is not, over the most samples.
fn best_layout(f: &Finding, ram: &[u8], samples: &[Sample]) -> (u32, i64, usize) {
    let mut scores: Vec<(usize, u32, i64)> = Vec::new();
    let mut best = (0u32, SCREEN_ROWS, 0usize);
    let mut runner_up = 0usize;
    for rows in [26i64, 27, 28, 32] {
        for shift in 0..0x400u32 {
            let screen = bake_with(f, ram, shift);
            let score = samples
                .iter()
                .filter(|s| {
                    let (x, y) = (s.player.0 + 8, s.player.1);
                    let at = |dy: i64| baked_cell(&screen, rows, x, y + dy);
                    !is_body(at(8)) && (is_body(at(16)) || is_body(at(32)))
                })
                .count();
            scores.push((score, shift, rows));
            if score > best.2 {
                runner_up = best.2;
                best = (shift, rows, score);
            } else if score > runner_up {
                runner_up = score;
            }
        }
    }
    scores.sort_unstable_by(|a, b| b.cmp(a));
    println!(
        "top layouts (score, offset, rows/screen): {:?}",
        &scores[..scores.len().min(6)]
    );
    println!(
        "layout search over {} samples: best offset {} rows/screen {} score {} (runner-up {runner_up})",
        samples.len(),
        best.0,
        best.1,
        best.2
    );
    best
}

fn depth_for_window(
    s: &BakedScreen,
    rows_per_screen: i64,
    cam_x: i64,
    cam_y: i64,
) -> (Vec<u8>, usize) {
    let (bx, by) = (
        cam_x - cam_x.rem_euclid(CELL),
        cam_y - cam_y.rem_euclid(CELL),
    );
    let mut depth = Vec::with_capacity(17 * 15);
    let mut bodies = 0;
    for r in 0..15 {
        for c in 0..17 {
            let d = match baked_cell(s, rows_per_screen, bx + c * CELL, by + r * CELL) {
                Some(cell) if cell.kind == Kind::Solid => cell.height_eighths,
                Some(cell) if cell.kind == Kind::OneWay => 2,
                _ => 0,
            };
            bodies += usize::from(d > 0);
            depth.push(d);
        }
    }
    (depth, bodies)
}

fn out_dir() -> PathBuf {
    let d = std::env::var_os("RF_W39_DEMO_DIR").map_or_else(
        || std::env::temp_dir().join("rf-w39-04-demo"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&d).expect("demo dir");
    d
}

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR)"]
fn smw_demo_renders_hd2d_frames_and_leaves_original_mode_identical() {
    let path = rom_dir().join("snes").join(FILE);
    let Some(raw) = load_rom(&path) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let Ok(gpu) = GpuContext::request_headless() else {
        println!("SKIP: no wgpu adapter");
        return;
    };
    let (pad, start) = session();
    let finding = find_collision(&raw, &pad);
    let recipe_text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../profiles/snes/super-mario-world/recipe.toml"),
    )
    .expect("recipe");
    let recipe = rf_profiles::load_recipe_str(&recipe_text).expect("recipe parses");
    let mut plugin = load_plugin(&recipe);

    // The play run, with the plugin drawing on the frames it will render.
    let grabs = [start + 500, start + 900, start + 1200, start + 1499];
    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    let mut layers = LayeredFrame::with_size(WIDTH, HEIGHT);
    let mut hashes = Vec::with_capacity(pad.len());
    let mut samples: Vec<Sample> = Vec::new();
    let mut taken: Vec<Grab> = Vec::new();
    for (f, &mask) in pad.iter().enumerate() {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&InputFrame::empty(), &mut layers);
        if core.peek(MODE) == IN_LEVEL {
            let rd16 = |a: u32| i64::from(core.peek(a)) | (i64::from(core.peek(a + 1)) << 8);
            let sample = Sample {
                cam: (rd16(0x7E_001A), rd16(0x7E_001C)),
                player: (rd16(0x7E_0094), rd16(0x7E_0096)),
            };
            if core.peek(0x7E_0071) == 0 {
                samples.push(Sample {
                    cam: sample.cam,
                    player: sample.player,
                });
            }
            if grabs.contains(&f) {
                plugin.set_memory(window(&core));
                taken.push(Grab {
                    frame: f,
                    sample,
                    markers: markers_of(&plugin.draw(f as u64)),
                    bg: layers.bg_rgba().to_vec(),
                    sprite: layers.sprite_rgba().to_vec(),
                });
            }
        }
        hashes.push(accuracy_hash(&core, &layers));
    }
    let ram: Vec<u8> = (finding.lo..=finding.hi).map(|a| core.peek(a)).collect();
    println!(
        "level frames sampled {}, grabs {}, buffer span {:#x}..={:#x}",
        samples.len(),
        taken.len(),
        finding.lo,
        finding.hi
    );

    // Bake, cache, reload; find the layout from the samples.
    let (shift, rows, score) = best_layout(&finding, &ram, &samples);
    let baked = bake_with(&finding, &ram, shift);
    let root = std::env::temp_dir().join(format!("rf-w39-04-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut cache = Cache::open(&root, 256 << 20).expect("cache");
    let sha = rf_cart::hash::hash_all(&raw).sha256;
    let set = StageSet {
        screens: vec![baked],
    };
    save_stage_set(&mut cache, &sha, &set).expect("saves");
    let loaded = load_stage_set(&mut cache, &sha)
        .expect("loads")
        .expect("present");
    assert_eq!(loaded, set);
    let _ = std::fs::remove_dir_all(root);
    let baked = &loaded.screens[0];
    println!(
        "baked: {}x{} cells, {} bodies, {} wall quads",
        baked.cols,
        baked.rows,
        baked.body_count(),
        baked.quads.len()
    );
    assert!(baked.body_count() > 0 && !baked.quads.is_empty());
    assert!(
        score * 5 >= samples.len(),
        "layout explains the feet cells of {score} of {} samples",
        samples.len()
    );

    let dir = out_dir();
    let (pass, lens) = (DioramaPass::new(&gpu), LensPass::new(&gpu));
    let mut grabbed = 0usize;
    for g in &taken {
        let (cam_x, cam_y) = g.sample.cam;
        let (depth, bodies) = depth_for_window(baked, rows, cam_x, cam_y);
        println!(
            "frame {}: camera ({cam_x},{cam_y}) player {:?} markers {} bodies in view {bodies}",
            g.frame,
            g.sample.player,
            g.markers.len(),
        );
        assert!(
            g.markers
                .iter()
                .any(|m| m.class == rf_renderer::diorama_objects::ObjectClass::Player),
            "the plugin marks the player at frame {}",
            g.frame
        );
        let stage = Hd2dStage {
            picture_rgba: &g.bg,
            sprite_rgba: &g.sprite,
            size: (WIDTH as u32, HEIGHT as u32),
            lattice: Hd2dLattice {
                origin: (
                    -(cam_x.rem_euclid(CELL) as f32),
                    -(cam_y.rem_euclid(CELL) as f32),
                ),
                tile_px: CELL as f32,
                cols: 17,
                rows: 15,
                depth: &depth,
            },
        };
        let look = Hd2dLook::default();
        let (img, plan) = render(&gpu, &pass, &lens, &stage, &g.markers, &look).expect("renders");
        println!(
            "  entities {} wall cells {} focus {:.2}",
            plan.entities.len(),
            plan.wall_cells,
            plan.focus_frac
        );
        let png = rf_renderer::png::encode_rgba(&img, look.out.0, look.out.1);
        std::fs::write(dir.join(format!("smw-hd2d-{grabbed}.png")), png).unwrap();
        // The Original-mode picture beside it: sprites over background.
        let mut flat = g.bg.clone();
        for (d, s) in flat.chunks_exact_mut(4).zip(g.sprite.chunks_exact(4)) {
            if s[3] != 0 {
                d.copy_from_slice(s);
            }
        }
        let png = rf_renderer::png::encode_rgba(&flat, WIDTH as u32, HEIGHT as u32);
        std::fs::write(dir.join(format!("smw-original-{grabbed}.png")), png).unwrap();
        grabbed += 1;
    }
    assert!(grabbed >= 2, "frames grabbed: {grabbed}");
    // The same run without HD-2D: every frame's Accuracy buffers and WRAM match.
    let plain = run_hashes(&raw, &pad, None);
    assert_eq!(plain, hashes, "Original mode diverged with HD-2D on");
    println!(
        "{} frames identical with and without HD-2D; {grabbed} PNGs in {}",
        pad.len(),
        dir.display()
    );
}
