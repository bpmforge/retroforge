//! Ticket W30-08: the idle observer — a read-only answer to "did this
//! 65816 reach an idle wait during the frame?".
//!
//! A game that finishes its per-frame work early parks the CPU until the
//! next vblank; a game that is slowing down does not (it is still working
//! when the next frame's interrupt arrives). That difference is the only
//! signal an emulator has, without knowing the game, for "this frame ran
//! short of CPU time", which is what a speed governor needs before it adds
//! any (it must never speed a game that is already running at its normal
//! rate).
//!
//! ## What counts as idle
//!
//! 1. **`WAI`** — the CPU halts until an interrupt line asserts. The WDC
//!    W65C816S datasheet: "WAI: ... the processor is halted until a
//!    hardware interrupt (RES, ABORT, NMI, IRQ) occurs", with the
//!    instruction's own cycles then spent halted. Both the executing step
//!    and every following halted step count, and their master cycles are
//!    idle cycles.
//! 2. **A tight no-write polling loop** — the other way games wait is
//!    `LDA $4210 / BPL` (fullsnes "SNES Interrupts": `$4210` bit 7 is the
//!    vblank flag software polls). Recognised as: a backward jump to an
//!    address in the same bank within [`LOOP_MAX_SPAN`] bytes, taken
//!    [`LOOP_MIN_ITERATIONS`] times in a row, with no bus write by any
//!    instruction in between and the program counter never leaving the
//!    loop body.
//!
//! ## It is a heuristic, and says so
//!
//! A read-only loop that is real work (summing a table with no stores) is
//! reported idle for as long as it runs; a game that idles through an
//! interrupt-driven main loop with a store per pass is reported busy.
//! Neither changes the machine: this type only watches. It is **not** in
//! any save state, it is never read by the simulation, and `SnesSystem::
//! step` feeds it values it had already computed — a state hash with the
//! observer present is the one without it.
//!
//! ## Which frame
//!
//! The answer is for the **last completed frame**: `SnesSystem::step`
//! latches the running totals when the frame counter turns over, so a
//! caller reading it right after a frame step (the shell's usual place)
//! gets the frame that step just ran.

/// Consecutive passes through the same backward jump that make a loop
/// "waiting" rather than "working" (the brief's number: a poll loop spins
/// hundreds of times, a short copy or search seldom reaches eight without a
/// store).
pub const LOOP_MIN_ITERATIONS: u8 = 8;
/// How far back, in bytes, a jump may land and still count as a tight loop.
pub const LOOP_MAX_SPAN: u16 = 16;

/// Running idle bookkeeping for one CPU (the S-CPU or the SA-1).
#[derive(Debug, Clone, Default)]
pub struct IdleTracker {
    now_idle: bool,
    now_cycles: u64,
    last_idle: bool,
    last_cycles: u64,
    /// The loop under observation: bank and landing address of its
    /// backward jump, and the highest address that jump was taken from.
    target: Option<(u8, u16)>,
    hi: u16,
    count: u8,
    /// Master cycles spent since the loop's first jump, credited as idle
    /// the moment the loop is confirmed.
    loop_cycles: u64,
    confirmed: bool,
}

impl IdleTracker {
    /// Record one executed (or halted) step.
    ///
    /// `start`/`end` are `(PBR, PC)` before and after; `spent` the master
    /// cycles the step cost; `writes` the bus writes it made; `waiting`
    /// whether the CPU was halted by `WAI` during or before it.
    pub fn observe(
        &mut self,
        start: (u8, u16),
        end: (u8, u16),
        spent: u64,
        writes: u64,
        waiting: bool,
    ) {
        if waiting {
            self.break_loop();
            self.now_idle = true;
            self.now_cycles += spent;
            return;
        }
        if writes > 0 {
            self.break_loop();
            return;
        }
        if let Some((bank, low)) = self.target {
            if start.0 != bank || start.1 < low || start.1 > self.hi {
                self.break_loop();
            }
        }
        let backward = end.0 == start.0 && end.1 <= start.1 && start.1 - end.1 <= LOOP_MAX_SPAN;
        if backward {
            if self.target == Some(end) {
                self.count = self.count.saturating_add(1);
                self.hi = self.hi.max(start.1);
            } else {
                self.target = Some(end);
                self.hi = start.1;
                self.count = 1;
                self.loop_cycles = 0;
                self.confirmed = false;
            }
        }
        if self.target.is_some() {
            self.loop_cycles += spent;
            if self.count >= LOOP_MIN_ITERATIONS {
                self.now_idle = true;
                self.now_cycles += if self.confirmed {
                    spent
                } else {
                    self.loop_cycles
                };
                self.confirmed = true;
            }
        }
    }

    fn break_loop(&mut self) {
        self.target = None;
        self.count = 0;
        self.loop_cycles = 0;
        self.confirmed = false;
    }

    /// The frame counter turned over: the running totals become "the last
    /// frame" and a new frame starts. A loop spanning the boundary keeps
    /// counting (it is still the same wait).
    pub fn frame_boundary(&mut self) {
        self.last_idle = self.now_idle;
        self.last_cycles = self.now_cycles;
        self.now_idle = false;
        self.now_cycles = 0;
    }

    /// Whether the last completed frame reached an idle wait.
    #[must_use]
    pub fn frame_idle(&self) -> bool {
        self.last_idle
    }

    /// Master cycles of the last completed frame spent idle.
    #[must_use]
    pub fn frame_idle_cycles(&self) -> u64 {
        self.last_cycles
    }

    /// Forget everything (reset).
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One pass through `BNE` at `$8002` back to `$8000`.
    fn spin(t: &mut IdleTracker, passes: u32) {
        for _ in 0..passes {
            t.observe((0, 0x8002), (0, 0x8000), 20, 0, false);
        }
    }

    fn idle_after(t: &mut IdleTracker) -> (bool, u64) {
        t.frame_boundary();
        (t.frame_idle(), t.frame_idle_cycles())
    }

    #[test]
    fn seven_passes_are_work_and_eight_are_a_wait() {
        let mut t = IdleTracker::default();
        spin(&mut t, u32::from(LOOP_MIN_ITERATIONS - 1));
        assert!(!idle_after(&mut t).0);
        let mut t = IdleTracker::default();
        spin(&mut t, u32::from(LOOP_MIN_ITERATIONS));
        assert!(idle_after(&mut t).0);
    }

    #[test]
    fn idle_cycles_include_the_iterations_that_confirmed_the_loop() {
        let mut t = IdleTracker::default();
        spin(&mut t, 8);
        spin(&mut t, 2);
        // All ten passes, 20 cycles each: the first eight were credited the
        // moment the eighth confirmed the wait.
        assert_eq!(idle_after(&mut t), (true, 200));
    }

    #[test]
    fn a_write_inside_the_loop_makes_it_work() {
        let mut t = IdleTracker::default();
        for _ in 0..20 {
            // A store before every pass.
            t.observe((0, 0x8001), (0, 0x8002), 10, 1, false);
            spin(&mut t, 1);
        }
        assert!(!idle_after(&mut t).0);
    }

    #[test]
    fn leaving_the_loop_ends_the_wait() {
        let mut t = IdleTracker::default();
        spin(&mut t, 8);
        t.frame_boundary();
        // The next step starts past the loop (it fell through): no more
        // idle time is credited and the count starts over.
        t.observe((0, 0x8004), (0, 0x8006), 20, 0, false);
        t.observe((0, 0x8006), (0, 0x8008), 20, 0, false);
        assert_eq!(idle_after(&mut t), (false, 0));
        spin(&mut t, 7);
        assert!(!idle_after(&mut t).0, "the count restarted from zero");
    }

    #[test]
    fn a_long_jump_back_is_not_a_tight_loop() {
        let mut t = IdleTracker::default();
        for _ in 0..50 {
            t.observe((0, 0x8000 + LOOP_MAX_SPAN + 1), (0, 0x8000), 20, 0, false);
        }
        assert!(!idle_after(&mut t).0);
    }

    #[test]
    fn a_loop_in_another_bank_is_a_different_loop() {
        let mut t = IdleTracker::default();
        for i in 0..20u8 {
            t.observe((i, 0x8002), (i, 0x8000), 20, 0, false);
        }
        assert!(!idle_after(&mut t).0, "each bank restarts the count");
    }

    #[test]
    fn wai_is_idle_and_its_halted_cycles_count() {
        let mut t = IdleTracker::default();
        t.observe((0, 0x8000), (0, 0x8001), 12, 0, true);
        for _ in 0..4 {
            t.observe((0, 0x8001), (0, 0x8001), 6, 0, true);
        }
        assert_eq!(idle_after(&mut t), (true, 36));
    }

    #[test]
    fn a_frame_boundary_latches_and_starts_a_clean_frame() {
        let mut t = IdleTracker::default();
        t.observe((0, 0x8000), (0, 0x8001), 12, 0, true);
        t.frame_boundary();
        assert!(t.frame_idle());
        // Nothing idle happened in the frame that followed.
        t.observe((0, 0x8001), (0, 0x8003), 12, 0, false);
        t.frame_boundary();
        assert!(!t.frame_idle());
        assert_eq!(t.frame_idle_cycles(), 0);
    }
}
