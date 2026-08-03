//! Targeted unit tests for ticket W1-01b's interrupt model (NMI edge
//! detection, IRQ level sensitivity, penultimate-cycle polling, and
//! BRK/IRQ hijacking by NMI). The nes6502 SingleStepTests vectors are
//! per-opcode with no IRQ/NMI lines at all (ticket text, confirmed by
//! `super::vectors`' harness never touching `nmi_line`/`irq_line`), so
//! this file is the acceptance evidence for EMULATION_CORES.md §2.1's
//! interrupt criterion, derived directly from nesdev.org/wiki/CPU_interrupts
//! rather than the vectors.
use crate::cpu::bus::CpuBus;
use crate::cpu::{Cpu, FLAG_I};

const NOP: u8 = 0xEA;
const BRK: u8 = 0x00;

/// A fully-controllable `CpuBus`: 64 KiB RAM, settable/held interrupt
/// lines, and a recorded bus trace. `nmi_assert_at_op`, when set, makes
/// `nmi_line()` report asserted starting at a specific *global* bus-op
/// index (counting every `read`/`write` since this bus was created,
/// `Cpu::step`'s own raw pre-`CountingBus` cycle included) and never
/// before — the penultimate-cycle test needs this exact a knob, since the
/// entire point is which bus operation the line change lands on.
struct TestBus {
    ram: [u8; 65536],
    op_count: u32,
    nmi_asserted: bool,
    nmi_assert_at_op: Option<u32>,
    irq_asserted: bool,
    trace: Vec<(u16, u8, bool)>,
}

impl TestBus {
    fn new() -> Self {
        TestBus {
            ram: [0; 65536],
            op_count: 0,
            nmi_asserted: false,
            nmi_assert_at_op: None,
            irq_asserted: false,
            trace: Vec::new(),
        }
    }

    fn load(&mut self, addr: u16, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            self.ram[addr as usize + i] = b;
        }
    }
}

impl CpuBus for TestBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.op_count += 1;
        let value = self.ram[addr as usize];
        self.trace.push((addr, value, false));
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.op_count += 1;
        self.ram[addr as usize] = value;
        self.trace.push((addr, value, true));
    }

    fn nmi_line(&self) -> bool {
        match self.nmi_assert_at_op {
            Some(threshold) => self.op_count >= threshold,
            None => self.nmi_asserted,
        }
    }

    fn irq_line(&self) -> bool {
        self.irq_asserted
    }
}

fn cpu_at(pc: u16) -> Cpu {
    Cpu {
        pc,
        ..Cpu::default()
    }
}

/// NMI is edge-detected (nesdev.org/wiki/CPU_interrupts: "reacts to
/// high-to-low transitions"): a line held asserted for many instructions
/// must still only fire **once**, not once per instruction the way a
/// level-sensitive input would.
#[test]
fn nmi_edge_latch_fires_once_while_held() {
    let mut bus = TestBus::new();
    bus.load(0x0200, &[NOP; 20]);
    bus.load(0xFFFA, &[0x00, 0x80]); // NMI vector -> $8000
    bus.nmi_asserted = true; // held low from before the very first step

    let mut cpu = cpu_at(0x0200);

    let mut entries = 0;
    for _ in 0..10 {
        cpu.step(&mut bus);
        if cpu.pc == 0x8000 {
            entries += 1;
            // Land back in ordinary NOPs so the remaining iterations
            // execute normally instead of decoding the NMI vector bytes
            // themselves as opcodes.
            cpu.pc = 0x0300;
            bus.load(0x0300, &[NOP; 10]);
        }
    }
    assert_eq!(
        entries, 1,
        "NMI held continuously low must fire exactly once (edge-detected, not level)"
    );
}

/// IRQ is level-sensitive (nesdev.org/wiki/CPU_interrupts: "reacts to a
/// low signal level"): held asserted with `I` clear, it must fire again
/// and again — but each entry sets `I`, which must suppress the very next
/// poll even though the line is still held (self-masking on entry).
#[test]
fn irq_level_fires_repeatedly_while_held_and_suppressed_by_i() {
    let mut bus = TestBus::new();
    bus.load(0x0200, &[NOP, NOP]);
    bus.load(0x8000, &[NOP]);
    bus.load(0xFFFE, &[0x00, 0x80]); // IRQ/BRK vector -> $8000
    bus.irq_asserted = true;

    let mut cpu = cpu_at(0x0200);
    cpu.set_flag(FLAG_I, false); // must be unmasked to fire at all

    cpu.step(&mut bus); // NOP @ $0200 — polls with I clear, IRQ asserted
    assert_eq!(cpu.pc, 0x0201);

    let before = bus.trace.len();
    cpu.step(&mut bus); // services the poll from the previous instruction
    assert_eq!(
        cpu.pc, 0x8000,
        "IRQ held with I clear must fire on the poll's next step"
    );
    assert!(
        cpu.flag(FLAG_I),
        "entry sequence must set I (nesdev cycle 6)"
    );
    assert_eq!(
        bus.trace.len() - before,
        7,
        "IRQ entry is 7 bus cycles (nesdev.org/wiki/CPU_interrupts IRQ/NMI table)"
    );

    let before = bus.trace.len();
    cpu.step(&mut bus); // NOP inside the "handler" — I is still set
    assert_eq!(cpu.pc, 0x8001);
    assert_eq!(
        bus.trace.len() - before,
        2,
        "I=1 must suppress the still-asserted IRQ — only the NOP's 2 cycles, no re-entry"
    );

    // The handler re-enables interrupts. A real one would execute CLI;
    // clearing the flag directly is equivalent for what's under test —
    // the poll that matters is the *next* instruction's, regardless of
    // how I got cleared.
    cpu.set_flag(FLAG_I, false);
    bus.load(0x8001, &[NOP]);
    cpu.step(&mut bus); // polls with I clear again -> schedules servicing

    let before = bus.trace.len();
    cpu.step(&mut bus); // services it again
    assert_eq!(
        cpu.pc, 0x8000,
        "IRQ held continuously (level-sensitive) must fire again once I is clear again"
    );
    assert_eq!(bus.trace.len() - before, 7);
}

/// nesdev.org/wiki/CPU_interrupts: "it's really the status of the
/// interrupt lines at the end of the second-to-last cycle that matters" —
/// not the last cycle. `NOP` (`$EA`) is a 2-cycle instruction whose only
/// bus op inside `CountingBus` is cycle 2 (cycle 1, the opcode fetch,
/// happens on the raw bus before `CountingBus` is even constructed — see
/// `exec.rs`'s `run_cycled` doc), so its penultimate cycle *is* cycle 1.
/// Asserting NMI no later than cycle 1 (global op index 1, the opcode
/// fetch itself) must be caught in time to fire on the very next `step`;
/// asserting it only from cycle 2 onward (index 2) must delay firing by
/// one additional instruction — the exact "one instruction late" behavior
/// this criterion is about.
#[test]
fn nmi_polls_on_penultimate_cycle_not_the_last() {
    for (assert_at_op, expected_fire_on_step) in [(1u32, 2usize), (2u32, 3usize)] {
        let mut bus = TestBus::new();
        bus.load(0x0200, &[NOP, NOP, NOP, NOP]);
        bus.load(0xFFFA, &[0x00, 0x80]); // NMI vector -> $8000
        bus.nmi_assert_at_op = Some(assert_at_op);

        let mut cpu = cpu_at(0x0200);

        let mut fired_on = None;
        for step_num in 1..=4 {
            cpu.step(&mut bus);
            if cpu.pc == 0x8000 {
                fired_on = Some(step_num);
                break;
            }
        }
        assert_eq!(
            fired_on,
            Some(expected_fire_on_step),
            "assert_at_op={assert_at_op}: expected NMI to fire on step \
             {expected_fire_on_step}, not before or later"
        );
    }
}

/// nesdev.org/wiki/CPU_interrupts "BRK"/"IRQ/NMI" hijack window: NMI
/// asserted during a `BRK`'s own entry sequence (before the vector fetch)
/// redirects that fetch to `$FFFA`/`$FFFB` instead of `$FFFE`/`$FFFF` —
/// "interrupt hijacking". The instruction is still `BRK` in every other
/// respect (its `B` flag is still pushed set); only the vector changes.
#[test]
fn brk_hijacked_by_nmi_vectors_through_nmi_not_irq() {
    let mut bus = TestBus::new();
    bus.load(0x0200, &[BRK, 0x00]); // BRK + its padding byte
    bus.load(0xFFFE, &[0x34, 0x12]); // IRQ/BRK vector -> $1234 (must NOT be taken)
    bus.load(0xFFFA, &[0x00, 0x80]); // NMI vector -> $8000 (must be taken)
    bus.nmi_asserted = true; // held from before BRK starts

    let mut cpu = cpu_at(0x0200);
    assert_eq!(cpu.s, 0xFD, "sanity: default stack pointer");

    cpu.step(&mut bus);

    assert_eq!(
        cpu.pc, 0x8000,
        "NMI asserted during BRK's own entry sequence must hijack the vector fetch"
    );
    assert!(
        cpu.flag(FLAG_I),
        "vector fetch still sets I regardless of which vector won"
    );

    // Pushes, in order: PCH @ $01FD, PCL @ $01FC, P @ $01FB (S started at
    // $FD, decrementing after each of the three pushes).
    let pushed_p = bus.ram[0x01FB];
    assert_ne!(
        pushed_p & 0x10,
        0,
        "a hijacked BRK still pushes P with B set — hijacking only changes \
         which vector gets fetched, not that this was a software BRK"
    );
    assert_eq!(cpu.s, 0xFA, "three pushes (PCH, PCL, P)");

    // The hijack consumed this edge — it must not also leave NMI pending
    // for a second, separate service right after, even though the line
    // is still (permanently) held asserted.
    bus.load(0x8000, &[NOP]);
    let before = bus.trace.len();
    cpu.step(&mut bus);
    assert_eq!(
        cpu.pc, 0x8001,
        "the hijack's edge must not also re-fire a second, separate NMI"
    );
    assert_eq!(
        bus.trace.len() - before,
        2,
        "just the NOP's 2 cycles, no second entry sequence"
    );
}
