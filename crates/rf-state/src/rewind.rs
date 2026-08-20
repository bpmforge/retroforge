//! Rewind: a ring of delta-compressed snapshots (ticket W8-02;
//! `docs/design/SAVE_STATES.md` §4, FR-STATE-008).
//!
//! ## The design §4 specifies, and why the delta direction matters
//!
//! §4 asks for a "ring of delta-compressed snapshots (XOR against
//! previous + zstd) every k frames". The obvious reading — store each
//! snapshot XOR'd against its predecessor and reconstruct forward from a
//! base — **does not survive ring eviction**: once the base is dropped,
//! every delta that chained off it is unreadable, and the ring would
//! have to periodically store a full snapshot to recover.
//!
//! This stores the deltas the other way round. The ring keeps the most
//! recent uncompressed state, and each entry is the XOR between one
//! snapshot and the next. Because XOR is its own inverse, rewinding walks
//! **backwards** from the live state:
//!
//! ```text
//! older = newer XOR delta
//! ```
//!
//! Evicting the oldest entry then costs nothing but depth — there is no
//! base to lose, and no periodic full snapshot to schedule. That is the
//! whole reason for the direction, and it is not obvious from §4's one
//! sentence.
//!
//! ## Why XOR before zstd
//!
//! Consecutive emulator states differ in a small fraction of their bytes,
//! so the XOR is mostly zeros — which zstd encodes almost for free. The
//! compression is doing the work; the XOR is what gives it something
//! compressible. Compressing the raw states instead would spend the same
//! CPU for a much larger ring.
//!
//! ## Off by default
//!
//! §4 is explicit: "Enabled only when the user turns it on (memory cost
//! is real)". [`RewindRing::new`] therefore takes a capacity and an
//! interval rather than defaulting to anything, and
//! [`RewindConfig::disabled`] exists so the caller's off state is a value
//! rather than an `Option` that someone will unwrap.

use std::collections::VecDeque;

use crate::error::ContainerError;

/// How the ring is configured. Rewind is **off** unless a caller asks
/// for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewindConfig {
    /// Frames between snapshots. `0` disables.
    pub interval: u64,
    /// How many snapshots to retain. `0` disables.
    pub depth: usize,
}

impl RewindConfig {
    /// The default: rewind does nothing until switched on.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            interval: 0,
            depth: 0,
        }
    }

    #[must_use]
    pub const fn enabled(interval: u64, depth: usize) -> Self {
        Self { interval, depth }
    }

    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.interval > 0 && self.depth > 0
    }

    /// How far back the ring can reach, in frames.
    #[must_use]
    pub const fn span_frames(&self) -> u64 {
        self.interval * self.depth as u64
    }
}

/// One step backwards: the compressed XOR between a snapshot and the one
/// after it.
#[derive(Debug, Clone)]
struct Step {
    /// The frame the OLDER state belongs to.
    frame: u64,
    /// `zstd(newer XOR older)`.
    delta: Vec<u8>,
}

/// A ring of delta-compressed snapshots.
#[derive(Debug, Clone)]
pub struct RewindRing {
    config: RewindConfig,
    /// The most recent snapshot, uncompressed. Every rewind walks back
    /// from here.
    live: Vec<u8>,
    live_frame: u64,
    steps: VecDeque<Step>,
    /// Total compressed bytes currently retained — measured, not
    /// estimated (criterion 3).
    compressed_bytes: usize,
}

impl RewindRing {
    #[must_use]
    pub fn new(config: RewindConfig) -> Self {
        Self {
            config,
            live: Vec::new(),
            live_frame: 0,
            steps: VecDeque::new(),
            compressed_bytes: 0,
        }
    }

    #[must_use]
    pub fn config(&self) -> RewindConfig {
        self.config
    }

    /// Snapshots currently retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// **Measured** compressed size of the ring, in bytes.
    ///
    /// §4 warns the memory cost is real ("tens of MB for NES, more for
    /// SNES"), so this is a number a UI can show and a test can assert —
    /// not an estimate.
    #[must_use]
    pub fn compressed_bytes(&self) -> usize {
        self.compressed_bytes
    }

    /// The uncompressed size one snapshot would take, for comparison.
    #[must_use]
    pub fn uncompressed_bytes(&self) -> usize {
        self.live.len() * (self.steps.len() + 1)
    }

    /// Should a snapshot be taken at this frame?
    #[must_use]
    pub fn wants_snapshot(&self, frame: u64) -> bool {
        self.config.is_enabled()
            && (self.live.is_empty() || frame >= self.live_frame + self.config.interval)
    }

    /// Record a snapshot.
    ///
    /// Cheap when disabled — it returns immediately, so a caller can
    /// invoke it unconditionally rather than branching at every call
    /// site.
    ///
    /// # Errors
    /// Returns [`ContainerError`] if compression fails.
    pub fn push(&mut self, frame: u64, state: &[u8]) -> Result<(), ContainerError> {
        if !self.config.is_enabled() {
            return Ok(());
        }
        if self.live.is_empty() {
            self.live = state.to_vec();
            self.live_frame = frame;
            return Ok(());
        }

        // XOR against the state we are replacing, then compress. A state
        // that changed length (it should not, but a core could grow a
        // chunk) falls back to storing the older state whole, so the ring
        // degrades in size rather than in correctness.
        let older = std::mem::replace(&mut self.live, state.to_vec());
        let raw = if older.len() == state.len() {
            older.iter().zip(state).map(|(a, b)| a ^ b).collect()
        } else {
            older
        };
        let delta = zstd::encode_all(raw.as_slice(), 1)
            .map_err(|e| ContainerError::Decompression(format!("rewind: compress failed: {e}")))?;

        self.compressed_bytes += delta.len();
        self.steps.push_back(Step {
            frame: self.live_frame,
            delta,
        });
        self.live_frame = frame;

        while self.steps.len() > self.config.depth {
            if let Some(dropped) = self.steps.pop_front() {
                self.compressed_bytes -= dropped.delta.len();
            }
        }
        Ok(())
    }

    /// Step back one snapshot, returning the state and its frame.
    ///
    /// Consumes the newest step: rewinding is destructive, which matches
    /// how a hold-key scrub behaves — you cannot rewind past the start
    /// and then fast-forward through history you already left.
    ///
    /// # Errors
    /// Returns [`ContainerError`] if a stored delta cannot be
    /// decompressed, which would mean the ring is corrupt.
    pub fn step_back(&mut self) -> Result<Option<(u64, Vec<u8>)>, ContainerError> {
        let Some(step) = self.steps.pop_back() else {
            return Ok(None);
        };
        self.compressed_bytes -= step.delta.len();

        let raw = zstd::decode_all(step.delta.as_slice()).map_err(|e| {
            ContainerError::Decompression(format!("rewind: decompress failed: {e}"))
        })?;

        // XOR is its own inverse, so the older state comes straight back
        // out of the live one. A length mismatch means this entry stored
        // the older state whole (see `push`).
        let older = if raw.len() == self.live.len() {
            self.live.iter().zip(&raw).map(|(a, b)| a ^ b).collect()
        } else {
            raw
        };

        self.live = older;
        self.live_frame = step.frame;
        Ok(Some((self.live_frame, self.live.clone())))
    }

    /// Rewind by approximately `frames`, returning the state landed on.
    ///
    /// Lands on the nearest retained snapshot at or before the target —
    /// §4's "rewind replays inputs from nearest snapshot", with the
    /// replay itself left to the caller, which is the only party that
    /// owns a machine to replay on.
    ///
    /// # Errors
    /// Propagates a decompression failure.
    pub fn rewind_frames(&mut self, frames: u64) -> Result<Option<(u64, Vec<u8>)>, ContainerError> {
        if !self.config.is_enabled() || self.steps.is_empty() {
            return Ok(None);
        }
        let target = self.live_frame.saturating_sub(frames);
        let mut landed = None;
        while self.live_frame > target {
            match self.step_back()? {
                Some(state) => landed = Some(state),
                None => break,
            }
        }
        Ok(landed)
    }

    /// Drop everything. Used when a state is loaded from disk, since the
    /// ring's history no longer belongs to the machine.
    pub fn clear(&mut self) {
        self.steps.clear();
        self.live.clear();
        self.live_frame = 0;
        self.compressed_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic stand-in for an emulated machine: a block of state
    /// that evolves from its own contents plus the frame's input.
    ///
    /// Deliberately state-shaped rather than a counter — the ring's whole
    /// premise is that consecutive states differ in a small fraction of
    /// their bytes, and a counter would not exercise that.
    #[derive(Clone, PartialEq, Eq, Debug)]
    struct Machine {
        mem: Vec<u8>,
        frame: u64,
    }

    impl Machine {
        fn new() -> Self {
            let mut mem = vec![0u8; 4096];
            for (i, b) in mem.iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }
            Self { mem, frame: 0 }
        }

        fn step(&mut self, input: u8) {
            // Touch a handful of bytes per frame, as a real machine does.
            for k in 0..16usize {
                let at = ((self.frame as usize * 7) + k * 97) % self.mem.len();
                self.mem[at] = self.mem[at].wrapping_add(input).wrapping_add(k as u8);
            }
            self.frame += 1;
        }

        fn save(&self) -> Vec<u8> {
            let mut out = self.frame.to_le_bytes().to_vec();
            out.extend_from_slice(&self.mem);
            out
        }

        fn load(bytes: &[u8]) -> Self {
            let mut f = [0u8; 8];
            f.copy_from_slice(&bytes[..8]);
            Self {
                frame: u64::from_le_bytes(f),
                mem: bytes[8..].to_vec(),
            }
        }
    }

    fn input_for(frame: u64) -> u8 {
        (frame % 7) as u8 + 1
    }

    /// **Rewind is off unless asked for.** §4 is explicit that the memory
    /// cost is real, so the default must do nothing.
    #[test]
    fn rewind_is_disabled_by_default_and_costs_nothing() {
        let mut ring = RewindRing::new(RewindConfig::disabled());
        assert!(!ring.config().is_enabled());
        for f in 0..100 {
            ring.push(f, &[0u8; 4096]).expect("push");
        }
        assert!(ring.is_empty());
        assert_eq!(ring.compressed_bytes(), 0);
        assert!(ring.rewind_frames(50).expect("rewind").is_none());
    }

    /// **Criterion 2.** Rewinding N frames and replaying forward must
    /// reach state identical to never having rewound at all.
    ///
    /// This is the acceptance in executable form: a rewind that lands
    /// somewhere plausible but not identical is exactly the bug worth
    /// catching, and only a byte-for-byte comparison catches it.
    #[test]
    fn rewinding_then_replaying_forward_is_identical_to_never_rewinding() {
        const INTERVAL: u64 = 10;
        let config = RewindConfig::enabled(INTERVAL, 32);

        // Reference run: straight through, no rewind.
        let mut reference = Machine::new();
        for f in 0..300u64 {
            reference.step(input_for(f));
        }

        // Rewinding run: same inputs, but rewound 100 frames partway and
        // replayed forward.
        let mut ring = RewindRing::new(config);
        let mut machine = Machine::new();
        for f in 0..300u64 {
            if ring.wants_snapshot(f) {
                ring.push(f, &machine.save()).expect("push");
            }
            machine.step(input_for(f));
        }

        let (landed_frame, state) = ring
            .rewind_frames(100)
            .expect("rewind")
            .expect("the ring must reach 100 frames back");
        let mut rewound = Machine::load(&state);
        assert_eq!(rewound.frame, landed_frame);
        assert!(landed_frame <= 200, "must land at or before the target");

        // Replay forward with the SAME inputs, from the frame landed on.
        for f in landed_frame..300 {
            rewound.step(input_for(f));
        }

        assert_eq!(
            rewound, reference,
            "replaying forward from a rewound snapshot must reproduce the \
             un-rewound run byte for byte"
        );
    }

    /// Stepping back repeatedly walks the whole ring, and each step lands
    /// on a real snapshot.
    #[test]
    fn stepping_back_walks_the_ring_one_snapshot_at_a_time() {
        let mut ring = RewindRing::new(RewindConfig::enabled(4, 8));
        let mut machine = Machine::new();
        let mut expected: Vec<(u64, Vec<u8>)> = Vec::new();
        for f in 0..64u64 {
            if ring.wants_snapshot(f) {
                let s = machine.save();
                expected.push((f, s.clone()));
                ring.push(f, &s).expect("push");
            }
            machine.step(input_for(f));
        }

        // Only the last `depth` snapshots survive; walk them backwards.
        let mut seen = 0;
        while let Some((frame, state)) = ring.step_back().expect("step") {
            let want = expected
                .iter()
                .rev()
                .find(|(f, _)| *f == frame)
                .expect("every returned frame must be one we pushed");
            assert_eq!(&state, &want.1, "frame {frame} restored incorrectly");
            seen += 1;
        }
        assert_eq!(seen, 8, "the ring retains exactly its configured depth");
    }

    /// The ring is bounded: an unbounded one is the memory bug §4 warns
    /// about.
    #[test]
    fn the_ring_never_grows_past_its_depth() {
        let mut ring = RewindRing::new(RewindConfig::enabled(1, 16));
        let mut machine = Machine::new();
        for f in 0..500u64 {
            ring.push(f, &machine.save()).expect("push");
            machine.step(input_for(f));
            assert!(ring.len() <= 16, "depth exceeded at frame {f}");
        }
        assert_eq!(ring.len(), 16);
    }

    /// **Criterion 3: the cost is MEASURED, not estimated.**
    ///
    /// The XOR-then-compress design only pays off if consecutive states
    /// really do differ in a small fraction of their bytes — so this
    /// asserts the ring is dramatically smaller than storing the
    /// snapshots whole, which is the claim the design rests on.
    #[test]
    fn the_delta_ring_is_far_smaller_than_storing_whole_snapshots() {
        let mut ring = RewindRing::new(RewindConfig::enabled(1, 64));
        let mut machine = Machine::new();
        for f in 0..200u64 {
            ring.push(f, &machine.save()).expect("push");
            machine.step(input_for(f));
        }

        let compressed = ring.compressed_bytes();
        let uncompressed = ring.uncompressed_bytes();
        assert!(compressed > 0, "the ring must actually hold something");
        assert!(
            compressed * 10 < uncompressed,
            "XOR + zstd should be an order of magnitude smaller: \
             {compressed} vs {uncompressed} bytes"
        );
        eprintln!(
            "rewind ring: {} snapshots, {compressed} bytes compressed vs \
             {uncompressed} uncompressed ({:.1}x)",
            ring.len(),
            uncompressed as f64 / compressed as f64
        );
    }

    /// A state whose length changes must not corrupt the ring — it
    /// degrades in SIZE rather than in correctness.
    #[test]
    fn a_state_that_changes_length_still_restores_exactly() {
        let mut ring = RewindRing::new(RewindConfig::enabled(1, 8));
        ring.push(0, &[1u8; 100]).expect("push");
        ring.push(1, &[2u8; 200]).expect("push");
        ring.push(2, &[3u8; 200]).expect("push");

        let (f, s) = ring.step_back().expect("step").expect("some");
        assert_eq!((f, s), (1, vec![2u8; 200]));
        let (f, s) = ring.step_back().expect("step").expect("some");
        assert_eq!(
            (f, s),
            (0, vec![1u8; 100]),
            "the shorter state comes back whole"
        );
    }

    /// Snapshots are taken on the configured interval, not every frame.
    #[test]
    fn snapshots_are_taken_on_the_interval() {
        let mut ring = RewindRing::new(RewindConfig::enabled(10, 100));
        let state = vec![0u8; 64];
        let mut taken = 0;
        for f in 0..100u64 {
            if ring.wants_snapshot(f) {
                ring.push(f, &state).expect("push");
                taken += 1;
            }
        }
        assert_eq!(taken, 10, "one snapshot per 10 frames");
        assert_eq!(RewindConfig::enabled(10, 100).span_frames(), 1000);
    }

    /// Clearing drops the history, which is what loading a state from
    /// disk must do — the ring's past no longer belongs to the machine.
    #[test]
    fn clearing_drops_the_history() {
        let mut ring = RewindRing::new(RewindConfig::enabled(1, 8));
        for f in 0..8u64 {
            ring.push(f, &[f as u8; 64]).expect("push");
        }
        assert!(!ring.is_empty());
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.compressed_bytes(), 0);
        assert!(ring.step_back().expect("step").is_none());
    }
}
