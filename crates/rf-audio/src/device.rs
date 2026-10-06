//! The cpal output stream (ticket W2-05), behind the `device` feature.
//!
//! ## Why this is optional, and what that costs
//!
//! `cpal` pulls in ALSA on Linux, which the CI runner does not install, and
//! nothing in here can be tested headlessly anyway — opening a device needs
//! a device. Everything that *can* be tested (the ring, the rate loop, the
//! resampler) lives outside this module and is compiled and tested
//! unconditionally. The app enables `device`; `cargo test --workspace` does
//! not need to. The cost is stated plainly: **this file's code path is
//! exercised by the ignored soak test and by running the app, not by the
//! gate.**
//!
//! ## Realtime discipline in the callback
//!
//! `docs/TECH_STACK.md` §"Frame path discipline": "no allocation in the
//! cpal callback; no locks". The callback here does exactly three things —
//! pop from the SPSC ring, convert to the device's sample type, and add to
//! an atomic underrun counter. No `Vec`, no `Mutex`, no logging. Anything
//! that needs to allocate or think happens on the producer side.
//!
//! ## Request-but-tolerate
//!
//! TECH_STACK's cpal row flags "Backend buffer-size quirks →
//! request-but-tolerate (R-15)": a requested buffer size is a hint, and
//! backends are free to hand back something else (or ignore
//! [`cpal::BufferSize::Fixed`] entirely). This module therefore never
//! asserts on the delivered buffer size; the ring is sized from the
//! *latency target*, and the rate controller absorbs whatever cadence the
//! device actually uses.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Error as CpalError, SampleFormat, StreamConfig};

use crate::ring::{self, RingConsumer};

/// Anything that can go wrong opening or starting a device.
#[derive(Debug)]
pub enum DeviceError {
    /// No default output device (headless machine, no sound card, audio
    /// service down).
    NoOutputDevice,
    /// The device refused to describe or build a stream.
    Cpal(String),
    /// The device's sample format is not one this module converts to.
    UnsupportedFormat(SampleFormat),
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::NoOutputDevice => write!(f, "no default audio output device"),
            DeviceError::Cpal(m) => write!(f, "audio device error: {m}"),
            DeviceError::UnsupportedFormat(fmt) => {
                write!(f, "unsupported device sample format: {fmt:?}")
            }
        }
    }
}

impl std::error::Error for DeviceError {}

/// A running output stream plus the counters the app reads.
///
/// Dropping this stops the stream — cpal streams are RAII, so the app must
/// keep it alive for as long as it wants sound.
pub struct AudioDevice {
    _stream: cpal::Stream,
    /// The rate the device actually runs at, which is what the resampler
    /// must target — never assume it is what was asked for.
    sample_rate: u32,
    channels: u16,
    underrun_samples: Arc<AtomicU64>,
    /// The device's name as cpal prints it (`Display`, cpal-0.18.1
    /// traits.rs: "For the device name as a string, use
    /// `device.to_string()`").
    device_name: String,
}

/// Names of every output device the default host offers, in host order.
/// Empty when enumeration fails — the caller still has "System default".
#[must_use]
pub fn output_device_names() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

impl AudioDevice {
    /// Open the default output device and start streaming from `consumer`.
    ///
    /// The core's mono stream is duplicated across however many channels
    /// the device wants: a mono core into a stereo device is the common
    /// case, and duplicating is the only correct answer for it (panning a
    /// mono NES would be a colourisation, not a fidelity choice).
    ///
    /// # Errors
    /// Returns [`DeviceError`] if there is no output device, the backend
    /// refuses to build the stream, or its sample format is not `f32`,
    /// `i16` or `u16`.
    pub fn open(consumer: RingConsumer) -> Result<Self, DeviceError> {
        Self::open_named(consumer, None)
    }

    /// Open the output device whose name is `name` (as
    /// [`output_device_names`] lists it), or the default device when
    /// `name` is `None` **or no longer present** — a USB headset unplugged
    /// since it was chosen must not leave the emulator silent (ticket
    /// W20-08). [`AudioDevice::device_name`] reports which one opened, so
    /// the caller can say so instead of pretending the choice took.
    ///
    /// # Errors
    /// As [`AudioDevice::open`].
    pub fn open_named(mut consumer: RingConsumer, name: Option<&str>) -> Result<Self, DeviceError> {
        let host = cpal::default_host();
        let chosen = name.and_then(|want| {
            host.output_devices()
                .ok()
                .and_then(|mut devices| devices.find(|d| d.to_string() == want))
        });
        let device = chosen
            .or_else(|| host.default_output_device())
            .ok_or(DeviceError::NoOutputDevice)?;
        let device_name = device.to_string();
        let supported = device
            .default_output_config()
            .map_err(|e| DeviceError::Cpal(e.to_string()))?;

        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let channels = config.channels;
        // cpal 0.18 note: `SampleRate` and `ChannelCount` are plain type
        // aliases (`u32`/`u16`), not the newtypes 0.15 used — verified
        // against the vendored source, per CLAUDE.md law 2.
        let sample_rate = config.sample_rate;

        let underrun_samples = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&underrun_samples);

        let err_fn = |e: CpalError| {
            // The callback thread cannot do anything useful with this, and
            // must not allocate; the app notices through the underrun
            // counter and the ring going quiet.
            eprintln!("rf-audio: stream error: {e}");
        };

        let stream = match sample_format {
            SampleFormat::F32 => {
                let mut scratch = vec![0i16; 4096];
                device.build_output_stream(
                    config,
                    move |out: &mut [f32], _| {
                        let frames = out.len() / channels as usize;
                        let want = frames.min(scratch.len());
                        let got = ring::pop_samples(&mut consumer, &mut scratch[..want]);
                        counter.fetch_add((want - got) as u64, Ordering::Relaxed);
                        for (frame, sample) in out
                            .chunks_mut(channels as usize)
                            .zip(scratch[..want].iter().copied())
                        {
                            let value = f32::from(sample) / f32::from(i16::MAX);
                            for slot in frame {
                                *slot = value;
                            }
                        }
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::I16 => {
                let mut scratch = vec![0i16; 4096];
                device.build_output_stream(
                    config,
                    move |out: &mut [i16], _| {
                        let frames = out.len() / channels as usize;
                        let want = frames.min(scratch.len());
                        let got = ring::pop_samples(&mut consumer, &mut scratch[..want]);
                        counter.fetch_add((want - got) as u64, Ordering::Relaxed);
                        for (frame, sample) in out
                            .chunks_mut(channels as usize)
                            .zip(scratch[..want].iter().copied())
                        {
                            for slot in frame {
                                *slot = sample;
                            }
                        }
                    },
                    err_fn,
                    None,
                )
            }
            other => return Err(DeviceError::UnsupportedFormat(other)),
        }
        .map_err(|e| DeviceError::Cpal(e.to_string()))?;

        stream
            .play()
            .map_err(|e| DeviceError::Cpal(e.to_string()))?;

        Ok(Self {
            _stream: stream,
            sample_rate,
            channels,
            underrun_samples,
            device_name,
        })
    }

    /// Which device actually opened — the requested one, or the default
    /// when the request was absent or no longer present.
    #[must_use]
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// The device's real sample rate — what the resampler must target.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The device's channel count. The core's mono stream is duplicated
    /// across all of them.
    #[must_use]
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Total samples the callback had to fill with silence because the ring
    /// was empty — the soak test's pass/fail quantity.
    #[must_use]
    pub fn underrun_samples(&self) -> u64 {
        self.underrun_samples.load(Ordering::Relaxed)
    }
}
