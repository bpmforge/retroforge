//! **W40-03: SA-1 lifting, lockstep on a real SA-1 cartridge** (ROM-gated,
//! law 5: only offsets, counts and frame numbers are printed).
//!
//! Both 65816s of the cartridge are recorded in one played stretch (the
//! S-CPU and the SA-1 each into their own log and atlas, keyed apart), then
//! the stretch is replayed by a plain core and a core with **both** atlases
//! installed, compared after every frame by `lockstep_snes` (which names the
//! SA-1's registers, I-RAM and BW-RAM besides the S-CPU's fields).
//!
//! ```text
//! cargo test --release -p rf-harness --test w40_03_lockstep_sa1 -- --ignored --nocapture
//! ```
//!
//! Environment (all optional): `RF_SCREENS_ROMS_DIR` (ROM root),
//! `RF_W40_GAME` (`kdl3` or `smrpg`; default `kdl3`), `RF_W40_SCRIPT`
//! (replace the built-in pad script), `RF_W40_OUT` (directory for probe PNGs,
//! used by `probe_script` only).

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, RebuildStats, ResetKind,
};
use rf_harness::lockstep::{Options, Side};
use rf_harness::lockstep_snes::run;
use rf_input::SnesButton;
use rf_snes::core::SnesCore;

struct Game {
    file: &'static str,
    /// Boot, logos and menus to the first level (the token scripts of
    /// `w38_01_data_regions_smw`: `wN` idles N frames, `<buttons>N` holds
    /// them for N frames; `A B X Y S E U D L R`).
    menus: &'static str,
    /// Two different bouts of play, each repeated.
    play_a: &'static str,
    play_b: &'static str,
}

fn game() -> Game {
    match std::env::var("RF_W40_GAME").as_deref() {
        Ok("smrpg") => Game {
            file: "Super Mario RPG - Legend of the Seven Stars (USA).zip",
            menus: "w600",
            play_a: "w60",
            play_b: "w60",
        },
        // Kirby's Dream Land 3: Nintendo/HAL logos, title, file select, the
        // opening Grass Land level (found with `probe_script`).
        _ => Game {
            file: "Kirby's Dream Land 3 (USA).zip",
            menus: "w300 S6 w194 S6 w194 S6 w194 A6 w194 A6 w194 A6 w194 S6 w194 A6 w194",
            play_a: "R50 R+A20 R40 A8 w20 R30 R+B12 w10",
            play_b: "R20 R+A30 A12 w14 R+B16 L20 R50 R+A24 w8",
        },
    }
}

/// `menus`, then `bout` repeated `reps` times.
fn script(menus: &str, bout: &str, reps: usize) -> Vec<u16> {
    let mut pad = frames_of(menus);
    for _ in 0..reps {
        pad.extend(frames_of(bout));
    }
    pad
}

fn reps() -> usize {
    std::env::var("RF_W40_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8)
}

fn rom_path(file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("snes").join(file);
    p.exists().then_some(p)
}

fn load_rom(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).ok()?;
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Snes { .. })
        ) {
            return Some(buf);
        }
    }
    None
}

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
                    "A" => SnesButton::A,
                    "B" => SnesButton::B,
                    "X" => SnesButton::X,
                    "Y" => SnesButton::Y,
                    "S" => SnesButton::Start,
                    "E" => SnesButton::Select,
                    "U" => SnesButton::Up,
                    "D" => SnesButton::Down,
                    "L" => SnesButton::Left,
                    "R" => SnesButton::Right,
                    other => panic!("button {other}"),
                }
                .bit();
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

fn input(mask: u16) -> InputFrame {
    let mut f = InputFrame::empty();
    f.ports[0] = mask;
    f
}

/// Keeps the last emitted frame as palette indices, for probe screenshots.
#[derive(Default)]
struct Shot {
    lines: Vec<Vec<u8>>,
}
impl CoreSink for Shot {
    fn video_scanline(&mut self, y: u16, p: &[PpuPixel]) {
        let y = usize::from(y);
        if self.lines.len() <= y {
            self.lines.resize(y + 1, Vec::new());
        }
        self.lines[y] = p.iter().map(|x| x.palette_index).collect();
    }
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn rgba_of(core: &SnesCore, shot: &Shot) -> Vec<u8> {
    let w = 256usize;
    let h = shot.lines.len().max(1);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for line in &shot.lines {
        // Hi-res lines are twice as wide: take every other pixel.
        let step = if line.len() >= 512 { 2 } else { 1 };
        for x in 0..w {
            let idx = line.get(x * step).copied().unwrap_or(0);
            let c = core.system().bus.ppu.cgram[usize::from(idx)];
            let (r, g, b) = (c & 31, (c >> 5) & 31, (c >> 10) & 31);
            rgba.extend_from_slice(&[(r * 8) as u8, (g * 8) as u8, (b * 8) as u8, 255]);
        }
    }
    rgba
}

/// Contact sheet of 256x224 frames, five per row.
fn sheet(frames: &[Vec<u8>]) -> (Vec<u8>, u32, u32) {
    let (w, h, cols) = (256usize, 224usize, 5usize);
    let rows = frames.len().div_ceil(cols).max(1);
    let mut out = vec![0u8; w * cols * h * rows * 4];
    for (i, f) in frames.iter().enumerate() {
        let (cx, cy) = ((i % cols) * w, (i / cols) * h);
        for y in 0..h.min(f.len() / (w * 4)) {
            let dst = ((cy + y) * w * cols + cx) * 4;
            out[dst..dst + w * 4].copy_from_slice(&f[y * w * 4..(y + 1) * w * 4]);
        }
    }
    (out, (w * cols) as u32, (h * rows) as u32)
}

/// Step through the pad script printing progress, saving a PNG every
/// `RF_W40_SHOT_EVERY` frames into `RF_W40_OUT`. Exploration aid for the
/// script; asserts nothing.
#[test]
#[ignore = "ROM-gated exploration aid for the pad script (RF_W40_OUT)"]
fn probe_script() {
    let g = game();
    let Some(path) = rom_path(g.file) else {
        eprintln!("SKIP: {} not found", g.file);
        return;
    };
    let raw = load_rom(&path).expect("loads");
    let pad = match std::env::var("RF_W40_SCRIPT") {
        Ok(text) => frames_of(&text),
        Err(_) => script(g.menus, g.play_a, reps()),
    };
    let every: usize = std::env::var("RF_W40_SHOT_EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let out = std::env::var("RF_W40_OUT").ok().map(PathBuf::from);
    if let Some(d) = &out {
        std::fs::create_dir_all(d).expect("out dir");
    }
    let mut core = SnesCore::load(&raw).expect("core");
    core.reset(ResetKind::Hard);
    let mut shot = Shot::default();
    let mut sheets = Vec::new();
    for (i, mask) in pad.iter().enumerate() {
        core.system_mut().bus.joypads.ports[0] = *mask;
        core.run_frame(&input(*mask), &mut shot);
        if (i + 1) % every == 0 || i + 1 == pad.len() {
            let sa1 = core
                .system()
                .bus
                .sa1
                .as_ref()
                .map_or(0, |s| s.instructions_executed);
            eprintln!("frame {} sa1 insns {sa1}", i + 1);
            sheets.push(rgba_of(&core, &shot));
        }
    }
    if let Some(d) = &out {
        let (px, w, h) = sheet(&sheets);
        std::fs::write(
            d.join("sheet.png"),
            rf_renderer::png::encode_rgba(&px, w, h),
        )
        .unwrap();
    }
}

/// Record both CPUs over `pad` on a plain core; returns the S-CPU and SA-1
/// logs.
fn record(raw: &[u8], pad: &[u16]) -> (rf_core_api::CoverageLog, rf_core_api::CoverageLog) {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(true);
    core.set_sa1_coverage_recording(true);
    let mut sink = Shot::default();
    for &mask in pad {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&input(mask), &mut sink);
    }
    (
        core.take_coverage().expect("S-CPU armed"),
        core.take_sa1_coverage().expect("SA-1 armed"),
    )
}

fn line(label: &str, s: &RebuildStats) {
    println!(
        "  {label}: {} instructions: {} observed / {} inferred dispatch; fallback {} ({:.3}%): \
         no node {}, width mismatch {}, RAM {}, other {}; {} interrupt entries; {} indirect \
         transfers checked, {} dispatch misses, {} distinct",
        s.instructions(),
        s.observed,
        s.inferred,
        s.fallback,
        s.fallback_rate() * 100.0,
        s.fallback_no_node,
        s.fallback_width,
        s.fallback_ram,
        s.fallback_other,
        s.interrupt_entries,
        s.indirect_checked,
        s.dispatch_misses,
        s.delta_len,
    );
}

/// Lockstep `pad` with the S-CPU atlas on the rebuilt core and the SA-1
/// atlas installed through the first-frame hook. Returns the S-CPU stats and
/// the SA-1 stats as of the frame before the last (the hook runs before each
/// frame).
fn lockstep(
    raw: &[u8],
    cpu_atlas: &rf_rebuild::Atlas,
    sa1_atlas: &rf_rebuild::Atlas,
    pad: &[u16],
    label: &str,
) -> (RebuildStats, RebuildStats) {
    let mut sa1_stats = RebuildStats::default();
    let mut hook = |frame: u32, side: Side, core: &mut SnesCore| {
        if side != Side::Rebuilt {
            return;
        }
        if frame == 0 {
            core.set_sa1_rebuild(Some(sa1_atlas.clone()))
                .expect("SA-1 atlas installs");
        } else if let Some(s) = core.sa1_rebuild_stats() {
            sa1_stats = s;
        }
    };
    let opts = Options {
        before_frame: Some(&mut hook),
        ..Options::default()
    };
    let started = Instant::now();
    let r = run(raw, cpu_atlas, pad.iter().map(|&m| input(m)), opts).expect("runs");
    if let Some(d) = &r.divergence {
        let fields: Vec<_> = d
            .fields
            .iter()
            .map(|f| {
                format!(
                    "{} @{:#x} plain {:#x} rebuilt {:#x}",
                    f.field, f.first_offset, f.plain, f.rebuilt
                )
            })
            .collect();
        panic!(
            "{label}: DIVERGED at frame {}: {fields:?}; fine {:?}",
            d.frame, d.fine
        );
    }
    println!(
        "{label}: {} frames, zero divergence ({:.0} s). S-CPU and SA-1 both lifted:",
        r.frames_run,
        started.elapsed().as_secs_f64()
    );
    line("S-CPU", &r.stats);
    line("SA-1 ", &sa1_stats);
    (r.stats, sa1_stats)
}

#[test]
#[ignore = "needs an SA-1 cartridge (RF_SCREENS_ROMS_DIR); minutes in release"]
fn both_65816s_lifted_equal_plain_over_two_scripts() {
    let g = game();
    let Some(path) = rom_path(g.file) else {
        println!("SKIP: {} not found", g.file);
        return;
    };
    let raw = load_rom(&path).expect("loads");
    let pad_a = script(g.menus, g.play_a, reps());
    let pad_b = script(g.menus, g.play_b, reps());
    assert!(pad_a != pad_b, "the second script must differ");

    let (cpu_log, sa1_log) = record(&raw, &pad_a);
    println!(
        "recorded {} frames: S-CPU {} instructions, SA-1 {} instructions",
        pad_a.len(),
        cpu_log.instructions_recorded,
        sa1_log.instructions_recorded
    );
    assert!(sa1_log.instructions_recorded > 1_000_000, "the SA-1 ran");
    let cpu_atlas = rf_harness::atlas::build_snes_atlas(&raw, &cpu_log).expect("S-CPU atlas");
    let sa1_atlas =
        rf_snes::rebuild::build_sa1_atlas_from_image(&raw, &sa1_log).expect("SA-1 atlas");
    for (label, a) in [("S-CPU", &cpu_atlas), ("SA-1", &sa1_atlas)] {
        println!(
            "{label} atlas: {} functions, {} insns ({} observed / {} inferred), ROM bytes executed {}",
            a.functions.len(),
            a.stats.observed_insns + a.stats.inferred_insns,
            a.stats.observed_insns,
            a.stats.inferred_insns,
            if label == "SA-1" { sa1_log.executed_prg_bytes() } else { cpu_log.executed_prg_bytes() },
        );
    }
    assert_ne!(
        sa1_atlas.stats.observed_insns, cpu_atlas.stats.observed_insns,
        "two cores, two atlases"
    );

    let (cpu, sa1) = lockstep(&raw, &cpu_atlas, &sa1_atlas, &pad_a, "script A (recorded)");
    assert!(cpu.observed > 0 && sa1.observed > 0);
    let (cpu, sa1) = lockstep(
        &raw,
        &cpu_atlas,
        &sa1_atlas,
        &pad_b,
        "script B (different play)",
    );
    assert!(cpu.observed > 0 && sa1.observed > 0);
}

// ---- no ROM needed: a synthetic SA-1 cartridge ------------------------------

/// A 32 KiB SA-1 image. The S-CPU programs the SA-1's reset vector (`$8100`),
/// releases it and spins; the SA-1 opens its I-RAM, then loops forever on
/// `JSR sub ; BRA` where `sub` is `INC $3010 ; RTS` (an indirect return per
/// pass).
fn sa1_image() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let base = 0x7FC0;
    rom[base + 0x15] = 0x23;
    rom[base + 0x16] = 0x35;
    rom[base + 0x17] = 6;
    rom[base + 0x18] = 3;
    let sum: u16 = 0xBEEF;
    rom[base + 0x1C..base + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    rom[base + 0x1E..base + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    let main: &[u8] = &[
        0xA9, 0x00, 0x8D, 0x03, 0x22, // CRV lo
        0xA9, 0x81, 0x8D, 0x04, 0x22, // CRV hi
        0xA9, 0x00, 0x8D, 0x00, 0x22, // release Reset
        0x80, 0xFE, // BRA *
    ];
    rom[..main.len()].copy_from_slice(main);
    let sa1: &[u8] = &[
        0xA9, 0xFF, 0x8D, 0x2A, 0x22, // CIWP: open I-RAM for the SA-1
        0x20, 0x10, 0x81, // loop: JSR $8110
        0x80, 0xFB, // BRA loop
    ];
    rom[0x100..0x100 + sa1.len()].copy_from_slice(sa1);
    rom[0x110..0x114].copy_from_slice(&[0xEE, 0x10, 0x30, 0x60]); // INC $3010 ; RTS
    rom
}

fn synthetic_atlases(raw: &[u8]) -> (rf_rebuild::Atlas, rf_rebuild::Atlas) {
    let (cpu_log, sa1_log) = record(raw, &[0u16; 8]);
    assert!(sa1_log.instructions_recorded > 1000, "the SA-1 ran");
    (
        rf_harness::atlas::build_snes_atlas(raw, &cpu_log).expect("S-CPU atlas"),
        rf_snes::rebuild::build_sa1_atlas_from_image(raw, &sa1_log).expect("SA-1 atlas"),
    )
}

fn synthetic_run(poke: Option<fn(&mut SnesCore)>) -> (rf_harness::lockstep::Report, RebuildStats) {
    let raw = sa1_image();
    let (cpu_atlas, sa1_atlas) = synthetic_atlases(&raw);
    let mut sa1_stats = RebuildStats::default();
    let mut hook = |frame: u32, side: Side, core: &mut SnesCore| {
        if side != Side::Rebuilt {
            return;
        }
        if frame == 0 {
            core.set_sa1_rebuild(Some(sa1_atlas.clone()))
                .expect("installs");
        } else if let Some(s) = core.sa1_rebuild_stats() {
            sa1_stats = s;
        }
        if frame == 3 {
            if let Some(poke) = poke {
                poke(core);
            }
        }
    };
    let opts = Options {
        before_frame: Some(&mut hook),
        ..Options::default()
    };
    let r = run(&raw, &cpu_atlas, (0..8).map(|_| input(0)), opts).expect("runs");
    (r, sa1_stats)
}

#[test]
fn both_65816s_lifted_equal_plain_on_a_synthetic_sa1_cartridge() {
    let (r, sa1) = synthetic_run(None);
    assert!(r.divergence.is_none(), "{:?}", r.divergence);
    assert_eq!(r.frames_run, 8);
    assert!(sa1.observed > 50_000, "{sa1:?}");
    assert_eq!((sa1.fallback, sa1.dispatch_misses), (0, 0), "{sa1:?}");
    assert!(sa1.indirect_checked > 1000, "{sa1:?}");
    assert!(r.stats.observed > 0, "the S-CPU is lifted too");
}

/// The comparison sees the SA-1: a difference on the rebuilt core's SA-1
/// only is caught at its frame under the SA-1's own field name.
#[test]
fn a_difference_in_the_sa1_is_caught_and_named() {
    type Poke = fn(&mut SnesCore);
    let cases: [(&str, Poke); 3] = [
        ("sa1.iram", |c| {
            c.system_mut().bus.sa1.as_mut().unwrap().iram[0x20] ^= 1;
        }),
        ("sa1.bwram", |c| {
            c.system_mut().bus.sa1.as_mut().unwrap().bwram[0x40] ^= 1;
        }),
        ("sa1.cpu.x", |c| {
            c.system_mut().bus.sa1.as_mut().unwrap().cpu.x ^= 0x10;
        }),
    ];
    for (field, poke) in cases {
        let (r, _) = synthetic_run(Some(poke));
        let d = r
            .divergence
            .unwrap_or_else(|| panic!("{field}: not caught"));
        assert_eq!(d.frame, 3, "{field}");
        assert!(
            d.fields.iter().any(|f| f.field == field),
            "{field}: {:?}",
            d.fields.iter().map(|f| f.field).collect::<Vec<_>>()
        );
    }
}
