//! Windows, colour math and mosaic (ticket W7-05).

use crate::ppu::window::{ColorMath, Mosaic, WindowLogic, Windows};
use crate::ppu::Ppu;
use rf_core_api::PixelLayer;

fn windows_with_two_spans() -> Windows {
    let mut w = Windows::default();
    w.write_register(0x2126, 10); // W1: 10..=20
    w.write_register(0x2127, 20);
    w.write_register(0x2128, 15); // W2: 15..=25
    w.write_register(0x2129, 25);
    w
}

/// A layer with NEITHER window enabled is never masked.
#[test]
fn a_layer_with_no_window_enabled_is_never_masked() {
    let w = windows_with_two_spans();
    for x in 0..=255u8 {
        assert!(!w.masks(0, x), "x={x}");
    }
}

/// **The logic operation only matters when BOTH windows are enabled.**
///
/// With one enabled, all four combiners agree — which is exactly why an
/// implementation that hard-codes OR passes most content and fails the
/// rest. These cases drive both-enabled specifically.
#[test]
fn the_four_window_combiners_differ_only_with_both_windows_enabled() {
    let mut w = windows_with_two_spans();
    w.enable[0] = (true, true);

    // x = 12: inside W1 (10-20), outside W2 (15-25).
    // x = 18: inside both.
    // x = 23: outside W1, inside W2.
    // x = 40: outside both.
    let probe = |w: &Windows| {
        (
            w.masks(0, 12),
            w.masks(0, 18),
            w.masks(0, 23),
            w.masks(0, 40),
        )
    };

    w.logic[0] = WindowLogic::Or;
    assert_eq!(probe(&w), (true, true, true, false));
    w.logic[0] = WindowLogic::And;
    assert_eq!(probe(&w), (false, true, false, false));
    w.logic[0] = WindowLogic::Xor;
    assert_eq!(probe(&w), (true, false, true, false));
    w.logic[0] = WindowLogic::Xnor;
    assert_eq!(probe(&w), (false, true, false, true));
}

/// With ONE window enabled the combiner is irrelevant — the
/// counter-test for the one above.
#[test]
fn with_one_window_enabled_every_combiner_agrees() {
    let mut w = windows_with_two_spans();
    w.enable[0] = (true, false);
    for logic in [
        WindowLogic::Or,
        WindowLogic::And,
        WindowLogic::Xor,
        WindowLogic::Xnor,
    ] {
        w.logic[0] = logic;
        assert!(w.masks(0, 12), "{logic:?}");
        assert!(!w.masks(0, 40), "{logic:?}");
    }
}

/// Inverting a window flips the membership test — it is not "the other
/// side of the span". An inverted EMPTY span therefore covers
/// everything.
#[test]
fn inverting_a_window_flips_membership_not_the_span() {
    let mut w = windows_with_two_spans();
    w.enable[0] = (true, false);
    w.invert[0] = (true, false);
    assert!(!w.masks(0, 12), "inside the span, inverted -> not masked");
    assert!(w.masks(0, 40), "outside the span, inverted -> masked");

    // An empty span (left > right) inverted covers the whole line.
    let mut e = Windows::default();
    e.write_register(0x2126, 200);
    e.write_register(0x2127, 100);
    e.enable[0] = (true, false);
    e.invert[0] = (true, false);
    for x in [0u8, 50, 150, 255] {
        assert!(e.masks(0, x), "an inverted empty span covers everything");
    }
}

/// **A window only masks the main screen if `$212E` says so.** Games
/// routinely configure a window and leave it disabled there; checking
/// only the span makes content vanish.
#[test]
fn a_window_masks_a_layer_only_when_212e_enables_it() {
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.bg_mode = 0;
    p.bgs[0].enabled = true;
    p.bgs[0].char_base = 0x1000;
    for row in 0..8 {
        p.vram[(0x1000 + 8 + row) * 2] = 0xFF;
    }
    for entry in 0..32 {
        let at = entry * 2;
        p.vram[at] = 1;
    }
    // Window 1 covers the whole line, enabled for BG1.
    p.write_register(0x2126, 0);
    p.write_register(0x2127, 255);
    p.write_register(0x2123, 0x02); // BG1: W1 enable
    assert_eq!(
        p.render_scanline(0).pixels[0].layer,
        PixelLayer::Background(0),
        "with $212E clear the window must not mask anything"
    );

    p.write_register(0x212E, 0x01); // apply BG1's mask on the main screen
    assert_eq!(
        p.render_scanline(0).pixels[0].layer,
        PixelLayer::Backdrop,
        "now the window removes BG1"
    );
}

/// A masked layer is REMOVED from priority resolution, so a lower layer
/// shows through rather than the screen going blank.
#[test]
fn masking_a_layer_lets_the_one_below_it_show() {
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.bg_mode = 0;
    p.bgs[0].enabled = true;
    p.bgs[1].enabled = true;
    p.bgs[0].char_base = 0x1000;
    p.bgs[1].char_base = 0x1000;
    p.bgs[1].tilemap_base = 0x0400;
    for row in 0..8 {
        p.vram[(0x1000 + 8 + row) * 2] = 0xFF;
    }
    p.vram[0] = 1;
    p.vram[0x0400 * 2] = 1;

    assert_eq!(
        p.render_scanline(0).pixels[0].layer,
        PixelLayer::Background(0)
    );

    p.write_register(0x2126, 0);
    p.write_register(0x2127, 255);
    p.write_register(0x2123, 0x02);
    p.write_register(0x212E, 0x01);
    assert_eq!(
        p.render_scanline(0).pixels[0].layer,
        PixelLayer::Background(1),
        "BG2 shows through where BG1 is masked"
    );
}

// ---------------------------------------------------------------------
// Colour math
// ---------------------------------------------------------------------

/// `$2132` can set any combination of the three components in ONE write.
#[test]
fn the_fixed_colour_register_selects_components_by_bit() {
    let mut c = ColorMath::default();
    c.write_register(0x2132, 0x20 | 15); // red only
    assert_eq!(c.fixed(), (15, 0, 0));
    c.write_register(0x2132, 0x40 | 7); // green only
    assert_eq!(c.fixed(), (15, 7, 0));
    // All three at once.
    c.write_register(0x2132, 0xE0 | 31);
    assert_eq!(c.fixed(), (31, 31, 31));
}

#[test]
fn colour_math_adds_subtracts_and_halves_with_clamping() {
    let mut c = ColorMath::default();
    c.write_register(0x2131, 0x00); // add, no half
    assert_eq!(c.blend((10, 10, 10), (5, 5, 5)), (15, 15, 15));
    assert_eq!(
        c.blend((30, 30, 30), (10, 10, 10)),
        (31, 31, 31),
        "clamps high"
    );

    c.write_register(0x2131, 0x80); // subtract
    assert_eq!(c.blend((10, 10, 10), (4, 4, 4)), (6, 6, 6));
    assert_eq!(c.blend((2, 2, 2), (10, 10, 10)), (0, 0, 0), "clamps low");

    c.write_register(0x2131, 0x40); // add + half
    assert_eq!(c.blend((10, 10, 10), (10, 10, 10)), (10, 10, 10));
}

/// The four clip/prevent modes: never, inside, outside, always.
#[test]
fn the_colour_window_modes_select_where_they_apply() {
    let mut c = ColorMath::default();
    for (mode, inside, outside) in [
        (0u8, false, false),
        (1, true, false),
        (2, false, true),
        (3, true, true),
    ] {
        c.write_register(0x2130, mode << 6);
        assert_eq!(c.clip_to_black(true), inside, "mode {mode} inside");
        assert_eq!(c.clip_to_black(false), outside, "mode {mode} outside");
    }
    c.write_register(0x2130, 1 << 4);
    assert!(c.prevented(true));
    assert!(!c.prevented(false));
}

/// **Clip-to-black reaches the indexed pixel stream**, because black is
/// palette index 0 — unlike the blend, which cannot.
#[test]
fn clip_to_black_forces_the_main_screen_to_the_backdrop() {
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.bg_mode = 0;
    p.bgs[0].enabled = true;
    p.bgs[0].char_base = 0x1000;
    for row in 0..8 {
        p.vram[(0x1000 + 8 + row) * 2] = 0xFF;
    }
    p.vram[0] = 1;
    assert_eq!(
        p.render_scanline(0).pixels[0].layer,
        PixelLayer::Background(0)
    );

    // Colour window covers everything, clip mode "always".
    p.write_register(0x2130, 0x03 << 6);
    assert_eq!(
        p.render_scanline(0).pixels[0].layer,
        PixelLayer::Backdrop,
        "clip-to-black is expressible on the indexed path"
    );
}

// ---------------------------------------------------------------------
// Mosaic
// ---------------------------------------------------------------------

#[test]
fn mosaic_size_is_stored_as_size_minus_one() {
    let mut m = Mosaic::default();
    m.write_register(0x0F); // size nibble 0 -> 1 pixel, all BGs enabled
    assert_eq!(m.size, 1);
    assert_eq!(m.enable, 0x0F);
    m.write_register(0xF1); // nibble 15 -> 16 pixels, BG1 only
    assert_eq!(m.size, 16);
    assert_eq!(m.enable, 0x01);
}

/// Mosaic snaps a coordinate DOWN to its block. Size 1 is a no-op, which
/// is why most content never notices a broken implementation.
#[test]
fn mosaic_snaps_coordinates_to_their_block() {
    let mut m = Mosaic::default();
    m.write_register(0x31); // size 4, BG1 only
    assert_eq!(m.snap(0, 0), 0);
    assert_eq!(m.snap(0, 3), 0);
    assert_eq!(m.snap(0, 4), 4);
    assert_eq!(m.snap(0, 7), 4);
    assert_eq!(m.snap(0, 100), 100);
    assert_eq!(m.snap(1, 3), 3, "BG2 is not enabled");

    m.write_register(0x0F); // size 1
    for v in 0..32u16 {
        assert_eq!(m.snap(0, v), v, "size 1 is a no-op");
    }
}

/// Mosaic actually blockifies the rendered line.
#[test]
fn mosaic_makes_neighbouring_pixels_identical() {
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.bg_mode = 0;
    p.bgs[0].enabled = true;
    p.bgs[0].char_base = 0x1000;
    // A character whose left half is colour 1 and right half colour 2,
    // so neighbouring pixels normally differ.
    for row in 0..8 {
        let at = (0x1000 + 8 + row) * 2;
        p.vram[at] = 0xF0; // plane 0: left four pixels
        p.vram[at + 1] = 0x0F; // plane 1: right four pixels
    }
    p.vram[0] = 1;

    let plain = p.render_scanline(0);
    assert_ne!(
        plain.pixels[3].palette_index, plain.pixels[4].palette_index,
        "without mosaic the halves differ"
    );

    p.write_register(0x2106, 0x71); // size 8, BG1
    let blocky = p.render_scanline(0);
    assert_eq!(
        blocky.pixels[3].palette_index, blocky.pixels[4].palette_index,
        "an 8-pixel mosaic block must be uniform"
    );
}
