//! Ticket W7-08 criterion 3: "audio RMS comparison against a reference
//! for a designated SPC set."
//!
//! **No such reference exists, and this file documents why rather than
//! fabricating one.** SPC files are commercial game-music rips; no
//! canonical hardware rendering of any SPC is published, and generating
//! one from another emulator would only prove agreement with THAT
//! emulator (see `tests/rom-manifest.toml`'s `blargg-spc-*` provenance
//! notes and plan.json's W7-08 2026-08-21 scope-amendment history, which
//! walks through the same dead end for the blargg SPC set specifically).
//!
//! What this file does instead follows the pattern already established
//! in this repo for exactly this situation: `docs/TESTING.md`'s "Audio
//! check" row and `crates/retroforge/tests/apu_mixer.rs` (NES,
//! `protocol = "audio_rms"` in `tests/rom-manifest.toml`) — a
//! **phase-cancellation self-oracle**. blargg's NES `apu_mixer` ROMs
//! generate a tone and its own inverse and cancel them to near-silence;
//! the ROM's own design is the reference, so no recording is needed.
//!
//! Here the same idea is built directly from the S-DSP's documented
//! behaviour rather than borrowed from an external ROM: two voices given
//! **bit-identical** BRR data, pitch, and GAIN-mode envelope, but
//! **opposite-sign VOL** (`$x0`/`$x1` are signed — a negative volume
//! inverts phase, the same fact `Echo::mix`'s per-voice sum already
//! relies on for panning). Two voices that process identical input
//! through identical per-sample state must produce sample-exact opposite
//! output, so the sum is not merely "near-silence" the way a real
//! recording's noise floor would force — it should be at most **one LSB**
//! away from zero, every sample, by construction.
//!
//! It is not exactly zero, and that residual is itself a real, cited
//! hardware behaviour rather than slop in this test: `$x4`/S4/S5 scale
//! `VOL` with `(sample * vol) >> 7`, and Rust's `>>` on a negative `i32`
//! is arithmetic (rounds toward `-infinity`), exactly like the SNES CPU's
//! own arithmetic shifts fullsnes documents elsewhere in this chapter
//! (e.g. the BRR predictors' own `SAR` steps, cited on `decode_brr`).
//! `(+100*s)>>7` and `(-100*s)>>7` therefore floor in OPPOSITE
//! directions and can differ by exactly 1 — asymmetric rounding, not a
//! mixing bug, and real hardware built the same way would show the same
//! one-count residual. [`two_phase_inverted_identical_voices_cancel_exactly`]
//! asserts the residual stays within that documented `+-1`, which is why
//! a real regression (a stray nondeterminism, a rounding path that isn't
//! sign-symmetric, or state that leaked between voices) still fails it —
//! those produce residuals far larger than one LSB, as the mutation check
//! below demonstrates.
//!
//! The mutation check the NES suite's own doc calls out as necessary
//! (`docs/TESTING.md`: "a +20% `tnd`-LUT mutation measures 0.31/0.21, so
//! the gate is not vacuous") is
//! [`detuning_one_voice_breaks_the_exact_cancellation`]: a small pitch
//! offset on one voice — still well within normal gameplay pitch-bend
//! range — must produce a measurable RMS, proving the exact-zero result
//! above is not simply "this mixer always outputs zero."

use rf_snes::apu::dsp::Dsp;

/// Nibbles `77997799`, shift 12, filter 0 — fullsnes's own "FULL-volume
/// SINE-wave" BRR waveform example (SNES APU DSP, "Waveform Examples").
/// Chosen because it is a real documented non-trivial waveform, not an
/// arbitrary pattern invented for this test.
const SINE_BLOCK: [u8; 9] = [
    (12 << 4) | 0x03, // range 12, filter 0, End+Loop (single block, loops on itself)
    0x77,
    0x99,
    0x77,
    0x99,
    0x77,
    0x99,
    0x77,
    0x99,
];

const BLOCK_ADDR: u16 = 0x10;
const SAMPLES: usize = 4096;

fn aram_with_sine_block() -> Vec<u8> {
    let mut aram = vec![0u8; 1024];
    aram[BLOCK_ADDR as usize..BLOCK_ADDR as usize + 9].copy_from_slice(&SINE_BLOCK);
    aram
}

/// **Voices 1 and 2, deliberately not 0.** Voice 0 alone has its `S3`
/// split into `S3a`/`S3b`/`S3c` at cycles 22/25/30 (see `dsp.rs`'s
/// `VStep` doc), which pushes its own `S4`/`S5` VOL-apply to cycle 31 —
/// on the far side of the `main`/DAC readout at cycles 26/27. The
/// documented consequence, found while building this test: voice 0's
/// contribution to a given output sample is computed one whole `mix()`
/// call later than voices 1-7's for what is otherwise the "same"
/// sample, a genuine pipeline asymmetry from the hardware's own layout,
/// not a bug. Pairing voice 0 against voice 1 made the exact-cancellation
/// oracle below fail on CORRECT code. Voices 1-7 share the same relative
/// S3-to-S4/S5 spacing, so any two of them (1 and 2, here) are the right
/// symmetric pair for this construction.
fn configure_voice(dsp: &mut Dsp, voice: usize, vol: i8, pitch: u16) {
    dsp.voices[voice].start = BLOCK_ADDR;
    dsp.voices[voice].loop_addr = BLOCK_ADDR;
    dsp.voices[voice].vol_left = vol;
    dsp.voices[voice].vol_right = vol;
    dsp.voices[voice].envelope.gain = 0x7F; // adsr_enabled defaults false: GAIN mode, held open.
    dsp.voices[voice].pitch = pitch;
}

/// **THE ORACLE.** Two identically-driven voices, opposite sign VOL, must
/// sum to within 1 LSB of (0, 0) at every sample — see the module doc for
/// why it is `+-1` and not exactly zero (asymmetric arithmetic-shift
/// rounding on the VOL multiply, a real and cited hardware behaviour).
#[test]
fn two_phase_inverted_identical_voices_cancel_exactly() {
    let mut aram = aram_with_sine_block();
    let mut dsp = Dsp::new();
    configure_voice(&mut dsp, 1, 100, 0x1000);
    configure_voice(&mut dsp, 2, -100, 0x1000);
    dsp.key_on(0x06);

    let mut sum_sq: i64 = 0;
    for i in 0..SAMPLES {
        let (l, r) = dsp.mix(&mut aram);
        assert!(
            l.abs() <= 1 && r.abs() <= 1,
            "sample {i}: phase-inverted identical voices must cancel to \
             within 1 LSB (the documented rounding residual), got ({l}, {r})"
        );
        sum_sq += i64::from(l) * i64::from(l) + i64::from(r) * i64::from(r);
    }
    let rms = ((sum_sq as f64) / (2.0 * SAMPLES as f64)).sqrt();
    assert!(
        rms <= 1.0,
        "RMS over the whole run must stay within the 1-LSB rounding \
         residual, got {rms:.6}"
    );
}

/// **THE MUTATION CHECK.** Same setup, but voice 2 is detuned by one part
/// in 64 (`pitch` `0x1000` -> `0x1040`, about a 1.6% pitch bend — well
/// within a game's normal vibrato range). The two voices now drift out of
/// phase via the Gaussian-interpolated resample, and the RMS over the
/// same window must clear a real, non-trivial threshold — proving the
/// exact-zero result above is a property of the SPECIFIC configuration,
/// not a mixer that always outputs silence regardless of input.
#[test]
fn detuning_one_voice_breaks_the_exact_cancellation() {
    let mut aram = aram_with_sine_block();
    let mut dsp = Dsp::new();
    configure_voice(&mut dsp, 1, 100, 0x1000);
    configure_voice(&mut dsp, 2, -100, 0x1040);
    dsp.key_on(0x06);

    let mut sum_sq: i64 = 0;
    for _ in 0..SAMPLES {
        let (l, r) = dsp.mix(&mut aram);
        sum_sq += i64::from(l) * i64::from(l) + i64::from(r) * i64::from(r);
    }
    let rms = ((sum_sq as f64) / (2.0 * SAMPLES as f64)).sqrt();
    // Peak sample magnitude at these VOLs is on the order of 100 * 15-bit
    // sample / 128 ~= a few hundred; a detuned mix's RMS measures in the
    // hundreds (see module doc's mutation-check rationale). 20 is a small
    // fraction of that peak and well clear of the exact-zero case above,
    // so this is a real gate, not a threshold picked to pass.
    assert!(
        rms > 20.0,
        "detuning one voice by 1/64 pitch must break the cancellation \
         measurably (got RMS {rms:.3}) — if this stays near zero, the \
         cancellation oracle above cannot be trusted to catch a real bug"
    );
}
