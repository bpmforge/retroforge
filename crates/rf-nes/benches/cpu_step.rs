//! Criterion benchmark for [`Cpu::step`] (ticket W1-01a acceptance
//! criterion 2). Lives outside `src/` so criterion's use of wall-clock
//! timing doesn't trip `scripts/validate-arch.sh`'s determinism grep,
//! which only scans `crates/rf-nes/src` (the CPU itself stays
//! wall-clock-free either way — only the benchmark harness times it).
//!
//! Run with `cargo bench -p rf-nes`.
//!
//! - `cpu_step_lda_immediate` isolates per-`step` overhead (fetch/decode/
//!   dispatch) using the cheapest official opcode shape (2-cycle
//!   immediate-mode read), resetting `PC` before each iteration so it
//!   always measures the same instruction.
//! - `cpu_step_loop_body` measures one `step` call at a time against a
//!   small self-perpetuating program mixing load/store/RMW/compare/branch/
//!   jump — closer to a real instruction-mix throughput number than a
//!   single repeated opcode.
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use rf_nes::{Cpu, CpuBus};

/// A plain 64 KiB RAM-backed bus — no mapper/mirroring, just enough for the
/// CPU to run a self-contained program (`rf-nes`'s real system bus is a
/// later ticket, W1-02).
struct FlatBus {
    ram: Box<[u8; 65536]>,
}

impl FlatBus {
    fn new() -> Self {
        FlatBus {
            ram: Box::new([0; 65536]),
        }
    }
}

impl CpuBus for FlatBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.ram[addr as usize]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.ram[addr as usize] = value;
    }
}

fn bench_single_instruction(c: &mut Criterion) {
    let mut bus = FlatBus::new();
    bus.ram[0x8000] = 0xA9; // LDA #$42
    bus.ram[0x8001] = 0x42;
    let mut cpu = Cpu::new();

    c.bench_function("cpu_step_lda_immediate", |b| {
        b.iter(|| {
            cpu.pc = 0x8000;
            black_box(cpu.step(&mut bus));
        });
    });
}

fn bench_loop_body(c: &mut Criterion) {
    let mut bus = FlatBus::new();
    // $8000  A9 00        LDA #$00
    // $8002  85 10        STA $10
    // $8004  E6 10  loop: INC $10
    // $8006  A5 10        LDA $10
    // $8008  C9 0A        CMP #$0A
    // $800A  D0 F8        BNE loop
    // $800C  4C 02 80     JMP $8002   (re-enter the loop forever)
    let program: &[u8] = &[
        0xA9, 0x00, 0x85, 0x10, 0xE6, 0x10, 0xA5, 0x10, 0xC9, 0x0A, 0xD0, 0xF8, 0x4C, 0x02, 0x80,
    ];
    bus.ram[0x8000..0x8000 + program.len()].copy_from_slice(program);
    let mut cpu = Cpu::new();
    cpu.pc = 0x8000;

    c.bench_function("cpu_step_loop_body", |b| {
        b.iter(|| {
            black_box(cpu.step(&mut bus));
        });
    });
}

criterion_group!(benches, bench_single_instruction, bench_loop_body);
criterion_main!(benches);
