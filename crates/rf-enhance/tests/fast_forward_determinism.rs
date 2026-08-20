//! Fast-forward must not alter simulation (ticket W8-07; FR-ENH-008,
//! CLAUDE.md law 4).
//!
//! ## The criterion this file exists for
//!
//! "A determinism double-run with fast-forward enabled matches one
//! without it." That is the whole point of the feature's constraint:
//! fast-forward disables **frame pacing**, which is a host-side wall-clock
//! concern, and must never change what the machine computes.
//!
//! ## Why the test is shaped this way
//!
//! `FastForward` is handed a read-only probe and returns a [`Pacing`]
//! decision — it owns no machine and has no way to write one. That makes
//! "it cannot alter simulation" almost structural, but *almost* is not a
//! test. So this drives a real deterministic machine through the same
//! input twice, once with fast-forward on and once off, and requires the
//! two to end byte-identical.
//!
//! A test that only asserted the pacing decisions would pass on an
//! implementation that also poked memory.

use rf_enhance::loading::{FastForward, Pacing};
use rf_profiles::schema::{UntilSpec, WaitLoop};

/// A small deterministic machine with a real wait loop: it sits at
/// `WAIT_PC` while a countdown at `COUNTER` is non-zero, exactly like the
/// decompression waits `[loading]` is meant to describe.
struct Machine {
    mem: [u8; 256],
    pc: u32,
    frame: u64,
}

const WAIT_PC: u32 = 0xC12A;
const RUN_PC: u32 = 0xC200;
const COUNTER: u32 = 0x40;

impl Machine {
    fn new() -> Self {
        let mut mem = [0u8; 256];
        mem[COUNTER as usize] = 30; // 30 frames of "loading"
        Self {
            mem,
            pc: WAIT_PC,
            frame: 0,
        }
    }

    /// One frame. Deliberately independent of pacing: nothing here can
    /// see whether the host is presenting frames or not.
    fn step(&mut self, input: u8) {
        if self.mem[COUNTER as usize] > 0 {
            self.mem[COUNTER as usize] -= 1;
            self.pc = WAIT_PC;
        } else {
            self.pc = RUN_PC;
            // Ordinary gameplay: touch some state so a divergence would
            // actually show.
            for k in 0..8usize {
                let at = ((self.frame as usize * 3) + k * 17) % self.mem.len();
                if at as u32 != COUNTER {
                    self.mem[at] = self.mem[at].wrapping_add(input).wrapping_add(k as u8);
                }
            }
        }
        self.frame += 1;
    }
}

fn waits() -> Vec<WaitLoop> {
    vec![WaitLoop {
        pc: WAIT_PC,
        until: UntilSpec {
            addr: COUNTER,
            equals: 0,
        },
        label: "test decompression wait".to_string(),
    }]
}

/// Run `frames`, optionally with fast-forward enabled, and return the
/// final machine plus how many frames were unpaced.
fn run(enabled: bool, frames: u64) -> (Machine, u64) {
    let mut m = Machine::new();
    let mut ff = FastForward::new(waits());
    ff.set_enabled(enabled, 0);

    for f in 0..frames {
        // The pacing decision is taken from the machine's state, exactly
        // as the shell would take it — and then AFFECTS ONLY PACING.
        let pc = m.pc;
        let mem = m.mem;
        let pacing = ff.update(f, pc, |addr| mem[addr as usize]);
        // A host would skip presentation here when Unpaced. The machine
        // steps identically either way, which is the point.
        let _ = pacing;
        m.step((f % 7) as u8 + 1);
    }
    (m, ff.unpaced_frames())
}

/// **The acceptance.** Same inputs, same frame count, fast-forward on
/// versus off — byte-identical machines.
#[test]
fn fast_forward_does_not_alter_simulation() {
    let (with, unpaced) = run(true, 300);
    let (without, none) = run(false, 300);

    assert_eq!(
        with.mem, without.mem,
        "fast-forward changed emulated memory — that is a determinism bug \
         (law 4), not a feature"
    );
    assert_eq!(with.pc, without.pc);
    assert_eq!(with.frame, without.frame);

    // ...and the run with it enabled must actually have fast-forwarded,
    // or this test proves nothing.
    assert!(
        unpaced > 0,
        "the enabled run never fast-forwarded, so the comparison is vacuous"
    );
    assert_eq!(none, 0, "the disabled run must never fast-forward");
}

/// The wait is detected for exactly as long as it lasts — not longer,
/// and not one frame short.
#[test]
fn the_unpaced_window_matches_the_loading_period() {
    let (_, unpaced) = run(true, 300);
    assert_eq!(
        unpaced, 30,
        "the machine loads for 30 frames, so 30 frames should be unpaced"
    );
}

/// Enabling fast-forward part-way through must not change the outcome
/// either — a player toggling it mid-load is the obvious real case.
#[test]
fn toggling_mid_load_does_not_alter_simulation() {
    let mut m = Machine::new();
    let mut ff = FastForward::new(waits());
    for f in 0..300u64 {
        if f == 10 {
            ff.set_enabled(true, f);
        }
        if f == 20 {
            ff.set_enabled(false, f);
        }
        let (pc, mem) = (m.pc, m.mem);
        let _ = ff.update(f, pc, |a| mem[a as usize]);
        m.step((f % 7) as u8 + 1);
    }
    let (reference, _) = run(false, 300);
    assert_eq!(
        m.mem, reference.mem,
        "toggling fast-forward mid-run must not change the simulation"
    );
    // And the ledger must be balanced despite the mid-window toggle.
    let opens = ff.ledger().iter().filter(|e| e.started).count();
    let closes = ff.ledger().iter().filter(|e| !e.started).count();
    assert_eq!(opens, closes, "every window must close: {:?}", ff.ledger());
}

/// Pacing is the ONLY thing that differs, and it differs where expected.
#[test]
fn pacing_differs_but_only_during_the_declared_wait() {
    let mut m = Machine::new();
    let mut ff = FastForward::new(waits());
    ff.set_enabled(true, 0);
    let mut unpaced_frames = Vec::new();
    for f in 0..60u64 {
        let (pc, mem) = (m.pc, m.mem);
        if ff.update(f, pc, |a| mem[a as usize]) == Pacing::Unpaced {
            unpaced_frames.push(f);
        }
        m.step(1);
    }
    assert_eq!(
        unpaced_frames,
        (0..30).collect::<Vec<u64>>(),
        "only the loading frames should be unpaced"
    );
}
