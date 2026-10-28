//! Ticket W38-05b: widescreen margins for a screen-flipping overworld, from the
//! neighbouring screens' own layouts.
//!
//! A game that flips whole screens (no scrolling world) gives a constant-widening
//! scan nothing to find: the picture beside the current screen is simply another
//! screen. The recipe knows one byte that moves by 1 sideways and by 16 up or down
//! when the screen changes (`screen_index`, found by observation, D-021), i.e. the
//! screens sit on a grid, `index = row * 16 + column`. This module is pure and
//! console-free: it holds a [`ScreenAtlas`] of per-screen background images
//! (indexed colours, learned by the shell/harness by running the game's own loader
//! on a scratch machine, never the live one) and lays the neighbours beside the
//! current screen for a wider output. Nothing here touches game state, so the
//! original picture stays byte-identical (law 6).
//!
//! **Honest limits.** Margins are laid only while the screen is at rest (the caller
//! passes `None` while it scrolls); a screen the atlas lacks leaves its margin
//! [`UNKNOWN`] (the compositor falls back to the VRAM margin). Interiors (caves,
//! dungeons) reuse the overworld index: [`ScreenAtlas::agreement`] is the gate that
//! refuses to lay overworld neighbours around a picture that is not that screen.

use std::collections::BTreeMap;

/// Width of one screen in pixels.
pub const SCREEN_W: usize = 256;
/// Screens per grid row (index step for sideways moves is 1).
pub const GRID_COLS: u8 = 16;
/// Grid rows (index step for vertical moves is [`GRID_COLS`]).
pub const GRID_ROWS: u8 = 8;
/// A pixel nothing is known about (a sprite covered it, or no neighbour exists).
pub const UNKNOWN: u8 = 0xFF;

/// A screen-change direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The screen index one step from `index` in `dir`, if the grid has one.
#[must_use]
pub fn neighbour(index: u8, dir: Dir) -> Option<u8> {
    let (col, row) = (index % GRID_COLS, index / GRID_COLS);
    if row >= GRID_ROWS {
        return None;
    }
    match dir {
        Dir::Left => col.checked_sub(1).map(|_| index - 1),
        Dir::Right => (col + 1 < GRID_COLS).then(|| index + 1),
        Dir::Up => row.checked_sub(1).map(|_| index - GRID_COLS),
        Dir::Down => (row + 1 < GRID_ROWS).then(|| index + GRID_COLS),
    }
}

/// The screen `dx` columns from `index` on the same grid row.
#[must_use]
pub fn column_offset(index: u8, dx: i32) -> Option<u8> {
    let (col, row) = (i32::from(index % GRID_COLS), index / GRID_COLS);
    let c = col + dx;
    ((0..i32::from(GRID_COLS)).contains(&c) && row < GRID_ROWS)
        .then(|| row * GRID_COLS + u8::try_from(c).unwrap_or(0))
}

/// One screen's picture: [`SCREEN_W`] columns of indexed colours, [`UNKNOWN`] where a
/// sprite hid the background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenImage {
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl ScreenImage {
    /// A blank image of `height` rows.
    #[must_use]
    pub fn unknown(height: usize) -> Self {
        Self {
            height,
            pixels: vec![UNKNOWN; SCREEN_W * height],
        }
    }

    /// Wrap `pixels` (must be `SCREEN_W * height` long, else `None`).
    #[must_use]
    pub fn new(height: usize, pixels: Vec<u8>) -> Option<Self> {
        (pixels.len() == SCREEN_W * height).then_some(Self { height, pixels })
    }

    /// Fill this image's unknown pixels from `other` (same height); returns how many.
    pub fn fill_unknown_from(&mut self, other: &ScreenImage) -> usize {
        if other.height != self.height {
            return 0;
        }
        let mut n = 0;
        for (d, &s) in self.pixels.iter_mut().zip(&other.pixels) {
            if *d == UNKNOWN && s != UNKNOWN {
                *d = s;
                n += 1;
            }
        }
        n
    }

    /// Pixels still unknown.
    #[must_use]
    pub fn unknown_count(&self) -> usize {
        self.pixels.iter().filter(|&&p| p == UNKNOWN).count()
    }
}

/// The learned screens, by index.
#[derive(Debug, Clone, Default)]
pub struct ScreenAtlas {
    screens: BTreeMap<u8, ScreenImage>,
}

impl ScreenAtlas {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, index: u8, image: ScreenImage) {
        self.screens.insert(index, image);
    }

    #[must_use]
    pub fn get(&self, index: u8) -> Option<&ScreenImage> {
        self.screens.get(&index)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.screens.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.screens.is_empty()
    }

    /// Fraction (0..=1) of the pixels known in both images that agree, comparing the
    /// live picture `live` (`SCREEN_W` wide, [`UNKNOWN`] where a sprite is) with the
    /// atlas screen `index`, over rows `rows`. `None` if the atlas lacks the screen
    /// or fewer than 1/4 of the compared pixels are known on both sides.
    #[must_use]
    pub fn agreement(&self, index: u8, live: &[u8], rows: std::ops::Range<usize>) -> Option<f32> {
        let s = self.get(index)?;
        let rows = rows.start.min(s.height)..rows.end.min(s.height);
        let (mut known, mut same, mut total) = (0usize, 0usize, 0usize);
        for y in rows {
            for x in 0..SCREEN_W {
                total += 1;
                let (a, b) = (
                    s.pixels[y * SCREEN_W + x],
                    live.get(y * SCREEN_W + x).copied().unwrap_or(UNKNOWN),
                );
                if a != UNKNOWN && b != UNKNOWN {
                    known += 1;
                    same += usize::from(a == b);
                }
            }
        }
        (known * 4 >= total && total > 0).then(|| same as f32 / known as f32)
    }

    /// The indexed margin picture for an output `out_w` wide around screen `index`:
    /// `out_w * (rows.end - rows.start)` pixels, the centre 256 columns [`UNKNOWN`]
    /// (the caller keeps its own picture there), the sides laid from the neighbours
    /// `+-1`, `+-2`... on the same grid row. A neighbour the grid or the atlas lacks
    /// leaves [`UNKNOWN`].
    #[must_use]
    pub fn margins(&self, index: u8, out_w: usize, rows: std::ops::Range<usize>) -> Vec<u8> {
        let h = rows.end.saturating_sub(rows.start);
        let mut out = vec![UNKNOWN; out_w * h];
        let pad = out_w.saturating_sub(SCREEN_W) / 2;
        for x in (0..out_w).filter(|&x| x < pad || x >= pad + SCREEN_W) {
            // World column relative to the centre screen's left edge (may be negative).
            let wx = x as i64 - pad as i64;
            let dx = i32::try_from(wx.div_euclid(SCREEN_W as i64)).unwrap_or(0);
            let sx = wx.rem_euclid(SCREEN_W as i64) as usize;
            let Some(img) = column_offset(index, dx).and_then(|i| self.get(i)) else {
                continue;
            };
            for (oy, y) in rows.clone().enumerate() {
                if y < img.height {
                    out[oy * out_w + x] = img.pixels[y * SCREEN_W + sx];
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(h: usize, v: u8) -> ScreenImage {
        ScreenImage::new(h, vec![v; SCREEN_W * h]).expect("sized")
    }

    #[test]
    fn neighbours_follow_one_and_sixteen() {
        assert_eq!(neighbour(0x77, Dir::Right), Some(0x78));
        assert_eq!(neighbour(0x78, Dir::Up), Some(0x68));
        assert_eq!(neighbour(0x68, Dir::Down), Some(0x78));
        assert_eq!(neighbour(0x70, Dir::Left), None, "column 0 has no left");
        assert_eq!(neighbour(0x7F, Dir::Right), None, "column 15 has no right");
        assert_eq!(neighbour(0x05, Dir::Up), None);
        assert_eq!(neighbour(0x75, Dir::Down), None);
        assert_eq!(neighbour(0x80, Dir::Right), None, "off the grid");
    }

    #[test]
    fn column_offset_stays_on_the_row() {
        assert_eq!(column_offset(0x12, 2), Some(0x14));
        assert_eq!(column_offset(0x12, -2), Some(0x10));
        assert_eq!(column_offset(0x12, -3), None);
        assert_eq!(column_offset(0x1E, 2), None);
    }

    #[test]
    fn margins_lay_neighbours_either_side() {
        let mut a = ScreenAtlas::new();
        for (i, v) in [(0x10u8, 1u8), (0x11, 2), (0x12, 3)] {
            a.insert(i, flat(4, v));
        }
        // 256 + 2 * 24 wide around screen 0x11.
        let m = a.margins(0x11, 304, 0..4);
        assert_eq!(m.len(), 304 * 4);
        assert_eq!(m[0], 1, "left pad shows screen 0x10");
        assert_eq!(m[23], 1);
        assert_eq!(m[24], UNKNOWN, "centre is the caller's");
        assert_eq!(m[24 + 255], UNKNOWN);
        assert_eq!(m[24 + 256], 3, "right pad shows screen 0x12");
        assert_eq!(m[303], 3);
    }

    #[test]
    fn missing_neighbours_stay_unknown_and_wide_pads_reach_two_screens() {
        let mut a = ScreenAtlas::new();
        a.insert(0x10, flat(2, 7));
        a.insert(0x11, flat(2, 8));
        // Screen 0x11: left neighbour 0x10 exists, the one beyond does not; right missing.
        let m = a.margins(0x11, 256 + 600, 0..2);
        let pad = 300;
        assert_eq!(m[pad - 1], 7);
        assert_eq!(m[pad - 256], 7);
        assert_eq!(m[pad - 257], UNKNOWN, "two screens left is off the grid");
        assert_eq!(m[pad + 256], UNKNOWN, "no right neighbour learned");
    }

    #[test]
    fn agreement_gates_on_known_pixels() {
        let mut a = ScreenAtlas::new();
        a.insert(5, flat(4, 9));
        assert_eq!(a.agreement(5, &vec![9; SCREEN_W * 4], 0..4), Some(1.0));
        assert_eq!(a.agreement(5, &vec![3; SCREEN_W * 4], 0..4), Some(0.0));
        assert_eq!(
            a.agreement(5, &vec![UNKNOWN; SCREEN_W * 4], 0..4),
            None,
            "nothing known: no verdict"
        );
        assert_eq!(a.agreement(6, &vec![9; SCREEN_W * 4], 0..4), None);
    }

    #[test]
    fn fill_unknown_merges_a_second_capture() {
        let mut a = ScreenImage::new(1, {
            let mut p = vec![4; SCREEN_W];
            p[10] = UNKNOWN;
            p
        })
        .expect("sized");
        let b = flat(1, 6);
        assert_eq!(a.fill_unknown_from(&b), 1);
        assert_eq!(a.pixels[10], 6);
        assert_eq!(a.unknown_count(), 0);
    }
}
