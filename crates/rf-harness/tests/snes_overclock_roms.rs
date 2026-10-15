//! Ticket W30-08 part A: the SNES CPU/SA-1 overclock and the idle ("tick")
//! signal on real cartridges, by observation.
//!
//! ROM-gated like `superfx_overclock.rs`: each test skips when its
//! environment variable is unset, and prints what it measured — nothing
//! here can say whether a game *plays* too fast, that is a by-eye check.
//!
//! - `RF_SA1_ROMS` (`:`-separated SA-1 dumps, bare or zipped): SA-1
//!   instructions run in `RF_SA1_FRAMES` frames (default 1200) at 100% and
//!   each of `RF_SA1_PCTS` (default `200,400`). Asserts 100% is the
//!   cartridge's own clock and that an overclock never runs fewer SA-1
//!   instructions.
//! - `RF_GOV_ROMS` (`:`-separated dumps): the census a speed governor needs
//!   to know whether `SnesCore::frame_idle` is a usable tick signal. 3600
//!   frames with Start held for 4 frames every 2 s; per second (60 frames)
//!   it prints how many frames reported idle, at CPU 100% and 200% — and,
//!   for an SA-1 cartridge, with the SA-1 at 100% and 200% instead.

use std::io::Read;
use std::path::Path;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, PpuPixel, Step};
use rf_snes::core::SnesCore;

/// Hashes each frame's indexed pixels (FNV-1a), so consecutive frames can
/// be compared: "is the picture still changing" is how the census tells an
/// idle game from a hung one. Sound is discarded.
struct FrameHasher {
    current: u64,
}

const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

impl FrameHasher {
    fn new() -> Self {
        Self { current: FNV_BASIS }
    }
    fn take(&mut self) -> u64 {
        std::mem::replace(&mut self.current, FNV_BASIS)
    }
}

impl CoreSink for FrameHasher {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        let mut h = self.current ^ u64::from(y);
        for p in pixels {
            h = (h ^ u64::from(p.palette_index)).wrapping_mul(0x100_0000_01b3);
        }
        self.current = h;
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

/// Start held for 4 frames every 2 s: enough to get a game past a title
/// that waits for it, the same opening the census uses.
fn press_start(core: &mut SnesCore, frame: u32) {
    core.system_mut().bus.joypads.ports[0] = if frame % 120 < 4 { 1 << 12 } else { 0 };
}

/// The cartridge inside `path` (bare or zipped).
fn rom_bytes(path: &Path) -> Option<Vec<u8>> {
    let raw = std::fs::read(path).ok()?;
    if !raw.starts_with(b"PK\x03\x04") {
        return Some(raw);
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(raw)).ok()?;
    for index in 0..archive.len() {
        let Ok(mut file) = archive.by_index(index) else {
            break;
        };
        if !file.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        if (&mut file).take(64 << 20).read_to_end(&mut buf).is_err() {
            continue;
        }
        if rf_cart::Cartridge::load(&buf).is_ok() {
            return Some(buf);
        }
    }
    None
}

fn list(var: &str) -> Option<Vec<String>> {
    let Ok(v) = std::env::var(var) else {
        eprintln!("SKIP: {var} unset");
        return None;
    };
    Some(
        v.split(':')
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect(),
    )
}

fn name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or(path.into(), |n| n.to_string_lossy().into_owned())
}

/// SA-1 instructions run in `frames` frames at `percent` (`None` = never
/// touched the setting), plus the final WRAM for the 100% identity check.
fn sa1_run(bytes: &[u8], percent: Option<u16>, frames: u32) -> (u64, Vec<u8>) {
    let mut core = SnesCore::load(bytes).expect("SA-1 dump loads");
    assert!(
        core.has_sa1(),
        "RF_SA1_ROMS lists a cartridge without an SA-1"
    );
    if let Some(p) = percent {
        core.set_sa1_overclock(p);
    }
    let mut sink = FrameHasher::new();
    for f in 0..frames {
        press_start(&mut core, f);
        let _ = core.step(Step::Frame, &mut sink);
    }
    let system = core.system();
    (
        system
            .bus
            .sa1
            .as_ref()
            .map_or(0, |s| s.instructions_executed),
        system.bus.wram.to_vec(),
    )
}

#[test]
fn sa1_overclock_on_real_cartridges() {
    let Some(paths) = list("RF_SA1_ROMS") else {
        return;
    };
    let frames: u32 = std::env::var("RF_SA1_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1200);
    let pcts: Vec<u16> = std::env::var("RF_SA1_PCTS")
        .unwrap_or_else(|_| "200,400".into())
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    for path in paths {
        let n = name(&path);
        let Some(bytes) = rom_bytes(Path::new(&path)) else {
            println!("{n}: no cartridge found, skipped");
            continue;
        };
        let untouched = sa1_run(&bytes, None, frames);
        let base = sa1_run(&bytes, Some(100), frames);
        assert_eq!(
            untouched, base,
            "{n}: 100% must be the cartridge's own clock"
        );
        println!("{n}: 100%: {} SA-1 instructions in {frames} frames", base.0);
        for &pct in &pcts {
            let r = sa1_run(&bytes, Some(pct), frames);
            println!(
                "{n}: {pct}%: {} SA-1 instructions ({:.2}x)",
                r.0,
                r.0 as f64 / base.0.max(1) as f64
            );
            assert!(r.0 >= base.0, "{n}: {pct}% ran fewer SA-1 instructions");
        }
    }
}

/// One census run: per-second idle-frame counts, S-CPU and SA-1.
struct Census {
    cpu_idle_per_second: Vec<u32>,
    sa1_idle_per_second: Vec<u32>,
    idle_cycle_fraction: f64,
    /// Frames whose picture differed from the one before.
    changed_frames: u32,
}

fn census(bytes: &[u8], cpu_pct: u16, sa1_pct: u16, frames: u32) -> Census {
    let mut core = SnesCore::load(bytes).expect("dump loads");
    core.set_cpu_overclock(cpu_pct);
    core.set_sa1_overclock(sa1_pct);
    let mut cpu = vec![0u32; (frames as usize).div_ceil(60)];
    let mut sa1 = vec![0u32; cpu.len()];
    let mut idle_cycles = 0u64;
    let mut sink = FrameHasher::new();
    let (mut last, mut changed_frames) = (0, 0);
    for f in 0..frames {
        press_start(&mut core, f);
        let _ = core.step(Step::Frame, &mut sink);
        let picture = sink.take();
        changed_frames += u32::from(picture != last);
        last = picture;
        let second = (f / 60) as usize;
        cpu[second] += u32::from(core.frame_idle());
        sa1[second] += u32::from(core.sa1_frame_idle());
        idle_cycles += core.frame_idle_cycles();
    }
    // A frame is 262 lines of 1364 master cycles (NTSC).
    let idle_cycle_fraction = idle_cycles as f64 / (f64::from(frames) * 357_368.0);
    Census {
        cpu_idle_per_second: cpu,
        sa1_idle_per_second: sa1,
        idle_cycle_fraction,
        changed_frames,
    }
}

#[test]
fn idle_signal_census_on_real_cartridges() {
    let Some(paths) = list("RF_GOV_ROMS") else {
        return;
    };
    let frames: u32 = std::env::var("RF_GOV_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    for path in paths {
        let n = name(&path);
        let Some(bytes) = rom_bytes(Path::new(&path)) else {
            println!("{n}: no cartridge found, skipped");
            continue;
        };
        let has_sa1 = SnesCore::load(&bytes).is_ok_and(|c| c.has_sa1());
        // Which setting to sweep: the SA-1 for an SA-1 cartridge (its
        // S-CPU is rarely the bottleneck), the 65816 otherwise.
        let runs: [(&str, u16, u16); 2] = if has_sa1 {
            [("SA-1 100%", 100, 100), ("SA-1 200%", 100, 200)]
        } else {
            [("CPU 100%", 100, 100), ("CPU 200%", 200, 100)]
        };
        for (label, cpu_pct, sa1_pct) in runs {
            let c = census(&bytes, cpu_pct, sa1_pct, frames);
            let total: u32 = c.cpu_idle_per_second.iter().sum();
            println!(
                "{n} [{label}] S-CPU idle frames {total}/{frames} ({:.0}% of master cycles idle), picture changed on {}/{frames} frames",
                c.idle_cycle_fraction * 100.0,
                c.changed_frames
            );
            println!("  per second (of 60): {:?}", c.cpu_idle_per_second);
            if has_sa1 {
                let sa1_total: u32 = c.sa1_idle_per_second.iter().sum();
                println!(
                    "  SA-1 idle frames {sa1_total}/{frames}: {:?}",
                    c.sa1_idle_per_second
                );
            }
        }
    }
}

/// One governed (or plain 100%) run: per-second tick counts, the
/// multiplier at the end of each second, and the governor's last status.
struct GovRun {
    ticks_per_second: Vec<u32>,
    multiplier_per_second: Vec<u16>,
    status: Option<rf_enhance::speed_governor::GovernorStatus>,
}

/// 3600 frames with the census Start pattern. `ceiling` `None` is plain
/// 100%; `Some` runs a [`SpeedGovernor`] with that ceiling on the SA-1 (for
/// an SA-1 cartridge) or the 65816, fed the S-CPU idle signal as its tick
/// and "the picture differs from the last frame" as its change flag — the
/// same observations the shell's core thread feeds it.
fn governed_run(bytes: &[u8], ceiling: Option<u16>, frames: u32) -> GovRun {
    use rf_enhance::speed_governor::{Observation, SpeedGovernor};
    let mut core = SnesCore::load(bytes).expect("dump loads");
    let on_sa1 = core.has_sa1();
    let mut governor = ceiling.map(|c| SpeedGovernor::new(c, None));
    let mut sink = FrameHasher::new();
    let mut last = 0;
    let mut ticks = vec![0u32; (frames as usize).div_ceil(60)];
    let mut multipliers = vec![100u16; ticks.len()];
    for f in 0..frames {
        press_start(&mut core, f);
        let _ = core.step(Step::Frame, &mut sink);
        let picture = sink.take();
        let obs = Observation {
            tick: core.frame_idle(),
            picture_changed: picture != last,
        };
        last = picture;
        let second = (f / 60) as usize;
        ticks[second] += u32::from(obs.tick);
        if let Some(g) = governor.as_mut() {
            let m = g.observe(obs);
            if on_sa1 {
                core.set_sa1_overclock(m);
            } else {
                core.set_cpu_overclock(m);
            }
            multipliers[second] = m;
        }
    }
    GovRun {
        ticks_per_second: ticks,
        multiplier_per_second: multipliers,
        status: governor.map(|g| g.status()),
    }
}

/// W30-08 part B: the speed governor on real cartridges. For each dump,
/// prints per-second tick counts at 100% and governed (ceiling 400%), the
/// learned target, and the multiplier timeline; then the two numbers that
/// matter — seconds below the target in each run, and seconds where the
/// governed run ticked ABOVE the target (it must never).
#[test]
fn speed_governor_on_real_cartridges() {
    let Some(paths) = list("RF_GOV_ROMS") else {
        return;
    };
    let frames: u32 = std::env::var("RF_GOV_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    for path in paths {
        let n = name(&path);
        let Some(bytes) = rom_bytes(Path::new(&path)) else {
            println!("{n}: no cartridge found, skipped");
            continue;
        };
        let plain = governed_run(&bytes, None, frames);
        let gov = governed_run(&bytes, Some(400), frames);
        let status = gov.status.expect("governed run reports");
        println!("{n}: 100% ticks/s : {:?}", plain.ticks_per_second);
        println!("{n}: governed 400% : {:?}", gov.ticks_per_second);
        println!("{n}: multiplier/s  : {:?}", gov.multiplier_per_second);
        println!(
            "{n}: learned target {:?} ({:?}), notice {:?}, final {}%",
            status.target, status.target_source, status.notice, status.multiplier
        );
        let Some(target) = status.target else {
            println!("{n}: no target learned, governor held 100%");
            assert!(
                gov.multiplier_per_second.iter().all(|m| *m == 100),
                "{n}: no target, no overclock"
            );
            continue;
        };
        let below = |run: &GovRun| {
            run.ticks_per_second
                .iter()
                .filter(|t| **t + 2 <= target)
                .count()
        };
        // Seconds where the plain run ticked at the target: the governed
        // run must not exceed it in the same second.
        let over = plain
            .ticks_per_second
            .iter()
            .zip(&gov.ticks_per_second)
            .filter(|(p, g)| **p >= target && **g > target + 1)
            .count();
        println!(
            "{n}: seconds below target {target}: 100% = {}, governed = {}; \
             governed ticked above target in {over} seconds that 100% hit it",
            below(&plain),
            below(&gov)
        );
        assert_eq!(
            over, 0,
            "{n}: the governor made the game faster than normal"
        );
    }
}
