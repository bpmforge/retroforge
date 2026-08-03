//! Acceptance criterion 3: "OAM DMA cycle-stealing correct"
//! ([nesdev.org/wiki/DMA](https://www.nesdev.org/wiki/DMA)).
//!
//! Both alignments are exercised explicitly, each with a precondition
//! assertion on `master_cycle`'s parity right before triggering the DMA —
//! without that, a setup change could silently make both cases exercise
//! the same branch and this pair would pass vacuously (this project's own
//! RF-L-08 failure mode).
use super::bus_with_pattern_rom;
use crate::cpu::CpuBus;
use crate::system::NesBus;

/// Fill RAM page `page` (`$pp00-$ppFF`) with byte `i` = `i as u8`, a
/// recognizable pattern for the byte-order assertions. Uses the ordinary
/// ticking `CpuBus::write`, so it also advances `master_cycle` by exactly
/// 256 — used deliberately below to land on a known parity.
fn fill_source_page(bus: &mut NesBus, page: u8) {
    for i in 0..256u16 {
        bus.write(((page as u16) << 8) | i, i as u8);
    }
}

#[test]
fn even_start_cycle_costs_513() {
    let mut bus = bus_with_pattern_rom(1, 1);
    fill_source_page(&mut bus, 0x02); // 256 writes: master_cycle 0 -> 256 (even)
    assert_eq!(
        bus.master_cycle() % 2,
        0,
        "precondition: must actually be testing the even-start branch"
    );

    let before = bus.master_cycle();
    bus.write(0x4014, 0x02);
    let stall = bus.last_oam_dma_stall().expect("DMA ran");
    assert_eq!(stall, 513);
    // Total bus-cycle delta is the write's own cycle plus the stall.
    assert_eq!(bus.master_cycle() - before, 1 + 513);
}

#[test]
fn odd_start_cycle_costs_514() {
    let mut bus = bus_with_pattern_rom(1, 1);
    fill_source_page(&mut bus, 0x02); // master_cycle -> 256 (even)
    bus.write(0x0000, 0); // one more tick -> 257 (odd)
    assert_eq!(
        bus.master_cycle() % 2,
        1,
        "precondition: must actually be testing the odd-start branch"
    );

    let before = bus.master_cycle();
    bus.write(0x4014, 0x02);
    let stall = bus.last_oam_dma_stall().expect("DMA ran");
    assert_eq!(stall, 514);
    assert_eq!(bus.master_cycle() - before, 1 + 514);
}

#[test]
fn the_two_alignments_differ_by_exactly_one_cycle() {
    let mut even_bus = bus_with_pattern_rom(1, 1);
    fill_source_page(&mut even_bus, 0x02);
    even_bus.write(0x4014, 0x02);

    let mut odd_bus = bus_with_pattern_rom(1, 1);
    fill_source_page(&mut odd_bus, 0x02);
    odd_bus.write(0x0000, 0);
    odd_bus.write(0x4014, 0x02);

    let even_stall = even_bus.last_oam_dma_stall().unwrap();
    let odd_stall = odd_bus.last_oam_dma_stall().unwrap();
    assert_eq!(odd_stall - even_stall, 1);
}

#[test]
fn all_256_bytes_land_in_oam_in_source_order() {
    let mut bus = bus_with_pattern_rom(1, 1);
    fill_source_page(&mut bus, 0x03);
    bus.write(0x4014, 0x03);

    for i in 0..256usize {
        assert_eq!(bus.oam()[i], i as u8, "OAM byte {i} mismatched");
    }
}

#[test]
fn dma_honors_and_advances_the_current_oam_addr() {
    // nesdev.org/wiki/DMA: "the transfer begins at the current OAM write
    // address". Set OAMADDR to a non-zero value first via $2003, then the
    // DMA's 256 bytes should land starting there and wrap around.
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.write(0x2003, 0x10); // OAMADDR = 0x10
    fill_source_page(&mut bus, 0x04);
    bus.write(0x4014, 0x04);

    assert_eq!(
        bus.oam()[0x10],
        0x00,
        "first DMA byte lands at OAMADDR, not 0"
    );
    assert_eq!(bus.oam()[0x11], 0x01);
    // Wraps around past 0xFF back to 0x00..0x0F.
    assert_eq!(bus.oam()[0x00], (0x100 - 0x10) as u8);
    assert_eq!(bus.oam()[0x0F], 0xFF);
}
