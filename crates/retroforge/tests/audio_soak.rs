//! The 5-minute underrun soak (ticket W2-05's second acceptance criterion:
//! "no underruns over 5-min soak on dev machine").
//!
//! ## Why this is `#[ignore]`d and feature-gated, and what that costs
//!
//! It needs a real audio device — that is the entire point, since an
//! underrun is a property of a device consuming faster than the emulator
//! produces — and it takes five minutes. Neither belongs in a gate that
//! runs on every commit, and CI has no sound card at all. So it is opt-in
//! twice over, and the cost is stated plainly: **the criterion this file
//! checks is verified by running it deliberately, not by the ordinary
//! gate.** The pieces it is built from (ring, rate loop, resampler,
//! filters, the chain in `crate::audio_out`) are all covered headlessly and
//! do run on every commit.
//!
//! Run it:
//!
//! ```text
//! cargo test --release -p retroforge --features audio --test audio_soak -- --ignored --nocapture
//! ```
//!
//! ## What it asserts, and why underruns rather than "it sounded fine"
//!
//! `rf_audio::AudioDevice` counts every sample the callback had to fill
//! with silence because the ring was empty. That count is the mechanical
//! form of "no underruns": it cannot be argued with, it does not depend on
//! anyone listening, and it is exactly the quantity
//! `docs/design/FAILURE_MODES.md` FM-02's underrun cascade is about.

#![cfg(feature = "audio")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use retroforge::audio_out::AudioOut;
use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, PpuPixel};

/// Feeds the audio chain and discards video.
struct SoakSink<'a> {
    audio: &'a mut AudioOut,
}

impl CoreSink for SoakSink<'_> {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, samples: &[i16]) {
        self.audio.push(samples);
    }
    fn event(&mut self, _ev: CoreEvent) {}
}

fn rom_path() -> Option<PathBuf> {
    // Any ROM that makes continuous sound will do; apu_mixer's square ROM
    // is small, deterministic and always playing something.
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../roms/nes/apu_mixer/square.nes");
    path.is_file().then_some(path)
}

#[test]
#[ignore = "needs a real audio device and runs for five minutes; see this file's module doc"]
fn five_minutes_of_playback_produces_no_underruns() {
    let Some(path) = rom_path() else {
        eprintln!("SKIP audio soak: roms/nes/apu_mixer/square.nes not found");
        return;
    };
    let mut audio = match AudioOut::open() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("SKIP audio soak: no audio device ({e})");
            return;
        }
    };
    assert!(
        audio.is_clock(),
        "an opened device must be the frame clock, or this soak measures the wall-clock pacer"
    );

    let rom = std::fs::read(&path).expect("rom readable");
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("rom loads");
    stepper.resume();

    let deadline = Instant::now() + Duration::from_secs(5 * 60);
    let mut frames = 0u64;
    let mut worst_fill = 1.0f32;
    let mut best_fill = 0.0f32;

    while Instant::now() < deadline {
        // The audio clock: run a frame only when the device has drained the
        // ring back to target (`crate::audio_out`'s module doc).
        while audio.should_wait() {
            std::thread::sleep(Duration::from_millis(1));
        }
        stepper.tick_running(&mut SoakSink { audio: &mut audio });
        frames += 1;
        let fill = audio.fill();
        worst_fill = worst_fill.min(fill);
        best_fill = best_fill.max(fill);
    }

    let stats = audio.stats();
    let underruns = audio.underrun_samples();
    eprintln!(
        "audio soak: {frames} frames, fill {worst_fill:.3}..{best_fill:.3}, final ratio \
         {:.5}, underrun samples {underruns}",
        stats.ratio
    );

    // Five minutes at ~60 fps.
    assert!(
        frames > 16_000,
        "the audio clock should have paced ~18000 frames in five minutes, got {frames} -- \
         either the emulator could not keep up or the clock is not pacing"
    );
    assert_eq!(
        underruns, 0,
        "the device had to fill {underruns} samples with silence over five minutes"
    );
    assert!(
        stats.ratio > 1.0 - 0.005 && stats.ratio < 1.0 + 0.005,
        "the rate correction must stay inside ±0.5%, ended at {}",
        stats.ratio
    );
}
