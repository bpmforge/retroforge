//! Acceptance criterion 1: "memory map per nesdev (mirrors, PPU/APU regs
//! stubbed as needed)". nesdev.org/wiki/CPU_memory_map: `$0000-$07FF` is
//! mirrored every `$0800` through `$1FFF`; `$2000-$2007` (PPU registers)
//! is mirrored every `$0008` through `$3FFF`.
use super::bus_with_pattern_rom;
use crate::cpu::CpuBus;

#[test]
fn ram_write_is_visible_at_all_three_mirrors() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x0000, 0x42);
    assert_eq!(bus.read(0x0800), 0x42);
    assert_eq!(bus.read(0x1000), 0x42);
    assert_eq!(bus.read(0x1800), 0x42);
}

#[test]
fn ram_mirror_write_is_visible_at_base_address() {
    // Mirroring is symmetric: a write through a mirror lands on the same
    // underlying byte the base address sees.
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x1801, 0x99);
    assert_eq!(bus.read(0x0001), 0x99);
    assert_eq!(bus.read(0x0801), 0x99);
    assert_eq!(bus.read(0x1001), 0x99);
}

#[test]
fn ram_mirroring_does_not_bleed_into_adjacent_bytes() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x0000, 0x11);
    bus.write(0x0001, 0x22);
    assert_eq!(bus.read(0x1800), 0x11);
    assert_eq!(bus.read(0x1801), 0x22);
}

/// PPU register mirroring, proven through `OAMADDR`/`OAMDATA` (index 3/4
/// of the 8-register window) since those are the only two registers this
/// ticket's stub gives real, observable side effects — a plain
/// open-bus-passthrough register wouldn't distinguish "mirrored to the
/// same register" from "mirrored to nothing in particular".
#[test]
fn ppu_register_mirroring_every_8_bytes_through_3fff() {
    let mut bus = bus_with_pattern_rom(1, 1);
    // $2003 (OAMADDR) sets the address; $200C is $2004 (OAMDATA) mirrored
    // once (+8). If mirroring is correct, this writes into OAM at 0x05 and
    // advances OAMADDR, exactly like writing OAMDATA at its base address.
    bus.write(0x2003, 0x05);
    bus.write(0x200C, 0xAB);
    assert_eq!(bus.oam()[0x05], 0xAB);

    // And the far end of the mirrored range ($3FFC = $2004 + 0x1FF8,
    // still index 4 mod 8) reaches the very same register.
    bus.write(0x3FFB, 0x10); // $3FFB & 7 == 3 -> OAMADDR = 0x10
    bus.write(0x3FFC, 0xCD); // $3FFC & 7 == 4 -> OAMDATA
    assert_eq!(bus.oam()[0x10], 0xCD);
}

/// Unimplemented PPU registers (everything except OAMADDR/OAMDATA) are an
/// obvious stub: writes are dropped, reads return open bus. This also
/// proves those registers *are* mirrored (both addresses observe the same
/// — unchanged — open-bus latch), without claiming any real PPUCTRL/
/// PPUSTATUS/... behavior.
#[test]
fn unimplemented_ppu_registers_are_open_bus_stubs() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x0000, 0x77); // drives the open-bus latch to 0x77
    assert_eq!(bus.read(0x2000), 0x77); // PPUCTRL: stub, open bus
    assert_eq!(bus.read(0x2801), 0x77); // mirror of $2001 (PPUMASK): same
}

#[test]
fn cartridge_prg_ram_is_readable_and_writable() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x6000, 0xAB);
    bus.write(0x7FFF, 0xCD);
    assert_eq!(bus.read(0x6000), 0xAB);
    assert_eq!(bus.read(0x7FFF), 0xCD);
    // Distinct from PRG ROM ($8000+): writes there have no effect.
    assert_ne!(bus.read(0x8000), 0xAB);
}

#[test]
fn prg_rom_writes_are_ignored_nrom_has_no_mapper_registers() {
    let mut bus = bus_with_pattern_rom(1, 1);
    let before = bus.read(0x8000);
    bus.write(0x8000, !before);
    assert_eq!(bus.read(0x8000), before, "NROM PRG ROM is not writable");
}

#[test]
fn disabled_test_registers_and_unmapped_expansion_are_open_bus() {
    let mut bus = bus_with_pattern_rom(1, 1);
    // A write to a disabled/unmapped region has no storage effect, but
    // real hardware still drives the byte being written onto the shared
    // data bus for that cycle — so a read right afterward, anywhere
    // equally undriven, sees the write's value, not some earlier one.
    bus.write(0x401F, 0xFF); // disabled test register: storage dropped
    assert_eq!(
        bus.read(0x401F),
        0xFF,
        "the write itself drove the open-bus latch"
    );
    assert_eq!(
        bus.read(0x5000),
        0xFF,
        "unmapped $4020-$5FFF: still open bus"
    );

    bus.write(0x0000, 0x5A); // a real RAM write re-drives the latch
    assert_eq!(
        bus.read(0x401F),
        0x5A,
        "open-bus latch tracks whatever was driven most recently"
    );
}
