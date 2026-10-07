//! **The SNES core sends its sound** (ticket W7-22).
//!
//! W7-08 made the S-DSP correct and reachable and stopped there, on
//! purpose ("routing this into rf-audio is NOT part of ticket W7-08"); no
//! ticket picked it up, so SNES games were silent — and, in an `audio`
//! build, unpaced, because the host paced frames by an audio ring nothing
//! filled. This boots a minimal LoROM and checks the core hands
//! `CoreSink::audio` a 48 kHz stream: about 800 samples a frame (48 000
//! at the SNES's ~60.1 fps).

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, PpuPixel, Step};
use rf_snes::core::SnesCore;

fn rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0] = 0x80; // BRA *
    rom[1] = 0xFE;
    for (i, b) in b"RF AUDIO TEST        ".iter().enumerate() {
        rom[0x7FC0 + i] = *b;
    }
    rom[0x7FD5] = 0x20;
    rom[0x7FD7] = 0x08;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    rom
}

#[derive(Default)]
struct Count {
    samples: usize,
}

impl CoreSink for Count {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, samples: &[i16]) {
        self.samples += samples.len();
    }
    fn event(&mut self, _ev: CoreEvent) {}
}

#[test]
fn a_running_snes_emits_about_48khz_of_mono_audio() {
    let mut core = SnesCore::load(&rom()).expect("loads");
    let mut sink = Count::default();
    for _ in 0..10 {
        core.step(Step::Frame, &mut sink);
    }
    sink.samples = 0;
    let frames = 120;
    for _ in 0..frames {
        core.step(Step::Frame, &mut sink);
    }
    let per_frame = sink.samples as f64 / f64::from(frames);
    assert!(
        (780.0..=820.0).contains(&per_frame),
        "expected ~799 samples a frame at 48 kHz, got {per_frame}"
    );
}
