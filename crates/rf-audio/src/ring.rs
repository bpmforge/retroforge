//! The lock-free SPSC ring between the emulator thread and the device
//! callback (ticket W2-05) — `rtrb`, per `docs/TECH_STACK.md`'s "Audio
//! ring" row ("Lock-free SPSC, realtime-safe; never allocate/lock in
//! callback").

use rtrb::{Consumer, Producer, RingBuffer};

/// Producer half — owned by whoever runs the emulator.
pub type RingProducer = Producer<i16>;
/// Consumer half — owned by the device callback.
pub type RingConsumer = Consumer<i16>;

/// Constructs the ring and reports its geometry.
///
/// Capacity is expressed in **samples**, not frames or milliseconds,
/// because that is the unit both the core and the device callback speak;
/// [`AudioRing::capacity_for_latency`] converts from the latency a human
/// would state.
pub struct AudioRing;

impl AudioRing {
    /// Split a new ring into its two halves.
    ///
    /// Named `split` rather than `new` because it does not construct a
    /// `Self` — [`AudioRing`] is a namespace for the geometry rules, not a
    /// value anyone holds.
    #[must_use]
    pub fn split(capacity_samples: usize) -> (RingProducer, RingConsumer) {
        RingBuffer::new(capacity_samples)
    }

    /// Capacity for a target latency, rounded up: `rate * ms / 1000`, times
    /// [`Self::CAPACITY_HEADROOM`].
    ///
    /// The headroom is not padding-for-comfort: dynamic rate control
    /// (`crate::RateController`) holds the ring at **half full**, so it
    /// needs room to swing both ways around that target. A ring sized
    /// exactly to the target latency would clip every upward excursion and
    /// the controller could only ever correct in one direction.
    #[must_use]
    pub fn capacity_for_latency(sample_rate: u32, latency_ms: u32, channels: usize) -> usize {
        let per_channel = (sample_rate as usize * latency_ms as usize).div_ceil(1000);
        per_channel * channels * Self::CAPACITY_HEADROOM
    }

    /// See [`Self::capacity_for_latency`].
    pub const CAPACITY_HEADROOM: usize = 2;
}

/// How full the ring is, as a fraction of capacity — the single input to
/// [`crate::RateController`].
///
/// `rtrb` reports free slots on the producer and readable slots on the
/// consumer; the producer's view is the one the rate controller runs on,
/// since the controller lives on the producer side (the callback must not
/// do arithmetic beyond a copy).
#[must_use]
pub fn fill_fraction(producer: &RingProducer) -> f32 {
    let capacity = producer.buffer().capacity();
    if capacity == 0 {
        return 0.0;
    }
    let used = capacity - producer.slots();
    used as f32 / capacity as f32
}

/// Push as many samples as fit, returning how many were written.
///
/// A short write is **not** an error and is deliberately not reported as
/// one: it means the consumer is behind, which dynamic rate control is
/// already correcting on the next call. Treating it as an error here would
/// push a transient into an error path that has nothing better to do than
/// drop the same samples.
pub fn push_samples(producer: &mut RingProducer, samples: &[i16]) -> usize {
    let room = producer.slots().min(samples.len());
    if room == 0 {
        return 0;
    }
    let Ok(chunk) = producer.write_chunk_uninit(room) else {
        return 0;
    };
    chunk.fill_from_iter(samples[..room].iter().copied())
}

/// Pop up to `out.len()` samples, filling the tail with silence when the
/// ring runs dry. Returns how many real samples were copied.
///
/// **Silence-fill rather than repeat-last or stall**: an underrun is
/// audible whatever you do, and silence is the only filler that cannot make
/// it worse (a repeated last sample turns a gap into a buzz at the callback
/// rate). `docs/design/FAILURE_MODES.md` FM-02's underrun cascade is about
/// not letting one underrun *cause the next*, which is what the returned
/// count is for: the caller counts them and the rate controller sees the
/// fill it produced.
pub fn pop_samples(consumer: &mut RingConsumer, out: &mut [i16]) -> usize {
    let available = consumer.slots().min(out.len());
    for slot in out.iter_mut().take(available) {
        *slot = consumer.pop().unwrap_or(0);
    }
    for slot in out.iter_mut().skip(available) {
        *slot = 0;
    }
    available
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_for_latency_rounds_up_and_leaves_room_to_swing_both_ways() {
        // 20 ms at 48 kHz mono = 960 samples, doubled for headroom.
        assert_eq!(AudioRing::capacity_for_latency(48_000, 20, 1), 1_920);
        // Stereo doubles it again.
        assert_eq!(AudioRing::capacity_for_latency(48_000, 20, 2), 3_840);
        // A rate that does not divide evenly still covers the latency.
        assert!(AudioRing::capacity_for_latency(44_100, 7, 1) >= 44_100 * 7 / 1000);
    }

    #[test]
    fn fill_fraction_tracks_pushes_and_pops() {
        let (mut tx, mut rx) = AudioRing::split(100);
        assert_eq!(fill_fraction(&tx), 0.0);

        assert_eq!(push_samples(&mut tx, &[1; 50]), 50);
        assert!((fill_fraction(&tx) - 0.5).abs() < 0.01);

        let mut out = [0i16; 25];
        assert_eq!(pop_samples(&mut rx, &mut out), 25);
        assert_eq!(out, [1i16; 25]);
        assert!((fill_fraction(&tx) - 0.25).abs() < 0.01);
    }

    #[test]
    fn a_full_ring_takes_what_fits_and_reports_it_rather_than_failing() {
        let (mut tx, _rx) = AudioRing::split(10);
        assert_eq!(push_samples(&mut tx, &[7; 25]), 10, "writes what fits");
        assert_eq!(push_samples(&mut tx, &[7; 5]), 0, "and then nothing");
    }

    /// The underrun contract: the caller's buffer is always fully written,
    /// the tail is silence (never a repeat), and the count says how much
    /// was real.
    #[test]
    fn an_underrun_fills_the_tail_with_silence_and_reports_the_shortfall() {
        let (mut tx, mut rx) = AudioRing::split(100);
        push_samples(&mut tx, &[i16::MAX; 4]);

        let mut out = [-1i16; 10];
        assert_eq!(pop_samples(&mut rx, &mut out), 4);
        assert_eq!(&out[..4], &[i16::MAX; 4]);
        assert_eq!(
            &out[4..],
            &[0i16; 6],
            "the tail is silence, not the last sample"
        );
    }
}
