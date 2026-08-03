//! Static cross-check between `cpu/exec.rs`'s dispatch `match` and the
//! independently-hand-authored [`super::vectors::OFFICIAL_OPCODES`] list —
//! catches a typo'd or missing match arm even with no vector data present
//! (unlike `super::vectors`, this needs no external files).
use std::collections::HashSet;
use std::panic;

use super::vectors::OFFICIAL_OPCODES;
use crate::cpu::bus::CpuBus;
use crate::cpu::{exec, Cpu};

/// Answers every read with 0 and discards every write — enough for
/// `exec::execute` to run any opcode's full cycle sequence without needing
/// meaningful operand bytes, since this test only cares whether dispatch
/// panics (unofficial opcode) or not (official), never register outcomes.
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
fn dispatch_matches_official_opcode_list() {
    let official: HashSet<u8> = OFFICIAL_OPCODES.iter().copied().collect();

    // The unofficial-opcode arm panics by design (cpu/exec.rs `unofficial`);
    // silence the default panic-hook stderr spam for the ~105 expected
    // panics below, restoring it before this test returns either way.
    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let mut mismatches = Vec::new();
    for opcode in 0u16..=255 {
        let opcode = opcode as u8;
        let mut cpu = Cpu::default();
        let mut bus = SinkBus;
        let dispatched = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            exec::execute(&mut cpu, &mut bus, opcode)
        }))
        .is_ok();
        let listed_official = official.contains(&opcode);
        if dispatched != listed_official {
            mismatches.push((opcode, dispatched, listed_official));
        }
    }
    panic::set_hook(prev_hook);

    assert!(
        mismatches.is_empty(),
        "cpu/exec.rs dispatch disagrees with OFFICIAL_OPCODES for (opcode, \
         dispatch_ran_without_panic, listed_as_official): {mismatches:02X?}"
    );
}
