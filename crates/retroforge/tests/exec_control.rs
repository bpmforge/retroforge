//! Execution control against a REAL machine (ticket W4-06e).
//!
//! Drives a hand-built NROM whose control flow is known exactly, so
//! step-over and step-out are checked against where the PC actually ends
//! up rather than against a mocked stepper that could agree with a wrong
//! implementation.

use retroforge::stepper::exec_control::{self, StopReason};
use retroforge::stepper::EmuStepper;
use rf_debugger::breakpoint::{Breakpoint, BreakpointTable, Condition};

const INES_MAGIC: [u8; 4] = [0x4E, 0x45, 0x53, 0x1A];

/// ```text
/// 8000  EA        NOP
/// 8001  20 10 80  JSR $8010      <- the call step-over must run through
/// 8004  EA        NOP            <- where step-over must land
/// 8005  4C 05 80  JMP $8005      <- self-loop, so a runaway test stops here
/// 8010  EA        NOP            (subroutine)
/// 8011  EA        NOP
/// 8012  60        RTS
/// ```
fn program_rom() -> Vec<u8> {
    let mut data = Vec::from(INES_MAGIC);
    data.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut prg = vec![0u8; 0x4000];
    let code: &[(usize, &[u8])] = &[
        (0x0000, &[0xEA]),
        (0x0001, &[0x20, 0x10, 0x80]),
        (0x0004, &[0xEA]),
        (0x0005, &[0x4C, 0x05, 0x80]),
        (0x0010, &[0xEA]),
        (0x0011, &[0xEA]),
        (0x0012, &[0x60]),
    ];
    for (at, bytes) in code {
        prg[*at..*at + bytes.len()].copy_from_slice(bytes);
    }
    prg[0x3FFC] = 0x00; // reset vector -> $8000
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

fn machine() -> EmuStepper {
    EmuStepper::from_ines_bytes(&program_rom()).expect("test rom loads")
}

#[test]
fn step_into_advances_exactly_one_instruction() {
    let mut m = machine();
    assert_eq!(m.pc(), 0x8000, "reset vector");
    assert_eq!(
        exec_control::step_into(&mut m, &mut NullSink, &BreakpointTable::new()),
        StopReason::Completed
    );
    assert_eq!(m.pc(), 0x8001, "one NOP");
    exec_control::step_into(&mut m, &mut NullSink, &BreakpointTable::new());
    assert_eq!(
        m.pc(),
        0x8010,
        "stepping INTO the JSR enters the subroutine"
    );
}

/// The distinction that makes step-over worth having: it lands after the
/// call, not inside it.
#[test]
fn step_over_runs_the_subroutine_and_lands_after_the_call() {
    let mut m = machine();
    exec_control::step_into(&mut m, &mut NullSink, &BreakpointTable::new());
    assert_eq!(m.pc(), 0x8001, "parked on the JSR");

    assert_eq!(
        exec_control::step_over(&mut m, &mut NullSink, &BreakpointTable::new()),
        StopReason::Completed
    );
    assert_eq!(
        m.pc(),
        0x8004,
        "step-over must land AFTER the call, not inside the subroutine"
    );
}

/// Step-over on a non-call must behave exactly as step-into — otherwise
/// it would either hang or skip a whole instruction.
#[test]
fn step_over_a_non_call_is_a_plain_step() {
    let mut m = machine();
    assert_eq!(
        exec_control::step_over(&mut m, &mut NullSink, &BreakpointTable::new()),
        StopReason::Completed
    );
    assert_eq!(m.pc(), 0x8001, "the NOP at $8000 is not a call");
}

#[test]
fn step_out_returns_from_the_subroutine() {
    let mut m = machine();
    // Step until inside the subroutine.
    while m.pc() != 0x8010 {
        exec_control::step_into(&mut m, &mut NullSink, &BreakpointTable::new());
    }
    assert_eq!(
        exec_control::step_out(&mut m, &mut NullSink, &BreakpointTable::new()),
        StopReason::Completed
    );
    assert_eq!(m.pc(), 0x8004, "step-out lands where the caller resumes");
}

#[test]
fn run_to_cursor_stops_at_the_requested_address() {
    let mut m = machine();
    assert_eq!(
        exec_control::run_to_cursor(&mut m, &mut NullSink, &BreakpointTable::new(), 0x8012),
        StopReason::Completed
    );
    assert_eq!(m.pc(), 0x8012, "the RTS inside the subroutine");
}

/// A breakpoint inside a subroutine being stepped over must FIRE, not be
/// silently skipped — breakpoints outrank the step request.
#[test]
fn a_breakpoint_inside_the_stepped_over_call_still_fires() {
    let mut m = machine();
    exec_control::step_into(&mut m, &mut NullSink, &BreakpointTable::new());
    assert_eq!(m.pc(), 0x8001);

    let mut table = BreakpointTable::new();
    table.add(Breakpoint {
        id: 7,
        condition: Condition::Pc(0x8011),
        enabled: true,
    });

    match exec_control::step_over(&mut m, &mut NullSink, &table) {
        StopReason::Breakpoint(hit) => assert_eq!(hit.id, 7),
        other => panic!("a breakpoint inside the call must stop it: {other:?}"),
    }
    assert_eq!(m.pc(), 0x8011, "stopped where the breakpoint was, mid-call");
}

/// The mode-invariant half of criterion 3, at this seam: stepping
/// instruction-by-instruction must reach BYTE-IDENTICAL machine state to
/// running the same span as whole frames. If single-stepping perturbed
/// anything — a drain skipped, a counter nudged — the debugger would be
/// showing a machine the emulator never runs.
#[test]
fn instruction_stepping_reaches_the_same_state_as_frame_stepping() {
    let mut stepped = machine();
    let mut framed = machine();

    // Run one frame's worth of instructions the slow way, counting how
    // many master cycles that took, then run the other machine by frames
    // until it has passed the same point.
    let target = framed.step_frame(&mut NullSink);
    assert!(target > 0, "a frame must advance");

    while stepped.frame_count() < framed.frame_count() {
        exec_control::step_into(&mut stepped, &mut NullSink, &BreakpointTable::new());
    }

    // Both machines are now at the same frame boundary-ish point; the
    // strong claim is that instruction stepping did not corrupt state, so
    // compare the reachable state hash.
    assert_eq!(
        stepped.frame_count(),
        framed.frame_count(),
        "both machines must be on the same frame"
    );
    assert_eq!(
        stepped.state_hash(),
        framed.state_hash(),
        "instruction stepping must reach the SAME state as frame stepping — a debugger \
         that perturbs the machine is showing something the emulator never runs"
    );
}
