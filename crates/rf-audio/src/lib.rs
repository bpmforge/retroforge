//! Low-latency audio output: lock-free ring, dynamic rate control, and the
//! device stream (ticket W2-05, FR-FE-006).
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.
//!
//! ## The shape of the problem
//!
//! Two clocks that cannot be made equal: the emulated console produces
//! samples at its own rate (`rf_nes::apu::OUTPUT_SAMPLE_RATE`, 48 kHz of
//! decimated 1.789 MHz mixer output), and the audio device consumes them at
//! whatever its crystal actually runs at — nominally the same number,
//! never exactly. Left alone, the difference accumulates until the ring
//! either starves (a click, every time) or overflows (dropped samples,
//! growing latency).
//!
//! The fix, per `docs/TECH_STACK.md`'s resampler row and the libretro
//! reference it cites: **measure the ring's fill and bend the resample
//! ratio to hold it at target**. [`RateController`] is that loop, bounded
//! to ±0.5% (FR-FE-006) — small enough to be inaudible, big enough to
//! absorb any real crystal mismatch, which is measured in parts per
//! million.
//!
//! ## Realtime discipline (TECH_STACK §"Frame path discipline")
//!
//! "no allocation in the cpal callback; no locks". [`AudioRing`] is an
//! `rtrb` SPSC queue: the emulator thread pushes, the device callback pops,
//! neither allocates and neither blocks. The callback's only other work is
//! a copy and a counter. Everything expensive — resampling, ratio
//! adjustment — happens on the producer side.

mod filter;
mod rate;
mod resample;
mod ring;

#[cfg(feature = "device")]
mod device;

#[cfg(feature = "device")]
pub use device::{AudioDevice, DeviceError};
pub use filter::Filters;
pub use rate::{RateController, RateStats, MAX_RATIO_DEVIATION, TARGET_FILL};
pub use resample::{ResampleError, Resampler};
pub use ring::{fill_fraction, pop_samples, push_samples, AudioRing, RingConsumer, RingProducer};

/// The rate loop's authority, as the resampler's constructor wants it.
/// A function rather than a re-exported constant so the two crates cannot
/// drift apart silently — see `resample::Resampler::new`'s note on
/// `rubato`'s reciprocal-symmetric band.
#[must_use]
pub fn rate_deviation() -> f32 {
    MAX_RATIO_DEVIATION
}

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-audio";

/// The internal sample rate this crate expects from a core, matching
/// `rf_nes::apu::OUTPUT_SAMPLE_RATE`. Declared here rather than imported so
/// `rf-audio` stays independent of any specific core (ARCHITECTURE §6:
/// upper layers talk to cores through `rf-core-api`, never by depending on
/// one) — `crate::tests::core_and_host_rates_agree` pins the two together
/// from the app shell, which may see both.
pub const CORE_SAMPLE_RATE: u32 = 48_000;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-audio");
    }
}
