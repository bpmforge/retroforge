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
        line.pixels.iter().all(|px| !px.dropped_by_limit),
        "PpuPixel::dropped_by_limit must stay false — the sink is \
         accuracy-exact (rf_core_api::video's W1-05a ruling)"
    );
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
