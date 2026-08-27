//! S-DSP skeleton: BRR decoding and basic voice mixing (ticket W6-04b;
//! `docs/design/EMULATION_CORES.md` §3.4).
//!
//! ## Scope: NO LONGER A SKELETON — this doc was stale
//!
//! **The paragraph that used to be here said this module "does NOT
//! implement the Gaussian interpolation filter, ADSR/GAIN envelopes,
//! echo, pitch modulation or noise". Every one of those is implemented
//! and wired into [`Dsp::mix`], and `crates/rf-snes/src/tests/dsp.rs`
//! carries 14 tests over them.** The claim was true of W6-04b, whose
//! acceptance was "BRR decode + basic voice mixing" with sample-exactness
//! deferred; it stopped being true when the features landed and nobody
//! updated this doc.
//!
//! That mattered: W7-08's own notes repeated the absence as fact and
//! scoped its remaining work around it, which is how a stale doc turns
//! into a stale plan. What is genuinely NOT established is
//! **sample-exactness** — that the numbers these produce match hardware —
//! which is exactly what W7-08's criteria 2 and 3 (BRR sample-exactness,
//! and an audio RMS comparison against a reference SPC set) exist to
//! check. Implemented and tested is not the same as verified against the
//! machine, and this module should not claim the stronger thing.
//!
//! What "clean audio" means for this ticket: the mixer must not clip
//! wrongly, must not produce discontinuities at loop points, and must
//! produce silence when nothing is keyed on. Those are asserted.
//!
//! ## BRR
//!
//! Samples are 9-byte blocks: one header, then eight bytes holding
//! sixteen 4-bit nibbles.
//!
//! * header bits 4-7 — **range**: a left shift applied to each nibble.
//!   Range 13-15 are invalid and behave as a special case rather than
//!   shifting further, which is the detail most decoders miss.
//! * header bits 2-3 — **filter**: 0-3, an IIR predictor over the two
//!   previously decoded samples.
//! * header bit 1 — loop, bit 0 — end of sample.
//!
//! The filters are the whole reason BRR sounds better than plain 4-bit
//! ADPCM: each is a fixed-point approximation of a low-pass predictor,
//! and the coefficients are exact integers, not tuned constants.

/// A decoded BRR block: 16 samples plus the flags from its header.
#[derive(Debug, Clone, Copy)]
pub struct BrrBlock {
    pub samples: [i16; 16],
    pub loops: bool,
    pub end: bool,
}

/// Decode one 9-byte BRR block.
///
/// `prev` is the two most recently decoded samples, newest first; the
/// filters need them, and passing them in rather than keeping them in a
/// decoder struct makes a single block independently testable.
#[must_use]
pub fn decode_brr(block: &[u8; 9], prev: [i16; 2]) -> BrrBlock {
    let header = block[0];
    let range = header >> 4;
    let filter = (header >> 2) & 0x03;
    let mut samples = [0i16; 16];
    let (mut p1, mut p2) = (i32::from(prev[0]), i32::from(prev[1]));

    for i in 0..16 {
        let byte = block[1 + i / 2];
        // High nibble first.
        let nibble = if i % 2 == 0 { byte >> 4 } else { byte & 0x0F };
        // Sign-extend the 4-bit sample.
        let mut s = i32::from((nibble as i8) << 4 >> 4);

        if range <= 12 {
            s = (s << range) >> 1;
        } else {
            // Ranges 13-15 do not shift further; they collapse to the
            // sign. Decoders that keep shifting produce loud garbage on
            // the rare samples that use them.
            s = (s >> 3) << 12;
        }

        s += match filter {
            0 => 0,
            1 => p1 + ((-p1) >> 4),
            2 => (p1 << 1) + ((-((p1 << 1) + p1)) >> 5) - p2 + (p2 >> 4),
            _ => (p1 << 1) + ((-(p1 * 13)) >> 6) - p2 + ((p2 * 3) >> 4),
        };

        let clamped = s.clamp(-0x8000, 0x7FFF) as i16;
        samples[i] = clamped;
        p2 = p1;
        p1 = i32::from(clamped);
    }

    BrrBlock {
        samples,
        loops: header & 0x02 != 0,
        end: header & 0x01 != 0,
    }
}

/// The S-DSP's 4-tap Gaussian resampling kernel, quarter table.
///
/// The hardware table is 512 entries covering the full fractional range;
/// the four taps for a fraction `f` are read at `255-f`, `511-f`,
/// `256+f` and `f`. Storing a quarter and mirroring is how the hardware
/// ROM is laid out, and reproducing that layout keeps the tap selection
/// below readable as the same expression the datasheet uses.
///
/// **Not the hardware's ROM table** — see [`build_gauss`] for what it is
/// and why.
pub const GAUSS: [i16; 512] = build_gauss();

const fn build_gauss() -> [i16; 512] {
    // Built from the **cubic B-spline basis**, which is a partition of
    // unity: its four weights sum to exactly 1 at every fraction, so the
    // four taps sum to 2048 and the filter is unity-gain by
    // construction rather than by luck.
    //
    // That property is the whole point. A resampling kernel whose taps do
    // not sum to a constant changes VOLUME WITH PITCH, so a sustained
    // note swells or fades as it bends — the subtlest possible audio bug,
    // and one a spectrum plot shows long before an ear does. The first
    // version of this table used a triangular-squared window and summed
    // to 4080; `the_gaussian_kernel_is_unity_gain_at_every_fraction`
    // caught it immediately.
    //
    // The cubic B-spline is bell-shaped and low-pass, i.e. the same
    // family as the hardware's kernel, but it is NOT the S-DSP's exact
    // 512-entry ROM table — reproducing that byte-for-byte belongs with
    // the sample-exactness work (FR-CORE-036), not here. Named honestly
    // so nobody mistakes this for the hardware table.
    //
    // Layout mirrors the hardware's: the low half holds one tap and the
    // high half another, and the four taps for a fraction are read at
    // `255-f`, `511-f`, `256+f` and `f`.
    let mut t = [0i16; 512];
    let s: i64 = 256;
    let s3: i64 = s * s * s;
    let mut i: i64 = 0;
    while i < 256 {
        // Low half: B3(t) = t^3 / 6.
        t[i as usize] = ((2048 * i * i * i) / (6 * s3)) as i16;
        // High half: B2(t) = (-3t^3 + 3t^2 + 3t + 1) / 6.
        let num = -3 * i * i * i + 3 * i * i * s + 3 * i * s * s + s3;
        t[(256 + i) as usize] = ((2048 * num) / (6 * s3)) as i16;
        i += 1;
    }

    // Correct integer truncation so the sum is EXACTLY 2048.
    //
    // Fractions `f` and `255-f` read the same four indices, so there are
    // 128 independent sums, not 256 — and index `f` participates in only
    // that one pair. Adjusting it therefore fixes its pair without
    // disturbing any other, which is why the loop runs to 128 and not
    // 256. (Running it to 256 double-corrects every pair and leaves the
    // table worse than it started.)
    let mut f: i64 = 0;
    while f < 128 {
        let sum = t[(255 - f) as usize] as i64
            + t[(511 - f) as usize] as i64
            + t[(256 + f) as usize] as i64
            + t[f as usize] as i64;
        t[f as usize] = (t[f as usize] as i64 + (2048 - sum)) as i16;
        f += 1;
    }
    t
}

/// The S-DSP's global counter, and the rate tables the envelope and noise
/// generators consult through it.
///
/// **The counter is not a per-voice divider.** One counter, shared by all
/// eight envelopes and the noise LFSR, counts DOWN from `0x77FF` to zero,
/// one step per sample, and is updated at cycle 29 of the sample loop. A
/// rate `R` fires when
///
/// ```text
/// (counter + COUNTER_OFFSETS[R]) % COUNTER_RATES[R] == 0
/// ```
///
/// The offsets are the part that is easy to miss and impossible to guess:
/// rates are NOT all aligned at zero, so two voices set to different rates
/// stay in a fixed relative phase, and a voice that changes rate mid-note
/// gets a first period that is short by exactly the right amount. A plain
/// "every N samples" divider gets the average rate right and that phase
/// relationship wrong.
///
/// Source: anomie's S-DSP Doc (`apudsp.txt`, `$Revision: 1212$`,
/// 2015-09-28, with updates by jwdonal), section COUNTERS. Cited the way
/// nesdev/fullsnes are cited elsewhere in this crate; transcribed from the
/// published document, not from emulator source (NFR-011).
pub const COUNTER_MAX: u16 = 0x77FF;

/// Samples per counter event, indexed by rate. Index 0 means NEVER.
pub const COUNTER_RATES: [u16; 32] = [
    0, 2048, 1536, 1280, 1024, 768, 640, 512, 384, 320, 256, 192, 160, 128, 96, 80, 64, 48, 40, 32,
    24, 20, 16, 12, 10, 8, 6, 5, 4, 3, 2, 1,
];

/// Per-rate phase offset. Index 0 is unused (rate 0 never fires).
pub const COUNTER_OFFSETS: [u16; 32] = [
    0, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040,
    536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 0, 0,
];

/// Does rate `rate` fire on this counter value?
///
/// Rate 0 is `Inf` in the source table — it never fires, which is how a
/// voice holds a level indefinitely.
#[must_use]
pub fn counter_fires(counter: u16, rate: u8) -> bool {
    let r = usize::from(rate & 0x1F);
    let period = COUNTER_RATES[r];
    if period == 0 {
        return false;
    }
    (counter.wrapping_add(COUNTER_OFFSETS[r])).is_multiple_of(period)
}

/// ADSR / GAIN envelope state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EnvelopeStage {
    #[default]
    Release,
    Attack,
    Decay,
    Sustain,
}

/// How one counter event moves the envelope.
///
/// These are the five behaviours `$x7` GAIN selects between, and ADSR
/// mode reaches them by *pretending* GAIN holds a particular byte — see
/// [`Envelope::effective_gain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Adjust {
    /// Direct Gain: the level is simply set, and the rate does not matter.
    Direct(i16),
    /// `E -= 32`
    LinearDecrease,
    /// `E -= ((E - 1) >> 8) + 1`
    ExpDecrease,
    /// `E += 32`
    LinearIncrease,
    /// `E += (E < 0x600) ? 32 : 8`
    BentIncrease,
    /// Attack with `aaaa == %1111`: `E += 1024` at rate 31.
    FastAttack,
}

/// One voice's envelope generator.
#[derive(Debug, Clone, Copy, Default)]
pub struct Envelope {
    pub stage: EnvelopeStage,
    /// 0-2047, the DSP's 11-bit envelope level.
    pub level: i16,
    /// `$x5` ADSR1 bit 7: ADSR mode rather than GAIN.
    pub adsr_enabled: bool,
    pub attack_rate: u8,
    pub decay_rate: u8,
    pub sustain_rate: u8,
    /// 0-7; Decay ends when the top 3 bits of the level reach this.
    pub sustain_level: u8,
    /// `$x7` GAIN, used when `adsr_enabled` is false.
    pub gain: u8,
    /// The last computed level BEFORE clamping.
    ///
    /// Bent Increase chooses its step from the level it is starting at,
    /// and the document is explicit that the *pre-clamp* value is what
    /// carries to the next sample (rule 4). Storing the clamped value
    /// instead makes a bend that overshoots settle one step early.
    pub(crate) pre_clamp: i16,
}

impl Envelope {
    pub const MAX: i16 = 0x7FF;

    pub fn key_on(&mut self) {
        self.stage = EnvelopeStage::Attack;
        self.level = 0;
        self.pre_clamp = 0;
    }

    pub fn key_off(&mut self) {
        self.stage = EnvelopeStage::Release;
    }

    /// The GAIN byte this envelope behaves as, and the rate it runs at.
    ///
    /// **ADSR is not a separate mechanism.** The document describes it as
    /// loading `VxGAIN` with different values depending on the stage —
    /// "VxGAIN is not actually altered, however" — and the pretend-bytes
    /// decode to exactly the rates the register table documents:
    ///
    /// | stage   | pretend GAIN | mode          | rate       |
    /// |---------|--------------|---------------|------------|
    /// | Attack  | `%110aaaa1`  | Linear Inc    | `a*2 + 1`  |
    /// | Decay   | `%1011ddd0`  | Exp Decrease  | `d*2 + 16` |
    /// | Sustain | `%101rrrrr`  | Exp Decrease  | `r`        |
    ///
    /// Implementing it as one decode rather than three special cases is
    /// not a tidiness choice — it is what makes the rates come out right
    /// without transcribing a second table that could disagree with the
    /// first.
    fn effective_gain(&self) -> (u8, Adjust) {
        let byte = if self.adsr_enabled {
            match self.stage {
                EnvelopeStage::Attack => {
                    if self.attack_rate == 0x0F {
                        return (31, Adjust::FastAttack);
                    }
                    0xC0 | (self.attack_rate << 1) | 1
                }
                EnvelopeStage::Decay => 0xB0 | (self.decay_rate << 1),
                EnvelopeStage::Sustain => 0xA0 | self.sustain_rate,
                // Release overrides all of these; handled by the caller.
                EnvelopeStage::Release => 0,
            }
        } else {
            self.gain
        };

        if byte & 0x80 == 0 {
            // Direct Gain: E = %GGGGGGG0000.
            return (0, Adjust::Direct(i16::from(byte & 0x7F) << 4));
        }
        let mode = match (byte >> 5) & 0x03 {
            0 => Adjust::LinearDecrease,
            1 => Adjust::ExpDecrease,
            2 => Adjust::LinearIncrease,
            _ => Adjust::BentIncrease,
        };
        (byte & 0x1F, mode)
    }

    /// Advance one sample, given the DSP's global counter.
    ///
    /// The order of operations is the document's, and each numbered rule
    /// below is one of its four:
    ///
    /// 1. the counter decides whether the level moves at all;
    /// 2. Decay hands over to Sustain when the top 3 bits match `lll`;
    /// 3. Attack hands over to Decay when the new value exceeds `0x7FF`
    ///    **pre-clamp — negative values trigger it too**, which is the
    ///    non-obvious half and the reason an overflowing step does not
    ///    silently wrap into a quiet note;
    /// 4. the pre-clamp value is what Bent Increase reads next sample.
    ///
    /// Source: anomie's `apudsp.txt` `$Revision: 1212$`, the `$x5`-`$x7`
    /// register entries.
    pub fn step(&mut self, counter: u16) {
        // Release overrides every setting of these registers: rate 31
        // (every sample), and E -= 8.
        if self.stage == EnvelopeStage::Release {
            if counter_fires(counter, 31) {
                self.level = (self.level - 8).max(0);
                self.pre_clamp = self.level;
            }
            return;
        }

        let (rate, adjust) = self.effective_gain();

        // Direct Gain ignores the counter entirely — "R does not matter".
        if let Adjust::Direct(level) = adjust {
            self.level = level.clamp(0, Self::MAX);
            self.pre_clamp = self.level;
            return;
        }

        if !counter_fires(counter, rate) {
            return;
        }

        let e = i32::from(self.level);
        let next = match adjust {
            Adjust::FastAttack => e + 1024,
            Adjust::LinearDecrease => e - 32,
            Adjust::ExpDecrease => e - (((e - 1) >> 8) + 1),
            Adjust::LinearIncrease => e + 32,
            Adjust::BentIncrease => {
                // The bend reads the PRE-CLAMP level from last time.
                if i32::from(self.pre_clamp) < 0x600 {
                    e + 32
                } else {
                    e + 8
                }
            }
            Adjust::Direct(_) => unreachable!("handled above"),
        };

        // Rule 4: save pre-clamp, then rule 1: store clamped.
        self.pre_clamp = next.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        self.level = next.clamp(0, i32::from(Self::MAX)) as i16;

        match self.stage {
            // Rule 3. `next > 0x7FF` OR negative — a step that overflows
            // 11 bits in either direction ends the attack.
            EnvelopeStage::Attack => {
                if next > i32::from(Self::MAX) || next < 0 {
                    self.stage = EnvelopeStage::Decay;
                }
            }
            // Rule 2. "the upper 3 bits of E equal the Sustain Level".
            EnvelopeStage::Decay => {
                if (self.level >> 8) as u8 == self.sustain_level {
                    self.stage = EnvelopeStage::Sustain;
                }
            }
            _ => {}
        }
    }

    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.stage == EnvelopeStage::Release && self.level == 0
    }
}

/// One of the eight voices.
#[derive(Debug, Clone, Copy, Default)]
pub struct Voice {
    /// `$x4` SRCN — index into the sample directory at `$5D` DIR. The
    /// register holds the INDEX; `start`/`loop_addr` below are what that
    /// index resolves to, and the resolution happens at key-on because
    /// that is when hardware reads the directory.
    pub srcn: u8,
    /// `$x9` OUTX — the voice's most recent output, latched so a program
    /// can read it back. Hardware exposes the high byte.
    pub last_output: i16,
    /// Start of the sample's BRR data in ARAM.
    pub start: u16,
    /// Where to jump on a looping block's end.
    pub loop_addr: u16,
    /// Left/right volume, signed.
    pub vol_left: i8,
    pub vol_right: i8,
    pub keyed_on: bool,
    /// `$x2`/`$x3` PITCH — 14-bit; $1000 is 1.0 (32 kHz playback).
    pub pitch: u16,
    pub envelope: Envelope,
    /// Pitch modulation computed at S3a, consumed at S3c.
    ///
    /// The two steps are two cycles apart for voices 1-7 and eight cycles
    /// apart for voice 0, so the bent value has to be carried rather than
    /// recomputed — `PMON` or the modulating voice's output can change in
    /// between.
    pitch_bent: Option<u16>,
    /// Fractional position within the current sample, 12-bit.
    pitch_counter: u16,
    /// The four most recent decoded samples, newest last — the Gaussian
    /// filter's window.
    history: [i16; 4],
    /// Current BRR block address.
    cursor: u16,
    /// Position within the current block.
    index: usize,
    block: Option<BrrBlock>,
    prev: [i16; 2],
}

impl Voice {
    /// Key on: restart from the sample's beginning.
    ///
    /// Resetting `prev` matters — a voice that kept the previous sample's
    /// filter history would start with a click, which is exactly the kind
    /// of "not clean" this ticket's acceptance rules out.
    pub fn key_on(&mut self) {
        self.keyed_on = true;
        self.cursor = self.start;
        self.index = 0;
        self.block = None;
        self.prev = [0, 0];
        self.pitch_counter = 0;
        self.history = [0; 4];
        self.envelope.key_on();
    }

    pub fn key_off(&mut self) {
        self.envelope.key_off();
    }

    /// Is the block the voice is currently playing flagged as the
    /// sample's last? Drives ENDX (`$7C`).
    fn hit_end(&self) -> bool {
        self.block.as_ref().is_some_and(|b| b.end)
    }

    /// Hard stop, used when a non-looping sample ends.
    fn silence(&mut self) {
        self.keyed_on = false;
        self.block = None;
        self.envelope.stage = EnvelopeStage::Release;
        self.envelope.level = 0;
    }

    /// Produce the next output sample: resampled at the voice's pitch
    /// through the Gaussian filter, then scaled by the envelope.
    ///
    /// `noise` replaces the BRR source entirely when the voice's noise
    /// bit is set — the DSP substitutes the shared noise generator for
    /// the sample stream rather than mixing it in.
    pub fn next_output(&mut self, aram: &[u8], noise: i16, use_noise: bool, counter: u16) -> i16 {
        if self.envelope.is_silent() && !self.keyed_on {
            return 0;
        }
        // Advance the pitch counter; every overflow of the 12-bit
        // fraction consumes one source sample.
        let step = self.pitch & 0x3FFF;
        let next = u32::from(self.pitch_counter) + u32::from(step);
        let consumed = next >> 12;
        self.pitch_counter = (next & 0x0FFF) as u16;
        for _ in 0..consumed {
            let s = if use_noise {
                noise
            } else {
                self.next_raw(aram)
            };
            self.history.rotate_left(1);
            self.history[3] = s;
        }

        let filtered = if use_noise {
            // Noise is not resampled — it is already at the output rate.
            self.history[3]
        } else {
            gaussian(&self.history, self.pitch_counter)
        };

        self.envelope.step(counter);
        ((i32::from(filtered) * i32::from(self.envelope.level)) >> 11) as i16
    }

    /// Decode the next raw BRR sample, reading through `aram`.
    fn next_raw(&mut self, aram: &[u8]) -> i16 {
        if !self.keyed_on {
            return 0;
        }
        if self.block.is_none() || self.index >= 16 {
            let mut raw = [0u8; 9];
            for (i, b) in raw.iter_mut().enumerate() {
                *b = aram[usize::from(self.cursor.wrapping_add(i as u16)) % aram.len()];
            }
            let decoded = decode_brr(&raw, self.prev);
            self.prev = [decoded.samples[15], decoded.samples[14]];
            if decoded.end {
                if decoded.loops {
                    self.cursor = self.loop_addr;
                } else {
                    self.silence();
                    return 0;
                }
            } else {
                self.cursor = self.cursor.wrapping_add(9);
            }
            self.block = Some(decoded);
            self.index = 0;
        }
        let s = self.block.expect("just decoded").samples[self.index];
        self.index += 1;
        s
    }
}

/// Apply the 4-tap Gaussian kernel to a voice's sample window.
///
/// The taps are read at four mirrored offsets into [`GAUSS`], which is
/// how the hardware ROM is indexed. The result is shifted down by 11
/// because the four coefficients sum to 2048.
#[must_use]
pub fn gaussian(history: &[i16; 4], fraction: u16) -> i16 {
    let f = usize::from(fraction >> 4) & 0xFF;
    let t0 = i32::from(GAUSS[255 - f]);
    let t1 = i32::from(GAUSS[511 - f]);
    let t2 = i32::from(GAUSS[256 + f]);
    let t3 = i32::from(GAUSS[f]);
    let sum = t0 * i32::from(history[0])
        + t1 * i32::from(history[1])
        + t2 * i32::from(history[2])
        + t3 * i32::from(history[3]);
    (sum >> 11).clamp(-0x8000, 0x7FFF) as i16
}

/// The shared noise generator: a 15-bit LFSR, as on hardware.
#[derive(Debug, Clone, Copy)]
pub struct Noise {
    state: u16,
}

impl Default for Noise {
    fn default() -> Self {
        // Seeded non-zero: an all-zero LFSR is a fixed point and would
        // produce silence forever.
        Self { state: 0x4000 }
    }
}

impl Noise {
    /// The current noise sample, without advancing the LFSR.
    #[must_use]
    pub fn current(&self) -> i16 {
        ((self.state as i16) << 1) >> 1
    }

    /// Advance only when the global counter says so.
    ///
    /// `FLG` bits 0-4 pick a rate out of the SAME table the envelopes
    /// use, so noise pitch and envelope timing share one clock — which is
    /// why a program can hear noise change pitch by writing `FLG` alone.
    pub fn step_if(&mut self, counter: u16, rate: u8) -> i16 {
        if counter_fires(counter, rate) {
            self.step()
        } else {
            self.current()
        }
    }

    pub fn step(&mut self) -> i16 {
        let bit = (self.state ^ (self.state >> 1)) & 1;
        self.state = (self.state >> 1) | (bit << 14);
        // Sign-extend the 15-bit state to a signed sample.
        ((self.state as i16) << 1) >> 1
    }
}

/// The echo unit: a ring buffer in ARAM plus an 8-tap FIR.
#[derive(Debug, Clone, Copy)]
pub struct Echo {
    /// `$6D` ESA — buffer base, in 256-byte pages.
    pub base_page: u8,
    /// `$7D` EDL — delay, in 16 ms units. 0 means a 4-sample minimum.
    pub delay: u8,
    /// `$0D` EFB — feedback, signed.
    pub feedback: i8,
    /// `$0F+x*$10` — the eight FIR coefficients, signed.
    pub fir: [i8; 8],
    /// `$2C`/`$3C` — echo volume, signed, per channel.
    pub vol_left: i8,
    pub vol_right: i8,
    /// `$6C` FLG bit 5: writing to the buffer is DISABLED when set.
    pub write_disabled: bool,
    offset: usize,
    history: [(i16, i16); 8],
    /// The ARAM byte offset this sample reads and writes.
    ///
    /// The read happens at cycle 22/23 and the write-back at 29/30, so
    /// the pointer computed at 22 has to survive until 30 — the document
    /// says as much ("Apply ESA using the previously loaded value along
    /// with the previously calculated echo offset").
    at: usize,
    /// The FIR output computed at cycles 22-25, consumed by the DAC at
    /// 26/27 and by the write-back at 29/30.
    last_fir: (i16, i16),
}

impl Default for Echo {
    fn default() -> Self {
        Self {
            base_page: 0,
            delay: 0,
            feedback: 0,
            fir: [0; 8],
            vol_left: 0,
            vol_right: 0,
            write_disabled: true,
            offset: 0,
            history: [(0, 0); 8],
            at: 0,
            last_fir: (0, 0),
        }
    }
}

impl Echo {
    /// Buffer length in bytes: `delay` x 2 KiB, with a 4-sample floor.
    #[must_use]
    pub fn buffer_len(&self) -> usize {
        if self.delay == 0 {
            // **EDL=0 is FOUR bytes — one stereo sample — not zero and not
            // four samples.** An earlier version returned 4*4 on the
            // assumption of "a 4-sample minimum"; that was written down
            // rather than checked. The buffer is `EDL << 11` bytes, and at
            // EDL=0 hardware still reserves one sample's worth so the ring
            // has somewhere to live.
            4
        } else {
            usize::from(self.delay) * 2048
        }
    }

    /// The 8-tap FIR, exactly as the hardware sequences it.
    ///
    /// **The order and the intermediate width both matter**, and doing the
    /// obvious thing instead — accumulate all eight taps in a wide
    /// register and clamp once — is measurably different:
    ///
    /// ```text
    /// S  = sum(i=0..6) (FIR[i] * x[n-7+i] >> 6)
    /// S  = S & 0xFFFF                 // WRAPAROUND, not saturation
    /// S += (FIR[7] * x[n] >> 6)
    /// S  = clamp(S, -32768, 32767)    // and THIS one saturates
    /// out = S & 0xFFFE                // echo buffer is 15-bit, left-aligned
    /// ```
    ///
    /// Three things that are easy to get wrong and were: the shift is
    /// **per tap** (`>> 6`) and not one `>> 7` over the sum; taps 0-6 CLIP
    /// by wrapping at 16 bits while tap 7 CLAMPS; and the result has bit 0
    /// cleared because the echo buffer stores 15 bits left-aligned.
    ///
    /// `FIR[0]` (`$0F`) multiplies the OLDEST sample and `FIR[7]` (`$7F`)
    /// the newest — which this implementation already had right.
    fn fir_tap(history: [i16; 8], coef: [i8; 8]) -> i16 {
        let mut acc = 0i32;
        for i in 0..7 {
            acc += (i32::from(history[i]) * i32::from(coef[i])) >> 6;
        }
        // Wraparound to 16 bits. `as i16` is the truncation hardware does;
        // clamping here instead would hide exactly the overflow this step
        // is modelling.
        acc = i32::from(acc as i16);
        acc += (i32::from(history[7]) * i32::from(coef[7])) >> 6;
        (acc.clamp(-0x8000, 0x7FFF) as i16) & !1
    }

    /// Cycles 22-25: compute the pointer, read the delayed sample, run
    /// the FIR.
    ///
    /// The document splits the echo across the sample loop rather than
    /// doing it in one place: the pointer and left sample at cycle 22
    /// (with `FFC0`), the right sample at 23 (`FFC1`/`FFC2`), then
    /// `FFC3`-`FFC5` at 24 and `FFC6`/`FFC7` at 25. Coefficients are
    /// therefore read AFTER the samples they multiply, which is why a
    /// program can rewrite `FFC7` between two samples and hear the change
    /// one sample earlier than a naive model predicts.
    pub fn read_and_filter(&mut self, aram: &[u8]) -> (i16, i16) {
        let base = usize::from(self.base_page) * 256;
        self.at = (base + self.offset) % aram.len().max(1);

        let read = |a: &[u8], i: usize| -> i16 {
            let lo = a[i % a.len()];
            let hi = a[(i + 1) % a.len()];
            (u16::from(lo) | (u16::from(hi) << 8)) as i16
        };
        let delayed = (read(aram, self.at), read(aram, self.at + 2));

        self.history.rotate_left(1);
        self.history[7] = delayed;
        self.last_fir = (
            Self::fir_tap(core::array::from_fn(|i| self.history[i].0), self.fir),
            Self::fir_tap(core::array::from_fn(|i| self.history[i].1), self.fir),
        );
        self.last_fir
    }

    /// The echo contribution to the main output, scaled by `EVOL`.
    ///
    /// Applied at cycles 26 (left) and 27 (right), where the document
    /// puts "Load and apply EVOLL/EVOLR".
    #[must_use]
    pub fn out(&self) -> (i16, i16) {
        (
            ((i32::from(self.last_fir.0) * i32::from(self.vol_left)) >> 7) as i16,
            ((i32::from(self.last_fir.1) * i32::from(self.vol_right)) >> 7) as i16,
        )
    }

    /// Cycles 29-30: mix feedback into the dry send and write it back.
    ///
    /// `channel` selects which half runs — the document writes the left
    /// channel at cycle 29 and the right at 30, each gated on its own
    /// re-read of `FLG` bit 5, so a program that flips `ECEN` between
    /// those two cycles freezes one channel and not the other.
    pub fn write_back(&mut self, aram: &mut [u8], dry: i16, channel: EchoChannel) {
        if self.write_disabled {
            return;
        }
        let fir = match channel {
            EchoChannel::Left => self.last_fir.0,
            EchoChannel::Right => self.last_fir.1,
        };
        let v = (i32::from(dry) + ((i32::from(fir) * i32::from(self.feedback)) >> 7))
            .clamp(-0x8000, 0x7FFF) as i16;
        let i = match channel {
            EchoChannel::Left => self.at,
            EchoChannel::Right => self.at + 2,
        };
        let b = v.to_le_bytes();
        let n = aram.len();
        aram[i % n] = b[0];
        aram[(i + 1) % n] = b[1];
    }

    /// Cycle 30: step the ring, wrapping when it passes the buffer end.
    pub fn advance(&mut self) {
        let len = self.buffer_len().max(4);
        // `offset` grows by 4 and is reduced modulo a positive `len`, so
        // this cannot fail to make progress (law 8).
        self.offset = (self.offset + 4) % len;
    }

    /// Process one stereo sample in one call.
    ///
    /// This is the sample-granular form, kept because it is what the DSP
    /// unit tests drive directly. [`Dsp::tick`] does NOT use it — it
    /// calls the three phases above at their documented cycles.
    pub fn process(&mut self, aram: &mut [u8], dry: (i16, i16)) -> (i16, i16) {
        self.read_and_filter(aram);
        self.write_back(aram, dry.0, EchoChannel::Left);
        self.write_back(aram, dry.1, EchoChannel::Right);
        self.advance();
        self.out()
    }
}

/// Which half of the stereo echo write-back is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoChannel {
    Left,
    Right,
}

/// One voice's step within the interleaved sample loop.
///
/// Voices 1-7 run `S3` as a single step; **voice 0 alone has it split**
/// into `S3a`/`S3b`/`S3c` at cycles 22, 25 and 30. That asymmetry is in
/// the source table, not an artefact of this transcription: voice 0's
/// processing straddles the echo and DAC work that occupies cycles 22-30,
/// so its three sub-steps are pushed apart to make room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VStep {
    S1,
    S2,
    /// The whole of S3, for voices 1-7.
    S3,
    S3a,
    S3b,
    S3c,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
}

/// The 32-cycle sample generation loop, transcribed from anomie's S-DSP
/// Doc (`apudsp.txt`, `$Revision: 1212$`, 2015-09-28, updates by jwdonal),
/// section SOUND GENERATION — "The full sample generation loop is as
/// follows".
///
/// **This table is the whole point of ticket W7-08's second half.**
/// blargg's `spc_dsp6.sfc` does not test whether the DSP produces the
/// right numbers; it tests *when* each register becomes readable, by
/// writing a value and counting how many reads survive before the DSP
/// lands on it. A sample-granular mixer updates all eight voices at once
/// and cannot produce that count at all, however correct its arithmetic.
///
/// Entries are `(voice, step)`. Cycles 26-29 carry no voice steps — they
/// are the DAC and echo write-back window, handled in [`Dsp::tick`].
///
/// PROVENANCE: transcribed from the published document. No emulator
/// source was read to produce it (NFR-011); anomie's doc is path (a) of
/// the four unblock routes recorded on the ticket, and it is the one that
/// landed.
#[rustfmt::skip]
const SCHEDULE: [&[(u8, VStep)]; 32] = [
    /*  0 */ &[(0, VStep::S5), (1, VStep::S2)],
    /*  1 */ &[(0, VStep::S6), (1, VStep::S3)],
    /*  2 */ &[(0, VStep::S7), (1, VStep::S4), (3, VStep::S1)],
    /*  3 */ &[(0, VStep::S8), (1, VStep::S5), (2, VStep::S2)],
    /*  4 */ &[(0, VStep::S9), (1, VStep::S6), (2, VStep::S3)],
    /*  5 */ &[(1, VStep::S7), (2, VStep::S4), (4, VStep::S1)],
    /*  6 */ &[(1, VStep::S8), (2, VStep::S5), (3, VStep::S2)],
    /*  7 */ &[(1, VStep::S9), (2, VStep::S6), (3, VStep::S3)],
    /*  8 */ &[(2, VStep::S7), (3, VStep::S4), (5, VStep::S1)],
    /*  9 */ &[(2, VStep::S8), (3, VStep::S5), (4, VStep::S2)],
    /* 10 */ &[(2, VStep::S9), (3, VStep::S6), (4, VStep::S3)],
    /* 11 */ &[(3, VStep::S7), (4, VStep::S4), (6, VStep::S1)],
    /* 12 */ &[(3, VStep::S8), (4, VStep::S5), (5, VStep::S2)],
    /* 13 */ &[(3, VStep::S9), (4, VStep::S6), (5, VStep::S3)],
    /* 14 */ &[(4, VStep::S7), (5, VStep::S4), (7, VStep::S1)],
    /* 15 */ &[(4, VStep::S8), (5, VStep::S5), (6, VStep::S2)],
    /* 16 */ &[(4, VStep::S9), (5, VStep::S6), (6, VStep::S3)],
    /* 17 */ &[(0, VStep::S1), (5, VStep::S7), (6, VStep::S4)],
    /* 18 */ &[(5, VStep::S8), (6, VStep::S5), (7, VStep::S2)],
    /* 19 */ &[(5, VStep::S9), (6, VStep::S6), (7, VStep::S3)],
    /* 20 */ &[(1, VStep::S1), (6, VStep::S7), (7, VStep::S4)],
    /* 21 */ &[(0, VStep::S2), (6, VStep::S8), (7, VStep::S5)],
    /* 22 */ &[(0, VStep::S3a), (6, VStep::S9), (7, VStep::S6)],
    /* 23 */ &[(7, VStep::S7)],
    /* 24 */ &[(7, VStep::S8)],
    /* 25 */ &[(0, VStep::S3b), (7, VStep::S9)],
    /* 26 */ &[],
    /* 27 */ &[],
    /* 28 */ &[],
    /* 29 */ &[],
    /* 30 */ &[(0, VStep::S3c)],
    /* 31 */ &[(0, VStep::S4), (2, VStep::S1)],
];

/// The loop is 64 cycles long, not 32.
///
/// Everything except the KON/KOFF poll repeats at T+32; those two steps
/// run every OTHER sample, and the internal KON bits are not cleared
/// until 63 cycles after they are loaded. Modelling this as a 32-cycle
/// loop gets key-on behaviour right half the time, which is worse than
/// getting it wrong consistently because it looks like a race.
pub const LOOP_CYCLES: u16 = 64;

/// The eight-voice mixer.
#[derive(Debug, Clone)]
pub struct Dsp {
    pub voices: [Voice; 8],
    pub noise: Noise,
    pub echo: Echo,
    /// `$3D` NON — per-voice noise enable.
    pub noise_enable: u8,
    /// `$2D` PMON — per-voice pitch modulation by the previous voice.
    pub pitch_mod: u8,
    /// `$4D` EON — per-voice echo send.
    pub echo_enable: u8,
    /// Master volume, signed, per channel.
    pub main_vol_left: i8,
    pub main_vol_right: i8,

    /// The raw 128-byte register file.
    ///
    /// **This is the only way an SPC700 program can reach the DSP**: `$F2`
    /// selects a register and `$F3` reads or writes it. Before this
    /// existed, `Apu::write_register` had no `$F3` arm at all and its read
    /// arm was a literal `0`, so no game and no test ROM could program a
    /// single DSP register — blargg's `spc_dsp6.sfc` stalled forever on
    /// `Echo/basics` writing into the void (ticket W7-08).
    ///
    /// Hardware reads back what was written for almost every register, so
    /// this array is authoritative for reads and the decoded fields above
    /// are the view [`Dsp::mix`] actually runs on. Keeping both is a
    /// deliberate second copy, and the direction of truth is one-way:
    /// [`Dsp::write_register`] is the ONLY writer of the decoded fields
    /// from register data, so they cannot drift apart the way two
    /// independently-maintained sources would.
    ///
    /// The exceptions — registers whose read does NOT come from here —
    /// are listed on [`Dsp::read_register`].
    pub regs: [u8; REG_COUNT],
    /// `$5D` DIR — page of the sample directory in ARAM.
    pub dir: u8,
    /// `$7C` ENDX — per-voice "the sample hit its end block" flags. Set by
    /// the mixer, cleared by ANY write to `$7C` (hardware ignores the
    /// value written).
    pub endx: u8,

    // ---- the 64-cycle sample loop (ticket W7-08) ----
    /// Position in the loop, 0-63. See [`LOOP_CYCLES`].
    cycle: u16,
    /// The global counter that clocks every envelope and the noise LFSR.
    /// Counts DOWN from [`COUNTER_MAX`]; **initialised to 0, not to the
    /// maximum**, which the source document calls out explicitly.
    counter: u16,
    /// ENDX as the DSP is building it, before S7 makes it readable.
    endx_pending: u8,
    /// Per-voice OUTX/ENVX prepared at S6/S7, readable at S8/S9.
    ///
    /// **This pair of latches is what `spc_dsp6` "Failed 03" measures.**
    /// The ROM writes `$88` to ENVX and counts how many reads return it
    /// before the DSP overwrites it; that count is precisely the
    /// prepared-to-visible distance. With a single field per register the
    /// schedule can be perfect and the count still wrong.
    outx_pending: [u8; 8],
    envx_pending: [u8; 8],
    /// This sample's accumulators, reset at the top of the loop.
    acc: (i32, i32),
    echo_send: (i32, i32),
    /// The DAC output latched at cycles 26 and 27.
    dac: (i16, i16),
    /// The previous voice's output, for pitch modulation.
    prev_out: i16,
    /// The noise sample, regenerated at cycle 30.
    noise_out: i16,
    /// KON/KOFF as written by the CPU, and as latched at cycle 30.
    kon_written: u8,
    koff_written: u8,
    kon_internal: u8,
    koff_internal: u8,
    /// `$6C` FLG bits 0-4 — the noise generator's rate.
    noise_rate: u8,
}

/// The S-DSP has 128 registers, `$00`-`$7F`.
pub const REG_COUNT: usize = 128;

impl Default for Dsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Dsp {
    #[must_use]
    pub fn new() -> Self {
        Self {
            voices: [Voice::default(); 8],
            noise: Noise::default(),
            echo: Echo::default(),
            noise_enable: 0,
            pitch_mod: 0,
            echo_enable: 0,
            main_vol_left: 0x7F,
            main_vol_right: 0x7F,
            regs: [0; REG_COUNT],
            dir: 0,
            endx: 0,
            cycle: 0,
            // "the counter is initialized to zero (not 0x77FF) on reset".
            counter: 0,
            endx_pending: 0,
            outx_pending: [0; 8],
            envx_pending: [0; 8],
            acc: (0, 0),
            echo_send: (0, 0),
            dac: (0, 0),
            prev_out: 0,
            noise_out: 0,
            kon_written: 0,
            koff_written: 0,
            kon_internal: 0,
            koff_internal: 0,
            noise_rate: 0,
        }
    }

    /// Read one DSP register (`$F3` with `$F2` selecting `addr`).
    ///
    /// Addresses are 7-bit: `$80`-`$FF` mirror `$00`-`$7F`, which is what
    /// the hardware does and what a program that leaves the high bit set
    /// depends on.
    ///
    /// **Everything reads back out of the register file**, including
    /// `$x8` ENVX and `$x9` OUTX — they are ordinary storage that the DSP
    /// overwrites on its own schedule, not computed-live values. `$7C`
    /// ENDX is the one value read from a field rather than the array,
    /// because its eight bits are per-voice flags the mixer maintains.
    ///
    /// The timing is the substance here, and it is what these reads are
    /// FOR. Each of the three becomes visible at its own cycle — ENDX at
    /// `S7`, OUTX at `S8`, ENVX at `S9` — one, two and three steps after
    /// the value was prepared. A program that writes ENVX and reads it
    /// back sees its own value until the DSP reaches that voice's `S9`,
    /// and how many reads fit in that window is exactly what blargg's
    /// `spc_dsp6` counts. See [`Dsp::tick`].
    #[must_use]
    pub fn read_register(&self, addr: u8) -> u8 {
        let a = addr & 0x7F;
        let voice = usize::from(a >> 4);
        match a & 0x0F {
            0xC if voice == 7 => self.endx,
            // **ENVX and OUTX read out of the register FILE, not from live
            // state** — they are ordinary storage that the DSP happens to
            // overwrite once per sample (see `Dsp::mix`).
            //
            // An earlier version computed them live and ignored writes, on
            // the reasoning that "returning the written value would be a
            // convincing lie". That reasoning was wrong, and blargg's
            // spc_dsp6 is what caught it: its very first check writes $88
            // to `$08` and reads it straight back. SNESdev is explicit —
            // "VxENVX is technically writable and not intended to be
            // written to. The S-DSP updates this register once per
            // sample." Between samples, a written value stands.
            _ => self.regs[usize::from(a)],
        }
    }

    /// Write one DSP register (`$F3` with `$F2` selecting `addr`).
    ///
    /// `aram` is now unused and kept only so the call sites — including
    /// `Apu`'s `$F3` arm — do not have to change shape.
    ///
    /// It used to be needed because key-on resolved the sample address
    /// here. **It no longer does, and that is a correctness fix, not a
    /// refactor:** `$x4` SRCN indexes the directory at `$5D` DIR, and
    /// hardware reads that directory during the voice's own `S3c`, one
    /// KON poll after the write. Resolving at write time used whatever
    /// DIR happened to hold at that instant, which is a stale value for
    /// any program that writes KON before DIR.
    pub fn write_register(&mut self, addr: u8, value: u8, _aram: &[u8]) {
        let a = addr & 0x7F;
        self.regs[usize::from(a)] = value;
        let voice = usize::from(a >> 4);
        let signed = value as i8;

        match a & 0x0F {
            0x0 => self.voices[voice].vol_left = signed,
            0x1 => self.voices[voice].vol_right = signed,
            // PITCH is 14 bits across two registers, so each write has to
            // preserve the other half rather than assume both arrive.
            0x2 => {
                let hi = self.voices[voice].pitch & 0x3F00;
                self.voices[voice].pitch = hi | u16::from(value);
            }
            0x3 => {
                let lo = self.voices[voice].pitch & 0x00FF;
                self.voices[voice].pitch = (u16::from(value & 0x3F) << 8) | lo;
            }
            0x4 => self.voices[voice].srcn = value,
            0x5 => {
                // ADSR1: bit 7 selects ADSR over GAIN, bits 4-6 decay,
                // bits 0-3 attack.
                let env = &mut self.voices[voice].envelope;
                env.adsr_enabled = value & 0x80 != 0;
                env.decay_rate = (value >> 4) & 0x07;
                env.attack_rate = value & 0x0F;
            }
            0x6 => {
                // ADSR2: bits 5-7 sustain LEVEL, bits 0-4 sustain RATE.
                let env = &mut self.voices[voice].envelope;
                env.sustain_level = (value >> 5) & 0x07;
                env.sustain_rate = value & 0x1F;
            }
            0x7 => self.voices[voice].envelope.gain = value,
            // ENVX and OUTX need no decode: they are storage. The write
            // already landed in `regs` above, which is what a read
            // returns until the next sample overwrites it.
            0x8 | 0x9 => {}
            // The eight FIR coefficients live one per voice row.
            0xF => self.echo.fir[voice] = signed,
            0xC => match voice {
                0 => self.main_vol_left = signed,
                1 => self.main_vol_right = signed,
                2 => self.echo.vol_left = signed,
                3 => self.echo.vol_right = signed,
                // **KON and KOFF are LATCHED, not acted on here.** The
                // DSP polls them at cycle 30 and each voice acts at its
                // own S3c "using previously loaded values", so a key-on
                // takes effect one poll later — and the poll runs every
                // OTHER sample. Keying on immediately was the old
                // behaviour and it made key-on look instantaneous, which
                // is precisely the timing these test ROMs measure.
                4 => self.kon_written = value,
                5 => self.koff_written = value,
                6 => {
                    // FLG. Bit 5 disables echo WRITES while still reading,
                    // which is how a game freezes an echo tail without
                    // clearing it. Bits 0-4 are the noise rate, read
                    // through the same counter table as the envelopes.
                    self.echo.write_disabled = value & 0x20 != 0;
                    self.noise_rate = value & 0x1F;
                }
                // ENDX: hardware clears every flag on ANY write and
                // ignores the value, so this must not store `value`.
                // Both copies clear — the visible one and the one the
                // mixer is building — or the next S7 would restore it.
                _ => {
                    self.endx = 0;
                    self.endx_pending = 0;
                }
            },
            0xD => match voice {
                0 => self.echo.feedback = signed,
                2 => self.pitch_mod = value,
                3 => self.noise_enable = value,
                4 => self.echo_enable = value,
                5 => self.dir = value,
                6 => self.echo.base_page = value,
                7 => self.echo.delay = value,
                _ => {}
            },
            _ => {}
        }
    }

    /// Run one voice's step of the interleaved loop.
    ///
    /// A directory entry is four bytes — start address then loop address,
    /// both little-endian — at `DIR * $100 + SRCN * 4`, and `S3c` resolves
    /// it. The reads are bounds-checked against ARAM rather than trusted:
    /// `dir` and `srcn` are both fully program-controlled, and `DIR = $FF`
    /// with a high SRCN addresses past the end of a 64 KiB ARAM.
    ///
    /// The arithmetic for a voice's sample happens once, at `S3c`, where
    /// the document puts "Apply the volume envelope"; the steps around it
    /// place each REGISTER-VISIBLE event at its own cycle. That split is
    /// deliberate and is stated here rather than left to be inferred: the
    /// test suite this implements measures when values become readable,
    /// not when they are computed, so the visibility latches are modelled
    /// exactly and the internal ordering of the maths is not.
    fn voice_step(&mut self, v: usize, step: VStep, aram: &mut [u8]) {
        let bit = 1u8 << v;
        match step {
            // S1: load VxSRCN.
            VStep::S1 => self.voices[v].srcn = self.regs[v * 0x10 + 0x4],

            // S2: sample pointer (resolved at key-on), VxPITCHL, VxADSR1.
            // Those three are decoded eagerly by `write_register`, so
            // there is nothing to move here.
            VStep::S2 => {}

            // S3 for voices 1-7 is a single step; voice 0's is split.
            VStep::S3 => {
                self.voice_step(v, VStep::S3a, aram);
                self.voice_step(v, VStep::S3b, aram);
                self.voice_step(v, VStep::S3c, aram);
            }

            // S3a: load VxPITCHH, apply pitch modulation.
            VStep::S3a => {
                if v > 0 && self.pitch_mod & bit != 0 {
                    let base = self.voices[v].pitch;
                    let factor = 1.0 + f64::from(self.prev_out) / f64::from(i16::MAX);
                    let bent = (f64::from(base) * factor) as i32;
                    self.voices[v].pitch_bent = Some(bent.clamp(0, 0x3FFF) as u16);
                }
            }

            // S3b: load the BRR header and the first of the two bytes to
            // decode. Both happen inside `Voice::next_raw`, which S3c
            // drives.
            VStep::S3b => {}

            // S3c: KOFF/KON on the PREVIOUSLY loaded values, then the
            // envelope and this voice's output.
            VStep::S3c => {
                if self.koff_internal & bit != 0 {
                    self.voices[v].key_off();
                }
                if self.kon_internal & bit != 0 {
                    let dir = usize::from(self.dir) * 256 + usize::from(self.voices[v].srcn) * 4;
                    let rd = |i: usize| -> u16 {
                        let n = aram.len().max(1);
                        u16::from(aram[i % n]) | (u16::from(aram[(i + 1) % n]) << 8)
                    };
                    self.voices[v].start = rd(dir);
                    self.voices[v].loop_addr = rd(dir + 2);
                    self.voices[v].key_on();
                    // "If KON, ENDX.x will be cleared in step S7."
                    self.endx_pending &= !bit;
                }

                let use_noise = self.noise_enable & bit != 0;
                let saved = self.voices[v].pitch;
                if let Some(bent) = self.voices[v].pitch_bent.take() {
                    self.voices[v].pitch = bent;
                }
                let out = self.voices[v].next_output(aram, self.noise_out, use_noise, self.counter);
                self.voices[v].pitch = saved;
                self.voices[v].last_output = out;
                // "This is the value used for modulating the next voice's
                // pitch, if applicable."
                self.prev_out = out;

                if self.voices[v].hit_end() {
                    self.endx_pending |= bit;
                }
            }

            // S4: load and apply VxVOLL.
            VStep::S4 => {
                let s = i32::from(self.voices[v].last_output);
                let sl = (s * i32::from(self.voices[v].vol_left)) >> 7;
                self.acc.0 += sl;
                if self.echo_enable & bit != 0 {
                    self.echo_send.0 += sl;
                }
            }

            // S5: load and apply VxVOLR; prepare the new ENDX.
            VStep::S5 => {
                let s = i32::from(self.voices[v].last_output);
                let sr = (s * i32::from(self.voices[v].vol_right)) >> 7;
                self.acc.1 += sr;
                if self.echo_enable & bit != 0 {
                    self.echo_send.1 += sr;
                }
                // "The new ENDX.x value is prepared, and can be
                // overwritten. Reads will not see it yet."
            }

            // S6: prepare the new VxOUTX. Not readable yet.
            VStep::S6 => self.outx_pending[v] = (self.voices[v].last_output >> 8) as u8,

            // S7: ENDX becomes readable; prepare the new VxENVX.
            VStep::S7 => {
                self.endx = self.endx_pending;
                self.envx_pending[v] = (self.voices[v].envelope.level >> 4) as u8;
            }

            // S8: OUTX becomes readable.
            VStep::S8 => self.regs[v * 0x10 + 0x9] = self.outx_pending[v],

            // S9: ENVX becomes readable.
            VStep::S9 => self.regs[v * 0x10 + 0x8] = self.envx_pending[v],
        }
    }

    /// Advance the DSP by ONE SPC cycle.
    ///
    /// **This is the engine; [`Dsp::mix`] is a wrapper that runs 32 of
    /// them.** The order below is anomie's, cycle for cycle — see
    /// [`SCHEDULE`] for the voice interleave and the provenance note.
    ///
    /// Returns the stereo sample when this cycle produced one (cycle 27,
    /// where the right channel reaches the DAC).
    pub fn tick(&mut self, aram: &mut [u8]) -> Option<(i16, i16)> {
        let c = self.cycle % 32;
        // The first half of the loop's 64 cycles; the KON/KOFF poll runs
        // only there, which is what "every other sample" means.
        let first_half = self.cycle < 32;

        if c == 31 {
            // The accumulators for the NEXT sample open here, because
            // voice 0's S4 lands on this cycle and belongs to it.
            self.acc = (0, 0);
            self.echo_send = (0, 0);
        }

        for &(v, step) in SCHEDULE[usize::from(c)] {
            self.voice_step(usize::from(v), step, aram);
        }

        let mut produced = None;
        match c {
            // "Apply ESA ... Load left channel sample from the echo
            // buffer. Load FFC0." The right sample and the remaining
            // coefficients follow at 23-25; this implementation reads all
            // of it here and the doc comment on `read_and_filter` says so.
            22 => {
                self.echo.read_and_filter(aram);
            }
            // "Load and apply MVOLL. Load and apply EVOLL. Output the
            // left sample to the DAC. Load and apply EFB."
            26 => {
                let main = (self.acc.0 * i32::from(self.main_vol_left)) >> 7;
                let echo = i32::from(self.echo.out().0);
                self.dac.0 = (main + echo).clamp(-0x8000, 0x7FFF) as i16;
            }
            // Same for the right channel, plus PMON.
            27 => {
                let main = (self.acc.1 * i32::from(self.main_vol_right)) >> 7;
                let echo = i32::from(self.echo.out().1);
                self.dac.1 = (main + echo).clamp(-0x8000, 0x7FFF) as i16;
                produced = Some(self.dac);
            }
            // "Load NON, EON, and DIR." Eagerly decoded on write.
            28 => {}
            29 => {
                // "Update global counter." It counts DOWN, wrapping at 0.
                self.counter = if self.counter == 0 {
                    COUNTER_MAX
                } else {
                    self.counter - 1
                };
                self.echo.write_back(
                    aram,
                    self.echo_send.0.clamp(-0x8000, 0x7FFF) as i16,
                    EchoChannel::Left,
                );
                if first_half {
                    // "Clear internal KON bits for any channels keyed on
                    // in the previous 2 samples."
                    self.kon_internal = 0;
                }
            }
            30 => {
                self.echo.write_back(
                    aram,
                    self.echo_send.1.clamp(-0x8000, 0x7FFF) as i16,
                    EchoChannel::Right,
                );
                self.echo.advance();
                self.noise_out = self.noise.step_if(self.counter, self.noise_rate);
                if first_half {
                    // "Load KOFF and internal KON."
                    self.koff_internal = self.koff_written;
                    self.kon_internal = self.kon_written;
                }
            }
            _ => {}
        }

        // Advances by exactly 1 on every path and wraps at a positive
        // constant, so the loop that drives it terminates (law 8).
        self.cycle = (self.cycle + 1) % LOOP_CYCLES;
        produced
    }

    /// Produce one stereo sample by running 32 cycles of the loop.
    ///
    /// Kept as the sample-granular entry point the DSP unit tests drive.
    /// It is now a WRAPPER over [`Dsp::tick`] rather than a second
    /// implementation, so the two cannot drift: there is exactly one
    /// model of what a sample is.
    pub fn mix(&mut self, aram: &mut [u8]) -> (i16, i16) {
        let mut out = self.dac;
        for _ in 0..32 {
            // Fixed trip count: terminates by construction (law 8).
            if let Some(s) = self.tick(aram) {
                out = s;
            }
        }
        out
    }

    /// Key on the voices selected by a `KON`-style bitmask.
    pub fn key_on(&mut self, mask: u8) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                v.key_on();
            }
        }
    }

    pub fn key_off(&mut self, mask: u8) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                v.key_off();
            }
        }
    }
}

impl EnvelopeStage {
    fn to_bits(self) -> u8 {
        match self {
            EnvelopeStage::Release => 0,
            EnvelopeStage::Attack => 1,
            EnvelopeStage::Decay => 2,
            EnvelopeStage::Sustain => 3,
        }
    }

    fn from_bits(v: u8) -> Result<Self, rf_core_api::StateError> {
        Ok(match v {
            0 => EnvelopeStage::Release,
            1 => EnvelopeStage::Attack,
            2 => EnvelopeStage::Decay,
            3 => EnvelopeStage::Sustain,
            other => {
                return Err(rf_core_api::StateError::Corrupt(format!(
                    "envelope stage {other} is not one of release/attack/decay/sustain"
                )))
            }
        })
    }
}

impl Dsp {
    /// Serialise the whole S-DSP (ticket W7-09).
    ///
    /// **The BRR decode cursor is state, not a cache.** `block`, `cursor`,
    /// `index`, `prev` and the four-sample `history` are where a voice is
    /// *inside* a compressed block; BRR is a delta format, so a restore
    /// that dropped `prev` would decode the next block against silence and
    /// produce a click, and one that dropped `history` would restart the
    /// Gaussian interpolator mid-note.
    ///
    /// The echo ring is saved as its own history plus the offset into it,
    /// for the same reason: it is a delay line, and its contents are
    /// audible for `delay` milliseconds after the restore.
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        for v in &self.voices {
            o.u16(v.start)?;
            o.u16(v.loop_addr)?;
            o.i8(v.vol_left)?;
            o.i8(v.vol_right)?;
            o.bool(v.keyed_on)?;
            o.u16(v.pitch)?;
            o.u8(v.envelope.stage.to_bits())?;
            o.i16(v.envelope.level)?;
            o.bool(v.envelope.adsr_enabled)?;
            o.u8(v.envelope.attack_rate)?;
            o.u8(v.envelope.decay_rate)?;
            o.u8(v.envelope.sustain_rate)?;
            o.u8(v.envelope.sustain_level)?;
            o.u8(v.envelope.gain)?;
            o.u16(v.pitch_counter)?;
            for h in v.history {
                o.i16(h)?;
            }
            o.u16(v.cursor)?;
            o.usize(v.index)?;
            match &v.block {
                None => o.bool(false)?,
                Some(b) => {
                    o.bool(true)?;
                    for s in b.samples {
                        o.i16(s)?;
                    }
                    o.bool(b.loops)?;
                    o.bool(b.end)?;
                }
            }
            for p in v.prev {
                o.i16(p)?;
            }
        }
        o.u16(self.noise.state)?;
        o.u8(self.echo.base_page)?;
        o.u8(self.echo.delay)?;
        o.i8(self.echo.feedback)?;
        for f in self.echo.fir {
            o.i8(f)?;
        }
        o.i8(self.echo.vol_left)?;
        o.i8(self.echo.vol_right)?;
        o.bool(self.echo.write_disabled)?;
        o.usize(self.echo.offset)?;
        for (l, r) in self.echo.history {
            o.i16(l)?;
            o.i16(r)?;
        }
        o.u8(self.noise_enable)?;
        o.u8(self.pitch_mod)?;
        o.u8(self.echo_enable)?;
        o.i8(self.main_vol_left)?;
        o.i8(self.main_vol_right)?;
        // **Mid-sample state, and it has to travel.** The DSP is now a
        // 64-cycle state machine, so a save taken between two cycles
        // resumes at the wrong point in the loop unless the position,
        // the global counter, the pending register latches and the
        // accumulators all come with it. Determinism is an invariant
        // here, not a nicety (ARCHITECTURE.md §3).
        o.u16(self.cycle)?;
        o.u16(self.counter)?;
        o.u8(self.endx_pending)?;
        for b in self.outx_pending {
            o.u8(b)?;
        }
        for b in self.envx_pending {
            o.u8(b)?;
        }
        o.u32(self.acc.0 as u32)?;
        o.u32(self.acc.1 as u32)?;
        o.u32(self.echo_send.0 as u32)?;
        o.u32(self.echo_send.1 as u32)?;
        o.i16(self.dac.0)?;
        o.i16(self.dac.1)?;
        o.i16(self.prev_out)?;
        o.i16(self.noise_out)?;
        o.u8(self.kon_written)?;
        o.u8(self.koff_written)?;
        o.u8(self.kon_internal)?;
        o.u8(self.koff_internal)?;
        o.u8(self.noise_rate)?;
        o.usize(self.echo.at)?;
        o.i16(self.echo.last_fir.0)?;
        o.i16(self.echo.last_fir.1)?;
        for v in &self.voices {
            o.i16(v.last_output)?;
            o.u8(v.srcn)?;
            match v.pitch_bent {
                None => o.bool(false)?,
                Some(b) => {
                    o.bool(true)?;
                    o.u16(b)?;
                }
            }
            o.i16(v.envelope.pre_clamp)?;
        }
        // The register file itself was never saved, which meant a
        // restored state read back zeros for every register a program
        // had written.
        for b in self.regs {
            o.u8(b)?;
        }
        o.u8(self.dir)?;
        o.u8(self.endx)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        for v in &mut self.voices {
            v.start = i.u16()?;
            v.loop_addr = i.u16()?;
            v.vol_left = i.i8()?;
            v.vol_right = i.i8()?;
            v.keyed_on = i.bool()?;
            v.pitch = i.u16()?;
            v.envelope.stage = EnvelopeStage::from_bits(i.u8()?)?;
            v.envelope.level = i.i16()?;
            v.envelope.adsr_enabled = i.bool()?;
            v.envelope.attack_rate = i.u8()?;
            v.envelope.decay_rate = i.u8()?;
            v.envelope.sustain_rate = i.u8()?;
            v.envelope.sustain_level = i.u8()?;
            v.envelope.gain = i.u8()?;
            v.pitch_counter = i.u16()?;
            for h in &mut v.history {
                *h = i.i16()?;
            }
            v.cursor = i.u16()?;
            v.index = i.usize()?;
            v.block = if i.bool()? {
                let mut samples = [0i16; 16];
                for s in &mut samples {
                    *s = i.i16()?;
                }
                Some(BrrBlock {
                    samples,
                    loops: i.bool()?,
                    end: i.bool()?,
                })
            } else {
                None
            };
            for p in &mut v.prev {
                *p = i.i16()?;
            }
        }
        self.noise.state = i.u16()?;
        self.echo.base_page = i.u8()?;
        self.echo.delay = i.u8()?;
        self.echo.feedback = i.i8()?;
        for f in &mut self.echo.fir {
            *f = i.i8()?;
        }
        self.echo.vol_left = i.i8()?;
        self.echo.vol_right = i.i8()?;
        self.echo.write_disabled = i.bool()?;
        self.echo.offset = i.usize()?;
        for h in &mut self.echo.history {
            *h = (i.i16()?, i.i16()?);
        }
        self.noise_enable = i.u8()?;
        self.pitch_mod = i.u8()?;
        self.echo_enable = i.u8()?;
        self.main_vol_left = i.i8()?;
        self.main_vol_right = i.i8()?;
        // Mirrors `save` exactly; see the note there on why mid-sample
        // state has to round-trip.
        self.cycle = i.u16()?;
        self.counter = i.u16()?;
        self.endx_pending = i.u8()?;
        for b in &mut self.outx_pending {
            *b = i.u8()?;
        }
        for b in &mut self.envx_pending {
            *b = i.u8()?;
        }
        self.acc = (i.u32()? as i32, i.u32()? as i32);
        self.echo_send = (i.u32()? as i32, i.u32()? as i32);
        self.dac = (i.i16()?, i.i16()?);
        self.prev_out = i.i16()?;
        self.noise_out = i.i16()?;
        self.kon_written = i.u8()?;
        self.koff_written = i.u8()?;
        self.kon_internal = i.u8()?;
        self.koff_internal = i.u8()?;
        self.noise_rate = i.u8()?;
        self.echo.at = i.usize()?;
        self.echo.last_fir = (i.i16()?, i.i16()?);
        for v in &mut self.voices {
            v.last_output = i.i16()?;
            v.srcn = i.u8()?;
            v.pitch_bent = if i.bool()? { Some(i.u16()?) } else { None };
            v.envelope.pre_clamp = i.i16()?;
        }
        for b in &mut self.regs {
            *b = i.u8()?;
        }
        self.dir = i.u8()?;
        self.endx = i.u8()?;
        Ok(())
    }
}
