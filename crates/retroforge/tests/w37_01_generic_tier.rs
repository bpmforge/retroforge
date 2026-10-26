//! **W37-01: Rebuild on a cold ROM, in lockstep across atlas swaps**
//! (ROM-gated, law 5; ignored, release).
//!
//! The shell turns Rebuild on with no cache and no recording: the first atlas
//! is the interrupt vectors alone, the coverage recorder runs underneath, and
//! every so often the atlas is rebuilt from what was recorded and swapped in
//! at a frame boundary. The executor is the interpreter with a map, so none
//! of that may change what the machine does. These tests run the real
//! sequence (`retroforge::rebuild::build_for`, the function the background
//! job calls; `set_rebuild_atlas`, the call the core thread makes) on the
//! rebuilt core of the lockstep checker, from power-on, over the W36 drive
//! scripts, and compare every frame against a plain core.
//!
//! Builds run inline here (the lockstep hook is synchronous); that moves the
//! swap frames, not the semantics: the core-thread tests in
//! `core_thread.rs` cover the thread hand-off, and a swap lands at a frame
//! boundary either way. Only counts, times and percentages are printed (D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w37_01_generic_tier -- --ignored --nocapture smb_level_one
//! cargo test --release -p retroforge --test w37_01_generic_tier -- --ignored --nocapture smw
//! cargo test --release -p retroforge --test w37_01_generic_tier -- --ignored --nocapture smb_all_32
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{EmulatorCore, InputFrame, RebuildStats};
use rf_harness::lockstep::{Options, Side};
use rf_input::SnesButton;
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_rebuild::Atlas;
use rf_snes::core::SnesCore;

/// Only for the shared SNES script constants.
#[allow(dead_code, unused_imports)]
#[path = "w33_07_common/mod.rs"]
mod discovery;

/// Frames between rebuild checks. The shell checks every 300 (5 s); the
/// tests check more often to see more swaps in a short run.
const GROW_EVERY: u32 = 120;

fn rom(console: &str, file: &str) -> Option<Vec<u8>> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p: PathBuf = Path::new(&root).join(console).join(file);
    p.exists()
        .then(|| retroforge::rom_open::load_rom_bytes(&p).expect("rom loads"))
}

fn insns(a: &Atlas) -> u64 {
    u64::from(a.stats.observed_insns) + u64::from(a.stats.inferred_insns)
}

/// The shell's swap guard (`rebuild::observed_of`): recorded code only grows.
fn observed(a: &Atlas) -> u64 {
    u64::from(a.stats.observed_insns)
}

/// One epoch: what a single atlas did before the next swap.
struct Epoch {
    frame: u32,
    functions: u32,
    insns: u64,
    stats: RebuildStats,
}

/// The shell's growth policy, driven from the lockstep hook.
struct Grower {
    raw: Vec<u8>,
    acc: Option<CoverageLog>,
    atlas: Atlas,
    last_growth: usize,
    swaps: u32,
    builds: Vec<Duration>,
    epochs: Vec<Epoch>,
    first_build: Duration,
}

impl Grower {
    /// Cold start: the vectors-only atlas.
    fn cold(raw: &[u8]) -> Self {
        let t = Instant::now();
        let atlas = retroforge::rebuild::build_for(raw, &CoverageLog::new(0)).expect("builds");
        Grower {
            raw: raw.to_vec(),
            acc: None,
            atlas,
            last_growth: 0,
            swaps: 0,
            builds: Vec::new(),
            epochs: Vec::new(),
            first_build: t.elapsed(),
        }
    }

    /// Per-frame hook for the rebuilt core. `trace` re-enables the divergence
    /// trace on the new executor.
    fn on_frame<C: EmulatorCore>(&mut self, frame: u32, core: &mut C, trace: impl FnOnce(&mut C)) {
        if frame == 0 {
            core.set_coverage_recording(true);
            return;
        }
        if !frame.is_multiple_of(GROW_EVERY) {
            return;
        }
        if let Some(t) = core.take_coverage() {
            match self.acc.as_mut() {
                Some(a) => {
                    a.merge(&t);
                }
                None => self.acc = Some(t),
            }
        }
        let Some(acc) = self.acc.as_ref() else { return };
        let growth = acc.opcode_prg_bytes() + acc.indirect.len() + acc.windows.len();
        if growth == self.last_growth {
            return;
        }
        self.last_growth = growth;
        let t = Instant::now();
        let atlas = retroforge::rebuild::build_for(&self.raw, acc).expect("builds");
        self.builds.push(t.elapsed());
        if observed(&atlas) < observed(&self.atlas) {
            return;
        }
        self.epochs.push(Epoch {
            frame,
            functions: self.atlas.functions.len() as u32,
            insns: insns(&self.atlas),
            stats: core.rebuild_stats().unwrap_or_default(),
        });
        core.set_rebuild_atlas(Some(Box::new(atlas.clone())))
            .expect("the swap is accepted");
        trace(core);
        self.atlas = atlas;
        self.swaps += 1;
    }

    fn report(&self, label: &str, last: RebuildStats) {
        println!(
            "{label}: vectors-only atlas {} functions / {} insns in {:?}; {} swaps; \
             background builds {:?}",
            self.epochs
                .first()
                .map_or(self.atlas.functions.len() as u32, |e| e.functions),
            self.epochs.first().map_or(insns(&self.atlas), |e| e.insns),
            self.first_build,
            self.swaps,
            self.builds
        );
        println!("  frame  atlas-fns  atlas-insns  instructions  fallback%  (the atlas that ran up to this frame)");
        for e in &self.epochs {
            println!(
                "  {:>5}  {:>9}  {:>11}  {:>12}  {:>8.3}",
                e.frame,
                e.functions,
                e.insns,
                e.stats.instructions(),
                e.stats.fallback_rate() * 100.0
            );
        }
        println!(
            "  final  {:>9}  {:>11}  {:>12}  {:>8.3}",
            self.atlas.functions.len(),
            insns(&self.atlas),
            last.instructions(),
            last.fallback_rate() * 100.0
        );
    }

    /// Fallback rate of the first epoch with real traffic, if any.
    fn first_rate(&self) -> Option<f64> {
        self.epochs
            .iter()
            .find(|e| e.stats.instructions() > 10_000)
            .map(|e| e.stats.fallback_rate())
    }
}

// ---- SMB (NES) ---------------------------------------------------------------

const SMB: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const B: u8 = 1 << 1;
const RIGHT: u8 = 1 << 7;
const LEAD_IN: u32 = 500;
const PLAY: u32 = 1200;
const FRAMES: u32 = LEAD_IN + PLAY;
const FORCE_FROM: u32 = 380;
const FORCE_TO: u32 = 404;
const JUDGE: u32 = 600;

/// The W36-03 drive script one (two Starts, then Right with jumps).
fn script_one(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => RIGHT | A,
        _ => RIGHT,
    }
}

/// The W36-03 drive script two (run, long and short jumps).
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

fn smb_inputs(script: fn(u32) -> u8) -> Vec<InputFrame> {
    (0..FRAMES)
        .map(|f| {
            let mut i = InputFrame::empty();
            i.ports[0] = u16::from(script(f));
            i
        })
        .collect()
}

/// Test-harness level entry (as in W36-04): world and level, zero-indexed.
fn force_level(frame: u32, core: &mut NesCore, level: Option<(u8, u8)>) {
    if let Some((w, l)) = level {
        if (FORCE_FROM..=FORCE_TO).contains(&frame) {
            core.bus_mut().write(0x075F, w);
            core.bus_mut().write(0x0760, l);
        }
    }
}

struct RunOut {
    frames: u32,
    divergence: Option<u32>,
    stats: RebuildStats,
    at_judge: Option<(u8, u8, u8)>,
}

fn smb_run(g: &mut Grower, script: fn(u32) -> u8, level: Option<(u8, u8)>) -> RunOut {
    let mut at_judge = None;
    let raw = g.raw.clone();
    let atlas = g.atlas.clone();
    let mut hook = |frame: u32, side: Side, core: &mut NesCore| {
        force_level(frame, core, level);
        if side == Side::Rebuilt {
            g.on_frame(frame, core, |c| c.set_rebuild_trace(256));
            if frame == JUDGE {
                at_judge = Some((core.peek(0x075F), core.peek(0x0760), core.peek(0x0770)));
            }
        }
    };
    let r = rf_harness::lockstep::run(
        &raw,
        &atlas,
        smb_inputs(script),
        Options {
            before_frame: Some(&mut hook),
            ..Options::default()
        },
    )
    .expect("lockstep runs");
    RunOut {
        frames: r.frames_run,
        divergence: r.divergence.as_ref().map(|d| d.frame),
        stats: r.stats,
        at_judge,
    }
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn smb_level_one_cold_rebuild_has_zero_divergence_across_swaps() {
    let Some(raw) = rom("nes", SMB) else {
        println!("SKIP: {SMB} not found");
        return;
    };
    let mut g = Grower::cold(&raw);
    assert_eq!(g.atlas.stats.observed_insns, 0, "vectors only");
    let r = smb_run(&mut g, script_one, None);
    g.report("SMB 1-1 real play, cold", r.stats);
    println!(
        "  {} frames compared, divergence {:?}, in play {:?}",
        r.frames, r.divergence, r.at_judge
    );
    assert_eq!(r.divergence, None);
    assert_eq!(r.frames, FRAMES);
    assert_eq!(r.at_judge, Some((0, 0, 1)));
    assert!(g.swaps >= 3, "the atlas grew: {} swaps", g.swaps);
    let first = g.first_rate().expect("an epoch with traffic");
    println!(
        "  fallback: first epoch {:.3}% -> final epoch {:.3}%",
        first * 100.0,
        r.stats.fallback_rate() * 100.0
    );
    assert!(
        r.stats.fallback_rate() < first,
        "the fallback rate fell as the atlas grew"
    );
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR); many minutes in release"]
fn smb_all_32_levels_cold_rebuild_has_zero_divergence_across_swaps() {
    let Some(raw) = rom("nes", SMB) else {
        println!("SKIP: {SMB} not found");
        return;
    };
    let mut g = Grower::cold(&raw);
    let mut diverged = Vec::new();
    let mut unreached = Vec::new();
    let mut total_frames = 0u32;
    println!("level  script  reached  frames  div   fallback%  swaps-so-far  atlas-fns");
    for w in 0..8u8 {
        for l in 0..4u8 {
            for (name, script) in [("one", script_one as fn(u32) -> u8), ("two", script_two)] {
                let r = smb_run(&mut g, script, Some((w, l)));
                let reached = r.at_judge == Some((w, l, 1));
                println!(
                    "{}-{}    {name}     {:<7}  {:<6}  {:<4}  {:>8.3}  {:>12}  {:>9}",
                    w + 1,
                    l + 1,
                    if reached { "yes" } else { "NO" },
                    r.frames,
                    r.divergence.map_or("none".to_string(), |f| f.to_string()),
                    r.stats.fallback_rate() * 100.0,
                    g.swaps,
                    g.atlas.functions.len()
                );
                if !reached {
                    unreached.push((w + 1, l + 1));
                }
                if r.divergence.is_some() {
                    diverged.push((w + 1, l + 1, name));
                }
                total_frames += r.frames;
            }
        }
    }
    println!(
        "TOTAL: 64 runs, {total_frames} frames compared, {} swaps, {} background builds (max {:?}), \
         final atlas {} functions",
        g.swaps,
        g.builds.len(),
        g.builds.iter().max(),
        g.atlas.functions.len()
    );
    assert!(diverged.is_empty(), "divergences: {diverged:?}");
    assert!(unreached.is_empty(), "levels not reached: {unreached:?}");
}

// ---- SMW (SNES) --------------------------------------------------------------

const SMW: &str = "Super Mario World (USA).zip";
const GAME_MODE: u32 = 0x7E_0100;
const IN_LEVEL: u8 = 0x14;
const PLAY_FRAMES: usize = 1500;
const SECOND_PLAY: &str = "R+B30 R10 w10 L8 R+B20 R+Y40 R30 B12 R20 D6 R+B14 R22 B8 w30 \
S6 w40 S6 w20 R+B+Y50 R40 w6 L6 R+B24 R30";
const SECOND_MORE: &str = "R+B+Y28 R16 L10 R+B18 R30 B10 w4 R24 D4 R+B22 R28";

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
                    "A" => SnesButton::A.bit(),
                    "B" => SnesButton::B.bit(),
                    "X" => SnesButton::X.bit(),
                    "Y" => SnesButton::Y.bit(),
                    "S" => SnesButton::Start.bit(),
                    "E" => SnesButton::Select.bit(),
                    "U" => SnesButton::Up.bit(),
                    "D" => SnesButton::Down.bit(),
                    "L" => SnesButton::Left.bit(),
                    "R" => SnesButton::Right.bit(),
                    other => panic!("button {other}"),
                };
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

fn session(play: &str, more: &str) -> (Vec<InputFrame>, usize) {
    let to_level = frames_of(discovery::SMW_TO_LEVEL);
    let level_start = to_level.len();
    let mut pad = to_level;
    pad.extend(frames_of(play));
    let more = frames_of(more);
    while pad.len() < level_start + PLAY_FRAMES {
        pad.extend(&more);
    }
    let inputs = pad
        .iter()
        .map(|&mask| {
            let mut f = InputFrame::empty();
            f.ports[0] = mask;
            f
        })
        .collect();
    (inputs, level_start)
}

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR); minutes in release"]
fn smw_yoshis_island_one_cold_rebuild_has_zero_divergence_across_swaps() {
    let Some(raw) = rom("snes", SMW) else {
        println!("SKIP: {SMW} not found");
        return;
    };
    let mut g = Grower::cold(&raw);
    assert_eq!(g.atlas.stats.observed_insns, 0, "vectors only");
    println!(
        "vectors-only SMW atlas: {} functions, {} insns",
        g.atlas.functions.len(),
        insns(&g.atlas)
    );
    let scripts = [
        (
            "A (W36-08 script)",
            session(discovery::SMW_PLAY, "R+B14 R22"),
        ),
        ("B (different play)", session(SECOND_PLAY, SECOND_MORE)),
    ];
    let mut first_rate = None;
    for (label, (inputs, level_start)) in scripts {
        let mut in_level_at_start = None;
        let raw_c = g.raw.clone();
        let atlas = g.atlas.clone();
        let swaps_before = g.swaps;
        let mut hook = |frame: u32, side: Side, core: &mut SnesCore| {
            if side == Side::Rebuilt {
                g.on_frame(frame, core, |c| c.set_rebuild_trace(256));
                if frame as usize == level_start {
                    in_level_at_start = Some(core.peek(GAME_MODE) == IN_LEVEL);
                }
            }
        };
        let n = inputs.len();
        let r = rf_harness::lockstep_snes::run(
            &raw_c,
            &atlas,
            inputs,
            Options::<SnesCore> {
                before_frame: Some(&mut hook),
                ..Options::default()
            },
        )
        .expect("lockstep runs");
        g.report(&format!("SMW {label}"), r.stats);
        println!(
            "  {} frames compared, divergence {:?}, {} swaps in this script, in the level at its start: {:?}",
            r.frames_run,
            r.divergence.as_ref().map(|d| d.frame),
            g.swaps - swaps_before,
            in_level_at_start
        );
        assert!(r.divergence.is_none(), "{label}: {:?}", r.divergence);
        assert_eq!(r.frames_run as usize, n);
        assert_eq!(
            in_level_at_start,
            Some(true),
            "{label}: in Yoshi's Island 1"
        );
        first_rate = first_rate.or(g.first_rate());
        if let Some(first) = first_rate {
            println!(
                "  fallback: first epoch {:.3}% -> this script's final epoch {:.3}%",
                first * 100.0,
                r.stats.fallback_rate() * 100.0
            );
        }
    }
    assert!(g.swaps >= 3, "the atlas grew: {} swaps", g.swaps);
    let first = first_rate.expect("an epoch with traffic");
    let last = g.epochs.last().map_or(1.0, |e| e.stats.fallback_rate());
    assert!(last < first, "the fallback rate fell as the atlas grew");
}
