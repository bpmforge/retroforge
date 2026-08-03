//! Criterion 1, end-to-end: a full 8-dot tile fetch (dots 1-8, scanline 0)
//! actually reads the seeded nametable/attribute/pattern bytes into the
//! fetch latches, and the shift registers reload at dot 9 — not before.
//! `background.rs`'s own inline tests already prove the dot->phase table
//! in isolation; this proves `Ppu::tick` wires that table up to real memory
//! reads.
use super::test_ppu;

#[test]
fn full_tile_fetch_populates_latches_and_reloads_shift_registers_at_dot_9() {
    let mut ppu = test_ppu();
    ppu.write_register(1, 0x08); // PPUMASK bit 3: show background
    ppu.scanline = 0;
    ppu.dot = 0;
    // v=0: coarse X=0, coarse Y=0, fine Y=0, nametable select=0 (NOT
    // v=0x2000 — that's the *PPU-bus address* of NT0, a different space;
    // as a `v` value 0x2000 would set fine Y to 2, not 0).
    ppu.v = 0x0000;

    ppu.mem_write(0x2000, 0x05); // nametable byte: tile index 5 (NT address = 0x2000 | (v & 0xFFF))
    ppu.mem_write(0x23C0, 0b0000_0001); // attribute byte: quadrant(0,0) = 1
                                        // pattern-low/high addr = (base=0) | (tile 5 << 4) | fine Y (0) [+ 0x08 for high]
    ppu.chr[0x50] = 0b1010_1010;
    ppu.chr[0x58] = 0b0101_0101;

    // tick() call k processes dot (k-1); 9 calls process dots 0..=8.
    for _ in 0..9 {
        ppu.tick();
    }
    assert_eq!(ppu.nt_latch, 0x05, "NT byte fetched at dot 2");
    assert_eq!(ppu.at_latch, 1, "AT quadrant bits fetched at dot 4");
    assert_eq!(ppu.pt_lo_latch, 0b1010_1010, "pattern low fetched at dot 6");
    assert_eq!(
        ppu.pt_hi_latch, 0b0101_0101,
        "pattern high fetched at dot 8"
    );
    assert_eq!(
        ppu.v & 0x001F,
        1,
        "coarse X incremented once, right after the dot-8 pattern-high fetch"
    );
    assert_eq!(
        ppu.bg_pattern_shift_lo & 0xFF,
        0,
        "reload happens at dot 9, which hasn't been processed yet"
    );

    ppu.tick(); // processes dot 9: reload
    assert_eq!(ppu.bg_pattern_shift_lo & 0xFF, 0b1010_1010);
    assert_eq!(ppu.bg_pattern_shift_hi & 0xFF, 0b0101_0101);
    assert_eq!(
        ppu.bg_attr_shift_lo & 0xFF,
        0xFF,
        "at_latch bit0 set -> attribute-plane-0 shift register filled with 1s"
    );
    assert_eq!(
        ppu.bg_attr_shift_hi & 0xFF,
        0x00,
        "at_latch bit1 clear -> attribute-plane-1 shift register filled with 0s"
    );
}

#[test]
fn no_fetch_activity_while_rendering_is_disabled() {
    let mut ppu = test_ppu();
    // PPUMASK left at 0: background and sprites both off.
    ppu.scanline = 0;
    ppu.dot = 0;
    ppu.v = 0x0000;
    ppu.mem_write(0x2000, 0x05);

    for _ in 0..20 {
        ppu.tick();
    }
    assert_eq!(
        ppu.nt_latch, 0,
        "no fetch ever ran, latch stays at its reset value"
    );
    assert_eq!(ppu.v, 0x0000, "v never advances without rendering enabled");
}
