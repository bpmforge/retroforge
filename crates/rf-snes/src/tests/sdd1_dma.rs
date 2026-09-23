//! S-DD1 DMA decompression, end to end through the bus (ticket W19-03).
//!
//! `crate::sdd1`'s own unit tests prove the decompressor against
//! hand-derived expectations in isolation; these prove the WIRING —
//! `SnesBus::run_channel` actually substitutes the decompressed stream for
//! a real general-purpose DMA into VRAM, and only for the channel/address
//! combination fullsnes's "S-DD1 I/O Ports"/"DMA from ROM returns
//! Decompressed Data" sentence describes.

use crate::bus::SnesBus;
use crate::cpu::CpuBus;
use rf_cart::SnesMapMode;

fn bus_with_sdd1(rom: Vec<u8>) -> SnesBus {
    let mut b = SnesBus::new(rom, 0, SnesMapMode::Sdd1);
    b.install_sdd1();
    b
}

/// Point the VRAM port at word address 0 with "increment after high byte"
/// (`$2115` bit7 set) — the shape a word-pattern (`pattern()` control bits
/// `001`) VRAM DMA always uses, so the low/high byte of one word land at
/// the same address before it advances.
fn reset_vram_port(b: &mut SnesBus) {
    b.write(0x00_2115, 0x80);
    b.write(0x00_2116, 0x00);
    b.write(0x00_2117, 0x00);
}

/// Channel 0, word pattern (`[0,1]`, control bit pattern `001`), destined
/// for `$2118`/`$2119` (VRAM data port), `count` bytes from `a`.
fn arm_channel_0_to_vram(b: &mut SnesBus, a: u32, count: u16) {
    b.write(0x00_4300, 0x01); // pattern [0,1], forward
    b.write(0x00_4301, 0x18); // $2118 VMDATAL
    b.write(0x00_4302, a as u8);
    b.write(0x00_4303, (a >> 8) as u8);
    b.write(0x00_4304, (a >> 16) as u8);
    b.write(0x00_4305, count as u8);
    b.write(0x00_4306, (count >> 8) as u8);
}

#[test]
fn dma_through_sdd1_decompresses_into_vram_then_the_transfer_bit_self_clears() {
    // A 64 KiB image: the first 64 bytes are an all-zero S-DD1 compressed
    // block (header `$00` selects the 2bpp mode — `crate::sdd1`'s own
    // `all_zero_stream_decompresses_to_all_zero_bytes_every_mode` test
    // derives, from the SAME fullsnes pseudocode, that an all-zero stream
    // decodes to all-zero output bytes in every mode); offset `$1000` is
    // filled with `$AB`, standing in for ordinary (non-decompressed) ROM
    // content the second half of this test reads raw.
    let mut rom = vec![0u8; 0x1_0000];
    for b in rom.iter_mut().skip(0x1000).take(16) {
        *b = 0xAB;
    }
    let mut bus = bus_with_sdd1(rom);

    // $4800/$4801: arm channel 0 for decompression (fullsnes "S-DD1 I/O
    // Ports": bit0 = DMA channel 0 in both registers). Bank register
    // $4804 is left at its zeroed reset value, selecting megabyte 0 of the
    // $C0-$CF group — file offset 0, where the all-zero block lives.
    bus.write(0x00_4800, 0x01);
    bus.write(0x00_4801, 0x01);

    reset_vram_port(&mut bus);
    arm_channel_0_to_vram(&mut bus, 0xC0_0000, 32); // 16 words

    bus.write(0x00_420B, 0x01);
    bus.service_dma();

    assert_eq!(
        &bus.ppu.vram[0..32],
        [0u8; 32].as_slice(),
        "an all-zero S-DD1 stream must decompress to all-zero VRAM bytes"
    );
    // Fullsnes: "$4801h... automatically cleared after DMA".
    assert_eq!(
        bus.read(0x00_4801),
        0x00,
        "the transfer bit for the channel that just decompressed must self-clear"
    );
    // Fullsnes: "$4800h... unchanged after DMA".
    assert_eq!(
        bus.read(0x00_4800),
        0x01,
        "the enable bit is untouched by a completed transfer"
    );

    // Second transfer, same channel, address now pointed at the raw `$AB`
    // filler: `$4801`'s bit already self-cleared above, so
    // `channel_decompresses(0)` is false and this run must read the
    // cartridge's ROM bytes directly, NOT run the decompressor again —
    // proving the substitution is gated on both enable bits together, not
    // a blanket transform of the whole `$C0-$FF` window.
    arm_channel_0_to_vram(&mut bus, 0xC0_1000, 4); // 2 words
    bus.write(0x00_420B, 0x01);
    bus.service_dma();

    assert_eq!(
        &bus.ppu.vram[32..36],
        [0xAB, 0xAB, 0xAB, 0xAB],
        "with the transfer bit cleared, the same channel must read raw ROM bytes"
    );
}

/// A channel whose `$4800` bit is clear never decompresses even with
/// `$4801` set — both registers must name the channel (module doc).
#[test]
fn channel_not_named_in_4800_never_decompresses_even_if_4801_is_set() {
    let mut rom = vec![0u8; 0x1_0000];
    // If this were (incorrectly) decompressed, the all-zero header would
    // still decode to all-zero bytes — so instead this leaves the source
    // region non-zero, which only a RAW read can reproduce.
    for b in rom.iter_mut().take(4) {
        *b = 0x7E;
    }
    let mut bus = bus_with_sdd1(rom);

    bus.write(0x00_4800, 0x00); // channel 0 NOT named
    bus.write(0x00_4801, 0x01); // transfer bit set anyway

    reset_vram_port(&mut bus);
    arm_channel_0_to_vram(&mut bus, 0xC0_0000, 4);
    bus.write(0x00_420B, 0x01);
    bus.service_dma();

    assert_eq!(&bus.ppu.vram[0..4], [0x7E, 0x7E, 0x7E, 0x7E]);
}

/// The bank register actually selects which megabyte appears at
/// `$C0-$CF` (fullsnes "`$4804h` ROM Bank for `$C00000h-$CFFFFFh`") —
/// checked directly against [`crate::mapping::sdd1_target`] rather than
/// through a DMA, so a mapping regression here fails at the layer that
/// caused it.
#[test]
fn bank_register_selects_which_rom_megabyte_appears_at_c0() {
    let mut rom = vec![0u8; 0x20_0000]; // 2 MiB: two selectable "megabytes"
    rom[0x00_0000] = 0x11;
    rom[0x10_0000] = 0x22;
    let mut bus = bus_with_sdd1(rom);
    bus.write(0x00_4804, 0x00);
    assert_eq!(bus.read(0xC0_0000), 0x11);
    bus.write(0x00_4804, 0x01);
    assert_eq!(bus.read(0xC0_0000), 0x22);
}
