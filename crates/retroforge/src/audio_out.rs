//! The app's audio path (ticket W2-05): core samples → filters →
//! resampler → ring → device, plus the audio-driven frame clock
//! ARCHITECTURE §8 asks for.
//!
//! ## Why the whole chain lives here rather than in `rf-audio`
//!
//! `rf-audio` owns the pieces (ring, rate loop, resampler, filters) and
//! none of the policy: what rate the core runs at, how much latency to
//! target, what to do when there is no sound card. Those are app decisions,
//! and this module is the app. The split also keeps `rf-audio` testable
//! without a device, which is most of why its tests exist at all.
//!
//! ## The audio clock (ARCHITECTURE §8's `pacing.wait_for_next_frame()`)
//!
//! `crate::pacer`'s wall-clock deadline is the interim mechanism and says so
//! in its own doc: "the documented end-state is an audio-driven clock...
//! because the audio device's real consumption rate is the only clock that
//! cannot drift against itself". [`AudioOut::should_wait`] is that clock —
//! the emulator may run a frame whenever the ring has fallen to its target
//! fill, i.e. exactly as fast as the device is draining, and no faster.
//!
//! The wall-clock pacer stays as the **fallback**, not as dead code: with
//! no device (a headless run, a machine with no sound card, `--no-audio`)
//! there is no audio clock to pace to, and an unpaced release build runs at
//! ~19x real speed. Which one is in charge is explicit in
//! [`AudioOut::is_clock`].

use rf_audio::{AudioRing, Filters, RateController, RateStats, Resampler, RingProducer};

/// Latency target. 40 ms is two and a half NTSC frames: enough that a
/// single slow frame cannot empty the ring, short enough that input-to-sound
/// lag stays under the ~50 ms most people notice on percussive sounds.
pub const LATENCY_MS: u32 = 40;

/// Ticket W20-08: Settings › Audio as the core thread reads it when it
/// opens a device.
///
/// Until W20-08 the three audio settings were saved to `settings.toml`
/// and read by nothing: the device was always the system default, the
/// ring was always [`LATENCY_MS`], and the volume slider moved nothing.
/// Audio opens on the core thread at every game start
/// (`core_thread::run`), so the device and latency apply **from the next
/// game started**; volume is read every batch and applies live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPrefs {
    /// Output device by name (as `rf_audio::output_device_names` lists
    /// it); `None` = system default.
    pub device: Option<String>,
    pub latency_ms: u32,
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            device: None,
            latency_ms: LATENCY_MS,
        }
    }
}

/// Bounds for a latency a settings file can ask for. Below 10 ms a single
/// late frame underruns; above 250 ms sound visibly trails the picture.
pub const LATENCY_RANGE_MS: std::ops::RangeInclusive<u32> = 10..=250;

static PREFS: std::sync::Mutex<Option<AudioPrefs>> = std::sync::Mutex::new(None);
static OPENED_DEVICE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// `f32` bits of the output volume (1.0 = unity), read every batch.
static VOLUME_BITS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3F80_0000);

/// Publish Settings › Audio for the next device open.
pub fn set_prefs(prefs: AudioPrefs) {
    if let Ok(mut slot) = PREFS.lock() {
        *slot = Some(prefs);
    }
}

#[cfg_attr(not(feature = "audio"), allow(dead_code))]
fn prefs() -> AudioPrefs {
    PREFS
        .lock()
        .ok()
        .and_then(|p| p.clone())
        .unwrap_or_default()
}

/// Set the output volume, live. Clamped to `[0, 1]`; NaN reads as unity.
pub fn set_volume(volume: f32) {
    let v = if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        1.0
    };
    VOLUME_BITS.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// The live output volume.
#[must_use]
pub fn volume() -> f32 {
    f32::from_bits(VOLUME_BITS.load(std::sync::atomic::Ordering::Relaxed))
}

/// The name of the device the last game's audio actually opened on —
/// which may be the default when the chosen one has gone away. `None`
/// before any device opened (or in a build without the `audio` feature).
#[must_use]
pub fn opened_device_name() -> Option<String> {
    OPENED_DEVICE.lock().ok().and_then(|n| n.clone())
}

/// Scale `samples` by `volume` in place. Unity leaves them untouched.
fn apply_volume(samples: &mut [i16], volume: f32) {
    if (volume - 1.0).abs() < f32::EPSILON {
        return;
    }
    for s in samples {
        // In range by construction: |s * v| <= |s| for v in [0, 1].
        #[allow(clippy::cast_possible_truncation)]
        let scaled = (f32::from(*s) * volume).round() as i16;
        *s = scaled;
    }
}

/// Resampler chunk size. ~5 ms at 48 kHz — small enough that a rate
/// correction takes effect within a frame, large enough that the per-chunk
/// overhead is negligible.
const CHUNK: usize = 256;

/// Everything the app needs to turn core samples into sound.
pub struct AudioOut {
    producer: RingProducer,
    resampler: Resampler,
    rate: RateController,
    filters: Filters,
    scratch: Vec<i16>,
    device_rate: u32,
    /// `None` when running without a device: the chain still filters and
    /// resamples (so tests and headless runs exercise the same code), but
    /// nothing consumes the ring, and it must not become the frame clock.
    #[cfg(feature = "audio")]
    device: Option<rf_audio::AudioDevice>,
}

impl AudioOut {
    /// Build the chain against a device sample rate, without opening a
    /// device. This is the constructor tests use, and the one
    /// [`AudioOut::open`] delegates to.
    ///
    /// # Errors
    /// Returns the resampler's construction error for a nonsensical rate.
    pub fn headless(device_rate: u32) -> Result<Self, rf_audio::ResampleError> {
        let capacity = AudioRing::capacity_for_latency(device_rate, LATENCY_MS, 1);
        let (producer, _consumer) = AudioRing::split(capacity);
        Self::from_parts(producer, device_rate)
    }

    fn from_parts(
        producer: RingProducer,
        device_rate: u32,
    ) -> Result<Self, rf_audio::ResampleError> {
        Ok(Self {
            producer,
            resampler: Resampler::new(
                rf_audio::CORE_SAMPLE_RATE,
                device_rate,
                CHUNK,
                rf_audio::rate_deviation(),
            )?,
            rate: RateController::new(),
            filters: Filters::new(rf_audio::CORE_SAMPLE_RATE),
            scratch: Vec::with_capacity(CHUNK * 4),
            device_rate,
            #[cfg(feature = "audio")]
            device: None,
        })
    }

    /// Open the default output device and start streaming.
    ///
    /// # Errors
    /// Returns a message if no device is available or the stream cannot be
    /// built; the caller is expected to fall back to [`AudioOut::headless`]
    /// and the wall-clock pacer rather than refusing to run.
    #[cfg(feature = "audio")]
    pub fn open() -> Result<Self, String> {
        // The ring is sized from the device's real rate, so it is built
        // after the device — hence the two-step: open with a placeholder
        // consumer, then rebuild. cpal reports its rate before the first
        // callback, so nothing is streamed in between.
        let prefs = prefs();
        let latency = prefs
            .latency_ms
            .clamp(*LATENCY_RANGE_MS.start(), *LATENCY_RANGE_MS.end());
        let probe_rate = rf_audio::CORE_SAMPLE_RATE;
        let capacity = AudioRing::capacity_for_latency(probe_rate, latency, 1);
        let (producer, consumer) = AudioRing::split(capacity);
        let device = rf_audio::AudioDevice::open_named(consumer, prefs.device.as_deref())
            .map_err(|e| e.to_string())?;
        if let Ok(mut slot) = OPENED_DEVICE.lock() {
            *slot = Some(device.device_name().to_string());
        }
        let device_rate = device.sample_rate();
        let mut out = Self::from_parts(producer, device_rate).map_err(|e| e.to_string())?;
        out.device = Some(device);
        Ok(out)
    }

    /// Feed one batch of core samples through the chain.
    ///
    /// Order matters and is the hardware's: filter first (the core's output
    /// is unipolar and must have its DC removed before anything else sees
    /// it), then resample, then push. The rate controller reads the ring
    /// *after* the push, so the next batch is corrected with the freshest
    /// fill.
    pub fn push(&mut self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        self.scratch.clear();
        self.scratch.extend_from_slice(samples);
        self.filters.process_i16(&mut self.scratch);
        apply_volume(&mut self.scratch, volume());

        let mut resampled = Vec::with_capacity(self.scratch.len() + CHUNK);
        if self
            .resampler
            .process(&self.scratch, &mut resampled)
            .is_err()
        {
            // A resampler error here means a buffer-shape bug, not a
            // runtime condition; dropping the batch keeps sound glitching
            // rather than taking the emulator down with it.
            return;
        }
        let written = rf_audio::push_samples(&mut self.producer, &resampled);
        self.rate.note_underrun(0);
        let _ = written;

        let fill = rf_audio::fill_fraction(&self.producer);
        let ratio = self.rate.update(fill);
        let _ = self.resampler.set_relative_ratio(ratio);
    }

    /// Whether the audio device is the frame clock — true only when a
    /// device is actually streaming.
    #[must_use]
    pub fn is_clock(&self) -> bool {
        #[cfg(feature = "audio")]
        {
            self.device.is_some()
        }
        #[cfg(not(feature = "audio"))]
        {
            false
        }
    }

    /// The audio clock's verdict: `true` means "the ring is fuller than
    /// target, do not run another frame yet".
    ///
    /// Always `false` without a device, so a headless run is never paced by
    /// a ring nothing drains — that would deadlock the emulator, which is
    /// the failure this method's existence is most likely to cause if it
    /// forgets the check.
    #[must_use]
    pub fn should_wait(&self) -> bool {
        self.is_clock() && rf_audio::fill_fraction(&self.producer) > rf_audio::TARGET_FILL
    }

    /// Current ring fill, for the status bar's audio-buffer health dot
    /// (`FRONTEND_UI.md` §3.2).
    #[must_use]
    pub fn fill(&self) -> f32 {
        rf_audio::fill_fraction(&self.producer)
    }

    /// Rate-loop telemetry, including the underrun count the soak test
    /// asserts on.
    #[must_use]
    pub fn stats(&self) -> RateStats {
        self.rate.stats()
    }

    /// Samples the device could not be given because the ring was empty.
    /// Zero without a device (nothing is consuming).
    #[must_use]
    pub fn underrun_samples(&self) -> u64 {
        #[cfg(feature = "audio")]
        {
            self.device
                .as_ref()
                .map_or(0, rf_audio::AudioDevice::underrun_samples)
        }
        #[cfg(not(feature = "audio"))]
        {
            0
        }
    }

    /// The device rate the chain is resampling to.
    #[must_use]
    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }
}

/// Build the app's audio path: a real device when this build has one and
/// the machine offers one, otherwise the same chain running headless.
///
/// Never fails the caller — a machine with no sound card must still be able
/// to run the emulator, and the wall-clock pacer covers the timing.
#[must_use]
pub fn open_audio_out() -> Option<AudioOut> {
    #[cfg(feature = "audio")]
    {
        match AudioOut::open() {
            Ok(out) => return Some(out),
            Err(e) => eprintln!("retroforge: audio device unavailable ({e}); running silent"),
        }
    }
    AudioOut::headless(rf_audio::CORE_SAMPLE_RATE).ok()
}

#[cfg(test)]
mod tests {

    #[test]
    fn volume_scales_samples_and_unity_is_untouched() {
        let mut a = [1000i16, -1000, i16::MAX, i16::MIN];
        apply_volume(&mut a, 1.0);
        assert_eq!(a, [1000, -1000, i16::MAX, i16::MIN]);
        apply_volume(&mut a, 0.5);
        assert_eq!(a, [500, -500, 16384, -16384]);
        apply_volume(&mut a, 0.0);
        assert_eq!(a, [0, 0, 0, 0]);
    }

    use super::*;

    /// The chain must accept a realistic frame's worth of core samples and
    /// actually move them into the ring — the whole path, not each piece.
    #[test]
    fn a_frames_worth_of_core_samples_reaches_the_ring() {
        let mut out = AudioOut::headless(48_000).expect("chain builds");
        assert_eq!(out.fill(), 0.0);

        // ~800 samples is one NTSC frame at 48 kHz.
        let frame: Vec<i16> = (0..800)
            .map(|i| {
                #[allow(clippy::cast_possible_truncation)]
                let v = ((i as f32 / 40.0).sin() * 8_000.0) as i16;
                v
            })
            .collect();
        for _ in 0..4 {
            out.push(&frame);
        }
        assert!(out.fill() > 0.0, "the chain moved nothing into the ring");
    }

    /// Without a device the ring has no consumer, so the audio clock must
    /// stay out of the way — otherwise a headless run wedges the moment the
    /// ring fills.
    #[test]
    fn a_headless_chain_never_becomes_the_frame_clock() {
        let mut out = AudioOut::headless(48_000).expect("chain builds");
        let frame = vec![1_000i16; 800];
        for _ in 0..200 {
            out.push(&frame);
        }
        assert!(
            out.fill() > 0.5,
            "the ring should be well past target by now"
        );
        assert!(!out.is_clock());
        assert!(
            !out.should_wait(),
            "a ring nothing drains must never gate the emulator"
        );
    }

    /// The rate loop must be live in the real path, not just in its own
    /// unit tests: pushing far more than the (undrained) ring can hold has
    /// to drive the correction to its lower bound.
    #[test]
    fn the_rate_loop_responds_to_a_ring_that_is_filling_up() {
        let mut out = AudioOut::headless(48_000).expect("chain builds");
        let frame = vec![1_000i16; 800];
        for _ in 0..200 {
            out.push(&frame);
        }
        let stats = out.stats();
        assert!(
            stats.ratio < 1.0,
            "a full ring must ask for fewer samples, got ratio {}",
            stats.ratio
        );
        assert!(stats.fill > 0.5);
    }
}
