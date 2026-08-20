//! S-DSP skeleton: BRR decoding and basic voice mixing (ticket W6-04b;
//! `docs/design/EMULATION_CORES.md` §3.4).
//!
//! ## Scope: a skeleton, and it says so
//!
//! The acceptance is "BRR decode + basic voice mixing, clean audio", with
//! **sample-exactness deferred to W7**. So this decodes BRR correctly and
//! mixes eight voices at a fixed rate, and does NOT implement the
//! Gaussian interpolation filter, ADSR/GAIN envelopes, echo, pitch
//! modulation or noise. Those are named here so their absence is a
//! recorded decision rather than something to discover later.
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

/// ADSR / GAIN envelope state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EnvelopeStage {
    #[default]
    Release,
    Attack,
    Decay,
    Sustain,
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
    /// 0-7; the level decay settles at `(sustain_level + 1) / 8` of full.
    pub sustain_level: u8,
    /// `$x7` GAIN, used when `adsr_enabled` is false.
    pub gain: u8,
}

impl Envelope {
    pub const MAX: i16 = 0x7FF;

    pub fn key_on(&mut self) {
        self.stage = EnvelopeStage::Attack;
        self.level = 0;
    }

    pub fn key_off(&mut self) {
        self.stage = EnvelopeStage::Release;
    }

    /// Advance one sample.
    ///
    /// Rates are modelled as a proportional step rather than the
    /// hardware's exact rate table: this ticket's acceptance is audio
    /// quality and shape, and the exact per-rate step counts belong with
    /// the sample-exactness work. The SHAPE is right — attack is linear,
    /// decay and release are exponential, and sustain holds at the level
    /// `sustain_level` selects — which is what makes a note sound like a
    /// note.
    pub fn step(&mut self) {
        if !self.adsr_enabled {
            // GAIN mode: the simple direct-set form.
            self.level = (i16::from(self.gain) << 4).min(Self::MAX);
            return;
        }
        let sustain = (i32::from(self.sustain_level) + 1) * i32::from(Self::MAX) / 8;
        match self.stage {
            EnvelopeStage::Attack => {
                let step = i16::from(self.attack_rate).max(1) * 4;
                self.level = (self.level + step).min(Self::MAX);
                if self.level >= Self::MAX {
                    self.stage = EnvelopeStage::Decay;
                }
            }
            EnvelopeStage::Decay => {
                let step = (i32::from(self.level) * i32::from(self.decay_rate.max(1)) / 256).max(1);
                self.level = (i32::from(self.level) - step).max(sustain) as i16;
                if i32::from(self.level) <= sustain {
                    self.stage = EnvelopeStage::Sustain;
                }
            }
            EnvelopeStage::Sustain => {
                let step = i32::from(self.level) * i32::from(self.sustain_rate) / 4096;
                self.level = (i32::from(self.level) - step).max(0) as i16;
            }
            EnvelopeStage::Release => {
                // Release is a fixed linear ramp to silence on hardware.
                self.level = (self.level - 8).max(0);
            }
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
    pub fn next_output(&mut self, aram: &[u8], noise: i16, use_noise: bool) -> i16 {
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

        self.envelope.step();
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
        }
    }
}

impl Echo {
    /// Buffer length in bytes: `delay` x 2 KiB, with a 4-sample floor.
    #[must_use]
    pub fn buffer_len(&self) -> usize {
        if self.delay == 0 {
            4 * 4
        } else {
            usize::from(self.delay) * 2048
        }
    }

    /// Process one stereo sample, returning the echo contribution.
    ///
    /// Reads the delayed sample from ARAM, runs it through the FIR, mixes
    /// the dry input with feedback, and writes back — unless `$6C` bit 5
    /// disables writing, which games use to freeze an echo tail without
    /// clearing it.
    pub fn process(&mut self, aram: &mut [u8], dry: (i16, i16)) -> (i16, i16) {
        let len = self.buffer_len().max(4);
        let base = usize::from(self.base_page) * 256;
        let at = (base + self.offset) % aram.len().max(1);

        let read = |a: &[u8], i: usize| -> i16 {
            let lo = a[i % a.len()];
            let hi = a[(i + 1) % a.len()];
            (u16::from(lo) | (u16::from(hi) << 8)) as i16
        };
        let delayed = (read(aram, at), read(aram, at + 2));

        self.history.rotate_left(1);
        self.history[7] = delayed;
        let mut fir = (0i32, 0i32);
        for (i, tap) in self.fir.iter().enumerate() {
            fir.0 += i32::from(self.history[i].0) * i32::from(*tap);
            fir.1 += i32::from(self.history[i].1) * i32::from(*tap);
        }
        let fir = (
            (fir.0 >> 7).clamp(-0x8000, 0x7FFF) as i16,
            (fir.1 >> 7).clamp(-0x8000, 0x7FFF) as i16,
        );

        if !self.write_disabled {
            let fb = |d: i16, f: i16| -> i16 {
                (i32::from(d) + ((i32::from(f) * i32::from(self.feedback)) >> 7))
                    .clamp(-0x8000, 0x7FFF) as i16
            };
            let w = (fb(dry.0, fir.0), fb(dry.1, fir.1));
            let mut put = |i: usize, v: i16| {
                let b = v.to_le_bytes();
                let n = aram.len();
                aram[i % n] = b[0];
                aram[(i + 1) % n] = b[1];
            };
            put(at, w.0);
            put(at + 2, w.1);
        }

        self.offset = (self.offset + 4) % len;
        (
            ((i32::from(fir.0) * i32::from(self.vol_left)) >> 7) as i16,
            ((i32::from(fir.1) * i32::from(self.vol_right)) >> 7) as i16,
        )
    }
}

/// The eight-voice mixer.
#[derive(Debug, Clone, Default)]
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
        }
    }

    /// Mix one stereo frame.
    ///
    /// Accumulates in `i32` and clamps once at the end. Clamping per
    /// voice instead would distort a mix that is only transiently over
    /// full scale — audible, and wrong.
    ///
    /// Needs `&mut` ARAM because the echo unit writes its ring buffer
    /// back into it, which is genuinely what the hardware does: echo
    /// memory is ARAM, not a private buffer, and a game that miscomputes
    /// `ESA`/`EDL` really can have its echo overwrite its samples.
    pub fn mix(&mut self, aram: &mut [u8]) -> (i16, i16) {
        let noise = self.noise.step();
        let (mut l, mut r) = (0i32, 0i32);
        let (mut echo_l, mut echo_r) = (0i32, 0i32);
        let mut previous = 0i16;

        for (i, v) in self.voices.iter_mut().enumerate() {
            // Pitch modulation: a voice's pitch is bent by the PREVIOUS
            // voice's output. Voice 0 has no predecessor, so PMON bit 0
            // does nothing on hardware — and must do nothing here.
            let base_pitch = v.pitch;
            if i > 0 && self.pitch_mod & (1 << i) != 0 {
                let factor = 1.0 + f64::from(previous) / f64::from(i16::MAX);
                let bent = (f64::from(base_pitch) * factor) as i32;
                v.pitch = bent.clamp(0, 0x3FFF) as u16;
            }

            let s = v.next_output(aram, noise, self.noise_enable & (1 << i) != 0);
            v.pitch = base_pitch;
            previous = s;

            let sl = (i32::from(s) * i32::from(v.vol_left)) >> 7;
            let sr = (i32::from(s) * i32::from(v.vol_right)) >> 7;
            l += sl;
            r += sr;
            if self.echo_enable & (1 << i) != 0 {
                echo_l += sl;
                echo_r += sr;
            }
        }

        let (el, er) = self.echo.process(
            aram,
            (
                echo_l.clamp(-0x8000, 0x7FFF) as i16,
                echo_r.clamp(-0x8000, 0x7FFF) as i16,
            ),
        );
        l += i32::from(el);
        r += i32::from(er);

        l = (l * i32::from(self.main_vol_left)) >> 7;
        r = (r * i32::from(self.main_vol_right)) >> 7;
        (
            l.clamp(-0x8000, 0x7FFF) as i16,
            r.clamp(-0x8000, 0x7FFF) as i16,
        )
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
