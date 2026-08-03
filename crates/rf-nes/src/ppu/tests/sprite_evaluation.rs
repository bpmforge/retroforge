//! Ticket W1-05a: secondary OAM evaluation, the 8-sprite-per-scanline
//! limit, the buggy overflow-flag diagonal scan, the `OAMADDR`
//! dots-257-320 reset, sprite compositing, and the one-scanline pipeline
//! delay. Sources cited in `crate::ppu::sprites`'s module doc; this file
//! doesn't re-cite them per test.
//!
//! Tests here fall into two groups:
//! - **Direct-call** tests invoke [`crate::ppu::Ppu::evaluate_sprites`] /
//!   `load_sprite_units` on a hand-poked `oam` array, bypassing the
//!   dot-driven pipeline entirely — used where only the algorithm itself
//!   (not its dot-timing wiring) is under test.
//! - **Tick-driven** tests drive the real `Ppu::tick` dot-by-dot, proving
//!   the wiring in `background.rs::process_render_dot` that calls those
//!   methods at the right dots (the `OAMADDR` reset and the first-scanline
//!   pipeline delay both require this — a direct call can't observe dot
//!   timing).
//!
//! Per the ticket's own trap warning ("tests that all start from a zero
//! state hide whole bug classes" — echoing W1-04a's `write_scroll` bug):
//! every fixture here starts from a NON-empty, NON-zero `oam` (never the
//! all-zero array `Ppu::new` starts with used as-is), and at least one test
//! ([`oam_addr_is_forced_to_zero_during_the_sprite_fetch_window`]) leaves
//! `OAMADDR` non-zero before the scanline runs.
use super::test_ppu;
use rf_core_api::PixelLayer;

/// Fill every tile this file uses (indices 1-10) with a fully solid 8x8
/// pattern (`pattern = 1` at every column and every row) so any covered
/// pixel is unambiguously opaque — the same "solid fill" discipline
/// `golden_frame_bg.rs` uses for backgrounds, applied here to sprites.
fn fill_solid_tiles(chr: &mut [u8]) {
    for tile in 1u16..=10 {
        let base = (tile * 16) as usize;
        for row in 0..8 {
            chr[base + row] = 0xFF; // lo plane: every bit set -> pattern 1
            chr[base + 8 + row] = 0x00; // hi plane: pattern stays 01, never 11
        }
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

const STATUS_SPRITE_OVERFLOW: u8 = 0x20;

#[test]
fn fewer_than_8_in_range_sprites_are_all_found_in_oam_order() {
    let mut ppu = test_ppu();
    ppu.scanline = 50;
    poke_sprite(&mut ppu.oam, 0, 50, 1, 0, 10); // in range (row 0)
    poke_sprite(&mut ppu.oam, 1, 43, 1, 0, 20); // in range (row 7, last valid)
    poke_sprite(&mut ppu.oam, 2, 51, 1, 0, 30); // NOT in range (one past)
    poke_sprite(&mut ppu.oam, 3, 100, 1, 0, 40); // NOT in range

    ppu.evaluate_sprites();

    assert_eq!(ppu.secondary_oam_count, 2);
    assert_eq!(ppu.secondary_oam[0].oam_index, 0);
    assert_eq!(ppu.secondary_oam[1].oam_index, 1);
    assert_eq!(
        ppu.status & STATUS_SPRITE_OVERFLOW,
        0,
        "no overflow with only 2 sprites present"
    );
}

#[test]
fn exactly_8_in_range_sprites_set_no_overflow_when_nothing_else_qualifies() {
    let mut ppu = test_ppu();
    ppu.scanline = 50;
    for n in 0..8u8 {
        poke_sprite(&mut ppu.oam, n, 50, 1, 0, n * 10);
    }
    // Every other primary-OAM entry (n=8..64) is left at `Ppu::new`'s
    // all-zero default -- Y=0 is never in range for scanline 50, so this
    // is a genuine "nothing beyond 8 qualifies" case, buggy scan or not.

    ppu.evaluate_sprites();

    assert_eq!(ppu.secondary_oam_count, 8);
    assert_eq!(
        ppu.status & STATUS_SPRITE_OVERFLOW,
        0,
        "no 9th sprite (real or misread) exists anywhere in this fixture"
    );
}

/// Acceptance criterion 2's core proof: a wrong 8-sprite cutoff, or a wrong
/// OAM-order priority, must be *visible in the rendered pixels* -- not just
/// in a count. 10 sprites (indices 0-9), all in range on the same
/// scanline, non-overlapping X positions: sprites 0-7 must render (their
/// `sprite_id` shows up in the composited pixel), and sprites 8-9 must be
/// **completely absent** -- their screen position must show the backdrop,
/// exactly as if they didn't exist, because the sink is accuracy-exact
/// (ruling in `crate::ppu::sprites`' module doc). A leaked dropped sprite
/// would turn a backdrop pixel into a `Sprite`-layer pixel at a known,
/// checked (x, y) -- maximally visible, not aliasable to anything else.
#[test]
fn only_the_first_8_oam_order_sprites_render_the_9th_and_10th_are_invisible() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    ppu.write_register(1, 0x14); // PPUMASK: show sprites + show in left 8px
    ppu.scanline = 50;

    let x_of = |i: u8| 8 + i * 9; // width 8, 1px gap: never overlaps
    for i in 0..10u8 {
        poke_sprite(&mut ppu.oam, i, 50, i + 1, 0, x_of(i));
    }

    ppu.evaluate_sprites();
    ppu.load_sprite_units();
    for x in 0..256u16 {
        ppu.output_pixel(x);
    }

    for i in 0..8u8 {
        let px = ppu.line_buffer[x_of(i) as usize];
        assert_eq!(px.layer, PixelLayer::Sprite, "sprite {i} must render");
        assert_eq!(px.sprite_id, Some(i), "sprite {i}'s own id must appear");
    }
    for i in 8..10u8 {
        let px = ppu.line_buffer[x_of(i) as usize];
        assert_eq!(
            px.layer,
            PixelLayer::Backdrop,
            "dropped sprite {i} must NOT leak into the framebuffer"
        );
        assert_eq!(px.sprite_id, None);
    }
    for x in 0..256usize {
        assert!(
            !ppu.line_buffer[x].dropped_by_limit,
            "dropped_by_limit is always false on the NES path (ruling, sprites.rs doc) at x={x}"
        );
    }
}

/// The buggy diagonal scan's false-negative case: a genuine 9th in-range
/// sprite (its real Y byte IS in range) is missed because the scan has
/// already drifted `m` off 0 by the time `n` reaches it. Sprite 8 (the
/// first candidate after the cap) is deliberately made out-of-range so the
/// very next check lands on sprite 9's TILE byte (m=1), not its Y byte
/// (m=0) -- and that tile byte is left at the all-zero default, which
/// reads as Y=0, out of range for scanline 50. A "fixed" implementation
/// that checks each remaining sprite's real Y byte directly (the naive,
/// textually-tempting shortcut this ticket explicitly warns against) WOULD
/// set the overflow flag here; this test fails against that implementation.
#[test]
fn overflow_flag_false_negative_from_the_m_drift_bug() {
    let mut ppu = test_ppu();
    ppu.scanline = 50;
    for n in 0..8u8 {
        poke_sprite(&mut ppu.oam, n, 50, 0, 0, 0);
    }
    poke_sprite(&mut ppu.oam, 8, 200, 0, 0, 0); // out of range: first overflow-phase check misses
                                                // Sprite 9's REAL Y (oam[36]) is genuinely in range, proving there IS
                                                // a real 9th sprite -- but the buggy scan checks oam[37] (its tile
                                                // byte, left at 0) instead, because by n=9 the drifted m is 1, not 0.
    ppu.oam[9 * 4] = 50; // sprite 9's real Y: in range
                         // oam[9*4 + 1] (tile byte) intentionally left at the default 0.

    ppu.evaluate_sprites();

    assert_eq!(
        ppu.status & STATUS_SPRITE_OVERFLOW,
        0,
        "the buggy scan reads sprite 9's TILE byte (0, out of range) instead of its \
         real Y byte (50, in range) -- flag must stay clear despite a genuine 9th sprite"
    );
}

/// The buggy diagonal scan's false-positive case: NO real 9th (or later)
/// sprite is in range anywhere, but a misaligned byte the drifted scan
/// reads AS a Y-coordinate happens to numerically fall in range, so the
/// flag gets set anyway. A "fixed" (naive-correct, real-Y-bytes-only)
/// implementation would find nothing and leave the flag clear; this test
/// fails against that implementation.
#[test]
fn overflow_flag_false_positive_from_the_m_drift_bug() {
    let mut ppu = test_ppu();
    ppu.scanline = 50;
    for n in 0..8u8 {
        poke_sprite(&mut ppu.oam, n, 50, 0, 0, 0);
    }
    poke_sprite(&mut ppu.oam, 8, 200, 0, 0, 0); // out of range: n=9, m=1 next
                                                // Sprite 9's REAL Y (oam[36]) stays 0 -- genuinely out of range, so
                                                // there is NO real 9th sprite. But its TILE byte (oam[37], m=1, what
                                                // the buggy scan actually reads next) is set to 50: numerically "in
                                                // range" if misread as a Y-coordinate.
    ppu.oam[9 * 4 + 1] = 50;

    ppu.evaluate_sprites();

    assert_eq!(
        ppu.status & STATUS_SPRITE_OVERFLOW,
        STATUS_SPRITE_OVERFLOW,
        "the buggy scan misreads sprite 9's tile byte (50) as an in-range Y -- flag must \
         incorrectly set even though no real 9th sprite exists"
    );
    assert_eq!(
        ppu.secondary_oam_count, 8,
        "the overflow phase never writes to secondary OAM -- still exactly 8"
    );
    assert!(
        ppu.secondary_oam[..8].iter().all(|s| s.oam_index != 9),
        "sprite 9 must never actually be copied in, false positive or not"
    );
}

/// nesdev.org/wiki/PPU_registers: "OAMADDR is set to 0 during each of ticks
/// 257-320... of the pre-render and visible scanlines" -- a tick-driven
/// proof (direct calls can't observe dot timing), and a discriminating
/// one: it checks `oam_addr` is UNCHANGED before dot 257, forced to 0
/// exactly there, stays forced through the whole 257-320 window (not just
/// once), and is free again past dot 320. Starts from a non-zero
/// `OAMADDR` (the ticket's own "test with... a non-zero OAMADDR" trap).
#[test]
fn oam_addr_is_forced_to_zero_during_the_sprite_fetch_window() {
    let mut ppu = test_ppu();
    ppu.write_register(1, 0x10); // show sprites: rendering_enabled() == true
    ppu.scanline = 0;
    ppu.dot = 0;
    ppu.write_register(3, 0x37); // OAMADDR = 0x37, non-zero

    while ppu.dot != 257 {
        ppu.tick();
    }
    assert_eq!(
        ppu.oam_addr(),
        0x37,
        "unchanged right up to (but not including) dot 257"
    );

    ppu.tick(); // processes dot 257
    assert_eq!(ppu.oam_addr(), 0, "forced to 0 at the window's first dot");

    ppu.write_register(3, 0x99); // a write mid-window
    ppu.tick(); // processes dot 258
    assert_eq!(
        ppu.oam_addr(),
        0,
        "still forced to 0 -- \"each of ticks 257-320\", not a one-shot at 257"
    );

    while ppu.dot != 321 {
        ppu.tick();
    }
    ppu.write_register(3, 0x55); // a write just past the window
    ppu.tick(); // processes dot 321, outside 257-320
    assert_eq!(ppu.oam_addr(), 0x55, "no longer forced once past dot 320");
}

/// Guards the design invariant `sprites.rs`'s module doc documents:
/// evaluation always starts at primary-OAM index 0, regardless of
/// `OAMADDR`'s live value -- it does NOT read `OAMADDR` as a starting
/// point. Direct-call (bypasses the reset entirely) so this is really
/// testing `evaluate_sprites` itself, not the reset from the previous
/// test. Non-empty, non-zero `oam` AND non-zero `OAMADDR`, per the
/// ticket's trap warning.
#[test]
fn evaluate_sprites_starts_at_oam_index_zero_regardless_of_oam_addr() {
    let mut ppu = test_ppu();
    ppu.scanline = 50;
    ppu.write_register(3, 0x20); // OAMADDR left at a non-zero, non-empty-looking value
    poke_sprite(&mut ppu.oam, 0, 50, 7, 0, 15); // must be found: proves n started at 0, not 8

    ppu.evaluate_sprites();

    assert_eq!(ppu.secondary_oam_count, 1);
    assert_eq!(ppu.secondary_oam[0].oam_index, 0);
    assert_eq!(ppu.secondary_oam[0].tile, 7);
}

/// nesdev.org/wiki/PPU_sprite_evaluation: "lower OAM index wins" among
/// overlapping sprites. Two fully-opaque, fully-overlapping sprites at
/// different OAM indices; only the lower index's `sprite_id` may appear.
#[test]
fn oam_order_priority_lower_index_wins_when_sprites_overlap() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    ppu.write_register(1, 0x14);
    ppu.scanline = 50;
    poke_sprite(&mut ppu.oam, 6, 50, 2, 0, 20); // higher OAM index, would lose
    poke_sprite(&mut ppu.oam, 2, 50, 1, 0, 20); // lower OAM index, must win

    ppu.evaluate_sprites();
    ppu.load_sprite_units();
    ppu.output_pixel(20);

    let px = ppu.line_buffer[20];
    assert_eq!(px.layer, PixelLayer::Sprite);
    assert_eq!(px.sprite_id, Some(2), "lower OAM index (2) must win over 6");
}

#[test]
fn horizontal_flip_mirrors_the_pattern_columns() {
    let mut ppu = test_ppu();
    // Tile 1, row 0: left nibble opaque (pattern=1), right nibble
    // transparent -- 0b11110000.
    ppu.chr[16] = 0b1111_0000;
    ppu.chr[16 + 8] = 0x00;
    ppu.write_register(1, 0x14);
    ppu.scanline = 50;
    poke_sprite(&mut ppu.oam, 0, 50, 1, 0x40, 0); // attr bit 6: flip horizontal

    ppu.evaluate_sprites();
    ppu.load_sprite_units();
    for x in 0..8u16 {
        ppu.output_pixel(x);
    }

    // Unflipped, columns 0-3 would be opaque and 4-7 transparent; flipped,
    // that must invert.
    for x in 0..4usize {
        assert_eq!(
            ppu.line_buffer[x].layer,
            PixelLayer::Backdrop,
            "column {x} must be transparent once flipped"
        );
    }
    for x in 4..8usize {
        assert_eq!(
            ppu.line_buffer[x].layer,
            PixelLayer::Sprite,
            "column {x} must be opaque once flipped"
        );
    }
}

#[test]
fn vertical_flip_mirrors_the_row_order() {
    let mut ppu = test_ppu();
    // Tile 1: only row 0 opaque, every other row transparent.
    ppu.chr[16] = 0xFF;
    for row in 1..8 {
        ppu.chr[16 + row] = 0x00;
    }
    ppu.write_register(1, 0x14);
    ppu.scanline = 57; // sprite y=50 => row 7 without flip, row 0 with flip
    poke_sprite(&mut ppu.oam, 0, 50, 1, 0x80, 20); // attr bit 7: flip vertical

    ppu.evaluate_sprites();
    ppu.load_sprite_units();
    ppu.output_pixel(20);

    assert_eq!(
        ppu.line_buffer[20].layer,
        PixelLayer::Sprite,
        "flipped, screen row 57 shows the tile's row 0 (the only opaque row)"
    );
}

#[test]
fn sprite_8x16_mode_selects_bank_from_tile_bit0_and_splits_top_bottom_tile() {
    let mut ppu = test_ppu();
    ppu.ctrl = 0x20; // PPUCTRL bit 5: 8x16 sprites
                     // Tile pair (bank 0, top tile 4 / bottom tile 5): make ONLY the bottom
                     // tile (5) opaque at row 0, so this can only pass if bank/top-bottom
                     // selection is right.
    ppu.chr[5 * 16] = 0xFF;
    ppu.chr[5 * 16 + 8] = 0x00;
    ppu.chr[4 * 16] = 0x00; // top tile stays fully transparent
    ppu.chr[4 * 16 + 8] = 0x00;
    ppu.write_register(1, 0x14);
    ppu.scanline = 58; // sprite y=50, height 16 -> row 8 = first row of the bottom half
    poke_sprite(&mut ppu.oam, 0, 50, 4, 0, 20); // tile byte 4: bank 0, top tile 4 (bottom = 5)

    ppu.evaluate_sprites();
    ppu.load_sprite_units();
    ppu.output_pixel(20);

    assert_eq!(
        ppu.line_buffer[20].layer,
        PixelLayer::Sprite,
        "row 8 (bottom tile's row 0) must read tile 5, which is opaque there"
    );
}

#[test]
fn left8_mask_hides_sprites_only_in_the_leftmost_8_pixels() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    ppu.scanline = 50;
    poke_sprite(&mut ppu.oam, 0, 50, 1, 0, 0); // x=0: entirely inside the left 8px

    // Show sprites (bit4) but NOT in the leftmost 8 pixels (bit2 clear).
    ppu.write_register(1, 0x10);
    ppu.evaluate_sprites();
    ppu.load_sprite_units();
    ppu.output_pixel(0);
    assert_eq!(
        ppu.line_buffer[0].layer,
        PixelLayer::Backdrop,
        "left8 masked off: sprite must not show at x=0"
    );

    // Now allow it (bit2 set too).
    ppu.write_register(1, 0x14);
    ppu.output_pixel(0);
    assert_eq!(
        ppu.line_buffer[0].layer,
        PixelLayer::Sprite,
        "left8 allowed: sprite must show at x=0"
    );
}

/// nesdev.org/wiki/PPU_OAM's attribute-byte bit 5: "Priority (0: in front
/// of background; 1: behind background)" -- the FULL truth table at a
/// pixel where a sprite is present, not just the one entry a single case
/// can vacuously satisfy:
/// - front-priority sprite + opaque BG -> **sprite** wins (bit clear).
/// - behind-priority sprite + opaque BG -> **BG** wins (bit set).
/// - behind-priority sprite + TRANSPARENT BG -> **sprite** still wins --
///   "behind background" loses only to an OPAQUE background pixel
///   (nesdev.org/wiki/PPU_rendering's priority-multiplexer table, quoted
///   in `crate::ppu::sprites::output_pixel`'s doc), not unconditionally;
///   this is the case that would fail against a naive implementation that
///   treats the priority bit as "never show" instead of "loses only to
///   opaque BG".
///
/// CONDUCTOR CAUGHT (2026-08-03): this test used to cover ONLY the first
/// case, and did so at scanline 0 with the sprite's own Y=0 -- but
/// `sprites.rs`'s one-scanline pipeline delay (module doc) means
/// scanline 0 NEVER renders any sprite (evaluation for scanline 0 would
/// have to happen on the pre-render line, which never evaluates), so
/// `active_sprite_count` was 0 for the whole scanline being checked and
/// the sprite compositing branch this test claimed to cover was never
/// even reached -- inverting `behind_background` at the call site made
/// zero tests fail. Fixed by evaluating on scanline 0 (Y=0 sprites) and
/// checking the render on scanline 1, matching the delay for real.
#[test]
fn priority_bit_full_truth_table_at_opaque_and_transparent_background() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr); // tiles 1-10: sprite tiles, opaque
    ppu.write_register(1, 0x1C); // show bg + bg-left8 + sprites + sprites-left8
    ppu.scanline = 0;
    ppu.dot = 0;

    // Background: tile 20 (opaque, pattern 1) everywhere except nametable
    // column 7 (screen x 56-63), which gets tile 21 -- left at its
    // `Ppu::new` default all-zero CHR, i.e. genuinely transparent (pattern
    // 0), not just "not covered by fill_solid_tiles".
    for row in 0u16..30 {
        for col in 0u16..32 {
            let tile = if col == 7 { 21 } else { 20 };
            ppu.mem_write(0x2000 + row * 32 + col, tile);
        }
    }
    for addr in 0x23C0u16..=0x23FFu16 {
        ppu.mem_write(addr, 0x00);
    }
    // Tile 20: opaque at EVERY row (screen row 1 is checked here, not row
    // 0 -- the one-scanline delay means these sprites render on scanline
    // 1, which reads the BG tile's row 1, not row 0; filling only row 0
    // was this test's own earlier bug, caught by the very failure this
    // fix responds to).
    for row in 0..8 {
        ppu.chr[20 * 16 + row] = 0xFF;
        ppu.chr[20 * 16 + 8 + row] = 0x00;
    }
    // tile 21 (x=56-63's column): left all-zero -- transparent, every row.

    ppu.palette[0] = 0x01; // backdrop (never expected here)
    ppu.palette[1] = 0x02; // BG palette group 0, pattern 1 (tile 20's value)
    ppu.palette[0x11] = 0x03; // sprite palette group 0, pattern 1

    poke_sprite(&mut ppu.oam, 0, 0, 1, 0x00, 20); // front priority, over opaque BG (col 2)
    poke_sprite(&mut ppu.oam, 1, 0, 1, 0x20, 40); // behind priority, over opaque BG (col 5)
    poke_sprite(&mut ppu.oam, 2, 0, 1, 0x20, 56); // behind priority, over TRANSPARENT BG (col 7)

    // Run scanline 0 (evaluates these Y=0 sprites for scanline 1's
    // rendering) THEN scanline 1 (where they actually render) -- the real
    // one-scanline delay, not a direct-call shortcut, since this test is
    // specifically about a compositing branch reached through the
    // dot-driven pipeline.
    for _ in 0..(341u32 * 2) {
        ppu.tick();
    }

    let front_over_opaque = ppu.line_buffer[20];
    assert_eq!(
        front_over_opaque.layer,
        PixelLayer::Sprite,
        "front-priority sprite must win over an opaque background pixel"
    );
    assert_eq!(front_over_opaque.palette_index, 0x03);

    let behind_over_opaque = ppu.line_buffer[40];
    assert_eq!(
        behind_over_opaque.layer,
        PixelLayer::Background(0),
        "behind-priority sprite must lose to an opaque background pixel"
    );
    assert_eq!(behind_over_opaque.palette_index, 0x02);

    let behind_over_transparent = ppu.line_buffer[56];
    assert_eq!(
        behind_over_transparent.layer,
        PixelLayer::Sprite,
        "behind-priority sprite must still win over a TRANSPARENT background pixel -- \
         \"behind background\" loses only to an opaque BG pixel, not unconditionally"
    );
    assert_eq!(behind_over_transparent.palette_index, 0x03);
}

/// nesdev.org/wiki/PPU_sprite_evaluation's Notes: "no sprites will be
/// rendered on the first scanline" (evaluation never runs on the
/// pre-render line, so scanline 0 always starts with an empty active set).
/// Tick-driven from real power-on state (`Ppu::new` starts on the
/// pre-render line) -- a direct call can't exercise this, since it's
/// exactly the dot-driven clear/evaluate/load wiring under test.
#[test]
fn no_sprites_render_on_the_first_scanline_of_a_frame() {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    ppu.write_register(1, 0x14);
    // Sprite at y=0: in range for scanline 0 by the plain range formula,
    // but evaluation never runs on the pre-render line, so it can only
    // ever be found during scanline 0's OWN dot-65 evaluation (for
    // scanline 1's rendering) -- never scanline 0's.
    poke_sprite(&mut ppu.oam, 0, 0, 1, 0, 20);

    // `Ppu::new` starts at (PRERENDER, 0). Run the pre-render line, then
    // scanline 0, in full.
    for _ in 0..(341u32 * 2) {
        ppu.tick();
    }
    assert_eq!(
        ppu.line_buffer[20].layer,
        PixelLayer::Backdrop,
        "scanline 0 must not render the sprite despite y=0 being 'in range'"
    );

    // Scanline 1 must show it (evaluated during scanline 0's dot 65,
    // latched at scanline 0's dot 257).
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(
        ppu.line_buffer[20].layer,
        PixelLayer::Sprite,
        "scanline 1 must render the sprite -- the one-scanline pipeline delay"
    );
    assert_eq!(ppu.line_buffer[20].sprite_id, Some(0));
}
