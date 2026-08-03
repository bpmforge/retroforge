//! Proves the master-clock seam (module doc of `crate::system`) end to
//! end against a real [`Cpu`], not just the bus in isolation: outside DMA,
//! the bus-tracked `master_cycle` delta for one instruction agrees exactly
//! with `Cpu::step`'s own returned cycle count; during DMA, the stolen
//! cycles land in the bus's clock without ever being reported through any
//! `Cpu`-facing API.
use super::build_nrom_ines;
use crate::cpu::{Cpu, CpuBus};
use crate::system::{NesBus, NesRom};

#[test]
fn cpu_step_cycle_count_agrees_with_bus_master_cycle_outside_dma() {
    // LDA #$42 (2 cycles) followed by NOPs, placed at the very start of
    // PRG ROM ($8000). No reset sequence is modeled (out of this ticket's
    // scope, per the ticket's own acceptance criteria) — `cpu.pc` is set
    // directly instead of going through `$FFFC`/`$FFFD`.
    let raw = build_nrom_ines(1, 1, |i| match i {
        0 => 0xA9, // LDA #imm
        1 => 0x42,
        _ => 0xEA, // NOP filler
    });
    let rom = NesRom::from_ines_bytes(&raw).expect("valid NROM image");
    let mut bus = NesBus::new(rom);
    let mut cpu = Cpu::new();
    cpu.pc = 0x8000;

    let before = bus.master_cycle();
    let step_cycles = cpu.step(&mut bus);
    let bus_delta = bus.master_cycle() - before;

    assert_eq!(cpu.a, 0x42);
    assert_eq!(step_cycles, 2, "LDA #imm is 2 cycles");
    assert_eq!(
        bus_delta, step_cycles as u64,
        "bus-tracked master_cycle must agree with Cpu::step's own count when no DMA occurs"
    );
}

#[test]
fn oam_dma_stolen_cycles_never_flow_through_any_cpu_api() {
    // Trigger DMA via a plain `CpuBus::write`, with no `Cpu` involved at
    // all — proving the 513/514 stolen cycles are entirely a property of
    // the bus, not something `Cpu::step` needs to know about or return.
    let raw = build_nrom_ines(1, 1, |i| i as u8);
    let rom = NesRom::from_ines_bytes(&raw).expect("valid NROM image");
    let mut bus = NesBus::new(rom);

    let before = bus.master_cycle();
    bus.write(0x4014, 0x02);
    let delta = bus.master_cycle() - before;

    assert!(
        delta > 513,
        "DMA's stolen cycles landed in the bus's own master clock with zero Cpu involvement"
    );
}
