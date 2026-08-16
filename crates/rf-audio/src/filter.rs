//! The NES output-stage filter chain (ticket W2-05) —
//! nesdev.org/wiki/APU_Mixer, verbatim: "The NES hardware follows the DACs
//! with a surprisingly involved circuit that adds several low-pass and
//! high-pass filters: a first-order high-pass filter at 90 Hz; another
//! first-order high-pass filter at 440 Hz; a first-order low-pass filter at
//! 14 kHz."
//!
//! ## Why this is not optional, and why it lives here
//!
//! The mixer's output is **unipolar**: `rf_nes::apu`'s `mixed_output`
//! returns 0.0..1.0, and the triangle's DAC alone parks it around 0.25 with
//! no signal playing at all. Sent to a device unfiltered, that DC offset
//! eats most of the headroom, thumps on start/stop, and is exactly the sort
//! of thing that damages speakers driven hard. The 90 Hz high-pass is what
//! removes it — the filters are not a tone control, they are the difference
//! between a usable signal and a biased one.
//!
//! It lives in `rf-audio` rather than in the core because it is analog
//! output-stage behavior, not machine state: a filter in the core would sit
//! inside the determinism invariant for no reason, and `rf_nes::apu`'s
//! module doc already routes it here.
//!
//! Measured consequence, recorded because it is the reason this module
//! exists at all: `apu_mixer`'s cancellation check reads ~0.88 of peak on
//! the square ROM before filtering (the DC swamps the RMS) and ~0.02 after.

/// One-pole filters at the three corner frequencies nesdev specifies.
///
/// Coefficients come from the standard one-pole bilinear forms, computed
/// from the sample rate at construction rather than hardcoded, so the chain
/// is correct at 44.1 kHz as well as 48 kHz.
#[derive(Debug, Clone)]
pub struct Filters {
    hp90: HighPass,
    hp440: HighPass,
    lp14k: LowPass,
}

impl Filters {
    /// Build the chain for `sample_rate`.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        Self {
            hp90: HighPass::new(90.0, sample_rate),
            hp440: HighPass::new(440.0, sample_rate),
            lp14k: LowPass::new(14_000.0, sample_rate),
        }
    }

    /// Filter one sample, in the order the hardware applies them.
    pub fn process(&mut self, sample: f32) -> f32 {
        let x = self.hp90.process(sample);
        let x = self.hp440.process(x);
        self.lp14k.process(x)
    }

    /// Filter a block of `i16` samples in place.
    pub fn process_i16(&mut self, samples: &mut [i16]) {
        for sample in samples {
            let filtered = self.process(f32::from(*sample));
            #[allow(clippy::cast_possible_truncation)]
            let clamped = filtered.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
            *sample = clamped;
        }
    }
}

/// `y[n] = a * (y[n-1] + x[n] - x[n-1])`, the standard one-pole high-pass.
#[derive(Debug, Clone)]
struct HighPass {
    a: f32,
    prev_in: f32,
    prev_out: f32,
}

impl HighPass {
    fn new(cutoff_hz: f32, sample_rate: u32) -> Self {
        let rc = 1.0 / (std::f32::consts::TAU * cutoff_hz);
        let dt = 1.0 / sample_rate as f32;
        Self {
            a: rc / (rc + dt),
            prev_in: 0.0,
            prev_out: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.a * (self.prev_out + x - self.prev_in);
        self.prev_in = x;
        self.prev_out = y;
        y
    }
}

/// `y[n] = y[n-1] + a * (x[n] - y[n-1])`, the standard one-pole low-pass.
#[derive(Debug, Clone)]
struct LowPass {
    a: f32,
    prev_out: f32,
}

impl LowPass {
    fn new(cutoff_hz: f32, sample_rate: u32) -> Self {
        let rc = 1.0 / (std::f32::consts::TAU * cutoff_hz);
        let dt = 1.0 / sample_rate as f32;
        Self {
            a: dt / (rc + dt),
            prev_out: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        self.prev_out += self.a * (x - self.prev_out);
        self.prev_out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn sine(freq: f32, samples: usize, rate: u32) -> Vec<f32> {
        (0..samples)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (t * freq * std::f32::consts::TAU).sin() * 10_000.0
            })
            .collect()
    }

    /// The reason the chain exists: a constant input must decay to nothing.
    /// The NES mixer's output is unipolar, so this is not a corner case —
    /// it is what every silent moment looks like.
    #[test]
    fn a_dc_input_is_removed() {
        let mut f = Filters::new(48_000);
        let mut last = 0.0;
        for _ in 0..48_000 {
            last = f.process(8_000.0);
        }
        assert!(
            last.abs() < 1.0,
            "DC must be removed, still at {last} after a second"
        );
    }

    /// A 1 kHz tone sits between the 440 Hz high-pass and the 14 kHz
    /// low-pass, so it must pass essentially untouched.
    #[test]
    fn a_mid_band_tone_passes_with_its_level_intact() {
        let mut f = Filters::new(48_000);
        let input = sine(1_000.0, 48_000, 48_000);
        let output: Vec<f32> = input.iter().map(|&s| f.process(s)).collect();
        // Skip the filters' settling time.
        let ratio = rms(&output[4_800..]) / rms(&input[4_800..]);
        assert!(
            (0.85..=1.05).contains(&ratio),
            "1 kHz should pass at roughly unity, got {ratio}"
        );
    }

    /// Both ends of the chain must actually attenuate, or the corner
    /// frequencies are decoration.
    #[test]
    fn the_chain_attenuates_below_and_above_its_corners() {
        let mut low = Filters::new(48_000);
        let input = sine(50.0, 48_000, 48_000);
        let output: Vec<f32> = input.iter().map(|&s| low.process(s)).collect();
        let low_ratio = rms(&output[4_800..]) / rms(&input[4_800..]);
        assert!(
            low_ratio < 0.3,
            "50 Hz is below both high-pass corners and must be cut, got {low_ratio}"
        );

        let mut high = Filters::new(48_000);
        let input = sine(20_000.0, 48_000, 48_000);
        let output: Vec<f32> = input.iter().map(|&s| high.process(s)).collect();
        let high_ratio = rms(&output[4_800..]) / rms(&input[4_800..]);
        assert!(
            high_ratio < 0.7,
            "20 kHz is above the 14 kHz corner and must be cut, got {high_ratio}"
        );
    }

    /// The coefficients are derived from the sample rate, not hardcoded for
    /// 48 kHz — the same tone must be treated the same way at 44.1 kHz.
    #[test]
    fn the_corners_track_the_sample_rate() {
        let mut at_48 = Filters::new(48_000);
        let mut at_441 = Filters::new(44_100);
        let in_48 = sine(1_000.0, 48_000, 48_000);
        let in_441 = sine(1_000.0, 44_100, 44_100);
        let out_48: Vec<f32> = in_48.iter().map(|&s| at_48.process(s)).collect();
        let out_441: Vec<f32> = in_441.iter().map(|&s| at_441.process(s)).collect();
        let r48 = rms(&out_48[4_800..]) / rms(&in_48[4_800..]);
        let r441 = rms(&out_441[4_410..]) / rms(&in_441[4_410..]);
        assert!(
            (r48 - r441).abs() < 0.05,
            "1 kHz should see the same response at both rates: {r48} vs {r441}"
        );
    }
}
