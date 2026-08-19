//! Audio channel scopes with per-channel mute/solo (ticket W4-10b;
//! FR-DBG-001, `docs/design/DEBUGGER.md` §3's "Audio | channel scopes,
//! mute/solo per channel").
//!
//! ## Mute is a HOST-SIDE MIX, and that is the whole design
//!
//! The obvious implementation is a gate inside the APU's mixer: skip a
//! muted channel when summing. **That would be wrong here**, and not
//! subtly — `Apu::take_samples` is emulation *output*, and
//! `crates/retroforge/tests/determinism.rs` plus the `.rfreplay` format
//! hash it. A mute that reached the mixer would make a debugging aid
//! change the thing being debugged, and two sessions that muted different
//! channels would produce different replay hashes for identical play.
//!
//! So the core captures every channel unmodified
//! ([`rf_nes::apu::Apu::take_channel_samples`], opt-in and output-only),
//! and everything in this module operates on those streams *after* they
//! have left the machine. Nothing here can perturb a simulation because
//! nothing here is upstream of one.
//!
//! The observable consequence, stated so nobody "fixes" it: muting a
//! channel changes what you HEAR but not what the emulator DOES, so a
//! `.rfreplay` recorded with three channels muted still hashes
//! identically. That is the correct behaviour and it is asserted.

/// Per-channel mute and solo state.
///
/// Solo is not "mute everything else" stored eagerly — it is evaluated at
/// mix time. Storing it eagerly loses information: un-soloing would have
/// to guess which channels the user had muted *before* they soloed, and
/// would silently un-mute them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuteState<const N: usize> {
    pub muted: [bool; N],
    pub soloed: [bool; N],
}

impl<const N: usize> Default for MuteState<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> MuteState<N> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            muted: [false; N],
            soloed: [false; N],
        }
    }

    /// Whether channel `i` is audible.
    ///
    /// **Solo wins over mute, and any solo silences every un-soloed
    /// channel.** That is the convention every DAW and tracker uses, and
    /// matching it matters more than any argument from first principles:
    /// a debugger that inverted it would confuse exactly the people most
    /// likely to reach for solo.
    #[must_use]
    pub fn audible(&self, i: usize) -> bool {
        if i >= N {
            return false;
        }
        if self.soloed.iter().any(|s| *s) {
            return self.soloed[i];
        }
        !self.muted[i]
    }

    /// Whether anything at all is audible — the UI needs this to explain
    /// silence rather than let the user think audio broke.
    #[must_use]
    pub fn anything_audible(&self) -> bool {
        (0..N).any(|i| self.audible(i))
    }

    pub fn toggle_mute(&mut self, i: usize) {
        if i < N {
            self.muted[i] = !self.muted[i];
        }
    }

    pub fn toggle_solo(&mut self, i: usize) {
        if i < N {
            self.soloed[i] = !self.soloed[i];
        }
    }

    pub fn clear(&mut self) {
        self.muted = [false; N];
        self.soloed = [false; N];
    }
}

/// Sum the audible channels into one host output stream.
///
/// Saturating rather than wrapping: five channels at full scale overflow
/// `i16`, and a wrap turns a loud passage into a burst of inverted-phase
/// noise — audibly catastrophic and easy to mistake for an emulation bug.
///
/// Channels are averaged over the count of *audible* ones rather than of
/// all of them, so soloing one channel does not make it a fifth as loud
/// as it was in the mix. That is a monitoring decision, not an accuracy
/// one: this output never feeds anything the emulator reads.
#[must_use]
pub fn mix_host_side<const N: usize>(channels: &[Vec<i16>; N], mute: &MuteState<N>) -> Vec<i16> {
    let audible: Vec<usize> = (0..N).filter(|i| mute.audible(*i)).collect();
    let len = channels.iter().map(Vec::len).min().unwrap_or(0);
    if audible.is_empty() || len == 0 {
        return vec![0; len];
    }
    let divisor = i32::try_from(audible.len()).unwrap_or(1).max(1);
    (0..len)
        .map(|s| {
            let sum: i32 = audible.iter().map(|c| i32::from(channels[*c][s])).sum();
            i16::try_from((sum / divisor).clamp(i32::from(i16::MIN), i32::from(i16::MAX)))
                .unwrap_or(0)
        })
        .collect()
}

/// One channel's scope trace, reduced to `points` min/max pairs.
///
/// Min AND max per bucket, not an average: an averaged waveform of a
/// square wave at any decent zoom level is a flat line at its DC offset,
/// which is the single most misleading thing a scope can show. Keeping
/// both bounds preserves the envelope, which is what the viewer is for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeTrace {
    pub min: Vec<i16>,
    pub max: Vec<i16>,
    /// False when the channel produced nothing this window — the UI says
    /// "silent" rather than drawing a flat line that looks like a bug.
    pub active: bool,
}

/// Reduce one channel's samples to a trace of at most `points` buckets.
#[must_use]
pub fn trace(samples: &[i16], points: usize) -> ScopeTrace {
    if samples.is_empty() || points == 0 {
        return ScopeTrace::default();
    }
    let points = points.min(samples.len());
    let bucket = samples.len().div_ceil(points);
    let mut out = ScopeTrace {
        min: Vec::with_capacity(points),
        max: Vec::with_capacity(points),
        active: samples.iter().any(|s| *s != 0),
    };
    for chunk in samples.chunks(bucket) {
        out.min.push(chunk.iter().copied().min().unwrap_or(0));
        out.max.push(chunk.iter().copied().max().unwrap_or(0));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 5;

    #[test]
    fn solo_wins_over_mute_and_silences_everything_else() {
        let mut m = MuteState::<N>::new();
        assert!((0..N).all(|i| m.audible(i)), "nothing muted by default");

        m.toggle_mute(0);
        assert!(!m.audible(0));
        assert!(m.audible(1));

        // Soloing channel 1 silences everything else INCLUDING channels
        // that were not muted...
        m.toggle_solo(1);
        assert!(m.audible(1));
        assert!(
            !m.audible(2),
            "an un-soloed channel is silent while a solo is active"
        );
        assert!(!m.audible(0));

        // ...and un-soloing must RESTORE the previous mute state rather
        // than un-muting everything, which is what storing solo eagerly
        // would have lost.
        m.toggle_solo(1);
        assert!(
            !m.audible(0),
            "channel 0 was muted before the solo and still is"
        );
        assert!(m.audible(2));
    }

    #[test]
    fn muting_everything_is_reported_rather_than_looking_broken() {
        let mut m = MuteState::<N>::new();
        for i in 0..N {
            m.toggle_mute(i);
        }
        assert!(!m.anything_audible());
        m.clear();
        assert!(m.anything_audible());
    }

    /// The mix must actually drop a muted channel — and must keep the
    /// others. A mix that returned silence for everything would satisfy
    /// "muted channels are inaudible" too.
    #[test]
    fn the_host_mix_drops_muted_channels_and_keeps_the_rest() {
        let channels: [Vec<i16>; N] = [
            vec![1000; 8],
            vec![2000; 8],
            vec![0; 8],
            vec![0; 8],
            vec![0; 8],
        ];
        let mut m = MuteState::<N>::new();
        let all = mix_host_side(&channels, &m);
        assert!(
            all.iter().all(|s| *s == 600),
            "(1000+2000)/5 = 600: {all:?}"
        );

        m.toggle_mute(0);
        let without = mix_host_side(&channels, &m);
        assert!(
            without.iter().all(|s| *s == 500),
            "with channel 0 muted the divisor is 4: 2000/4 = 500, got {without:?}"
        );
        assert_ne!(all, without, "muting must change the host mix");

        m.clear();
        m.toggle_solo(1);
        let solo = mix_host_side(&channels, &m);
        assert!(
            solo.iter().all(|s| *s == 2000),
            "a soloed channel is heard at full level, not a fifth of it: {solo:?}"
        );
    }

    #[test]
    fn muting_everything_mixes_to_silence_of_the_right_length() {
        let channels: [Vec<i16>; N] = std::array::from_fn(|_| vec![1234; 6]);
        let mut m = MuteState::<N>::new();
        for i in 0..N {
            m.toggle_mute(i);
        }
        let out = mix_host_side(&channels, &m);
        assert_eq!(out.len(), 6, "silence still has to be the right duration");
        assert!(out.iter().all(|s| *s == 0));
    }

    /// A square wave must not average away. This is the assertion that
    /// makes the scope worth drawing.
    #[test]
    fn the_trace_preserves_a_square_waves_envelope() {
        let square: Vec<i16> = (0..1000)
            .map(|i| if (i / 10) % 2 == 0 { 8000 } else { -8000 })
            .collect();
        let t = trace(&square, 20);
        assert_eq!(t.min.len(), 20);
        assert_eq!(t.max.len(), 20);
        assert!(t.active);
        assert!(
            t.min.iter().all(|v| *v == -8000) && t.max.iter().all(|v| *v == 8000),
            "an averaging reduction would collapse this to a flat line at 0"
        );
    }

    #[test]
    fn a_silent_channel_is_reported_as_silent_not_drawn_flat() {
        let t = trace(&[0i16; 100], 10);
        assert!(!t.active, "silence must be distinguishable from a bug");
        assert_eq!(t.min.len(), 10);
        let empty = trace(&[], 10);
        assert!(!empty.active);
        assert!(empty.min.is_empty());
    }
}
