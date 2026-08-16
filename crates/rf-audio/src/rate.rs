//! Dynamic rate control (ticket W2-05, FR-FE-006): hold the ring at half
//! full by bending the resample ratio, within ±0.5%.
//!
//! ## Why ±0.5%, and why a proportional term is enough
//!
//! The error being corrected is a crystal mismatch between the host's audio
//! clock and the emulated console's nominal rate — parts per million, not
//! parts per hundred. ±0.5% is three orders of magnitude more authority
//! than the steady-state error needs, which is what lets the loop also
//! absorb transients (a stalled frame, a resized buffer) without ever
//! reaching for a correction big enough to hear: 0.5% is ~8.7 cents, below
//! the ~10-cent threshold at which a pitch shift becomes noticeable on a
//! sustained tone, and the loop only sits at the extreme while catching up.
//!
//! A proportional controller with no integral term is deliberate. The
//! quantity being controlled *is* an integrator already (ring fill is the
//! accumulated difference of two rates), so P alone gives zero steady-state
//! error; adding I to an integrating plant is how you get an oscillator.

/// Where the controller tries to hold the ring, as a fraction of capacity.
/// Half full leaves equal room to absorb a producer stall and a consumer
/// stall — the two failure directions are symmetric, so the setpoint is.
pub const TARGET_FILL: f32 = 0.5;

/// The ±0.5% authority FR-FE-006 specifies.
pub const MAX_RATIO_DEVIATION: f32 = 0.005;

/// How hard the loop pushes: full authority is reached when the ring is a
/// quarter of capacity away from target (i.e. empty or full).
///
/// Derivation, so the constant is checkable rather than tuned-by-ear: the
/// error range that matters is `TARGET_FILL ± 0.25`; mapping that to
/// `±MAX_RATIO_DEVIATION` gives `0.005 / 0.25 = 0.02`.
const PROPORTIONAL_GAIN: f32 = MAX_RATIO_DEVIATION / 0.25;

/// What the controller last did and why — for the status bar's
/// audio-buffer health dot (`FRONTEND_UI.md` §3.2) and for the soak test.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateStats {
    /// Ring fill at the last update, as a fraction of capacity.
    pub fill: f32,
    /// The relative resample ratio last applied: 1.0 means "no correction",
    /// 1.005 means "emit 0.5% more samples than nominal".
    pub ratio: f32,
    /// Cumulative underruns observed since construction.
    pub underruns: u64,
}

/// The rate-control loop. Lives on the producer side; the device callback
/// never touches it (`crate::` module doc's realtime discipline).
#[derive(Debug)]
pub struct RateController {
    ratio: f32,
    fill: f32,
    underruns: u64,
}

impl Default for RateController {
    fn default() -> Self {
        Self::new()
    }
}

impl RateController {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ratio: 1.0,
            fill: TARGET_FILL,
            underruns: 0,
        }
    }

    /// Feed the loop the current ring fill and get the relative resample
    /// ratio to apply (`rubato`'s `set_resample_ratio_relative`).
    ///
    /// **The sign, derived rather than guessed.** The ratio is output-rate
    /// over input-rate, so a ratio above 1.0 makes the resampler emit MORE
    /// samples per core sample — i.e. push more into the ring. The device
    /// drains the ring at its own fixed rate, which the controller cannot
    /// touch. Therefore:
    ///
    /// - ring **above** target (filling, heading for overflow and growing
    ///   latency) ⇒ produce **fewer** samples ⇒ ratio **below** 1.0;
    /// - ring **below** target (draining, heading for an underrun) ⇒
    ///   produce **more** samples ⇒ ratio **above** 1.0.
    ///
    /// Same direction libretro's dynamic rate control uses. This function's
    /// first draft had it backwards, behind a plausible-sounding "a filling
    /// ring must be drained faster" comment; what caught it was
    /// `the_loop_converges_on_a_persistent_clock_mismatch`, which simulates
    /// the closed loop instead of asserting a direction in prose. A test
    /// that only checked the sign would have agreed with the bug.
    pub fn update(&mut self, fill: f32) -> f32 {
        self.fill = fill;
        let error = fill - TARGET_FILL;
        self.ratio = (1.0 - error * PROPORTIONAL_GAIN)
            .clamp(1.0 - MAX_RATIO_DEVIATION, 1.0 + MAX_RATIO_DEVIATION);
        self.ratio
    }

    /// Record that the device callback could not be filled completely.
    /// Counted, never corrected for directly: the fill it produced is
    /// already the loop's input, and reacting twice to one event is how a
    /// controller rings.
    pub fn note_underrun(&mut self, missing_samples: usize) {
        if missing_samples > 0 {
            self.underruns += 1;
        }
    }

    #[must_use]
    pub fn stats(&self) -> RateStats {
        RateStats {
            fill: self.fill,
            ratio: self.ratio,
            underruns: self.underruns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_at_target_asks_for_no_correction() {
        let mut c = RateController::new();
        assert_eq!(c.update(TARGET_FILL), 1.0);
    }

    /// The sign convention — the one thing here that is easy to get
    /// backwards (this file did, on its first draft) and impossible to
    /// notice by reading. See [`RateController::update`]'s derivation.
    #[test]
    fn a_filling_ring_produces_less_and_a_draining_one_produces_more() {
        let mut c = RateController::new();
        assert!(c.update(0.9) < 1.0, "nearly full -> emit fewer samples");
        assert!(c.update(0.1) > 1.0, "nearly empty -> emit more samples");
    }

    /// FR-FE-006's ±0.5% is a hard bound, not a target: even a completely
    /// empty or completely full ring may not exceed it.
    #[test]
    fn the_correction_never_leaves_the_half_percent_band() {
        let mut c = RateController::new();
        for fill in [0.0, 0.001, 0.25, 0.75, 0.999, 1.0] {
            let ratio = c.update(fill);
            assert!(
                (1.0 - MAX_RATIO_DEVIATION..=1.0 + MAX_RATIO_DEVIATION).contains(&ratio),
                "fill {fill} produced {ratio}, outside ±0.5%"
            );
        }
    }

    /// Full authority is reached exactly at the quarter-capacity excursion
    /// the gain is derived from — the derivation in `PROPORTIONAL_GAIN`'s
    /// doc, checked rather than asserted in prose.
    #[test]
    fn full_authority_is_reached_a_quarter_of_capacity_from_target() {
        let mut c = RateController::new();
        let full_side = c.update(TARGET_FILL + 0.25);
        assert!((full_side - (1.0 - MAX_RATIO_DEVIATION)).abs() < 1e-6);
        let empty_side = c.update(TARGET_FILL - 0.25);
        assert!((empty_side - (1.0 + MAX_RATIO_DEVIATION)).abs() < 1e-6);
    }

    /// **The test that matters**: the closed loop must converge, not merely
    /// point the right way. Simulated with a device consuming 0.3% faster
    /// than the core produces — three orders of magnitude worse than a real
    /// crystal mismatch, and still inside the ±0.5% authority.
    ///
    /// Model, stated so it can be argued with: each iteration the resampler
    /// emits `nominal * ratio` samples into the ring and the device removes
    /// `nominal * 1.003`. Both are per-frame quantities; the loop runs once
    /// per frame in the app.
    #[test]
    fn the_loop_converges_on_a_persistent_clock_mismatch() {
        let mut c = RateController::new();
        let capacity = 4096.0f32;
        let mut used = capacity * TARGET_FILL;
        let nominal = 800.0f32;
        let device_rate = nominal * 1.003;

        for _ in 0..5_000 {
            let ratio = c.update(used / capacity);
            used = (used + nominal * ratio - device_rate).clamp(0.0, capacity);
        }

        let fill = used / capacity;
        assert!(
            (fill - TARGET_FILL).abs() < 0.2,
            "loop failed to hold the ring near target: settled at {fill}"
        );
        assert!(
            c.stats().ratio > 1.0,
            "a device draining faster than nominal must settle above ratio 1.0, got {}",
            c.stats().ratio
        );
        assert!(
            used > 0.0 && used < capacity,
            "the ring must neither starve nor overflow: {used} of {capacity}"
        );
    }

    #[test]
    fn underruns_are_counted_only_when_samples_were_actually_missing() {
        let mut c = RateController::new();
        c.note_underrun(0);
        assert_eq!(c.stats().underruns, 0);
        c.note_underrun(12);
        c.note_underrun(1);
        assert_eq!(c.stats().underruns, 2);
    }
}
