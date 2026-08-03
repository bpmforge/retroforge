//! nestest golden-trace conformance test (ticket W1-03 acceptance criteria
//! 1 + 2, FR-DBG-003 + FR-CORE-021): runs the real `nestest.nes` ROM from
//! its automated-test-mode entry point (`$C000`) and diffs every line of
//! `crate::trace::format_trace_line`'s output against the real, fetched
//! `nestest.log`, byte-for-byte.
//!
//! ## ROM/log availability (gitignored `roms/`, NFR-006)
//!
//! Fetch with `scripts/fetch-test-roms.sh nestest-rom nestest-log`. Same
//! absence-skip discipline as `crate::cpu::tests::vectors` — and the same
//! reason: `cargo test --workspace` must pass on a fresh checkout with
//! these files absent, so this test prints why and returns rather than
//! failing or hanging.
//!
//! `RF_NESTEST_ROM`/`RF_NESTEST_LOG` override the file paths; the defaults
//! are `<CARGO_MANIFEST_DIR>/../../roms/nes/other/nestest.{nes,log}` (the
//! layout `scripts/fetch-test-roms.sh` produces).
//!
//! ## Format notes (ticket W1-03 pre-flight, verified against the real
//! fetched log)
//!
//! `nestest.log` is CRLF-terminated (8991 lines) — the trailing `\r` is
//! stripped from each line before comparison, or every line diff fails.
//! No log content is copied into this file (TESTING.md §3's no-vendor/
//! no-rehost rule, G-43, names `nestest/.log` explicitly) — this test only
//! ever reads the real file from disk at run time.
use std::path::{Path, PathBuf};

use crate::cpu::Cpu;
use crate::system::{NesBus, NesRom};
use crate::trace::format_trace_line;

/// Resolves a fetched-artifact path: `env_var` if set and it names a real
/// file, else the default relative-to-this-crate path if *that* is a real
/// file, else `None` (the absence-skip path — never panics).
fn resolve(env_var: &str, default_rel: &str) -> Option<PathBuf> {
    if let Ok(configured) = std::env::var(env_var) {
        let path = PathBuf::from(configured);
        return if path.is_file() { Some(path) } else { None };
    }
    let default = Path::new(env!("CARGO_MANIFEST_DIR")).join(default_rel);
    if default.is_file() {
        Some(default)
    } else {
        None
    }
}

#[test]
fn nestest_golden_trace_matches_byte_exact() {
    let Some(rom_path) = resolve("RF_NESTEST_ROM", "../../roms/nes/other/nestest.nes") else {
        eprintln!(
            "SKIP nestest_golden_trace_matches_byte_exact: nestest.nes not found. Set \
             RF_NESTEST_ROM or fetch it at the default path: \
             scripts/fetch-test-roms.sh nestest-rom"
        );
        return;
    };
    let Some(log_path) = resolve("RF_NESTEST_LOG", "../../roms/nes/other/nestest.log") else {
        eprintln!(
            "SKIP nestest_golden_trace_matches_byte_exact: nestest.log not found. Set \
             RF_NESTEST_LOG or fetch it at the default path: \
             scripts/fetch-test-roms.sh nestest-log"
        );
        return;
    };

    let rom_bytes = std::fs::read(&rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let log_bytes = std::fs::read(&log_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", log_path.display()));
    let log_text = String::from_utf8(log_bytes).expect("nestest.log is ASCII/UTF-8");

    // CRLF-terminated (ticket W1-03 pre-flight) — strip the trailing '\r'
    // from each line, and drop the empty trailing split the final
    // terminator produces.
    let golden_lines: Vec<&str> = log_text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.is_empty())
        .collect();

    let rom = NesRom::from_ines_bytes(&rom_bytes).expect("valid nestest.nes image");
    let mut bus = NesBus::new(rom);
    let mut cpu = Cpu::power_on(&mut bus);
    // nestest's automated (non-interactive) test-mode entry point: bypasses
    // the ROM's own reset-vector code path and starts execution at $C000
    // directly. Nestest-specific test-harness behavior, not a general
    // reset semantic — see `Cpu::power_on`'s doc.
    cpu.pc = 0xC000;

    let mut total = 0usize;
    let mut mismatches = 0usize;
    let mut first_divergence: Option<(usize, String, String)> = None;

    for &expected in &golden_lines {
        let got = format_trace_line(&cpu, &bus, bus.master_cycle());
        total += 1;
        if got != expected {
            mismatches += 1;
            if first_divergence.is_none() {
                first_divergence = Some((total, expected.to_string(), got.clone()));
            }
        }
        cpu.step(&mut bus);
    }

    eprintln!(
        "nestest: {total} lines compared, {} matched, {mismatches} mismatched",
        total - mismatches
    );
    if let Some((line_no, expected, got)) = &first_divergence {
        eprintln!("  first divergence at line {line_no}:");
        eprintln!("    expected: {expected}");
        eprintln!("    got:      {got}");
    }
    assert_eq!(
        total, 8991,
        "nestest.log is documented as exactly 8991 lines (ticket W1-03 pre-flight)"
    );
    assert_eq!(
        mismatches, 0,
        "{mismatches} of {total} nestest trace lines diverged from the golden log (see stderr for the first)"
    );
}
