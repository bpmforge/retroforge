//! **W36-06b step 1: what does SMB's OAM hold for an enemy kept alive deep in a 32:9 margin?**
//! (ROM-gated, law 5.) Test-side only: runs the W36-06 "Mario unhurt" 1-1 script with the live-enemies
//! mod on and, on every frame where a slot is deep in a margin, classifies OAM entries near the slot's
//! expected spot as WRAPPED (8-bit X wrapped onto the real picture), HIDDEN (Y pushed off the visible
//! range) or ABSENT. The matcher is validated first on slots inside the 256-px picture.
//!
//! ```text
//! cargo test --release -p rf-harness --test w36_06b_oam_observation -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_enhance::patch::prg::apply_prg_bytes;
use rf_harness::atlas::build_nes_atlas;
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_rebuild::{resolve_patches, Atlas, Recipe, ResolvedPatch};

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const B: u8 = 1 << 1;
const RIGHT: u8 = 1 << 7;
const MOD: &str = "live-enemies";
fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("nes").join(FILE);
    p.exists().then_some(p)
}

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

/// Two Starts, then Right with A pressed 12 frames in every 60 (W36-04's
/// script one).
fn script_one(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => RIGHT | A,
        _ => RIGHT,
    }
}

/// Run the whole way, jump 20 frames in every 90 (W36-04's script two).
fn script_two(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f => {
            let t = f - 500;
            let jump = t % 90 < 20 || t % 150 < 6;
            RIGHT | B | if jump { A } else { 0 }
        }
    }
}

type Script = fn(u32) -> u8;

fn inputs(script: Script, frames: u32) -> Vec<InputFrame> {
    (0..frames)
        .map(|f| {
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(script(f));
            i
        })
        .collect()
}

struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn force_level(frame: u32, core: &mut NesCore, level: Option<(u8, u8)>) {
    if let Some((w, l)) = level {
        if (380..=404).contains(&frame) {
            core.bus_mut().write(0x075F, w);
            core.bus_mut().write(0x0760, l);
        }
    }
}

/// Record a run of `script` on the image and return its coverage.
fn record(raw: &[u8], script: Script, level: Option<(u8, u8)>, frames: u32) -> CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for (f, input) in (0u32..).zip(inputs(script, frames)) {
        force_level(f, &mut core, level);
        core.run_frame(&input, &mut NullSink);
    }
    core.take_coverage().expect("armed")
}

fn recipe() -> Recipe {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/nes/super-mario-bros/recipe.toml");
    rf_profiles::load_recipe_file(&path).expect("the SMB recipe parses")
}

/// The mod's patch rows resolved against the atlas of the unpatched `raw`.
fn resolve(raw: &[u8], atlas: &Atlas) -> Vec<ResolvedPatch> {
    let _ = raw;
    resolve_patches(atlas, &recipe(), MOD).expect("the mod resolves")
}

fn mod_image(raw: &[u8], patches: &[ResolvedPatch]) -> Vec<u8> {
    let edits: Vec<(u32, u8)> = patches.iter().map(|p| (p.prg_offset, p.value)).collect();
    apply_prg_bytes(raw, &edits).expect("applies")
}

/// The atlas the app would have after one recorded 1-1 run (700 frames: the
/// first enemy has just appeared).
fn atlas_of_unpatched(raw: &[u8], frames: u32) -> Atlas {
    build_nes_atlas(raw, &record(raw, script_one, None, frames)).expect("atlas")
}

const ENEMY_Y: u16 = 0x00CF;

/// OAM entries as (y, tile, attr, x).
fn entries(core: &NesCore) -> Vec<(u8, u8, u8, u8)> {
    core.bus()
        .oam()
        .chunks_exact(4)
        .map(|e| (e[0], e[1], e[2], e[3]))
        .collect()
}

use std::collections::{HashMap, HashSet};

#[derive(Default, Debug)]
struct Tally {
    frames: u32,
    wrapped: u32,
    hidden: u32,
    absent: u32,
    /// Camera-relative x range over which the wrapped entries were seen.
    wrapped_rel: Option<(i64, i64)>,
}

type Tiles = HashMap<u8, HashSet<u8>>;

/// One pass of the unhurt script. `known` maps enemy id -> tiles seen on that enemy's entries while it was
/// inside the picture (so Mario, blocks and stale entries are not mistaken for the enemy); pass one fills it.
fn pass(image: &[u8], known: &Tiles) -> (Tally, Tally, Tally, Tiles) {
    let mut core = NesCore::from_ines_bytes(image).expect("loads");
    core.reset(ResetKind::Hard);
    let (mut on_screen, mut right, mut left) =
        (Tally::default(), Tally::default(), Tally::default());
    let mut learned = Tiles::new();
    let mut samples = 0;
    for (f, input) in (0u32..).zip(inputs(script_two, 3200)) {
        if core.peek(0x0770) == 1 {
            core.bus_mut().write(0x079E, 0x30);
        }
        core.run_frame(&input, &mut NullSink);
        if f < 500 || core.peek(0x0770) != 1 {
            continue;
        }
        let cam = i64::from(core.peek(0x071A)) * 256 + i64::from(core.peek(0x071C));
        let oam = entries(&core);
        for k in 0..5u32 {
            if core.peek(0x0F + k) == 0 {
                continue;
            }
            let id = core.peek(0x16 + k);
            let rel = i64::from(core.peek(0x6E + k)) * 256 + i64::from(core.peek(0x87 + k)) - cam;
            let ey = i64::from(core.peek(u32::from(ENEMY_Y) + k));
            let wx = rel.rem_euclid(256);
            let near_x = |x: u8| {
                let dx = (i64::from(x) - wx).rem_euclid(256);
                !(20..=244).contains(&dx)
            };
            // Mario's own entries (screen x $03AD, y $00CE) are never the enemy's.
            let (px, py) = (i64::from(core.peek(0x03AD)), i64::from(core.peek(0x00CE)));
            let mario = |&(y, _, _, x): &(u8, u8, u8, u8)| {
                (-16..16).contains(&(i64::from(x) - px))
                    && (-12..=24).contains(&(i64::from(y) - py))
            };
            let near = |e: &(u8, u8, u8, u8)| {
                let &(y, _, _, x) = e;
                !mario(e) && y < 0xEF && near_x(x) && (-24..=8).contains(&(i64::from(y) - ey))
            };
            let tally = match rel {
                16..=224 => {
                    let set = learned.entry(id).or_default();
                    for e in oam.iter().filter(|e| near(e)) {
                        set.insert(e.1);
                    }
                    &mut on_screen
                }
                336..=555 => &mut right,
                -300..=-81 => &mut left,
                _ => continue,
            };
            // An entry that another slot's enemy, standing in the picture, explains is not this slot's.
            let others: Vec<(i64, i64)> = (0..5u32)
                .filter(|&k2| k2 != k && core.peek(0x0F + k2) != 0)
                .map(|k2| {
                    let r2 = i64::from(core.peek(0x6E + k2)) * 256
                        + i64::from(core.peek(0x87 + k2))
                        - cam;
                    (r2, i64::from(core.peek(u32::from(ENEMY_Y) + k2)))
                })
                .filter(|(r2, _)| (-16..256).contains(r2))
                .collect();
            let explained = |e: &(u8, u8, u8, u8)| {
                !(16..=224).contains(&rel)
                    && others.iter().any(|&(r2, y2)| {
                        let dx = i64::from(e.3) - r2;
                        (-12..20).contains(&dx) && (-24..=8).contains(&(i64::from(e.0) - y2))
                    })
            };
            let near = |e: &(u8, u8, u8, u8)| near(e) && !explained(e);
            let tiles = known.get(&id);
            let is_enemy = |e: &(u8, u8, u8, u8)| tiles.is_some_and(|t| t.contains(&e.1));
            tally.frames += 1;
            if oam.iter().any(|e| near(e) && is_enemy(e)) {
                tally.wrapped += 1;
                if !(16..=224).contains(&rel) {
                    println!("WRAP f{f} slot{k} id{id} rel {rel}");
                }
                let (lo, hi) = tally.wrapped_rel.unwrap_or((rel, rel));
                tally.wrapped_rel = Some((lo.min(rel), hi.max(rel)));
                if samples < 4 && !std::ptr::eq(tally, &on_screen) {
                    samples += 1;
                    println!(
                        "frame {f} slot {k} id {id} rel {rel} ey {ey}: {:?}",
                        oam.iter().filter(|e| near(e)).collect::<Vec<_>>()
                    );
                }
            } else if oam
                .iter()
                .any(|e| e.0 >= 0xEF && near_x(e.3) && is_enemy(e))
            {
                tally.hidden += 1;
            } else {
                tally.absent += 1;
            }
        }
    }
    (on_screen, right, left, learned)
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn oam_for_enemies_kept_alive_in_the_margins() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let atlas = atlas_of_unpatched(&raw, 700);
    let image = mod_image(&raw, &resolve(&raw, &atlas));
    let (_, _, _, known) = pass(&image, &Tiles::new());
    println!("enemy tiles learned on-screen: {known:?}");
    let (on_screen, right, left, _) = pass(&image, &known);
    println!("WRAPPED = an entry with one of the enemy's own tiles drawn at the wrapped 8-bit X near the slot's Y;");
    println!("HIDDEN = such a tile at the wrapped X but Y >= 0xEF; ABSENT = neither (cleared or stale beyond the tile test)");
    for (n, t) in [
        ("on-screen (matcher check)", &on_screen),
        ("deep right", &right),
        ("deep left", &left),
    ] {
        println!(
            "{n:<26} slot-frames {:>5}  wrapped {:>5} (rel {:?})  hidden {:>5}  absent {:>5}",
            t.frames, t.wrapped, t.wrapped_rel, t.hidden, t.absent
        );
    }
    assert!(
        right.frames + left.frames > 100,
        "the run exercised the margins"
    );
    assert!(
        on_screen.frames > 100 && on_screen.wrapped * 10 > on_screen.frames * 8,
        "the matcher finds on-screen enemies: {on_screen:?}"
    );
}

/// (x, top-row y) pairs of the entries a PPU draws from `oam` (y < 0xEF), 8x8 mode only.
fn oam_pairs(oam: &[(u8, u8, u8, u8)]) -> Vec<(i16, i16)> {
    let mut v: Vec<(i16, i16)> = oam
        .iter()
        .filter(|e| e.0 < 0xEF)
        .map(|e| (i16::from(e.3), i16::from(e.0) + 1))
        .collect();
    v.sort_unstable();
    v
}

/// When is a sprite that is on the picture of frame N in OAM? Compares the sprites the PPU drew in
/// `run_frame(N)` (`completed_sprites`) with OAM read at the end of `run_frame(N)` and of `run_frame(N-1)`.
#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn which_frames_oam_does_the_picture_come_from() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.bus_mut().set_tile_capture(true);
    let mut prev_oam = Vec::new();
    let (mut same_end, mut same_prev, mut frames) = (0u32, 0u32, 0u32);
    for (f, input) in (0u32..).zip(inputs(script_two, 1500)) {
        if core.peek(0x0770) == 1 {
            core.bus_mut().write(0x079E, 0x30);
        }
        core.run_frame(&input, &mut NullSink);
        let end_oam = entries(&core);
        if f > 600 && core.peek(0x0770) == 1 {
            let mut drawn: Vec<(i16, i16)> = core
                .bus()
                .completed_sprites()
                .iter()
                .filter(|s| s.base == 0x0000 || s.base == 0x1000)
                .map(|s| (s.x, s.y))
                .collect();
            drawn.sort_unstable();
            drawn.dedup();
            // Compare on the sprites' top-left tiles only: both lists as sets of (x, y).
            let covers = |pairs: &[(i16, i16)]| {
                drawn.iter().all(|d| {
                    pairs
                        .iter()
                        .any(|p| p.0 == d.0 && (p.1 - d.1).rem_euclid(8) == 0)
                })
            };
            frames += 1;
            same_end += u32::from(covers(&oam_pairs(&end_oam)));
            same_prev += u32::from(covers(&oam_pairs(&prev_oam)));
        }
        prev_oam = end_oam;
    }
    println!("frames {frames}: drawn sprites all found in OAM at the END of the same run_frame: {same_end}; in OAM at the end of the PREVIOUS run_frame: {same_prev}");
}
