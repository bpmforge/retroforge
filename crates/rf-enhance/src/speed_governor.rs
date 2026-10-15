//! Ticket W30-08: the speed governor — an overclock that may only REMOVE
//! SLOWDOWN.
//!
//! "We don't want to distort the game, we are enhancing it." A plain
//! overclock makes a game that was running at its normal rate run faster
//! (the whole simulation speeds up), which is a distortion. The governor
//! raises a chip's clock multiplier only while the game is running SHORT of
//! its normal logic rate, and lowers it again the moment it is not, so the
//! game never ticks faster than it was designed to. The player's slider is
//! the governor's **ceiling**, not a fixed speed.
//!
//! ## Pure by construction
//!
//! The governor owns no machine and does no I/O: the shell feeds it one
//! [`Observation`] per real frame and applies the multiplier it returns.
//! Its decisions depend only on the observation stream, so a synthetic
//! stream reproduces them exactly (the unit tests below). It lives in
//! `rf-enhance`, which may not depend on a core, so the observations are
//! plain values.
//!
//! One instance per overclocked chip: the S-CPU, the SA-1, and — through
//! this same type, fed the same kind of observation — a future Super FX
//! governor. (That is the seam: a new chip needs a new instance and a
//! `tick` source, not new control logic.)
//!
//! ## The signal
//!
//! `tick` says "a logic tick finished this frame". The shell derives it
//! from the profile's `tick_counter` byte (changed by +1 mod 256 this
//! frame) when a profile declares one, otherwise from the S-CPU's idle
//! observer (`frame_idle`): a game that finishes its frame's work early
//! parks the CPU, a game that is slowing down does not. The tick **rate**
//! is ticks over the last 60 frames.
//!
//! ## The target
//!
//! The rate the game ticks at when it is not slowing down. Either the
//! profile's `target_tick_hz`, or LEARNED: the mode of the per-second tick
//! counts seen while the multiplier is 100 and the picture is changing. At
//! least [`MIN_LEARN_SECONDS`] such seconds are needed before the governor
//! may act; until then it holds 100. A game that is 30-locked therefore
//! learns 30 and is never pushed to 60.
//!
//! ## Control (every 30 frames)
//!
//! - rate at least 2 below target: one notch (+100) up, to the ceiling;
//! - rate more than 1 above target: one notch down at once (never faster
//!   than normal);
//! - a raise that left the rate no better (within 1) is undone and not
//!   retried for 10 s, then 20 s, doubling to 30 s: if the clock is not what
//!   the game is short of (it waits on something else, or the tick signal is
//!   noise) a faster chip only wastes the host's time;
//! - rate at target with the multiplier above 100: after a hold (3 s at the
//!   start), probe one notch down. A shortfall within 2 s of a probe steps
//!   back up and DOUBLES the hold before the next probe (cap 30 s). The
//!   least speed that keeps the target wins.
//!
//! ## Two refusals
//!
//! - **No signal, no overclock.** If a game never ticks 10 times a second
//!   at 100% (its idle never shows, no counter is declared), or a minute at
//!   normal speed never yields anything to act on, the governor never raises
//!   anything and says so ([`Notice::NoSignal`]).
//! - **Hang guard.** A game that hangs when the clock is raised (a hung
//!   game looks idle too, so idle alone cannot catch it) stops changing the
//!   picture. If the picture is unchanged for 180 frames while the
//!   multiplier is above 100, the governor drops to 100 and stays there for
//!   the rest of the session ([`Notice::TurnedOff`]).

use std::collections::VecDeque;

/// The multiplier step, in percent. The cores accept whole multiples of 100.
pub const NOTCH: u16 = 100;
/// The clock the console shipped with.
pub const NORMAL: u16 = 100;
/// The highest multiplier any governor will ask for (the Super FX goes to
/// 800%; the callers cap the 65816 and the SA-1 at 400% themselves).
pub const MAX_CEILING: u16 = 800;

/// Frames in the tick-rate window ("ticks over the last 60 frames").
pub const RATE_WINDOW: usize = 60;
/// Frames between control decisions.
pub const EVAL_INTERVAL: u64 = 30;
/// Valid seconds the target needs before the governor may act on a learned
/// target.
pub const MIN_LEARN_SECONDS: u32 = 10;
/// Fewer ticks than this in a second is "no signal".
pub const MIN_SIGNAL_TICKS: u32 = 10;
/// The picture must change in at least this many frames of a second for
/// that second to count as the game running (a frozen frame teaches
/// nothing).
const MIN_CHANGING_FRAMES: u32 = 10;
/// Seconds spent at normal speed after which a game that still gives the
/// governor nothing to act on (no usable signal, or never ten seconds of a
/// changing picture) is called "no signal" rather than "learning".
const NO_SIGNAL_SECONDS: u32 = 60;
/// Frames the picture may stay unchanged above 100% before the hang guard
/// trips (3 s).
pub const HANG_FRAMES: u32 = 180;
/// The first hold at a notch before probing one down (3 s).
const HOLD_START: u32 = 180;
/// The longest hold between probes (30 s).
const HOLD_MAX: u32 = 1800;
/// A shortfall this soon after a probe means the probe was wrong (2 s).
const PROBE_WINDOW: u32 = 120;
/// The first wait, after a raise that did not help, before trying again
/// (10 s); it doubles on each repeat, up to [`HOLD_MAX`].
const FUTILE_START: u32 = 600;
/// A raise helped only if the rate rose by more than this.
const FUTILE_MARGIN: u32 = 1;
/// How far below the target counts as a shortfall.
const SHORTFALL: u32 = 2;
/// How far above the target counts as running too fast.
const EXCESS: u32 = 1;

/// What the shell saw of one real frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Observation {
    /// A logic tick finished this frame.
    pub tick: bool,
    /// This frame's resolved picture differs from the previous frame's.
    pub picture_changed: bool,
}

/// Where the governor got its target rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSource {
    /// Not known yet.
    None,
    /// The profile's `[overclock].target_tick_hz`.
    Profile,
    /// The mode of the rates seen at normal speed.
    Learned,
}

/// What the governor wants the player to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// Still watching the game at normal speed; nothing raised yet.
    Learning,
    /// Running: the multiplier follows the game.
    Active,
    /// The game never ticks often enough to measure.
    NoSignal,
    /// The hang guard tripped; the clock stays at normal for the session.
    TurnedOff,
}

/// A snapshot for the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernorStatus {
    pub notice: Notice,
    /// The multiplier in force, percent.
    pub multiplier: u16,
    /// Ticks over the last 60 frames.
    pub rate: u32,
    /// The target rate, once known.
    pub target: Option<u32>,
    pub target_source: TargetSource,
}

/// The governor for one chip.
#[derive(Debug, Clone)]
pub struct SpeedGovernor {
    ceiling: u16,
    profile_target: Option<u32>,
    multiplier: u16,
    frame: u64,
    /// One entry per recent frame, newest last (at most [`RATE_WINDOW`]).
    ticks: VecDeque<bool>,
    // --- learning ---
    /// Counts of seconds by tick count (index 0..=60), valid seconds only.
    histogram: [u32; RATE_WINDOW + 1],
    valid_seconds: u32,
    /// Seconds spent entirely at normal speed, whatever the picture did.
    clean_seconds: u32,
    signal_seen: bool,
    sec_frames: u32,
    sec_ticks: u32,
    sec_changing: u32,
    sec_clean: bool,
    // --- control ---
    frames_since_change: u32,
    /// Frames at the current notch while at target (hold timer).
    held: u32,
    hold: u32,
    /// Frames since the last probe step down, while one is in flight.
    probe_age: Option<u32>,
    /// The rate when the clock was last raised, until the next decision
    /// judges whether the raise helped.
    rate_before_raise: Option<u32>,
    /// Frames left in which raising is off the table because the last raise
    /// did not help, and the length of the next such wait.
    futile_lock: u32,
    futile_hold: u32,
    // --- hang guard ---
    still: u32,
    turned_off: bool,
}

impl SpeedGovernor {
    /// A governor at normal speed with the given ceiling (percent; clamped
    /// to a whole multiple of 100 in 100..=800) and an optional profile
    /// target (ticks per second).
    #[must_use]
    pub fn new(ceiling: u16, profile_target_hz: Option<u32>) -> Self {
        Self {
            ceiling: clamp_ceiling(ceiling),
            profile_target: profile_target_hz.filter(|t| *t > 0),
            multiplier: NORMAL,
            frame: 0,
            ticks: VecDeque::with_capacity(RATE_WINDOW),
            histogram: [0; RATE_WINDOW + 1],
            valid_seconds: 0,
            clean_seconds: 0,
            signal_seen: false,
            sec_frames: 0,
            sec_ticks: 0,
            sec_changing: 0,
            sec_clean: true,
            frames_since_change: 0,
            held: 0,
            hold: HOLD_START,
            probe_age: None,
            rate_before_raise: None,
            futile_lock: 0,
            futile_hold: FUTILE_START,
            still: 0,
            turned_off: false,
        }
    }

    /// Move the ceiling. A multiplier above the new ceiling drops to it at
    /// once.
    pub fn set_ceiling(&mut self, ceiling: u16) {
        self.ceiling = clamp_ceiling(ceiling);
        if self.multiplier > self.ceiling {
            self.change_to(self.ceiling);
        }
    }

    /// Set or clear the profile's target rate.
    pub fn set_profile_target(&mut self, hz: Option<u32>) {
        self.profile_target = hz.filter(|t| *t > 0);
    }

    /// The multiplier to apply to the chip now.
    #[must_use]
    pub fn multiplier(&self) -> u16 {
        self.multiplier
    }

    /// The target, once known: the profile's, else the learned mode (after
    /// enough seconds at normal speed).
    #[must_use]
    pub fn target(&self) -> Option<u32> {
        self.profile_target.or_else(|| self.learned_target())
    }

    fn learned_target(&self) -> Option<u32> {
        if self.valid_seconds < MIN_LEARN_SECONDS {
            return None;
        }
        // The mode among seconds that show a signal; on a tie the faster
        // rate (the game's undisturbed pace is the faster of two equals).
        let mut best: Option<(u32, usize)> = None;
        for (rate, &count) in self.histogram.iter().enumerate() {
            if rate < MIN_SIGNAL_TICKS as usize || count == 0 {
                continue;
            }
            if best.is_none_or(|(c, _)| count >= c) {
                best = Some((count, rate));
            }
        }
        best.map(|(_, rate)| rate as u32)
    }

    /// The card's view.
    #[must_use]
    pub fn status(&self) -> GovernorStatus {
        let (target, target_source) = match (self.profile_target, self.learned_target()) {
            (Some(t), _) => (Some(t), TargetSource::Profile),
            (None, Some(t)) => (Some(t), TargetSource::Learned),
            (None, None) => (None, TargetSource::None),
        };
        let notice = if self.turned_off {
            Notice::TurnedOff
        } else if self.no_signal() {
            Notice::NoSignal
        } else if self.armed() {
            Notice::Active
        } else {
            Notice::Learning
        };
        GovernorStatus {
            notice,
            multiplier: self.multiplier,
            rate: self.window_rate(RATE_WINDOW),
            target,
            target_source,
        }
    }

    /// No signal: enough seconds watched at normal speed and not one of them
    /// ticked [`MIN_SIGNAL_TICKS`] times — or a full minute at normal speed
    /// without ever reaching a state the governor can act on (a game whose
    /// idle only shows in its first seconds, like a title screen).
    fn no_signal(&self) -> bool {
        (self.valid_seconds >= MIN_LEARN_SECONDS && !self.signal_seen)
            || (self.clean_seconds >= NO_SIGNAL_SECONDS && !self.armed())
    }

    /// May the governor act: not turned off, a signal has been seen, and a
    /// target is known.
    fn armed(&self) -> bool {
        !self.turned_off && self.signal_seen && self.target().is_some()
    }

    /// Ticks in the newest `frames` frames, scaled to a per-60-frame rate.
    fn window_rate(&self, frames: usize) -> u32 {
        let n = frames.min(self.ticks.len());
        if n == 0 {
            return 0;
        }
        let ticks = self.ticks.iter().rev().take(n).filter(|t| **t).count() as u32;
        (ticks * RATE_WINDOW as u32 + n as u32 / 2) / n as u32
    }

    /// Feed one real frame; returns the multiplier to apply from the next
    /// frame on.
    pub fn observe(&mut self, obs: Observation) -> u16 {
        self.frame += 1;
        if self.ticks.len() == RATE_WINDOW {
            self.ticks.pop_front();
        }
        self.ticks.push_back(obs.tick);
        self.frames_since_change = self.frames_since_change.saturating_add(1);

        self.learn(obs);
        if self.turned_off {
            return self.multiplier;
        }
        self.hang_guard(obs);
        if self.turned_off {
            return self.multiplier;
        }
        if self.frame.is_multiple_of(EVAL_INTERVAL) {
            self.control();
        }
        self.multiplier
    }

    /// Accumulate per-second statistics. A second counts only if the
    /// multiplier was 100 for all of it and the picture was changing.
    fn learn(&mut self, obs: Observation) {
        if self.multiplier != NORMAL {
            self.sec_clean = false;
        }
        self.sec_frames += 1;
        self.sec_ticks += u32::from(obs.tick);
        self.sec_changing += u32::from(obs.picture_changed);
        if self.sec_frames < RATE_WINDOW as u32 {
            return;
        }
        self.clean_seconds += u32::from(self.sec_clean);
        if self.sec_clean && self.sec_changing >= MIN_CHANGING_FRAMES {
            self.histogram[self.sec_ticks.min(RATE_WINDOW as u32) as usize] += 1;
            self.valid_seconds += 1;
            if self.sec_ticks >= MIN_SIGNAL_TICKS {
                self.signal_seen = true;
            }
        }
        self.sec_frames = 0;
        self.sec_ticks = 0;
        self.sec_changing = 0;
        self.sec_clean = self.multiplier == NORMAL;
    }

    fn hang_guard(&mut self, obs: Observation) {
        if self.multiplier <= NORMAL || obs.picture_changed {
            self.still = 0;
            return;
        }
        self.still += 1;
        if self.still >= HANG_FRAMES {
            self.turned_off = true;
            self.change_to(NORMAL);
        }
    }

    fn change_to(&mut self, multiplier: u16) {
        self.multiplier = multiplier;
        self.frames_since_change = 0;
        self.held = 0;
        self.still = 0;
    }

    /// One control decision (called every [`EVAL_INTERVAL`] frames).
    fn control(&mut self) {
        // The window since the last change, so a decision never judges the
        // new clock by the old one's frames; at least half a second of it.
        let window = (self.frames_since_change as usize).min(RATE_WINDOW);
        if window < EVAL_INTERVAL as usize {
            return;
        }
        let Some(target) = self.target().filter(|_| self.armed()) else {
            return;
        };
        let rate = self.window_rate(window);
        if let Some(age) = self.probe_age.as_mut() {
            *age += EVAL_INTERVAL as u32;
        }
        self.futile_lock = self.futile_lock.saturating_sub(EVAL_INTERVAL as u32);
        let before_raise = self.rate_before_raise.take();

        if rate + SHORTFALL <= target {
            // Short of the target: a probe that fell short was wrong.
            if self.probe_age.take().is_some_and(|a| a <= PROBE_WINDOW) {
                self.hold = (self.hold * 2).min(HOLD_MAX);
            }
            // The last raise left the rate where it was: the clock is not
            // what the game is short of (it waits on something else, or its
            // tick signal is noise). Raising further only runs the chip
            // faster for nothing, so undo it and stop trying for a while.
            if self.multiplier > NORMAL && before_raise.is_some_and(|b| rate <= b + FUTILE_MARGIN) {
                self.futile_lock = self.futile_hold;
                self.futile_hold = (self.futile_hold * 2).min(HOLD_MAX);
                self.change_to(self.multiplier - NOTCH);
                return;
            }
            if self.multiplier < self.ceiling && self.futile_lock == 0 {
                self.rate_before_raise = Some(rate);
                self.change_to(self.multiplier + NOTCH);
            }
            return;
        }
        // At target: whatever the last raise did, it did enough.
        if before_raise.is_some() {
            self.futile_hold = FUTILE_START;
        }
        // A probe that survived its window was right: forget it and go back
        // to the short hold.
        if self.probe_age.is_some_and(|a| a > PROBE_WINDOW) {
            self.probe_age = None;
            self.hold = HOLD_START;
        }
        if rate > target + EXCESS {
            // Faster than the game's own pace: never.
            self.probe_age = None;
            if self.multiplier > NORMAL {
                self.change_to(self.multiplier - NOTCH);
            }
        } else if self.multiplier > NORMAL {
            self.held += EVAL_INTERVAL as u32;
            if self.held >= self.hold && self.probe_age.is_none() {
                self.probe_age = Some(0);
                self.change_to(self.multiplier - NOTCH);
            }
        }
    }
}

fn clamp_ceiling(ceiling: u16) -> u16 {
    (ceiling / NOTCH * NOTCH).clamp(NORMAL, MAX_CEILING)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic game: logic ticks at `rate(multiplier, second)` per 60
    /// frames (Bresenham-spread), capped by its own `cap` (a 60 Hz game
    /// cannot tick more than once a frame; a 30-locked one cannot exceed
    /// 30). The picture changes every frame unless `frozen`.
    struct Game {
        cap: u32,
        /// Natural ticks per second at 100% for the given second.
        base: Box<dyn Fn(u32) -> u32>,
        /// `Some(second)`: from then on the game hangs when the clock is
        /// above 100 (picture frozen, ticks report idle).
        hang_above_normal: bool,
        acc: u32,
        frame: u32,
    }

    impl Game {
        fn new(cap: u32, base: impl Fn(u32) -> u32 + 'static) -> Self {
            Self {
                cap,
                base: Box::new(base),
                hang_above_normal: false,
                acc: 0,
                frame: 0,
            }
        }

        fn rate(&self, multiplier: u16) -> u32 {
            let second = self.frame / 60;
            ((self.base)(second) * u32::from(multiplier) / 100).min(self.cap)
        }

        fn frame(&mut self, multiplier: u16) -> (Observation, u32) {
            let hung = self.hang_above_normal && multiplier > NORMAL;
            let rate = if hung { 60 } else { self.rate(multiplier) };
            self.acc += rate;
            let tick = self.acc >= 60;
            if tick {
                self.acc -= 60;
            }
            self.frame += 1;
            (
                Observation {
                    tick,
                    picture_changed: !hung,
                },
                rate,
            )
        }
    }

    struct Trace {
        multipliers: Vec<u16>,
        /// Delivered game rate (ticks/s the game actually ran at) per frame.
        rates: Vec<u32>,
    }

    fn run(gov: &mut SpeedGovernor, game: &mut Game, frames: u32) -> Trace {
        let mut multipliers = Vec::new();
        let mut rates = Vec::new();
        let mut m = gov.multiplier();
        for _ in 0..frames {
            let (obs, rate) = game.frame(m);
            m = gov.observe(obs);
            multipliers.push(m);
            rates.push(rate);
        }
        Trace { multipliers, rates }
    }

    #[test]
    fn steady_sixty_stays_at_normal() {
        let mut gov = SpeedGovernor::new(400, None);
        let mut game = Game::new(60, |_| 60);
        let t = run(&mut gov, &mut game, 60 * 60);
        assert!(t.multipliers.iter().all(|m| *m == 100));
        let s = gov.status();
        assert_eq!(s.target, Some(60));
        assert_eq!(s.target_source, TargetSource::Learned);
        assert_eq!(s.notice, Notice::Active);
    }

    #[test]
    fn learning_needs_ten_seconds_and_holds_normal_meanwhile() {
        // A game that is slow from the very first frame: nothing may be
        // raised until ten seconds have been watched.
        let mut gov = SpeedGovernor::new(400, None);
        let mut game = Game::new(60, |_| 45);
        let t = run(&mut gov, &mut game, 60 * 10);
        assert!(t.multipliers.iter().all(|m| *m == 100), "held for 10 s");
        assert_eq!(gov.status().notice, Notice::Active);
        assert_eq!(gov.target(), Some(45), "its own mode is the target");
    }

    #[test]
    fn periodic_dips_raise_only_during_dips_and_never_exceed_sixty() {
        // 60/s, except seconds 20-23 of every 30 run at 45/s.
        let mut gov = SpeedGovernor::new(400, None);
        let mut game = Game::new(60, |s| if (20..24).contains(&(s % 30)) { 45 } else { 60 });
        let t = run(&mut gov, &mut game, 60 * 150);
        assert!(
            t.rates.iter().all(|r| *r <= 60),
            "the game never ran faster than 60 ticks/s"
        );
        // Raised at some point in each dip window, never in the first 15 s
        // of any period, and back to 100 before the next dip.
        let at = |sec: u32| t.multipliers[(sec * 60) as usize];
        for period in 1..5u32 {
            let base = period * 30;
            assert_eq!(at(base + 5), 100, "period {period}: calm before the dip");
            assert!(
                (20..28).any(|s| at(base + s) > 100),
                "period {period}: raised for the dip"
            );
        }
        // The dip costs: at 100% it would be 45; the governor lifted it.
        let mean_dip: u32 = (0..4u32)
            .map(|i| t.rates[((120 + 21 + i) * 60) as usize])
            .sum::<u32>()
            / 4;
        assert!(mean_dip > 45, "slowdown reduced: {mean_dip}");
    }

    #[test]
    fn a_thirty_locked_game_learns_thirty_and_is_never_pushed_to_sixty() {
        let mut gov = SpeedGovernor::new(400, None);
        let mut game = Game::new(30, |_| 30);
        let t = run(&mut gov, &mut game, 60 * 90);
        assert_eq!(gov.target(), Some(30));
        assert!(t.multipliers.iter().all(|m| *m == 100));
        assert!(t.rates.iter().all(|r| *r <= 30));
    }

    /// A tick source reports at most one tick a frame, so only a target
    /// below 60 can be exceeded: a 30 Hz game pushed to 40 steps down at
    /// the next decision, never staying above its normal rate.
    #[test]
    fn a_rate_above_target_steps_down_at_the_next_decision() {
        let mut gov = SpeedGovernor::new(400, Some(30));
        let feed = |gov: &mut SpeedGovernor, per_second: u32, frames: u32| {
            let mut acc = 0;
            for _ in 0..frames {
                acc += per_second;
                let tick = acc >= 60;
                if tick {
                    acc -= 60;
                }
                gov.observe(Observation {
                    tick,
                    picture_changed: true,
                });
            }
        };
        feed(&mut gov, 20, 60);
        assert!(gov.multiplier() > 100, "short of 30: raised");
        let raised = gov.multiplier();
        feed(&mut gov, 40, EVAL_INTERVAL as u32 * 2);
        assert!(gov.multiplier() < raised, "40 > 31: stepped down");
        feed(&mut gov, 40, 60 * 3);
        assert_eq!(gov.multiplier(), 100, "and all the way to normal");
    }

    #[test]
    fn profile_target_acts_without_learning() {
        let mut gov = SpeedGovernor::new(400, Some(60));
        // 45/s from frame 0; the profile target means no 10 s wait for the
        // target, only for a signal (a signal needs one valid second).
        let mut game = Game::new(60, |_| 45);
        let t = run(&mut gov, &mut game, 60 * 12);
        assert!(
            t.multipliers[..59].iter().all(|m| *m == 100),
            "no signal yet in the first second"
        );
        assert!(
            t.multipliers.iter().any(|m| *m > 100),
            "raised on the profile's word"
        );
        assert_eq!(gov.status().target_source, TargetSource::Profile);
    }

    #[test]
    fn no_signal_never_raises() {
        // 5 ticks a second: below the signal threshold, like a game whose
        // idle never shows.
        let mut gov = SpeedGovernor::new(400, None);
        let mut game = Game::new(60, |_| 5);
        let t = run(&mut gov, &mut game, 60 * 60);
        assert!(t.multipliers.iter().all(|m| *m == 100));
        assert_eq!(gov.status().notice, Notice::NoSignal);
        // Same with a profile target: no signal, no overclock.
        let mut gov = SpeedGovernor::new(400, Some(60));
        let mut game = Game::new(60, |_| 5);
        let t = run(&mut gov, &mut game, 60 * 60);
        assert!(t.multipliers.iter().all(|m| *m == 100));
        assert_eq!(gov.status().notice, Notice::NoSignal);
    }

    #[test]
    fn a_game_that_never_gives_a_usable_second_is_called_no_signal_after_a_minute() {
        // Ticks in its first seconds, then a frozen picture and silence: it
        // never reaches ten seconds of changing picture.
        let mut gov = SpeedGovernor::new(400, None);
        let feed = |gov: &mut SpeedGovernor, tick: bool, changed: bool, n: u32| {
            for _ in 0..n {
                gov.observe(Observation {
                    tick,
                    picture_changed: changed,
                });
            }
        };
        feed(&mut gov, true, true, 60 * 3);
        assert_eq!(gov.status().notice, Notice::Learning, "not yet known");
        feed(&mut gov, false, false, 60 * 70);
        assert_eq!(gov.status().notice, Notice::NoSignal);
        assert_eq!(gov.multiplier(), 100);
    }

    #[test]
    fn a_hang_reverts_and_caps_for_the_session() {
        let mut gov = SpeedGovernor::new(400, None);
        // Slows from second 20 on, and hangs the moment the clock is raised.
        let mut game = Game::new(60, |s| if s >= 20 { 40 } else { 60 });
        game.hang_above_normal = true;
        let t = run(&mut gov, &mut game, 60 * 60);
        assert!(t.multipliers.iter().any(|m| *m > 100), "it did try");
        assert_eq!(*t.multipliers.last().unwrap(), 100);
        assert_eq!(gov.status().notice, Notice::TurnedOff);
        // It stays off.
        let tail = &t.multipliers[(40 * 60)..];
        assert!(tail.iter().all(|m| *m == 100));
        // And the hang lasted no longer than the guard allows.
        let first = t.multipliers.iter().position(|m| *m > 100).unwrap();
        let off = first
            + t.multipliers[first..]
                .iter()
                .position(|m| *m == 100)
                .unwrap();
        assert!(off - first <= HANG_FRAMES as usize + 1, "{}", off - first);
    }

    #[test]
    fn the_ceiling_is_respected_and_lowering_it_applies_at_once() {
        let mut gov = SpeedGovernor::new(200, None);
        let mut game = Game::new(60, |s| if s < 12 { 60 } else { 20 });
        let t = run(&mut gov, &mut game, 60 * 40);
        assert_eq!(t.multipliers.iter().copied().max(), Some(200));
        gov.set_ceiling(100);
        assert_eq!(gov.multiplier(), 100);
        gov.set_ceiling(950);
        assert_eq!(gov.status().multiplier, 100);
    }

    /// A game whose rate ignores the clock (it waits on something else): the
    /// governor may try one raise, must see it did not help, and put it
    /// back; it must not sit above 100 for any length of time.
    #[test]
    fn a_raise_that_does_not_help_is_undone_and_not_retried_at_once() {
        let mut gov = SpeedGovernor::new(400, None);
        // 60/s for 12 s (learned), then 0 ticks in 2 s of every 4 whatever
        // the clock does, like an attract loop that never idles.
        let mut acc = 0u32;
        let mut multipliers = Vec::new();
        for frame in 0..60 * 120u32 {
            let second = frame / 60;
            let quiet = second >= 12 && (second / 2) % 2 == 1;
            acc += if quiet { 0 } else { 60 };
            let tick = acc >= 60;
            if tick {
                acc -= 60;
            }
            multipliers.push(gov.observe(Observation {
                tick,
                picture_changed: true,
            }));
        }
        let above: usize = multipliers.iter().filter(|m| **m > 100).count();
        assert!(above > 0, "it did try once");
        // Each try is one 30-frame excursion, at most 200%.
        assert!(multipliers.iter().all(|m| *m <= 200), "never climbs on");
        assert!(
            above <= 6 * EVAL_INTERVAL as usize,
            "{above} frames above normal for a clock that does nothing"
        );
        // And the waits between tries grow.
        let starts: Vec<usize> = (1..multipliers.len())
            .filter(|&i| multipliers[i] > 100 && multipliers[i - 1] == 100)
            .collect();
        let gaps: Vec<usize> = starts.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            gaps.windows(2).all(|g| g[1] >= g[0]),
            "gaps never shrink: {gaps:?}"
        );
    }

    #[test]
    fn a_failed_probe_doubles_the_hold() {
        // A permanent 45/s slowdown after learning at 60: the governor needs
        // 200% for good. Probes down fail; each failure must double the wait
        // before the next, so probes get rarer.
        let mut gov = SpeedGovernor::new(400, None);
        let mut game = Game::new(60, |s| if s >= 12 { 45 } else { 60 });
        let t = run(&mut gov, &mut game, 60 * 120);
        let probes: Vec<usize> = (1..t.multipliers.len())
            .filter(|&i| t.multipliers[i] < t.multipliers[i - 1])
            .collect();
        assert!(probes.len() >= 2, "it probed: {probes:?}");
        let gaps: Vec<usize> = probes.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            gaps.windows(2).all(|g| g[1] >= g[0]) && gaps.last() > gaps.first(),
            "gaps grow as holds double (to the cap): {gaps:?}"
        );
        // Most of the time it sits at the speed that works.
        let high = t.multipliers.iter().filter(|m| **m >= 200).count();
        assert!(high > t.multipliers.len() / 2);
    }

    #[test]
    fn deterministic_for_one_stream() {
        let go = || {
            let mut gov = SpeedGovernor::new(400, None);
            let mut game = Game::new(60, |s| if s % 17 < 5 { 40 } else { 60 });
            run(&mut gov, &mut game, 60 * 90).multipliers
        };
        assert_eq!(go(), go());
    }
}
