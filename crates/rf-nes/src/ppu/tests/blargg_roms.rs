//! Real-ROM cargo-test peer of `crates/rf-harness/src/blargg_evidence.rs`
//! (ticket W1-05b) — same "second, independent peer" relationship
//! `crate::system::tests::nestest` has to `rf-harness`'s
//! `nestest_evidence::run` (see that module's doc): this file independently
//! reimplements the tiny `$6000`/RAM-result protocol readers rather than
//! depending on `rf-harness` (which would invert the crate dependency
//! direction — `rf-harness` depends on `rf-nes`, never the reverse).
//!
//! ## ROM availability (gitignored `roms/`, NFR-006)
//!
//! Fetch with `scripts/fetch-test-roms.sh` (no arguments fetches every
//! artifact, including these). Same absence-skip discipline as
//! `crate::cpu::tests::vectors` and `crate::system::tests::nestest`:
//! `cargo test --workspace` must pass on a fresh checkout with these files
//! absent, so these tests print why and return rather than failing or
//! hanging.
//!
//! ## Why `ppu_vbl_nmi` doesn't assert 10/10
//!
//! Six of the ten real `ppu_vbl_nmi` sub-ROMs fail against this crate for a
//! documented, investigated reason (`crate::ppu`'s module doc "Scope
//! fence" section; `crates/rf-harness/waivers.toml`'s six matching
//! entries) — a sub-CPU-cycle timing ceiling, not a regression. Asserting
//! 10/10 here would make `cargo test --workspace` permanently fail for
//! every contributor who fetches these ROMs, for a gap this ticket already
//! tracks as evidence, not silently. This test instead asserts only that
//! the four ROMs known to pass keep passing (a regression there IS a real
//! bug), and reports the other six's status without asserting on it.
use std::path::{Path, PathBuf};

use crate::cpu::Cpu;
use crate::system::NesBus;

const VALIDITY_SIGNATURE: [u8; 3] = [0xDE, 0xB0, 0x61];

/// Resolves a fetched-artifact path relative to this crate, `None` (the
/// absence-skip path) if it isn't a real file — same convention as
/// `crate::system::tests::nestest::resolve`, without the env-var override
/// (this suite never runs in CI, so there's no cache-path flexibility to
/// support).
fn resolve(default_rel: &str) -> Option<PathBuf> {
    let default = Path::new(env!("CARGO_MANIFEST_DIR")).join(default_rel);
    if default.is_file() {
        Some(default)
    } else {
        None
    }
}

/// Drives `rom_path` through the real reset vector and polls `$6000+`
/// frame-by-frame (via `NesBus::frame_count`) until the protocol reports a
/// final status or `max_frames` elapses. Returns `Some(status_byte)` (the
/// blargg readme's `$00-$7F` = "completed and gave that result code";
/// `$00` = passed) or `None` on timeout.
fn run_six_thousand(rom_path: &Path, max_frames: u32) -> Option<u8> {
    let rom_bytes = std::fs::read(rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let mut bus = NesBus::from_ines_bytes(&rom_bytes)
        .unwrap_or_else(|e| panic!("invalid rom image {}: {e}", rom_path.display()));
    let mut cpu = Cpu::power_on(&mut bus);

    for _ in 0..max_frames {
        let start_frame = bus.frame_count();
        while bus.frame_count() == start_frame {
            cpu.step(&mut bus);
        }
        let region = bus.prg_ram();
        if region[1..4] != VALIDITY_SIGNATURE {
            continue;
        }
        let status = region[0];
        if status != 0x80 {
            return Some(status);
        }
    }
    None
}

/// Drives `rom_path` for exactly `max_frames`, then reads the RAM byte at
/// `result_addr` — see `rf_harness::blargg_evidence::run_ram_result`'s doc
/// for why this protocol has no earlier-completion signal to poll for.
fn run_ram_result(rom_path: &Path, max_frames: u32, result_addr: u16) -> u8 {
    let rom_bytes = std::fs::read(rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let mut bus = NesBus::from_ines_bytes(&rom_bytes)
        .unwrap_or_else(|e| panic!("invalid rom image {}: {e}", rom_path.display()));
    let mut cpu = Cpu::power_on(&mut bus);

    for _ in 0..max_frames {
        let start_frame = bus.frame_count();
        while bus.frame_count() == start_frame {
            cpu.step(&mut bus);
        }
    }
    bus.peek(result_addr)
}

#[test]
fn sprite_hit_tests_all_eleven_pass() {
    let names = [
        "01.basics",
        "02.alignment",
        "03.corners",
        "04.flip",
        "05.left_clip",
        "06.right_edge",
        "07.screen_bottom",
        "08.double_height",
        "09.timing_basics",
        "10.timing_order",
        "11.edge_timing",
    ];

    let mut missing = 0;
    let mut failures = Vec::new();
    for name in names {
        let rel = format!("../../roms/nes/sprite_hit_tests_2005.10.05/{name}.nes");
        let Some(rom_path) = resolve(&rel) else {
            missing += 1;
            continue;
        };
        let value = run_ram_result(&rom_path, 600, 0x00F8);
        if value != 1 {
            failures.push(format!("{name}: result byte = {value} (1 = pass)"));
        }
    }

    if missing == names.len() {
        eprintln!(
            "SKIP sprite_hit_tests_all_eleven_pass: sprite_hit_tests_2005.10.05/*.nes not found. \
             Fetch them first: scripts/fetch-test-roms.sh"
        );
        return;
    }
    assert!(
        missing == 0,
        "found some but not all 11 sprite_hit_tests ROMs (partial fetch?) -- {missing} missing"
    );
    assert!(
        failures.is_empty(),
        "sprite_hit_tests failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn ppu_vbl_nmi_known_good_roms_still_pass() {
    // (name, frame_budget, "must currently pass" per this ticket's real,
    // investigated evidence -- see this file's module doc).
    let roms: [(&str, u32, bool); 10] = [
        ("01-vbl_basics", 3600, true),
        ("02-vbl_set_time", 3600, false),
        ("03-vbl_clear_time", 3600, true),
        ("04-nmi_control", 3600, true),
        ("05-nmi_timing", 3600, false),
        ("06-suppression", 3600, false),
        ("07-nmi_on_timing", 3600, false),
        ("08-nmi_off_timing", 3600, false),
        ("09-even_odd_frames", 3600, true),
        ("10-even_odd_timing", 3600, false),
    ];

    let mut missing = 0;
    let mut regressions = Vec::new();
    for (name, frame_budget, must_pass) in roms {
        let rel = format!("../../roms/nes/ppu_vbl_nmi/rom_singles/{name}.nes");
        let Some(rom_path) = resolve(&rel) else {
            missing += 1;
            continue;
        };
        let status = run_six_thousand(&rom_path, frame_budget);
        let passed = status == Some(0);
        eprintln!(
            "ppu_vbl_nmi/{name}: {}",
            match status {
                Some(0) => "PASS".to_string(),
                Some(code) => format!("FAIL (code ${code:02X})"),
                None => "TIMEOUT".to_string(),
            }
        );
        if must_pass && !passed {
            regressions.push(format!(
                "{name}: expected to pass (previously verified) but got {status:?}"
            ));
        }
    }

    if missing == roms.len() {
        eprintln!(
            "SKIP ppu_vbl_nmi_known_good_roms_still_pass: ppu_vbl_nmi/rom_singles/*.nes not \
             found. Fetch them first: scripts/fetch-test-roms.sh"
        );
        return;
    }
    assert!(
        missing == 0,
        "found some but not all 10 ppu_vbl_nmi rom_singles (partial fetch?) -- {missing} missing"
    );
    assert!(
        regressions.is_empty(),
        "ppu_vbl_nmi regressions in previously-passing ROMs:\n{}",
        regressions.join("\n")
    );
}
