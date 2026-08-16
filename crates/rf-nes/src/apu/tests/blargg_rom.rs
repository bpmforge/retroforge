//! The real-ROM acceptance gate for this ticket: blargg's `apu_test`
//! (FR-CORE-024), run through the same `$6000` protocol
//! `crate::ppu::tests::blargg_roms` uses, and with the same absence-skip
//! discipline — `roms/` is gitignored (NFR-006), so `cargo test
//! --workspace` must pass on a fresh checkout where these files do not
//! exist.
//!
//! The combined `apu_test.nes` runs all eight sub-tests in one pass
//! (`1-len_ctr`, `2-len_table`, `3-irq_flag`, `4-jitter`, `5-len_timing`,
//! `6-irq_flag_timing`, `7-dmc_basics`, `8-dmc_rates`) and reports the
//! first failure's number as its result code, so a red run names the
//! sub-test in its message.

use std::path::{Path, PathBuf};

use crate::cpu::Cpu;
use crate::system::NesBus;

const VALIDITY_SIGNATURE: [u8; 3] = [0xDE, 0xB0, 0x61];

fn resolve(default_rel: &str) -> Option<PathBuf> {
    let default = Path::new(env!("CARGO_MANIFEST_DIR")).join(default_rel);
    default.is_file().then_some(default)
}

/// Runs the ROM to completion, returning `(status_byte, text)`.
fn run(rom_path: &Path, max_frames: u32) -> (Option<u8>, String) {
    let rom_bytes = std::fs::read(rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let mut bus = NesBus::from_ines_bytes(&rom_bytes)
        .unwrap_or_else(|e| panic!("invalid rom image {}: {e}", rom_path.display()));
    let mut cpu = Cpu::power_on(&mut bus);

    let mut status = None;
    for _ in 0..max_frames {
        let start_frame = bus.frame_count();
        while bus.frame_count() == start_frame {
            cpu.step(&mut bus);
        }
        let region = bus.prg_ram();
        if region[1..4] != VALIDITY_SIGNATURE {
            continue;
        }
        if region[0] != 0x80 {
            status = Some(region[0]);
            break;
        }
    }

    let region = bus.prg_ram();
    let text = region[4..]
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect();
    (status, text)
}

#[test]
fn apu_test_passes() {
    let rel = "../../roms/nes/apu_test/apu_test.nes";
    let Some(rom_path) = resolve(rel) else {
        eprintln!(
            "SKIP apu_test_passes: roms/nes/apu_test/apu_test.nes not found. Fetch it first: \
             scripts/fetch-test-roms.sh"
        );
        return;
    };
    let (status, text) = run(&rom_path, 3600);
    eprintln!("apu_test: {status:?}\n{text}");
    assert_eq!(status, Some(0), "apu_test reported:\n{text}");
}
