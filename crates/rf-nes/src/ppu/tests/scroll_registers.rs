//! Criterion 2: `v`/`t`/`x`/`w` semantics for `$2000`/`$2005`/`$2006`/`$2002`.
//!
//! Every expected bit pattern below is hand-computed from the exact
//! pseudocode quoted in `crate::ppu::scroll`'s module doc (itself quoted
//! from [nesdev.org/wiki/PPU_scrolling](https://www.nesdev.org/wiki/PPU_scrolling)),
//! never invented: each test's doc comment shows the arithmetic.
use super::test_ppu;

#[test]
fn write_2000_sets_t_bits_10_and_11_from_value_bits_0_and_1() {
    // t: ...GH.. ........ <- d: ......GH. Start from t=all-ones (except the
    // untouched bits can't be all-ones since t is only 15 bits; use 0x7FFF)
    // to prove ONLY bits 10-11 move, in both directions.
    let mut ppu = test_ppu();
    ppu.t = 0x7FFF;
    ppu.write_register(0, 0x00); // GH=00 -> t bits10-11 cleared
    assert_eq!(ppu.t, 0x7FFF & !0x0C00);

    ppu.t = 0x0000;
    ppu.write_register(0, 0x03); // GH=11 -> t bits10-11 set
    assert_eq!(ppu.t, 0x0C00);
}

#[test]
fn write_2005_first_write_sets_coarse_x_and_fine_x_then_toggles_w() {
    // value = 0b0101_1101 = 0x5D. First write: t[4:0] <- value[7:3] = 0b01011 = 0x0B;
    // x <- value[2:0] = 0b101 = 5.
    let mut ppu = test_ppu();
    assert!(!ppu.w, "w starts cleared");
    ppu.write_register(5, 0x5D);
    assert_eq!(ppu.t & 0x001F, 0x0B, "coarse X = value >> 3");
    assert_eq!(ppu.x, 0x05, "fine x = value & 7");
    assert!(ppu.w, "w toggles to 1 after the first write");
}

#[test]
fn write_2005_second_write_sets_fine_y_and_coarse_y_and_resets_w() {
    // Same byte (0x5D) as a *second* write: t[14:12] <- value[2:0] = 5;
    // t[9:5] <- value[7:3] = 0x0B. Combined with coarse X = 0x0B from the
    // first write (unaffected by the second), t should read 0x516B:
    //   0x5000 (fine Y=5 <<12) | 0x0160 (coarse Y=0x0B <<5) | 0x000B (coarse X)
    // = 0x5000 | 0x0160 | 0x000B = 0x516B.
    let mut ppu = test_ppu();
    ppu.write_register(5, 0x5D); // first write
    ppu.write_register(5, 0x5D); // second write
    assert_eq!(ppu.t, 0x516B);
    assert!(!ppu.w, "w resets to 0 after the second write");
}

#[test]
fn write_2006_first_write_masks_to_6_bits_into_t_high_byte() {
    // First write: t[13:8] <- value & 0x3F, t[14] forced to 0. value=0xFF
    // proves the top 2 bits are masked away: t = (0xFF & 0x3F) << 8 = 0x3F00.
    let mut ppu = test_ppu();
    ppu.write_register(6, 0xFF);
    assert_eq!(ppu.t, 0x3F00);
    assert!(ppu.w);
}

#[test]
fn write_2006_second_write_sets_t_low_byte_and_copies_t_into_v() {
    let mut ppu = test_ppu();
    ppu.write_register(6, 0x3F); // first write: t = 0x3F00
    ppu.write_register(6, 0xAB); // second write: t = 0x3FAB, v <- t
    assert_eq!(ppu.t, 0x3FAB);
    assert_eq!(ppu.v, 0x3FAB, "second $2006 write copies t into v");
    assert!(!ppu.w);
}

#[test]
fn reading_2002_resets_the_w_latch() {
    // nesdev.org/wiki/PPU_registers: reading PPUSTATUS clears w. Prove it
    // by observing that a $2005 write *after* the $2002 read is treated as
    // a first write again (changes x), not a second write (which wouldn't).
    let mut ppu = test_ppu();
    ppu.write_register(5, 0x08); // first write: w -> true, x <- 0
    assert!(ppu.w);
    let _ = ppu.read_register(2, 0x00);
    assert!(!ppu.w, "$2002 read must clear w");

    ppu.write_register(5, 0x5D); // must be treated as a FIRST write again
    assert_eq!(ppu.x, 0x05, "x only changes on a first write");
    assert!(ppu.w, "and toggles back to true");
}

#[test]
fn reading_2002_clears_vblank_but_not_sprite0_or_overflow() {
    let mut ppu = test_ppu();
    ppu.status = 0xE0; // vblank | sprite0hit | overflow, all set
    let result = ppu.read_register(2, 0x00);
    assert_eq!(result & 0xE0, 0xE0, "read returns all three flags as set");
    assert_eq!(ppu.status & 0x80, 0, "vblank cleared by the read");
    assert_eq!(
        ppu.status & 0x60,
        0x60,
        "sprite0hit/overflow untouched by a $2002 read"
    );
}

#[test]
fn reading_2002_low_5_bits_pass_through_open_bus() {
    let mut ppu = test_ppu();
    ppu.status = 0x80;
    let result = ppu.read_register(2, 0b0001_0111);
    assert_eq!(result, 0x80 | 0b0001_0111);
}

#[test]
fn peek_2002_does_not_clear_vblank_or_w() {
    let mut ppu = test_ppu();
    ppu.status = 0x80;
    ppu.w = true;
    let result = ppu.peek_register(2, 0x00);
    assert_eq!(result & 0x80, 0x80);
    assert_eq!(ppu.status & 0x80, 0x80, "peek must not clear vblank");
    assert!(ppu.w, "peek must not clear w");
}

#[test]
fn write_2000_bit2_selects_the_2007_increment_step() {
    let mut ppu = test_ppu();
    ppu.write_register(0, 0x00); // increment by 1
    ppu.v = 0x2000;
    ppu.write_register(7, 0xAA);
    assert_eq!(ppu.v, 0x2001);

    ppu.write_register(0, 0x04); // increment by 32
    ppu.v = 0x2000;
    ppu.write_register(7, 0xAA);
    assert_eq!(ppu.v, 0x2020);
}

#[test]
fn coarse_x_increment_wraps_at_31_and_flips_horizontal_nametable() {
    // nesdev: if (v & 0x001F) == 31 { v &= ~0x001F; v ^= 0x0400 } else { v += 1 }.
    let mut ppu = test_ppu();
    ppu.v = 0x0005;
    ppu.increment_coarse_x();
    assert_eq!(ppu.v, 0x0006);

    ppu.v = 0x001F; // coarse X at max
    ppu.increment_coarse_x();
    assert_eq!(ppu.v, 0x0400, "wraps to 0 and flips the horizontal NT bit");
}

#[test]
fn y_increment_rolls_coarse_y_at_29_and_flips_vertical_nametable() {
    // nesdev's coarse-Y==29 special case (the last visible row): coarse Y
    // resets to 0 AND the vertical nametable bit flips, unlike a plain
    // overflow at 31.
    let mut ppu = test_ppu();
    ppu.v = 0x7000 | (29 << 5); // fine Y = 7 (about to roll), coarse Y = 29
    ppu.increment_y();
    assert_eq!(ppu.v & 0x7000, 0, "fine Y rolled over to 0");
    assert_eq!(ppu.v & 0x03E0, 0, "coarse Y reset to 0 at the 29 boundary");
    assert_eq!(ppu.v & 0x0800, 0x0800, "vertical nametable bit flipped");
}

#[test]
fn y_increment_rolls_coarse_y_at_31_without_flipping_nametable() {
    // The out-of-bounds (31) case some games briefly scroll into: resets
    // to 0 but does NOT flip the nametable bit (nesdev: "coarse Y = 0,
    // nametable not switched").
    let mut ppu = test_ppu();
    ppu.v = 0x7000 | (31 << 5) | 0x0800;
    ppu.increment_y();
    assert_eq!(ppu.v & 0x03E0, 0);
    assert_eq!(ppu.v & 0x0800, 0x0800, "nametable bit must NOT flip at 31");
}

#[test]
fn copy_horizontal_moves_coarse_x_and_horizontal_nametable_bit_only() {
    let mut ppu = test_ppu();
    ppu.v = 0x7FFF;
    ppu.t = 0x0000;
    ppu.copy_horizontal();
    assert_eq!(
        ppu.v & 0x041F,
        0,
        "coarse X + horizontal NT bit cleared from t"
    );
    assert_eq!(
        ppu.v & !0x041F,
        0x7FFF & !0x041F,
        "every other bit of v is untouched"
    );
}

#[test]
fn copy_vertical_moves_fine_y_coarse_y_and_vertical_nametable_bit_only() {
    let mut ppu = test_ppu();
    ppu.v = 0x7FFF;
    ppu.t = 0x0000;
    ppu.copy_vertical();
    assert_eq!(
        ppu.v & 0x7BE0,
        0,
        "fine Y + coarse Y + vertical NT bit cleared from t"
    );
    assert_eq!(
        ppu.v & !0x7BE0,
        0x7FFF & !0x7BE0,
        "every other bit of v is untouched"
    );
}
