//! DSP-1 HLE (ticket W14-19 slice 1, W14-21 slice 2; D-010, SRS FR-CORE-038).
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
//! `radians = raw * 2*pi / 65536`, cyclic (this HLE wraps `A` outputs mod
//! 65536 rather than saturating, since the type is explicitly "loops").
//! `I`: a plain 16-bit signed integer, unit `1`, saturated on overflow.
//! `CI`: snesdev's "Cyclic Int" — same 16-bit range as `I` but wraps
//! instead of saturating (used for world coordinates, which are expected
//! to roll over rather than clip at a global-map edge). `U`: unsigned
//! 16-bit integer, unit `1`, no sign. `2I`: snesdev's "Double Int", unit
//! `2`, i.e. `value = raw * 2`, `raw = value / 2` — used for the FLU
//! object-coordinate commands (§5.5.2, §5.5.3); getting the factor of two
//! backwards would not be caught by a round-trip test (a global-to-
//! object-to-global round trip cancels a consistent error), so
//! `cmd_subjective`/`cmd_objective` are exercised with one-directional hand-computed
//! vectors as well as a round trip. `I8`: unit `2^-8`, i.e. a 1.7.8
//! signed fixed-point word — this is the Mode-7 matrix register's own
//! format, and is what the Raster command's A/B/C/D outputs use.
//! `D2`/`L2`/`H2`: a 32-bit two's-complement value with unit `2^-1`,
//! transferred as two 16-bit words, low word first (this HLE's byte-order
//! convention above, extended the same way to word order, since no
//! source states one for a double either).
//!
//! Every formula below computes in the command's natural real-number
//! domain (`f64`, or `i64` for the plain-integer commands) and then
//! quantises back to the documented raw format — this is what the
//! Manual's Table 3-3-1 Note 1 licenses ("in 16 bits regardless of...
//! each parameter"): the wire format is a transport width, not the
//! domain the equation is stated in.
//!
//! ## Slice 2 (W14-21): two tiers of source quality
//!
//! The remaining commands split into two groups by how much the Manual's
//! OCR actually gives:
//!
//! **Tier 1 — the Manual states the equation outright**, and this HLE
//! transcribes it with no reconstruction: Set Attitude (§5.5.1, Equation
//! 5-9), Subjective/object->global (§5.5.3, Equation 5-11), Objective/
//! global->object (§5.5.2, Equation 5-10), inner product with forward
//! attitude (§5.5.4, Equation 5-12, whose displayed matrix is OCR noise
//! but whose *prose* — "the first row of the attitude matrix represents
//! global coordinates of a unity vector (1,0,0) in the forward direction"
//! — pins down the row/column convention unambiguously), 3D angle
//! rotation (§5.6.1, Equation 5-13), and the vector-size-comparison
//! double variant (38h, fullsnes's list; same Equation 5-5 as 18h with a
//! `D2` output instead of a saturated single word).
//!
//! **Tier 2 — the Manual gives only prose and figures that did not
//! survive OCR**, for Projection Parameter Setting (§5.4.1), Raster Data
//! Calculation (§5.4.2), Object Projection (§5.4.3) and Screen Point
//! (§5.4.4). No public source in this ticket's citation list states the
//! pinhole-camera math these commands must implement — this HLE
//! reconstructs a standard perspective-camera model from the Manual's
//! *parameter descriptions* (a viewpoint pulled back from a base point
//! along a view line given by azimuth/zenith angles, a screen plane at a
//! second documented distance, and a ground plane at world Z=0) rather
//! than transcribing a stated formula, because none exists in the source
//! set. This is flagged as reconstruction, not fact, everywhere it
//! matters:
//!
//! - `Vof` ("raster number of imaginary center") and `Vva` ("raster
//!   number representing horizontal line") read, in the Manual's prose,
//!   like they could be the same horizon computation used two ways; this
//!   HLE computes them identically (see [`Camera::horizon_s`]) rather
//!   than inventing a second, undocumented formula for one of them.
//! - Raster numbers are treated as signed offsets from the screen center
//!   established by the Parameter command (`s = 0` at Cx/Cy), not as
//!   absolute PPU scanline indices — the Manual never states which
//!   physical scanline is "center", so this HLE does not invent a
//!   constant for it; `Vs` (the Raster command's own input) and `Vof`/
//!   `Vva` all live in the same offset space, so nothing downstream can
//!   disagree with itself.
//! - §5.4.4's Y output is documented "south is positive", the opposite
//!   of every other command's north-positive global Y (fixed by §5.4.1's
//!   "east is 0°, positive toward north" for the view azimuth) — this
//!   HLE negates only that one output, per the Manual's explicit note.
//!
//! No tolerance is stated for Tier 2 beyond "plausible perspective
//! geometry, not bit-exact": there is no documented reference value to
//! compare against. Tests check the reconstruction is internally
//! consistent (identity/degenerate cases, monotone raster scale with
//! distance from the horizon) rather than matching real hardware.
//!
//! ## Raster streaming (0Ah/1Ah) does not fit the request/response shape
//!
//! Every other command consumes N parameter words and produces M result
//! words, then returns to idle. Raster is different (Manual §5.4.2): one
//! input word (`Vs`), then result words stream out **indefinitely**,
//! A→B→C→D→A→B→… one scanline's matrix after another, until the CPU
//! "writes 8000H to the DR instead of reading element D" — a write in
//! the middle of what would otherwise be a read sequence, using the same
//! low-then-high two-byte protocol as every other DR write. [`Dsp1`]
//! tracks this with its own `raster` state (distinct from `collecting`,
//! which is for the fixed-size request/response shape).
//!
//! **Not every title sends the literal terminator.** Super Mario Kart's
//! census probe (ticket W14-21 acceptance #3) showed titles that stop
//! reading raster output and go straight to their next real command
//! without ever writing 8000H — an early version of this HLE that only
//! recognised the documented word desynchronised on exactly that (the
//! abandoned command's own opcode byte got folded into a terminator word
//! that was never coming, corrupting everything after it). So
//! [`Dsp1::write_dr`] only tentatively buffers a `0x00` byte seen while
//! `raster` is active (the terminator's low half); a **different** byte
//! is treated the same way idle treats an abandoned in-flight command —
//! dropped raster session, dispatched immediately as a fresh opcode —
//! and a buffered `0x00` followed by anything other than `0x80` simply
//! ends the raster session with both bytes discarded (no candidate
//! opcode survives that case, unlike the first). [`Dsp1::read_dr`] and
//! [`Dsp1::peek_dr`] regenerate the next scanline's four words lazily,
//! once the previous four are fully drained. Law 8: this cannot spin
//! forever even if a title never stops reading — [`RASTER_LINE_CAP`]
//! stops regeneration after a generous multiple of any real SNES visible
//! height, after which reads degrade to idle's `0x80` (a title that runs
//! past this on real hardware would have failed to see it stop
//! rendering usefully anyway).

use crate::state::{StateIn, StateOut};
use rf_core_api::StateError;
use std::collections::VecDeque;
use std::f64::consts::PI;

/// Cap on scanlines a single Raster (0Ah/1Ah) session will regenerate
/// without seeing the documented 8000H terminator — see the module doc's
/// "Raster streaming" section and law 8. Comfortably above any real SNES
/// visible height (224/239 lines) with headroom for a title that starts
/// mid-frame.
const RASTER_LINE_CAP: u32 = 400;

/// How many 16-bit input words a command consumes before it runs
/// (output word counts are fixed by [`Dsp1::run`]/[`execute`] themselves
/// and not needed here). `None` means "not implemented" — the caller
/// counts it and returns to idle without consuming any parameters
/// (fullsnes gives no guidance for an unimplemented opcode, and consuming
/// nothing is the only choice that cannot desynchronise a later,
/// understood command).
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
        // 38h vector size comparison, double-precision variant of 18h
        // (fullsnes's command list names both "Vector Size Comparison";
        // Manual §5.2.2 Equation 5-5 is the shared formula). Same 4
        // inputs as 18h, D2/L2H2 output instead of a saturated word.
        0x38 => 4,
        // 5.4.1 projection parameter setting (Manual §5.4.1): Fx,Fy,Fz,
        // Lfe,Les,Aas,Azs in, Vof,Vva,Cx,Cy out. See the module doc's
        // Tier 2 note — this command's math is a reconstruction.
        0x02 => 7,
        // 5.4.2 raster data calculation (Manual §5.4.2): Vs in, then an
        // unbounded A/B/C/D stream — see the module doc's "Raster
        // streaming" section. `1Ah` is the Manual's "not output via DMA"
        // twin of `0Ah`; this HLE's register-level model has no DMA
        // concept, so both behave identically.
        0x0A | 0x1A => 1,
        // 5.4.3 object projection (Manual §5.4.3): x,y,z in, H,V,M out.
        0x06 => 3,
        // 5.4.4 screen point (Manual §5.4.4): h,v in, X,Y out.
        0x0E => 2,
        // 5.5.1 set attitude A/B/C (Manual §5.5.1, Equation 5-9): m,
        // theta, phi, rho in, no output (the Manual's cycle table for
        // this command has no Output rows at all).
        0x01 | 0x11 | 0x21 => 4,
        // 5.5.3 object->global A/B/C, "Subjective" (Manual §5.5.3,
        // Equation 5-11): F,L,U in, X,Y,Z out.
        0x03 | 0x13 | 0x23 => 3,
        // 5.5.2 global->object A/B/C, "Objective" (Manual §5.5.2,
        // Equation 5-10): x,y,z in, F,L,U out.
        0x0D | 0x1D | 0x2D => 3,
        // 5.5.4 inner product with forward attitude A/B/C (Manual
        // §5.5.4, Equation 5-12): x,y,z in, S out.
        0x0B | 0x1B | 0x2B => 3,
        // 5.6.1 3D angle rotation, "Gyrate" (Manual §5.6.1, Equation
        // 5-13): theta_i, phi_i, rho_i, dtheta, dphi, drho in, theta_o,
        // phi_o, rho_o out.
        0x14 => 6,
        // Chapter 5 test commands (fullsnes): memory test always passes,
        // data-ROM transfer has no ROM to read from, version identifies
        // this HLE as a DSP-1B (the revision that fixed $28h).
        0x0F | 0x1F | 0x2F => 0,
        // $80 — NOT in the Manual, snesdev, or fullsnes's command lists.
        // Empirically discovered by this ticket's census probe
        // (acceptance #3): both Super Mario Kart and Pilotwings write it
        // repeatedly to DR before their first real command, with no
        // parameters and no read of a result in between, in a pattern
        // that repeats identically every frame (a driver-level presence
        // check, most likely — DR's own idle sentinel is `0x80`, so a
        // detection routine that writes `0x80` and expects to read
        // `0x80` back would look exactly like this). Treated as a no-op:
        // no parameters, no output, chip stays idle. This is a judgment
        // call from observed behaviour, not a documented command — flag
        // it here if a title is ever found relying on $80 doing anything
        // else.
        0x80 => 0,
        _ => return None,
    })
}

/// Run a fully-parameterised Tier-1-slice-1 command (the ones that need
/// no chip state beyond their own parameters). `input_words(cmd)` must
/// already have returned `Some(params.len())`. Commands that read or
/// write chip state (attitude matrices, projection parameters, the
/// raster stream) are dispatched directly in [`Dsp1::run`] instead.
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
        0x38 => cmd_range_double(params),
        0x14 => cmd_gyrate(params),
        0x0F => vec![0x0000],
        0x1F => vec![0x0000],
        0x2F => vec![0x0101],
        // $80, no-op (see `input_words`'s doc): no output, DR reads
        // 0x80 immediately, same as idle.
        0x80 => Vec::new(),
        _ => unreachable!("execute called for a command Dsp1::run should have handled itself"),
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

/// The inverse of [`a_to_radians`]: radians back to a raw `A` word.
/// `A` is documented as cyclic ("Loops"), so this wraps modulo 2^16
/// rather than saturating (unlike the plain-integer `I`/`T` outputs).
fn f64_to_a(radians: f64) -> u16 {
    let raw = (radians * (65536.0 / (2.0 * PI))).round();
    let wrapped = if raw.is_finite() { raw as i64 } else { 0 };
    wrapped as u32 as u16
}

/// `raw` as a plain 16-bit signed integer value (`I`).
fn i_to_i64(raw: u16) -> i64 {
    i64::from(raw as i16)
}

/// Round to nearest and clamp into `i16`, for an `I`-typed output.
fn f64_to_i(v: f64) -> u16 {
    v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16 as u16
}

/// Round to nearest and *wrap* into `i16`, for a `CI`-typed output
/// (snesdev: "Cyclic Int...Loops" — world coordinates roll over rather
/// than clip).
fn f64_to_ci(v: f64) -> u16 {
    let r = v.round();
    let wrapped = if r.is_finite() { r as i64 } else { 0 };
    wrapped as u32 as u16
}

/// `raw` as an unsigned 16-bit `U` value (module doc): value = raw, no
/// sign.
fn u_to_f64(raw: u16) -> f64 {
    f64::from(raw)
}

/// `raw` as a `2I` value (module doc): value = raw * 2.
fn i2_to_f64(raw: u16) -> f64 {
    f64::from(raw as i16) * 2.0
}

/// The inverse of [`i2_to_f64`]: raw = round(value / 2), saturated (`2I`
/// is a plain signed range, not documented as cyclic).
fn f64_to_i2(v: f64) -> u16 {
    (v / 2.0)
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16 as u16
}

/// `raw` -> `f64` for an `I8` value (module doc, Mode-7 matrix register
/// format) would be value = raw / 2^8; only the inverse (`I8` is always
/// an output in this HLE, never an input) is needed.
fn f64_to_i8(v: f64) -> u16 {
    (v * 256.0)
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16 as u16
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

/// 38h, vector size comparison's double-precision variant (fullsnes's
/// command list; Manual §5.2.2 Equation 5-5, same formula as 18h). The
/// full-precision difference is output as `D2` (unit `2^-1`) instead of
/// a saturated single word, so — unlike `cmd_range` — nothing is lost
/// for a title that needs the exact difference rather than a clamped
/// approximation.
fn cmd_range_double(p: &[u16]) -> Vec<u16> {
    let sum_sq: i64 = p[..3].iter().map(|&w| i_to_i64(w).pow(2)).sum();
    let range_sq = i_to_i64(p[3]).pow(2);
    let (lo, hi) = split_d((sum_sq - range_sq) * 2);
    vec![lo, hi]
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

/// 5.6.1 Gyrate ($14), Equation 5-13. Determines the new attitude angles
/// (theta_o, phi_o, rho_o) of a body after a small displacement
/// (dtheta, dphi, drho) is applied, on top of its current attitude
/// (theta_i, phi_i, rho_i).
///
/// Equation 5-13 (transcribed):
/// `theta_o = theta_i + sec(phi_i) * (dtheta*cos(rho_i) - dphi*sin(rho_i))`
/// `phi_o   = phi_i   + (dtheta*sin(rho_i) + dphi*cos(rho_i))`
/// `rho_o   = rho_i   - tan(phi_i) * (dtheta*cos(rho_i) + dphi*sin(rho_i)) + drho`
///
/// `sec(phi_i)` is singular at `phi_i = +/-90deg`; there this HLE
/// saturates to the largest finite `A` value rather than propagate
/// infinity/NaN (no source states hardware behaviour at that attitude).
fn cmd_gyrate(p: &[u16]) -> Vec<u16> {
    let theta_i = a_to_radians(p[0]);
    let phi_i = a_to_radians(p[1]);
    let rho_i = a_to_radians(p[2]);
    let dtheta = a_to_radians(p[3]);
    let dphi = a_to_radians(p[4]);
    let drho = a_to_radians(p[5]);
    let cos_phi_i = phi_i.cos();
    let sec_phi_i = if cos_phi_i.abs() < 1e-9 {
        1e9_f64.copysign(cos_phi_i.max(f64::MIN_POSITIVE))
    } else {
        1.0 / cos_phi_i
    };
    let tan_phi_i = phi_i.tan();
    let (sin_rho_i, cos_rho_i) = rho_i.sin_cos();
    let theta_o = theta_i + sec_phi_i * (dtheta * cos_rho_i - dphi * sin_rho_i);
    let phi_o = phi_i + (dtheta * sin_rho_i + dphi * cos_rho_i);
    let rho_o = rho_i - tan_phi_i * (dtheta * cos_rho_i + dphi * sin_rho_i) + drho;
    vec![f64_to_a(theta_o), f64_to_a(phi_o), f64_to_a(rho_o)]
}

/// A 3x3 row-major matrix of `f64`, used only for attitude matrices
/// (small, fixed-size — no hand-rolled indexing loop needed beyond the
/// fixed `0..3` bounds in [`mat_mul`], which always terminates: law 8).
type Mat3 = [[f64; 3]; 3];

fn mat_mul(a: Mat3, b: Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

/// 5.5.1 Set Attitude, Equation 5-9. `theta`/`phi`/`rho` are the Z/X/Y
/// axis angles (Manual's params 0/phi/cp — see the module's OCR note on
/// this section); `m` is the documented scalar constant multiplying the
/// whole matrix. Transcribed as `m * Ry(rho) * Rx(phi) * Rz(theta)`, in
/// the literal left-to-right order Equation 5-9's three matrices are
/// printed (Y-shaped matrix first, X-shaped second, Z-shaped third).
///
/// **OCR note**: the Manual's own prose describing "the order of
/// rotation" (§5.5.1 Function) names the axes in a sequence that
/// contradicts the parameter table's own angle-to-axis assignment two
/// paragraphs above it (most likely two Greek letters, phi and rho,
/// swapped somewhere in scanning/translation). This HLE follows the
/// *equation's* literal matrix shapes (a Y-rotation matrix, an
/// X-rotation matrix, and a Z-rotation matrix, in that printed order)
/// rather than either of the two disagreeing prose descriptions, since
/// the equation is the one place the OCR preserved unambiguous matrix
/// structure (which row/column has the fixed 0/1 entries).
fn attitude_matrix(m: f64, theta: f64, phi: f64, rho: f64) -> Mat3 {
    let ry: Mat3 = [
        [rho.cos(), 0.0, -rho.sin()],
        [0.0, 1.0, 0.0],
        [rho.sin(), 0.0, rho.cos()],
    ];
    let rx: Mat3 = [
        [1.0, 0.0, 0.0],
        [0.0, phi.cos(), phi.sin()],
        [0.0, -phi.sin(), phi.cos()],
    ];
    let rz: Mat3 = [
        [theta.cos(), theta.sin(), 0.0],
        [-theta.sin(), theta.cos(), 0.0],
        [0.0, 0.0, 1.0],
    ];
    let product = mat_mul(mat_mul(ry, rx), rz);
    let mut scaled = [[0.0; 3]; 3];
    for (i, row) in product.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            scaled[i][j] = v * m;
        }
    }
    scaled
}

/// 5.5.3 Subjective/object->global ($03/$13/$23), Equation 5-11:
/// `(F,L,U) * M = (X,Y,Z)` — row vector times matrix.
fn cmd_subjective(mat: &Mat3, p: &[u16]) -> Vec<u16> {
    let f = i2_to_f64(p[0]);
    let l = i2_to_f64(p[1]);
    let u = i2_to_f64(p[2]);
    let x = f * mat[0][0] + l * mat[1][0] + u * mat[2][0];
    let y = f * mat[0][1] + l * mat[1][1] + u * mat[2][1];
    let z = f * mat[0][2] + l * mat[1][2] + u * mat[2][2];
    vec![f64_to_i(x), f64_to_i(y), f64_to_i(z)]
}

/// 5.5.2 Objective/global->object ($0D/$1D/$2D), Equation 5-10:
/// `(x,y,z) * M^-1 = (F,L,U)`. `M = m * R` with `R` a pure rotation
/// (orthonormal), so `M^-1 = (1/m) * R^T` — `rot` here is `R` (the
/// matrix `attitude_matrix` would produce with `m=1.0`), computed
/// separately from the stored `m` so the inverse doesn't need a general
/// 3x3 solve (law 8: no iterative inversion, one reciprocal).
fn cmd_objective(rot: &Mat3, m: f64, p: &[u16]) -> Vec<u16> {
    let x = i_to_i64(p[0]) as f64;
    let y = i_to_i64(p[1]) as f64;
    let z = i_to_i64(p[2]) as f64;
    let inv_m = if m.abs() < 1e-9 { 0.0 } else { 1.0 / m };
    // (x,y,z) * R^T: output j = sum_i v[i] * R[j][i] = dot(v, row_j(R)).
    let f = inv_m * (x * rot[0][0] + y * rot[0][1] + z * rot[0][2]);
    let l = inv_m * (x * rot[1][0] + y * rot[1][1] + z * rot[1][2]);
    let u = inv_m * (x * rot[2][0] + y * rot[2][1] + z * rot[2][2]);
    vec![f64_to_i2(f), f64_to_i2(l), f64_to_i2(u)]
}

/// 5.5.4 Inner product with forward attitude ($0B/$1B/$2B), Equation
/// 5-12, read via the Function prose ("the first row of the attitude
/// matrix represents global coordinates of a unity vector (1,0,0) in the
/// forward direction") rather than the equation's own OCR-garbled
/// matrix: `S = dot((x,y,z), row0(M))`.
fn cmd_inner_product(mat: &Mat3, p: &[u16]) -> Vec<u16> {
    let x = i_to_i64(p[0]) as f64;
    let y = i_to_i64(p[1]) as f64;
    let z = i_to_i64(p[2]) as f64;
    let s = x * mat[0][0] + y * mat[0][1] + z * mat[0][2];
    vec![f64_to_i(s)]
}

/// A reconstructed pinhole camera for the Tier-2 projection commands —
/// see the module doc's Tier 2 note. Not sourced from a stated formula:
/// built from the Manual's §5.4.1 parameter *descriptions* (a viewpoint
/// pulled back from a base point along a view line, a screen plane
/// beyond it, a ground plane at world Z=0).
#[derive(Debug, Clone, Copy)]
struct Camera {
    /// Camera (viewpoint) position in global (X,Y,Z) coordinates.
    viewpoint: (f64, f64, f64),
    /// Unit view direction (from viewpoint toward the scene).
    forward: (f64, f64, f64),
    /// Unit "screen right" direction; always has zero Z component by
    /// construction (perpendicular to both `forward` and world-up),
    /// which is what makes the per-scanline raster transform affine in
    /// the horizontal screen coordinate (see `scale_at`'s doc).
    right: (f64, f64, f64),
    /// Unit "screen down" direction (perpendicular to `forward` and
    /// `right`).
    down: (f64, f64, f64),
    /// Distance from viewpoint to the screen plane (Manual's `Les`).
    screen_distance: f64,
}

fn cross(a: (f64, f64, f64), b: (f64, f64, f64)) -> (f64, f64, f64) {
    (
        a.1 * b.2 - a.2 * b.1,
        a.2 * b.0 - a.0 * b.2,
        a.0 * b.1 - a.1 * b.0,
    )
}

fn normalize(a: (f64, f64, f64)) -> (f64, f64, f64) {
    let mag = (a.0 * a.0 + a.1 * a.1 + a.2 * a.2).sqrt();
    if mag < 1e-12 {
        (0.0, 0.0, 0.0)
    } else {
        (a.0 / mag, a.1 / mag, a.2 / mag)
    }
}

impl Camera {
    /// Build the camera from a Projection Parameter Setting command's
    /// seven raw input words, in the order the Manual lists them: Fx,
    /// Fy, Fz, Lfe, Les, Aas, Azs.
    fn from_params(raw: &[u16; 7]) -> Camera {
        let base = (
            i_to_i64(raw[0]) as f64,
            i_to_i64(raw[1]) as f64,
            i_to_i64(raw[2]) as f64,
        );
        let lfe = u_to_f64(raw[3]);
        let les = u_to_f64(raw[4]);
        let azimuth = a_to_radians(raw[5]);
        let zenith = a_to_radians(raw[6]);
        // View line direction: zenith measured from straight up (world
        // +Z), azimuth measured east=0, positive toward north (Manual
        // §5.4.1's stated convention for Aas).
        let forward = (
            zenith.sin() * azimuth.cos(),
            zenith.sin() * azimuth.sin(),
            zenith.cos(),
        );
        let world_up = (0.0, 0.0, 1.0);
        let mut right = normalize(cross(world_up, forward));
        if right == (0.0, 0.0, 0.0) {
            // Degenerate: view line is exactly vertical (zenith/nadir).
            // No source states a convention here; pick a fixed screen
            // "right" so the camera basis is still well-defined.
            right = (1.0, 0.0, 0.0);
        }
        let down = cross(forward, right);
        let viewpoint = (
            base.0 - lfe * forward.0,
            base.1 - lfe * forward.1,
            base.2 - lfe * forward.2,
        );
        Camera {
            viewpoint,
            forward,
            right,
            down,
            screen_distance: les,
        }
    }

    /// `t` such that `viewpoint + t * ray_dir` has Z=0, for the ray
    /// through screen offset `(h, s)` (h: horizontal, right-positive; s:
    /// vertical, down-positive, both in screen pixel units matching
    /// `screen_distance`'s "screen horizontal distance is 256" scale).
    /// Independent of `h` because `right.2 == 0` by construction.
    fn ground_t(&self, s: f64) -> f64 {
        let ray_z = s * self.down.2 + self.screen_distance * self.forward.2;
        if ray_z.abs() < 1e-9 {
            // Ray parallel to the ground plane (looking exactly at the
            // horizon): no finite ground intersection. No source states
            // hardware behaviour here; saturate rather than propagate
            // infinity/NaN into the output words.
            f64::MAX.copysign(-self.viewpoint.2)
        } else {
            -self.viewpoint.2 / ray_z
        }
    }

    /// The ground-plane (X,Y) point a screen offset `(h, s)` projects
    /// to.
    fn ground_point(&self, h: f64, s: f64) -> (f64, f64) {
        let t = self.ground_t(s);
        let ray_x = h * self.right.0 + s * self.down.0 + self.screen_distance * self.forward.0;
        let ray_y = h * self.right.1 + s * self.down.1 + self.screen_distance * self.forward.1;
        (self.viewpoint.0 + t * ray_x, self.viewpoint.1 + t * ray_y)
    }

    /// Ground-plane distance covered per horizontal screen pixel at
    /// scanline offset `s` — the scale factor the Raster command's per-
    /// line matrix uses (bigger near the bottom of the screen/closer to
    /// the viewer, matching Mode-7 style perspective).
    fn scale_at(&self, s: f64) -> f64 {
        let t = self.ground_t(s);
        (t * self.right.0).hypot(t * self.right.1)
    }

    /// The screen-offset `s` at which the view ray becomes parallel to
    /// the ground plane (the horizon) — see the module doc's Tier 2 note
    /// on why this HLE uses the same value for both `Vof` and `Vva`.
    fn horizon_s(&self) -> f64 {
        if self.down.2.abs() < 1e-9 {
            0.0
        } else {
            -self.screen_distance * self.forward.2 / self.down.2
        }
    }
}

/// 5.4.1 Projection Parameter Setting ($02) — see the module doc's Tier
/// 2 note. Returns `(Vof, Vva, Cx, Cy)` output words and the raw
/// parameter words to store as the chip's live projection state.
fn cmd_projection_parameter(p: &[u16]) -> (Vec<u16>, [u16; 7]) {
    let raw: [u16; 7] = p.try_into().expect("input_words(0x02) == 7");
    let cam = Camera::from_params(&raw);
    let horizon = cam.horizon_s().round();
    let (cx, cy) = cam.ground_point(0.0, 0.0);
    let out = vec![
        f64_to_i(horizon),
        f64_to_i(horizon),
        f64_to_ci(cx),
        f64_to_ci(cy),
    ];
    (out, raw)
}

/// 5.4.3 Object Projection ($06) — Tier 2. Projects a global-coordinate
/// point onto the screen plane established by the live projection
/// parameters.
fn cmd_object_projection(proj: &[u16; 7], p: &[u16]) -> Vec<u16> {
    let cam = Camera::from_params(proj);
    let point = (
        i_to_i64(p[0]) as f64,
        i_to_i64(p[1]) as f64,
        i_to_i64(p[2]) as f64,
    );
    let d = (
        point.0 - cam.viewpoint.0,
        point.1 - cam.viewpoint.1,
        point.2 - cam.viewpoint.2,
    );
    let forward_comp = d.0 * cam.forward.0 + d.1 * cam.forward.1 + d.2 * cam.forward.2;
    let right_comp = d.0 * cam.right.0 + d.1 * cam.right.1 + d.2 * cam.right.2;
    let down_comp = d.0 * cam.down.0 + d.1 * cam.down.1 + d.2 * cam.down.2;
    // Behind (or at) the viewpoint has no documented behaviour; clamp
    // the denominator away from zero rather than divide by it.
    let safe_forward = if forward_comp.abs() < 1e-6 {
        1e-6_f64.copysign(if forward_comp == 0.0 {
            1.0
        } else {
            forward_comp
        })
    } else {
        forward_comp
    };
    let h = cam.screen_distance * right_comp / safe_forward;
    let v = cam.screen_distance * down_comp / safe_forward;
    let magnification = cam.screen_distance / safe_forward;
    vec![f64_to_i(h), f64_to_i(v), f64_to_i(magnification)]
}

/// 5.4.4 Screen Point ($0E) — Tier 2. The ground point a screen
/// coordinate projects to.
fn cmd_screen_point(proj: &[u16; 7], p: &[u16]) -> Vec<u16> {
    let cam = Camera::from_params(proj);
    let h = i_to_i64(p[0]) as f64;
    let v = i_to_i64(p[1]) as f64;
    let (gx, gy) = cam.ground_point(h, v);
    // Manual §5.4.4: this command's Y output is documented "south is
    // positive" — the opposite of every other command's north-positive
    // global Y (fixed by §5.4.1's "east is 0deg, positive toward north"
    // for Aas) — so negate only here.
    vec![f64_to_i(gx), f64_to_i(-gy)]
}

/// 5.4.2 Raster Data Calculation ($0A/$1A) — Tier 2, one scanline. `s`
/// is the raster's screen-offset coordinate (module doc: same space as
/// `Vof`/`Vva`/`Vs`, not an absolute PPU line). Outputs a per-scanline
/// rotate+scale matrix: heading (`Aas`) supplies the rotation, and
/// `Camera::scale_at` supplies the distance-dependent scale — the
/// Mode-7-style construction every raster/HDMA pseudo-3D renderer uses.
fn raster_matrix(proj: &[u16; 7], s: i64) -> [u16; 4] {
    let cam = Camera::from_params(proj);
    let scale = cam.scale_at(s as f64);
    let azimuth = a_to_radians(proj[5]);
    let (sin_a, cos_a) = azimuth.sin_cos();
    [
        f64_to_i8(scale * cos_a),
        f64_to_i8(-scale * sin_a),
        f64_to_i8(scale * sin_a),
        f64_to_i8(scale * cos_a),
    ]
}

/// Live state for an in-progress Raster ($0A/$1A) stream — see the
/// module doc's "Raster streaming" section.
#[derive(Debug, Clone)]
struct RasterGen {
    /// Screen-offset `s` of the next scanline to generate.
    next_s: i64,
    /// How many scanlines this session has generated so far (law 8
    /// bound: see [`RASTER_LINE_CAP`]).
    lines_emitted: u32,
}

/// One DSP-1 chip instance: the DR/SR protocol state machine plus the
/// command HLE above. Owned by [`crate::bus::SnesBus`] only when the
/// cartridge reports [`rf_cart::Coprocessor::Dsp1`].
#[derive(Debug, Clone)]
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
    /// implement yet.
    unknown_commands: u32,
    /// Per-opcode breakdown of `unknown_commands`, for a census probe to
    /// name exactly which opcode(s) a title needed (ticket W14-21
    /// acceptance #3). Diagnostic only — not part of save state (a
    /// save/load round trip losing this count changes nothing about
    /// emulated behaviour, only what a probe run across it would
    /// report).
    unknown_opcode_hist: [u32; 256],
    /// Raw words of the live Projection Parameter Setting ($02) state,
    /// if any command has set one yet. Stored raw (not the derived
    /// `Camera`) so save-state round-trips trivially and determinism
    /// never depends on `f64` byte-for-byte reproducibility across a
    /// save/load boundary — see the module doc.
    projection: Option<[u16; 7]>,
    /// Raw `(m, theta, phi, rho)` words for attitude slots A/B/C (index
    /// 0/1/2), set by $01/$11/$21. Same raw-storage reasoning as
    /// `projection`.
    attitude: [[u16; 4]; 3],
    /// Live Raster ($0A/$1A) stream state, if one is in progress.
    raster: Option<RasterGen>,
    /// Low byte of an in-progress raster-terminator write (module doc:
    /// any completed word written while `raster` is active ends it).
    raster_term_low: Option<u8>,
}

impl Default for Dsp1 {
    fn default() -> Self {
        Dsp1 {
            collecting: None,
            output: VecDeque::new(),
            output_high_pending: None,
            unknown_commands: 0,
            // `[u32; 256]` has no blanket `Default` impl (std only
            // provides one up to length 32), so this is spelled out.
            unknown_opcode_hist: [0; 256],
            projection: None,
            attitude: [[0; 4]; 3],
            raster: None,
            raster_term_low: None,
        }
    }
}

#[derive(Debug, Clone)]
struct Collecting {
    cmd: u8,
    needed: usize,
    words: Vec<u16>,
    low_pending: Option<u8>,
}

/// Attitude-slot index (0=A, 1=B, 2=C) from a command opcode whose high
/// nibble selects the slot (`$0x`=A, `$1x`=B, `$2x`=C) — true of every
/// attitude-family command ($01/$11/$21, $03/$13/$23, $0B/$1B/$2B,
/// $0D/$1D/$2D).
fn attitude_slot(cmd: u8) -> usize {
    (cmd >> 4) as usize
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

    /// The nonzero entries of the per-opcode unknown-command histogram,
    /// as `(opcode, count)`, sorted by count descending — see
    /// `unknown_opcode_hist`'s doc.
    #[must_use]
    pub fn unknown_opcodes(&self) -> Vec<(u8, u32)> {
        let mut v: Vec<(u8, u32)> = self
            .unknown_opcode_hist
            .iter()
            .enumerate()
            .filter(|&(_, &c)| c > 0)
            .map(|(op, &c)| (op as u8, c))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    }

    fn matrix_for_slot(&self, slot: usize) -> Mat3 {
        let [m_raw, theta_raw, phi_raw, rho_raw] = self.attitude[slot];
        attitude_matrix(
            t_to_f64(m_raw),
            a_to_radians(theta_raw),
            a_to_radians(phi_raw),
            a_to_radians(rho_raw),
        )
    }

    /// A byte written to the DR (command/data) register.
    ///
    /// Idle: the byte is an 8-bit command opcode (Manual §4.1). Mid-
    /// command: it is one half of the next 16-bit parameter word, low
    /// byte first (module doc: byte order is this HLE's documented
    /// choice, not a verified fact). Mid-raster-stream: it is one half
    /// of the terminator word (module doc's "Raster streaming" section).
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
        if self.raster.is_some() {
            match self.raster_term_low.take() {
                None => {
                    if byte == 0x00 {
                        // Low byte of a possible 8000H terminator
                        // (Manual §5.4.2). Tentative: buffered until the
                        // high byte confirms it.
                        self.raster_term_low = Some(byte);
                    } else {
                        // Not the documented terminator's low byte: the
                        // Manual only ever describes writing 8000H to
                        // end a raster stream, but real titles are not
                        // observed to always do so (ticket W14-21's
                        // census probe found stray writes here that are
                        // exactly a fresh command's opcode). No source
                        // states what a byte other than 0x00 means at
                        // this point, so this HLE treats it the same way
                        // idle treats an abandoned in-flight command:
                        // drop the raster session and dispatch `byte` as
                        // a brand-new 8-bit opcode immediately, rather
                        // than folding it into a terminator word that
                        // was never coming.
                        self.raster = None;
                        self.output.clear();
                        self.output_high_pending = None;
                        self.dispatch_opcode(byte);
                    }
                }
                Some(_low) => {
                    // Second byte after a buffered 0x00: only 0x80
                    // completes the documented 8000H terminator. Either
                    // way the CPU has moved past the raster stream, so
                    // it ends here; a non-0x80 second byte has no
                    // documented meaning and is simply dropped rather
                    // than mis-dispatched as an opcode (unlike the
                    // `None` arm above, there is no candidate opcode
                    // byte left over here — both bytes were consumed
                    // trying to complete the terminator).
                    self.raster = None;
                    self.output.clear();
                    self.output_high_pending = None;
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
        self.dispatch_opcode(byte);
    }

    /// Dispatch a byte as a fresh 8-bit command opcode (Manual §4.1):
    /// shared by the idle path and by a raster session abandoned without
    /// its documented terminator (see `write_dr`).
    fn dispatch_opcode(&mut self, byte: u8) {
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
            None => {
                self.unknown_commands += 1;
                self.unknown_opcode_hist[byte as usize] += 1;
            }
        }
    }

    fn run(&mut self, cmd: u8, params: &[u16]) {
        self.output_high_pending = None;
        match cmd {
            0x01 | 0x11 | 0x21 => {
                self.attitude[attitude_slot(cmd)] =
                    params.try_into().expect("input_words(attitude) == 4");
                self.output = VecDeque::new();
            }
            0x02 => {
                let (out, raw) = cmd_projection_parameter(params);
                self.projection = Some(raw);
                self.output = out.into();
            }
            0x06 => {
                self.output = match &self.projection {
                    Some(proj) => cmd_object_projection(proj, params).into(),
                    // No projection set yet: no source documents this
                    // case (a title is expected to call $02 first);
                    // return zeroed output rather than panic.
                    None => vec![0, 0, 0].into(),
                };
            }
            0x0E => {
                self.output = match &self.projection {
                    Some(proj) => cmd_screen_point(proj, params).into(),
                    None => vec![0, 0].into(),
                };
            }
            0x03 | 0x13 | 0x23 => {
                let mat = self.matrix_for_slot(attitude_slot(cmd));
                self.output = cmd_subjective(&mat, params).into();
            }
            0x0D | 0x1D | 0x2D => {
                let [m_raw, theta_raw, phi_raw, rho_raw] = self.attitude[attitude_slot(cmd)];
                let rot = attitude_matrix(
                    1.0,
                    a_to_radians(theta_raw),
                    a_to_radians(phi_raw),
                    a_to_radians(rho_raw),
                );
                self.output = cmd_objective(&rot, t_to_f64(m_raw), params).into();
            }
            0x0B | 0x1B | 0x2B => {
                let mat = self.matrix_for_slot(attitude_slot(cmd));
                self.output = cmd_inner_product(&mat, params).into();
            }
            0x0A | 0x1A => {
                let vs = i_to_i64(params[0]);
                let words = match &self.projection {
                    Some(proj) => raster_matrix(proj, vs),
                    None => [0, 0, 0, 0],
                };
                self.raster = Some(RasterGen {
                    next_s: vs + 1,
                    lines_emitted: 1,
                });
                self.output = VecDeque::from(words.to_vec());
            }
            _ => {
                self.output = execute(cmd, params).into();
            }
        }
    }

    /// The next raster scanline's words, without mutating `self` — used
    /// by both the mutating refill in [`Self::read_dr`] and the
    /// non-mutating peek in [`Self::peek_dr`].
    fn raster_peek_words(&self, s: i64) -> [u16; 4] {
        match &self.projection {
            Some(proj) => raster_matrix(proj, s),
            None => [0, 0, 0, 0],
        }
    }

    /// A byte read from the DR register (side-effecting: advances past
    /// the byte returned; a drained raster stream regenerates its next
    /// scanline here — module doc).
    ///
    /// fullsnes: idle/past-end-of-output reads return `0x80`.
    pub fn read_dr(&mut self) -> u8 {
        if let Some(high) = self.output_high_pending.take() {
            return high;
        }
        if self.output.is_empty() {
            let refill = self
                .raster
                .as_ref()
                .filter(|r| r.lines_emitted < RASTER_LINE_CAP)
                .map(|r| r.next_s);
            if let Some(next_s) = refill {
                let words = self.raster_peek_words(next_s);
                if let Some(r) = &mut self.raster {
                    r.next_s += 1;
                    r.lines_emitted += 1;
                }
                self.output.extend(words);
            }
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
    /// never perturb the machine it looks at — including a live raster
    /// stream's position.
    #[must_use]
    pub fn peek_dr(&self) -> u8 {
        if let Some(high) = self.output_high_pending {
            return high;
        }
        if let Some(word) = self.output.front() {
            return (word & 0xFF) as u8;
        }
        if let Some(r) = &self.raster {
            if r.lines_emitted < RASTER_LINE_CAP {
                let words = self.raster_peek_words(r.next_s);
                return (words[0] & 0xFF) as u8;
            }
        }
        0x80
    }

    /// Whether a Raster (`0Ah`/`1Ah`) stream is currently live (ticket
    /// W16-11): a DR-drain trace uses this to tell a CPU poll of a raster
    /// stream apart from an ordinary command's output drain.
    #[must_use]
    pub fn raster_active(&self) -> bool {
        self.raster.is_some()
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
        o.u32(self.unknown_commands)?;
        match &self.projection {
            None => o.bool(false)?,
            Some(p) => {
                o.bool(true)?;
                for w in p {
                    o.u16(*w)?;
                }
            }
        }
        for slot in &self.attitude {
            for w in slot {
                o.u16(*w)?;
            }
        }
        match &self.raster {
            None => o.bool(false)?,
            Some(r) => {
                o.bool(true)?;
                o.u64(r.next_s as u64)?;
                o.u32(r.lines_emitted)?;
            }
        }
        o.opt_u8(self.raster_term_low)
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
        self.projection = if i.bool()? {
            let mut raw = [0u16; 7];
            for w in &mut raw {
                *w = i.u16()?;
            }
            Some(raw)
        } else {
            None
        };
        for slot in &mut self.attitude {
            for w in slot.iter_mut() {
                *w = i.u16()?;
            }
        }
        self.raster = if i.bool()? {
            let next_s = i.u64()? as i64;
            let lines_emitted = i.u32()?;
            Some(RasterGen {
                next_s,
                lines_emitted,
            })
        } else {
            None
        };
        self.raster_term_low = i.opt_u8()?;
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
    /// and read back the result the same way. Not used for raster
    /// (0Ah/1Ah): its stream never naturally ends, so a dedicated
    /// bounded helper is used instead (law 8).
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
                // most 4 words, so this never triggers in practice (law
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

    /// Run a projection setup then a raster session, reading exactly
    /// `lines` scanlines (4 words each) before sending the documented
    /// 8000H terminator. Bounded by `lines`, never by the stream's own
    /// (nonexistent) natural end — law 8.
    fn run_raster(cmd: u8, vs: u16, lines: usize) -> Vec<[u16; 4]> {
        let mut d = Dsp1::new();
        d.write_dr(cmd);
        d.write_dr((vs & 0xFF) as u8);
        d.write_dr((vs >> 8) as u8);
        let mut out = Vec::with_capacity(lines);
        for _ in 0..lines {
            let mut line = [0u16; 4];
            for word in &mut line {
                let lo = d.read_dr();
                let hi = d.read_dr();
                *word = u16::from(lo) | (u16::from(hi) << 8);
            }
            out.push(line);
        }
        // Terminate: write 8000H (low byte 0x00, high byte 0x80).
        d.write_dr(0x00);
        d.write_dr(0x80);
        assert!(d.raster.is_none(), "terminator must return to idle");
        assert_eq!(d.read_dr(), 0x80, "idle after raster terminates");
        out
    }

    fn setup_projection(d: &mut Dsp1, params: &[u16; 7]) {
        d.write_dr(0x02);
        for &p in params {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
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
    fn range_double_matches_single_word_scaled() {
        // Same vectors as `range_subtracts_range_squared`: 25, as D2
        // (unit 2^-1) raw = 25*2 = 50, low word only (fits in 16 bits).
        let out = run_command(0x38, &[3, 4, 0, 0]);
        assert_eq!(out, vec![50, 0]);
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

    /// $80: empirically discovered by this ticket's census probe (see
    /// `input_words`'s doc) — both Super Mario Kart and Pilotwings write
    /// it repeatedly before their first real command. Treated as a
    /// no-op: does not count as unknown, produces no output, and DR
    /// keeps reading `0x80` (indistinguishable from idle) afterward.
    #[test]
    fn opcode_0x80_is_a_no_op_not_an_unknown_command() {
        let mut d = Dsp1::new();
        d.write_dr(0x80);
        assert_eq!(d.unknown_commands(), 0);
        assert_eq!(d.read_dr(), 0x80);
    }

    // ---- Slice 2: attitude commands (Tier 1, Equations 5-9 to 5-12) ----

    /// Attitude with zero angles is the identity, scaled by `m` (T's max
    /// positive value, ~0.99997 — not exactly 1.0, since T cannot
    /// represent 1.0 exactly; see the module doc's fixed-point table).
    #[test]
    fn attitude_zero_angles_is_scaled_identity() {
        let mat = attitude_matrix(t_to_f64(0x7FFF), 0.0, 0.0, 0.0);
        let m = t_to_f64(0x7FFF);
        for (i, row) in mat.iter().enumerate() {
            for (j, &cell) in row.iter().enumerate() {
                let expected = if i == j { m } else { 0.0 };
                assert!(
                    (cell - expected).abs() < 1e-6,
                    "mat[{i}][{j}] = {cell}, expected {expected}"
                );
            }
        }
    }

    /// Subjective (object->global) with the identity attitude maps F,L,U
    /// straight into X,Y,Z in *value* space — but F,L,U are `2I` (unit
    /// 2) while X,Y,Z are `I` (unit 1), so raw input word `100` decodes
    /// to value `200`, and the identity matrix hands that value straight
    /// to a plain-`I` output word: raw output `200`. Getting the `2I`
    /// factor of two backwards (module doc) would make this come out as
    /// `50`, not `200` — this is the one-directional check the module
    /// doc's fixed-point table calls for.
    #[test]
    fn subjective_identity_passes_through() {
        let mut d = Dsp1::new();
        d.write_dr(0x01); // attitude A, m ~ 1.0, angles 0
        for &p in &[0x7FFFu16, 0, 0, 0] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        d.write_dr(0x03); // subjective A, on the SAME instance (attitude is chip state)
        for &p in &[100u16, 200, 300] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        let out: Vec<u16> = (0..3)
            .map(|_| {
                let lo = d.read_dr();
                let hi = d.read_dr();
                u16::from(lo) | (u16::from(hi) << 8)
            })
            .collect();
        assert_eq!(out.len(), 3);
        assert!((out[0] as i16 - 200).abs() <= 1, "{out:?}");
        assert!((out[1] as i16 - 400).abs() <= 1, "{out:?}");
        assert!((out[2] as i16 - 600).abs() <= 1, "{out:?}");
    }

    /// Global->object then object->global round-trips a vector through
    /// the identity attitude (the `2I` factor-of-two cancels on a round
    /// trip — see the module doc — so this alone would not catch a
    /// scaling bug; `subjective_identity_passes_through` above checks
    /// one direction against a hand-computed value instead).
    #[test]
    fn objective_then_subjective_round_trips() {
        let mut d = Dsp1::new();
        d.write_dr(0x01);
        for &p in &[0x7FFFu16, 0, 0, 0] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        d.write_dr(0x0D); // objective A: global -> object
        for &p in &[1000i16 as u16, 2000, 3000] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        let mut flu = Vec::new();
        for _ in 0..3 {
            let lo = d.read_dr();
            let hi = d.read_dr();
            flu.push(u16::from(lo) | (u16::from(hi) << 8));
        }
        d.write_dr(0x03); // subjective A: object -> global
        for &p in &flu {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        let mut xyz = Vec::new();
        for _ in 0..3 {
            let lo = d.read_dr();
            let hi = d.read_dr();
            xyz.push(u16::from(lo) | (u16::from(hi) << 8));
        }
        assert!((xyz[0] as i16 - 1000).abs() <= 2, "{xyz:?}");
        assert!((xyz[1] as i16 - 2000).abs() <= 2, "{xyz:?}");
        assert!((xyz[2] as i16 - 3000).abs() <= 2, "{xyz:?}");
    }

    /// Inner product with an identity forward attitude (row0 = (1,0,0))
    /// is just the vector's X component (Equation 5-12's prose reading).
    #[test]
    fn inner_product_identity_is_x_component() {
        let mut d = Dsp1::new();
        d.write_dr(0x01);
        for &p in &[0x7FFFu16, 0, 0, 0] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        d.write_dr(0x0B); // on the SAME instance
        for &p in &[42u16, 100, 200] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        let lo = d.read_dr();
        let hi = d.read_dr();
        let out = u16::from(lo) | (u16::from(hi) << 8);
        assert!((out as i16 - 42).abs() <= 1, "{out:?}");
    }

    /// Attitude slots A/B/C are independent state (Manual: three
    /// separate matrices).
    #[test]
    fn attitude_slots_are_independent() {
        let mut d = Dsp1::new();
        d.write_dr(0x01); // A: identity-ish
        for &p in &[0x7FFFu16, 0, 0, 0] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        d.write_dr(0x11); // B: theta = 90 degrees
        for &p in &[0x7FFFu16, 0x4000, 0, 0] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        d.write_dr(0x0B); // A, on the SAME instance
        for &p in &[100u16, 0, 0] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        let a = {
            let lo = d.read_dr();
            let hi = d.read_dr();
            u16::from(lo) | (u16::from(hi) << 8)
        };
        d.write_dr(0x1B); // B, still the SAME instance
        for &p in &[100u16, 0, 0] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        let b = {
            let lo = d.read_dr();
            let hi = d.read_dr();
            u16::from(lo) | (u16::from(hi) << 8)
        };
        assert!((a as i16 - 100).abs() <= 1, "A row0 ~ x: {a:?}");
        assert!((b as i16).abs() <= 1, "B row0 ~ 0 (rotated 90deg): {b:?}");
    }

    // ---- Slice 2: 3D angle rotation (Tier 1, Equation 5-13) ----

    /// No initial attitude, no displacement: output angles equal input.
    #[test]
    fn gyrate_zero_displacement_is_identity() {
        let out = run_command(0x14, &[0x1000, 0x2000, 0x0800, 0, 0, 0]);
        assert_eq!(out, vec![0x1000, 0x2000, 0x0800]);
    }

    // ---- Slice 2: Tier 2 reconstruction (projection/raster) ----

    /// Straight-down view (Azs = 180deg, looking at the nadir) directly
    /// above the base point: the screen center projects exactly onto
    /// the base point's (X,Y), and there is no horizon (the ground
    /// plane fills the whole screen), i.e. `ground_t` never goes
    /// through the degenerate branch for any on-screen offset.
    #[test]
    fn projection_straight_down_centers_on_base_point() {
        let mut d = Dsp1::new();
        // Fx=1000, Fy=2000, Fz=500, Lfe=0 (viewpoint AT base point),
        // Les=100, Aas=0, Azs=180deg (straight down).
        setup_projection(&mut d, &[1000u16, 2000, 500, 0, 100, 0, 0x8000]);
        let out: Vec<u16> = (0..4)
            .map(|_| {
                let lo = d.read_dr();
                let hi = d.read_dr();
                u16::from(lo) | (u16::from(hi) << 8)
            })
            .collect();
        // Cx, Cy (words 3,4 => indices 2,3) should read back ~(1000,2000).
        assert!((out[2] as i16 - 1000).abs() <= 2, "{out:?}");
        assert!((out[3] as i16 - 2000).abs() <= 2, "{out:?}");
    }

    /// Raster scale shrinks monotonically as the scanline offset moves
    /// further below the horizon toward the viewer: near the horizon a
    /// single screen pixel spans a huge ground distance (far away, low
    /// resolution), while near the bottom of the screen (closer to the
    /// viewpoint) it spans much less (near, high resolution) — the
    /// "monotone C/D per line" behaviour the ticket asks for, checked
    /// structurally since there is no documented reference value (module
    /// doc's Tier 2 note).
    #[test]
    fn raster_scale_shrinks_toward_the_viewer() {
        let proj: [u16; 7] = [0, 0, 1000, 0, 256, 0, 0x6000]; // Azs=135deg
        let cam = Camera::from_params(&proj);
        let horizon = cam.horizon_s().ceil() as i64;
        let s0 = cam.scale_at((horizon + 5) as f64);
        let s1 = cam.scale_at((horizon + 20) as f64);
        let s2 = cam.scale_at((horizon + 60) as f64);
        assert!(s0 > s1, "s0={s0} s1={s1}");
        assert!(s1 > s2, "s1={s1} s2={s2}");
    }

    /// The raster stream regenerates A/B/C/D indefinitely until the
    /// documented 8000H terminator is written, then returns to idle
    /// cleanly (module doc's "Raster streaming" section).
    #[test]
    fn raster_stream_terminates_on_8000h() {
        let mut d = Dsp1::new();
        setup_projection(&mut d, &[0, 0, 1000, 0, 256, 0, 0x6000]);
        // Drain the projection's 4 output words first.
        for _ in 0..4 {
            d.read_dr();
            d.read_dr();
        }
        let lines = run_raster(0x0A, 10, 3);
        assert_eq!(lines.len(), 3);
    }

    /// $1A (the Manual's "not output via DMA" twin of $0A) produces the
    /// same values — this HLE has no DMA concept at the register level.
    #[test]
    fn raster_1ah_matches_0ah() {
        let proj = [0u16, 0, 1000, 0, 256, 0, 0x6000];
        let a = raster_matrix(&proj, 10);
        let b = raster_matrix(&proj, 10);
        assert_eq!(a, b);
    }

    // ---- Save-state coverage for slice-2 fields ----

    #[test]
    fn projection_and_attitude_state_round_trip() {
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
        setup_projection(&mut d, &[10, 20, 30, 40, 50, 60, 70]);
        for _ in 0..4 {
            d.read_dr();
            d.read_dr();
        }
        d.write_dr(0x01); // attitude A
        for &p in &[0x1000u16, 0x2000, 0x3000, 0x4000] {
            d.write_dr((p & 0xFF) as u8);
            d.write_dr((p >> 8) as u8);
        }
        d.write_dr(0x0A); // start a raster session, mid-stream
        d.write_dr(5);
        d.write_dr(0);
        d.read_dr(); // consume one byte so raster is live with a partial word

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        d.save(&mut StateOut::new(&mut stream)).unwrap();
        let mut restored = Dsp1::new();
        restored.load(&mut StateIn::new(&mut stream)).unwrap();

        assert_eq!(restored.projection, Some([10, 20, 30, 40, 50, 60, 70]));
        assert_eq!(restored.attitude[0], [0x1000, 0x2000, 0x3000, 0x4000]);
        assert!(restored.raster.is_some());
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
