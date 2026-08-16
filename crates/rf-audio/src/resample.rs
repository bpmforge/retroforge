//! The core-rate → device-rate resampler (ticket W2-05) — `rubato`, per
//! `docs/TECH_STACK.md`'s row ("On-the-fly ratio adjustment
//! (`set_resample_ratio_relative`) = dynamic rate control per libretro
//! reference").
//!
//! ## API note, because this crate moved
//!
//! `rubato` **3.0**'s surface is not 0.x's: there is no `FastFixedIn`, the
//! constructor is [`Async::new_poly`] with a [`FixedAsync`] discriminator,
//! and buffers cross the boundary as `audioadapter` adapters rather than
//! `Vec<Vec<T>>`. This wrapper exists partly so that one shape lives in one
//! place — verified against the vendored 3.0.0 source and its
//! `polyfixedin_ramp64` example, not from memory (CLAUDE.md law 2).
//!
//! ## Why polynomial, not sinc
//!
//! The ratio being corrected is within ±0.5% of 1.0 (`crate::rate`), so the
//! resampler is doing interpolation, not decimation: there is no new
//! aliasing to suppress, because the core already band-limited its output
//! when it decimated 1.789 MHz to 48 kHz (`rf_nes::apu`'s
//! `accumulate_sample`). Cubic polynomial interpolation at a near-unity
//! ratio is inaudible and costs a fraction of a sinc kernel's work per
//! sample, which matters because this runs on the emulator thread between
//! frames.

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, PolynomialDegree, Resampler as _};

/// Anything that can go wrong resampling. Deliberately small: the only
/// runtime failure `rubato` can report here is a buffer-shape mismatch,
/// which is a bug in this file rather than a condition a caller can handle.
#[derive(Debug)]
pub enum ResampleError {
    /// The resampler could not be constructed with the requested geometry.
    Construction(String),
    /// A processing call failed.
    Process(String),
}

impl std::fmt::Display for ResampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResampleError::Construction(m) => write!(f, "resampler construction failed: {m}"),
            ResampleError::Process(m) => write!(f, "resampling failed: {m}"),
        }
    }
}

impl std::error::Error for ResampleError {}

/// Mono `i16` resampler with a live, bounded ratio adjustment.
///
/// Mono because the NES APU mixes to one channel (`rf_nes::apu`'s
/// `mixed_sample`); a stereo core would construct this with `channels = 2`
/// and interleave, which the adapter layer already supports.
pub struct Resampler {
    inner: Async<f32>,
    chunk_size: usize,
    /// Input samples not yet consumed: `FixedAsync::Input` demands exactly
    /// `chunk_size` frames per call, and a core hands over whatever a frame
    /// happened to produce (~800), so the remainder waits here.
    pending: Vec<f32>,
    scratch_in: Vec<f32>,
    scratch_out: Vec<f32>,
}

impl Resampler {
    /// Build a resampler from `input_rate` to `output_rate`.
    ///
    /// `max_relative` is the ratio authority the rate controller may use;
    /// [`crate::rate::MAX_RATIO_DEVIATION`] is the value the app passes,
    /// and `rubato` refuses anything outside the band it was constructed
    /// with, so the two must agree.
    ///
    /// # Errors
    /// Returns [`ResampleError::Construction`] for a zero rate or a
    /// nonsensical relative bound.
    pub fn new(
        input_rate: u32,
        output_rate: u32,
        chunk_size: usize,
        max_relative: f32,
    ) -> Result<Self, ResampleError> {
        let ratio = f64::from(output_rate) / f64::from(input_rate);
        // TRAP, measured: `rubato` treats `max_resample_ratio_relative` as
        // a RECIPROCAL-symmetric band -- passing 1.005 allows [1/1.005,
        // 1.005] = [0.995025, 1.005], which EXCLUDES the additively
        // symmetric 0.995 the rate controller produces at full downward
        // authority. Constructed naively, the first fully-drained ring of a
        // session would have its correction rejected. The band is therefore
        // widened to whichever side is larger.
        let deviation = f64::from(max_relative).abs();
        let max_rel = (1.0 + deviation).max(1.0 / (1.0 - deviation));
        let inner = Async::<f32>::new_poly(
            ratio,
            max_rel,
            PolynomialDegree::Cubic,
            chunk_size,
            1,
            FixedAsync::Input,
        )
        .map_err(|e| ResampleError::Construction(e.to_string()))?;
        Ok(Self {
            inner,
            chunk_size,
            pending: Vec::with_capacity(chunk_size * 2),
            scratch_in: vec![0.0; chunk_size],
            scratch_out: Vec::new(),
        })
    }

    /// Apply the rate controller's relative ratio (1.0 = no correction).
    ///
    /// `ramp: true` — the ratio is interpolated across the next chunk
    /// rather than stepped, so a correction never produces a click. That is
    /// the whole reason a 0.5% bend is inaudible in practice.
    ///
    /// # Errors
    /// Returns [`ResampleError::Process`] if `relative` is outside the band
    /// this resampler was constructed with.
    pub fn set_relative_ratio(&mut self, relative: f32) -> Result<(), ResampleError> {
        self.inner
            .set_resample_ratio_relative(f64::from(relative), true)
            .map_err(|e| ResampleError::Process(e.to_string()))
    }

    /// Feed `input` and append every complete output chunk to `out`.
    ///
    /// Input is buffered: a call that does not complete a chunk produces no
    /// output and is not an error — the samples are held for the next call.
    ///
    /// # Errors
    /// Returns [`ResampleError::Process`] if `rubato` rejects the buffers.
    pub fn process(&mut self, input: &[i16], out: &mut Vec<i16>) -> Result<(), ResampleError> {
        self.pending
            .extend(input.iter().map(|&s| f32::from(s) / f32::from(i16::MAX)));

        while self.pending.len() >= self.chunk_size {
            self.scratch_in
                .copy_from_slice(&self.pending[..self.chunk_size]);
            self.pending.drain(..self.chunk_size);

            let frames_out = self.inner.output_frames_next();
            self.scratch_out.resize(frames_out, 0.0);

            let input_adapter = InterleavedSlice::new(&self.scratch_in, 1, self.chunk_size)
                .map_err(|e| ResampleError::Process(e.to_string()))?;
            let mut output_adapter =
                InterleavedSlice::new_mut(&mut self.scratch_out, 1, frames_out)
                    .map_err(|e| ResampleError::Process(e.to_string()))?;
            let indexing = Indexing {
                input_offset: 0,
                output_offset: 0,
                active_channels_mask: None,
                partial_len: None,
            };
            let (_, produced) = self
                .inner
                .process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing))
                .map_err(|e| ResampleError::Process(e.to_string()))?;

            out.extend(self.scratch_out[..produced].iter().map(|&s| {
                #[allow(clippy::cast_possible_truncation)]
                let scaled = (s * f32::from(i16::MAX))
                    .clamp(f32::from(i16::MIN), f32::from(i16::MAX))
                    as i16;
                scaled
            }));
        }
        Ok(())
    }

    /// Samples held back waiting for a complete chunk.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate::MAX_RATIO_DEVIATION;

    fn tone(samples: usize, period: usize) -> Vec<i16> {
        (0..samples)
            .map(|i| {
                let phase = (i % period) as f32 / period as f32;
                #[allow(clippy::cast_possible_truncation)]
                let v = ((phase * std::f32::consts::TAU).sin() * 16_000.0) as i16;
                v
            })
            .collect()
    }

    #[test]
    fn a_unity_ratio_preserves_length_and_signal_energy() {
        let mut r = Resampler::new(48_000, 48_000, 256, MAX_RATIO_DEVIATION).expect("construct");
        let input = tone(4096, 64);
        let mut out = Vec::new();
        r.process(&input, &mut out).expect("process");

        // Allow for the resampler's own delay: it cannot emit what it has
        // not yet seen, so the output is short by a bounded amount.
        assert!(
            out.len() >= input.len() - 512 && out.len() <= input.len() + 512,
            "unity ratio produced {} samples from {}",
            out.len(),
            input.len()
        );
        let energy_in: f64 = input.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        let energy_out: f64 = out.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        let ratio = energy_out / energy_in;
        assert!(
            (0.8..=1.2).contains(&ratio),
            "energy should survive a unity resample, got {ratio}"
        );
    }

    #[test]
    fn a_rate_change_scales_the_output_length() {
        let mut up = Resampler::new(48_000, 96_000, 256, MAX_RATIO_DEVIATION).expect("construct");
        let mut out = Vec::new();
        up.process(&tone(4096, 64), &mut out).expect("process");
        assert!(
            out.len() > 7_000,
            "doubling the rate should roughly double the samples, got {}",
            out.len()
        );

        let mut down = Resampler::new(48_000, 24_000, 256, MAX_RATIO_DEVIATION).expect("construct");
        let mut out = Vec::new();
        down.process(&tone(4096, 64), &mut out).expect("process");
        assert!(
            out.len() < 2_400,
            "halving the rate should roughly halve the samples, got {}",
            out.len()
        );
    }

    /// The ±0.5% band must be *reachable* — a resampler constructed with a
    /// tighter internal bound than the controller's authority would reject
    /// the controller's own output at the extremes, which is the kind of
    /// mismatch that only shows up under load.
    #[test]
    fn the_full_rate_control_band_is_accepted() {
        let mut r = Resampler::new(48_000, 48_000, 256, MAX_RATIO_DEVIATION).expect("construct");
        r.set_relative_ratio(1.0 + MAX_RATIO_DEVIATION)
            .expect("upper bound");
        r.set_relative_ratio(1.0 - MAX_RATIO_DEVIATION)
            .expect("lower bound");
        r.set_relative_ratio(1.0).expect("centre");
    }

    #[test]
    fn a_partial_chunk_is_held_rather_than_flushed_or_dropped() {
        let mut r = Resampler::new(48_000, 48_000, 256, MAX_RATIO_DEVIATION).expect("construct");
        let mut out = Vec::new();
        r.process(&tone(100, 32), &mut out).expect("process");
        assert!(out.is_empty(), "a partial chunk produces nothing yet");
        assert_eq!(r.pending(), 100, "and is held, not dropped");

        r.process(&tone(200, 32), &mut out).expect("process");
        assert!(!out.is_empty(), "completing the chunk produces output");
        assert_eq!(r.pending(), 44, "with the remainder still held");
    }
}
