//! MDMA tests (ticket W6-02a).
//!
//! Scope is "sufficient to boot libSFX fixture ROMs"; HDMA and the
//! contention edge cases are W7 per EMULATION_CORES.md §3.2. What these
//! assert is that the transfer MOVES REAL BYTES — a stub that cleared
//! `$420B` and copied nothing would boot the same ROMs and report the
//! same success.

use crate::bus::SnesBus;
use crate::cpu::CpuBus;
use crate::dma::{CYCLES_PER_BYTE, CYCLES_PER_CHANNEL};
use rf_cart::SnesMapMode;

/// A bus whose `$2100`-block registers are backed by observable storage.
///
/// The PPU is W6-03a, so writes to `$2118` currently go nowhere. To prove
/// DMA actually transfers, the test drives DMA in the other direction —
/// WRAM to WRAM via the WRAM port — which uses only machinery this ticket
/// owns.
fn bus() -> SnesBus {
    SnesBus::new(vec![0; 32 * 1024], 0, SnesMapMode::LoRom)
}

/// Set up channel 0 and fire it.
fn transfer(b: &mut SnesBus, control: u8, b_addr: u8, a_addr: u32, count: u16) -> u64 {
    b.write(0x00_4300, control);
    b.write(0x00_4301, b_addr);
    b.write(0x00_4302, a_addr as u8);
    b.write(0x00_4303, (a_addr >> 8) as u8);
    b.write(0x00_4304, (a_addr >> 16) as u8);
    b.write(0x00_4305, count as u8);
    b.write(0x00_4306, (count >> 8) as u8);
    b.write(0x00_420B, 0x01);
    b.service_dma()
}

#[test]
fn dma_moves_real_bytes_through_the_wram_port() {
    let mut b = bus();
    // Source: WRAM low mirror, reachable at $00:0100.
    for (i, v) in [0x11u8, 0x22, 0x33, 0x44].iter().enumerate() {
        b.wram[0x100 + i] = *v;
    }
    // Destination: the WRAM port pointed at $1:0000, well away from the
    // source so an overlap cannot fake a pass.
    b.write(0x00_2181, 0x00);
    b.write(0x00_2182, 0x00);
    b.write(0x00_2183, 0x01);

    // Pattern 0 (single byte to one register), A-bus incrementing,
    // writing to $2180 (WMDATA).
    let cycles = transfer(&mut b, 0x00, 0x80, 0x00_0100, 4);

    assert_eq!(
        &b.wram[0x1_0000..0x1_0004],
        &[0x11, 0x22, 0x33, 0x44],
        "DMA must transfer the actual bytes, in order"
    );
    assert_eq!(
        cycles,
        CYCLES_PER_CHANNEL + 4 * CYCLES_PER_BYTE,
        "8 master cycles per byte plus channel setup"
    );
}

#[test]
fn a_fixed_source_address_repeats_the_same_byte() {
    let mut b = bus();
    b.wram[0x200] = 0x7E;
    b.write(0x00_2181, 0x00);
    b.write(0x00_2182, 0x00);
    b.write(0x00_2183, 0x01);
    // Bit 3 set = fixed A-bus address.
    transfer(&mut b, 0x08, 0x80, 0x00_0200, 3);
    assert_eq!(&b.wram[0x1_0000..0x1_0003], &[0x7E, 0x7E, 0x7E]);
}

#[test]
fn a_decrementing_source_walks_backwards() {
    let mut b = bus();
    for (i, v) in [0xAAu8, 0xBB, 0xCC].iter().enumerate() {
        b.wram[0x300 + i] = *v;
    }
    b.write(0x00_2181, 0x00);
    b.write(0x00_2182, 0x00);
    b.write(0x00_2183, 0x01);
    // Bit 4 set = decrement.
    transfer(&mut b, 0x10, 0x80, 0x00_0302, 3);
    assert_eq!(&b.wram[0x1_0000..0x1_0003], &[0xCC, 0xBB, 0xAA]);
}

/// Firing no channels must cost nothing and move nothing — the guard
/// against `service_dma` running channel 0 unconditionally.
#[test]
fn an_empty_enable_mask_does_nothing() {
    let mut b = bus();
    b.wram[0x400] = 0x99;
    b.write(0x00_420B, 0x00);
    assert_eq!(b.service_dma(), 0);
    assert_eq!(b.wram[0x1_0000], 0, "nothing transferred");
}

/// `$420B` is edge-triggered: servicing it once must not leave it armed.
#[test]
fn dma_does_not_refire_without_a_new_trigger() {
    let mut b = bus();
    b.wram[0x500] = 0x42;
    b.write(0x00_2181, 0x00);
    b.write(0x00_2182, 0x00);
    b.write(0x00_2183, 0x01);
    transfer(&mut b, 0x00, 0x80, 0x00_0500, 1);
    let after_first = b.service_dma();
    assert_eq!(
        after_first, 0,
        "a second service with no write must be free"
    );
}

#[test]
fn the_transfer_patterns_have_the_documented_shapes() {
    use crate::dma::Channel;
    let pattern = |bits: u8| {
        Channel {
            control: bits,
            ..Channel::default()
        }
        .pattern()
        .to_vec()
    };
    assert_eq!(pattern(0), vec![0]);
    assert_eq!(pattern(1), vec![0, 1]);
    assert_eq!(pattern(2), vec![0, 0]);
    assert_eq!(pattern(3), vec![0, 0, 1, 1]);
    assert_eq!(pattern(4), vec![0, 1, 2, 3]);
    assert_eq!(pattern(6), pattern(2), "mode 6 mirrors mode 2");
    assert_eq!(pattern(7), pattern(3), "mode 7 mirrors mode 3");
}
