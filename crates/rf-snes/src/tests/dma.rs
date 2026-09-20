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

/// W14-26: `$43x0`-`$43xA` are `(R/W)` per fullsnes ("4200h-437Fh - PPU2
/// and CPU Register Overview / DMA"), not write-only — the CPU can read
/// back exactly what it wrote. Before this ticket, `read_register_pure`
/// had no arm for `0x4300..=0x437F`, so every read there fell through to
/// open bus and silently discarded `Channel`'s state; NHL 95 (USA) uses
/// channel 1's `A1T1`/`A1B1` bytes (`$4312`-`$4314`) as 24-bit-pointer
/// scratch storage via `LDA [dp]` after setting the CPU's direct page to
/// `$4300`, and a wrong readback there fed it a garbage pointer that
/// walked into a hardware register, corrupting X, and ultimately ran the
/// stack pointer into ROM.
#[test]
fn dma_channel_registers_read_back_what_was_written() {
    let mut b = bus();
    // Channel 1's block is $4310-$431F.
    b.write(0x00_4310, 0x81); // DMAPn
    b.write(0x00_4311, 0x22); // BBADn
    b.write(0x00_4312, 0x12); // A1TnL
    b.write(0x00_4313, 0x34); // A1TnH
    b.write(0x00_4314, 0x56); // A1Bn
    b.write(0x00_4315, 0x78); // DASnL
    b.write(0x00_4316, 0x9A); // DASnH
    b.write(0x00_4317, 0xBC); // DASBn
    b.write(0x00_4318, 0xDE); // A2AnL
    b.write(0x00_4319, 0xF0); // A2AnH
    b.write(0x00_431A, 0x55); // NLTRn

    assert_eq!(b.read(0x00_4310), 0x81);
    assert_eq!(b.read(0x00_4311), 0x22);
    assert_eq!(b.read(0x00_4312), 0x12);
    assert_eq!(b.read(0x00_4313), 0x34);
    assert_eq!(b.read(0x00_4314), 0x56);
    assert_eq!(b.read(0x00_4315), 0x78);
    assert_eq!(b.read(0x00_4316), 0x9A);
    assert_eq!(b.read(0x00_4317), 0xBC);
    assert_eq!(b.read(0x00_4318), 0xDE);
    assert_eq!(b.read(0x00_4319), 0xF0);
    assert_eq!(b.read(0x00_431A), 0x55);

    // A different channel's bytes must not alias — this is the exact
    // shape the game's own `LDA [$12]` pointer read depends on: bytes at
    // $4312-$4314 belong to channel 1 only.
    b.write(0x00_4302, 0xAA);
    assert_eq!(b.read(0x00_4312), 0x12, "channel 0's A1TL must not alias");

    // `peek` (the debugger/tool path) must agree with `read` — this is
    // what `title_probe`'s diagnostics rely on.
    assert_eq!(CpuBus::peek(&b, 0x00_4312), 0x12);
    assert_eq!(CpuBus::peek(&b, 0x00_4314), 0x56);

    // Running DMA on an unrelated channel must not disturb the value —
    // the game reads this back long after the write, across whatever
    // else the frame does.
    b.wram[0x600] = 0x01;
    transfer(&mut b, 0x00, 0x80, 0x00_0600, 1);
    assert_eq!(
        b.read(0x00_4312),
        0x12,
        "channel 1's scratch bytes must survive channel 0's own DMA"
    );
}
