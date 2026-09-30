//! Cartridge RAM in the `$6000-$7FFF` window of the system banks, for a
//! LoROM board whose header cannot say what it carries (ticket W14-60).
//!
//! Plain LoROM leaves `$6000-$7FFF` of banks `$00-$3F`/`$80-$BF` unmapped
//! (`map`'s own comment). Two carts in the library run a RAM self-test
//! there first (a write-then-read-back loop that retries until the byte
//! reads back), so on an open-bus window they spin forever. fullsnes
//! "SNES Memory Map" / "SNES Cartridge ROM Header" describe the RAM as a
//! cartridge-side chip whose presence only the board knows; the header's
//! RAM-size byte is the sole declaration, and these headers are absent.
//!
//! The window is write-allocated: a byte reads back what was written, and
//! a byte never written still reads as open bus, which is what an
//! undecoded window returns. That keeps a game that probes the window
//! before using it seeing the same first read either way.

use crate::bus::SnesBus;
use crate::cpu::CpuBus;
use rf_cart::SnesMapMode;

fn bus() -> SnesBus {
    SnesBus::new(vec![0; 32 * 1024], 0, SnesMapMode::LoRom)
}

#[test]
fn an_installed_window_reads_back_what_was_written_in_every_system_bank() {
    let mut b = bus();
    b.install_window_ram();
    for (bank, offset, value) in [
        (0x00u32, 0x6000u32, 0x5A),
        (0x30, 0x7FFF, 0xA5),
        (0x8F, 0x62A5, 0x3C),
    ] {
        b.write((bank << 16) | offset, value);
        assert_eq!(
            b.read((bank << 16) | offset),
            value,
            "{bank:02X}:{offset:04X}"
        );
    }
}

#[test]
fn a_byte_never_written_still_reads_as_open_bus() {
    let mut b = bus();
    b.install_window_ram();
    // Prime the data bus with a known value, then read an untouched cell:
    // it must return that bus value, not a RAM zero.
    b.write(0x00_0010, 0xC3);
    assert_eq!(b.read(0x00_6100), 0xC3);
}

#[test]
fn without_the_install_the_window_stays_open_and_swallows_writes() {
    let mut b = bus();
    b.write(0x00_6000, 0x77);
    b.write(0x00_0010, 0x11);
    assert_eq!(
        b.read(0x00_6000),
        0x11,
        "plain LoROM: open bus, write dropped"
    );
}

#[test]
fn the_window_is_a_window_not_the_rest_of_the_bank() {
    let mut b = bus();
    b.install_window_ram();
    b.write(0x00_6000, 0x99);
    // $8000 is ROM (zero-filled here); $5FFF is a register, not the RAM.
    assert_eq!(b.read(0x00_8000), 0x00);
    assert_ne!(b.read(0x00_5FFF), 0x99);
}
