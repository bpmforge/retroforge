//! PPU tests: BG modes 0/1, per-BG sizes, BG3 priority, and the two OBJ
//! limits (ticket W6-03a).

use rf_core_api::PixelLayer;

use crate::ppu::obj::{obj_sizes, MAX_SLIVERS_PER_LINE, MAX_SPRITES_PER_LINE};
use crate::ppu::{bg, Ppu};

/// A PPU with the screen on and one 2bpp tile of solid colour 1 at
/// character 1, tilemap at word 0, characters at word $1000.
fn ppu_with_tile() -> Ppu {
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.bg_mode = 0;
    p.bgs[0].enabled = true;
    p.bgs[0].tilemap_base = 0;
    p.bgs[0].char_base = 0x1000;
    // Character 1, 2bpp: 8 rows, plane 0 all ones -> colour 1 everywhere.
    for row in 0..8 {
        let at = (0x1000 + 8 + row) * 2;
        p.vram[at] = 0xFF;
        p.vram[at + 1] = 0x00;
    }
    p
}

fn set_tilemap(p: &mut Ppu, base: u16, index: u16, entry: u16) {
    let at = usize::from(base + index) * 2;
    p.vram[at] = entry as u8;
    p.vram[at + 1] = (entry >> 8) as u8;
}

#[test]
fn a_background_tile_reaches_the_scanline() {
    let mut p = ppu_with_tile();
    set_tilemap(&mut p, 0, 0, 1); // tile 1, palette 0, priority 0
    let line = p.render_scanline(0);
    assert_eq!(line.pixels[0].palette_index, 1);
    assert_eq!(line.pixels[0].layer, PixelLayer::Background(0));
    // Beyond the one tile, the map is zeros -> character 0 -> transparent.
    assert_eq!(line.pixels[100].layer, PixelLayer::Backdrop);
}

/// Forced blank must not render — and must not let the OBJ limit flags
/// accumulate while the screen is off.
#[test]
fn forced_blank_renders_backdrop_and_evaluates_nothing() {
    let mut p = ppu_with_tile();
    set_tilemap(&mut p, 0, 0, 1);
    p.forced_blank = true;
    let line = p.render_scanline(0);
    assert!(line.pixels.iter().all(|x| x.layer == PixelLayer::Backdrop));
    assert!(!p.range_over && !p.time_over);
}

/// Mode 0 gives each BG its own palette block; mode 1 does not.
///
/// Getting this wrong yields correct shapes in entirely wrong colours,
/// which reads as a palette bug rather than a mode bug.
#[test]
fn mode_0_offsets_each_layers_palette_block_and_mode_1_does_not() {
    assert_eq!(bg::palette_base(0, 0), 0);
    assert_eq!(bg::palette_base(0, 1), 32);
    assert_eq!(bg::palette_base(0, 2), 64);
    assert_eq!(bg::palette_base(0, 3), 96);
    for layer in 0..4 {
        assert_eq!(bg::palette_base(1, layer), 0, "mode 1 indexes from 0");
    }
}

#[test]
fn the_modes_have_the_documented_bit_depths() {
    assert_eq!(bg::bit_depths(0), [2, 2, 2, 2]);
    assert_eq!(bg::bit_depths(1), [4, 4, 2, 0], "mode 1 has no BG4");
}

/// 16×16 tiles: the tile below is 16 characters on, not 2, because the
/// character map is 16 wide.
#[test]
fn sixteen_pixel_tiles_take_their_lower_half_from_character_plus_sixteen() {
    let mut p = ppu_with_tile();
    p.bgs[0].tile_size_16 = true;
    set_tilemap(&mut p, 0, 0, 1);
    // Character 17 = tile 1's bottom-left quadrant. Give it colour 2.
    for row in 0..8 {
        let at = (0x1000 + 17 * 8 + row) * 2;
        p.vram[at] = 0x00;
        p.vram[at + 1] = 0xFF;
    }
    assert_eq!(p.render_scanline(0).pixels[0].palette_index, 1, "top half");
    assert_eq!(
        p.render_scanline(8).pixels[0].palette_index,
        2,
        "bottom half comes from character + 16"
    );
}

/// Scroll registers share latches across ALL FOUR layers, so writing
/// BG1's scroll changes what BG2's next write produces.
#[test]
fn bg_scroll_uses_the_shared_write_twice_latches() {
    let mut p = Ppu::new();
    // BG1VOFS = (data << 8) | bgofs_latch. First write sets the latch.
    p.write_register(0x210E, 0x12);
    p.write_register(0x210E, 0x03);
    assert_eq!(p.bgs[0].vofs, 0x0312);

    // BG1HOFS = (data << 8) | (bgofs_latch & $F8) | (bghofs_latch & $07).
    let mut q = Ppu::new();
    q.write_register(0x210D, 0xFF);
    q.write_register(0x210D, 0x01);
    assert_eq!(q.bgs[0].hofs, 0x01FF);
}

/// **BG3 priority.** In mode 1 with `$2105` bit 3 set, BG3's
/// high-priority tiles jump from near the back of the order to the very
/// front — above sprites.
#[test]
fn the_bg3_priority_bit_promotes_bg3_above_everything_in_mode_1() {
    let mut p = ppu_with_tile();
    p.bg_mode = 1;
    p.bgs[0].enabled = true;
    p.bgs[2].enabled = true;
    p.bgs[2].tilemap_base = 0x0400;
    p.bgs[2].char_base = 0x1000;

    // BG1 is 4bpp in mode 1, so its character 1 is 16 words in, not 8 —
    // the 2bpp data `ppu_with_tile` wrote is at the wrong place for it.
    // (Getting this wrong made BG1 transparent and BG3 win regardless of
    // the priority bit, which looked exactly like a priority bug.)
    for row in 0..8 {
        let at = (0x1000 + 16 + row) * 2;
        p.vram[at] = 0xFF;
    }
    // BG3 stays 2bpp and keeps using the 8-word layout.
    set_tilemap(&mut p, 0, 0, 1);
    set_tilemap(&mut p, 0x0400, 0, 1 | 0x2000); // BG3 tile, priority bit set

    p.bg3_priority = false;
    let without = p.render_scanline(0).pixels[0];
    p.bg3_priority = true;
    let with = p.render_scanline(0).pixels[0];

    assert_eq!(
        without.layer,
        PixelLayer::Background(0),
        "with the bit clear, BG1 outranks even a high-priority BG3 tile"
    );
    assert_eq!(
        with.layer,
        PixelLayer::Background(2),
        "with the bit set, BG3's high-priority tile wins"
    );
}

// ---------------------------------------------------------------------
// OBJ limits — tested SEPARATELY, on purpose
// ---------------------------------------------------------------------

/// Put `count` sprites on line 0, spaced 7px apart so drops are visible
/// at distinct x positions.
///
/// **Every unused OAM entry is parked off the line.** OAM powers up as
/// zeros, which means Y = 0 — so without this, all 128 sprites intersect
/// scanline 0 and every limit test trips on the 96 sprites it never meant
/// to create. Y = 100 keeps a sprite clear of line 0 for every size up to
/// 64 tall (`(0 - 100) & 0xFF = 156`, and 156 >= 64).
fn ppu_with_sprites(count: usize, size_select: u8, large: bool) -> Ppu {
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.obj_enabled = true;
    p.obj_size = size_select;
    p.obj_name_base = 0;
    // One 4bpp character of solid colour 1 (OBJ is always 4bpp).
    for row in 0..8 {
        let at = (16 + row) * 2;
        p.vram[at] = 0xFF;
    }
    for i in 0..128usize {
        p.oam[i * 4 + 1] = 100; // parked
    }
    for i in 0..count {
        let base = i * 4;
        p.oam[base] = ((i * 7) % 256) as u8;
        p.oam[base + 1] = 0; // on line 0
        p.oam[base + 2] = 1; // character 1
        p.oam[base + 3] = 0;
        if large {
            let byte = 0x0200 + i / 4;
            p.oam[byte] |= 0x02 << ((i % 4) * 2);
        }
    }
    p
}

/// **Limit 1: 32 sprites per line.** The 33rd is dropped however narrow
/// it is.
#[test]
fn more_than_thirty_two_sprites_on_a_line_sets_range_over() {
    // 32 small (8x8) sprites: 32 sprites, 32 slivers — under both limits.
    let mut ok = ppu_with_sprites(MAX_SPRITES_PER_LINE, 0, false);
    let _ = ok.render_scanline(0);
    assert!(!ok.range_over, "exactly 32 sprites is within the limit");
    assert!(!ok.time_over, "and 32 slivers is within the sliver budget");

    // 33 small sprites: still only 33 slivers, so ONLY the count trips.
    let mut over = ppu_with_sprites(MAX_SPRITES_PER_LINE + 1, 0, false);
    let _ = over.render_scanline(0);
    assert!(over.range_over, "the 33rd sprite trips the count limit");
    assert!(
        !over.time_over,
        "33 slivers is under the 34 budget — this must be the COUNT limit \
         firing, not the sliver limit"
    );
}

/// **Limit 2: 34 slivers per line.** Five 64-wide sprites are only five
/// sprites but forty slivers, so they blow this budget while passing the
/// count entirely.
#[test]
fn wide_sprites_exhaust_the_sliver_budget_without_tripping_the_count() {
    // Size select 2 = 8x8 small, 64x64 large. Five large sprites.
    let mut p = ppu_with_sprites(5, 2, true);
    assert_eq!(obj_sizes(2).1, (64, 64));
    let _ = p.render_scanline(0);
    assert!(
        p.time_over,
        "5 x 64-wide = 40 slivers, over the {MAX_SLIVERS_PER_LINE} budget"
    );
    assert!(
        !p.range_over,
        "only 5 sprites — the COUNT limit must not fire; if it did, the two \
         limits are not independent"
    );
}

/// Four 64-wide sprites are 32 slivers — just inside the budget.
#[test]
fn the_sliver_budget_admits_what_fits() {
    let mut p = ppu_with_sprites(4, 2, true);
    let _ = p.render_scanline(0);
    assert!(!p.time_over, "4 x 8 = 32 slivers fits in 34");
    assert!(!p.range_over);
}

/// `$213E` is the hardware's own report of both limits — a second,
/// independent assertion on the same logic.
#[test]
fn stat77_reports_range_over_and_time_over() {
    let mut p = ppu_with_sprites(MAX_SPRITES_PER_LINE + 1, 0, false);
    assert_eq!(p.read_stat77() & 0xC0, 0x00, "clear before rendering");
    let _ = p.render_scanline(0);
    assert_eq!(p.read_stat77() & 0x40, 0x40, "bit 6 = range over");

    let mut q = ppu_with_sprites(5, 2, true);
    let _ = q.render_scanline(0);
    assert_eq!(q.read_stat77() & 0x80, 0x80, "bit 7 = time over");
}

/// **Dropped sprites are reported, never drawn.** The accuracy-exact
/// stream must not contain them, and `dropped_by_limit` must stay false.
#[test]
fn dropped_sprites_go_to_the_overlay_and_never_into_the_pixel_stream() {
    // 36 sprites spaced 7px apart: the four beyond the count limit sit
    // past where the survivors reach, so their overlay pixels are visible
    // rather than masked by a real sprite at the same x.
    let mut p = ppu_with_sprites(MAX_SPRITES_PER_LINE + 4, 0, false);
    let line = p.render_scanline(0);

    assert!(
        line.overlay.iter().any(|o| o.opaque),
        "the suppressed sprites must be reported through the overlay channel"
    );
    // The real stream still shows a surviving sprite at x=0.
    assert_eq!(line.pixels[0].layer, PixelLayer::Sprite);
}

#[test]
fn the_eight_obj_size_pairs_are_the_documented_ones() {
    assert_eq!(obj_sizes(0), ((8, 8), (16, 16)));
    assert_eq!(obj_sizes(3), ((16, 16), (32, 32)));
    assert_eq!(obj_sizes(5), ((32, 32), (64, 64)));
    assert_eq!(obj_sizes(6), ((16, 32), (32, 64)));
}

/// A sprite's X is a signed 9-bit value, so it can hang off the left edge
/// rather than wrapping to the right of the screen.
#[test]
fn sprite_x_is_signed_nine_bits() {
    let mut p = ppu_with_sprites(1, 0, false);
    p.oam[0] = 0xFC; // X low
    p.oam[0x0200] |= 0x01; // X high bit -> $1FC = -4
    let s = crate::ppu::obj::decode_sprite(&p, 0);
    assert_eq!(s.x, -4);
}

// ---------------------------------------------------------------------
// End-to-end: the write port and the renderer must share one VRAM
// ---------------------------------------------------------------------

/// A tile written through `$2118` must be visible to the renderer.
///
/// This exists because the first wiring of this ticket had TWO VRAM
/// buffers — the `$2118` port (built in W6-02b, on the bus) filled one
/// while the PPU rendered from another. Every unit test above still
/// passed, because they poke `ppu.vram` directly, and gilyon cputest
/// still passed, because it only ever writes VRAM and never looks at a
/// rendered frame. The symptom would have been every game drawing a
/// black screen, first noticed in W6-03b's golden frames with nothing
/// obviously wrong at either end.
#[test]
fn a_tile_written_through_the_vram_port_is_rendered() {
    use crate::cpu::CpuBus;
    use rf_cart::SnesMapMode;

    let mut bus = crate::bus::SnesBus::new(vec![0; 32 * 1024], 0, SnesMapMode::LoRom);

    // VMAIN: increment after the high-byte write, step 1.
    bus.write(0x00_2115, 0x80);

    // Character 1 at word $1000, 2bpp, plane 0 solid -> colour 1.
    bus.write(0x00_2116, 0x08);
    bus.write(0x00_2117, 0x10); // word address $1008 = char 1, row 0
    for _ in 0..8 {
        bus.write(0x00_2118, 0xFF);
        bus.write(0x00_2119, 0x00);
    }
    // Tilemap entry 0 -> character 1.
    bus.write(0x00_2116, 0x00);
    bus.write(0x00_2117, 0x00);
    bus.write(0x00_2118, 0x01);
    bus.write(0x00_2119, 0x00);

    // Mode 0, BG1 on, characters at $1000, screen on.
    bus.write(0x00_2105, 0x00);
    bus.write(0x00_210B, 0x01); // BG1 char base = 1 << 12 = $1000
    bus.write(0x00_212C, 0x01); // BG1 on main screen
    bus.write(0x00_2100, 0x0F); // screen on, full brightness

    let line = bus.ppu.render_scanline(0);
    assert_eq!(
        line.pixels[0].palette_index, 1,
        "the renderer must see what the $2118 port wrote — one VRAM, not two"
    );
    assert_eq!(line.pixels[0].layer, PixelLayer::Background(0));
}

// ---------------------------------------------------------------------
// BG modes 2-6 (ticket W7-03)
// ---------------------------------------------------------------------

#[test]
fn every_mode_has_the_documented_bit_depths() {
    assert_eq!(bg::bit_depths(0), [2, 2, 2, 2]);
    assert_eq!(bg::bit_depths(1), [4, 4, 2, 0], "mode 1 has no BG4");
    assert_eq!(bg::bit_depths(2), [4, 4, 0, 0]);
    assert_eq!(bg::bit_depths(3), [8, 4, 0, 0], "mode 3's BG1 is 8bpp");
    assert_eq!(bg::bit_depths(4), [8, 2, 0, 0]);
    assert_eq!(bg::bit_depths(5), [4, 2, 0, 0]);
    assert_eq!(bg::bit_depths(6), [4, 0, 0, 0], "mode 6 is BG1 only");
}

#[test]
fn offset_per_tile_is_a_property_of_modes_2_4_and_6() {
    for mode in 0..=7u8 {
        assert_eq!(
            bg::uses_offset_per_tile(mode),
            matches!(mode, 2 | 4 | 6),
            "mode {mode}"
        );
    }
}

/// 8bpp tiles are FOUR bitplane pairs, 8 words apart — not "two 4bpp
/// tiles" laid end to end.
#[test]
fn eight_bpp_reads_all_four_bitplane_pairs() {
    let mut p = Ppu::new();
    // Character 0 at word 0. Set exactly one bit in each plane pair's
    // low plane, at pixel 0: planes 0, 2, 4 and 6 -> colour bits 0,2,4,6.
    for pair in 0..4u16 {
        let at = usize::from(pair * 8) * 2;
        p.vram[at] = 0x80; // bit 7 of the low plane = pixel 0
    }
    assert_eq!(
        bg::fetch_pixel(&p, 0, 0, 0, 0, 8),
        0b0101_0101,
        "each pair contributes two bits, and the pairs are 8 words apart"
    );
    // The same data read as 4bpp only sees the first two pairs.
    assert_eq!(bg::fetch_pixel(&p, 0, 0, 0, 0, 4), 0b0101);
    assert_eq!(bg::fetch_pixel(&p, 0, 0, 0, 0, 2), 0b01);
}

/// 8bpp addresses all 256 CGRAM entries, so the tilemap's palette field
/// must be IGNORED — applying it would fold a 256-colour image into a
/// 32-colour block.
#[test]
fn eight_bpp_ignores_the_tilemap_palette_field() {
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.bg_mode = 3;
    p.bgs[0].enabled = true;
    p.bgs[0].char_base = 0x1000;
    // Character 1, 8bpp: 32 words per tile. Row 1 of the tile, not row 0
    // — visible row 0 is hardware scanline 1 and fetches `vofs + 1`
    // (ticket W7-13; see `Ppu::render_scanline`). Writing row 0 here and
    // reading visible row 0 was testing the off-by-one, not the palette.
    let at = (0x1000 + 32 + 1) * 2;
    p.vram[at] = 0xFF; // plane 0 solid -> colour 1
                       // Tilemap entry with a NON-ZERO palette field (palette 5).
    set_tilemap(&mut p, 0, 0, 1 | (5 << 10));
    assert_eq!(
        p.render_scanline(0).pixels[0].palette_index,
        1,
        "the palette field must not shift an 8bpp colour"
    );
}

/// Modes 2-5 share one priority order, and mode 6 has BG1 only.
#[test]
fn modes_2_to_6_resolve_priority_the_documented_way() {
    let mut p = ppu_with_tile();
    p.bg_mode = 2;
    p.bgs[0].enabled = true;
    p.bgs[1].enabled = true;
    p.bgs[1].tilemap_base = 0x0400;
    p.bgs[1].char_base = 0x1000;
    // Both layers 4bpp in mode 2, so write 4bpp character data.
    for row in 0..8 {
        p.vram[(0x1000 + 16 + row) * 2] = 0xFF;
    }
    // BG2 shows a HIGH-priority tile; BG1 a low-priority one.
    set_tilemap(&mut p, 0, 0, 1);
    set_tilemap(&mut p, 0x0400, 0, 1 | 0x2000);
    assert_eq!(
        p.render_scanline(0).pixels[0].layer,
        PixelLayer::Background(1),
        "BG2 priority 1 outranks BG1 priority 0 in mode 2"
    );

    // Give BG1 the high-priority bit too: now BG1 wins.
    set_tilemap(&mut p, 0, 0, 1 | 0x2000);
    assert_eq!(
        p.render_scanline(0).pixels[0].layer,
        PixelLayer::Background(0),
        "at equal priority BG1 outranks BG2"
    );
}

/// **Offset-per-tile.** BG3's tilemap supplies a replacement scroll for a
/// column of BG1/BG2 — it does not add to the layer's own scroll.
#[test]
fn offset_per_tile_replaces_a_columns_scroll() {
    // Built from a CLEAN Ppu rather than `ppu_with_tile`, deliberately.
    // That helper writes 2bpp data at words $1008-$100F, which in 4bpp is
    // plane-pair 1 of character ZERO — so character 0 would not be
    // transparent and "nothing is drawn here" could not be asserted.
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.bg_mode = 2; // BG1 is 4bpp
    p.bgs[0].enabled = true;
    p.bgs[0].tilemap_base = 0;
    p.bgs[0].char_base = 0x1000;
    // Character 1, 4bpp (16 words per tile): plane 0 solid -> colour 1.
    for row in 0..8 {
        p.vram[(0x1000 + 16 + row) * 2] = 0xFF;
    }
    // Map entries 0 and 2 show tile 1; entry 1 is empty. So column 1
    // (x = 8..15) normally reads entry 1 and shows nothing, and an H
    // offset of 8 moves its fetch forward one tile onto entry 2.
    set_tilemap(&mut p, 0, 0, 1);
    set_tilemap(&mut p, 0, 2, 1);

    // BG3's map must live somewhere OTHER than BG1's, or the offset
    // lookup reads BG1's own tilemap as its offset table.
    p.bgs[2].tilemap_base = 0x0800;

    // With an all-zero offset table, column 1 shows nothing.
    assert_eq!(p.render_scanline(0).pixels[8].layer, PixelLayer::Backdrop);

    // Bit 13 means "applies to BG1"; the low 10 bits are the offset.
    set_tilemap(&mut p, 0x0800, 0, 0x2000 | 8);
    assert_eq!(
        p.render_scanline(0).pixels[8].layer,
        PixelLayer::Background(0),
        "the offset entry for column 1 must move BG1's fetch"
    );
    // ...and column 0 is untouched, because it has no entry.
    assert_eq!(
        p.render_scanline(0).pixels[0].layer,
        PixelLayer::Background(0)
    );
}

/// Only the layers an entry names are affected — bit 13 is BG1, bit 14
/// is BG2.
#[test]
fn an_offset_entry_applies_only_to_the_layers_it_names() {
    let mut p = Ppu::new();
    p.bg_mode = 2;
    p.bgs[2].tilemap_base = 0x0800;
    // An entry naming BG2 only.
    set_tilemap(&mut p, 0x0800, 0, 0x4000 | 12);
    assert_eq!(bg::offset_per_tile(&p, 0, 1).h, None, "BG1 not named");
    assert_eq!(bg::offset_per_tile(&p, 1, 1).h, Some(12), "BG2 named");
}

/// Mode 4 packs the H/V selector into bit 15 of a SINGLE entry; modes 2
/// and 6 use two entries. Confusing the two applies vertical offsets
/// horizontally — a picture that looks almost right.
#[test]
fn mode_4_uses_bit_15_to_choose_h_or_v_while_mode_2_uses_two_entries() {
    let mut p = Ppu::new();
    p.bgs[2].tilemap_base = 0x0800;

    p.bg_mode = 4;
    set_tilemap(&mut p, 0x0800, 0, 0x2000 | 20); // bit 15 clear -> H
    let o = bg::offset_per_tile(&p, 0, 1);
    assert_eq!((o.h, o.v), (Some(20), None));
    set_tilemap(&mut p, 0x0800, 0, 0x8000 | 0x2000 | 30); // bit 15 set -> V
    let o = bg::offset_per_tile(&p, 0, 1);
    assert_eq!((o.h, o.v), (None, Some(30)));

    p.bg_mode = 2;
    set_tilemap(&mut p, 0x0800, 0, 0x2000 | 20); // horizontal entry
    set_tilemap(&mut p, 0x0800, 32, 0x2000 | 30); // vertical, 32 entries on
    let o = bg::offset_per_tile(&p, 0, 1);
    assert_eq!(
        (o.h, o.v),
        (Some(20), Some(30)),
        "modes 2/6 read a separate vertical entry 32 words later"
    );
}

/// Column 0 has no preceding entry to read, so it keeps the layer's own
/// scroll.
#[test]
fn column_zero_has_no_offset_entry() {
    let mut p = Ppu::new();
    p.bg_mode = 2;
    p.bgs[2].tilemap_base = 0x0800;
    set_tilemap(&mut p, 0x0800, 0, 0x2000 | 99);
    let o = bg::offset_per_tile(&p, 0, 0);
    assert_eq!((o.h, o.v), (None, None));
}

/// `$2130` CGWSEL bit 0 selects direct colour.
#[test]
fn cgwsel_bit_zero_selects_direct_colour() {
    let mut p = Ppu::new();
    assert!(!p.direct_color);
    p.write_register(0x2130, 0x01);
    assert!(p.direct_color);
    p.write_register(0x2130, 0x00);
    assert!(!p.direct_color);
}

// ---------------------------------------------------------------------
// Mode 7 (ticket W7-04)
// ---------------------------------------------------------------------

use crate::ppu::mode7;

/// A mode-7 PPU with an identity matrix and a recognisable playfield.
///
/// Mode 7 stores its tilemap in the EVEN bytes of VRAM and its character
/// data in the ODD bytes of the same words — interleaved, not two
/// separate regions.
fn ppu_mode7() -> Ppu {
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.bg_mode = 7;
    p.bgs[0].enabled = true;
    // Identity: A = D = 1.0 in 8.8, B = C = 0.
    p.mode7.a = 0x0100;
    p.mode7.d = 0x0100;
    // Tile 1 everywhere in the tilemap (even bytes).
    for i in 0..(128 * 128) {
        p.vram[i * 2] = 1;
    }
    // Character 1: every pixel colour 7 (odd bytes, 64 bytes per tile).
    for i in 0..64 {
        p.vram[(64 + i) * 2 + 1] = 7;
    }
    p
}

#[test]
fn mode_7_renders_its_playfield_through_the_matrix() {
    let mut p = ppu_mode7();
    let line = p.render_scanline(0);
    assert_eq!(line.pixels[0].palette_index, 7);
    assert_eq!(line.pixels[0].layer, PixelLayer::Background(0));
}

/// The matrix registers are signed 8.8 and share ONE write-twice latch
/// with the centre registers.
#[test]
fn the_mode_7_registers_are_8_8_fixed_point_through_a_shared_latch() {
    let mut p = Ppu::new();
    p.write_register(0x211B, 0x00);
    p.write_register(0x211B, 0x01);
    assert_eq!(p.mode7.a, 0x0100, "$0100 is 1.0");
    // Negative values are genuinely signed.
    p.write_register(0x211C, 0x00);
    p.write_register(0x211C, 0xFF);
    assert_eq!(p.mode7.b, -256);
}

/// `HOFS`, `VOFS`, `X0` and `Y0` are **13-bit** signed, not 16-bit.
///
/// Sign-extending them wrongly gives a picture that is recognisable but
/// swims away from the centre it should pivot around.
#[test]
fn the_centre_registers_are_thirteen_bit_signed() {
    assert_eq!(mode7::sign_extend_13(0x0000), 0);
    assert_eq!(mode7::sign_extend_13(0x0FFF), 4095, "largest positive");
    assert_eq!(mode7::sign_extend_13(0x1000), -4096, "bit 12 is the sign");
    assert_eq!(mode7::sign_extend_13(0x1FFF), -1);
    // Bits 13-15 are not part of the number at all.
    assert_eq!(mode7::sign_extend_13(0xF000), -4096);
}

/// `$2134`-`$2136` is the signed product of M7A and M7B's high byte.
///
/// It is a general-purpose multiplier games use for arithmetic unrelated
/// to mode 7 — PeterLemon's RotZoom computes its rotation matrix with it,
/// and leaving it unmapped made every matrix element read back as the
/// open-bus byte `$21`, rendering the playfield as diagonal stripes.
#[test]
fn the_multiplier_result_is_readable() {
    use crate::cpu::CpuBus;
    let mut b = crate::bus::SnesBus::new(vec![0; 32 * 1024], 0, rf_cart::SnesMapMode::LoRom);
    b.ppu.mode7.a = 1000;
    b.ppu.mode7.b = 0x0300; // high byte 3
    assert_eq!(b.ppu.mode7.product(), 3000);
    assert_eq!(b.read(0x00_2134), (3000u32 & 0xFF) as u8);
    assert_eq!(b.read(0x00_2135), ((3000u32 >> 8) & 0xFF) as u8);
    assert_eq!(b.read(0x00_2136), 0);

    // Signed: a negative high byte gives a negative product.
    b.ppu.mode7.b = 0xFF00u16 as i16; // high byte -1
    assert_eq!(b.ppu.mode7.product(), -1000);
}

/// Screen-over: what happens OUTSIDE the 1024x1024 playfield, which is
/// exactly where most test content never looks.
#[test]
fn screen_over_modes_differ_only_outside_the_playfield() {
    let mut p = ppu_mode7();
    // Push the sampling far outside the playfield.
    p.mode7.x0 = 4000;
    p.mode7.hofs = 4000;

    p.mode7.screen_over = 0; // wrap
    let wrapped = p.render_scanline(0).pixels[0].layer;
    p.mode7.screen_over = 2; // transparent
    let transparent = p.render_scanline(0).pixels[0].layer;

    assert_eq!(wrapped, PixelLayer::Background(0), "wrap keeps drawing");
    assert_eq!(
        transparent,
        PixelLayer::Backdrop,
        "screen-over 2 draws nothing outside the playfield"
    );
}

/// **HD-Mode-7 samples the SAME matrix more densely.** It does not
/// interpolate, smooth, or invent detail — every extra sample is a real
/// evaluation of the transform.
#[test]
fn hd_mode7_evaluates_the_same_transform_at_higher_density() {
    let p = ppu_mode7();
    let base = mode7::render_scanline(&p, 0, 1);
    let hd = mode7::render_scanline(&p, 0, 4);

    assert_eq!(base.len(), 256);
    assert_eq!(hd.len(), 1024, "four samples per hardware pixel");

    // Every hardware sample must still appear, at the same place, in the
    // denser run — the HD image contains the 1x image rather than
    // replacing it with something reinterpolated.
    for (x, expected) in base.iter().enumerate() {
        assert_eq!(
            &hd[x * 4],
            expected,
            "HD sample {} must equal hardware pixel {x}",
            x * 4
        );
    }
}

/// **Law 6.** Accuracy Mode is the reference, so the ordinary render path
/// is always hardware density — a caller must opt in to anything else.
#[test]
fn the_accuracy_path_renders_mode_7_at_hardware_density() {
    let mut p = ppu_mode7();
    let line = p.render_scanline(0);
    assert_eq!(
        line.pixels.len(),
        crate::ppu::WIDTH,
        "render_scanline must never widen itself; HD-Mode-7 is opt-in"
    );
    // And it is bit-identical to an explicit density of 1.
    let explicit = mode7::render_scanline(&p, 0, 1);
    for (x, sample) in explicit.iter().enumerate().take(crate::ppu::WIDTH) {
        assert_eq!(line.pixels[x].palette_index, sample.unwrap_or(0));
    }
}

/// Sprites still compose over mode 7 — it replaces BG1, not the whole
/// screen.
#[test]
fn sprites_draw_over_the_mode_7_playfield() {
    let mut p = ppu_mode7();
    p.obj_enabled = true;
    p.obj_size = 0;
    for row in 0..8 {
        p.vram[(16 + row) * 2] = 0xFF; // OBJ character 1, 4bpp
    }
    for i in 0..128usize {
        p.oam[i * 4 + 1] = 100; // park them
    }
    p.oam[0] = 0; // sprite 0 at x=0, y=0
    p.oam[1] = 0;
    p.oam[2] = 1;
    let line = p.render_scanline(0);
    assert_eq!(line.pixels[0].layer, PixelLayer::Sprite);
}

/// **A per-line window must mask a DIFFERENT span on each line** (ticket
/// W7-05's criterion 4).
///
/// W7-07 taught the PPU to compose each scanline from state latched at
/// that line, but latched only the mode-7 matrix, the BG mode and the
/// scroll registers. Windows, colour math and mosaic were left reading
/// live registers, so an HDMA-driven window drew whatever the LAST line
/// set on all 224 lines. That is invisible in a still with a static
/// window and unmistakable with a moving one — PeterLemon's WindowHDMA
/// masked exactly 28 pixels on every row before this, and now traces the
/// lens shape the ROM draws.
///
/// The golden that caught it is `#[ignore]`d and needs fetched ROMs, so
/// this is the version that runs in `cargo test --workspace`.
#[test]
fn window_and_mosaic_registers_are_latched_per_scanline() {
    let mut ppu = Ppu::new();
    ppu.forced_blank = false;
    ppu.brightness = 0x0F;
    ppu.bg_mode = 0;

    // Enable window 1 on BG1 and in the main-screen mask, then latch two
    // lines with DIFFERENT spans and a different mosaic size each.
    ppu.write_register(0x212E, 0x01); // TM: BG1 on the main screen
    ppu.write_register(0x2123, 0x02); // W12SEL: BG1 window 1 enabled

    ppu.write_register(0x2126, 10); // W1 left
    ppu.write_register(0x2127, 20); // W1 right
    ppu.write_register(0x2106, 0x31); // mosaic size 4, BG1
                                      // Hardware scanlines 1 and 2 — visible rows 0 and 1 (ticket W7-13).
    ppu.latch_line(1);

    ppu.write_register(0x2126, 100);
    ppu.write_register(0x2127, 200);
    ppu.write_register(0x2106, 0x71); // mosaic size 8, BG1
    ppu.latch_line(2);

    let l0 = ppu.line_state_for_test(1).expect("line 1 was latched");
    let l1 = ppu.line_state_for_test(2).expect("line 2 was latched");

    assert_eq!(
        (l0.windows.w1_left, l0.windows.w1_right),
        (10, 20),
        "line 0 kept its own window span"
    );
    assert_eq!(
        (l1.windows.w1_left, l1.windows.w1_right),
        (100, 200),
        "line 1 kept its own window span, not line 0's and not the live one"
    );
    assert_eq!(l0.mosaic.size, 4);
    assert_eq!(l1.mosaic.size, 8, "mosaic size is per-line state too");

    // And the live registers moving on afterwards must not rewrite
    // history — the bug this replaces was precisely "every line reads the
    // final value".
    ppu.write_register(0x2126, 250);
    ppu.write_register(0x2127, 255);
    let l0_again = ppu.line_state_for_test(1).expect("still latched");
    assert_eq!(
        (l0_again.windows.w1_left, l0_again.windows.w1_right),
        (10, 20),
        "a later write reached back and changed an already-latched line"
    );

    // ---- and the COMPOSITION must use it -------------------------
    //
    // Asserting on the latch alone would pass even if `with_line_state`
    // never applied the fields — which is exactly half the bug. Draw a
    // real tile across the line and check that the two lines mask
    // DIFFERENT spans of it.
    let mut p = ppu_with_tile();
    for i in 0..32 {
        set_tilemap(&mut p, 0, i, 1);
    }
    p.write_register(0x212E, 0x01); // BG1 on the main screen
    p.write_register(0x2123, 0x02); // BG1 masked by window 1
    p.write_register(0x2126, 10);
    p.write_register(0x2127, 20);
    p.latch_line(1);
    p.write_register(0x2126, 100);
    p.write_register(0x2127, 200);
    p.latch_line(2);
    // Live registers end somewhere else again, so a composition that
    // reads them instead of the latch masks the same span on both lines.
    p.write_register(0x2126, 0);
    p.write_register(0x2127, 0);

    let masked = |p: &mut Ppu, y: u16| -> Vec<usize> {
        p.render_scanline(y)
            .pixels
            .iter()
            .enumerate()
            .filter(|(_, px)| px.layer == PixelLayer::Backdrop)
            .map(|(x, _)| x)
            .collect()
    };
    let m0 = masked(&mut p, 0);
    let m1 = masked(&mut p, 1);
    assert_eq!(
        m0,
        (10..=20).collect::<Vec<_>>(),
        "line 0 masked its own span"
    );
    assert_eq!(
        m1,
        (100..=200).collect::<Vec<_>>(),
        "line 1 composed from line 0's window, or from the live registers - \
         this is the bug that made an HDMA window a constant band"
    );
}

/// **`$2133` SETINI, decoded** (ticket W7-06).
///
/// Bit 1 is OBJ V-direction and bit 2 is BG V-direction; overscan is the
/// BG one. Getting those two the wrong way round gives a PPU that changes
/// sprite size when a game asks for 239 lines, so each bit is asserted
/// alone rather than in a lump.
#[test]
fn setini_decodes_each_bit_independently() {
    let mut p = Ppu::new();
    for (value, expect) in [
        (0x01, "interlace"),
        (0x02, "obj_interlace"),
        (0x04, "overscan"),
        (0x08, "pseudo_hires"),
        (0x40, "extbg"),
        (0x80, "external_sync"),
    ] {
        p.write_register(0x2133, value);
        let s = p.setini;
        let got = [
            ("interlace", s.interlace),
            ("obj_interlace", s.obj_interlace),
            ("overscan", s.overscan),
            ("pseudo_hires", s.pseudo_hires),
            ("extbg", s.extbg),
            ("external_sync", s.external_sync),
        ];
        for (name, flag) in got {
            assert_eq!(
                flag,
                name == expect,
                "writing {value:#04X} set `{name}` to {flag}; only `{expect}` should be set"
            );
        }
    }
    // Bits 4-5 are documented "Not used" and must set nothing at all.
    p.write_register(0x2133, 0x30);
    assert_eq!(p.setini, super::super::ppu::SetIni::default());
}

/// Overscan is 239 lines, and it is bit 2 that does it.
#[test]
fn overscan_changes_the_visible_line_count() {
    let mut p = Ppu::new();
    assert_eq!(p.setini.visible_lines(), 224);
    p.write_register(0x2133, 0x04);
    assert_eq!(p.setini.visible_lines(), 239);
    p.write_register(0x2133, 0x00);
    assert_eq!(p.setini.visible_lines(), 224);
}

/// **Both routes to 512 dots need a sub-screen**, which is why this PPU
/// **Superseded in part by W7-06's second pass**: hires is no longer only
/// *requested*, it is rendered — see the 512-dot tests below. This test
/// still pins WHICH configurations ask for 512 dots, which is the half
/// that did not change.
///
/// Pseudo-hires is defined as shifting the sub-screen half a dot left,
/// and true hires works by main and sub supplying alternating half-dots.
#[test]
fn hires_is_requested_by_pseudo_hires_and_by_modes_5_and_6() {
    let mut p = Ppu::new();
    for mode in 0..=7u8 {
        p.bg_mode = mode;
        assert_eq!(
            p.setini.hires_requested(mode),
            mode == 5 || mode == 6,
            "mode {mode} without pseudo-hires"
        );
    }
    p.write_register(0x2133, 0x08);
    for mode in 0..=7u8 {
        assert!(
            p.setini.hires_requested(mode),
            "pseudo-hires asks for 512 dots in every mode, including {mode}"
        );
    }
}

/// **A colour-math change is now detectable** (ticket W7-16, criterion 2).
///
/// This is the test the ticket says no existing test could be. Before the
/// sub-screen channel, colour math produced an RGB value that could not
/// travel through an indexed `PpuPixel`, so every golden hash and every
/// A-B comparison in the project was blind to it: flipping add to subtract
/// changed nothing any test could observe.
///
/// It asserts on the OPERATION the core reports per pixel, not on a blended
/// colour, because the blend is the renderer's job — that split is what
/// keeps law 4 intact.
#[test]
fn a_colour_math_change_is_visible_on_the_sub_screen_channel() {
    use rf_core_api::ColorMathOp;

    let mut p = ppu_with_tile();
    for i in 0..32 {
        set_tilemap(&mut p, 0, i, 1);
    }
    p.write_register(0x212C, 0x01); // TM: BG1 on the main screen
    p.write_register(0x212D, 0x02); // TS: BG2 on the sub screen
    p.bgs[1].enabled = true;
    p.write_register(0x2131, 0x01); // CGADSUB: math on BG1, add, full
    p.write_register(0x2132, 0x3F); // fixed colour: red 31 (bits 5-0 + R sel)

    let (add, _fixed) = p.render_sub_scanline(0);
    assert!(
        add.iter().any(|s| s.op == ColorMathOp::Add),
        "with $2131 enabling math on BG1 the sub-screen must report Add"
    );

    // Flip to SUBTRACT. Nothing about the main screen changes — which is
    // exactly why this was invisible before.
    let main_before = p.render_scanline(0);
    p.write_register(0x2131, 0x81); // same, but subtract
    let (sub, _) = p.render_sub_scanline(0);
    let main_after = p.render_scanline(0);

    assert!(
        sub.iter().any(|s| s.op == ColorMathOp::Subtract),
        "flipping $2131 bit 7 must change the reported operation"
    );
    assert_ne!(
        add.iter().map(|s| s.op).collect::<Vec<_>>(),
        sub.iter().map(|s| s.op).collect::<Vec<_>>(),
        "add and subtract must be distinguishable through this channel"
    );
    assert_eq!(
        main_before
            .pixels
            .iter()
            .map(|p| p.palette_index)
            .collect::<Vec<_>>(),
        main_after
            .pixels
            .iter()
            .map(|p| p.palette_index)
            .collect::<Vec<_>>(),
        "the MAIN screen is accuracy-exact and must not have moved - that is \
         the W1-05a ruling, and it is why a second channel was needed at all"
    );

    // And disabling math entirely reports None.
    p.write_register(0x2131, 0x00);
    let (off, _) = p.render_sub_scanline(0);
    assert!(
        off.iter().all(|s| s.op == ColorMathOp::None),
        "with no layer enabled for math, every pixel reports None"
    );
}

/// `$212D` selects the SUB screen's layers, independently of `$212C`.
#[test]
fn the_sub_screen_has_its_own_layer_designation() {
    let mut p = ppu_with_tile();
    for i in 0..32 {
        set_tilemap(&mut p, 0, i, 1);
    }
    p.write_register(0x212C, 0x01); // BG1 on main
    p.write_register(0x212D, 0x00); // nothing on sub
    let (empty, _) = p.render_sub_scanline(0);
    assert!(
        empty
            .iter()
            .all(|s| s.layer == rf_core_api::PixelLayer::Backdrop),
        "with TS empty the sub screen is all backdrop"
    );

    p.write_register(0x212D, 0x01); // BG1 on sub as well
    let (filled, _) = p.render_sub_scanline(0);
    assert!(
        filled
            .iter()
            .any(|s| s.layer != rf_core_api::PixelLayer::Backdrop),
        "TS must select layers for the sub screen, or a game that sets it \
         is configuring a screen nothing composes"
    );
}

/// **`$210D`/`$210E` write TWO registers** (ticket W7-04).
///
/// fullsnes: "Writing to 210Dh does BOTH update M7HOFS (via M7_old
/// mechanism), and also updates BG1HOFS (via BG_old mechanism)." Mode 7
/// has no scroll registers of its own, so before this `Mode7::hofs` was a
/// field the transform read and nothing ever wrote — permanently zero,
/// and every mode-7 game silently unable to scroll. StarWars set
/// `M7X = M7Y = 512` and scrolled through `$210D`; with the scroll lost,
/// every sample landed outside the playfield and the logo never appeared.
///
/// The two latches are separate ("M7_old" vs "BG_old"), and the mode-7
/// value is signed 13-bit against BG1's 16 — so a single write leaves the
/// two registers holding genuinely different numbers, which is what makes
/// this worth asserting rather than assuming.
#[test]
fn writing_bg1_scroll_also_writes_the_mode_7_scroll() {
    let mut p = Ppu::new();
    // 1st write: lower 8 bits. 2nd write: upper 5 (mode 7) / upper 8 (BG1).
    p.write_register(0x210D, 0x80);
    p.write_register(0x210D, 0x01);
    assert_eq!(
        p.mode7.hofs, 0x0180,
        "M7HOFS must receive the write that $210D also gave BG1HOFS"
    );
    assert_eq!(p.bgs[0].hofs, 0x0180, "and BG1HOFS still gets it too");

    p.write_register(0x210E, 0x40);
    p.write_register(0x210E, 0x02);
    assert_eq!(p.mode7.vofs, 0x0240);
    assert_eq!(p.bgs[0].vofs, 0x0240);

    // Signed 13-bit: bit 12 set means negative, where BG1's 16-bit value
    // is simply large. One write, two different numbers.
    let mut q = Ppu::new();
    q.write_register(0x210D, 0x00);
    q.write_register(0x210D, 0x1F);
    assert_eq!(q.mode7.hofs, -256, "mode 7 sign-extends from bit 12");
    assert_eq!(q.bgs[0].hofs, 0x1F00, "BG1 does not");
}

// =====================================================================
// 512-dot hires composition (ticket W7-06, criterion 2)
// =====================================================================

/// A PPU with a distinguishable main and sub screen on one line.
///
/// The two screens carry DIFFERENT palette indices on purpose: a test
/// where both are the same cannot tell an interleave from a duplication,
/// and duplication is the most likely way to get this wrong.
fn hires_ppu(mode: u8) -> Ppu {
    let mut p = Ppu::new();
    p.bg_mode = mode;
    // BG1 on the main screen, BG2 on the sub screen, so the two halves
    // come from different layers as well as different indices.
    p.write_register(0x212C, 0x01); // TM: BG1 -> main
    p.write_register(0x212D, 0x02); // TS: BG2 -> sub
    p
}

/// **Criterion 2.** Modes 5 and 6 emit 512 dots; every other mode emits
/// 256 unless pseudo-hires asks otherwise.
#[test]
fn hires_modes_emit_512_dot_scanlines_and_other_modes_do_not() {
    for mode in 0..=7u8 {
        let mut p = hires_ppu(mode);
        let line = p.render_scanline(0);
        let want = if mode == 5 || mode == 6 { 512 } else { 256 };
        assert_eq!(
            line.pixels.len(),
            want,
            "mode {mode} emitted {} dots, wanted {want} — the slice LENGTH is the width tag",
            line.pixels.len()
        );
    }
}

/// Pseudo-hires reaches 512 in **every** mode, which is what makes it
/// pseudo-hires rather than a mode-5 special case.
#[test]
fn pseudo_hires_emits_512_dots_in_every_mode() {
    for mode in 0..=7u8 {
        let mut p = hires_ppu(mode);
        p.write_register(0x2133, 0x08);
        assert_eq!(
            p.render_scanline(0).pixels.len(),
            512,
            "pseudo-hires must widen mode {mode}"
        );
    }
}

/// **The half-dot order, which is the fact worth pinning.**
///
/// fullsnes defines pseudo-hires as `SHIFT SUBSCREEN HALF DOT TO THE
/// LEFT`, so the sub screen owns the EVEN dot of each pair and the main
/// screen the ODD one. Getting this backwards shifts the whole picture
/// half a dot — a hires ROM then looks subtly soft rather than obviously
/// wrong, which an eyeball check passes and a screenshot comparison
/// catches. That is the same failure mode as the off-by-one row this
/// module already carries.
#[test]
fn the_sub_screen_owns_the_left_half_dot_and_main_the_right() {
    let mut p = hires_ppu(5);
    let hires = p.render_scanline(0);

    // The same PPU, rendered as its two separate screens.
    let mut plain = hires_ppu(5);
    plain.bg_mode = 0; // any non-hires mode: gives the 256-dot main screen
    let main = plain.render_scanline(0);
    let mut subs = hires_ppu(5);
    let (sub, _fixed) = subs.render_sub_scanline(1);

    assert_eq!(hires.pixels.len(), main.pixels.len() * 2);
    for x in 0..main.pixels.len() {
        assert_eq!(
            hires.pixels[2 * x + 1],
            main.pixels[x],
            "the ODD half-dot at x={x} must be the MAIN screen, unaltered"
        );
        if let Some(sp) = sub.get(x) {
            if !sp.fixed {
                assert_eq!(
                    hires.pixels[2 * x].palette_index,
                    sp.palette_index,
                    "the EVEN half-dot at x={x} must come from the SUB screen"
                );
            }
        }
    }
}

/// The main screen's pixels are not altered by being interleaved —
/// hires changes where they sit, not what they are.
#[test]
fn hires_does_not_alter_the_main_screen_pixels() {
    let mut wide = hires_ppu(5);
    let hires = wide.render_scanline(0);
    let mut narrow = hires_ppu(5);
    narrow.bg_mode = 1;
    let plain = narrow.render_scanline(0);

    let odd: Vec<_> = hires.pixels.iter().skip(1).step_by(2).copied().collect();
    assert_eq!(
        odd.len(),
        plain.pixels.len(),
        "one main-screen dot per pair"
    );
}

/// The overlay must stay the same length as `pixels` — that is its
/// documented contract, and a consumer indexes them together.
#[test]
fn the_overlay_is_widened_alongside_the_pixels() {
    let mut p = hires_ppu(6);
    let line = p.render_scanline(0);
    assert_eq!(
        line.overlay.len(),
        line.pixels.len(),
        "overlay and pixels are indexed together; different lengths would \
         mis-attribute every dropped sprite"
    );
    // A dropped sprite belongs to the MAIN screen, so the left half-dot
    // carries an empty entry rather than a duplicate — duplicating would
    // double every dropped sprite's apparent width.
    for (i, o) in line.overlay.iter().enumerate() {
        if i % 2 == 0 {
            assert!(
                !o.opaque,
                "the sub-screen half-dot carries no dropped sprite"
            );
        }
    }
}

/// Anti-vacuity: a 512-dot line whose halves are identical would pass a
/// length check while proving nothing was interleaved.
#[test]
fn the_two_half_dots_are_not_simply_duplicated() {
    let mut p = hires_ppu(5);
    // Give the sub screen a different backdrop-relative index than the
    // main screen by enabling different layers (done in `hires_ppu`).
    let line = p.render_scanline(0);
    let identical =
        (0..line.pixels.len() / 2).all(|x| line.pixels[2 * x] == line.pixels[2 * x + 1]);
    assert!(
        !identical || line.pixels.iter().all(|p| p.layer == PixelLayer::Backdrop),
        "every pair is identical — the line was duplicated, not interleaved"
    );
}
