//! **W37-05: savestate fuzzing on a real cartridge** (ROM-gated, law 5;
//! `#[ignore]`, release).
//!
//! Baseline = the W36-04 coverage (both drive scripts over all 32 levels)
//! plus the W37-00 SMB movies replayed at their best alignments. Seed
//! savestates are cut from the script drive (menu, title, a level start and a
//! mid-level frame for each of the 32 levels), plus three test-side pokes of a
//! seed (last life, a nearly-expired timer). Then `rf_harness::fuzz` runs from
//! a fixed seed under a stated budget and the union is compared with the
//! baseline: PRG coverage, atlas functions, and which RAM-fact states were
//! live in the segments that ran new code.
//!
//! RAM facts (SMB RAM map, Data Crystal; the profile lists lives/world/level):
//! `$0770` mode (0 title or demo, 1 play, 2 victory, 3 game over), `$0776`
//! pause flag bit 0, `$000E` player state (`6`/`$B` dying), `$001D` float
//! state (3 = sliding down the flagpole), `$07F8-$07FA` timer digits, `$075A`
//! lives, `$075F`/`$0760` world/level. Only counts leave the test (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w37_05_fuzz -- --ignored --nocapture
//! ```
//! The multicart menu seed is left out (its code is not SMB's) unless
//! `RF_W37_FUZZ_MENU` is set. Budget knobs: `RF_W37_FUZZ_EPISODES` (1500), `RF_W37_FUZZ_FRAMES`
//! (1500000), `RF_W37_FUZZ_SECS` (300), `RF_W37_FUZZ_SEED`.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rf_cache::Cache;
use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::{build_nes_atlas, store_atlas};
use rf_harness::fuzz::{self, Config, Entry, Hooks, Savestate};
use rf_harness::movie::{self, Movie};
use rf_harness::movie_replay::{plan, replay_nes, Plan};
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u16 = 1 << 3;
const LEAD_IN: u32 = 500;
const FRAMES: u32 = LEAD_IN + 1200;
const LEVEL_START: u32 = 520;
const MID_LEVEL: u32 = 1000;

fn roms_root() -> PathBuf {
    PathBuf::from(
        std::env::var("RF_SCREENS_ROMS_DIR")
            .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string()),
    )
}

fn rom() -> Option<Vec<u8>> {
    let bytes = std::fs::read(roms_root().join("nes").join(FILE)).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.by_ref().take(64 << 20).read_to_end(&mut buf).ok()?;
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Nes { .. })
        ) {
            return Some(buf);
        }
    }
    None
}

fn atlas_cache_dir() -> PathBuf {
    std::env::var("RF_W37_CACHE_DIR").map_or_else(
        |_| std::env::temp_dir().join("retroforge-w37-atlas-cache"),
        PathBuf::from,
    )
}

fn env_num<T: std::str::FromStr>(var: &str, default: T) -> T {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Null;

impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

// ---- the W36-04 drive scripts (the "before" coverage); same as W37-00 -----

fn script_one(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => 8,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => 0x80 | 1,
        _ => 0x80,
    }
}

fn script_two(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => 8,
        205..=399 | 405..=499 => 0,
        f => {
            let t = f - 500;
            let jump = t % 90 < 20 || t % 150 < 6;
            0x80 | 2 | u8::from(jump)
        }
    }
}

/// One script run with the level forced during the title's second Start
/// (test-side state setup, as w36_04_lockstep_smb); `grab` frames are cut
/// into savestates.
fn record(
    raw: &[u8],
    script: fn(u32) -> u8,
    level: Option<(u8, u8)>,
    grab: &[(u32, String)],
    seeds: &mut Vec<Entry>,
) -> CoverageLog {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for f in 0..FRAMES {
        if let (Some((w, l)), true) = (level, (380..=404).contains(&f)) {
            core.bus_mut().write(0x075F, w);
            core.bus_mut().write(0x0760, l);
        }
        let mut i = InputFrame::empty();
        i.ports[0] = u16::from(script(f));
        core.run_frame(&i, &mut Null);
        for (_, name) in grab.iter().filter(|g| g.0 == f) {
            seeds.push(Entry {
                name: name.clone(),
                state: Savestate::capture(&core).expect("captures"),
                parent: None,
            });
        }
    }
    core.take_coverage().expect("armed")
}

fn merge_into(union: &mut Option<CoverageLog>, log: &CoverageLog) {
    match union.as_mut() {
        Some(u) => {
            assert!(u.merge(log));
        }
        None => *union = Some(log.clone()),
    }
}

/// Both scripts over all 32 levels, plus the seeds.
fn baseline_and_seeds(raw: &[u8]) -> (CoverageLog, Vec<Entry>) {
    let mut union = None;
    let mut seeds = Vec::new();
    for w in 0..8u8 {
        for l in 0..4u8 {
            let mut grab = vec![
                (LEVEL_START, format!("{}-{} start", w + 1, l + 1)),
                (MID_LEVEL, format!("{}-{} mid", w + 1, l + 1)),
            ];
            if (w, l) == (0, 0) {
                grab.push((150, "multicart menu".to_string()));
                grab.push((390, "SMB title".to_string()));
            }
            let a = record(raw, script_one, Some((w, l)), &grab, &mut seeds);
            let b = record(raw, script_two, Some((w, l)), &[], &mut Vec::new());
            merge_into(&mut union, &a);
            merge_into(&mut union, &b);
        }
    }
    (union.expect("recorded"), seeds)
}

// ---- the W37-00 movies, replayed at their best alignments ----------------

fn laid_out(lead: &[u16], offset: i32, m: &Movie) -> Plan {
    match usize::try_from(offset) {
        Ok(d) => plan(lead, d, 0, m),
        Err(_) => plan(lead, 0, offset.unsigned_abs() as usize, m),
    }
}

/// Replay a movie until the first sign it left the movie (lives dropped, game
/// over, back to the title), as W37-00's heuristic; coverage up to there.
fn movie_coverage(raw: &[u8], m: &Movie, offset: i32) -> (CoverageLog, u32) {
    let lead: Vec<u16> = std::iter::repeat_n(0u16, 160)
        .chain(std::iter::repeat_n(START, 5))
        .collect();
    let pl = laid_out(&lead, offset, m);
    let (mut armed, mut lives) = (false, 0u8);
    let r = replay_nes(raw, &pl, true, |f, c| {
        let (mode, l) = (c.peek(0x0770), c.peek(0x075A));
        if !armed {
            if mode == 1 && f as usize >= pl.movie_start {
                armed = true;
                lives = l;
            }
            return true;
        }
        let bad = l < lives || mode == 3 || mode == 0;
        lives = l;
        !bad
    })
    .expect("loads");
    (r.coverage.expect("armed"), r.frames_run)
}

fn movies_baseline(raw: &[u8], into: &mut CoverageLog) -> Vec<String> {
    let mut notes = Vec::new();
    for (name, var, def, offset) in [
        (
            "warpless",
            "RF_W37_SMB_WARPLESS",
            "3728/x/happylee_mars608-smb-warpless.fm2",
            20,
        ),
        (
            "warps",
            "RF_W37_SMB_WARPS",
            "1715/x/happylee-supermariobros,warped.fm2",
            -1,
        ),
    ] {
        let p = std::env::var(var).map_or_else(
            |_| roms_root().join("tas").join(def),
            std::path::PathBuf::from,
        );
        let Ok(text) = std::fs::read_to_string(&p) else {
            notes.push(format!("{name}: movie not found, skipped"));
            continue;
        };
        let m = movie::fm2::parse(&text).expect("parses");
        let (log, frames) = movie_coverage(raw, &m, offset);
        assert!(into.merge(&log));
        notes.push(format!(
            "{name}: {frames} frames, movie alone {:.1}% of PRG",
            log.prg_coverage_fraction() * 100.0
        ));
    }
    notes
}

// ---- labels: RAM facts -----------------------------------------------------

const LABELS: [&str; 10] = [
    "title/demo (mode 0)",
    "play (mode 1)",
    "victory (mode 2)",
    "game over (mode 3)",
    "paused",
    "player dying",
    "flagpole slide",
    "timer at zero",
    "life lost this frame",
    "world/level changed",
];

#[derive(Default)]
struct Labeller {
    lives: u8,
    level: (u8, u8),
    primed: bool,
}

impl Labeller {
    fn bits(&mut self, c: &NesCore) -> u64 {
        let (mode, lives) = (c.peek(0x0770), c.peek(0x075A));
        let level = (c.peek(0x075F), c.peek(0x0760));
        let mut m = match mode {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 8,
            _ => 0,
        };
        if c.peek(0x0776) & 1 == 1 && mode == 1 {
            m |= 16;
        }
        if matches!(c.peek(0x000E), 6 | 0x0B) {
            m |= 32;
        }
        if c.peek(0x001D) == 3 {
            m |= 64;
        }
        if mode == 1 && (c.peek(0x07F8), c.peek(0x07F9), c.peek(0x07FA)) == (0, 0, 0) {
            m |= 128;
        }
        if self.primed {
            if lives < self.lives {
                m |= 256;
            }
            if level != self.level {
                m |= 512;
            }
        }
        (self.lives, self.level, self.primed) = (lives, level, true);
        m
    }
}

/// A seed from `from` with some RAM poked after it is restored (test-side
/// state setup, never shipped behaviour).
fn poked(raw: &[u8], from: &Entry, name: &str, pokes: &[(u16, u8)]) -> Entry {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.reset(ResetKind::Hard);
    from.state.restore(&mut core).expect("restores");
    for &(a, v) in pokes {
        core.bus_mut().write(a, v);
    }
    Entry {
        name: name.to_string(),
        state: Savestate::capture(&core).expect("captures"),
        parent: None,
    }
}

fn function_keys(a: &rf_rebuild::Atlas) -> BTreeSet<(u32, u8)> {
    a.functions
        .iter()
        .map(|f| (f.entry_rom, f.entry_mode))
        .collect()
}

#[test]
#[ignore = "needs the SMB multicart ROM; minutes in release"]
fn smb_savestate_fuzz_beats_scripts_and_movies() {
    let Some(raw) = rom() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let t = Instant::now();
    let (mut baseline, mut seeds) = baseline_and_seeds(&raw);
    println!(
        "W36 scripts (64 runs): PRG {:.1}% in {:.0?}, {} seeds cut",
        baseline.prg_coverage_fraction() * 100.0,
        t.elapsed(),
        seeds.len()
    );
    let scripts_only = baseline.prg_coverage_fraction();
    for n in movies_baseline(&raw, &mut baseline) {
        println!("movie: {n}");
    }
    let atlas_before = build_nes_atlas(&raw, &baseline).expect("atlas");
    println!(
        "baseline (scripts + movies): PRG {:.1}% (scripts alone {:.1}%), {} atlas functions",
        baseline.prg_coverage_fraction() * 100.0,
        scripts_only * 100.0,
        atlas_before.functions.len()
    );

    let start11 = seeds
        .iter()
        .find(|s| s.name == "1-1 start")
        .expect("1-1 start")
        .clone();
    let start41 = seeds
        .iter()
        .find(|s| s.name == "4-1 start")
        .expect("4-1 start")
        .clone();
    let extra = [
        poked(&raw, &start11, "1-1 last life", &[(0x075A, 0)]),
        poked(&raw, &start41, "4-1 last life", &[(0x075A, 0)]),
        poked(
            &raw,
            &start11,
            "1-1 timer nearly out",
            &[(0x07F8, 0), (0x07F9, 0), (0x07FA, 2)],
        ),
    ];
    seeds.extend(extra);
    // The multicart's game-select menu (and the other game behind it) is not
    // SMB: its code would inflate the gain, so it is a seed only on request.
    if std::env::var("RF_W37_FUZZ_MENU").is_err() {
        seeds.retain(|s| s.name != "multicart menu");
    }

    let cfg = Config {
        seed: env_num("RF_W37_FUZZ_SEED", Config::default().seed),
        max_episodes: env_num("RF_W37_FUZZ_EPISODES", 1500),
        max_frames: env_num("RF_W37_FUZZ_FRAMES", 1_500_000),
        ..Config::default()
    };
    let wall = Duration::from_secs(env_num("RF_W37_FUZZ_SECS", 300));
    println!(
        "fuzz budget: seed {:#x}, <= {} episodes, <= {} frames, <= {:.0?} wall, {}-{} frames/episode, \
         segment {}, {} seed savestates",
        cfg.seed,
        cfg.max_episodes,
        cfg.max_frames,
        wall,
        cfg.min_episode_frames,
        cfg.max_episode_frames,
        cfg.segment,
        seeds.len()
    );
    let mut lab = Labeller::default();
    let started = Instant::now();
    let mut label = |c: &NesCore| lab.bits(c);
    let mut stop = || started.elapsed() > wall;
    let mut progress = |s: &str| println!("{s} [{:.0?}]", started.elapsed());
    let before_bytes = baseline.executed_prg_bytes();
    let report = fuzz::fuzz(
        &raw,
        seeds.clone(),
        baseline.clone(),
        &cfg,
        &mut Hooks {
            label: &mut label,
            stop: &mut stop,
            progress: &mut progress,
        },
    )
    .expect("fuzz runs");

    let atlas_after = build_nes_atlas(&raw, &report.coverage).expect("atlas");
    let (kb, ka) = (function_keys(&atlas_before), function_keys(&atlas_after));
    let new_fns = ka.difference(&kb).count();
    let lost_fns = kb.difference(&ka).count();
    let h = report.halts;
    println!(
        "FUZZ DONE ({}): {} episodes, {} frames in {:.0?}; corpus {} ({} seeds + {} derived); {} finds",
        report.stopped_by,
        report.episodes,
        report.frames,
        started.elapsed(),
        report.corpus.len(),
        report.seeds,
        report.corpus.len() - report.seeds,
        report.finds.len()
    );
    println!(
        "PRG executed bytes {} -> {} (+{}); coverage {:.2}% -> {:.2}%",
        before_bytes,
        report.coverage.executed_prg_bytes(),
        report.new_prg_bytes,
        baseline.prg_coverage_fraction() * 100.0,
        report.coverage.prg_coverage_fraction() * 100.0
    );
    println!(
        "atlas functions {} -> {} ({} new entries, {} entries absorbed into larger functions)",
        atlas_before.functions.len(),
        atlas_after.functions.len(),
        new_fns,
        lost_fns
    );
    println!(
        "halts: jam {}, no-frame {}, frozen {}, panic {}; episodes with new RAM-executed code {}",
        h.jam, h.no_frame, h.frozen, h.panic, h.new_ram_exec_episodes
    );
    println!("new code by RAM-fact label (segments that ran new code, new bytes; bits overlap):");
    for (bit, g) in &report.label_gains {
        println!(
            "  {:<22} {:>4} segments {:>6} bytes",
            LABELS.get(*bit as usize).copied().unwrap_or("?"),
            g.segments,
            g.bytes
        );
    }
    let mut by_entry: std::collections::BTreeMap<String, (u32, usize)> = Default::default();
    for f in &report.finds {
        let e = by_entry
            .entry(report.corpus[f.entry].name.clone())
            .or_default();
        e.0 += 1;
        e.1 += f.new_bytes;
    }
    let mut top: Vec<_> = by_entry.into_iter().collect();
    top.sort_by_key(|(_, v)| std::cmp::Reverse(v.1));
    println!(
        "finds by start state (top 12): {:?}",
        &top[..top.len().min(12)]
    );

    // Replay a few finds and halts from their records: the fuzz is repeatable.
    let mut replayed = 0;
    for f in report.finds.iter().take(3) {
        let (log, _) = fuzz::replay(
            &raw,
            &report.corpus[f.entry].state,
            f.ep_seed,
            f.planned,
            f.frames_run,
        )
        .expect("replays");
        for &o in &f.sample {
            assert_ne!(log.prg_flags[o as usize], 0, "find replays");
        }
        replayed += 1;
    }
    for r in report.halt_records.iter().take(3) {
        let (_, halt) = fuzz::replay(
            &raw,
            &report.corpus[r.entry].state,
            r.ep_seed,
            r.planned,
            r.frames_run,
        )
        .expect("replays");
        assert_eq!(halt, Some(r.kind), "halt replays");
        replayed += 1;
    }
    println!("replayed {replayed} recorded episodes from their (state, seed): identical");

    let sha = rf_harness::hex_sha256(&raw);
    let dir = atlas_cache_dir();
    let mut cache = Cache::open(&dir, 64 << 20).expect("cache");
    store_atlas(&mut cache, &sha, &atlas_after).expect("stores");
    println!(
        "baked: {} functions, PRG {:.1}% into {}",
        atlas_after.functions.len(),
        report.coverage.prg_coverage_fraction() * 100.0,
        dir.display()
    );

    assert!(report.new_prg_bytes > 0, "fuzz must add PRG coverage");
    assert!(
        atlas_after.functions.len() > atlas_before.functions.len(),
        "fuzz must add atlas functions"
    );
    assert_eq!(h.panic, 0, "the core panicked on unseen code");
}
