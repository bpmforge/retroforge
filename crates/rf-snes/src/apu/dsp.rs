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
    }

    pub fn key_off(&mut self) {
        self.keyed_on = false;
        self.block = None;
    }

    /// Produce the next sample, reading BRR data through `aram`.
    pub fn next_sample(&mut self, aram: &[u8]) -> i16 {
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
                    self.key_off();
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

/// The eight-voice mixer.
#[derive(Debug, Clone, Default)]
pub struct Dsp {
    pub voices: [Voice; 8],
    /// Master volume, signed, per channel.
    pub main_vol_left: i8,
    pub main_vol_right: i8,
}

impl Dsp {
    #[must_use]
    pub fn new() -> Self {
        Self {
            voices: [Voice::default(); 8],
            main_vol_left: 0x7F,
            main_vol_right: 0x7F,
        }
    }

    /// Mix one stereo frame.
    ///
    /// Accumulates in `i32` and clamps once at the end. Clamping per
    /// voice instead would distort a mix that is only transiently over
    /// full scale — audible, and wrong.
    pub fn mix(&mut self, aram: &[u8]) -> (i16, i16) {
        let (mut l, mut r) = (0i32, 0i32);
        for v in &mut self.voices {
            let s = i32::from(v.next_sample(aram));
            l += (s * i32::from(v.vol_left)) >> 7;
            r += (s * i32::from(v.vol_right)) >> 7;
        }
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
