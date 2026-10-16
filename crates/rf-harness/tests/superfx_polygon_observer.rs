//! Ticket W31-02: the Super FX polygon observer and the 1x reference
//! re-rasteriser, proven against the GSU's own pixels on Star Fox (USA).
//!
//! ROM-gated: `RF_SUPERFX_POLY_ROMS` is a `:`-separated list of Star Fox
//! dumps (bare or zipped); unset, every test here skips. The addresses come
//! from the shipped profile (`profiles/snes/star-fox`, `[superfx_polygons]`,
//! found by observation: `docs/research/W31-01-starfox-polygon-map.md`).
//!
//! For every render job the observer completes, `rasterise_job_with` draws
//! the recorded polygon stream and is compared against the GSU's bitmap at
//! `STOP`, on the pixels that job plotted (the bitmap also holds earlier
//! jobs' pixels, so the rest is not comparable): exact palette-index match
//! over those pixels, and pixel IoU of coverage (drawn vs plotted).
//!
//! Frame bounds: `RF_POLY_INTRO` (default `850:1100`, no input) and
//! `RF_POLY_ATTRACT` (default `2600:2800`, no input).

use std::io::Read;
use std::path::Path;

use rf_core_api::gsu_poly::PolyJob;
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, PpuPixel, Step};
use rf_renderer::gsu_poly::{rasterise_job_with, RightEdge, NONE};
use rf_snes::core::SnesCore;
use rf_snes::gsu_observer::GsuPolyConfig;

const ROMS_VAR: &str = "RF_SUPERFX_POLY_ROMS";

#[derive(Default)]
struct FrameHasher {
    current: u64,
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

fn roms() -> Option<Vec<(String, Vec<u8>)>> {
    let Ok(list) = std::env::var(ROMS_VAR) else {
        eprintln!("SKIP: {ROMS_VAR} unset - set it to Star Fox (USA) dumps to run this check");
        return None;
    };
    let out: Vec<_> = list
        .split(':')
        .filter(|p| !p.is_empty())
        .filter_map(|p| Some((p.to_owned(), rom_bytes(Path::new(p))?)))
        .collect();
    (!out.is_empty()).then_some(out)
}

/// The shipped Star Fox profile's `[superfx_polygons]` as an observer config.
fn star_fox_config(capture_bitmap: bool) -> GsuPolyConfig {
    let text = include_str!("../../../profiles/snes/star-fox/profile.toml");
    let sp = rf_profiles::loader::load_str(text)
        .expect("profile loads")
        .profile
        .superfx_polygons
        .expect("Star Fox declares [superfx_polygons]");
    let u16_of = |v: u32| u16::try_from(v).expect("validated by the loader");
    GsuPolyConfig {
        pc_bank: u8::try_from(sp.pc_bank).expect("bank"),
        count_read_pc: u16_of(sp.count_read_pc),
        count_addr: u16_of(sp.count_addr),
        buffer_addr: u16_of(sp.buffer_addr),
        walker_pcs: (
            u16_of(sp.walker_pcs[0]),
            u16_of(sp.walker_pcs[1].min(0xFFFF)),
        ),
        viewport: (u16_of(sp.viewport[0]), u16_of(sp.viewport[1])),
        capture_bitmap,
    }
}

fn frame_range(var: &str, default: (u32, u32)) -> (u32, u32) {
    std::env::var(var)
        .ok()
        .and_then(|v| {
            let (a, b) = v.split_once(':')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or(default)
}

#[derive(Default, Debug)]
struct Score {
    jobs: u64,
    polygons: u64,
    plots: u64,
    /// GSU-plotted pixels.
    plotted: u64,
    /// ... of which the re-rasteriser drew exactly the GSU's index.
    exact: u64,
    /// pixels drawn by both / by either (coverage IoU).
    both: u64,
    either: u64,
}

impl Score {
    fn add(&mut self, job: &PolyJob, rule: RightEdge) {
        let img = rasterise_job_with(job, job.viewport, rule);
        let bitmap = job.bitmap.as_ref().expect("capture_bitmap was on");
        for (i, &drawn) in job.plotted.iter().enumerate() {
            let mine = img[i] != NONE;
            self.either += u64::from(drawn || mine);
            self.both += u64::from(drawn && mine);
            if drawn {
                self.plotted += 1;
                self.exact += u64::from(img[i] == bitmap[i]);
            }
        }
    }
    fn exact_rate(&self) -> f64 {
        self.exact as f64 / self.plotted.max(1) as f64
    }
    fn iou(&self) -> f64 {
        self.both as f64 / self.either.max(1) as f64
    }
}

/// Run `bytes` with no input, the observer on from `from`, to `to`; return
/// every completed job and how many the 3-deep ring would have lost.
fn collect_jobs(bytes: &[u8], from: u32, to: u32) -> (Vec<std::sync::Arc<PolyJob>>, u64) {
    let mut core = SnesCore::load(bytes).expect("loads");
    let mut sink = FrameHasher::default();
    let mut jobs = Vec::new();
    let mut lost = 0u64;
    for f in 0..to {
        if f == from {
            core.set_gsu_poly_observer(Some(star_fox_config(true)));
        }
        let _ = core.step(Step::Frame, &mut sink);
        if f >= from {
            let done = core.gsu_poly_jobs_completed();
            let new = core.take_gsu_poly_jobs();
            lost += done.saturating_sub(jobs.len() as u64 + new.len() as u64);
            jobs.extend(new);
        }
    }
    (jobs, lost)
}

fn prove(name: &str, scene: &str, jobs: &[std::sync::Arc<PolyJob>], lost: u64) -> (Score, Score) {
    let mut incl = Score::default();
    let mut excl = Score::default();
    for job in jobs {
        incl.jobs += 1;
        for op in &job.ops {
            match op {
                rf_core_api::gsu_poly::PolyOp::Polygon { .. } => incl.polygons += 1,
                rf_core_api::gsu_poly::PolyOp::Plot { .. } => incl.plots += 1,
            }
        }
        incl.add(job, RightEdge::Inclusive);
        excl.add(job, RightEdge::Exclusive);
    }
    println!(
        "{name} {scene}: {} jobs ({lost} lost to the ring), {} polygons, {} loose plots, {} plotted pixels",
        incl.jobs, incl.polygons, incl.plots, incl.plotted
    );
    for (rule, s) in [
        ("right edge inclusive", &incl),
        ("right edge exclusive", &excl),
    ] {
        println!(
            "  {rule}: exact index match {:.4} ({}/{}), coverage IoU {:.4}",
            s.exact_rate(),
            s.exact,
            s.plotted,
            s.iou()
        );
    }
    (incl, excl)
}

#[test]
fn rasteriser_matches_the_gsu_bitmap_on_star_fox() {
    let Some(roms) = roms() else { return };
    let scenes = [
        ("intro", frame_range("RF_POLY_INTRO", (850, 1100))),
        ("attract", frame_range("RF_POLY_ATTRACT", (2600, 2800))),
    ];
    for (name, bytes) in &roms {
        for (scene, (from, to)) in scenes {
            let (jobs, lost) = collect_jobs(bytes, from, to);
            let (incl, excl) = prove(name, scene, &jobs, lost);
            assert!(
                incl.jobs > 0 && incl.polygons > 0,
                "{scene}: no polygons seen"
            );
            // Measured 2026-10-15: inclusive 0.996 exact / 0.997-0.999 IoU,
            // exclusive 0.955-0.966 / 0.973-0.986.
            assert!(
                incl.iou() > 0.99 && incl.exact_rate() > 0.99,
                "{scene}: the re-rasteriser disagrees with the GSU ({incl:?})"
            );
            assert!(
                incl.exact_rate() > excl.exact_rate(),
                "{scene}: the right-edge-inclusive rule should win"
            );
        }
    }
}

/// Law 4 on the real game: the observer on or off, the machine is identical
/// (every frame's picture, WRAM, GSU RAM, instructions run).
#[test]
fn observer_does_not_change_the_machine_on_star_fox() {
    let Some(roms) = roms() else { return };
    let (from, to) = frame_range("RF_POLY_HASH", (600, 1200));
    for (name, bytes) in &roms {
        let run = |observe: bool| {
            let mut core = SnesCore::load(bytes).expect("loads");
            let mut sink = FrameHasher::default();
            let mut hashes = Vec::new();
            for f in 0..to {
                if observe && f == from {
                    core.set_gsu_poly_observer(Some(star_fox_config(true)));
                }
                sink.current = 0xcbf2_9ce4_8422_2325;
                let _ = core.step(Step::Frame, &mut sink);
                hashes.push(sink.current);
            }
            let sys = core.system();
            let gsu = sys.bus.gsu.as_ref().expect("Super FX");
            (
                hashes,
                sys.bus.wram.to_vec(),
                gsu.ram.clone(),
                gsu.regs.instructions_executed(),
                core.gsu_poly_jobs_completed(),
            )
        };
        let off = run(false);
        let on = run(true);
        println!(
            "{name}: observer on completed {} jobs over {to} frames",
            on.4
        );
        assert_eq!(off.0, on.0, "{name}: a frame's picture changed");
        assert_eq!(off.1, on.1, "{name}: WRAM changed");
        assert_eq!(off.2, on.2, "{name}: GSU RAM changed");
        assert_eq!(off.3, on.3, "{name}: GSU instruction count changed");
        assert_eq!(off.4, 0);
        assert!(
            on.4 > 0,
            "{name}: the observer saw no job in frames {from}-{to}"
        );
    }
}
