//! Wall-clock frame pacing (ticket W2-18) — run the machine at NTSC
//! speed instead of as fast as the host happens to manage.
//!
//! Before this existed, `crate::core_thread`'s loop emulated a frame, sent
//! it, and immediately started the next one; the only sleep in it was the
//! *paused* idle path. Measured on real ROMs: **113 fps in debug (1.9x
//! real speed) and 1132 fps in release (18.8x)** — playable-but-quick in a
//! debug build, unusable in a release one.
//!
//! ## This is the interim mechanism, and it knows it
//!
//! The documented end-state is an **audio-driven clock**: `rf-audio`'s
//! cpal ring plus dynamic rate control, resampling within ±0.5% against
//! buffer fill (ticket W2-05, behind W2-01a → W2-01b). That is the right
//! long-term answer because the audio device's real consumption rate is
//! the only clock that cannot drift against itself, and because it is what
//! `docs/design/FAILURE_MODES.md` FM-02's underrun cascade mitigation
//! assumes. A host-clock sleep cannot do either. W2-05 subsumes this
//! module; until then an 18.8x-too-fast release build is the worse state.
//!
//! ## Why pacing cannot affect determinism
//!
//! It gates **when** [`crate::stepper::EmuStepper::tick_running_with_input`]
//! is called — never what it is called with, and never the machine. The
//! same input log still produces the same state, which the 10k-frame
//! double-run suite keeps proving because it drives `EmuStepper` directly
//! and never constructs a pacer. `scripts/validate-arch.sh`'s determinism
//! lint (FR-CORE-003, "no wall-clock reads") greps only
//! `crates/rf-{core-api,nes,snes,cart}/src`; this is the app shell, which
//! is exactly where a host clock belongs.
//!
//! ## Two failure modes this deliberately avoids
//!
//! - **Drift.** Sleeping a fixed period per frame accumulates every
//!   scheduling overshoot forever, so the emulator runs slightly slow and
//!   the error compounds. [`FramePacer`] instead tracks an absolute
//!   *deadline* and advances it by exactly one frame period each time, so
//!   an overshoot on one frame is absorbed by a shorter sleep on the next.
//! - **Death spiral.** If the host stalls (a breakpoint, a laptop
//!   suspend), a pure deadline scheme owes a huge backlog and then runs
//!   flat-out trying to repay it — the emulator appears to fast-forward
//!   exactly when the user least wants it. [`MAX_LAG`] caps that: past it,
//!   the pacer resynchronises to now and drops the debt.
use std::time::{Duration, Instant};

/// NTSC frame period, 16.639267 ms ≈ **60.0988 Hz**.
///
/// Derivation, so the constant is checkable rather than magic: the NES
/// master clock is 21,477,272 Hz and the PPU runs at master ÷ 4. A frame
/// is 341 dots × 262 scanlines = 89,342 dots, except that with rendering
/// enabled the pre-render line is one dot shorter on odd frames, giving a
/// hardware *average* of 89,341.5. So
/// `89,341.5 × 4 ÷ 21,477,272 s = 16,639,267 ns`.
///
/// This crate's own PPU always emits the full 89,342 dots (it implements
/// the odd-frame skip but the average is what wall-clock feel should
/// track), so pacing to the hardware average rather than to our own
/// per-frame dot count is deliberate — the difference is 0.0006%, far
/// below what the OS scheduler resolves anyway.
pub const NTSC_FRAME_PERIOD: Duration = Duration::from_nanos(16_639_267);

/// How far behind schedule the pacer tolerates before giving up on the
/// backlog and resynchronising. Two frames is enough to absorb ordinary
/// scheduler jitter without letting a real stall turn into a visible
/// fast-forward.
const MAX_LAG: Duration = Duration::from_millis(34);

/// Paces a run loop to a fixed frame period. See module doc.
///
/// Deliberately holds no clock of its own: every method takes `now`, so
/// the whole thing is testable with synthetic timestamps and no test ever
/// sleeps.
#[derive(Debug)]
pub struct FramePacer {
    period: Duration,
    /// When the next frame is due. `None` until the first frame, and
    /// reset by [`FramePacer::resync`] whenever the loop stops running —
    /// otherwise the time spent paused would look like an enormous
    /// backlog to repay.
    next_due: Option<Instant>,
    enabled: bool,
}

impl Default for FramePacer {
    fn default() -> Self {
        Self::new()
    }
}

impl FramePacer {
    #[must_use]
    pub fn new() -> Self {
        FramePacer {
            period: NTSC_FRAME_PERIOD,
            next_due: None,
            enabled: true,
        }
    }

    /// Turn pacing off (run flat-out) or back on. Exists because
    /// **FR-ENH-008** defines loading-skip fast-forward as "disabling frame
    /// pacing without altering simulation" — a mechanism with no off
    /// switch could not support it. Disabling clears the schedule so
    /// re-enabling does not inherit a stale deadline.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.next_due = None;
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Forget the schedule — call when the machine stops running, so a
    /// long pause is not mistaken for a backlog when it resumes.
    pub fn resync(&mut self) {
        self.next_due = None;
    }

    /// How long the caller should sleep before running the next frame,
    /// and advance the schedule by one frame.
    ///
    /// Returns [`Duration::ZERO`] when the frame is already due (or
    /// overdue, or pacing is disabled), so a caller can skip the sleep
    /// syscall entirely in the common catch-up case.
    pub fn next_delay(&mut self, now: Instant) -> Duration {
        if !self.enabled {
            return Duration::ZERO;
        }
        let due = match self.next_due {
            // First frame after a start or resync: run immediately and
            // schedule from here.
            None => now,
            Some(due) => due,
        };

        let delay = due.saturating_duration_since(now);

        // Advance the schedule by exactly one period — this is what makes
        // the pacer drift-free (module doc).
        let mut next = due + self.period;
        // ... unless we are so far behind that repaying the backlog would
        // read as a fast-forward. Then drop the debt.
        if now > next + MAX_LAG {
            next = now + self.period;
        }
        self.next_due = Some(next);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_runs_immediately() {
        let mut p = FramePacer::new();
        assert_eq!(
            p.next_delay(Instant::now()),
            Duration::ZERO,
            "starting up must not stall for a frame period before the first frame"
        );
    }

    /// The core property, modelled the way the real loop actually calls
    /// it: the caller sleeps the returned delay, spends a little time
    /// emulating the frame, then asks again. The next ask must cover the
    /// rest of the period — i.e. period minus the work already done.
    ///
    /// (The first draft of this test called `next_delay` exactly *at* the
    /// deadline and expected a full period back. That was the test being
    /// wrong, not the pacer: arriving exactly on time correctly means "run
    /// now, wait zero". The delay returned is time-until-due, not
    /// time-between-frames.)
    #[test]
    fn steady_state_asks_for_the_remainder_of_the_period() {
        let mut p = FramePacer::new();
        let t0 = Instant::now();
        assert_eq!(p.next_delay(t0), Duration::ZERO);

        let work = Duration::from_millis(1);
        let delay = p.next_delay(t0 + work);
        assert_eq!(
            delay,
            NTSC_FRAME_PERIOD - work,
            "a frame that took 1 ms must be asked to wait out the rest of the period"
        );
    }

    /// Drift is the failure a naive `sleep(period)` has: every overshoot is
    /// kept forever. Here a frame overruns its whole period, so the next
    /// one is already overdue and must not be asked to wait — and the
    /// schedule must stay anchored to the original deadline rather than
    /// sliding forward by the overrun.
    #[test]
    fn an_overrunning_frame_is_absorbed_not_accumulated() {
        let mut p = FramePacer::new();
        let t0 = Instant::now();
        p.next_delay(t0); // next due t0 + period

        let overrun = Duration::from_millis(4);
        assert_eq!(
            p.next_delay(t0 + NTSC_FRAME_PERIOD + overrun),
            Duration::ZERO,
            "an already-overdue frame must not be asked to wait"
        );

        // Schedule advanced from the DEADLINE (t0 + 2*period), not from
        // the late arrival — so once the caller catches up, the original
        // cadence is intact rather than permanently 4 ms late.
        let delay = p.next_delay(t0 + NTSC_FRAME_PERIOD + overrun);
        assert_eq!(
            delay,
            NTSC_FRAME_PERIOD - overrun,
            "the lost time is repaid against the original schedule, not baked in"
        );
    }

    /// The opposite failure: after a long stall a pure deadline scheme
    /// owes hundreds of frames and runs flat-out repaying them, which the
    /// user sees as an unwanted fast-forward. Past MAX_LAG the debt drops.
    #[test]
    fn a_long_stall_resyncs_instead_of_fast_forwarding() {
        let mut p = FramePacer::new();
        let t0 = Instant::now();
        p.next_delay(t0);

        // Host stalled a full second — ~60 frames of backlog.
        let stalled = t0 + Duration::from_secs(1);
        assert_eq!(p.next_delay(stalled), Duration::ZERO);

        // Back to normal pacing immediately. Had the debt been kept, this
        // would return ZERO for the next ~60 calls.
        let work = Duration::from_millis(1);
        assert_eq!(
            p.next_delay(stalled + work),
            NTSC_FRAME_PERIOD - work,
            "after resync the pacer must be back on schedule, not repaying a 1 s backlog"
        );
    }

    #[test]
    fn disabling_pacing_never_sleeps() {
        let mut p = FramePacer::new();
        p.set_enabled(false);
        assert!(!p.is_enabled());
        let t0 = Instant::now();
        p.next_delay(t0);
        assert_eq!(
            p.next_delay(t0),
            Duration::ZERO,
            "FR-ENH-008 fast-forward: disabled pacing must never ask for a sleep"
        );
    }

    /// Re-enabling must not inherit a deadline computed before the pause,
    /// which would make the first frame back look wildly overdue.
    #[test]
    fn re_enabling_starts_a_fresh_schedule() {
        let mut p = FramePacer::new();
        let t0 = Instant::now();
        p.next_delay(t0);
        p.set_enabled(false);
        p.set_enabled(true);
        let resumed = t0 + Duration::from_secs(5);
        assert_eq!(p.next_delay(resumed), Duration::ZERO);
        let work = Duration::from_millis(1);
        assert_eq!(
            p.next_delay(resumed + work),
            NTSC_FRAME_PERIOD - work,
            "schedule must restart from the resume point, owing nothing"
        );
    }

    #[test]
    fn resync_after_a_pause_does_not_owe_a_backlog() {
        let mut p = FramePacer::new();
        let t0 = Instant::now();
        p.next_delay(t0);
        p.resync(); // machine paused here
        let resumed = t0 + Duration::from_secs(30);
        assert_eq!(p.next_delay(resumed), Duration::ZERO);
        let work = Duration::from_millis(1);
        assert_eq!(p.next_delay(resumed + work), NTSC_FRAME_PERIOD - work);
    }

    /// The constant is the whole point of the module, so pin it: 60.0988 Hz
    /// within a hundredth of a hertz.
    #[test]
    fn frame_period_matches_ntsc_rate() {
        let hz = 1.0 / NTSC_FRAME_PERIOD.as_secs_f64();
        assert!(
            (hz - 60.0988).abs() < 0.01,
            "NTSC_FRAME_PERIOD must be 60.0988 Hz, got {hz}"
        );
    }
}
