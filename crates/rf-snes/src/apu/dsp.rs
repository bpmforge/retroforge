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
        let fir = (
            Self::fir_tap(core::array::from_fn(|i| self.history[i].0), self.fir),
            Self::fir_tap(core::array::from_fn(|i| self.history[i].1), self.fir),
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
        }
    }

    /// Read one DSP register (`$F3` with `$F2` selecting `addr`).
    ///
    /// Addresses are 7-bit: `$80`-`$FF` mirror `$00`-`$7F`, which is what
    /// the hardware does and what a program that leaves the high bit set
    /// depends on.
    ///
    /// Almost everything reads back out of the register file. The three
    /// exceptions are the registers hardware WRITES rather than stores,
    /// and returning the last value the CPU wrote to them would be a
    /// convincing lie — a program polling ENVX for an envelope to decay
    /// would spin forever:
    ///
    /// * `$x8` ENVX — the voice's current envelope, top 7 bits.
    /// * `$x9` OUTX — the voice's last output, high byte.
    /// * `$7C` ENDX — per-voice sample-ended flags.
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
    /// `aram` is needed because **key-on resolves the sample address
    /// here**: `$x4` SRCN is only an index into the directory at `$5D`
    /// DIR, and hardware reads that directory when the voice keys on, not
    /// when SRCN is written. Resolving at write time instead would use a
    /// stale DIR for any program that sets SRCN first and DIR second —
    /// which is the ordinary order.
    pub fn write_register(&mut self, addr: u8, value: u8, aram: &[u8]) {
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
                4 => self.key_on_masked(value, aram),
                5 => self.key_off(value),
                6 => {
                    // FLG. Bit 5 disables echo WRITES while still reading,
                    // which is how a game freezes an echo tail without
                    // clearing it.
                    self.echo.write_disabled = value & 0x20 != 0;
                }
                // ENDX: hardware clears every flag on ANY write and
                // ignores the value, so this must not store `value`.
                _ => self.endx = 0,
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

    /// Key on the voices in `mask`, resolving each one's sample address
    /// through the `$5D` DIR directory first.
    ///
    /// A directory entry is four bytes — start address then loop address,
    /// both little-endian — at `DIR * $100 + SRCN * 4`. The reads are
    /// bounds-checked against ARAM rather than trusted: `dir` and `srcn`
    /// are both fully program-controlled, and `DIR = $FF` with a high
    /// SRCN addresses past the end of a 64 KiB ARAM.
    fn key_on_masked(&mut self, mask: u8, aram: &[u8]) {
        let dir = self.dir;
        for i in 0..8 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let srcn = self.voices[i].srcn;
            let entry = usize::from(dir) * 0x100 + usize::from(srcn) * 4;
            let word = |off: usize| -> u16 {
                let lo = aram.get(entry + off).copied().unwrap_or(0);
                let hi = aram.get(entry + off + 1).copied().unwrap_or(0);
                u16::from(lo) | (u16::from(hi) << 8)
            };
            self.voices[i].start = word(0);
            self.voices[i].loop_addr = word(2);
            self.voices[i].key_on();
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
        let mut endx = self.endx;
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
            v.last_output = s;
            // ENDX is sticky: hardware sets the bit when the voice reaches
            // a block with the END flag and only a write to `$7C` clears
            // it, so this ORs rather than assigns. Assigning would make a
            // looping sample's flag flicker and a program that polls it
            // miss the event entirely.
            if v.hit_end() {
                endx |= 1 << i;
            }

            let sl = (i32::from(s) * i32::from(v.vol_left)) >> 7;
            let sr = (i32::from(s) * i32::from(v.vol_right)) >> 7;
            l += sl;
            r += sr;
            if self.echo_enable & (1 << i) != 0 {
                echo_l += sl;
                echo_r += sr;
            }
        }

        self.endx = endx;
        // **The DSP updates ENVX and OUTX once per sample.** That cadence
        // is the whole behaviour: a value the CPU wrote stands until the
        // next sample lands on it, which is what makes `$08` read back as
        // written when a program writes and reads within one sample.
        for i in 0..8 {
            let v = &self.voices[i];
            self.regs[i * 0x10 + 0x8] = (v.envelope.level >> 4) as u8;
            self.regs[i * 0x10 + 0x9] = (v.last_output >> 8) as u8;
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
        o.i8(self.main_vol_right)
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
        Ok(())
    }
}
