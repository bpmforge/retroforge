//! Static cross-checks between `cpu/exec.rs`'s dispatch `match` and the
//! independently-hand-authored [`super::vectors::OFFICIAL_OPCODES`]/
//! [`super::vectors::UNOFFICIAL_OPCODES`] lists — catch a typo'd,
//! missing, or duplicated opcode byte even with no vector data present
//! (unlike `super::vectors`, these need no external files).
use std::collections::HashSet;

use super::vectors::{OFFICIAL_OPCODES, UNOFFICIAL_OPCODES};
use crate::cpu::bus::CpuBus;
use crate::cpu::{exec, Cpu, UnstableOp};

/// Answers every read with 0 and discards every write — enough for
/// `exec::execute` to run any opcode's full cycle sequence without needing
/// meaningful operand bytes, since these tests only care about coverage
/// and dispatch counts, never register outcomes. Interrupt lines use the
/// `CpuBus` defaults (never asserted), which is fine here: `execute`
/// dispatches purely on the opcode byte and never blocks on line state.
struct SinkBus;

impl CpuBus for SinkBus {
    fn read(&mut self, _addr: u16) -> u8 {
        0
    }

    fn write(&mut self, _addr: u16, _value: u8) {}
}

#[test]
fn official_opcode_count_is_151() {
    assert_eq!(
        OFFICIAL_OPCODES.len(),
        151,
        "6502 has exactly 151 official opcodes"
    );
    let unique: HashSet<u8> = OFFICIAL_OPCODES.iter().copied().collect();
    assert_eq!(unique.len(), 151, "OFFICIAL_OPCODES has a duplicate entry");
}

#[test]
fn unofficial_opcode_count_is_105() {
    assert_eq!(
        UNOFFICIAL_OPCODES.len(),
        105,
        "6502 has exactly 105 unofficial/illegal opcodes (256 - 151 official)"
    );
    let unique: HashSet<u8> = UNOFFICIAL_OPCODES.iter().copied().collect();
    assert_eq!(
        unique.len(),
        105,
        "UNOFFICIAL_OPCODES has a duplicate entry"
    );
}

/// The real coverage guarantee (ticket W1-01b): `OFFICIAL_OPCODES` and
/// `UNOFFICIAL_OPCODES`, together, partition all 256 possible opcode
/// bytes exactly — no gap (a byte in neither list, silently unimplemented
/// or dispatched only by luck) and no overlap (a byte double-counted,
/// masking a real gap elsewhere). This is what would have caught a
/// mis-enumerated illegal opcode even with zero vector data present.
#[test]
fn opcode_lists_partition_all_256_bytes() {
    let official: HashSet<u8> = OFFICIAL_OPCODES.iter().copied().collect();
    let unofficial: HashSet<u8> = UNOFFICIAL_OPCODES.iter().copied().collect();

    let overlap: Vec<u8> = official.intersection(&unofficial).copied().collect();
    assert!(
        overlap.is_empty(),
        "opcode(s) listed as both official and unofficial: {overlap:02X?}"
    );

    let missing: Vec<u8> = (0u16..=255)
        .map(|b| b as u8)
        .filter(|b| !official.contains(b) && !unofficial.contains(b))
        .collect();
    assert!(
        missing.is_empty(),
        "opcode(s) in neither OFFICIAL_OPCODES nor UNOFFICIAL_OPCODES: {missing:02X?}"
    );
}

/// Every one of the 256 possible opcode bytes dispatches through
/// `exec::execute` without panicking — cross-checks that `cpu/exec.rs`'s
/// `match` genuinely has a working arm for each byte (not just that the
/// byte is *listed* somewhere, which `opcode_lists_partition_all_256_bytes`
/// already checks structurally). `execute` itself has no `_` wildcard arm
/// (rustc's own exhaustiveness check enforces full coverage at compile
/// time — see `cpu/exec.rs`'s `dispatch` doc), so this test's real value
/// is confirming no arm panics/underflows/etc. for an all-zero operand
/// stream, not coverage per se.
#[test]
fn dispatch_covers_all_256_opcodes() {
    let mut panicked = Vec::new();
    for opcode in 0u16..=255 {
        let opcode = opcode as u8;
        let mut cpu = Cpu::default();
        let mut bus = SinkBus;
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            exec::execute(&mut cpu, &mut bus, opcode)
        }))
        .is_ok();
        if !ok {
            panicked.push(opcode);
        }
    }
    assert!(
        panicked.is_empty(),
        "opcode(s) panicked during dispatch: {panicked:02X?}"
    );
}

/// Direct unit test for the unstable-op marker (ticket W1-01b: "flag them
/// in the trace log" — this ticket exposes [`Cpu::unstable_op`] as that
/// marker, W1-03's trace logger is the future consumer; see
/// `cpu/exec.rs`'s `dispatch` doc). Runs every one of the 256 opcode
/// bytes once and checks two things together: the exact 8 unstable
/// opcode bytes set `unstable_op` to the *matching* [`UnstableOp`]
/// variant, and every other byte (both official and the 97 *stable*
/// illegals) leaves it `None` — a marker that over-fires on stable ops
/// would be just as wrong as one that misses an unstable one.
#[test]
fn unstable_op_marker_set_exactly_for_unstable_opcodes() {
    let expected: &[(u8, UnstableOp)] = &[
        (0x8B, UnstableOp::Ane),
        (0xAB, UnstableOp::Lxa),
        (0x93, UnstableOp::Sha),
        (0x9F, UnstableOp::Sha),
        (0x9E, UnstableOp::Shx),
        (0x9C, UnstableOp::Shy),
        (0x9B, UnstableOp::Tas),
        (0xBB, UnstableOp::Las),
    ];

    let mut mismatches = Vec::new();
    for opcode in 0u16..=255 {
        let opcode = opcode as u8;
        let mut cpu = Cpu::default();
        let mut bus = SinkBus;
        exec::execute(&mut cpu, &mut bus, opcode);

        let want = expected
            .iter()
            .find(|&&(b, _)| b == opcode)
            .map(|&(_, marker)| marker);
        if cpu.unstable_op != want {
            mismatches.push((opcode, cpu.unstable_op, want));
        }
    }
    assert!(
        mismatches.is_empty(),
        "opcode(s) with wrong unstable_op marker (opcode, got, want): {mismatches:02X?}"
    );
}
