//! Ticket W4-10b criterion 3, and the one that decides the design:
//! **mute/solo is a host-side mix and provably does not perturb the
//! simulation.**
//!
//! `EmuStepper::state_hash` is SHA-256 over every save-state region — CPU
//! registers, bus counters, the entire PPU and APU, WRAM, mapper
//! registers and battery RAM. If a mute reached the mixer, or capture
//! changed the machine, this test fails. That matters because
//! `tests/determinism.rs` and the `.rfreplay` format hash the same thing:
//! a debugging aid that changed it would make two sessions that muted
//! different channels produce different replay hashes for identical play.

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_debugger::audio_scope::{self, MuteState};
use rf_nes::apu::CHANNEL_COUNT;

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// A ROM that actually drives the APU: all five channels enabled, a
/// square wave running, and RAM traffic so the CPU state moves too. A
/// silent workload would make every channel's stream identical and the
/// invariance test vacuous.
fn apu_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    let prg = &mut rom[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x0F, 0x8D, 0x15, 0x40, // enable all five channels
        0xA9, 0x9F, 0x8D, 0x00, 0x40, // pulse 1 duty/volume
        0xA9, 0x40, 0x8D, 0x02, 0x40, //
        0xA9, 0x08, 0x8D, 0x03, 0x40, //
        0xA9, 0x9F, 0x8D, 0x04, 0x40, // pulse 2
        0xA9, 0x60, 0x8D, 0x06, 0x40, //
        0xA9, 0x08, 0x8D, 0x07, 0x40, //
        0xA9, 0xFF, 0x8D, 0x08, 0x40, // triangle linear counter
        0xA9, 0x30, 0x8D, 0x0A, 0x40, //
        0xA9, 0x08, 0x8D, 0x0B, 0x40, //
        0xA9, 0x0F, 0x8D, 0x0C, 0x40, // noise volume
        0xA9, 0x04, 0x8D, 0x0E, 0x40, //
        0xA9, 0x08, 0x8D, 0x0F, 0x40, //
        0xAD, 0x00, 0x00, 0x69, 0x01, 0x8D, 0x00, 0x00, 0x4C, 0x41, 0x80,
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    rom
}

fn run(stepper: &mut EmuStepper, frames: u64) {
    stepper.resume();
    for _ in 0..frames {
        stepper.latch_and_advance_frame(InputFrame::empty(), &mut NullSink);
    }
}

/// **Criterion 3.** Every mute combination, against a run with capture
/// off, must leave the machine byte-identical.
#[test]
fn state_hash_is_identical_under_every_mute_combination() {
    let rom = apu_rom();

    // Baseline: capture off entirely — a build with no scopes.
    let mut baseline = EmuStepper::from_ines_bytes(&rom).expect("rom loads");
    run(&mut baseline, 20);
    let expected = baseline.state_hash();

    // Every one of the 32 mute combinations, with capture ON.
    for bits in 0u32..(1 << CHANNEL_COUNT) {
        let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("rom loads");
        stepper.set_audio_channel_capture(true);
        let mut mute = MuteState::<CHANNEL_COUNT>::new();
        for i in 0..CHANNEL_COUNT {
            if bits & (1 << i) != 0 {
                mute.toggle_mute(i);
            }
        }
        run(&mut stepper, 20);
        // Drain and mix host-side, exactly as the UI does — if THIS
        // could perturb the machine, doing it before the hash is what
        // would catch it.
        let channels = stepper.take_audio_channel_samples();
        let _ = audio_scope::mix_host_side(&channels, &mute);

        assert_eq!(
            stepper.state_hash(),
            expected,
            "mute combination {bits:#07b} changed the machine — a debugging aid must never \
             perturb the thing being debugged, and determinism.rs plus .rfreplay hash exactly \
             this value"
        );
    }
}

/// Solo is evaluated at mix time, so it must be invariant too — and this
/// is a separate assertion because solo takes a different branch.
#[test]
fn state_hash_is_identical_under_solo() {
    let rom = apu_rom();
    let mut baseline = EmuStepper::from_ines_bytes(&rom).expect("rom loads");
    run(&mut baseline, 20);
    let expected = baseline.state_hash();

    for i in 0..CHANNEL_COUNT {
        let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("rom loads");
        stepper.set_audio_channel_capture(true);
        let mut mute = MuteState::<CHANNEL_COUNT>::new();
        mute.toggle_solo(i);
        run(&mut stepper, 20);
        let channels = stepper.take_audio_channel_samples();
        let _ = audio_scope::mix_host_side(&channels, &mute);
        assert_eq!(
            stepper.state_hash(),
            expected,
            "soloing channel {i} changed the machine"
        );
    }
}

/// **Anti-vacuity, and without it everything above is worthless.** The
/// invariance tests pass trivially if capture produces nothing and the
/// mix is empty. This pins that the capture is real, at the mixed
/// stream's own rate, and that different channels genuinely differ.
#[test]
fn capture_produces_real_per_channel_streams_at_the_mixed_rate() {
    let rom = apu_rom();
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("rom loads");

    // Off by default: an untraced session pays nothing.
    run(&mut stepper, 2);
    assert!(
        stepper
            .take_audio_channel_samples()
            .iter()
            .all(Vec::is_empty),
        "channel capture must be off by default"
    );

    stepper.set_audio_channel_capture(true);
    run(&mut stepper, 20);
    let channels = stepper.take_audio_channel_samples();

    for (i, c) in channels.iter().enumerate() {
        assert!(
            !c.is_empty(),
            "channel {i} produced no samples — the scopes would draw nothing"
        );
    }
    // Same rate as the mixed stream: ~735 samples per NTSC frame at
    // 44.1 kHz, so 20 frames is on the order of 14 700. The bound is
    // loose on purpose — the point is that this is an AUDIO-rate stream,
    // not a per-frame or per-scanline sample.
    let len = channels[0].len();
    assert!(
        len > 10_000,
        "only {len} samples over 20 frames — that is not audio-rate capture"
    );
    assert!(
        channels.iter().all(|c| c.len() == len),
        "every channel must share one timebase, or the host mix lines them up wrongly"
    );

    // At least two channels must actually differ, or a mute could not
    // possibly be audible and the mix tests prove nothing.
    assert_ne!(
        channels[0], channels[2],
        "pulse 1 and triangle produced identical streams — the capture is not per-channel"
    );
    assert!(
        channels.iter().any(|c| c.iter().any(|s| *s != 0)),
        "every channel is silent; the workload is not driving the APU"
    );
}

/// Turning capture off must free the buffers, not merely stop appending —
/// a session that opened the scopes once should not hold their memory for
/// the rest of its life.
#[test]
fn disabling_capture_drops_what_was_captured() {
    let rom = apu_rom();
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("rom loads");
    stepper.set_audio_channel_capture(true);
    run(&mut stepper, 5);
    stepper.set_audio_channel_capture(false);
    assert!(
        stepper
            .take_audio_channel_samples()
            .iter()
            .all(Vec::is_empty),
        "disabling capture must discard the buffered streams"
    );
}
