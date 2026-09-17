//! DSP-1 HLE (ticket W14-19; D-010, SRS FR-CORE-038).
//!
//! **Clean-room, command-level HLE.** Per D-010 this reproduces the
//! DSP-1's *documented* command outputs, not the uPD7725's program ROM
//! (copyrighted firmware, never shipped in this tree — law 5). Every
//! formula below is transcribed from public hardware documentation, cited
//! per command, and computed with ordinary floating point / fixed-point
//! arithmetic this project derived itself — never a table or routine
//! lifted from an existing emulator.
//!
//! Sources, cited by short name in the commands below:
//! - "Manual" = SNES Development Manual Book II, Book 3 "Use of DSP1"
//!   (chapter 4 register protocol, chapter 5 commands).
//! - "snesdev" = snes.nesdev.org/wiki/DSP-1 (data-type table, command
//!   table with input/output order and equations).
//! - "fullsnes" = problemkaputt.de/fullsnes.htm, "DSP1 Commands".
//!
//! ## Register protocol (Manual §4.1-4.3; fullsnes)
//!
//! The chip exposes two 8-bit-wide ports, DR (data, RW) and SR (status,
//! R). The Manual states data crosses the bus "in 16 bits regardless of
//! the number of bits in each parameter" (Table 3-3-1 note 1) and that DR
//! starts in an 8-bit mode for the command byte, then switches to 16-bit
//! mode "once the command is received" (§4.1) — i.e. the first DR write
//! after idle is a bare 8-bit opcode, and every parameter/result after it
//! is a 16-bit word split across two DR accesses.
//!
//! **Byte order — NOT recoverable from the OCR.** The Manual's own
//! figures (3-4-1 "Memory Mapping", 3-4-3 "Operations Flow", 3-4-4
//! "Operational Timing") are diagrams that did not survive OCR as text,
//! and neither snesdev's nor fullsnes's prose states which half of a
//! 16-bit word crosses DR first. This HLE picks **low byte first, high
//! byte second**, for both parameter writes and result reads, matching
//! the convention every other 16-bit memory-mapped register in this core
//! uses (`$2116`/`$2117` VMADDL/H, `$4214`-`$4217` RDDIV/RDMPY, `$2134`-
//! `$2136` the mode-7 product) — see `crates/rf-snes/src/bus.rs`. This is
//! a documented judgment call, not a verified fact; if a title's output
//! comes out byte-swapped against a real DSP-1, this is the first place
//! to look.
//!
//! DR reads: fullsnes — "On completion of a valid command the Data
//! Register should contain the value 0x80. This is to prevent a valid
//! command from executing should a device read past the end of output."
//! This HLE returns `0x80` whenever DR is read with no output pending
//! (idle, or past the end of a command's results).
//!
//! SR reads: snesdev — bit 7 (the byte's MSB, "the upper 8 bits" the
//! Manual says are the only ones wired to the SNES side, §4.2) is RQM,
//! "Data Request (0 = DSP Busy; 1 = Ready for R/W)" (snesdev), which the
//! Manual states the same way (§4.3). **This HLE is never busy** — every
//! command completes synchronously inside the write that supplies its
//! last parameter — so RQM, and therefore the whole SR byte as the CPU
//! sees it, is always `0x80`.
//!
//! ## Fixed-point conventions (snesdev's data-type table)
//!
//! `T` (and `M`, whose documented range is identical to `T`'s despite the
//! table listing its unit as `1`, treated here as the same typo-for-2^-15
//! shape): a 16-bit two's-complement fraction, unit `2^-15`, i.e. value =
//! `raw / 32768`. `A`: a 16-bit angle, unit `2*pi/2^16`, i.e.
//! `radians = raw * 2*pi / 65536`. `I`: a plain 16-bit signed integer,
//! unit `1`. `C`: a plain 16-bit signed integer used as a power-of-two
//! exponent. `D2`/`L2`/`H2`: a 32-bit two's-complement value with unit
//! `2^-1`, transferred as two 16-bit words, low word first (this HLE's
//! byte-order convention above, extended the same way to word order,
//! since no source states one for a double either).
//!
//! Every formula below computes in the command's natural real-number
//! domain (`f64`, or `i64` for the plain-integer commands) and then
//! quantises back to the documented raw format — this is what the
//! Manual's Table 3-3-1 Note 1 licenses ("in 16 bits regardless of...
//! each parameter"): the wire format is a transport width, not the
//! domain the equation is stated in.

use crate::state::{StateIn, StateOut};
use rf_core_api::StateError;
use std::collections::VecDeque;
use std::f64::consts::PI;

/// How many 16-bit input words a command consumes before it runs
/// (output word counts are fixed by [`execute`] itself and not needed
/// here). `None` means "not implemented" — the caller counts it and
/// returns to idle without consuming any parameters (fullsnes gives no
/// guidance for an unimplemented opcode, and consuming nothing is the
/// only choice that cannot desynchronise a later, understood command).
fn input_words(cmd: u8) -> Option<usize> {
    Some(match cmd {
        // 5.1.1 16-bit multiply (snesdev: $00, T/I x2 -> T/H2; this HLE
        // reads the single documented test vector — 0x4000*0x4000=
        // 0x2000 — as a single T output, not the L2/H2 pair the "/H2"
        // notation could also mean; see `cmd_multiply`'s test).
        0x00 | 0x20 => 2,
        // 5.1.2 float inverse (snesdev: $10, M,C -> M,C).
        0x10 => 2,
        // 5.1.3 triangle / sin-cos (snesdev: $04, A,T/I -> T/I,T/I).
        0x04 => 2,
        // 5.2.1 radius / vector size (snesdev: $08, I,I,I -> L2,H2).
        0x08 => 3,
        // 5.2.2 range (snesdev: $18, T/I x4 -> T/H2, single word here —
        // same reading as multiply above).
        0x18 => 4,
        // 5.2.3 distance (snesdev: $28, I/T x3 -> I/T). fullsnes: bugged
        // on DSP-1/1A, fixed on DSP-1B — this HLE always reports version
        // 0x0101 (DSP-1B, see `cmd_version`) and always computes the
        // correct `sqrt`, i.e. it never reproduces the 1/1A bug.
        0x28 => 3,
        // 5.3.1 2D rotate (snesdev: $0C, A,I,I -> I,I).
        0x0C => 3,
        // 5.3.2 3D polar rotate (snesdev: $1C, A,A,A,I,I,I -> I,I,I).
        0x1C => 6,
        // Chapter 5 test commands (fullsnes): memory test always passes,
        // data-ROM transfer has no ROM to read from, version identifies
        // this HLE as a DSP-1B (the revision that fixed $28h).
        0x0F | 0x1F | 0x2F => 0,
        _ => return None,
    })
}

/// Run a fully-parameterised command. `input_words(cmd)` must already
/// have returned `Some(params.len())`.
fn execute(cmd: u8, params: &[u16]) -> Vec<u16> {
    match cmd {
        0x00 | 0x20 => cmd_multiply(params),
        0x10 => cmd_inverse(params),
        0x04 => cmd_triangle(params),
        0x08 => cmd_radius(params),
        0x18 => cmd_range(params),
        0x28 => cmd_distance(params),
        0x0C => cmd_rotate(params),
        0x1C => cmd_polar(params),
        0x0F => vec![0x0000],
        0x1F => vec![0x0000],
        0x2F => vec![0x0101],
        _ => unreachable!("execute called for a command input_words() did not recognise"),
    }
}

/// `raw` (two's complement 16 bits) as a `T`/`M` value: `raw / 2^15`.
fn t_to_f64(raw: u16) -> f64 {
    f64::from(raw as i16) / 32768.0
}

/// The inverse of [`t_to_f64`], rounding to nearest and clamping to what
/// a 16-bit two's-complement register can hold (the documented range is
/// `-1.0 .. 0.999969...`; a command whose real result overflows that
/// saturates rather than wrapping, since a silent wraparound would be a
/// worse answer than a clipped one for the games that never drive a
/// command that far).
fn f64_to_t(v: f64) -> u16 {
    let raw = (v * 32768.0).round();
    raw.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16 as u16
}

/// `raw` as an `A` angle, in radians: `raw * 2*pi / 2^16` (snesdev).
fn a_to_radians(raw: u16) -> f64 {
    f64::from(raw as i16) * (2.0 * PI / 65536.0)
}

/// `raw` as a plain 16-bit signed integer value (`I`).
fn i_to_i64(raw: u16) -> i64 {
    i64::from(raw as i16)
}

/// Round to nearest and clamp into `i16`, for an `I`-typed output.
fn f64_to_i(v: f64) -> u16 {
    v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16 as u16
}

/// 5.1.1 Multiply ($00/$20). snesdev: `I1 * I2 = O1`, T/I in, T/H2 out.
///
/// Read as a Q1.15 fixed multiply per the one documented vector this
/// ticket names: `0x4000 * 0x4000 = 0x2000` (0.5 * 0.5 = 0.25, and
/// 0.25 as T is `0x2000`) — a plain 16x16 integer product would instead
/// overflow into $10000000, so the equation's "I1 * I2" has to be read in
/// the *value* domain (Manual Table 3-3-1 note 1), not the raw domain.
fn cmd_multiply(p: &[u16]) -> Vec<u16> {
    let a = t_to_f64(p[0]);
    let b = t_to_f64(p[1]);
    vec![f64_to_t(a * b)]
}

/// 5.1.2 Float inverse ($10). snesdev: `1 / I1 * 2^I2 = O1 * 2^O2`.
///
/// `M,C` is a floating-point pair: value = `M/2^15 * 2^C`. Computes the
/// reciprocal in `f64` and re-decomposes it into the same mantissa/
/// exponent shape via the `f64`'s own IEEE-754 exponent field (no
/// hand-rolled frexp loop — law 8 — since the exponent is one bitfield
/// read, not a search).
fn cmd_inverse(p: &[u16]) -> Vec<u16> {
    let m = t_to_f64(p[0]);
    let c = i_to_i64(p[1]);
    let x = m * 2f64.powi(c as i32);
    if x == 0.0 {
        // No documented behaviour for a divide by zero; saturate to the
        // largest representable magnitude rather than produce NaN/Inf,
        // which a real chip's fixed-point ALU cannot hold either.
        return vec![0x7FFF, i64_to_c(30)];
    }
    let result = 1.0 / x;
    let bits = result.abs().to_bits();
    // IEEE-754 double: unbiased exponent `e` s.t. `1.0 <= mantissa < 2.0`
    // and `|result| = mantissa * 2^e`. Shifting one more power of two
    // into the exponent gives a mantissa in `[0.5, 1.0)`, which is what a
    // Q1.15 `M` can hold with its sign bit carrying `result`'s sign.
    let e = ((bits >> 52) & 0x7FF) as i64 - 1023;
    let out_exp = e + 1;
    let out_mantissa = result / 2f64.powi(out_exp as i32);
    vec![f64_to_t(out_mantissa), i64_to_c(out_exp)]
}

fn i64_to_c(v: i64) -> u16 {
    v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16 as u16
}

/// 5.1.3 Triangle ($04). snesdev: `A, T/I -> T/I, T/I`,
/// `O2 = I2*cos(I1), O1 = I2*sin(I1)`.
///
/// Output order read as `(sin*radius, cos*radius)` — the snesdev
/// "Outputs (In Order)" column lists two `T/I` slots without naming
/// which is which, and the equation prose states `O2` (cosine) before
/// `O1` (sine) even though `O1` is conventionally the first output; this
/// HLE keeps `O1 = sin`, `O2 = cos`, i.e. reads the prose's `O1`/`O2`
/// labels literally rather than their left-to-right order. Radius is
/// read as `T` (Q1.15): the ticket's own worked example — angle 90
/// degrees, radius near-full-scale — gives sin close to `0x7FFF` and
/// cos close to `0`, which only comes out clean in Q1.15.
fn cmd_triangle(p: &[u16]) -> Vec<u16> {
    let angle = a_to_radians(p[0]);
    let radius_raw = f64::from(p[1] as i16);
    let sin_out = (radius_raw * angle.sin()).round();
    let cos_out = (radius_raw * angle.cos()).round();
    vec![
        sin_out.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16 as u16,
        cos_out.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16 as u16,
    ]
}

/// Pack a 32-bit two's-complement value as `(low, high)` 16-bit words —
/// this HLE's word order for `L/H`- and `L2/H2`-typed outputs (see the
/// module doc: no source states one, so it follows the byte-order
/// convention chosen for DR words, low half first).
fn split_d(v: i64) -> (u16, u16) {
    let raw = v as i32 as u32;
    ((raw & 0xFFFF) as u16, (raw >> 16) as u16)
}

/// 5.2.1 Radius ($08). snesdev: `I1^2 + I2^2 + I3^2 = Ox`, I,I,I -> L2,H2.
///
/// `L2/H2` carries a `D2` value, unit `2^-1` — the sum of squares is
/// already an integer, so the D2 raw value is simply that integer scaled
/// by 2 (`value = raw / 2`).
fn cmd_radius(p: &[u16]) -> Vec<u16> {
    let sum_sq: i64 = p.iter().map(|&w| i_to_i64(w).pow(2)).sum();
    let (lo, hi) = split_d(sum_sq * 2);
    vec![lo, hi]
}

/// 5.2.2 Range ($18). snesdev: `I1^2+I2^2+I3^2-I4^2 = O`, T/I x4 -> T/H2.
///
/// Read as plain `I` integers with a single 16-bit (not double-word)
/// result, saturated rather than wrapped, matching `cmd_multiply`'s
/// reading of the same "T/H2" output notation as one word.
fn cmd_range(p: &[u16]) -> Vec<u16> {
    let sum_sq: i64 = p[..3].iter().map(|&w| i_to_i64(w).pow(2)).sum();
    let range_sq = i_to_i64(p[3]).pow(2);
    vec![f64_to_i((sum_sq - range_sq) as f64)]
}

/// 5.2.3 Distance ($28). snesdev: `sqrt(I1^2+I2^2+I3^2) = O`, I/T x3 ->
/// I/T. Computed as the mathematically correct square root: fullsnes
/// records $28 as "bugged in DSP1/DSP1A (fixed in DSP1B)"; this HLE
/// reports itself as a DSP-1B (`cmd_version`) and therefore must not
/// reproduce that bug.
fn cmd_distance(p: &[u16]) -> Vec<u16> {
    let sum_sq: f64 = p.iter().map(|&w| (i_to_i64(w) as f64).powi(2)).sum();
    vec![f64_to_i(sum_sq.sqrt())]
}

/// 5.3.1 Rotate ($0C). snesdev: `A,I,I -> I,I`,
/// `(I2,I3) * [cos(I1) -sin(I1); sin(I1) cos(I1)] = O1,O2`.
fn cmd_rotate(p: &[u16]) -> Vec<u16> {
    let angle = a_to_radians(p[0]);
    let x = i_to_i64(p[1]) as f64;
    let y = i_to_i64(p[2]) as f64;
    let (s, c) = (angle.sin(), angle.cos());
    vec![f64_to_i(x * c - y * s), f64_to_i(x * s + y * c)]
}

/// 5.3.2 Polar / 3D rotate ($1C). snesdev's matrix product, read
/// left-to-right as `Ry(I3) * Rx(I2) * Rz(I1) * (I4,I5,I6)`: the first
/// listed matrix (using angle `I3`) has the `cos/sin` pair on rows 0 and
/// 2 with row 1 fixed at `(0,1,0)`, which is a rotation about Y; the
/// second (`I2`) fixes row/col 0 at `(1,0,0)`, a rotation about X; the
/// third (`I1`) fixes the bottom-right at `1`, a rotation about Z —
/// applied to the input coordinate in that matrix order.
fn cmd_polar(p: &[u16]) -> Vec<u16> {
    let (rz, rx, ry) = (a_to_radians(p[0]), a_to_radians(p[1]), a_to_radians(p[2]));
    let (mut x, mut y, mut z) = (
        i_to_i64(p[3]) as f64,
        i_to_i64(p[4]) as f64,
        i_to_i64(p[5]) as f64,
    );
    // Rz(I1): rotate about Z.
    let (s, c) = (rz.sin(), rz.cos());
    let (nx, ny) = (x * c - y * s, x * s + y * c);
    x = nx;
    y = ny;
    // Rx(I2): rotate about X.
    let (s, c) = (rx.sin(), rx.cos());
    let (ny, nz) = (y * c - z * s, y * s + z * c);
    y = ny;
    z = nz;
    // Ry(I3): rotate about Y.
    let (s, c) = (ry.sin(), ry.cos());
    let (nx, nz) = (x * c + z * s, -x * s + z * c);
    x = nx;
    z = nz;
    vec![f64_to_i(x), f64_to_i(y), f64_to_i(z)]
}

/// One DSP-1 chip instance: the DR/SR protocol state machine plus the
/// command HLE above. Owned by [`crate::bus::SnesBus`] only when the
/// cartridge reports [`rf_cart::Coprocessor::Dsp1`].
#[derive(Debug, Clone, Default)]
pub struct Dsp1 {
    /// Parameter words already received for the in-progress command, and
    /// the low byte of the next one if only half has arrived.
    collecting: Option<Collecting>,
    /// Result words not yet drained, and whether the low half of the
    /// front one has already been read.
    output: VecDeque<u16>,
    output_high_pending: Option<u8>,
    /// How many command bytes named an opcode `shape` does not
    /// recognise. Surfaced via [`Self::unknown_commands`] so a census
    /// probe can tell what a title needed that this slice does not
    /// implement yet (raster/projection/attitude commands land here).
    unknown_commands: u32,
}

#[derive(Debug, Clone)]
struct Collecting {
    cmd: u8,
    needed: usize,
    words: Vec<u16>,
    low_pending: Option<u8>,
}

impl Dsp1 {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many command bytes this instance has seen that `shape()` does
    /// not implement.
    #[must_use]
    pub fn unknown_commands(&self) -> u32 {
        self.unknown_commands
    }

    /// A byte written to the DR (command/data) register.
    ///
    /// Idle: the byte is an 8-bit command opcode (Manual §4.1). Mid-
    /// command: it is one half of the next 16-bit parameter word, low
    /// byte first (module doc: byte order is this HLE's documented
    /// choice, not a verified fact).
    pub fn write_dr(&mut self, byte: u8) {
        if let Some(c) = &mut self.collecting {
            match c.low_pending.take() {
                None => c.low_pending = Some(byte),
                Some(low) => {
                    c.words.push(u16::from(low) | (u16::from(byte) << 8));
                    if c.words.len() == c.needed {
                        let Collecting { cmd, words, .. } = self.collecting.take().unwrap();
                        self.run(cmd, &words);
                    }
                }
            }
            return;
        }
        // Idle: a write while output is still pending is treated as
        // abandoning that output and starting a fresh command — no
        // source documents what real hardware does here, but the
        // alternative (queuing two commands) has even less support and
        // this at least cannot desynchronise every write after it.
        self.output.clear();
        self.output_high_pending = None;
        match input_words(byte) {
            Some(0) => self.run(byte, &[]),
            Some(needed) => {
                self.collecting = Some(Collecting {
                    cmd: byte,
                    needed,
                    words: Vec::with_capacity(needed),
                    low_pending: None,
                });
            }
            None => self.unknown_commands += 1,
        }
    }

    fn run(&mut self, cmd: u8, params: &[u16]) {
        self.output = execute(cmd, params).into();
        self.output_high_pending = None;
    }

    /// A byte read from the DR register (side-effecting: advances past
    /// the byte returned).
    ///
    /// fullsnes: idle/past-end-of-output reads return `0x80`.
    pub fn read_dr(&mut self) -> u8 {
        if let Some(high) = self.output_high_pending.take() {
            return high;
        }
        match self.output.pop_front() {
            Some(word) => {
                self.output_high_pending = Some((word >> 8) as u8);
                (word & 0xFF) as u8
            }
            None => 0x80,
        }
    }

    /// The byte [`Self::read_dr`] would return, without consuming it.
    /// Used by `peek` (debugger/tracer/save-state paths), which must
    /// never perturb the machine it looks at.
    #[must_use]
    pub fn peek_dr(&self) -> u8 {
        if let Some(high) = self.output_high_pending {
            return high;
        }
        match self.output.front() {
            Some(word) => (word & 0xFF) as u8,
            None => 0x80,
        }
    }

    /// The status byte the CPU sees at any SR address in the window.
    /// Always `0x80`: RQM (bit 7) is "ready", and this HLE is never busy
    /// (module doc).
    #[must_use]
    pub fn read_sr(&self) -> u8 {
        0x80
    }

    pub(crate) fn save(&self, o: &mut StateOut) -> Result<(), StateError> {
        match &self.collecting {
            None => o.bool(false)?,
            Some(c) => {
                o.bool(true)?;
                o.u8(c.cmd)?;
                o.usize(c.needed)?;
                o.usize(c.words.len())?;
                for w in &c.words {
                    o.u16(*w)?;
                }
                o.opt_u8(c.low_pending)?;
            }
        }
        o.usize(self.output.len())?;
        for w in &self.output {
            o.u16(*w)?;
        }
        o.opt_u8(self.output_high_pending)?;
        o.u32(self.unknown_commands)
    }

    pub(crate) fn load(&mut self, i: &mut StateIn) -> Result<(), StateError> {
        self.collecting = if i.bool()? {
            let cmd = i.u8()?;
            let needed = i.usize()?;
            let len = i.usize()?;
            let mut words = Vec::with_capacity(len);
            for _ in 0..len {
                words.push(i.u16()?);
            }
            let low_pending = i.opt_u8()?;
            Some(Collecting {
                cmd,
                needed,
                words,
                low_pending,
            })
        } else {
            None
        };
        let out_len = i.usize()?;
        self.output = VecDeque::with_capacity(out_len);
        for _ in 0..out_len {
            self.output.push_back(i.u16()?);
        }
        self.output_high_pending = i.opt_u8()?;
        self.unknown_commands = i.u32()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Idle DR reads `0x80` (fullsnes).
    #[test]
    fn idle_dr_reads_0x80() {
        let mut d = Dsp1::new();
        assert_eq!(d.read_dr(), 0x80);
        assert_eq!(d.peek_dr(), 0x80);
    }

    /// SR's RQM bit is always set: this HLE is never busy.
    #[test]
    fn sr_always_ready() {
        let d = Dsp1::new();
        assert_eq!(d.read_sr() & 0x80, 0x80);
    }

    /// Feed a command byte then its parameter words, low byte then high,
    /// and read back the result the same way.
    fn run_command(cmd: u8, params: &[u16]) -> Vec<u16> {
        let mut d = Dsp1::new();
        d.write_dr(cmd);
        for &p in params {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        let mut out = Vec::new();
        loop {
            let lo = d.read_dr();
            let hi = d.read_dr();
            let word = u16::from(lo) | (u16::from(hi) << 8);
            if lo == 0x80 && hi == 0x80 && out.len() >= 8 {
                // Defensive bound only; every command here produces at
                // most 3 words, so this never triggers in practice (law
                // 8: this loop's exit is the `break` below on every real
                // path, not this guard).
                break;
            }
            out.push(word);
            if d.peek_dr() == 0x80 && d.output.is_empty() {
                break;
            }
        }
        out
    }

    #[test]
    fn multiply_vector() {
        // 0.5 * 0.5 = 0.25, as T: 0x4000 * 0x4000 = 0x2000.
        let out = run_command(0x00, &[0x4000, 0x4000]);
        assert_eq!(out, vec![0x2000]);
    }

    #[test]
    fn multiply_alias_0x20() {
        assert_eq!(run_command(0x20, &[0x4000, 0x4000]), vec![0x2000]);
    }

    #[test]
    fn inverse_vector() {
        // 1 / (0.5 * 2^0) = 2.0 = 0.5 * 2^2 -> M=0x4000, C=2.
        let out = run_command(0x10, &[0x4000, 0x0000]);
        assert_eq!(out, vec![0x4000, 0x0002]);
    }

    #[test]
    fn triangle_at_90_degrees() {
        // angle 90 deg = 0x4000 (65536/4); radius near-full-scale T.
        let out = run_command(0x04, &[0x4000, 0x7FFF]);
        assert_eq!(out.len(), 2);
        assert!((out[0] as i16 - 0x7FFF).abs() <= 1, "sin ~ 0x7FFF: {out:?}");
        assert!((out[1] as i16).abs() <= 1, "cos ~ 0: {out:?}");
    }

    #[test]
    fn radius_3_4_0_is_5_squared_scaled() {
        // 3-4-5 triangle: sum of squares = 25, D2 raw = 25*2 = 50.
        let out = run_command(0x08, &[3, 4, 0]);
        assert_eq!(out, vec![50, 0]);
    }

    #[test]
    fn range_subtracts_range_squared() {
        // 3^2+4^2+0^2 - 0^2 = 25.
        let out = run_command(0x18, &[3, 4, 0, 0]);
        assert_eq!(out, vec![25]);
    }

    #[test]
    fn distance_3_4_0_is_5() {
        let out = run_command(0x28, &[3, 4, 0]);
        assert_eq!(out, vec![5]);
    }

    #[test]
    fn rotate_90_degrees() {
        // (10,0) rotated 90 degrees counterclockwise -> (0,10).
        let out = run_command(0x0C, &[0x4000, 10, 0]);
        assert_eq!(out.len(), 2);
        assert!((out[0] as i16).abs() <= 1);
        assert!((out[1] as i16 - 10).abs() <= 1);
    }

    #[test]
    fn polar_rz_90_degrees_only() {
        // Rz(90 deg) applied to (10000,0,0) -> (0,10000,0); Rx/Ry at 0
        // are identities.
        let out = run_command(0x1C, &[0x4000, 0, 0, 10000, 0, 0]);
        assert_eq!(out.len(), 3);
        assert!((out[0] as i16).abs() <= 1, "x ~ 0: {out:?}");
        assert!((out[1] as i16 - 10000).abs() <= 1, "y ~ 10000: {out:?}");
        assert!((out[2] as i16).abs() <= 1, "z ~ 0: {out:?}");
    }

    #[test]
    fn memory_test_returns_zero() {
        assert_eq!(run_command(0x0F, &[]), vec![0x0000]);
    }

    #[test]
    fn data_rom_transfer_returns_zero() {
        assert_eq!(run_command(0x1F, &[]), vec![0x0000]);
    }

    #[test]
    fn version_is_dsp1b() {
        assert_eq!(run_command(0x2F, &[]), vec![0x0101]);
    }

    #[test]
    fn unknown_command_consumes_nothing_and_counts() {
        let mut d = Dsp1::new();
        d.write_dr(0xFF); // not in the published set
        assert_eq!(d.unknown_commands(), 1);
        // Back to idle immediately: the next write is a fresh command,
        // not a stray parameter byte.
        d.write_dr(0x2F);
        assert_eq!(d.read_dr(), 0x01);
        assert_eq!(d.read_dr(), 0x01);
    }

    #[test]
    fn save_load_round_trip_mid_command() {
        struct MemStream {
            buf: Vec<u8>,
            at: usize,
        }
        impl rf_core_api::StateWriter for MemStream {
            fn write_all(&mut self, bytes: &[u8]) -> Result<(), StateError> {
                self.buf.extend_from_slice(bytes);
                Ok(())
            }
        }
        impl rf_core_api::StateReader for MemStream {
            fn read_exact(&mut self, out: &mut [u8]) -> Result<(), StateError> {
                let end = self.at + out.len();
                out.copy_from_slice(&self.buf[self.at..end]);
                self.at = end;
                Ok(())
            }
        }

        let mut d = Dsp1::new();
        d.write_dr(0x00); // multiply, expects 2 words
        d.write_dr(0x00); // low byte of first param
        d.unknown_commands = 3;

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        d.save(&mut StateOut::new(&mut stream)).unwrap();
        let mut restored = Dsp1::new();
        restored.load(&mut StateIn::new(&mut stream)).unwrap();
        assert_eq!(restored.unknown_commands(), 3);
        // Finish the command on the restored instance the same way, and
        // it must produce the same result as finishing it fresh.
        restored.write_dr(0x40); // high byte of first param -> 0x4000
        restored.write_dr(0x00);
        restored.write_dr(0x40); // second param 0x4000
        assert_eq!(restored.read_dr(), 0x00);
        assert_eq!(restored.read_dr(), 0x20);
    }
}
