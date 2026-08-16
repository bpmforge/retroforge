//! blargg's `apu_mixer` suite (ticket W2-05, FR-CORE-024) — the ROM-level
//! check on W2-01b's non-linear mixer LUTs.
//!
//! ## The oracle is the ROM's own design, and the ROM states it on screen
//!
//! These four ROMs never self-report; they print their expected structure
//! and leave the judging to a listener. Read out of the nametable, the
//! square ROM says: "1. Should play short tone. 2. Should be nearly silent.
//! 3. Should play short tone." — because, per `apu_mixer/readme.txt`, "the
//! tests have the channel under test generate a tone, then generate the
//! inverse waveform using the DMC DAC, canceling to (near) silence if
//! everything is correct."
//!
//! So the section between the two beeps is the measurement, and the ROM
//! carries its own answer. No reference recording is needed, and none
//! exists in this repo (`docs/TESTING.md`'s row said "RMS envelope match
//! against known-good recording"; this is that check with the ROM's own
//! cancellation standing in for the recording — the stronger oracle, since
//! a recording would also encode this engine's filtering, decimation and
//! volume scaling, while cancellation measures only the ratios between
//! channels, which is exactly what the LUTs decide).
//!
//! ## What is gated tightly, what is not, and why — measured, not assumed
//!
//! The first version of this file asserted "the quietest eighth of the
//! capture is quiet", and it passed happily with the `tnd` LUT scaled by
//! **+20%**: it was measuring the idle silence *before* the first beep. The
//! gate is now anchored to the section *between* the beeps, located by
//! finding them, and its sensitivity is recorded here from that deliberate
//! mutation run (100 ms RMS windows, as a fraction of the beep peak):
//!
//! | ROM | correct | tnd LUT x1.20 | gated |
//! |---|---|---|---|
//! | `square` | 0.061 | 0.31 | yes, < 0.10 |
//! | `dmc` | 0.060 | 0.21 | yes, < 0.10 |
//! | `triangle` | 0.092 | 0.11 | no — see below |
//! | `noise` | n/a | n/a | no — see below |
//!
//! - **`triangle` is not gated on residue.** It cancels to 0.092 of peak
//!   against an idle noise floor of 0.057, so a threshold tight enough to
//!   catch the mutation (0.11) would sit inside the measurement's own
//!   spread. That residue is itself a finding — a correctly weighted
//!   triangle should cancel to the floor the way `square` does — and it is
//!   recorded in W2-05's notes rather than papered over with a threshold
//!   chosen to make the test pass.
//! - **`noise` is not gated on residue at all**, because its section is
//!   *supposed* to be loud: its own on-screen text says "2. Should fade
//!   noise in, and out, **without any tone**". "Without a tone" is a
//!   spectral property, and checking it needs an FFT with a peak-to-median
//!   bin ratio; this test does not fake that with an amplitude measurement.
//!
//! Both get the structural check their content allows (two beeps, bounded
//! activity between them), which is real but weaker, and both say so.

use std::path::{Path, PathBuf};

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, PpuPixel};

/// Collects every audio sample the core emits, and nothing else.
#[derive(Default)]
struct AudioSink {
    samples: Vec<i16>,
}

impl CoreSink for AudioSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, samples: &[i16]) {
        self.samples.extend_from_slice(samples);
    }
    fn event(&mut self, _ev: CoreEvent) {}
}

/// 100 ms at the core's 48 kHz output rate.
const WINDOW: usize = 4_800;
/// Long enough for every ROM's second beep (the latest, `noise`, lands at
/// ~18.8 s) plus margin.
const FRAMES: u32 = 1_300;

fn resolve(name: &str) -> Option<PathBuf> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../roms/nes/apu_mixer/{name}.nes"));
    path.is_file().then_some(path)
}

/// Runs `rom`, returning its audio through the same output-stage filter
/// chain the app applies (`rf_audio::Filters` — nesdev's 90 Hz / 440 Hz
/// high-passes and 14 kHz low-pass).
///
/// **Filtering here is not cosmetic.** The mixer's output is unipolar
/// (`rf_nes::apu`'s `mixed_output` returns 0.0..1.0, and the triangle DAC
/// alone parks it near 0.25), so an unfiltered capture's RMS is dominated
/// by DC and measures the offset rather than the signal: measured, the
/// square ROM's cancellation section reads **0.88 of peak unfiltered and
/// 0.06 filtered**. The filtered stream is also what a user hears.
fn capture(rom_path: &Path) -> Vec<i16> {
    let rom = std::fs::read(rom_path).expect("rom readable");
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("rom loads");
    let mut sink = AudioSink::default();
    for _ in 0..FRAMES {
        stepper.step_frame(&mut sink);
    }
    let mut samples = sink.samples;
    rf_audio::Filters::new(rf_audio::CORE_SAMPLE_RATE).process_i16(&mut samples);
    samples
}

fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / samples.len() as f64).sqrt()
}

/// The "RMS envelope" the acceptance criterion names: one RMS per window.
fn rms_envelope(samples: &[i16]) -> Vec<f64> {
    samples.chunks(WINDOW).map(rms).collect()
}

/// What one ROM's capture looks like, located rather than assumed.
struct Structure {
    peak: f64,
    /// Envelope windows strictly between the two beeps, with a window of
    /// margin either side so a beep's decay tail is not counted as residue.
    between: Vec<f64>,
}

fn analyze(name: &str, envelope: &[f64]) -> Structure {
    let peak = envelope.iter().copied().fold(0.0f64, f64::max);
    assert!(
        peak > 500.0,
        "{name}: nothing audible in the whole capture (peak RMS {peak:.1}) -- the ROM never \
         played, so anything measured between 'beeps' would be meaningless"
    );

    let beeps: Vec<usize> = envelope
        .iter()
        .enumerate()
        .filter(|(_, v)| **v > peak * 0.5)
        .map(|(i, _)| i)
        .collect();
    assert!(
        beeps.len() >= 2,
        "{name}: expected two beeps bracketing the test section, found {beeps:?}"
    );

    // The first contiguous run of loud windows is beep 1; the next run
    // starts beep 2.
    let first_end = beeps
        .windows(2)
        .find(|pair| pair[1] != pair[0] + 1)
        .map_or(beeps[0], |pair| pair[0]);
    let second_start = *beeps
        .iter()
        .find(|&&w| w > first_end + 1)
        .unwrap_or_else(|| panic!("{name}: no second beep after window {first_end}"));

    let lo = first_end + 2;
    let hi = second_start.saturating_sub(1);
    assert!(
        hi > lo + 4,
        "{name}: the section between the beeps is too short to measure ({lo}..{hi})"
    );
    Structure {
        peak,
        between: envelope[lo..hi].to_vec(),
    }
}

/// 90th percentile — used instead of the maximum because the ROMs warn on
/// screen that "Some clicking might occur between the two tones", and one
/// click window should not decide the result.
fn p90(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[(sorted.len() * 9 / 10).min(sorted.len() - 1)]
}

/// The gated ROMs: those whose cancellation is sharp enough that a wrong
/// mixer moves the measurement well clear of the noise floor (module doc's
/// mutation table).
const GATED: [(&str, f64); 2] = [("square", 0.10), ("dmc", 0.10)];
/// The structural-only ROMs, and why (module doc).
const STRUCTURAL: [&str; 2] = ["triangle", "noise"];

#[test]
fn square_and_dmc_cancel_to_near_silence_between_their_beeps() {
    let mut missing = 0;
    let mut failures = Vec::new();

    for (name, threshold) in GATED {
        let Some(path) = resolve(name) else {
            missing += 1;
            continue;
        };
        let envelope = rms_envelope(&capture(&path));
        let structure = analyze(name, &envelope);
        let residue = p90(&structure.between) / structure.peak;
        eprintln!(
            "apu_mixer/{name}: peak {:.0}, residue p90 {residue:.4} of peak (limit {threshold})",
            structure.peak
        );
        if residue > threshold {
            failures.push(format!(
                "{name}: cancellation residue {residue:.4} of peak exceeds {threshold}"
            ));
        }
    }

    if missing == GATED.len() {
        eprintln!("SKIP apu_mixer: ROMs not found. Fetch: scripts/fetch-test-roms.sh");
        return;
    }
    assert_eq!(
        missing, 0,
        "partial fetch: {missing} of 2 gated ROMs missing"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `triangle` and `noise` get the structure check their content allows, and
/// their residues are printed so a regression stays visible in the log even
/// where it cannot be gated. See the module doc for why each is here rather
/// than in [`GATED`].
#[test]
fn triangle_and_noise_have_the_structure_their_rom_text_describes() {
    let mut missing = 0;
    for name in STRUCTURAL {
        let Some(path) = resolve(name) else {
            missing += 1;
            continue;
        };
        let envelope = rms_envelope(&capture(&path));
        let structure = analyze(name, &envelope);
        let residue = p90(&structure.between) / structure.peak;
        eprintln!(
            "apu_mixer/{name}: peak {:.0}, residue p90 {residue:.4} of peak (NOT gated -- see \
             module doc)",
            structure.peak
        );
        assert!(
            residue < 0.5,
            "{name}: the section between the beeps is nearly as loud as the beeps \
             ({residue:.3} of peak) -- that is neither 'nearly silent' nor 'noise without a \
             tone', it is a broken mixer"
        );
    }
    if missing == STRUCTURAL.len() {
        eprintln!("SKIP apu_mixer: ROMs not found. Fetch: scripts/fetch-test-roms.sh");
    }
}
