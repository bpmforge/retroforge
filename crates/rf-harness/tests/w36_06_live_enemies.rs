//! **W36-06: SMB enemies stay alive in the 32:9 margins** (ROM-gated, law 5).
//!
//! The mod is three bytes of the player's own ROM image, placed by the SMB
//! recipe (`profiles/nes/super-mario-bros/recipe.toml`, `[[patch]]` rows): the
//! distance at which level objects come alive to the right of the screen, and
//! the two "too far outside the screen, remove" distances. The rows are
//! resolved against the atlas of the UNPATCHED image and applied to a copy.
//! Method and findings: `docs/research/W36-06-live-enemies.md`.
//!
//! Everything is measured from RAM (the Data Crystal SMB RAM map, each address
//! checked by observation): enemy slots `$0F-$13` (flag), `$6E-$72` + `$87-$8B`
//! (level x), the camera `$071A`/`$071C`. An enemy's camera-relative x is its
//! level x minus the camera; the 32:9 picture shows 300 px each side of the
//! 256-px screen, so "in the margin" is relative x in 256..556 (right) or
//! -300..0 (left). Mod-off SMB never keeps an enemy more than about 72 px
//! outside the screen, so the DEEP bands (right 336..556, left -300..-80) are
//! where mod-off must show nothing.
//!
//! Test-side state setup (never shipped): level select by writing `$075F`/
//! `$0760` on frames 380-404 (as W36-04), and for the long runs a poke of the
//! injury timer `$079E` every frame in play on BOTH variants so Mario survives
//! long enough to meet many enemies.
//!
//! ```text
//! cargo test --release -p rf-harness --test w36_06_live_enemies -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_enhance::patch::prg::apply_prg_bytes;
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options, Side};
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_rebuild::{resolve_patches, Atlas, Recipe, ResolvedPatch};

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const B: u8 = 1 << 1;
const RIGHT: u8 = 1 << 7;
const MOD: &str = "live-enemies";
/// Pixels of margin each side at 32:9 over the NES's 240-line picture:
/// (856 - 256) / 2, from `Aspect::Wide32x9.output_width(240, 1.0)`.
const MARGIN: i64 = 300;
/// Where mod-off SMB never has an enemy: beyond right edge + 80 / left of -80.
const DEEP_RIGHT: i64 = 256 + 80;
const DEEP_LEFT: i64 = -80;

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

fn world_x(core: &NesCore) -> i64 {
    i64::from(core.peek(0x006D)) * 256 + i64::from(core.peek(0x0086))
}

#[derive(Default, Debug, Clone)]
struct Play {
    frames_in_play: u32,
    /// Frames with at least one active enemy inside the right / left margin.
    right_margin: u32,
    left_margin: u32,
    /// The same for the deep bands, where mod-off must show none.
    right_deep: u32,
    left_deep: u32,
    /// Deep-band enemy-frames; and over 8-frame windows that ended deep, how
    /// many saw the enemy's level x change (alive and moving, not parked; a
    /// walker advances well under a pixel a frame, so one frame is too short).
    deep_moving: u32,
    deep_windows: u32,
    /// The same two counts for the right-hand deep band alone.
    rdeep_moving: u32,
    rdeep_windows: u32,
    deep_enemy_frames: u32,
    /// Longest unbroken run of frames with a deep-band enemy.
    deep_run: u32,
    spawns: u32,
    /// Spawns that happened with the enemy already well inside the 256-px
    /// screen (relative x below 224): a slot was busy when the object's turn
    /// came, so it came alive late.
    late_spawns: u32,
    /// Frames on which all five slots were occupied.
    all_full_frames: u32,
    max_active: u32,
    best_x: i64,
    deaths: u32,
    /// FNV over WRAM after every frame.
    wram_hash: u64,
}

/// Play `frames` frames of `script` on `image`, optionally keeping Mario
/// unhurt, and tally the enemy slots each frame in play.
fn play(image: &[u8], script: Script, level: Option<(u8, u8)>, frames: u32, unhurt: bool) -> Play {
    let mut core = NesCore::from_ines_bytes(image).expect("loads");
    core.reset(ResetKind::Hard);
    let mut p = Play {
        wram_hash: 0xcbf2_9ce4_8422_2325,
        ..Play::default()
    };
    let mut prev_flag = [0u8; 5];
    // Per slot: level x at the checkpoint and frames since it was taken.
    let mut check: [Option<(i64, u32)>; 5] = [None; 5];
    let mut run = 0u32;
    let mut lives = None::<u8>;
    for (f, input) in (0u32..).zip(inputs(script, frames)) {
        force_level(f, &mut core, level);
        if unhurt && core.peek(0x0770) == 1 {
            core.bus_mut().write(0x079E, 0x30);
        }
        core.run_frame(&input, &mut NullSink);
        for a in 0..0x800u32 {
            p.wram_hash = (p.wram_hash ^ u64::from(core.peek(a))).wrapping_mul(0x0100_0000_01b3);
        }
        let l = core.peek(0x075A);
        if lives.is_some_and(|prev| l < prev) {
            p.deaths += 1;
        }
        lives = Some(l);
        if f < 500 || core.peek(0x0770) != 1 {
            prev_flag = [0; 5];
            check = [None; 5];
            run = 0;
            continue;
        }
        p.frames_in_play += 1;
        p.best_x = p.best_x.max(world_x(&core));
        let cam = i64::from(core.peek(0x071A)) * 256 + i64::from(core.peek(0x071C));
        let (mut right, mut left, mut rdeep, mut ldeep, mut active) =
            (false, false, false, false, 0);
        for k in 0..5u32 {
            let flag = core.peek(0x0F + k);
            let wx = i64::from(core.peek(0x6E + k)) * 256 + i64::from(core.peek(0x87 + k));
            let rel = wx - cam;
            if flag != 0 {
                active += 1;
                right |= (256..256 + MARGIN).contains(&rel);
                left |= (-MARGIN..0).contains(&rel);
                let deep = (DEEP_RIGHT..256 + MARGIN).contains(&rel)
                    || (-MARGIN..DEEP_LEFT).contains(&rel);
                rdeep |= (DEEP_RIGHT..256 + MARGIN).contains(&rel);
                ldeep |= (-MARGIN..DEEP_LEFT).contains(&rel);
                if prev_flag[k as usize] == 0 {
                    p.spawns += 1;
                    p.late_spawns += u32::from(rel < 224);
                }
                let slot = &mut check[k as usize];
                let (x0, age) = slot.unwrap_or((wx, 0));
                let age = age + 1;
                p.deep_enemy_frames += u32::from(deep);
                *slot = Some(if age == 8 {
                    // A window of 8 frames ended: judged only if it ended deep.
                    if deep {
                        p.deep_windows += 1;
                        p.deep_moving += u32::from(x0 != wx);
                        if rel >= DEEP_RIGHT {
                            p.rdeep_windows += 1;
                            p.rdeep_moving += u32::from(x0 != wx);
                        }
                    }
                    (wx, 0)
                } else {
                    (x0, age)
                });
            } else {
                check[k as usize] = None;
            }
            prev_flag[k as usize] = flag;
        }
        p.right_margin += u32::from(right);
        p.left_margin += u32::from(left);
        p.right_deep += u32::from(rdeep);
        p.left_deep += u32::from(ldeep);
        p.all_full_frames += u32::from(active == 5);
        p.max_active = p.max_active.max(active);
        if rdeep || ldeep {
            run += 1;
            p.deep_run = p.deep_run.max(run);
        } else {
            run = 0;
        }
    }
    p
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn the_recipe_resolves_from_any_recording_and_edits_exactly_three_bytes() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let before = raw.clone();
    // A short recording (the first enemy has just appeared) and a long one
    // in another level must resolve to the same three PRG offsets.
    let short = resolve(&raw, &atlas_of_unpatched(&raw, 700));
    let long_log = record(&raw, script_two, Some((3, 0)), 2200);
    let long = resolve_patches(&build_nes_atlas(&raw, &long_log).unwrap(), &recipe(), MOD)
        .expect("resolves from 4-1 coverage");
    assert_eq!(short, long, "the same rows from a different recording");
    assert_eq!(short.len(), 3);
    for p in &short {
        println!(
            "{}: PRG offset {:#x} -> {:#04x}",
            p.name, p.prg_offset, p.value
        );
    }
    let patched = mod_image(&raw, &short);
    assert_eq!(raw, before, "the base image is never mutated");
    assert_eq!(patched.len(), raw.len());
    let diff: Vec<usize> = (0..raw.len()).filter(|&i| raw[i] != patched[i]).collect();
    assert_eq!(diff.len(), 3, "exactly the three bytes differ");
    assert!(
        rf_cart::Cartridge::load(&patched).is_ok(),
        "the patched image still loads"
    );
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn mod_off_is_the_plain_game_bit_for_bit() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    // Mod off: the image is the untouched base; resolving and applying an
    // EMPTY edit list changes nothing, and two runs agree on every frame.
    let same = apply_prg_bytes(&raw, &[]).expect("applies");
    assert_eq!(same, raw);
    let a = play(&raw, script_one, None, 1500, false);
    let b = play(&same, script_one, None, 1500, false);
    assert_eq!(a.wram_hash, b.wram_hash, "WRAM after every frame");
    assert_eq!((a.spawns, a.best_x), (b.spawns, b.best_x));
    // And the mod is not a no-op: with it on the run is different.
    let atlas = atlas_of_unpatched(&raw, 700);
    let on = mod_image(&raw, &resolve(&raw, &atlas));
    let c = play(&on, script_one, None, 1500, false);
    assert_ne!(a.wram_hash, c.wram_hash, "the mod changes the simulation");
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn level_one_enemies_stay_active_in_the_margins_with_the_mod_on() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let atlas = atlas_of_unpatched(&raw, 700);
    let on_img = mod_image(&raw, &resolve(&raw, &atlas));
    println!("1-1 real play, margin {MARGIN} px each side (deep bands: right >= {DEEP_RIGHT}, left < {DEEP_LEFT})");
    println!("(moving = windows of 8 frames ending deep in which the enemy's level x changed; a death freezes every enemy, so the unhurt runs are the fair ones)");
    println!("variant       script  inplay  rmargin lmargin  rdeep ldeep  deepEnemyFrames windows moving  longestRun  spawns  full  best_x deaths");
    let mut off_deep = 0;
    let mut on_best = Play::default();
    let mut on_one = Play::default();
    for (name, sc, unhurt, frames) in [
        ("plain", script_one as Script, false, 2200u32),
        ("plain", script_two, true, 3200),
    ] {
        for (label, img) in [("mod-off", &raw), ("mod-on ", &on_img)] {
            let p = play(img, sc, None, frames, unhurt);
            println!(
                "{label:<9} {name} {:>8} {:>7} {:>7} {:>7} {:>6} {:>5} {:>15} {:>7} {:>6} {:>10} {:>7} {:>5} {:>7} {:>6}",
                if unhurt { "two+safe" } else { "one" },
                p.frames_in_play, p.right_margin, p.left_margin, p.right_deep, p.left_deep,
                p.deep_enemy_frames, p.deep_windows, p.deep_moving, p.deep_run, p.spawns, p.all_full_frames,
                p.best_x, p.deaths
            );
            if unhurt {
                println!(
                    "          right-deep windows {} moving {}",
                    p.rdeep_windows, p.rdeep_moving
                );
            }
            if label == "mod-off" {
                off_deep += p.right_deep + p.left_deep;
            } else if unhurt {
                on_best = p;
            } else {
                on_one = p;
            }
        }
    }
    assert_eq!(off_deep, 0, "mod-off keeps no enemy in the deep margin");
    assert!(on_best.right_deep >= 60, "right: {on_best:?}");
    assert!(on_best.left_deep >= 60, "left: {on_best:?}");
    assert!(
        on_one.right_deep >= 60 && on_one.left_deep >= 60,
        "unassisted real play too: {on_one:?}"
    );
    assert!(on_best.deep_run >= 30, "a sustained run: {on_best:?}");
    assert!(
        on_best.deep_moving * 2 > on_best.deep_windows,
        "alive and moving, not parked: {on_best:?}"
    );
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn slot_pressure_over_three_levels() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let atlas = atlas_of_unpatched(&raw, 700);
    let patches = resolve(&raw, &atlas);
    // The shipped mod, plus the same mod with a shorter activation lead (the
    // bound a recipe author could pick): 0x30 is the game's own lead.
    let with_lead = |lead: u8| {
        let mut p = patches.clone();
        for row in p.iter_mut().filter(|r| r.name == "activation-lead") {
            row.value = lead;
        }
        mod_image(&raw, &p)
    };
    println!("(script two, Mario unhurt, 3200 frames; spawns = slot flag 0->1; late = came alive inside the 256-px screen)");
    println!("level  variant      spawns  late  maxActive  allFiveFull  best_x  deaths");
    for (w, l) in [(0u8, 0u8), (0, 1), (3, 0)] {
        let off = play(&raw, script_two, Some((w, l)), 3200, true);
        let mut rows = vec![("off (game's own)".to_string(), off.clone())];
        for lead in [0x80u8, 0xB0, 0xF0] {
            let on = play(&with_lead(lead), script_two, Some((w, l)), 3200, true);
            rows.push((format!("lead {} px", u32::from(lead)), on));
        }
        for (label, p) in &rows {
            println!(
                "{}-{}    {label:<16} {:>6}  {:>4}  {:>9}  {:>11}  {:>6}  {:>6}",
                w + 1,
                l + 1,
                p.spawns,
                p.late_spawns,
                p.max_active,
                p.all_full_frames,
                p.best_x,
                p.deaths
            );
        }
        assert!(off.spawns > 0 && rows.iter().all(|(_, p)| p.spawns > 0));
    }
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn lockstep_rebuilt_vs_plain_on_the_patched_image_has_zero_divergence() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let unpatched_atlas = atlas_of_unpatched(&raw, 700);
    let patched = mod_image(&raw, &resolve(&raw, &unpatched_atlas));
    // The atlas of the patched image itself (the app's Rebuild panel builds
    // it from the patched session's own recording)...
    let own = build_nes_atlas(&patched, &record(&patched, script_two, None, 2200)).expect("atlas");
    // ...and the unpatched image's atlas on the patched run (the executor
    // reads bytes from the real bus, so a stale atlas must not diverge either).
    for (label, atlas) in [
        ("patched-image atlas", &own),
        ("unpatched atlas", &unpatched_atlas),
    ] {
        let frames = 2200;
        let mut deep = 0u32;
        let mut hook = |_f: u32, side: Side, core: &mut NesCore| {
            if side == Side::Rebuilt {
                let cam = i64::from(core.peek(0x071A)) * 256 + i64::from(core.peek(0x071C));
                for k in 0..5u32 {
                    let rel =
                        i64::from(core.peek(0x6E + k)) * 256 + i64::from(core.peek(0x87 + k)) - cam;
                    if core.peek(0x0F + k) != 0
                        && !(DEEP_LEFT..DEEP_RIGHT).contains(&rel)
                        && core.peek(0x0770) == 1
                    {
                        deep += 1;
                    }
                }
            }
        };
        let r = run(
            &patched,
            atlas,
            inputs(script_two, frames),
            Options {
                before_frame: Some(&mut hook),
                ..Options::default()
            },
        )
        .expect("lockstep runs");
        println!(
            "{label}: {} frames, divergence {:?}, {:.3}% fallback, deep-margin enemy-frames seen {deep}",
            r.frames_run,
            r.divergence.as_ref().map(|d| d.frame),
            r.stats.fallback_rate() * 100.0
        );
        assert!(r.divergence.is_none(), "{label}: diverged: {:?}", r.bundle);
        assert_eq!(r.frames_run, frames);
        assert!(deep > 0, "the run actually exercised the mod");
    }
}
