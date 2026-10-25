//! Cost of the code-coverage recorder (ticket W36-01): one `NesCore` frame
//! with the recorder off against the same frame with it armed.
//!
//! `machine_frame.rs` drives `Cpu`/`NesBus` directly, so it never reaches
//! `NesCore::tick` where the recorder's one-branch gate lives; this bench
//! goes through `EmulatorCore::run_frame`. "Off" is the number that must
//! stay within noise of the pre-recorder core (the claim is *zero cost when
//! disarmed*); "on" is what a user pays while recording.
//!
//! Same synthetic in-tree workload as `machine_frame.rs` (rendering on, an
//! APU channel, a RAM loop), for the same reason: `roms/` is gitignored and
//! CI never has it.

use criterion::{criterion_group, criterion_main, Criterion};
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel};
use rf_nes::core::NesCore;

struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

fn workload_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    let prg = &mut rom[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x1E, 0x8D, 0x01, 0x20, // rendering on
        0xA9, 0x9F, 0x8D, 0x00, 0x40, // pulse 1 volume
        0xA9, 0x40, 0x8D, 0x02, 0x40, // timer low
        0xA9, 0x08, 0x8D, 0x03, 0x40, // length load
        0xA9, 0x01, 0x8D, 0x15, 0x40, // enable pulse 1
        0xAD, 0x00, 0x00, // loop: LDA $0000
        0x69, 0x01, // ADC #$01
        0x8D, 0x00, 0x00, // STA $0000
        0xEE, 0x01, 0x00, // INC $0001
        0x4C, 0x18, 0x80, // JMP $8018
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    rom
}

fn bench_frame(c: &mut Criterion, name: &str, armed: bool) {
    let mut core = NesCore::from_ines_bytes(&workload_rom()).expect("workload rom loads");
    core.set_coverage_recording(armed);
    let input = InputFrame::empty();
    // Two warm-up frames: the first is atypical (reset, register setup).
    for _ in 0..2 {
        core.run_frame(&input, &mut NullSink);
    }
    c.bench_function(name, |b| {
        b.iter(|| core.run_frame(&input, &mut NullSink));
    });
}

fn bench_coverage(c: &mut Criterion) {
    bench_frame(c, "nes_core_frame_recorder_off", false);
    bench_frame(c, "nes_core_frame_recorder_on", true);
}

criterion_group!(benches, bench_coverage);
criterion_main!(benches);
