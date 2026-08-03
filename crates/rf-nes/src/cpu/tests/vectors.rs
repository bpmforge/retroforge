//! SingleStepTests `nes6502` vector harness (FR-CORE-020, acceptance
//! criterion 1 of ticket W1-01a).
//!
//! Replays every official-opcode test case from
//! `SingleStepTests/ProcessorTests` (pinned commit
//! `bb11756436da8fd16cce86aef63dc6725f48836f`, `tests/rom-manifest.toml`
//! artifact `singlestep-nes6502`) against [`Cpu::step`] and diffs both the
//! final register/RAM state *and* the cycle-by-cycle bus trace.
//!
//! ## Vector availability (gitignored `roms/`, NFR-006)
//!
//! `tests/rom-manifest.toml`'s pinned hash for artifact `singlestep-nes6502`
//! does not match the bytes `codeload.github.com` actually serves for that
//! commit today: verified this session, the manifest declares
//! `ed89588aa3de8cf087861de57f079f3a5ab050dd9de3dfcfa07598b65db6cf75`, the
//! real download from the pinned-commit URL hashes to
//! `05b602aeb508c62c5aaee259475f8d5e949ed4a9aba0f8220b67ba78708d2d71` —
//! `scripts/fetch-test-roms.sh singlestep-nes6502` fails closed on that
//! mismatch for anyone who runs the documented path. `tests/rom-manifest.toml`
//! is outside this ticket's write scope to fix.
//!
//! Separately, that artifact is a zip of the *entire* `ProcessorTests` repo
//! (every CPU architecture the SingleStepTests project covers), which is
//! far larger than needed — `nes6502/v1` alone is already ~1.0 GB
//! uncompressed at this commit (measured this session via `du -sh` after
//! extraction). Given both the hash mismatch and the whole-repo zip's size,
//! fetch only the `nes6502/v1` subset with a partial+sparse git clone:
//!
//! ```sh
//! mkdir -p roms/nes/singlestep-nes6502-src
//! cd roms/nes/singlestep-nes6502-src
//! git init -q
//! git remote add origin https://github.com/SingleStepTests/ProcessorTests.git
//! git sparse-checkout init --cone
//! git sparse-checkout set nes6502/v1
//! git fetch --depth 1 --filter=blob:none origin bb11756436da8fd16cce86aef63dc6725f48836f
//! git checkout FETCH_HEAD
//! ```
//!
//! That reproduces exactly the commit the manifest pins, without the
//! per-artifact hash gate. `roms/` is gitignored (`.gitignore` line 5,
//! NFR-006) regardless of which path populates it.
//!
//! `RF_NES6502_VECTORS` overrides the vector directory; the default is
//! `<CARGO_MANIFEST_DIR>/../../roms/nes/singlestep-nes6502-src/nes6502/v1`
//! (i.e. the layout the command above produces). When neither the env var
//! nor the default directory resolves to a real directory, the test below
//! prints why and returns — it never fails or hangs on a machine that
//! hasn't fetched the vectors (this ticket's hard requirement: `cargo test
//! --workspace` must pass with the vectors absent).
use std::path::{Path, PathBuf};

use super::json::{self, CpuState};
use crate::cpu::bus::CpuBus;
use crate::cpu::Cpu;

/// The 151 official 6502 opcodes this ticket implements, grouped by
/// opcode-matrix row exactly as tallied in `cpu/exec.rs`'s module doc
/// (151 total, cross-checked by
/// `super::opcode_table::official_opcode_count_is_151`).
#[rustfmt::skip]
pub(crate) const OFFICIAL_OPCODES: [u8; 151] = [
    // row +00: BRK JSR RTI RTS LDY# CPY# CPX#
    0x00, 0x20, 0x40, 0x60, 0xA0, 0xC0, 0xE0,
    // row +01: ORA AND EOR ADC STA LDA CMP SBC (indir,x)
    0x01, 0x21, 0x41, 0x61, 0x81, 0xA1, 0xC1, 0xE1,
    // row +02: LDX#
    0xA2,
    // row +04: BIT STY LDY CPY CPX (zp)
    0x24, 0x84, 0xA4, 0xC4, 0xE4,
    // row +05: ORA AND EOR ADC STA LDA CMP SBC (zp)
    0x05, 0x25, 0x45, 0x65, 0x85, 0xA5, 0xC5, 0xE5,
    // row +06: ASL ROL LSR ROR STX LDX DEC INC (zp)
    0x06, 0x26, 0x46, 0x66, 0x86, 0xA6, 0xC6, 0xE6,
    // row +08: PHP PLP PHA PLA DEY TAY INY INX (implied)
    0x08, 0x28, 0x48, 0x68, 0x88, 0xA8, 0xC8, 0xE8,
    // row +09: ORA AND EOR ADC LDA CMP SBC (imm)
    0x09, 0x29, 0x49, 0x69, 0xA9, 0xC9, 0xE9,
    // row +0a: ASL ROL LSR ROR TXA TAX DEX NOP (accu/implied)
    0x0A, 0x2A, 0x4A, 0x6A, 0x8A, 0xAA, 0xCA, 0xEA,
    // row +0c: BIT JMP JMP() STY LDY CPY CPX (abs)
    0x2C, 0x4C, 0x6C, 0x8C, 0xAC, 0xCC, 0xEC,
    // row +0d: ORA AND EOR ADC STA LDA CMP SBC (abs)
    0x0D, 0x2D, 0x4D, 0x6D, 0x8D, 0xAD, 0xCD, 0xED,
    // row +0e: ASL ROL LSR ROR STX LDX DEC INC (abs)
    0x0E, 0x2E, 0x4E, 0x6E, 0x8E, 0xAE, 0xCE, 0xEE,
    // row +10: BPL BMI BVC BVS BCC BCS BNE BEQ (relative)
    0x10, 0x30, 0x50, 0x70, 0x90, 0xB0, 0xD0, 0xF0,
    // row +11: ORA AND EOR ADC STA LDA CMP SBC (indir),y
    0x11, 0x31, 0x51, 0x71, 0x91, 0xB1, 0xD1, 0xF1,
    // row +14: STY LDY (zp,x)
    0x94, 0xB4,
    // row +15: ORA AND EOR ADC STA LDA CMP SBC (zp,x)
    0x15, 0x35, 0x55, 0x75, 0x95, 0xB5, 0xD5, 0xF5,
    // row +16: ASL ROL LSR ROR STX(zp,y) LDX(zp,y) DEC INC (zp,x)
    0x16, 0x36, 0x56, 0x76, 0x96, 0xB6, 0xD6, 0xF6,
    // row +18: CLC SEC CLI SEI TYA CLV CLD SED (implied)
    0x18, 0x38, 0x58, 0x78, 0x98, 0xB8, 0xD8, 0xF8,
    // row +19: ORA AND EOR ADC STA LDA CMP SBC (abs,y)
    0x19, 0x39, 0x59, 0x79, 0x99, 0xB9, 0xD9, 0xF9,
    // row +1a: TXS TSX (implied)
    0x9A, 0xBA,
    // row +1c: LDY (abs,x)
    0xBC,
    // row +1d: ORA AND EOR ADC STA LDA CMP SBC (abs,x)
    0x1D, 0x3D, 0x5D, 0x7D, 0x9D, 0xBD, 0xDD, 0xFD,
    // row +1e: ASL ROL LSR ROR LDX(abs,y) DEC INC (abs,x)
    0x1E, 0x3E, 0x5E, 0x7E, 0xBE, 0xDE, 0xFE,
];

/// A full 64 KiB address space, initialized from a vector's `initial.ram`,
/// recording every access [`Cpu::step`] makes so it can be diffed against
/// the vector's `cycles` array operation-for-operation.
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

fn state_matches(cpu: &Cpu, want: &CpuState) -> bool {
    cpu.pc == want.pc
        && cpu.s == want.s
        && cpu.a == want.a
        && cpu.x == want.x
        && cpu.y == want.y
        && cpu.p == want.p
}

/// Replays one test case; `Err` names the first discrepancy (register
/// state, RAM, or bus-trace operation) found.
fn run_one(v: &json::Vector<'_>) -> Result<(), String> {
    let mut cpu = Cpu {
        a: v.initial.a,
        x: v.initial.x,
        y: v.initial.y,
        s: v.initial.s,
        pc: v.initial.pc,
        p: 0,
    };
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
            "case {}: bus trace has {} operations, want {} — trace: {:?}, expected: {:?}",
            v.name,
            bus.trace.len(),
            v.cycles.len(),
            bus.trace,
            v.cycles
                .iter()
                .map(|c| (c.addr, c.value, c.is_write))
                .collect::<Vec<_>>()
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

/// Resolves the vector directory: `RF_NES6502_VECTORS` if set, else the
/// default path documented in the module doc. Returns `None` (never
/// panics) if neither exists — the absence-skip path.
fn vectors_dir() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("RF_NES6502_VECTORS") {
        let path = PathBuf::from(configured);
        return if path.is_dir() { Some(path) } else { None };
    }
    let default = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../roms/nes/singlestep-nes6502-src/nes6502/v1");
    if default.is_dir() {
        Some(default)
    } else {
        None
    }
}

/// Runs every case in `<dir>/<opcode>.json`; returns `(pass, fail,
/// first_failure_message)`.
fn run_opcode_file(dir: &Path, opcode: u8) -> (usize, usize, Option<String>) {
    let path = dir.join(format!("{opcode:02x}.json"));
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("nes6502 vectors: failed to read {}: {e}", path.display()));
    let vectors = json::parse_vectors(&bytes);
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
    (pass, fail, first_failure)
}

/// Acceptance criterion 1: every official opcode's full SingleStepTests
/// vector file passes, state *and* cycle-by-cycle bus trace. Skips (does
/// not fail) when the vectors aren't present locally — see module doc.
#[test]
fn nes6502_official_opcode_vectors() {
    let Some(dir) = vectors_dir() else {
        eprintln!(
            "SKIP nes6502_official_opcode_vectors: vectors not found. Set RF_NES6502_VECTORS \
             or fetch them at the default path — see the doc comment on \
             crates/rf-nes/src/cpu/tests/vectors.rs for the exact commands."
        );
        return;
    };

    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    let mut failing_opcodes: Vec<(u8, usize, usize, String)> = Vec::new();

    for &opcode in &OFFICIAL_OPCODES {
        let (pass, fail, first_failure) = run_opcode_file(&dir, opcode);
        total_pass += pass;
        total_fail += fail;
        if fail > 0 {
            failing_opcodes.push((opcode, pass, fail, first_failure.unwrap_or_default()));
        }
    }

    eprintln!(
        "nes6502 vectors: {} opcodes tested, {total_pass} cases passed, {total_fail} cases failed",
        OFFICIAL_OPCODES.len()
    );
    if !failing_opcodes.is_empty() {
        for (opcode, pass, fail, msg) in &failing_opcodes {
            eprintln!("  ${opcode:02X}: {pass} passed, {fail} failed; first failure: {msg}");
        }
        panic!(
            "{} of {} official opcodes have failing nes6502 vector cases (see stderr above)",
            failing_opcodes.len(),
            OFFICIAL_OPCODES.len()
        );
    }
}
