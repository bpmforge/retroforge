//! DSP-1 HLE coverage probe (ticket W14-21, acceptance #3): runs a named
//! title for 600 frames and reports how many command bytes its DSP-1
//! encountered that this HLE does not implement.
//!
//! Local only, like `rf-harness`'s `title_probe`: `#[ignore]`d and
//! env-gated (`DSP1_PROBE_ROM=/path/to/rom.zip`), so a plain `cargo test`
//! never touches a ROM. Lives in `rf-snes` (not `rf-harness`, which
//! depends on `rf-snes` and so cannot be a dependency of it) and loads
//! the cartridge the same way `rf-harness`'s census does: `zip` if the
//! file is a zip, otherwise the raw bytes, handed to
//! `rf_cart::Cartridge::load`/`SnesCore::load`.
//!
//! ```text
//! DSP1_PROBE_ROM=~/Games/Roms/snes/Super Mario Kart (USA).zip \
//!     cargo test --release -p rf-snes --test dsp1_probe -- --ignored --nocapture
//! ```
//!
//! Also includes `scan_dsp_coprocessor_titles` (ticket W14-21 acceptance
//! #5): scans every archive under `DSP1_SCAN_DIR` and prints the ones
//! whose header reports [`rf_cart::Coprocessor::Dsp1`] (the nibble that
//! DSP-1/2/3/4 all share — D-010, `docs/DECISIONS.md`), by filename, so
//! the still-refused DSP-family title can be named without engine code
//! ever identifying a game by title (law 5: this is a one-off developer
//! diagnostic, not a code path the emulator runs).

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, OverlayPixel, PpuPixel};
use rf_snes::core::SnesCore;
use std::io::Read;
use std::path::Path;

#[derive(Default)]
struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn overlay_scanline(&mut self, _y: u16, _pixels: &[OverlayPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _event: CoreEvent) {}
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

fn print_dsp1_report(path: &str, core: &SnesCore) {
    match core.system().bus.dsp1.as_ref() {
        Some(d) => {
            println!(
                "{path}: DSP-1 unknown_commands after 600 frames = {}",
                d.unknown_commands()
            );
            for (op, count) in d.unknown_opcodes() {
                println!("  opcode {op:#04x}: {count} times");
            }
        }
        None => println!("{path}: no DSP-1 coprocessor installed"),
    }
}

/// Run `DSP1_PROBE_ROM` for 600 frames and print the DSP-1 unknown-
/// command counter. `#[ignore]`d: needs a real ROM this repo cannot
/// ship (law 5).
#[test]
#[ignore]
fn dsp1_unknown_command_counter() {
    let Ok(path) = std::env::var("DSP1_PROBE_ROM") else {
        eprintln!("skipped: set DSP1_PROBE_ROM=/path/to/rom.zip");
        return;
    };
    let rom = rom_bytes(Path::new(&path)).unwrap_or_else(|| panic!("could not load {path}"));
    let mut core = SnesCore::load(&rom).expect("valid SNES image");
    let mut sink = NullSink;
    for _ in 0..600 {
        core.run_frame(&InputFrame::empty(), &mut sink);
    }
    print_dsp1_report(&path, &core);
}

/// Bit 12 of the SNES joypad word, per `rf_input::SnesButton::Start`
/// (`bit() == 12`) — not depended on directly: `rf-snes` cannot depend on
/// `rf-input` (the dependency runs the other way), so the bit is
/// reproduced here as a literal, cited rather than guessed.
const START_BIT: u16 = 1 << 12;
/// Bit 7 — `SnesButton::A`.
const A_BIT: u16 = 1 << 7;

/// Ticket W16-11 acceptance #1: run `DSP1_PROBE_ROM` for 600 frames while
/// driving the pad (Start/A mash — the port `InputFrame` argument to
/// `run_frame` is not wired to anything yet, ticket W11-12's note in
/// `SnesCore::run_frame`; controller state is driven the same way
/// `peterlemon_golden.rs`/`gilyon_cputest.rs` drive it, straight into
/// `system_mut().bus.joypads.ports[0]`), and report the DSP-1 DR-drain
/// trace: how many DR reads happened by CPU poll vs. general DMA vs.
/// HDMA, and how many DMA/HDMA units were sourced from the DSP-1 window.
/// `#[ignore]`d: needs a real ROM this repo cannot ship (law 5).
#[test]
#[ignore]
fn dsp1_raster_drain_trace() {
    let Ok(path) = std::env::var("DSP1_PROBE_ROM") else {
        eprintln!("skipped: set DSP1_PROBE_ROM=/path/to/rom.zip");
        return;
    };
    let rom = rom_bytes(Path::new(&path)).unwrap_or_else(|| panic!("could not load {path}"));
    let mut core = SnesCore::load(&rom).expect("valid SNES image");
    let mut sink = NullSink;

    let mut first_cpu_read = None;
    let mut first_mdma = None;
    let mut first_hdma = None;
    let mut first_raster_session = None;

    for frame in 0..600u32 {
        // 2-on/2-off Start, held A throughout: mashes past a title screen
        // and into attract/gameplay without depending on a specific menu
        // layout (`alter_ego_replay.rs`'s square-wave technique, module
        // doc there).
        let start = if frame % 4 < 2 { START_BIT } else { 0 };
        core.system_mut().bus.joypads.ports[0] = start | A_BIT;

        core.run_frame(&InputFrame::empty(), &mut sink);

        if let Some(d) = core.system().bus.dsp1.as_ref() {
            if first_raster_session.is_none() && d.raster_active() {
                first_raster_session = Some(frame);
            }
        }
        let t = core.system().bus.dsp1_trace;
        if first_cpu_read.is_none() && t.cpu_raster_reads > 0 {
            first_cpu_read = Some(frame);
        }
        if first_mdma.is_none() && t.mdma_reads > 0 {
            first_mdma = Some(frame);
        }
        if first_hdma.is_none() && t.hdma_reads > 0 {
            first_hdma = Some(frame);
        }
    }

    let t = core.system().bus.dsp1_trace;
    println!("{path}: DSP-1 DR-drain trace after 600 frames:");
    println!("  first raster session seen at frame {first_raster_session:?}");
    println!(
        "  cpu_raster_reads = {} (first at frame {:?})",
        t.cpu_raster_reads, first_cpu_read
    );
    println!(
        "  mdma_reads = {} (first at frame {:?}); mdma_channel_starts = {}, of which fixed-address = {}",
        t.mdma_reads, first_mdma, t.mdma_channel_starts, t.mdma_channel_starts_fixed
    );
    println!(
        "  hdma_reads = {} (first at frame {:?}); hdma_units = {}",
        t.hdma_reads, first_hdma, t.hdma_units
    );
    print_dsp1_report(&path, &core);
}

/// Diagnostic only (ticket W14-21 acceptance #5): print every archive's
/// load outcome and coprocessor nibble under `DSP1_SCAN_DIR`, to find a
/// candidate DSP-family title that a plain coprocessor-nibble filter
/// would silently skip (a load failure, or a header variant this build
/// classifies as something other than DSP).
#[test]
#[ignore]
fn scan_all_headers() {
    let Ok(dir) = std::env::var("DSP1_SCAN_DIR") else {
        eprintln!("skipped: set DSP1_SCAN_DIR=/path/to/snes/roms");
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        panic!("could not read {dir}");
    };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        let Some(bytes) = rom_bytes(&path) else {
            println!("{}: could not extract a ROM", path.display());
            continue;
        };
        match rf_cart::Cartridge::load(&bytes) {
            Ok(rf_cart::Cartridge::Snes { header, .. }) => {
                println!(
                    "{}: OK map={:?} coprocessor={:?}",
                    path.display(),
                    header.map_mode,
                    header.coprocessor
                );
            }
            Ok(rf_cart::Cartridge::Nes { .. }) => {}
            Err(e) => println!("{}: Cartridge::load FAILED: {e:?}", path.display()),
        }
    }
}

/// Scan `DSP1_SCAN_DIR` for every archive whose header reports the DSP
/// coprocessor nibble, and print filename + load/render outcome for
/// each. `#[ignore]`d: needs a real ROM library this repo cannot ship
/// (law 5).
#[test]
#[ignore]
fn scan_dsp_coprocessor_titles() {
    let Ok(dir) = std::env::var("DSP1_SCAN_DIR") else {
        eprintln!("skipped: set DSP1_SCAN_DIR=/path/to/snes/roms");
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        panic!("could not read {dir}");
    };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        let Some(bytes) = rom_bytes(&path) else {
            continue;
        };
        let Ok(rf_cart::Cartridge::Snes { header, .. }) = rf_cart::Cartridge::load(&bytes) else {
            continue;
        };
        if header.coprocessor != rf_cart::Coprocessor::Dsp1 {
            continue;
        }
        let name = path.display();
        match SnesCore::load(&bytes) {
            Err(e) => println!("{name}: DSP coprocessor, LOAD FAILED: {e:?}"),
            Ok(mut core) => {
                let mut sink = NullSink;
                for _ in 0..600 {
                    core.run_frame(&InputFrame::empty(), &mut sink);
                }
                print_dsp1_report(&name.to_string(), &core);
            }
        }
    }
}
