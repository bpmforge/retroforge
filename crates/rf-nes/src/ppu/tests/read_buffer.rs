//! `$2007` delayed-read buffer + palette-bypass quirk
//! ([nesdev.org/wiki/PPU_registers](https://www.nesdev.org/wiki/PPU_registers)):
//! "reading from PPUDATA... returns the contents of an internal read
//! buffer... effectively delaying PPUDATA reads by one", except palette
//! addresses, which "return... immediately". Called out by the ticket
//! brief by name as "a classic silent-wrongness source" — tested here
//! independent of any golden frame.
use super::test_ppu;

fn set_v_via_2006(ppu: &mut super::Ppu, addr: u16) {
    ppu.write_register(6, (addr >> 8) as u8);
    ppu.write_register(6, (addr & 0xFF) as u8);
}

#[test]
fn nametable_read_is_delayed_by_one_read() {
    let mut ppu = test_ppu();
    set_v_via_2006(&mut ppu, 0x2005);
    ppu.mem_write(0x2005, 0xAB); // seed the byte the first read should NOT see yet
    ppu.mem_write(0x2006, 0xCD); // the byte the SECOND read should return

    let first = ppu.read_register(7);
    assert_eq!(
        first, 0x00,
        "first read returns the buffer's pre-seeded (empty) value, not 0xAB"
    );
    assert_eq!(ppu.v, 0x2006, "v advanced by 1 (ctrl bit2 clear)");

    let second = ppu.read_register(7);
    assert_eq!(
        second, 0xAB,
        "second read returns what the FIRST read buffered"
    );

    let third = ppu.read_register(7);
    assert_eq!(
        third, 0xCD,
        "third read returns what the second read buffered"
    );
}

#[test]
fn palette_read_bypasses_the_buffer_and_returns_immediately() {
    let mut ppu = test_ppu();
    ppu.palette[0x05] = 0x2A;
    set_v_via_2006(&mut ppu, 0x3F05);

    let value = ppu.read_register(7);
    assert_eq!(
        value & 0x3F,
        0x2A,
        "palette reads return the value immediately, no priming read needed"
    );
}

/// "$2007 DD------ palette" (blargg's `ppu_open_bus` readme): a palette
/// read drives bits 5-0 and takes bits 7-6 from the PPU's OWN decay
/// register — not from the CPU bus's open-bus latch, which is a separate
/// thing (ticket W2-19; the readme's first paragraph says so outright).
#[test]
fn palette_read_top_2_bits_come_from_the_ppu_decay_register() {
    let mut ppu = test_ppu();
    ppu.palette[0x00] = 0x3F;
    // Any PPU-register write sets all eight decay bits; $2006 is the one
    // this helper uses anyway, and $C0 puts 1s exactly in the two bits
    // the palette read must not drive.
    ppu.write_register(0, 0xC0);
    set_v_via_2006(&mut ppu, 0x3F00);
    ppu.write_register(0, 0xC0);
    let value = ppu.read_register(7);
    assert_eq!(
        value,
        0x3F | 0xC0,
        "bits 7-6 read back from the decay register"
    );

    // ...and with those decay bits clear, they read back clear — the
    // anti-vacuity half, since asserting only the $C0 case would pass on
    // an implementation that simply ORed in $C0 unconditionally.
    ppu.write_register(0, 0x00);
    set_v_via_2006(&mut ppu, 0x3F00);
    ppu.write_register(0, 0x00);
    assert_eq!(ppu.read_register(7), 0x3F);
}

/// Test 9 of `ppu_open_bus`: a palette read refreshes bits 5-0 only, so
/// the top two bits keep decaying even under a stream of such reads.
#[test]
fn palette_read_does_not_refresh_the_top_2_decay_bits() {
    let mut ppu = test_ppu();
    ppu.palette[0x00] = 0x3F;
    ppu.write_register(0, 0xFF); // all eight decay bits set, clocks armed

    for _ in 0..80 {
        set_v_via_2006(&mut ppu, 0x3F00);
        let _ = ppu.read_register(7);
        ppu.age_decay_register_for_test();
    }

    assert_eq!(
        ppu.decay & 0xC0,
        0,
        "bits 7-6 must have decayed despite the palette reads"
    );
    assert_eq!(
        ppu.decay & 0x3F,
        0x3F,
        "bits 5-0 were refreshed by every read and must still stand"
    );
}

#[test]
fn palette_read_still_refills_the_buffer_from_the_mirrored_nametable_underneath() {
    // nesdev: "the PPU also performs a normal read from PPU memory at the
    // specified address, 'underneath' the palette data, and the result of
    // this read goes into the read buffer as normal." This module masks
    // the address to the $2xxx nametable range (addr & 0x2FFF) for that
    // underneath read (see `crate::ppu::scroll::read_data`'s doc) — prove
    // the buffer ends up holding THAT byte, observable via the next
    // non-palette read.
    let mut ppu = test_ppu();
    ppu.mem_write(0x2F05, 0x77); // $3F05 & 0x2FFF == $2F05
    ppu.palette[0x05] = 0x2A;
    set_v_via_2006(&mut ppu, 0x3F05);

    let palette_value = ppu.read_register(7);
    assert_eq!(palette_value & 0x3F, 0x2A);

    // v is now 0x3F06 (still a palette address) after the increment; move
    // it to a plain nametable address to observe the buffered value without
    // yet another palette bypass.
    ppu.v = 0x2000;
    let buffered = ppu.read_register(7);
    assert_eq!(
        buffered, 0x77,
        "the palette read's buffer refill used the mirrored nametable byte"
    );
}

#[test]
fn write_2007_increments_v_and_does_not_touch_the_read_buffer() {
    let mut ppu = test_ppu();
    set_v_via_2006(&mut ppu, 0x2010);
    ppu.write_register(7, 0x42);
    assert_eq!(ppu.v, 0x2011);
    assert_eq!(ppu.mem_read(0x2010), 0x42);
}
