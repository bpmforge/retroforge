//! Ticket W30-01: the Super FX overclock on real cartridges, by
//! observation.
//!
//! ROM-gated, like the profile census: `RF_SUPERFX_ROMS` is a `:`-separated
//! list of Super FX dumps (bare or zipped). Unset, it skips. Each ROM runs
//! `RF_SUPERFX_FRAMES` frames (default 1200, about 20 s of its attract
//! sequence, with Start pressed every 2 s) at 100% and at each overclock in
//! `RF_SUPERFX_PCTS` (default `200,400`), and prints the GSU opcodes run
//! and how many of those frames differed from the one before — the
//! game's own picture rate. Nothing here can say whether a game *plays*
//! too fast; that is a by-eye check, recorded in the ticket.
//!
//! What it does assert: 100% is the cartridge's own clock (the run equals
//! one that never touched the setting), an overclock never runs less GSU
//! work, and at least one listed cartridge gets more. A game whose GSU
//! finishes every job inside its frame anyway (Yoshi's Island's opening,
//! measured 2026-10-14) runs the identical opcode count at 200%: the chip
//! was not the bottleneck, so there is nothing to speed up.

use std::io::Read;
use std::path::Path;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, PpuPixel, Step};
use rf_snes::core::SnesCore;

const ROMS_VAR: &str = "RF_SUPERFX_ROMS";

/// Hashes each frame's indexed pixels so consecutive frames can be compared.
#[derive(Default)]
struct FrameHasher {
    current: u64,
}

impl FrameHasher {
    fn take(&mut self) -> u64 {
        std::mem::replace(&mut self.current, 0xcbf2_9ce4_8422_2325)
    }
}

impl CoreSink for FrameHasher {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        // FNV-1a over the line number and every pixel's palette index.
        let mut h = self.current ^ u64::from(y);
        for p in pixels {
            h = (h ^ u64::from(p.palette_index)).wrapping_mul(0x100_0000_01b3);
        }
        self.current = h;
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

struct Run {
    gsu_ops: u64,
    changed_frames: u32,
    wram: Vec<u8>,
}

fn run(bytes: &[u8], percent: Option<u16>, frames: u32) -> Run {
    let mut core = SnesCore::load(bytes).expect("Super FX dump loads");
    assert!(
        core.has_gsu(),
        "RF_SUPERFX_ROMS lists a cartridge without a Super FX"
    );
    if let Some(p) = percent {
        core.set_gsu_overclock(p);
    }
    let mut sink = FrameHasher::default();
    sink.take();
    let mut last = 0;
    let mut changed_frames = 0;
    for f in 0..frames {
        // Start held for 4 frames every 2 s: enough to get a game past a
        // title that waits for it, the same opening the census uses.
        core.system_mut().bus.joypads.ports[0] = if f % 120 < 4 { 1 << 12 } else { 0 };
        let _ = core.step(Step::Frame, &mut sink);
        let h = sink.take();
        if h != last {
            changed_frames += 1;
        }
        last = h;
    }
    let system = core.system();
    Run {
        gsu_ops: system
            .bus
            .gsu
            .as_ref()
            .map_or(0, |g| g.regs.instructions_executed()),
        changed_frames,
        wram: system.bus.wram.to_vec(),
    }
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

#[test]
fn superfx_overclock_on_real_cartridges() {
    let Ok(list) = std::env::var(ROMS_VAR) else {
        eprintln!("SKIP: {ROMS_VAR} unset — set it to Super FX dumps to run this check");
        return;
    };
    let frames: u32 = std::env::var("RF_SUPERFX_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1200);
    let pcts: Vec<u16> = std::env::var("RF_SUPERFX_PCTS")
        .unwrap_or_else(|_| "200,400".into())
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    let mut best_ratio = 0.0f64;
    for path in list.split(':').filter(|p| !p.is_empty()) {
        let name = Path::new(path)
            .file_name()
            .map_or(path.into(), |n| n.to_string_lossy());
        let Some(bytes) = rom_bytes(Path::new(path)) else {
            println!("{name}: no cartridge found, skipped");
            continue;
        };
        let untouched = run(&bytes, None, frames);
        let base = run(&bytes, Some(100), frames);
        assert_eq!(
            (untouched.gsu_ops, &untouched.wram),
            (base.gsu_ops, &base.wram),
            "{name}: 100% must be the cartridge's own clock"
        );
        println!(
            "{name}: 100%: {} GSU ops, {}/{frames} frames changed",
            base.gsu_ops, base.changed_frames
        );
        for &pct in &pcts {
            let r = run(&bytes, Some(pct), frames);
            println!(
                "{name}: {pct}%: {} GSU ops ({:.2}x), {}/{frames} frames changed",
                r.gsu_ops,
                r.gsu_ops as f64 / base.gsu_ops.max(1) as f64,
                r.changed_frames
            );
            assert!(
                r.gsu_ops >= base.gsu_ops,
                "{name}: {pct}% ran less GSU work than 100%"
            );
            best_ratio = best_ratio.max(r.gsu_ops as f64 / base.gsu_ops.max(1) as f64);
        }
    }
    assert!(
        best_ratio > 1.5 || pcts.is_empty(),
        "no listed cartridge got more GSU work from the overclock (best {best_ratio:.2}x)"
    );
}
