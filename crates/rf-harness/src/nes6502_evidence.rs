//! nes6502 SingleStepTests vector runner for local-gate evidence (ticket
//! W0-07, `docs/evidence/local-gate.json`).
//!
//! This is a **second, independent** peer of
//! `crates/rf-nes/src/cpu/tests/vectors.rs` — not a refactor of it and not
//! a wrapper around `cargo test -p rf-nes`. Two reasons neither of those
//! alternatives works:
//!
//! 1. `crates/rf-nes/**` is outside this ticket's write scope, and
//!    `vectors.rs` lives under a `#[cfg(test)]`-gated module tree, so it
//!    is not reachable from a normal (non-test) build of `rf-nes` at all
//!    — there is nothing here to "call into".
//! 2. Shelling out to `cargo test -p rf-nes ... ` and keying evidence off
//!    the *exit code* would be silently vacuous: `vectors.rs`'s own tests
//!    skip-and-return `Ok` when the vectors directory is absent (by
//!    design — `cargo test --workspace` must pass on a fresh checkout).
//!    An exit-code-only instrument can therefore report "pass" against
//!    zero cases, which is exactly the fake-success shape this project's
//!    RF-L-08 lesson warns about. [`run_all`] instead counts cases and
//!    opcode files directly, so its caller ([`crate::bin::local_gate_evidence`],
//!    via `scripts/local-gate.sh`) can assert `opcodes_tested == 256` and
//!    `total_fail == 0` and refuse to emit evidence otherwise — see that
//!    binary.
//!
//! `rf-harness` is allowed to depend on `rf-nes` directly —
//! `scripts/validate-arch.sh`'s layering rule exempts "the test harness"
//! by name, alongside the app shell and the cores themselves.
//!
//! If `rf_nes::Cpu`'s public API changes, this is the file to update; a
//! silent mismatch would show up as this evidence diverging from what
//! `cargo test -p rf-nes` actually reports, which is exactly the kind of
//! drift `scripts/validate-evidence.mjs`'s staleness check exists to
//! catch (see that script and `docs/TESTING.md` §4).
//!
//! Comparison rule replicated **exactly** from `vectors.rs`
//! (`state_matches`): `final.p` is compared with [`rf_nes::cpu::FLAG_B`]
//! masked out, because 13 of the 256 unofficial-opcode vector files have
//! `final.p` bit 4 set in 100% of their cases as a SingleStepTests
//! generation artifact (the `B` flag has no physical existence outside a
//! stack push on real 6502 hardware) — see `vectors.rs`'s doc comment for
//! the full citation. A naive unmasked comparison here would report
//! roughly 130,000 spurious failures across those 13 files.
use std::path::Path;

use crate::vector_json::{self, Vector};
use rf_nes::cpu::FLAG_B;
use rf_nes::{Cpu, CpuBus};

/// Per-opcode result.
#[derive(Debug)]
pub struct OpcodeResult {
    pub opcode: u8,
    pub pass: usize,
    pub fail: usize,
    pub first_failure: Option<String>,
}

/// Aggregate result of running every `<opcode>.json` file found in a
/// vectors directory.
#[derive(Debug)]
pub struct Nes6502Summary {
    pub opcodes_tested: usize,
    pub total_pass: usize,
    pub total_fail: usize,
    /// Only opcodes with `fail > 0` — kept for diagnostics, not counted
    /// twice.
    pub failing: Vec<OpcodeResult>,
}

/// A full 64 KiB address space, initialized from a vector's `initial.ram`,
/// recording every access `Cpu::step` makes — identical in shape to
/// `vectors.rs`'s `RecordingBus` (see module doc for why this is a
/// separate copy rather than a shared one).
struct RecordingBus {
    ram: Box<[u8; 65536]>,
    trace: Vec<(u16, u8, bool)>,
}

impl RecordingBus {
    fn new(initial_ram: &[(u16, u8)]) -> Self {
        let mut ram = Box::new([0u8; 65536]);
        for &(addr, val) in initial_ram {
            ram[addr as usize] = val;
        }
        RecordingBus {
            ram,
            trace: Vec::new(),
        }
    }
}

impl CpuBus for RecordingBus {
    fn read(&mut self, addr: u16) -> u8 {
        let value = self.ram[addr as usize];
        self.trace.push((addr, value, false));
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.ram[addr as usize] = value;
        self.trace.push((addr, value, true));
    }
}

fn state_matches(cpu: &Cpu, want: &vector_json::CpuState) -> bool {
    cpu.pc == want.pc
        && cpu.s == want.s
        && cpu.a == want.a
        && cpu.x == want.x
        && cpu.y == want.y
        && cpu.p == want.p & !FLAG_B
}

fn run_one(v: &Vector<'_>) -> Result<(), String> {
    // Unlike `vectors.rs` (inside the `rf-nes` crate, where `Cpu { .. }`
    // struct-literal + `..Cpu::default()` functional-update syntax is
    // legal because privacy is checked per-module, not per-field),
    // `rf-harness` is a *different* crate: `Cpu`'s interrupt-bookkeeping
    // fields are private, so a struct literal naming this type at all
    // — even with `..Cpu::default()` filling in the rest — does not
    // type-check here. Plain field assignment through the public
    // `a`/`x`/`y`/`s`/`pc` fields (and the public `set_p` setter) has no
    // such restriction.
    let mut cpu = Cpu::default();
    cpu.a = v.initial.a;
    cpu.x = v.initial.x;
    cpu.y = v.initial.y;
    cpu.s = v.initial.s;
    cpu.pc = v.initial.pc;
    cpu.set_p(v.initial.p);

    let mut bus = RecordingBus::new(&v.initial.ram);
    let reported_cycles = cpu.step(&mut bus);

    if !state_matches(&cpu, &v.expected_final) {
        return Err(format!(
            "case {}: register mismatch: got pc={:04X} s={:02X} a={:02X} x={:02X} y={:02X} p={:02X}, \
             want pc={:04X} s={:02X} a={:02X} x={:02X} y={:02X} p={:02X}",
            v.name,
            cpu.pc, cpu.s, cpu.a, cpu.x, cpu.y, cpu.p,
            v.expected_final.pc, v.expected_final.s, v.expected_final.a,
            v.expected_final.x, v.expected_final.y, v.expected_final.p
        ));
    }

    for &(addr, expected) in &v.expected_final.ram {
        let got = bus.ram[addr as usize];
        if got != expected {
            return Err(format!(
                "case {}: ram[${addr:04X}] = ${got:02X}, want ${expected:02X}",
                v.name
            ));
        }
    }

    if bus.trace.len() != v.cycles.len() {
        return Err(format!(
            "case {}: bus trace has {} operations, want {}",
            v.name,
            bus.trace.len(),
            v.cycles.len()
        ));
    }
    if reported_cycles as usize != v.cycles.len() {
        return Err(format!(
            "case {}: Cpu::step returned {reported_cycles} cycles, want {}",
            v.name,
            v.cycles.len()
        ));
    }
    for (i, (got, want)) in bus.trace.iter().zip(v.cycles.iter()).enumerate() {
        let &(got_addr, got_val, got_write) = got;
        if got_addr != want.addr || got_val != want.value || got_write != want.is_write {
            return Err(format!(
                "case {}: bus op {} mismatch: got (${:04X}, ${:02X}, {}), want (${:04X}, ${:02X}, {})",
                v.name,
                i,
                got_addr,
                got_val,
                if got_write { "write" } else { "read" },
                want.addr,
                want.value,
                if want.is_write { "write" } else { "read" }
            ));
        }
    }

    Ok(())
}

fn run_opcode_file(dir: &Path, opcode: u8) -> Result<OpcodeResult, String> {
    let path = dir.join(format!("{opcode:02x}.json"));
    let bytes =
        std::fs::read(&path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let vectors = vector_json::parse_vectors(&bytes);
    let mut pass = 0;
    let mut fail = 0;
    let mut first_failure = None;
    for v in &vectors {
        match run_one(v) {
            Ok(()) => pass += 1,
            Err(msg) => {
                fail += 1;
                if first_failure.is_none() {
                    first_failure = Some(msg);
                }
            }
        }
    }
    Ok(OpcodeResult {
        opcode,
        pass,
        fail,
        first_failure,
    })
}

/// Runs every `<opcode>.json` file present in `dir` for `opcode` in
/// `0x00..=0xFF` (files that don't exist are skipped, not errors — the
/// real dataset has all 256; a partial directory just yields a lower
/// `opcodes_tested`, which the caller checks against the expected 256
/// rather than trusting silently).
///
/// # Errors
/// Returns `Err` if `dir` cannot be read at all (e.g. doesn't exist) — the
/// caller decides what "no evidence to generate" means, this function
/// does not paper over it with a zero-count success.
pub fn run_all(dir: &Path) -> Result<Nes6502Summary, String> {
    if !dir.is_dir() {
        return Err(format!("vectors directory not found: {}", dir.display()));
    }

    let mut opcodes_tested = 0usize;
    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    let mut failing = Vec::new();

    for opcode in 0u16..=0xFF {
        let opcode = opcode as u8;
        let path = dir.join(format!("{opcode:02x}.json"));
        if !path.is_file() {
            continue;
        }
        let result = run_opcode_file(dir, opcode)?;
        opcodes_tested += 1;
        total_pass += result.pass;
        total_fail += result.fail;
        if result.fail > 0 {
            failing.push(result);
        }
    }

    Ok(Nes6502Summary {
        opcodes_tested,
        total_pass,
        total_fail,
        failing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rf-harness-nes6502-evidence-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// LDA #$00 (opcode $A9) starting at PC=0x0100: reads the immediate
    /// operand ($00), sets A=0, N=0, Z=1. Hand-derived, not copied from
    /// the real dataset (no ROM/vector bytes committed here).
    const LDA_IMM_ZERO: &str = r#"[{"name":"a9 00","initial":{"pc":256,"s":253,"a":170,"x":0,"y":0,"p":36,"ram":[[256,169],[257,0]]},"final":{"pc":258,"s":253,"a":0,"x":0,"y":0,"p":38,"ram":[[256,169],[257,0]]},"cycles":[[256,169,"read"],[257,0,"read"]]}]"#;

    #[test]
    fn run_all_passes_on_a_correct_hand_derived_case() {
        let dir = tempdir();
        fs::write(dir.join("a9.json"), LDA_IMM_ZERO).unwrap();

        let summary = run_all(&dir).expect("dir exists");
        assert_eq!(summary.opcodes_tested, 1);
        assert_eq!(summary.total_pass, 1);
        assert_eq!(summary.total_fail, 0);
        assert!(summary.failing.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_all_reports_a_deliberately_wrong_expectation_as_a_failure() {
        let dir = tempdir();
        // Same case as LDA_IMM_ZERO, but `final.a` is deliberately wrong
        // (should be 0, claims 5).
        let case = r#"[{"name":"a9 00 bad","initial":{"pc":256,"s":253,"a":170,"x":0,"y":0,"p":36,"ram":[[256,169],[257,0]]},"final":{"pc":258,"s":253,"a":5,"x":0,"y":0,"p":38,"ram":[[256,169],[257,0]]},"cycles":[[256,169,"read"],[257,0,"read"]]}]"#;
        fs::write(dir.join("a9.json"), case).unwrap();

        let summary = run_all(&dir).expect("dir exists");
        assert_eq!(summary.opcodes_tested, 1);
        assert_eq!(summary.total_pass, 0);
        assert_eq!(summary.total_fail, 1);
        assert_eq!(summary.failing.len(), 1);
        assert_eq!(summary.failing[0].opcode, 0xA9);

        fs::remove_dir_all(&dir).ok();
    }

    /// The load-bearing FLAG_B-masking regression test: a case whose
    /// `final.p` has bit 4 (FLAG_B) set — matching the 13 real
    /// SingleStepTests files documented in `vectors.rs` — must still pass,
    /// because `Cpu::p` never latches that bit (see module doc / FLAG_B).
    #[test]
    fn final_p_with_flag_b_set_is_masked_and_still_passes() {
        let dir = tempdir();
        // final.p = 38 | 0x10 (FLAG_B) = 54.
        let case = r#"[{"name":"a9 00 flagb","initial":{"pc":256,"s":253,"a":170,"x":0,"y":0,"p":36,"ram":[[256,169],[257,0]]},"final":{"pc":258,"s":253,"a":0,"x":0,"y":0,"p":54,"ram":[[256,169],[257,0]]},"cycles":[[256,169,"read"],[257,0,"read"]]}]"#;
        fs::write(dir.join("a9.json"), case).unwrap();

        let summary = run_all(&dir).expect("dir exists");
        assert_eq!(summary.total_pass, 1);
        assert_eq!(summary.total_fail, 0);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_directory_is_an_error_not_a_zero_count_success() {
        let err = run_all(Path::new("/definitely/does/not/exist/anywhere")).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn skips_opcodes_with_no_file_rather_than_erroring() {
        let dir = tempdir();
        fs::write(dir.join("a9.json"), LDA_IMM_ZERO).unwrap();
        // Only a9.json exists; the other 255 possible files are absent.
        let summary = run_all(&dir).expect("dir exists");
        assert_eq!(summary.opcodes_tested, 1);

        fs::remove_dir_all(&dir).ok();
    }
}
