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
