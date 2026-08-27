//! **The PPU reports the tiles it actually drew** (ticket W11-05).
//!
//! An HD-pack rule is keyed on a tile's identity and palette, so applying
//! one needs to know which tile landed in which screen cell. The obvious
//! cheap route — snapshot the scroll register at frame end and work
//! backwards — is wrong for any game that changes scroll mid-frame, which
//! is how nearly every status bar on the NES is drawn. So the tiles are
//! recorded where the PPU actually resolves them.
//!
//! **The positioning is the part worth testing.** A reloaded tile goes
//! into the LOW byte of a 16-bit shifter whose HIGH bits feed the output,
//! so it becomes visible eight shifts later — pixel `dot + 7`, not
//! `dot - 1`. An off-by-one-tile error there looks entirely plausible on
//! a repeating background.

use super::test_ppu;

/// Fill the first nametable so every cell holds a distinct-ish tile
/// index, and enable rendering.
fn ppu_with_grid() -> super::Ppu {
    let mut p = test_ppu();
    for cell in 0..(32 * 30usize) {
        // Tile index = cell number, wrapped. Distinct within any row.
        p.vram[cell] = (cell % 251) as u8;
    }
    p.write_register(1, 0b0000_1000); // $2001 PPUMASK: show background
    p.set_tile_capture(true);
    p
}

/// Run until one complete frame has been PUBLISHED, and stop there.
///
/// **Where you stop matters, and it is the publish point.** A frame's
/// visible tiles finish at scanline 239, but they are published when the
/// pre-render line begins — twenty-two scanlines later. Reading before
/// that gives the previous frame; reading much later gives it too, since
/// the next publish is a whole frame away. This stops immediately after
/// the publish, so `drawn_tiles` is the frame just drawn.
///
/// Both loops are bounded by a tick budget and assert rather than spin:
/// an unbounded `while` here would be a hang, and a hanging test is a
/// denial of service, not a failing test (law 8).
fn run_captured_frame(p: &mut super::Ppu) {
    const BUDGET: u32 = 341 * 262 * 4;
    let mut ticks = 0u32;
    // Two publishes: the first may hand over a partial frame collected
    // from whatever power-on state the PPU started in.
    for _ in 0..2 {
        // Leave the pre-render line, so the next entry to it is a real
        // boundary rather than the one we are already standing on.
        while p.scanline == 261 {
            p.tick();
            ticks += 1;
            assert!(ticks < BUDGET, "never left the pre-render line");
        }
        // ...then run to the instant it begins again: the publish.
        while p.scanline != 261 {
            p.tick();
            ticks += 1;
            assert!(ticks < BUDGET, "never reached the next pre-render line");
        }
    }
}

#[test]
fn every_visible_cell_is_reported_once_and_in_the_right_place() {
    let mut p = ppu_with_grid();
    run_captured_frame(&mut p);
    let tiles = p.completed_tiles();

    assert!(
        !tiles.is_empty(),
        "capture is on and rendering is enabled, so tiles must be reported"
    );

    // With no scrolling the grid is exact: 32 columns at x = 0, 8, ... 248
    // on each of 240 scanlines.
    let on_screen: Vec<_> = tiles.iter().filter(|t| t.x >= 0 && t.x < 256).collect();
    assert_eq!(
        on_screen.len(),
        32 * 240,
        "an unscrolled frame draws exactly 32 tiles on each of 240 lines"
    );
    for t in &on_screen {
        assert_eq!(
            t.x % 8,
            0,
            "unscrolled tiles must land on 8-pixel boundaries, got x={}",
            t.x
        );
        assert!(t.y < 240, "y must be a visible scanline, got {}", t.y);
    }

    // **The mapping itself**: the tile at screen cell (col, row) must be
    // the nametable byte for that cell. This is what an off-by-one-tile
    // positioning error breaks, and nothing else here would catch it.
    for t in on_screen.iter().filter(|t| t.y == 0) {
        let col = (t.x / 8) as usize;
        let expected = (col % 251) as u8;
        assert_eq!(
            t.tile, expected,
            "row 0 column {col} should draw nametable byte {expected:#04X}, got {:#04X} \
             — a whole-tile positioning error",
            t.tile
        );
    }
    // And a row further down, so a correct first row cannot pass alone.
    for t in on_screen.iter().filter(|t| t.y == 100) {
        let col = (t.x / 8) as usize;
        let expected = ((100 / 8 * 32 + col) % 251) as u8;
        assert_eq!(t.tile, expected, "row 100 column {col} drew the wrong tile");
    }
}

/// Fine-X scrolling shifts the whole grid left, and the leftmost tile
/// hangs off the edge rather than being dropped.
#[test]
fn fine_x_shifts_the_reported_tiles_left() {
    let mut p = ppu_with_grid();
    // PPUSCROLL x = 3: fine X = 3.
    p.write_register(5, 3); // $2005 PPUSCROLL: X = 3 -> fine X = 3
    p.write_register(5, 0); // ...and Y = 0
    run_captured_frame(&mut p);
    let row0: Vec<_> = p.completed_tiles().iter().filter(|t| t.y == 0).collect();
    assert!(!row0.is_empty(), "row 0 must still be reported");
    let min_x = row0.iter().map(|t| t.x).min().unwrap();
    assert_eq!(
        min_x, -3,
        "with fine X = 3 the first tile starts 3 pixels off the left edge, \
         and must be REPORTED there rather than clipped away — a pack has to \
         draw its partly-visible left column too"
    );
}

/// Capture off costs nothing and reports nothing.
#[test]
fn capture_off_reports_nothing() {
    let mut p = ppu_with_grid();
    p.set_tile_capture(false);
    run_captured_frame(&mut p);
    assert!(
        p.completed_tiles().is_empty(),
        "with capture off the PPU must not accumulate tiles"
    );
}
