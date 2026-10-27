//! Ticket W41-02: ROM-read overrides on the SNES bus. Overrides key on the
//! 24-bit address the CPU asked for, so mirrors of one ROM byte are distinct
//! addresses (as on the Game Genie, which compares the cartridge-bus
//! address).

use crate::bus::SnesBus;
use crate::cpu::CpuBus;
use rf_cart::SnesMapMode;
use rf_core_api::{RomOverride, MAX_ROM_OVERRIDES};

/// 64 KiB LoROM, bytes recognisable per offset.
fn bus() -> SnesBus {
    let mut rom = vec![0u8; 64 * 1024];
    rom[0] = 0xA1; // $00:8000 and its mirrors
    rom[0x8000] = 0xB2; // $01:8000
    SnesBus::new(rom, 0, SnesMapMode::LoRom)
}

fn ov(addr: u32, value: u8, compare: Option<u8>) -> RomOverride {
    RomOverride {
        addr,
        value,
        compare,
    }
}

fn install(b: &mut SnesBus, list: &[RomOverride]) -> bool {
    rf_core_api::install_rom_overrides(&mut b.rom_overrides, list)
}

#[test]
fn an_override_replaces_the_rom_byte_at_exactly_its_cpu_address() {
    let mut b = bus();
    assert!(install(&mut b, &[ov(0x00_8000, 0x99, None)]));
    assert_eq!(b.read(0x00_8000), 0x99);
    assert_eq!(
        b.read(0x80_8000),
        0xA1,
        "the fast-ROM mirror is a different address and is not overridden"
    );
    assert_eq!(b.read(0x01_8000), 0xB2, "other ROM bytes untouched");
    assert!(install(&mut b, &[]));
    assert_eq!(b.read(0x00_8000), 0xA1, "empty list restores the byte");
}

#[test]
fn compare_gates_on_the_rom_byte() {
    let mut b = bus();
    assert!(install(&mut b, &[ov(0x00_8000, 0x99, Some(0xA1))]));
    assert_eq!(b.read(0x00_8000), 0x99);
    assert!(install(&mut b, &[ov(0x00_8000, 0x99, Some(0xA0))]));
    assert_eq!(b.read(0x00_8000), 0xA1);
}

#[test]
fn peek_agrees_with_read() {
    let mut b = bus();
    assert!(install(&mut b, &[ov(0x00_8000, 0x99, None)]));
    assert_eq!(b.peek(0x00_8000), 0x99);
    assert_eq!(b.peek(0x80_8000), 0xA1);
}

#[test]
fn the_limit_is_enforced_by_the_shared_installer() {
    let mut b = bus();
    assert!(install(&mut b, &[ov(0x00_8000, 0x99, None)]));
    let many: Vec<_> = (0..=MAX_ROM_OVERRIDES as u32)
        .map(|i| ov(0x00_8100 + i, 1, None))
        .collect();
    assert!(!install(&mut b, &many));
    assert_eq!(b.read(0x00_8000), 0x99, "refused install changed nothing");
}

#[test]
fn a_dma_from_rom_sees_the_override() {
    // A-bus DMA source reads go through `CpuBus::read`, so they hit the
    // override too (the Game Genie sits on the cartridge bus).
    let mut b = bus();
    assert!(install(&mut b, &[ov(0x00_8000, 0x99, None)]));
    b.write(0x00_2181, 0x00);
    b.write(0x00_2182, 0x00);
    b.write(0x00_2183, 0x01); // WRAM port -> $1:0000
    b.write(0x00_4300, 0x00); // one register, A->B, incrementing
    b.write(0x00_4301, 0x80); // $2180
    b.write(0x00_4302, 0x00);
    b.write(0x00_4303, 0x80);
    b.write(0x00_4304, 0x00); // source $00:8000
    b.write(0x00_4305, 0x01);
    b.write(0x00_4306, 0x00);
    b.write(0x00_420B, 0x01);
    b.service_dma();
    assert_eq!(b.wram[0x1_0000], 0x99);
}
