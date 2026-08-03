//! nestest golden-trace runner for local-gate evidence (ticket W1-03,
//! `docs/evidence/local-gate.json`'s second Tier-A-local row).
//!
//! This is a **second, independent** peer of
//! `crates/rf-nes/src/system/tests/nestest.rs` — not a refactor of it and
//! not a wrapper around `cargo test -p rf-nes` — for exactly the same two
//! reasons `nes6502_evidence.rs` is a peer of `cpu/tests/vectors.rs` (see
//! that module's doc): `crates/rf-nes/**` is outside this ticket's write
//! scope and its test tree is `#[cfg(test)]`-gated, unreachable from a
//! normal build; and keying evidence off `cargo test`'s exit code would be
//! silently vacuous, since that test skips-and-passes when the ROM/log are
//! absent (by design — `cargo test --workspace` must pass on a fresh
//! checkout). [`run`] instead drives `rf_nes::Cpu::power_on` and
//! `rf_nes::trace::format_trace_line` directly and counts real lines
//! compared/matched, so its caller (`crates/rf-harness/src/bin/
//! local_gate_evidence.rs`, via `scripts/local-gate.sh`) can assert
//! `lines_compared == 8991` and `lines_matched == lines_compared` and
//! refuse to emit evidence otherwise.
//!
//! `rf-harness` is allowed to depend on `rf-nes` directly —
//! `scripts/validate-arch.sh`'s layering rule exempts "the test harness" by
//! name, alongside the app shell and the cores themselves.
use std::path::Path;

use rf_nes::trace::format_trace_line;
use rf_nes::{Cpu, NesBus, NesRom};

/// One real-vs-golden divergence, kept for diagnostics.
#[derive(Debug, Clone)]
pub struct Divergence {
    pub line_number: usize,
    pub expected: String,
    pub got: String,
}

/// Aggregate result of diffing the full nestest trace against the golden
/// log.
#[derive(Debug)]
pub struct NestestSummary {
    pub lines_compared: usize,
    pub lines_matched: usize,
    pub first_divergence: Option<Divergence>,
}

/// Runs nestest from its automated-test-mode entry point (`$C000`) and
/// diffs every trace line against `log_path`'s content.
///
/// # Errors
/// Returns `Err` if the ROM/log can't be read or parsed at all — the
/// caller decides what "no evidence to generate" means, this function
/// does not paper over it with a zero-count success.
pub fn run(rom_path: &Path, log_path: &Path) -> Result<NestestSummary, String> {
    let rom_bytes = std::fs::read(rom_path)
        .map_err(|e| format!("failed to read {}: {e}", rom_path.display()))?;
    let log_bytes = std::fs::read(log_path)
        .map_err(|e| format!("failed to read {}: {e}", log_path.display()))?;
    let log_text = String::from_utf8(log_bytes)
        .map_err(|e| format!("{} is not valid UTF-8: {e}", log_path.display()))?;

    // CRLF-terminated (ticket W1-03 pre-flight, verified against the real
    // fetched log) — strip the trailing '\r' from each line, and drop the
    // empty trailing split the final terminator produces.
    let golden_lines: Vec<&str> = log_text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.is_empty())
        .collect();

    let rom = NesRom::from_ines_bytes(&rom_bytes)
        .map_err(|e| format!("invalid nestest ROM image {}: {e}", rom_path.display()))?;
    let mut bus = NesBus::new(rom);
    let mut cpu = Cpu::power_on(&mut bus);
    // nestest's automated (non-interactive) test-mode entry point: bypasses
    // the ROM's own reset-vector code path and starts execution at $C000
    // directly — nestest-specific test-harness behavior, not a general
    // reset semantic (see `Cpu::power_on`'s doc).
    cpu.pc = 0xC000;

    let mut lines_matched = 0usize;
    let mut first_divergence = None;

    for (i, &expected) in golden_lines.iter().enumerate() {
        let got = format_trace_line(&cpu, &bus, bus.master_cycle());
        if got == expected {
            lines_matched += 1;
        } else if first_divergence.is_none() {
            first_divergence = Some(Divergence {
                line_number: i + 1,
                expected: expected.to_string(),
                got,
            });
        }
        cpu.step(&mut bus);
    }

    Ok(NestestSummary {
        lines_compared: golden_lines.len(),
        lines_matched,
        first_divergence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> std::path::PathBuf {
        // Tests in one binary run in PARALLEL THREADS sharing a process
        // id, and two threads can observe the same `SystemTime` tick, so
        // pid+nanos alone is NOT unique — a collision makes one test's
        // cleanup delete another's working directory. Observed twice as a
        // one-off flake during W1-03. The atomic counter closes the race.
        static TEMPDIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rf-harness-nestest-evidence-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            TEMPDIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A minimal synthetic NROM image whose reset vector points at $8000,
    /// where a single `SEI` (`$78`, implied, 2 cycles) sits — hand-derived,
    /// not real nestest content (TESTING.md §3 G-43: no vendoring
    /// nestest.nes/.log). Just enough to prove [`run`] reads two real
    /// files, drives `Cpu::power_on`, and diffs lines — not a nestest
    /// conformance claim.
    fn build_minimal_nrom() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
        data.push(1); // 1 PRG bank (16 KiB)
        data.push(1); // 1 CHR bank
        data.extend_from_slice(&[0u8; 10]); // flags6/7 + 8 reserved: mapper 0
        let mut prg = vec![0xEAu8; 16 * 1024]; // NOP-filled
        prg[0] = 0x78; // SEI at $C000 (mirrored from $8000)
        prg[0x3FFC] = 0x00; // reset vector low -> $C000
        prg[0x3FFD] = 0xC0; // reset vector high
        data.extend_from_slice(&prg);
        data.extend(vec![0u8; 8 * 1024]); // CHR
        data
    }

    #[test]
    fn run_matches_a_hand_derived_single_line_golden_file() {
        let dir = tempdir();
        let rom_path = dir.join("mini.nes");
        let log_path = dir.join("mini.log");
        fs::write(&rom_path, build_minimal_nrom()).unwrap();
        // SEI at $C000, A/X/Y=0, P=$24 (I set by Cpu::default, matching
        // nestest's own convention), SP=$FD, PPU derived from CYC:7 via the
        // documented (cyc*3)/341,(cyc*3)%341 identity -> 0,21.
        fs::write(
            &log_path,
            "C000  78        SEI                             A:00 X:00 Y:00 P:24 SP:FD PPU:  0, 21 CYC:7\r\n",
        )
        .unwrap();

        let summary = run(&rom_path, &log_path).expect("both files present and valid");
        assert_eq!(summary.lines_compared, 1);
        assert_eq!(summary.lines_matched, 1);
        assert!(summary.first_divergence.is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_reports_a_deliberately_wrong_golden_line_as_a_divergence() {
        let dir = tempdir();
        let rom_path = dir.join("mini.nes");
        let log_path = dir.join("mini.log");
        fs::write(&rom_path, build_minimal_nrom()).unwrap();
        fs::write(
            &log_path,
            "C000  78        SEI                             A:99 X:00 Y:00 P:24 SP:FD PPU:  0, 21 CYC:7\r\n",
        )
        .unwrap();

        let summary = run(&rom_path, &log_path).expect("both files present and valid");
        assert_eq!(summary.lines_compared, 1);
        assert_eq!(summary.lines_matched, 0);
        let d = summary
            .first_divergence
            .expect("must record the divergence");
        assert_eq!(d.line_number, 1);
        assert!(d.expected.contains("A:99"));
        assert!(d.got.contains("A:00"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_rom_is_an_error_not_a_zero_count_success() {
        let dir = tempdir();
        let log_path = dir.join("mini.log");
        fs::write(&log_path, "irrelevant\r\n").unwrap();
        let err = run(Path::new("/definitely/does/not/exist.nes"), &log_path).unwrap_err();
        assert!(err.contains("failed to read"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_log_is_an_error_not_a_zero_count_success() {
        let dir = tempdir();
        let rom_path = dir.join("mini.nes");
        fs::write(&rom_path, build_minimal_nrom()).unwrap();
        let err = run(&rom_path, Path::new("/definitely/does/not/exist.log")).unwrap_err();
        assert!(err.contains("failed to read"));
        fs::remove_dir_all(&dir).ok();
    }
}
