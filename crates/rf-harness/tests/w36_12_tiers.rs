//! **W36-12: per-frame cost of the three executors on the SMB atlas**
//! (ROM-gated, law 5; prints numbers only, no ROM bytes).
//!
//! One scripted run of Super Mario Bros. (the W36-03 drive script: two
//! Starts, then Right with jumps) is recorded, the atlas built from it, and
//! the same 1,700 frames are then played under four configurations:
//!
//! 1. interpreter, no atlas (the reference),
//! 2. block executor (W36-03: atlas map plus the interpreter),
//! 3. block executor plus the IR tier,
//! 4. IR tier alone.
//!
//! Every frame of every configuration is timed; the configurations are
//! interleaved over several repetitions so a loaded machine drifts across all
//! of them equally, and the per-frame median and mean are reported. The
//! video hash of each run must equal the reference's: a faster-looking tier
//! that changed a pixel would be a bug, not a result.
//!
//! ```text
//! cargo test --release -p rf-harness --test w36_12_tiers -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_nes::core::NesCore;
use rf_nes::ExecTier;

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const RIGHT: u8 = 1 << 7;
const FRAMES: u32 = 1700;
/// Frames before this are menus and the intro; timing starts here.
const TIMED_FROM: u32 = 500;
const REPS: usize = 3;

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

fn script(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => RIGHT | A,
        _ => RIGHT,
    }
}

/// FNV-1a over every pixel index and audio sample the core emits.
struct HashSink(u64);

impl HashSink {
    fn feed(&mut self, b: u8) {
        self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
}

impl CoreSink for HashSink {
    fn video_scanline(&mut self, y: u16, p: &[PpuPixel]) {
        self.feed(y as u8);
        for px in p {
            self.feed(px.palette_index);
        }
    }
    fn audio(&mut self, s: &[i16]) {
        for v in s {
            self.feed(*v as u8);
        }
    }
    fn event(&mut self, _e: CoreEvent) {}
}

struct Run {
    per_frame: Vec<Duration>,
    hash: u64,
}

fn play(raw: &[u8], atlas: Option<&rf_nes::rebuild::Atlas>, tier: ExecTier) -> Run {
    let mut core = NesCore::from_ines_bytes(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_exec_tier(tier);
    if let Some(a) = atlas {
        core.set_rebuild(Some(a.clone())).expect("atlas fits");
    }
    let mut sink = HashSink(0xcbf2_9ce4_8422_2325);
    let mut per_frame = Vec::new();
    for f in 0..FRAMES {
        let mut input = InputFrame::empty();
        input.ports[0] = u16::from(script(f));
        let t = Instant::now();
        core.run_frame(&input, &mut sink);
        let dt = t.elapsed();
        if f >= TIMED_FROM {
            per_frame.push(dt);
        }
    }
    Run {
        per_frame,
        hash: sink.0,
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR); timing, run alone"]
fn smb_per_frame_cost_of_each_executor() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);

    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    let mut null = HashSink(0);
    for f in 0..FRAMES {
        let mut input = InputFrame::empty();
        input.ports[0] = u16::from(script(f));
        core.run_frame(&input, &mut null);
    }
    let log: CoverageLog = core.take_coverage().expect("armed");
    let atlas = build_nes_atlas(&raw, &log).expect("atlas");
    println!(
        "atlas: {} functions, {} blocks",
        atlas.functions.len(),
        atlas.blocks.len()
    );

    let configs: [(&str, bool, ExecTier); 4] = [
        ("interpreter", false, ExecTier::Interpreter),
        ("block executor (W36-03)", true, ExecTier::Interpreter),
        ("block executor + IR", true, ExecTier::Ir),
        ("IR, no atlas", false, ExecTier::Ir),
    ];
    let mut all: Vec<Vec<Duration>> = vec![Vec::new(); configs.len()];
    let mut reference_hash = None;
    for rep in 0..REPS {
        for (i, (name, use_atlas, tier)) in configs.iter().enumerate() {
            let r = play(&raw, use_atlas.then_some(&atlas), *tier);
            match reference_hash {
                None => reference_hash = Some(r.hash),
                Some(h) => assert_eq!(r.hash, h, "{name}, rep {rep}: output differs"),
            }
            all[i].extend(r.per_frame);
        }
    }
    println!("{FRAMES} frames per run, timed from frame {TIMED_FROM}, {REPS} interleaved reps");
    println!("executor                    median ms/frame   mean ms/frame   p95 ms/frame");
    let mut base = None;
    for ((name, _, _), frames) in configs.iter().zip(all.iter_mut()) {
        frames.sort();
        let n = frames.len();
        let median = ms(frames[n / 2]);
        let mean = ms(frames.iter().sum::<Duration>()) / n as f64;
        let p95 = ms(frames[n * 95 / 100]);
        let rel = base.map_or(String::new(), |b: f64| {
            format!("  ({:+.1}%)", (median / b - 1.0) * 100.0)
        });
        base.get_or_insert(median);
        println!("{name:<27} {median:>14.3} {mean:>15.3} {p95:>14.3}{rel}");
    }
}
