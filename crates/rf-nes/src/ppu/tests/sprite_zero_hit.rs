//! Ticket W1-05b: sprite-0 hit (`STATUS_SPRITE0_HIT`). Sources cited in
//! `crate::ppu::sprites`'s module doc "Sprite-0 hit" section; this file
//! doesn't re-cite them per test. Every fixture pads unused OAM slots with
//! `$FF` (never leaves them zeroed — the ticket's own trap warning: phantom
//! Y=0 sprites in zero-filled OAM can falsely appear "in range"), and every
//! rendering-pipeline assertion checks the scanline the hit actually lands
//! on (evaluation on line N feeds rendering on line N+1 — `sprites.rs`'s
//! "one-scanline pipeline delay").
use super::{test_ppu, Ppu};

const STATUS_SPRITE0_HIT: u8 = 0x40;

/// Fill every tile this file uses (indices 1-2) with a fully solid 8x8
/// opaque pattern — same "solid fill" discipline `sprite_evaluation.rs`
/// uses.
fn fill_solid_tiles(chr: &mut [u8]) {
    for tile in 1u16..=2 {
        let base = (tile * 16) as usize;
        for row in 0..8 {
            chr[base + row] = 0xFF;
            chr[base + 8 + row] = 0x00;
        }
    }
}

/// Fill the whole first nametable with an opaque background tile (index 1)
/// so every on-screen x has an opaque BG pixel wherever it's checked.
fn fill_opaque_background(ppu: &mut Ppu) {
    for addr in 0x2000u16..0x2400 {
        ppu.mem_write(addr, 1);
    }
}

/// `$FF`-pad every OAM byte (the ticket's own trap: never leave OAM
/// zero-filled — a zeroed sprite is `Y=0, tile=0, attr=0, X=0`, which is a
/// real, in-range-at-some-scanlines sprite, not an empty slot).
fn ff_pad_oam(oam: &mut [u8; 256]) {
    oam.fill(0xFF);
}

/// Fill every row of tile `tile` with a pattern opaque in exactly one
/// column (`col`, 0-7 counting from the sprite's left edge) — every row
/// gets the same single-bit pattern, so the result is independent of which
/// row a sprite's one-scanline pipeline delay happens to render (unlike
/// setting only row 0: a `Y = 0` sprite actually renders row 1 on the
/// scanline this file's tests check, per `sprites.rs`'s "one-scanline
/// pipeline delay" — an earlier version of this file's x=255/x=254 tests
/// set only row 0 and passed vacuously, since the real rendered row was
/// then fully transparent regardless of which column was "opaque").
fn single_opaque_column_tile(chr: &mut [u8], tile: u16, col: u8) {
    let base = (tile * 16) as usize;
    let bit = 7 - col;
    for row in 0..8 {
        chr[base + row] = 1 << bit;
        chr[base + 8 + row] = 0x00;
    }
}

/// Write one sprite's 4 bytes into `oam` at primary index `n`.
fn poke_sprite(oam: &mut [u8; 256], n: u8, y: u8, tile: u8, attr: u8, x: u8) {
    let base = n as usize * 4;
    oam[base] = y;
    oam[base + 1] = tile;
    oam[base + 2] = attr;
    oam[base + 3] = x;
}

/// Runs the pre-render line then one full visible scanline (341*2 ticks
/// from `Ppu::new`'s starting `(PRERENDER, 0)`), landing exactly after
/// scanline 0 finishes rendering — sprite-0 (placed at `y = 0`) is
/// evaluated during scanline 0's own dot 65 and rendered on scanline 1, so
/// this drives evaluation only; callers that need the pixel rendered run
/// one more scanline (341 more ticks).
fn run_prerender_and_one_scanline(ppu: &mut Ppu) {
    for _ in 0..(341u32 * 2) {
        ppu.tick();
    }
}

/// Direct replication of `sprite_hit_tests_2005.10.05/source/01.basics.asm`
/// test code 3 ("Should hit even when completely behind background"):
/// opaque BG everywhere, opaque solid sprite-0, attribute byte `$20`
/// (priority bit 5 set = behind background). Real hardware -- and
/// `sprites.rs`'s `sprite_pixel`, which never consults `behind_background`
/// while searching for an opaque match -- both say this must still hit.
#[test]
fn hits_even_when_sprite_is_behind_an_opaque_background() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    ppu.write_register(1, 0x18); // PPUMASK: show BG + sprites, no left-8

    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x20, 20); // sprite 0, behind-BG priority

    run_prerender_and_one_scanline(&mut ppu); // evaluate on scanline 0
    for _ in 0..341u32 {
        ppu.tick();
    } // render on scanline 1

    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        STATUS_SPRITE0_HIT,
        "sprite 0 must hit even though its priority bit says \"behind background\" -- \
         nesdev.org/wiki/PPU_OAM: sprite priority does not gate sprite-0 hit"
    );
}

#[test]
fn hits_when_sprite_is_in_front_of_an_opaque_background() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    ppu.write_register(1, 0x18);

    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 20); // front priority

    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }

    assert_eq!(ppu.status & STATUS_SPRITE0_HIT, STATUS_SPRITE0_HIT);
}

/// Isolates x=255 specifically: a sprite at x=248 whose pattern is opaque
/// ONLY in its rightmost column (screen x=255) -- columns 248-254 (the
/// sprite's other 7 columns) are deliberately transparent, so if the
/// x=255 exclusion were missing OR if some other bug were suppressing the
/// whole sprite, this test could not tell the difference from "the
/// exclusion works." A sprite that also hits at 248-254 (as the old,
/// vacuous version of this test used) can't isolate x=255 at all -- the
/// flag would already be set from an earlier column by the time x=255 is
/// even checked.
#[test]
fn no_hit_at_x_255() {
    let mut ppu = test_ppu();
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    single_opaque_column_tile(&mut ppu.chr, 1, 7); // rightmost column only
    ppu.write_register(1, 0x18);

    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 248); // opaque column lands at x=255 only

    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }

    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        0,
        "nesdev.org/wiki/PPU_OAM: sprite-0 hit never fires at x=255, \"for an obscure \
         reason related to the pixel pipeline\" -- this sprite's ONLY opaque column \
         lands there, so a working exclusion must leave the flag entirely clear"
    );
}

/// The same isolated-opaque-column trick as `no_hit_at_x_255`, but placed
/// one column to the left (x=254) -- proves the exclusion is narrowly
/// x=255 only, not an off-by-one that also eats x=254.
#[test]
fn hit_still_fires_at_x_254_the_column_immediately_before_the_exclusion() {
    let mut ppu = test_ppu();
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    single_opaque_column_tile(&mut ppu.chr, 1, 6); // second-from-right column only
    ppu.write_register(1, 0x18);

    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 248); // opaque column lands at x=254 only

    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }

    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        STATUS_SPRITE0_HIT,
        "x=254 (one column left of the x=255 exclusion) must still hit"
    );
}

#[test]
fn no_hit_in_the_left_8_pixels_when_either_left8_mask_bit_is_clear() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 0); // entirely inside x=0..7

    // BG-left8 (bit1) clear, sprite-left8 (bit2) set: show BG(8)+sprites(0x10)+sprite-left8(4).
    ppu.write_register(1, 0x1C & !0x02);
    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        0,
        "BG-left8 clear (bit1) must suppress the hit at x<8 even though sprite-left8 is set"
    );

    // Reset and try the other bit clear: sprite-left8 (bit2) clear, BG-left8 (bit1) set.
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 0);
    ppu.write_register(1, 0x1C & !0x04);
    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        0,
        "sprite-left8 clear (bit2) must suppress the hit at x<8 even though BG-left8 is set"
    );

    // Both set: the hit must now fire.
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 0);
    ppu.write_register(1, 0x1E); // BG+sprites+bg-left8+sprite-left8
    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        STATUS_SPRITE0_HIT,
        "with both left-8 mask bits set, the same x<8 sprite must hit"
    );
}

#[test]
fn no_hit_when_background_rendering_is_disabled() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 20);
    ppu.write_register(1, 0x10); // sprites only, BG show bit clear

    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(ppu.status & STATUS_SPRITE0_HIT, 0);
}

#[test]
fn no_hit_when_sprite_rendering_is_disabled() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 20);
    ppu.write_register(1, 0x08); // BG only, sprite show bit clear

    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(ppu.status & STATUS_SPRITE0_HIT, 0);
}

#[test]
fn no_hit_when_either_layer_is_transparent_at_that_pixel() {
    // Opaque sprite, transparent BG (BG tile left at the all-zero default).
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    ff_pad_oam(&mut ppu.oam);
    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 20);
    ppu.write_register(1, 0x18); // BG+sprites, no left8 (BG left transparent anyway)

    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        0,
        "opaque sprite over a transparent BG pixel must not hit"
    );

    // Opaque BG, transparent sprite (tile index 2 left blank).
    let mut ppu = test_ppu();
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    poke_sprite(&mut ppu.oam, 0, 0, 0, 0x00, 20); // tile 0: never filled, transparent
    ppu.write_register(1, 0x18);

    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        0,
        "transparent sprite over an opaque BG pixel must not hit"
    );
}

/// nesdev.org/wiki/PPU_registers: the three status flags are "automatically
/// cleared on dot 1 of the prerender scanline" -- proves sprite-0 hit
/// specifically follows that same rule (not e.g. cleared by reading
/// `$2002`, which only clears vblank).
#[test]
fn cleared_at_prerender_dot_1_not_by_reading_2002() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    ppu.write_register(1, 0x18);
    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 20);

    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(ppu.status & STATUS_SPRITE0_HIT, STATUS_SPRITE0_HIT);

    // Reading $2002 clears vblank/w, never sprite-0-hit.
    let _ = ppu.read_register(2, 0);
    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        STATUS_SPRITE0_HIT,
        "reading $2002 must not clear sprite-0 hit"
    );

    // Run until the NEXT frame's pre-render dot 1 (261,1) has actually
    // been PROCESSED: the loop condition alone would stop as soon as
    // `(scanline, dot)` first *reads* (261, 1), which is the state left
    // behind by the tick that processed dot 0 -- dot 1 itself, and its
    // status-bit clear, hasn't run yet at that point. One more `tick()`
    // after the loop is what actually executes dot 1's clear.
    while !(ppu.scanline == 261 && ppu.dot == 1) {
        ppu.tick();
    }
    ppu.tick();
    assert_eq!(
        ppu.status & STATUS_SPRITE0_HIT,
        0,
        "pre-render dot 1 of the NEXT frame must clear sprite-0 hit"
    );
}

/// Once set, the flag must not un-set itself for the rest of the same
/// frame even though `output_pixel` re-evaluates the hit condition on
/// every dot of every remaining scanline (most of which have no sprite-0
/// pixel at all, which must not accidentally clear a bit only ever ORed).
#[test]
fn stays_set_for_the_rest_of_the_frame_once_hit() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    fill_opaque_background(&mut ppu);
    ff_pad_oam(&mut ppu.oam);
    ppu.write_register(1, 0x18);
    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 20); // hits early (rendered on scanline 1)

    run_prerender_and_one_scanline(&mut ppu);
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(ppu.status & STATUS_SPRITE0_HIT, STATUS_SPRITE0_HIT);

    // Run most of the rest of the visible frame (through scanline 200) --
    // sprite-0's own pixel is long past (only rendered once, on scanline
    // 1), yet the flag must remain set the whole time.
    for _ in 0..(341u32 * 199) {
        ppu.tick();
        assert_eq!(
            ppu.status & STATUS_SPRITE0_HIT,
            STATUS_SPRITE0_HIT,
            "must stay set through scanline {} dot {}",
            ppu.scanline,
            ppu.dot
        );
    }
}
