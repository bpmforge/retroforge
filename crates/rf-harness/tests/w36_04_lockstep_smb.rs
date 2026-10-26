//! **W36-04: the lockstep checker on a real cartridge** (ROM-gated, law 5).
//!
//! 1. World 1-1 by real play (the W36-03 drive script: two Starts, then Right
//!    with jumps), atlas built from a recording of that run: the plain and
//!    rebuilt cores must agree after every frame.
//! 2. All 32 levels, 1-1 to 8-4. No whole-game input movie exists here, so
//!    each level is entered in THIS HARNESS ONLY by writing the observed
//!    world/level RAM (`$075F`/`$0760`, zero-indexed) into BOTH cores on every
//!    frame from 380 to 404, the stretch ending with the title screen's
//!    second Start (frames 400-404). Observed (see the per-level table the
//!    test prints): the game reads those bytes while it handles that Start, and
//!    the level that loads is the forced one (`$0750` and the nametables
//!    differ for all 32). Forcing later (frames 406-408, on the "WORLD x-y"
//!    intro screen) only moves the world, not the level, so the window has to
//!    reach the Start frame. This is test-side state setup, never shipped
//!    behaviour. One atlas is built from the union of coverage over all
//!    levels and both scripts, then each level runs in lockstep with both.
//!
//! Only counts and hashes are printed; no ROM bytes leave the test (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w36_04_lockstep_smb -- --ignored --nocapture
//! RF_W36_TIER=ir cargo test --release -p rf-harness --test w36_04_lockstep_smb -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, RebuildStats, ResetKind,
};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options, Side};
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const B: u8 = 1 << 1;
const RIGHT: u8 = 1 << 7;
/// 500 frames to get through both title screens plus the intro, then play.
const LEAD_IN: u32 = 500;
const PLAY: u32 = 1200;
const FRAMES: u32 = LEAD_IN + PLAY;
/// Frames on which the harness writes the level into RAM (module doc).
const FORCE_FROM: u32 = 380;
const FORCE_TO: u32 = 404;
/// Frame at which "reached" is judged and the level fingerprint is taken.
const JUDGE: u32 = 600;
const FINGERPRINT: u32 = 575;

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

/// Run (B) the whole way, jump 20 frames in every 90 and hop at the start of
/// every 150. Same as `w36_03_rebuild_smb`'s script two.
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

fn world_x(core: &NesCore) -> u16 {
    u16::from(core.peek(0x006D)) * 256 + u16::from(core.peek(0x0086))
}

fn fingerprint(core: &NesCore) -> u64 {
    core.state_view()
        .vram
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// What one scripted run saw, recorded by the hook on the rebuilt core.
#[derive(Default, Clone)]
struct Observed {
    at_judge: Option<(u8, u8, u8, u8)>, // world, level, mode ($0770), area type ($074E)
    print: u64,
    start_x: u16,
    best_x: u16,
    lives_last: u8,
}

struct LevelRun {
    frames: u32,
    divergence: Option<u32>,
    stats: RebuildStats,
    seen: Observed,
    bundle: Option<PathBuf>,
}

fn lockstep_level(
    raw: &[u8],
    atlas: &rf_nes::rebuild::Atlas,
    script: fn(u32) -> u8,
    level: Option<(u8, u8)>,
    tag: &str,
) -> LevelRun {
    let mut seen = Observed::default();
    let dir = std::env::temp_dir().join(format!("rf-w36-04-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tier = rebuilt_tier();
    let mut hook = |frame: u32, side: Side, core: &mut NesCore| {
        force_level(frame, core, level);
        if side == Side::Rebuilt {
            core.set_exec_tier(tier);
            assert_eq!(
                core.exec_tier(),
                tier,
                "the rebuilt core runs the chosen tier"
            );
        }
        if side == Side::Rebuilt {
            if frame == FINGERPRINT {
                seen.print = fingerprint(core);
            }
            if frame == JUDGE {
                seen.at_judge = Some((
                    core.peek(0x075F),
                    core.peek(0x0760),
                    core.peek(0x0770),
                    core.peek(0x074E),
                ));
                seen.start_x = world_x(core);
            }
            if frame > JUDGE {
                seen.best_x = seen.best_x.max(world_x(core));
            }
            seen.lives_last = core.peek(0x075A);
        }
    };
    let opts = Options {
        bundle_dir: Some(dir.clone()),
        before_frame: Some(&mut hook),
        ..Options::default()
    };
    let r = run(raw, atlas, inputs(script), opts).expect("lockstep runs");
    LevelRun {
        frames: r.frames_run,
        divergence: r.divergence.as_ref().map(|d| d.frame),
        stats: r.stats,
        seen,
        bundle: r.bundle.map(|b| b.dir),
    }
}

/// `RF_W36_TIER=ir` runs the rebuilt core on the IR tier (ticket W36-12); the
/// plain core always stays on the interpreter, the reference. Same test, same
/// scripts, same zero-divergence bar.
fn rebuilt_tier() -> rf_nes::ExecTier {
    if std::env::var("RF_W36_TIER").is_ok_and(|v| v == "ir") {
        rf_nes::ExecTier::Ir
    } else {
        rf_nes::ExecTier::Interpreter
    }
}

fn sum(a: RebuildStats, b: RebuildStats) -> RebuildStats {
    RebuildStats {
        observed: a.observed + b.observed,
        inferred: a.inferred + b.inferred,
        fallback: a.fallback + b.fallback,
        fallback_width: a.fallback_width + b.fallback_width,
        fallback_no_node: a.fallback_no_node + b.fallback_no_node,
        fallback_ram: a.fallback_ram + b.fallback_ram,
        fallback_other: a.fallback_other + b.fallback_other,
        interrupt_entries: a.interrupt_entries + b.interrupt_entries,
        indirect_checked: a.indirect_checked + b.indirect_checked,
        dispatch_misses: a.dispatch_misses + b.dispatch_misses,
        delta_len: a.delta_len.max(b.delta_len),
        delta_dropped: a.delta_dropped + b.delta_dropped,
        atlas_functions: a.atlas_functions.max(b.atlas_functions),
    }
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn smb_level_one_by_real_play_has_zero_divergence() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let log = record(&raw, script_one, None);
    let atlas = build_nes_atlas(&raw, &log).expect("atlas");
    let r = lockstep_level(&raw, &atlas, script_one, None, "real-1-1");
    println!(
        "1-1 real play: {} frames, divergence {:?}, {:.3}% fallback ({} instrs), Mario x {} -> {}",
        r.frames,
        r.divergence,
        r.stats.fallback_rate() * 100.0,
        r.stats.instructions(),
        r.seen.start_x,
        r.seen.best_x
    );
    assert_eq!(r.divergence, None, "bundle: {:?}", r.bundle);
    assert_eq!(r.frames, FRAMES);
    let (w, l, mode, _) = r.seen.at_judge.expect("judged");
    assert_eq!((w, l, mode), (0, 0, 1), "in play in 1-1");
    assert!(r.seen.best_x > r.seen.start_x + 200, "Mario got going");
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn smb_all_32_levels_have_zero_divergence() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let levels: Vec<(u8, u8)> = (0..8).flat_map(|w| (0..4).map(move |l| (w, l))).collect();

    let t = Instant::now();
    let mut union: Option<CoverageLog> = None;
    for &lv in &levels {
        for script in [script_one as fn(u32) -> u8, script_two] {
            let log = record(&raw, script, Some(lv));
            match union.as_mut() {
                Some(u) => {
                    u.merge(&log);
                }
                None => union = Some(log),
            }
        }
    }
    let union = union.expect("recorded");
    let atlas = build_nes_atlas(&raw, &union).expect("atlas");
    println!(
        "coverage union of 64 runs + atlas in {:?}: {} functions, {:.1}% of PRG executed",
        t.elapsed(),
        atlas.functions.len(),
        union.prg_coverage_fraction() * 100.0
    );

    println!("level  reached  mode  area  frames  div   fallback%   instrs   maxx(one/two)  lives");
    let mut total = RebuildStats::default();
    let mut total_frames = 0u32;
    let mut diverged = Vec::new();
    let mut unreached = Vec::new();
    let mut prints = std::collections::HashSet::new();
    for &(w, l) in &levels {
        let one = lockstep_level(&raw, &atlas, script_one, Some((w, l)), &format!("{w}-{l}a"));
        let two = lockstep_level(&raw, &atlas, script_two, Some((w, l)), &format!("{w}-{l}b"));
        let (jw, jl, mode, area) = one.seen.at_judge.unwrap_or((255, 255, 255, 255));
        let reached = (jw, jl, mode) == (w, l, 1);
        let stats = sum(one.stats, two.stats);
        let frames = one.frames + two.frames;
        let div = match (one.divergence, two.divergence) {
            (None, None) => "none".to_string(),
            (a, b) => format!("{a:?}/{b:?}"),
        };
        println!(
            "{}-{}    {:<7}  {:<4}  {:<4}  {:<6}  {:<4}  {:>8.4}  {:>9}  {:>5}/{:<5}    {}/{}",
            w + 1,
            l + 1,
            if reached { "yes" } else { "NO" },
            mode,
            area,
            frames,
            div,
            stats.fallback_rate() * 100.0,
            stats.instructions(),
            one.seen.best_x,
            two.seen.best_x,
            one.seen.lives_last,
            two.seen.lives_last
        );
        if !reached {
            unreached.push((w + 1, l + 1));
        }
        if one.divergence.is_some() || two.divergence.is_some() {
            diverged.push(((w + 1, l + 1), one.bundle.clone(), two.bundle.clone()));
        }
        prints.insert(one.seen.print);
        total = sum(total, stats);
        total_frames += frames;
    }
    println!(
        "TOTAL: {} levels x 2 scripts, {} frames compared, {} instructions, {:.4}% fallback \
         ({} fallback), {} indirect checked, {} dispatch misses, {} distinct level fingerprints",
        levels.len(),
        total_frames,
        total.instructions(),
        total.fallback_rate() * 100.0,
        total.fallback,
        total.indirect_checked,
        total.dispatch_misses,
        prints.len()
    );
    assert!(diverged.is_empty(), "divergences: {diverged:?}");
    assert!(unreached.is_empty(), "levels not reached: {unreached:?}");
    assert_eq!(
        prints.len(),
        levels.len(),
        "every level loaded something different"
    );
}
