//! Acceptance criterion 2: "NROM loads via rf-cart". Builds synthetic
//! iNES images the same way `crates/rf-cart/src/nes.rs`'s own tests do,
//! then proves `NesBus`/`NesRom` slice and map them correctly — including
//! the "16 KiB PRG mirrors into both halves, 32 KiB is mapped straight
//! through" rule ([nesdev.org/wiki/NROM](https://www.nesdev.org/wiki/NROM)).
use super::build_nrom_ines;
use crate::cpu::CpuBus;
use crate::system::{NesBus, NesLoadError, NesRom};

#[test]
fn sixteen_kib_prg_mirrors_into_both_halves() {
    let raw = build_nrom_ines(1, 1, |i| i as u8);
    let mut bus = NesBus::from_ines_bytes(&raw).expect("valid 16 KiB NROM image");

    assert_eq!(bus.read(0x8000), 0x00);
    assert_eq!(bus.read(0xC000), 0x00, "second half must mirror the first");
    assert_eq!(bus.read(0x8001), 0x01);
    assert_eq!(bus.read(0xC001), 0x01);
    // Last byte of the 16 KiB bank (index 0x3FFF -> byte 0xFF, wrapping
    // u8) shows up at both $BFFF and the reset vector location $FFFF.
    assert_eq!(bus.read(0xBFFF), 0x3FFFu16 as u8);
    assert_eq!(bus.read(0xFFFF), 0x3FFFu16 as u8);
}

#[test]
fn thirty_two_kib_halves_are_independently_addressable() {
    // Distinguish the two halves with a fill function that differs across
    // the 16 KiB boundary, rather than relying on the wrapping `i as u8`
    // pattern (which repeats every 256 bytes and can't tell "mirrored"
    // from "coincidentally equal" at bank granularity).
    let raw = build_nrom_ines(2, 1, |i| if i < 16 * 1024 { 0xAA } else { 0xBB });
    let mut bus = NesBus::from_ines_bytes(&raw).expect("valid 32 KiB NROM image");
    assert_eq!(bus.read(0x8000), 0xAA, "first half ($8000) is bank 0");
    assert_eq!(
        bus.read(0xC000),
        0xBB,
        "second half ($C000) is bank 1, not a mirror"
    );
}

#[test]
fn sixteen_kib_halves_are_identical_not_independently_addressable() {
    let raw = build_nrom_ines(1, 1, |i| if i < 8 * 1024 { 0xAA } else { 0xBB });
    let mut bus = NesBus::from_ines_bytes(&raw).expect("valid 16 KiB NROM image");
    assert_eq!(bus.read(0x8000), 0xAA);
    assert_eq!(
        bus.read(0xC000),
        0xAA,
        "16 KiB image: $C000 mirrors $8000's bank, not bank-independent"
    );
    // Offset 0x2000 into the 16 KiB bank (past the 8 KiB 0xAA/0xBB split);
    // $A000 and its mirror $E000 (+0x4000) must both land here too.
    assert_eq!(bus.read(0xA000), 0xBB);
    assert_eq!(bus.read(0xE000), 0xBB);
}

#[test]
fn unimplemented_mapper_is_rejected_even_though_rf_cart_parses_its_header() {
    // Mapper 1 (MMC1) is in rf-cart's SUPPORTED_MAPPERS (header parses
    // fine) but rf-nes implements only mapper 0 in this ticket.
    let mut raw = build_nrom_ines(1, 1, |i| i as u8);
    raw[6] = 0x10; // flags6 high nibble = mapper low nibble = 1
    let err = NesRom::from_ines_bytes(&raw).unwrap_err();
    assert_eq!(err, NesLoadError::UnimplementedMapper(1));
}

#[test]
fn chr_rom_is_sliced_after_prg_rom() {
    let raw = build_nrom_ines(1, 1, |i| i as u8);
    let rom = NesRom::from_ines_bytes(&raw).expect("valid NROM image");
    assert_eq!(rom.prg_rom().len(), 16 * 1024);
    assert_eq!(rom.chr_rom().len(), 8 * 1024);
    assert!(!rom.chr_is_ram());
}

#[test]
fn zero_chr_rom_size_means_chr_ram() {
    let raw = build_nrom_ines(1, 0, |i| i as u8);
    let rom = NesRom::from_ines_bytes(&raw).expect("valid NROM image with CHR RAM");
    assert!(rom.chr_is_ram());
    assert!(
        !rom.chr_rom().is_empty(),
        "CHR RAM must still have a backing store"
    );
}
