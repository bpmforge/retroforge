//! Ticket W4-06e, criterion 3: the "one-branch cost when no breakpoint is
//! armed" claim, **measured through the real machine** rather than
//! asserted.
//!
//! `#[ignore]`d like `frame_bundle_perf.rs`: a wall-clock measurement, not
//! a hermetic correctness test. Run with:
//!   cargo test --release -p retroforge --test breakpoint_cost -- --ignored --nocapture

use std::time::{Duration, Instant};

use retroforge::stepper::exec_control::{self};
use retroforge::stepper::EmuStepper;
use rf_debugger::breakpoint::{Breakpoint, BreakpointTable, Condition};

const INES_MAGIC: [u8; 4] = [0x4E, 0x45, 0x53, 0x1A];

/// A busy loop doing real bus traffic, so the measurement is dominated by
/// emulation rather than by loop overhead.
fn workload_rom() -> Vec<u8> {
    let mut data = Vec::from(INES_MAGIC);
    data.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut prg = vec![0u8; 0x4000];
    let code: &[u8] = &[
        0xAD, 0x00, 0x00, // LDA $0000
        0x69, 0x01, // ADC #$01
        0x8D, 0x00, 0x00, // STA $0000
        0xEE, 0x01, 0x00, // INC $0001
        0x4C, 0x00, 0x80, // JMP $8000
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    data.extend_from_slice(&prg);
    data.extend_from_slice(&vec![0u8; 0x2000]);
    data
}

struct NullSink;
impl rf_core_api::CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[rf_core_api::PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: rf_core_api::CoreEvent) {}
}

fn time_instructions(table: &BreakpointTable, count: u32) -> Duration {
    let mut m = EmuStepper::from_ines_bytes(&workload_rom()).expect("rom loads");
    // Warm up: first instructions include reset and cold caches.
    for _ in 0..10_000 {
        exec_control::step_into(&mut m, &mut NullSink, table);
    }
    let start = Instant::now();
    for _ in 0..count {
        exec_control::step_into(&mut m, &mut NullSink, table);
    }
    start.elapsed()
}

/// The unarmed table must cost essentially nothing next to an armed one,
/// and — the claim that actually matters — next to the emulation itself.
///
/// Asserted loosely and printed precisely, for the same reason W3-03a's
/// perf gate is: this is a wall-clock measurement on a shared machine, and
/// a tight inequality would be flaky and then quietly loosened, which is
/// how a perf gate becomes decoration. The assertion catches only the
/// failure mode that matters — an "unarmed" table that is not actually
/// free.
#[test]
#[ignore = "wall-clock perf measurement, run manually (W4-06e)"]
fn an_unarmed_breakpoint_table_costs_essentially_nothing() {
    const N: u32 = 300_000;

    let empty = BreakpointTable::new();
    assert!(!empty.armed());

    let mut armed = BreakpointTable::new();
    // A memory condition is the most expensive kind (it calls `peek`),
    // and one that never matches, so the loop runs to completion.
    armed.add(Breakpoint {
        id: 1,
        condition: Condition::MemoryEquals {
            addr: 0x07FF,
            value: 0xA5,
        },
        enabled: true,
    });
    assert!(armed.armed());

    let unarmed_time = time_instructions(&empty, N);
    let armed_time = time_instructions(&armed, N);

    let per = |d: Duration| d.as_secs_f64() * 1e9 / f64::from(N);
    println!(
        "W4-06e ({} build, {N} instructions): unarmed {:.1} ns/instr, armed {:.1} ns/instr, \
         overhead {:.1} ns/instr ({:+.1}%)",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        per(unarmed_time),
        per(armed_time),
        per(armed_time) - per(unarmed_time),
        100.0 * (per(armed_time) - per(unarmed_time)) / per(unarmed_time),
    );

    assert!(
        unarmed_time <= armed_time.mul_f64(1.10),
        "an UNARMED table was slower than an armed one ({:.1} vs {:.1} ns/instr) — the \
         fast path is not a fast path",
        per(unarmed_time),
        per(armed_time),
    );
}
