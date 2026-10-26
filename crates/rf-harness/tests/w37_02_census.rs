//! **W37-02: the generic tier across the library** (ignored, release).
//!
//! For a seeded sample of the ROM library (and every SNES coprocessor title
//! in it) this boots each cartridge with Rebuild on **cold** and runs it in
//! lockstep against a plain core:
//!
//! - the atlas starts as the vectors alone (`build_seeded_atlas`, the same
//!   functions `retroforge::rebuild::build_for` calls; the harness cannot
//!   depend on that crate), the coverage recorder is armed on the rebuilt
//!   core, and every [`GROW_EVERY`] frames the atlas is rebuilt from what was
//!   recorded and swapped in at the frame boundary, as the shell does;
//! - both cores get the same fixed input script (idle, then a Start press
//!   every 120 frames), from power-on;
//! - the lockstep checker compares registers, memory, video and audio after
//!   every frame. **Any divergence is a bug**, reported with the path of its
//!   repro bundle, never counted as a result of the sample.
//!
//! Each ROM runs in a child process (the `boot_census` design: a panic,
//! abort or hang costs one row and cannot take the census down; the parent's
//! wall clock is the authority). The child prints one tab-separated
//! `W37` line; the parent folds the lines into per-mapper and per-chip
//! tables. No ROM bytes and no paths are written out (law 5), only titles
//! of failures, hashes of nothing, and counts.
//!
//! Counts only: no speed is measured or claimed (D-029).
//!
//! ```text
//! RF_W37_OUT=/tmp/w37.tsv RF_W37_BUNDLES=/tmp/w37-bundles \
//!   cargo test --release -p rf-harness --test w37_02_census -- --ignored --nocapture census
//! ```
//!
//! Knobs (all optional): `RF_ROM_ROOT` (default `~/Games/Roms`, holding
//! `nes/` and `snes/`), `RF_W37_NES` / `RF_W37_SNES` (sample sizes, 200),
//! `RF_W37_SEED`, `RF_W37_FRAMES` (600), `RF_W37_BUDGET_MIN` (wall-clock
//! budget for the whole run, 150; ROMs not reached are listed as skipped),
//! `RF_W37_ONLY` (substring of a title: run just those, for repros).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rf_core_api::coverage::CoverageLog;
use rf_core_api::{EmulatorCore, InputFrame, RebuildStats};
use rf_harness::lockstep::{Options, Side};
use rf_rebuild::Atlas;

const ROM_VAR: &str = "RF_W37_CHILD_ROM";
const GROW_EVERY: u32 = 120;
const PER_ROM_TIMEOUT: Duration = Duration::from_secs(600);
const NES_START: u16 = 1 << 3;
/// `$4218` layout (what the lockstep checker latches): Start is bit 12.
const SNES_START: u16 = 1 << 12;

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn root() -> PathBuf {
    std::env::var("RF_ROM_ROOT").map_or_else(
        |_| PathBuf::from("/Users/bmatthews/Games/Roms"),
        PathBuf::from,
    )
}

// ---- shared ------------------------------------------------------------------

fn archives_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("zip" | "nes" | "sfc" | "smc" | "fig")
            )
        })
        .collect();
    out.sort();
    out
}

/// The cartridge inside `path` (a bare ROM or a zip). Capped at 64 MiB; a
/// nested archive is skipped (the `boot_census` reader).
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
        if !file.is_file() || file.name().to_ascii_lowercase().ends_with(".zip") {
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

fn title_of(p: &Path) -> String {
    p.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// What a SNES cartridge needs beyond the CPU, from its header alone.
fn snes_chip(raw: &[u8]) -> (String, String) {
    let Ok(rf_cart::Cartridge::Snes { header, .. }) = rf_cart::Cartridge::load(raw) else {
        return ("?".into(), "?".into());
    };
    let dbg = format!("{:?}", header.coprocessor);
    let mut chip = dbg.split(['(', ' ', '{']).next().unwrap_or("?").to_string();
    if chip == "None" {
        // SPC7110 has no `Coprocessor` variant in this build: its header is
        // chipset `$Fx` with sub-type `$FFBF = 0` (fullsnes, "SNES Cart
        // SPC7110"), which a plain load would read as no chip at all.
        let base = if header.had_copier_header { 512 } else { 0 };
        let at = |o: usize| raw.get(base + o).copied();
        for hdr in [0x7F00usize, 0xFF00] {
            if at(hdr + 0xD6).is_some_and(|c| matches!(c, 0xF5 | 0xF9)) && at(hdr + 0xBF) == Some(0)
            {
                chip = "Spc7110(header)".into();
            }
        }
    }
    (chip, format!("{:?}", header.map_mode))
}

// ---- child: one ROM ----------------------------------------------------------

type BuildFn = Box<dyn Fn(&CoverageLog) -> Result<Atlas, String>>;

/// The shell's growth policy, driven from the lockstep hook (see
/// `crates/retroforge/tests/w37_01_generic_tier.rs`, which this follows).
struct Grower {
    build: BuildFn,
    acc: Option<CoverageLog>,
    atlas: Atlas,
    last_growth: usize,
    swaps: u32,
    build_ms: u128,
    first_fns: usize,
    /// Executor counters of every atlas that ran and was swapped out.
    epochs: Vec<RebuildStats>,
    /// Frame at which to measure static-map agreement (SNES), and the result.
    probe_at: Option<u32>,
    probe: Option<MapProbe>,
}

/// How the atlas's static cartridge map compares with the bus's own
/// resolution of the same CPU addresses at the probe frame.
#[derive(Default, Clone, Copy)]
struct MapProbe {
    agree: u64,
    differ: u64,
    not_rom: u64,
    differ_inferred: u64,
}

impl Grower {
    fn new(build: BuildFn, probe_at: Option<u32>) -> Result<Self, String> {
        let t = Instant::now();
        let atlas = build(&CoverageLog::new(0))?;
        Ok(Grower {
            build,
            acc: None,
            first_fns: atlas.functions.len(),
            atlas,
            last_growth: 0,
            swaps: 0,
            build_ms: t.elapsed().as_millis(),
            epochs: Vec::new(),
            probe_at,
            probe: None,
        })
    }

    fn on_frame<C: EmulatorCore>(
        &mut self,
        frame: u32,
        core: &mut C,
        trace: impl FnOnce(&mut C),
        rom_of: impl Fn(&C, u32) -> Option<usize>,
    ) {
        if frame == 0 {
            core.set_coverage_recording(true);
            return;
        }
        if Some(frame) == self.probe_at {
            let mut p = MapProbe::default();
            for b in &self.atlas.blocks {
                for i in &b.insns {
                    match rom_of(core, i.cpu) {
                        Some(r) if r == i.rom as usize => p.agree += 1,
                        Some(_) => {
                            p.differ += 1;
                            p.differ_inferred +=
                                u64::from(i.prov == rf_rebuild::Provenance::Inferred);
                        }
                        None => p.not_rom += 1,
                    }
                }
            }
            self.probe = Some(p);
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
        let Ok(atlas) = (self.build)(acc) else { return };
        self.build_ms += t.elapsed().as_millis();
        if atlas.stats.observed_insns < self.atlas.stats.observed_insns {
            return;
        }
        self.epochs.push(core.rebuild_stats().unwrap_or_default());
        core.set_rebuild_atlas(Some(Box::new(atlas.clone())))
            .expect("the swap is accepted");
        trace(core);
        self.atlas = atlas;
        self.swaps += 1;
    }
}

fn input(frame: u32, start: u16) -> InputFrame {
    let mut i = InputFrame::empty();
    // A five-frame Start press every 120 frames, idle between.
    if frame % 120 >= 60 && frame % 120 < 65 {
        i.ports[0] = start;
    }
    i
}

struct Row {
    fields: Vec<(&'static str, String)>,
}

impl Row {
    fn put(&mut self, k: &'static str, v: impl ToString) {
        self.fields.push((k, v.to_string()));
    }
}

fn sum(epochs: &[RebuildStats], last: RebuildStats) -> RebuildStats {
    let mut t = RebuildStats::default();
    for s in epochs.iter().copied().chain([last]) {
        t.observed += s.observed;
        t.inferred += s.inferred;
        t.fallback += s.fallback;
        t.fallback_width += s.fallback_width;
        t.fallback_no_node += s.fallback_no_node;
        t.fallback_ram += s.fallback_ram;
        t.fallback_other += s.fallback_other;
        t.interrupt_entries += s.interrupt_entries;
        t.indirect_checked += s.indirect_checked;
        t.dispatch_misses += s.dispatch_misses;
    }
    t
}

fn finish(row: &mut Row, g: &Grower, report: &rf_harness::lockstep::Report, frames: u32) {
    let t = sum(&g.epochs, report.stats);
    row.put("frames", report.frames_run);
    row.put("planned", frames);
    match &report.divergence {
        Some(d) => {
            row.put("divergence", d.frame);
            row.put(
                "fields",
                d.fields
                    .iter()
                    .take(6)
                    .map(|f| f.field.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        None => row.put("divergence", "-"),
    }
    row.put(
        "bundle",
        report
            .bundle
            .as_ref()
            .map_or("-".to_string(), |b| b.dir.display().to_string()),
    );
    row.put("insns", t.observed + t.inferred + t.fallback);
    row.put("observed", t.observed);
    row.put("inferred", t.inferred);
    row.put("fallback", t.fallback);
    row.put("fb_width", t.fallback_width);
    row.put("fb_nonode", t.fallback_no_node);
    row.put("fb_ram", t.fallback_ram);
    row.put("fb_other", t.fallback_other);
    row.put("indirect", t.indirect_checked);
    row.put("misses", t.dispatch_misses);
    row.put("swaps", g.swaps);
    row.put("fns_first", g.first_fns);
    row.put("fns_final", g.atlas.functions.len());
    row.put(
        "atlas_insns",
        u64::from(g.atlas.stats.observed_insns) + u64::from(g.atlas.stats.inferred_insns),
    );
    row.put("atlas_observed", g.atlas.stats.observed_insns);
    row.put("build_ms", g.build_ms);
    if let Some(p) = g.probe {
        row.put("map_agree", p.agree);
        row.put("map_differ", p.differ);
        row.put("map_differ_inferred", p.differ_inferred);
        row.put("map_not_rom", p.not_rom);
    }
}

fn run_nes(raw: &[u8], frames: u32, bundles: &Path, slug: &str, row: &mut Row) {
    let rom = match rf_nes::NesRom::from_ines_bytes(raw) {
        Ok(r) => r,
        Err(e) => {
            row.put("booted", "refused");
            row.put("why", format!("{e:?}"));
            return;
        }
    };
    let prg = rom.prg_rom().to_vec();
    let mut g = match Grower::new(
        Box::new(move |log| Ok(rf_nes::rebuild::build_seeded_atlas(&prg, log))),
        None,
    ) {
        Ok(g) => g,
        Err(e) => {
            row.put("booted", "no");
            row.put("why", e);
            return;
        }
    };
    let atlas = g.atlas.clone();
    let mut hook = |frame: u32, side: Side, core: &mut rf_nes::core::NesCore| {
        if side == Side::Rebuilt {
            g.on_frame(frame, core, |c| c.set_rebuild_trace(256), |_, _| None);
        }
    };
    let result = rf_harness::lockstep::run(
        raw,
        &atlas,
        (0..frames).map(|f| input(f, NES_START)),
        Options {
            before_frame: Some(&mut hook),
            bundle_dir: Some(bundles.join(slug)),
            ..Options::default()
        },
    );
    match result {
        Ok(r) => {
            row.put("booted", "yes");
            finish(row, &g, &r, frames);
        }
        Err(e) => {
            row.put("booted", "refused");
            row.put("why", e);
        }
    }
}

fn run_snes(raw: &[u8], frames: u32, bundles: &Path, slug: &str, audit: bool, row: &mut Row) {
    let image = raw.to_vec();
    let mut g = match Grower::new(
        Box::new(move |log| {
            rf_snes::rebuild::build_seeded_atlas_from_image(&image, log).map_err(|e| e.to_string())
        }),
        Some(frames.saturating_sub(1)),
    ) {
        Ok(g) => g,
        Err(e) => {
            row.put("booted", "refused");
            row.put("why", e);
            return;
        }
    };
    let atlas = g.atlas.clone();
    let (mut audited, mut wrong) = (0u64, 0u64);
    let mut hook = |frame: u32, side: Side, core: &mut rf_snes::core::SnesCore| {
        if side == Side::Rebuilt {
            if audit && (frame == 120 || frame == 360 || frame + 1 == frames) {
                let (n, bad) = audit_rom_offsets(core);
                audited += n;
                wrong += bad;
            }
            g.on_frame(
                frame,
                core,
                |c| c.set_rebuild_trace(256),
                |c, cpu| c.system().bus.rom_offset(cpu),
            );
        }
    };
    let result = rf_harness::lockstep_snes::run(
        raw,
        &atlas,
        (0..frames).map(|f| input(f, SNES_START)),
        Options {
            before_frame: Some(&mut hook),
            bundle_dir: Some(bundles.join(slug)),
            ..Options::default()
        },
    );
    match result {
        Ok(r) => {
            row.put("booted", "yes");
            finish(row, &g, &r, frames);
            if audit {
                row.put("audit_checked", audited);
                row.put("audit_wrong", wrong);
            }
        }
        Err(e) => {
            row.put("booted", "refused");
            row.put("why", e);
        }
    }
}

/// Every 24-bit address the bus calls ROM right now: does a CPU read of it
/// return `rom[offset]`? That is the whole contract an atlas node keyed by
/// ROM offset leans on. Returns (addresses checked, addresses that differ).
fn audit_rom_offsets(core: &rf_snes::core::SnesCore) -> (u64, u64) {
    use rf_snes::cpu::CpuBus as _;
    let bus = &core.system().bus;
    let (mut n, mut bad) = (0u64, 0u64);
    for addr in 0..0x100_0000u32 {
        if let Some(o) = bus.rom_offset(addr) {
            n += 1;
            bad += u64::from(bus.peek(addr) != bus.rom[o]);
        }
    }
    (n, bad)
}

#[test]
#[ignore = "child half of the census; the parent re-executes this"]
fn census_child() {
    let Ok(path) = std::env::var(ROM_VAR) else {
        eprintln!("SKIP: {ROM_VAR} unset, this is the census's child half");
        return;
    };
    let frames = env_num("RF_W37_FRAMES", 600) as u32;
    let bundles = std::env::var("RF_W37_BUNDLES").map_or_else(
        |_| std::env::temp_dir().join("w37-02-bundles"),
        PathBuf::from,
    );
    let path = PathBuf::from(path);
    let slug: String = title_of(&path)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(48)
        .collect();
    let mut row = Row { fields: Vec::new() };
    let Some(raw) = rom_bytes(&path) else {
        row.put("console", "?");
        row.put("booted", "refused");
        row.put("why", "no cartridge in the archive");
        print_row(&row);
        return;
    };
    let t = Instant::now();
    // A panic is a result, not a crash of the census: the parent also
    // counts a child that dies, but the message is worth having.
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || match rf_cart::Cartridge::load(&raw) {
                Ok(rf_cart::Cartridge::Nes { header, .. }) => {
                    row.put("console", "nes");
                    row.put("mapper", header.mapper);
                    run_nes(&raw, frames, &bundles, &slug, &mut row);
                }
                Ok(rf_cart::Cartridge::Snes { .. }) => {
                    let (chip, map) = snes_chip(&raw);
                    row.put("console", "snes");
                    row.put("mapper", format!("{chip}/{map}"));
                    run_snes(&raw, frames, &bundles, &slug, chip != "None", &mut row);
                }
                Err(e) => {
                    row.put("console", "?");
                    row.put("booted", "refused");
                    row.put("why", format!("{e:?}"));
                }
            },
        ));
    if let Err(p) = outcome {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        row.put("panic", msg.replace(['\t', '\n'], " "));
    }
    row.put("wall_ms", t.elapsed().as_millis());
    print_row(&row);
}

fn print_row(row: &Row) {
    let line: Vec<String> = row.fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("W37\t{}", line.join("\t"));
}

// ---- parent: sample, run, fold -------------------------------------------------

/// splitmix64: a fixed, dependency-free stream for the sample.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Fisher-Yates over a copy; the loop index counts down every pass.
fn shuffled<T: Clone>(items: &[T], seed: u64) -> Vec<T> {
    let mut v = items.to_vec();
    let mut rng = Rng(seed);
    for i in (1..v.len()).rev() {
        let j = (rng.next() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    v
}

struct Job {
    path: PathBuf,
    /// Selected because it carries a coprocessor, or a mapper the sample
    /// missed, not by the sample.
    forced: bool,
}

fn select(seed: u64, nes_n: usize, snes_n: usize) -> Vec<Job> {
    let nes = archives_in(&root().join("nes"));
    let mut jobs: Vec<Job> = shuffled(&nes, seed)
        .into_iter()
        .take(nes_n)
        .map(|path| Job {
            path,
            forced: false,
        })
        .collect();
    // Every mapper the sample missed still gets up to three boards (seeded),
    // so each `prg_rom_offset` implementation meets a real cartridge.
    let nes_mapper = |p: &Path| -> Option<u16> {
        match rf_cart::Cartridge::load(&rom_bytes(p)?) {
            Ok(rf_cart::Cartridge::Nes { header, .. }) => Some(header.mapper),
            _ => None,
        }
    };
    let covered: std::collections::BTreeSet<u16> =
        jobs.iter().filter_map(|j| nes_mapper(&j.path)).collect();
    let mut per_mapper: BTreeMap<u16, usize> = BTreeMap::new();
    let mut extras = Vec::new();
    for p in shuffled(&nes, seed ^ 0xA5A5)
        .into_iter()
        .filter(|p| !jobs.iter().any(|j| &j.path == p))
    {
        let Some(m) = nes_mapper(&p).filter(|m| !covered.contains(m)) else {
            continue;
        };
        let n = per_mapper.entry(m).or_default();
        if *n < 3 {
            *n += 1;
            extras.push(Job {
                path: p,
                forced: true,
            });
        }
    }
    if std::env::var("RF_W37_EXTRA_ONLY").is_ok() {
        println!("extra mapper boards only: {}", extras.len());
        return extras;
    }
    jobs.extend(extras);
    let snes = archives_in(&root().join("snes"));
    let (mut chips, mut plain) = (Vec::new(), Vec::new());
    for p in snes {
        let chip = rom_bytes(&p).map(|raw| snes_chip(&raw).0);
        match chip.as_deref() {
            Some("None" | "?") | None => plain.push(p),
            Some(_) => chips.push(p),
        }
    }
    println!(
        "library: {} nes archives, {} snes ({} with a coprocessor)",
        nes.len(),
        plain.len() + chips.len(),
        chips.len()
    );
    jobs.extend(
        shuffled(&plain, seed ^ 0x55AA)
            .into_iter()
            .take(snes_n)
            .map(|path| Job {
                path,
                forced: false,
            }),
    );
    jobs.extend(chips.into_iter().map(|path| Job { path, forced: true }));
    jobs
}

fn run_child(exe: &Path, job: &Job, deadline: Instant) -> Option<BTreeMap<String, String>> {
    let mut child = Command::new(exe)
        .args(["--exact", "census_child", "--ignored", "--nocapture"])
        .env(ROM_VAR, &job.path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        }
        if started.elapsed() > PER_ROM_TIMEOUT || Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            let mut m = BTreeMap::new();
            m.insert("crash".into(), "timeout".into());
            return Some(m);
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let out = reader.join().unwrap_or_default();
    let mut m: BTreeMap<String, String> = out
        .lines()
        .find_map(|l| l.strip_prefix("W37\t"))
        .map(|l| {
            l.split('\t')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if m.is_empty() {
        m.insert(
            "crash".into(),
            format!("child died: {:?}", status.and_then(|s| s.code())),
        );
    }
    Some(m)
}

#[derive(Default)]
struct Bucket {
    n: u32,
    booted: u32,
    refused: u32,
    panics: u32,
    divergences: u32,
    insns: u64,
    observed: u64,
    inferred: u64,
    fallback: u64,
    width: u64,
    nonode: u64,
    ram: u64,
    other: u64,
    atlas_insns: u64,
    map_differ: u64,
    map_differ_inferred: u64,
    map_total: u64,
    audit_checked: u64,
    audit_wrong: u64,
    rates: Vec<f64>,
}

impl Bucket {
    fn add(&mut self, m: &BTreeMap<String, String>) {
        let num = |k: &str| m.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        self.n += 1;
        if m.contains_key("panic") || m.contains_key("crash") {
            self.panics += 1;
        }
        match m.get("booted").map(String::as_str) {
            Some("yes") => self.booted += 1,
            Some("refused") => self.refused += 1,
            _ => {}
        }
        if m.get("divergence").is_some_and(|d| d != "-") {
            self.divergences += 1;
        }
        let insns = num("insns");
        self.insns += insns;
        self.observed += num("observed");
        self.inferred += num("inferred");
        self.fallback += num("fallback");
        self.width += num("fb_width");
        self.nonode += num("fb_nonode");
        self.ram += num("fb_ram");
        self.other += num("fb_other");
        self.atlas_insns += num("atlas_insns");
        self.map_differ += num("map_differ");
        self.map_differ_inferred += num("map_differ_inferred");
        self.audit_checked += num("audit_checked");
        self.audit_wrong += num("audit_wrong");
        self.map_total += num("map_agree") + num("map_differ") + num("map_not_rom");
        if insns > 0 {
            self.rates.push(num("fallback") as f64 / insns as f64);
        }
    }

    fn pct(part: u64, whole: u64) -> f64 {
        if whole == 0 {
            0.0
        } else {
            100.0 * part as f64 / whole as f64
        }
    }

    fn median(&self) -> f64 {
        let mut r = self.rates.clone();
        r.sort_by(f64::total_cmp);
        r.get(r.len() / 2).copied().unwrap_or(0.0) * 100.0
    }

    fn line(&self, label: &str) -> String {
        format!(
            "{label:<22} {:>4} {:>6} {:>7} {:>6} {:>5} {:>13} {:>7.3} {:>7.3} {:>6.1} {:>6.1} {:>6.1} {:>6.1}",
            self.n,
            self.booted,
            self.refused,
            self.panics,
            self.divergences,
            self.insns,
            Self::pct(self.fallback, self.insns),
            self.median(),
            Self::pct(self.width, self.fallback),
            Self::pct(self.nonode, self.fallback),
            Self::pct(self.ram, self.fallback),
            Self::pct(self.other, self.fallback),
        )
    }
}

const HEADER: &str = "group                     n  booted refused panics  div     instructions  fb%all fb%med  width% nonode%   ram% other%";

fn print_table(title: &str, rows: &BTreeMap<String, Bucket>) {
    println!("\n{title}\n{HEADER}");
    let mut all = Bucket::default();
    for (k, b) in rows {
        println!("{}", b.line(k));
        all.n += b.n;
        all.booted += b.booted;
        all.refused += b.refused;
        all.panics += b.panics;
        all.divergences += b.divergences;
        all.insns += b.insns;
        all.fallback += b.fallback;
        all.width += b.width;
        all.nonode += b.nonode;
        all.ram += b.ram;
        all.other += b.other;
        all.rates.extend(&b.rates);
    }
    println!("{}", all.line("ALL"));
}

#[test]
#[ignore = "local: runs a real ROM library, one child process per title"]
fn census() {
    if std::env::var(ROM_VAR).is_ok() {
        return;
    }
    let seed = env_num("RF_W37_SEED", 0x37_02);
    let (nes_n, snes_n) = (
        env_num("RF_W37_NES", 200) as usize,
        env_num("RF_W37_SNES", 200) as usize,
    );
    let budget = Duration::from_secs(60 * env_num("RF_W37_BUDGET_MIN", 150));
    let only = std::env::var("RF_W37_ONLY").ok();
    let exe = std::env::current_exe().expect("the test binary knows its own path");
    let mut jobs = select(seed, nes_n, snes_n);
    if let Some(o) = &only {
        jobs.retain(|j| title_of(&j.path).contains(o.as_str()));
    }
    assert!(!jobs.is_empty(), "a census that ran nothing is not a pass");
    println!(
        "census: {} ROMs (seed {seed:#x}, {} frames each, budget {} min)",
        jobs.len(),
        env_num("RF_W37_FRAMES", 600),
        budget.as_secs() / 60
    );
    let started = Instant::now();
    let deadline = started + budget;
    let out_path = std::env::var("RF_W37_OUT").ok();
    let (mut nes, mut snes): (BTreeMap<String, Bucket>, BTreeMap<String, Bucket>) =
        (BTreeMap::new(), BTreeMap::new());
    let mut skipped = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for (i, job) in jobs.iter().enumerate() {
        if Instant::now() > deadline {
            skipped = jobs.len() - i;
            break;
        }
        let title = title_of(&job.path);
        let Some(m) = run_child(&exe, job, deadline) else {
            bad.push(format!("{title}: child did not start"));
            continue;
        };
        if let Some(path) = &out_path {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let kv: Vec<String> = m.iter().map(|(k, v)| format!("{k}={v}")).collect();
                let _ = writeln!(f, "{title}\tforced={}\t{}", job.forced, kv.join("\t"));
            }
        }
        let console = m.get("console").map_or("?", String::as_str);
        let mapper = m.get("mapper").cloned().unwrap_or_else(|| "?".into());
        let group = if console == "nes" {
            format!("mapper {mapper:>3}")
        } else {
            mapper
        };
        let table = if console == "snes" {
            &mut snes
        } else {
            &mut nes
        };
        table.entry(group).or_default().add(&m);
        if let Some(p) = m.get("panic").or_else(|| m.get("crash")) {
            bad.push(format!("PANIC/CRASH {title}: {p}"));
        }
        if m.get("divergence").is_some_and(|d| d != "-") {
            bad.push(format!(
                "DIVERGENCE {title}: frame {} fields {} bundle {}",
                m["divergence"],
                m.get("fields").map_or("", String::as_str),
                m.get("bundle").map_or("-", String::as_str)
            ));
        }
        if m.get("audit_wrong").is_some_and(|w| w != "0") {
            bad.push(format!(
                "AUDIT {title}: {} ROM addresses read differently from rom[offset]",
                m["audit_wrong"]
            ));
        }
        if (i + 1) % 25 == 0 {
            println!(
                "  [{}/{}] {:.0} min elapsed",
                i + 1,
                jobs.len(),
                started.elapsed().as_secs_f32() / 60.0
            );
        }
    }
    print_table("NES by mapper", &nes);
    print_table("SNES by chip/map", &snes);
    let mut map = Bucket::default();
    for b in snes.values() {
        map.map_differ += b.map_differ;
        map.map_differ_inferred += b.map_differ_inferred;
        map.map_total += b.map_total;
    }
    println!(
        "\nSNES static map vs the bus at the last frame: {} atlas instructions, {} placed at another offset ({} of them inferred)",
        map.map_total, map.map_differ, map.map_differ_inferred
    );
    for (k, b) in &snes {
        if b.audit_checked > 0 {
            println!(
                "  {k:<22} ROM addresses audited {:>13}, read differently from rom[offset] {:>4}",
                b.audit_checked, b.audit_wrong
            );
        }
        if b.map_total > 0 {
            println!(
                "  {k:<22} atlas instructions {:>9}, placed elsewhere {:>7}, of which inferred {:>7}",
                b.map_total, b.map_differ, b.map_differ_inferred
            );
        }
    }
    println!(
        "\nran {} of {} ROMs in {:.1} min ({} skipped for budget)",
        jobs.len() - skipped,
        jobs.len(),
        started.elapsed().as_secs_f32() / 60.0,
        skipped
    );
    for b in &bad {
        println!("{b}");
    }
    println!("no speed is measured or claimed (D-029)");
    assert!(
        bad.is_empty(),
        "{} ROMs panicked, crashed or diverged (see above)",
        bad.len()
    );
}
