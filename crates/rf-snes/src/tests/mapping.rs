//! Address-mapping tests (FR-CORE-035).

use rf_cart::SnesMapMode::{HiRom, LoRom};

use crate::mapping::{map, Target, WRAM_LEN};

const ROM_32K: usize = 32 * 1024;
const ROM_1M: usize = 1024 * 1024;

fn rom_at(mode: rf_cart::SnesMapMode, bank: u8, offset: u16, rom_len: usize) -> usize {
    match map(mode, bank, offset, rom_len, 0) {
        Target::Rom(i) => i,
        other => panic!("expected ROM at {bank:02X}:{offset:04X}, got {other:?}"),
    }
}

/// The two aliases the mirror-map fixture names — asserted directly.
///
/// These are the exact pairs FORMAT.md documents, and the LoROM one is
/// the check that was silently vacuous in the ROM itself until W6-02a.
#[test]
fn the_documented_bank_aliases_resolve_to_the_same_rom_byte() {
    // LoROM: $00:8000 is $80:8000.
    assert_eq!(
        rom_at(LoRom, 0x00, 0x8000, ROM_32K),
        rom_at(LoRom, 0x80, 0x8000, ROM_32K),
        "LoROM must mirror ROM into the low banks"
    );
    // HiROM: $40:8000 is $C0:8000.
    assert_eq!(
        rom_at(HiRom, 0x40, 0x8000, ROM_1M),
        rom_at(HiRom, 0xC0, 0x8000, ROM_1M),
        "HiROM must mirror ROM into the low banks"
    );
}

/// The alias test above would pass if EVERY address mapped to ROM byte 0.
/// This makes the mapping prove it distinguishes addresses at all.
#[test]
fn different_banks_reach_different_rom_bytes() {
    assert_eq!(rom_at(LoRom, 0x00, 0x8000, ROM_1M), 0x0_0000);
    assert_eq!(rom_at(LoRom, 0x01, 0x8000, ROM_1M), 0x0_8000);
    assert_eq!(rom_at(LoRom, 0x02, 0x8000, ROM_1M), 0x1_0000);
    assert_eq!(rom_at(LoRom, 0x00, 0xFFFF, ROM_1M), 0x0_7FFF);

    assert_eq!(rom_at(HiRom, 0xC0, 0x0000, ROM_1M), 0x0_0000);
    assert_eq!(rom_at(HiRom, 0xC0, 0xFFFF, ROM_1M), 0x0_FFFF);
    assert_eq!(rom_at(HiRom, 0xC1, 0x0000, ROM_1M), 0x1_0000);
}

/// An undersized ROM repeats rather than reading nothing — and the reset
/// vector of the 32 KiB fixture depends on it.
#[test]
fn undersized_roms_mirror_and_the_reset_vector_resolves() {
    // Bank $01 is past the end of a 32 KiB LoROM; it wraps to the start.
    assert_eq!(rom_at(LoRom, 0x01, 0x8000, ROM_32K), 0);
    // The reset vector at $00:FFFC lands at file offset $7FFC, which is
    // where a 32 KiB LoROM keeps it.
    assert_eq!(rom_at(LoRom, 0x00, 0xFFFC, ROM_32K), 0x7FFC);
    // HiROM keeps its vectors at $FFFC of the first 64 KiB.
    assert_eq!(rom_at(HiRom, 0x00, 0xFFFC, 64 * 1024), 0xFFFC);
}

#[test]
fn wram_banks_cover_the_full_128k_and_mirror_into_system_banks() {
    assert_eq!(map(LoRom, 0x7E, 0x0000, ROM_1M, 0), Target::Wram(0));
    assert_eq!(map(LoRom, 0x7E, 0xFFFF, ROM_1M, 0), Target::Wram(0xFFFF));
    assert_eq!(map(LoRom, 0x7F, 0x0000, ROM_1M, 0), Target::Wram(0x1_0000));
    assert_eq!(
        map(LoRom, 0x7F, 0xFFFF, ROM_1M, 0),
        Target::Wram(WRAM_LEN - 1),
        "banks $7E-$7F must cover WRAM exactly, with no gap and no overrun"
    );

    // The low 8 KiB is visible from every system bank — the alias the
    // fixture's check 2 exercises.
    for bank in [0x00u8, 0x01, 0x3F, 0x80, 0xBF] {
        assert_eq!(
            map(LoRom, bank, 0x1234, ROM_1M, 0),
            Target::Wram(0x1234),
            "bank {bank:02X} must mirror WRAM's low 8 KiB"
        );
    }
    // ...but only the low 8 KiB.
    assert_ne!(
        map(LoRom, 0x00, 0x2000, ROM_1M, 0),
        Target::Wram(0x2000),
        "$2000 is registers, not WRAM"
    );
}

#[test]
fn the_register_window_is_2000_to_5fff_in_system_banks_only() {
    for offset in [0x2000u16, 0x2100, 0x4016, 0x4200, 0x420B, 0x5FFF] {
        assert_eq!(
            map(LoRom, 0x00, offset, ROM_1M, 0),
            Target::Register(offset)
        );
        assert_eq!(
            map(HiRom, 0x80, offset, ROM_1M, 0),
            Target::Register(offset)
        );
    }
    // Banks $40-$7D are cartridge, not registers — a mapping that put a
    // register window there would shadow ROM.
    assert!(matches!(
        map(HiRom, 0x40, 0x2100, ROM_1M, 0),
        Target::Rom(_)
    ));
}

#[test]
fn save_ram_lands_where_each_map_puts_it_and_is_open_when_absent() {
    // HiROM: $20-$3F:$6000-$7FFF.
    assert!(matches!(
        map(HiRom, 0x20, 0x6000, ROM_1M, 8192),
        Target::Sram(_)
    ));
    // LoROM: banks $70-$7D below $8000.
    assert!(matches!(
        map(LoRom, 0x70, 0x0000, ROM_1M, 8192),
        Target::Sram(_)
    ));
    // With no save RAM, those windows must not alias onto something else.
    assert_eq!(map(HiRom, 0x20, 0x6000, ROM_1M, 0), Target::Open);
    assert_eq!(map(LoRom, 0x00, 0x6000, ROM_1M, 0), Target::Open);
}

/// WRAM banks must win over the cartridge range they sit inside.
///
/// `$7E`/`$7F` fall within `$40-$7D`-adjacent territory, and an ordering
/// mistake would map work RAM to ROM — which boots, runs, and corrupts
/// everything quietly.
#[test]
fn wram_banks_are_not_captured_by_the_cartridge_range() {
    for bank in [0x7Eu8, 0x7F] {
        for offset in [0x0000u16, 0x7FFF, 0x8000, 0xFFFF] {
            assert!(
                matches!(map(LoRom, bank, offset, ROM_1M, 8192), Target::Wram(_)),
                "LoROM {bank:02X}:{offset:04X} must be WRAM"
            );
            assert!(
                matches!(map(HiRom, bank, offset, ROM_1M, 8192), Target::Wram(_)),
                "HiROM {bank:02X}:{offset:04X} must be WRAM"
            );
        }
    }
}

/// Every address in the whole space maps to something, in both modes,
/// without panicking and without an out-of-range index.
#[test]
fn every_address_maps_within_bounds() {
    for mode in [LoRom, HiRom] {
        for bank in 0..=0xFFu32 {
            // Sampling offsets rather than all 65536 keeps this a unit
            // test; the boundaries that matter are covered exactly above.
            for offset in (0..=0xFFFFu32).step_by(0x40) {
                let addr_bank = bank as u8;
                let off = offset as u16;
                match map(mode, addr_bank, off, ROM_32K, 8192) {
                    Target::Rom(i) => assert!(i < ROM_32K, "{addr_bank:02X}:{off:04X}"),
                    Target::Wram(i) => assert!(i < WRAM_LEN, "{addr_bank:02X}:{off:04X}"),
                    Target::Sram(i) => assert!(i < 8192, "{addr_bank:02X}:{off:04X}"),
                    Target::Register(_) | Target::Open => {}
                }
            }
        }
    }
}
