//! Whole-machine milliseconds per frame (ticket W2-09; NFR-002).
//!
//! ## Why this bench had to exist
//!
//! NFR-002 states a budget in **ms per frame for the machine** (NES ≤ 2 ms),
//! and `docs/TESTING.md` §9 says the nightly gate compares "criterion
//! benches per core (`ms/frame`, Accuracy config, fixed replay workload)"
//! against `benches/baseline.json`. Before this, the closest thing in the
//! tree was `event_emission.rs`'s `frame_tick_event_mask_none`, which ticks
//! **the PPU alone** through one 89,342-dot frame — a real number, and a
//! useful one for the event-mask work it was written for, but a *lower
//! bound* on the machine's cost, not the machine's cost. Putting NFR-002's
//! budget on it would have been an overstatement dressed as a measurement.
//!
//! This runs what the budget is actually about: CPU stepping the real bus,
//! which ticks the PPU three dots and the APU once per cycle, for exactly
//! one frame, draining video and audio the way a host does.
//!
//! ## Accuracy config, and (since ticket W3-07b) the other one
//!
//! §9 says "Accuracy config", and `machine_frame_accuracy` is the number
//! the nightly gate compares against `benches/baseline.json`. W3-07b added
//! the compatibility catch-up scheduler, so there is now a second path, and
//! two more cases measure it — not to gate on, but because the ticket's
//! third acceptance criterion says the scheduler "may not be paid for out
//! of the accuracy mode", and a claim about what a switch costs and buys
//! should be re-runnable rather than a number somebody once wrote down.
//!
//! The two compatibility cases are deliberately the two ENDS of the range,
//! because a single one would misrepresent it. The scheduler skips dots the
//! PPU would have processed to no effect, and how many of those there are
//! depends entirely on whether rendering is on: 8% of dots with rendering
//! enabled against ~31% with it disabled (measured over
//! `mmc3_test_2`/`cpu_timing_test6`/synthetic workloads; see
//! `docs/TESTING.md`). Measuring only the rendering-off case — which is how
//! most of the blargg suite runs — would overstate the win by roughly four
//! times for anyone actually playing a game.
//!
//! ## The workload is synthetic and in-tree, on purpose
//!
//! `roms/` is gitignored (NFR-006) and CI never has it, so a bench built on
//! a fetched ROM could not run in the nightly gate it exists to feed. This
//! builds a tiny NROM image that keeps the CPU, PPU and APU all busy —
//! rendering enabled, a running APU channel, a loop that touches RAM — so
//! the number moves when any of the three gets slower.

use criterion::{criterion_group, criterion_main, Criterion};
use rf_core_api::{CoreEvent, CoreSink, PpuPixel};
use rf_nes::{Cpu, NesBus};

/// Discards output so the sink never becomes part of what is measured —
/// but the DRAIN stays inside the timed region, because a host pays it
/// every frame and because `Ppu::drain`'s own doc warns that skipping it
/// grows the completed-scanline queue instead (the trap
/// `event_emission.rs` documents having fallen into).
struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

/// A minimal NROM image whose reset vector lands on a loop that enables
/// rendering, starts an APU square wave, and then works RAM forever.
fn workload_rom() -> Vec<u8> {
    workload_rom_with_rendering(true)
}

/// As [`workload_rom`], but optionally leaving rendering **off** — the
/// other end of the catch-up scheduler's range (ticket W3-07b). Everything
/// else about the workload is identical, so the two numbers differ in the
/// one variable being studied.
fn workload_rom_with_rendering(rendering: bool) -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1; // 16 KiB PRG
    rom[5] = 1; // 8 KiB CHR
    rom[6] = 0; // mapper 0

    let prg = &mut rom[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x1E, // LDA #$1E   ; background + sprites, no left-column clip (patched below)
        0x8D, 0x01, 0x20, // STA $2001  ; rendering on -> the PPU does real work
        0xA9, 0x9F, // LDA #$9F   ; duty 2, constant volume 15
        0x8D, 0x00, 0x40, // STA $4000
        0xA9, 0x40, // LDA #$40
        0x8D, 0x02, 0x40, // STA $4002  ; timer low
        0xA9, 0x08, // LDA #$08
        0x8D, 0x03, 0x40, // STA $4003  ; length load -> channel running
        0xA9, 0x01, // LDA #$01
        0x8D, 0x15, 0x40, // STA $4015  ; enable pulse 1
        // Loop: read/modify/write RAM so the CPU is doing bus traffic
        // rather than spinning on registers alone.
        0xAD, 0x00, 0x00, // LDA $0000
        0x69, 0x01, // ADC #$01
        0x8D, 0x00, 0x00, // STA $0000
        0xEE, 0x01, 0x00, // INC $0001
        0x4C, 0x18, 0x80, // JMP $8018  ; back to the LDA above
    ];
    prg[..code.len()].copy_from_slice(code);
    if !rendering {
        prg[1] = 0x00; // the LDA's operand: $2001 <- 0, rendering off
    }
    prg[0x3FFC] = 0x00; // reset vector -> $8000
    prg[0x3FFD] = 0x80;
    rom
}

/// Time one steady-state frame of `rom` under the given config.
fn bench_one(c: &mut Criterion, name: &str, rom: &[u8], accuracy: bool) {
    c.bench_function(name, |b| {
        b.iter_batched(
            || {
                let mut bus = NesBus::from_ines_bytes(rom).expect("workload rom loads");
                bus.set_accuracy_mode(accuracy);
                let mut cpu = Cpu::power_on(&mut bus);
                let start = bus.frame_count();
                while bus.frame_count() < start + 2 {
                    cpu.step(&mut bus);
                    bus.drain_video(&mut NullSink);
                    bus.drain_audio(&mut NullSink);
                }
                (cpu, bus)
            },
            |(mut cpu, mut bus)| {
                let start = bus.frame_count();
                while bus.frame_count() == start {
                    cpu.step(&mut bus);
                    bus.drain_video(&mut NullSink);
                    bus.drain_audio(&mut NullSink);
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_machine_frame(c: &mut Criterion) {
    let rom = workload_rom();
    let rom_no_render = workload_rom_with_rendering(false);

    // Ticket W3-07b: the same workload under both schedulers, at both ends
    // of the range the scheduler's win depends on.
    bench_one(c, "machine_frame_compat_rendering_on", &rom, false);
    bench_one(
        c,
        "machine_frame_accuracy_rendering_off",
        &rom_no_render,
        true,
    );
    bench_one(
        c,
        "machine_frame_compat_rendering_off",
        &rom_no_render,
        false,
    );

    c.bench_function("machine_frame_accuracy", |b| {
        b.iter_batched(
            || {
                let mut bus = NesBus::from_ines_bytes(&rom).expect("workload rom loads");
                let mut cpu = Cpu::power_on(&mut bus);
                // Two frames of warm-up: the first is atypical (reset,
                // register setup), and the timed frame should be a
                // steady-state one.
                let start = bus.frame_count();
                while bus.frame_count() < start + 2 {
                    cpu.step(&mut bus);
                    bus.drain_video(&mut NullSink);
                    bus.drain_audio(&mut NullSink);
                }
                (cpu, bus)
            },
            |(mut cpu, mut bus)| {
                let start = bus.frame_count();
                while bus.frame_count() == start {
                    cpu.step(&mut bus);
                    bus.drain_video(&mut NullSink);
                    bus.drain_audio(&mut NullSink);
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, bench_machine_frame);
criterion_main!(benches);
