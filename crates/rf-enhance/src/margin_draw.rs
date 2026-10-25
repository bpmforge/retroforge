//! Ticket W36-06b: objects the game keeps alive past the picture's edge, drawn in the widescreen pad.
//!
//! Pure and console-free (the shell converts a machine's drawn sprites and reads its memory). A NES draws
//! with 8-bit sprite X, so an object 400 px right of the camera is either hidden by the game or wrapped
//! onto the real picture; the game never draws it where it is. The shell therefore asks the game to draw
//! it anyway, on a private copy of the machine whose camera it moved by a whole page
//! (`retroforge::margin_draw`), and this module turns what that copy drew into pixels:
//!
//! * [`keep_near`] keeps only the tiles within tolerance of an object's expected spot (the copy draws
//!   everything else too: the player, blocks, the HUD);
//! * [`rasterise`] paints those tiles, from the game's own pattern bytes and palette, into an indexed
//!   [`STRIP_W`]-wide strip: the pad on one side;
//! * [`ghost_indices`] / [`ghost_dots`] find the entries of the REAL frame that the game wrapped onto the
//!   real picture for the same object, so the compositor can take them off it (never by changing sprite
//!   evaluation: sprite-0 hit and overflow are machine state, and the core is untouched).
//!
//! Hardware: sprite X is one byte and sprites never wrap their pixels across the right edge (nesdev wiki
//! "PPU OAM", "PPU sprite evaluation"); a tile's pattern is two bit planes 8 bytes apart (nesdev wiki
//! "PPU pattern tables"); sprite palettes start at `$3F10` (nesdev wiki "PPU palettes").
//!
//! **Honest limits.** Drawing ignores a sprite's behind-background priority bit. Only objects named by
//! the caller are kept: platforms or items drawn as sprites in the margin are not. An object straddling
//! the picture's own edge is drawn from the tile the game still shows, so its seam can be off by a tile.

/// Columns in one strip: how far into the pad an 8-bit sprite X can reach once the camera has been moved
/// by a page (the pad itself is clipped to this).
pub const STRIP_W: usize = 256;

/// A strip pixel nothing drew.
pub const TRANSPARENT: u8 = 0xFF;

/// One 8x8 sprite tile a PPU drew, console-free (`rf_nes::ppu::DrawnSprite`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// Screen x of the leftmost pixel; sprite X is one byte so this is 0..=255.
    pub x: i16,
    /// Screen y of the TOP row (OAM y + 1).
    pub y: i16,
    pub tile: u8,
    /// Pattern-table base (`$0000` or `$1000`) of the tile.
    pub base: u16,
    /// Sprite palette 0..=3 (`$3F10`-relative).
    pub palette: u8,
    pub flip_x: bool,
    pub flip_y: bool,
    /// Primary-OAM index the tile came from.
    pub oam_index: u8,
}

/// Where the game's own tables say an object is on the redrawn frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Object {
    /// Screen x of the object's left edge on the redrawn frame (its level x minus the moved camera).
    pub x: i32,
    /// The object's y as the game stores it (its sprite's OAM y is within a few pixels of it).
    pub y: i32,
}

/// A box on the redrawn frame whose tiles are never an object's (the player's: it is drawn at its
/// screen x whatever the camera says).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Avoid {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

/// How far a tile may sit from an object's expected spot and still be its own. An enemy is one or two
/// tiles wide and tall; the expected spot is read a frame or two off the drawn one.
const X_BEFORE: i32 = 12;
const X_AFTER: i32 = 20;
const Y_ABOVE: i32 = 24;
const Y_BELOW: i32 = 16;

/// The tiles of `tiles` within tolerance of any of `objects` and outside every `avoid` box.
#[must_use]
pub fn keep_near(tiles: &[Tile], objects: &[Object], avoid: &[Avoid]) -> Vec<Tile> {
    tiles
        .iter()
        .filter(|t| {
            let (x, y) = (i32::from(t.x), i32::from(t.y) - 1);
            let inside = |a: &Avoid| (a.x0..a.x1).contains(&x) && (a.y0..a.y1).contains(&y);
            !avoid.iter().any(inside)
                && objects.iter().any(|o| {
                    (o.x - X_BEFORE..o.x + X_AFTER).contains(&x)
                        && (o.y - Y_ABOVE..=o.y + Y_BELOW).contains(&y)
                })
        })
        .copied()
        .collect()
}

/// Paint `tile` into `strip` (`STRIP_W` x `height`, [`TRANSPARENT`] where nothing is). `chr(addr)` reads
/// the pattern table; `palette` is the 32-byte palette RAM. Pixels outside the strip are dropped.
fn blit(
    strip: &mut [u8],
    height: usize,
    tile: &Tile,
    chr: &dyn Fn(usize) -> u8,
    palette: &[u8; 32],
) {
    let at = usize::from(tile.base) + usize::from(tile.tile) * 16;
    // Bounded `for` ranges only (law 8): 8 rows x 8 columns.
    for row in 0..8usize {
        let src = if tile.flip_y { 7 - row } else { row };
        let (lo, hi) = (chr(at + src), chr(at + src + 8));
        let y = i32::from(tile.y) + row as i32;
        let Some(y) = usize::try_from(y).ok().filter(|y| *y < height) else {
            continue;
        };
        for col in 0..8usize {
            let bit = if tile.flip_x { col } else { 7 - col };
            let v = ((hi >> bit) & 1) << 1 | ((lo >> bit) & 1);
            let x = i32::from(tile.x) + col as i32;
            let Some(x) = usize::try_from(x).ok().filter(|x| *x < STRIP_W) else {
                continue;
            };
            if v != 0 {
                let p = 0x10 + usize::from(tile.palette & 3) * 4 + usize::from(v);
                strip[y * STRIP_W + x] = palette[p] & 0x3F;
            }
        }
    }
}

/// Rasterise `tiles` into a `STRIP_W` x `height` strip. The lowest OAM index wins an overlap, as on the
/// hardware, so tiles are painted highest index first.
#[must_use]
pub fn rasterise(
    tiles: &[Tile],
    height: usize,
    chr: &dyn Fn(usize) -> u8,
    palette: &[u8; 32],
) -> Vec<u8> {
    let mut strip = vec![TRANSPARENT; STRIP_W * height];
    let mut order: Vec<&Tile> = tiles.iter().collect();
    order.sort_by_key(|t| std::cmp::Reverse(t.oam_index));
    for t in order {
        blit(&mut strip, height, t, chr, palette);
    }
    strip
}

/// OAM entries the game drew on the REAL picture for an object it also drew in the redraw: the entry is
/// the same four bytes in both OAMs (the wrapped 8-bit X is the same number whichever page the camera is
/// on) and its index is one of `kept`'s. Entries the game hid (y >= 0xEF) are not drawn and not returned.
#[must_use]
pub fn ghost_indices(real_oam: &[u8; 256], redraw_oam: &[u8; 256], kept: &[Tile]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for t in kept {
        let i = usize::from(t.oam_index);
        let (a, b) = (&real_oam[i * 4..i * 4 + 4], &redraw_oam[i * 4..i * 4 + 4]);
        if a == b && a[0] < 0xEF && !out.contains(&t.oam_index) {
            out.push(t.oam_index);
        }
    }
    out
}

/// One dot of the real picture to give back to the background: `(x, y, background colour index)`.
pub type GhostDot = (u16, u16, u8);

/// The dots of the real picture (`ids`: the OAM index of the sprite that won each dot, `width` wide,
/// `0xFF` for none; `bg_under`: the background colour under every dot) won by one of `ghosts`.
#[must_use]
pub fn ghost_dots(ids: &[u8], width: usize, ghosts: &[u8], bg_under: &[u8]) -> Vec<GhostDot> {
    if ghosts.is_empty() || width == 0 {
        return Vec::new();
    }
    ids.iter()
        .zip(bg_under)
        .enumerate()
        .filter(|(_, (id, _))| ghosts.contains(id))
        .map(|(i, (_, bg))| ((i % width) as u16, (i / width) as u16, *bg))
        .collect()
}

/// What the core thread hands the compositor for one frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarginDraw {
    /// Rows in each strip.
    pub height: usize,
    /// The left pad: column `c` is picture x `c - STRIP_W`.
    pub left: Vec<u8>,
    /// The right pad: column `c` is picture x `STRIP_W + c`.
    pub right: Vec<u8>,
    /// Dots of the real picture that were a wrapped copy of a margin object.
    pub ghosts: Vec<GhostDot>,
}

impl MarginDraw {
    /// Whether anything is drawn or taken off the picture.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ghosts.is_empty()
            && !self.left.iter().any(|p| *p != TRANSPARENT)
            && !self.right.iter().any(|p| *p != TRANSPARENT)
    }

    /// Paint the strips over the pad of `out` (`out_w` x `h` RGBA, the 256-dot picture centred in it) and
    /// return how many dots were painted. `resolve` turns a palette index into RGB. Only strip columns
    /// that fall inside the pad are used; the 256 centre dots are never touched here.
    pub fn paint_pad(
        &self,
        out: &mut [u8],
        out_w: usize,
        h: usize,
        resolve: &dyn Fn(u8) -> [u8; 3],
    ) -> usize {
        let pad = out_w.saturating_sub(256) / 2;
        let rows = h.min(self.height);
        let mut painted = 0;
        // Bounded `for` ranges only (law 8).
        for (strip, picture_x0) in [
            (&self.left, -(STRIP_W as i64)),
            (&self.right, STRIP_W as i64),
        ] {
            if strip.len() < STRIP_W * self.height {
                continue;
            }
            for y in 0..rows {
                for c in 0..STRIP_W {
                    let idx = strip[y * STRIP_W + c];
                    if idx == TRANSPARENT {
                        continue;
                    }
                    // Picture x -> output x: the picture's dot 0 is at output column `pad`.
                    let ox = picture_x0 + c as i64 + pad as i64;
                    if ox < 0 || ox >= out_w as i64 {
                        continue;
                    }
                    let ox = ox as usize;
                    if (pad..pad + 256).contains(&ox) {
                        continue;
                    }
                    let rgb = resolve(idx);
                    if let Some(d) = out.get_mut((y * out_w + ox) * 4..(y * out_w + ox) * 4 + 4) {
                        d[..3].copy_from_slice(&rgb);
                        d[3] = 0xFF;
                        painted += 1;
                    }
                }
            }
        }
        painted
    }

    /// Replace the ghost dots of the real picture (the centre 256 dots of `out`) with the background
    /// colour under them; returns how many were replaced.
    pub fn remove_ghosts(
        &self,
        out: &mut [u8],
        out_w: usize,
        h: usize,
        resolve: &dyn Fn(u8) -> [u8; 3],
    ) -> usize {
        let pad = out_w.saturating_sub(256) / 2;
        let mut removed = 0;
        for &(x, y, bg) in &self.ghosts {
            let (x, y) = (usize::from(x), usize::from(y));
            if x >= 256 || y >= h {
                continue;
            }
            let at = (y * out_w + pad + x) * 4;
            if let Some(d) = out.get_mut(at..at + 4) {
                d[..3].copy_from_slice(&resolve(bg));
                d[3] = 0xFF;
                removed += 1;
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pattern table whose tile 1 is a solid colour-3 square and tile 2 a left-edge column of colour 1.
    fn chr(addr: usize) -> u8 {
        match addr {
            16..=31 => 0xFF,
            32..=39 => 0x80,
            _ => 0,
        }
    }

    fn palette() -> [u8; 32] {
        let mut p = [0u8; 32];
        for (i, v) in p.iter_mut().enumerate() {
            *v = 0x80 | i as u8;
        }
        p
    }

    fn tile(x: i16, y: i16, t: u8, oam: u8) -> Tile {
        Tile {
            x,
            y,
            tile: t,
            base: 0,
            palette: 1,
            flip_x: false,
            flip_y: false,
            oam_index: oam,
        }
    }

    #[test]
    fn only_tiles_near_an_object_and_outside_the_avoid_box_are_kept() {
        let tiles = [
            tile(100, 185, 1, 0),
            tile(108, 185, 1, 1),
            tile(40, 185, 1, 2),
            tile(100, 50, 1, 3),
            tile(100, 185, 1, 4),
        ];
        let obj = [Object { x: 100, y: 184 }];
        let avoid = [Avoid {
            x0: 90,
            y0: 200,
            x1: 110,
            y1: 230,
        }];
        let kept = keep_near(&tiles, &obj, &avoid);
        assert_eq!(kept.len(), 3, "x 40 and y 50 are far; the rest are near");
        let avoid = [Avoid {
            x0: 90,
            y0: 170,
            x1: 110,
            y1: 230,
        }];
        assert!(
            keep_near(&tiles, &obj, &avoid).is_empty(),
            "the player's box wins"
        );
    }

    #[test]
    fn rasterise_uses_the_games_pattern_and_sprite_palette_and_flips() {
        let p = palette();
        let s = rasterise(&[tile(10, 5, 1, 0)], 16, &chr, &p);
        // Tile 1 is colour 3 everywhere: palette[0x10 + 1*4 + 3].
        assert_eq!(s[5 * STRIP_W + 10], p[0x17] & 0x3F);
        assert_eq!(s[12 * STRIP_W + 17], p[0x17] & 0x3F);
        assert_eq!(s[4 * STRIP_W + 10], TRANSPARENT);
        assert_eq!(s[5 * STRIP_W + 18], TRANSPARENT);
        // Tile 2: only the leftmost column, colour 1 (lo plane bit 7); flipped it is the rightmost.
        let mut t = tile(0, 0, 2, 0);
        let s = rasterise(&[t], 8, &chr, &p);
        assert_eq!(s[0], p[0x15] & 0x3F);
        assert_eq!(s[7], TRANSPARENT);
        t.flip_x = true;
        let s = rasterise(&[t], 8, &chr, &p);
        assert_eq!(s[0], TRANSPARENT);
        assert_eq!(s[7], p[0x15] & 0x3F);
    }

    #[test]
    fn the_lowest_oam_index_wins_an_overlap_and_the_edges_clip() {
        let p = palette();
        let mut a = tile(0, 0, 2, 1);
        a.palette = 0;
        let mut b = tile(0, 0, 2, 0);
        b.palette = 2;
        let s = rasterise(&[a, b], 8, &chr, &p);
        assert_eq!(s[0], p[0x19] & 0x3F, "index 0 (palette 2) is on top");
        // Off the strip on every side: nothing panics, nothing is drawn.
        let far = [
            tile(-8, 0, 1, 0),
            tile(300, 0, 1, 0),
            tile(0, -8, 1, 0),
            tile(0, 20, 1, 0),
        ];
        assert!(rasterise(&far, 8, &chr, &p)
            .iter()
            .all(|p| *p == TRANSPARENT));
    }

    #[test]
    fn a_ghost_is_an_entry_both_oams_hold_unchanged_and_visible() {
        let mut real = [0xF8u8; 256];
        let mut redraw = [0xF8u8; 256];
        for oam in [&mut real, &mut redraw] {
            oam[8..12].copy_from_slice(&[100, 7, 0, 60]);
        }
        // Entry 3: hidden in the real OAM, drawn in the redraw -> not a ghost.
        redraw[12..16].copy_from_slice(&[100, 7, 0, 60]);
        // Entry 4: drawn in both but with a different x -> not a ghost.
        real[16..20].copy_from_slice(&[100, 7, 0, 61]);
        redraw[16..20].copy_from_slice(&[100, 7, 0, 60]);
        let kept = [
            tile(60, 101, 7, 2),
            tile(60, 101, 7, 3),
            tile(60, 101, 7, 4),
        ];
        assert_eq!(ghost_indices(&real, &redraw, &kept), vec![2]);
        // Two tiles of the same entry give one index.
        let kept = [tile(60, 101, 7, 2), tile(60, 109, 8, 2)];
        assert_eq!(ghost_indices(&real, &redraw, &kept), vec![2]);
    }

    #[test]
    fn ghost_dots_collect_the_dots_a_ghost_won_with_their_background() {
        let mut ids = vec![0xFFu8; 4 * 2];
        ids[1] = 2;
        ids[6] = 2;
        ids[5] = 9;
        let bg: Vec<u8> = (0..8).collect();
        let dots = ghost_dots(&ids, 4, &[2], &bg);
        assert_eq!(dots, vec![(1, 0, 1), (2, 1, 6)]);
        assert!(ghost_dots(&ids, 4, &[], &bg).is_empty());
    }

    #[test]
    fn paint_pad_fills_only_the_pad_and_remove_ghosts_only_the_centre() {
        let (out_w, h) = (256 + 2 * 40, 4);
        let mut out = vec![7u8; out_w * h * 4];
        let md = MarginDraw {
            height: h,
            left: {
                let mut v = vec![TRANSPARENT; STRIP_W * h];
                v[STRIP_W - 1] = 5; // picture x -1: the dot next to the picture
                v[STRIP_W - 41] = 5; // picture x -41: one past the 40-dot pad
                v[STRIP_W - 40] = 5; // picture x -40: the pad's outer dot
                v
            },
            right: {
                let mut v = vec![TRANSPARENT; STRIP_W * h];
                v[0] = 6; // picture x 256
                v[39] = 6; // picture x 295: the pad's outer dot
                v[40] = 6; // x 296: past the pad
                v
            },
            ghosts: vec![(3, 1, 9)],
        };
        let resolve = |i: u8| [i, i, i];
        let n = md.paint_pad(&mut out, out_w, h, &resolve);
        assert_eq!(n, 4, "two on each side, the two past the pad dropped");
        assert_eq!(out[((40 - 1) * 4)..][..4], [5, 5, 5, 255]);
        assert_eq!(out[0..4], [5, 5, 5, 255], "x -40 is output column 0");
        assert_eq!(out[((40 + 256) * 4)..][..4], [6, 6, 6, 255]);
        assert_eq!(out[((40 + 256 + 39) * 4)..][..4], [6, 6, 6, 255]);
        assert_eq!(md.remove_ghosts(&mut out, out_w, h, &resolve), 1);
        assert_eq!(out[((out_w + 40 + 3) * 4)..][..4], [9, 9, 9, 255]);
        // Nothing in the centre but the ghost dot changed.
        let centre_changed = (0..h)
            .flat_map(|y| (40..40 + 256).map(move |x| (y, x)))
            .filter(|(y, x)| out[(y * out_w + x) * 4] != 7)
            .count();
        assert_eq!(centre_changed, 1);
        assert!(!md.is_empty());
        assert!(MarginDraw::default().is_empty());
    }
}
