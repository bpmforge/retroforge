//! Loading fast-forward (ticket W8-07; FR-ENH-008,
//! `docs/design/GAME_PROFILES.md` §2's `[loading]`).
//!
//! ## What this does and, more importantly, what it must not
//!
//! FR-ENH-008: "Loading fast-forward shall trigger only on
//! profile-declared wait loops (PC + condition), **disabling frame
//! pacing without altering simulation**."
//!
//! That last clause is the whole design constraint. Fast-forward makes
//! the host present fewer frames per unit of wall-clock time; it does
//! **not** skip frames, change what the CPU executes, or touch a single
//! byte of emulated state. A fast-forward that altered state would be a
//! determinism bug (CLAUDE.md law 4), not a feature — so this module
//! deliberately returns a *pacing decision* and owns no machine at all.
//! It cannot alter simulation because it is given no way to.
//!
//! ## Why "PC + condition" and not "PC alone"
//!
//! A wait loop is a PC the game sits at *while* something is not yet
//! true. The PC alone is not enough: the same address is reached once
//! per iteration whether the wait has one frame left or a thousand, and
//! a game will also pass through it on the frame the wait ENDS. The
//! condition is what says the wait is still in progress, which is why
//! `[[loading.wait_loops]]` carries both and why [`is_waiting`] requires
//! both.
//!
//! ## Off by default, and ledgered
//!
//! Like every enhancement (law 6), this is opt-in. [`FastForward::new`]
//! starts disabled, and each transition is recorded so the ledger can
//! show a player exactly when the emulator ran faster than the hardware
//! would have.

use rf_profiles::schema::WaitLoop;

/// What the host should do with frame pacing this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pacing {
    /// Present at the machine's natural rate.
    Normal,
    /// Run as fast as the host allows: a profile-declared wait loop is
    /// in progress.
    Unpaced,
}

/// One entry in the ledger: a fast-forward window that opened or closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    /// The `label` from the profile row, so the ledger names the wait in
    /// the profile author's words rather than an address.
    pub label: String,
    pub frame: u64,
    pub started: bool,
}

/// Decides pacing from the machine's PC and a memory probe.
#[derive(Debug, Clone)]
pub struct FastForward {
    enabled: bool,
    waits: Vec<WaitLoop>,
    /// The wait currently in progress, by index.
    active: Option<usize>,
    ledger: Vec<LedgerEntry>,
    /// Frames spent unpaced, so a UI can report what was skipped past.
    unpaced_frames: u64,
}

impl FastForward {
    /// Build from a profile's `[loading]` section. **Disabled** until a
    /// caller enables it.
    #[must_use]
    pub fn new(waits: Vec<WaitLoop>) -> Self {
        Self {
            enabled: false,
            waits,
            active: None,
            ledger: Vec::new(),
            unpaced_frames: 0,
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Turn it on or off.
    ///
    /// Disabling mid-wait closes the open ledger entry rather than
    /// leaving it dangling — a ledger that says a window opened and never
    /// says it closed is worse than no ledger.
    pub fn set_enabled(&mut self, on: bool, frame: u64) {
        if self.enabled && !on {
            self.close_active(frame);
        }
        self.enabled = on;
    }

    #[must_use]
    pub fn ledger(&self) -> &[LedgerEntry] {
        &self.ledger
    }

    #[must_use]
    pub fn unpaced_frames(&self) -> u64 {
        self.unpaced_frames
    }

    /// Is `wait` currently in progress?
    ///
    /// Both halves must hold: the machine is AT the declared PC, and the
    /// declared condition has NOT yet been met.
    ///
    /// ## The direction of `until` is easy to get backwards — it was
    ///
    /// `until = { addr, equals }` reads "wait UNTIL `addr` equals this",
    /// so the wait is in progress while the address does **not** hold
    /// that value, and it ENDS on the frame it does. The first version
    /// here compared with `==` and therefore flagged exactly one frame:
    /// the one the wait finished on. The determinism test caught it
    /// immediately — 1 unpaced frame instead of 30, and that single frame
    /// was the last one rather than the first.
    ///
    /// Which is the failure this module's own doc predicted: "a game will
    /// also pass through [the PC] on the frame the wait ENDS". It turned
    /// out to describe the bug rather than guard against it.
    #[must_use]
    pub fn is_waiting(wait: &WaitLoop, pc: u32, probe: impl Fn(u32) -> u8) -> bool {
        pc == wait.pc && u32::from(probe(wait.until.addr)) != wait.until.equals
    }

    /// Decide pacing for this frame.
    ///
    /// `probe` reads emulated memory. It is `&dyn`-free and read-only on
    /// purpose: this module is handed no way to WRITE, so it cannot alter
    /// simulation even by mistake.
    pub fn update(&mut self, frame: u64, pc: u32, probe: impl Fn(u32) -> u8) -> Pacing {
        if !self.enabled {
            // Still close a window that was open when it was switched
            // off, so the ledger stays balanced.
            self.close_active(frame);
            return Pacing::Normal;
        }

        let matched = self
            .waits
            .iter()
            .position(|w| Self::is_waiting(w, pc, &probe));

        match (self.active, matched) {
            (Some(a), Some(b)) if a == b => {
                self.unpaced_frames += 1;
                Pacing::Unpaced
            }
            (_, Some(b)) => {
                self.close_active(frame);
                self.active = Some(b);
                self.ledger.push(LedgerEntry {
                    label: self.waits[b].label.clone(),
                    frame,
                    started: true,
                });
                self.unpaced_frames += 1;
                Pacing::Unpaced
            }
            (Some(_), None) => {
                self.close_active(frame);
                Pacing::Normal
            }
            (None, None) => Pacing::Normal,
        }
    }

    fn close_active(&mut self, frame: u64) {
        if let Some(a) = self.active.take() {
            self.ledger.push(LedgerEntry {
                label: self.waits[a].label.clone(),
                frame,
                started: false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_profiles::schema::UntilSpec;

    fn wait(pc: u32, addr: u32, equals: u32, label: &str) -> WaitLoop {
        WaitLoop {
            pc,
            until: UntilSpec { addr, equals },
            label: label.to_string(),
        }
    }

    fn ff() -> FastForward {
        FastForward::new(vec![wait(0xC12A, 0x0778, 0, "level decompression wait")])
    }

    /// **Off by default** (law 6): a fresh install must not run faster
    /// than the hardware would have.
    #[test]
    fn fast_forward_is_disabled_until_asked_for() {
        let mut f = ff();
        assert!(!f.is_enabled());
        // Even sitting exactly in the declared wait loop.
        assert_eq!(f.update(0, 0xC12A, |_| 1), Pacing::Normal);
        assert_eq!(f.unpaced_frames(), 0);
        assert!(f.ledger().is_empty());
    }

    /// **PC alone is not enough.** The same address is reached once per
    /// iteration whether the wait has one frame left or a thousand — and
    /// the game passes through it on the frame the wait ENDS.
    #[test]
    fn both_the_pc_and_the_condition_must_hold() {
        // `until = { addr, equals: 0 }` means "wait until it becomes 0",
        // so the wait runs while it is NON-zero.
        let w = wait(0xC12A, 0x0778, 0, "x");
        assert!(
            FastForward::is_waiting(&w, 0xC12A, |_| 1),
            "at the PC and the target value not yet reached: still waiting"
        );
        assert!(
            !FastForward::is_waiting(&w, 0xC12A, |_| 0),
            "the address reached its target — the wait is OVER, even though \
             the PC is still the loop's"
        );
        assert!(
            !FastForward::is_waiting(&w, 0xC000, |_| 1),
            "condition unmet but the machine is elsewhere"
        );
    }

    /// Enabled and in the wait: unpaced. Out of the wait: back to normal.
    #[test]
    fn pacing_follows_the_declared_wait() {
        let mut f = ff();
        f.set_enabled(true, 0);
        assert_eq!(f.update(1, 0xC12A, |_| 1), Pacing::Unpaced);
        assert_eq!(f.update(2, 0xC12A, |_| 1), Pacing::Unpaced);
        // The address reaches its target: the wait is over.
        assert_eq!(f.update(3, 0xC12A, |_| 0), Pacing::Normal);
        assert_eq!(f.unpaced_frames(), 2);
    }

    /// **Every window is ledgered, and every open has a close.** A ledger
    /// saying a window opened and never closed is worse than none.
    #[test]
    fn each_window_is_ledgered_open_and_closed() {
        let mut f = ff();
        f.set_enabled(true, 0);
        f.update(10, 0xC12A, |_| 1);
        f.update(11, 0xC12A, |_| 1);
        f.update(12, 0xC12A, |_| 0);

        let l = f.ledger();
        assert_eq!(l.len(), 2, "one open, one close: {l:?}");
        assert!(l[0].started && !l[1].started);
        assert_eq!(l[0].frame, 10);
        assert_eq!(l[1].frame, 12);
        assert_eq!(l[0].label, "level decompression wait", "the author's words");
    }

    /// Disabling mid-wait closes the entry rather than leaving it
    /// dangling.
    #[test]
    fn disabling_mid_window_closes_the_ledger_entry() {
        let mut f = ff();
        f.set_enabled(true, 0);
        f.update(5, 0xC12A, |_| 1);
        assert_eq!(f.ledger().len(), 1);
        f.set_enabled(false, 6);
        let l = f.ledger();
        assert_eq!(l.len(), 2);
        assert!(!l[1].started, "the window must be closed, not left open");
    }

    /// Re-entering the same wait after leaving it opens a NEW window,
    /// rather than silently continuing the old one.
    #[test]
    fn leaving_and_re_entering_opens_a_second_window() {
        let mut f = ff();
        f.set_enabled(true, 0);
        f.update(1, 0xC12A, |_| 1);
        f.update(2, 0xC12A, |_| 0); // target reached: out
        f.update(3, 0xC12A, |_| 1); // waiting again
        let starts = f.ledger().iter().filter(|e| e.started).count();
        assert_eq!(starts, 2, "two distinct windows: {:?}", f.ledger());
    }

    /// A profile with no `[loading]` rows never fast-forwards, however
    /// enabled it is — there is nothing declared to trigger on.
    #[test]
    fn a_profile_with_no_declared_waits_never_fast_forwards() {
        let mut f = FastForward::new(Vec::new());
        f.set_enabled(true, 0);
        for frame in 0..100 {
            assert_eq!(f.update(frame, 0xC12A, |_| 1), Pacing::Normal);
        }
        assert_eq!(f.unpaced_frames(), 0);
    }

    /// Switching between two declared waits closes one window and opens
    /// the other.
    #[test]
    fn moving_between_two_waits_closes_the_first() {
        let mut f = FastForward::new(vec![
            wait(0x1000, 0x10, 0, "first"),
            wait(0x2000, 0x20, 0, "second"),
        ]);
        f.set_enabled(true, 0);
        f.update(1, 0x1000, |_| 1);
        f.update(2, 0x2000, |_| 1);
        let l = f.ledger();
        assert_eq!(l.len(), 3, "open first, close first, open second: {l:?}");
        assert_eq!(l[0].label, "first");
        assert!(!l[1].started && l[1].label == "first");
        assert!(l[2].started && l[2].label == "second");
    }
}
