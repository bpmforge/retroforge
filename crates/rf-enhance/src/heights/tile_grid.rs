//! The per-frame tile grid (ticket W34-01).
//!
//! One [`TileGrid`] describes one horizontal band of one background layer as
//! the PPU drew it this frame: a rectangle of cells, each named by what is
//! IN the tile ([`TileKey`]: a hash of its pattern bytes and one of its
//! colours) rather than by a character number, because a game that reloads
//! VRAM between levels renumbers its tiles (HD2D_PLAN B7).
//!
//! ## Shape
//!
//! - A layer is one grid when its scroll is constant down the picture and
//!   several when the game changes it mid-frame (split-screen HUDs, HDMA
//!   parallax): each grid covers the picture rows `rows_px` and carries its
//!   own origin, so a cell is always positioned by the registers it was
//!   really drawn with (HD2D_PLAN B10).
//! - Cells are `cell_px` square (8, or 16 for a SNES 16x16 background and for
//!   the NES, where a "metatile" is the 2x2 tiles one attribute byte colours).
//! - A cell is 16 bytes ([`TileCell`]); the worst case the plan budgets is
//!   33 x 29 cells x 4 layers = 61 KB a frame.
//!
//! ## Flips
//!
//! For an 8 px cell the pattern hash is taken of the tile's canonical
//! orientation (the smallest hash of its four flips), and the cell's flip
//! bits say how to get from that canonical tile to what is on screen, so a
//! mirrored copy of a tile has the same key. For 16 px cells the hash is of
//! the unflipped arrangement of the four tiles and the flip bits are the
//! tilemap's own.

/// A tile's identity: stable across VRAM reloads and across levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    /// Hash of the pattern bytes (see the module doc for flips).
    pub pattern: u64,
    /// Hash of the colours the tile was drawn in, folded to 16 bits.
    pub palette: u16,
    /// Background layer, 0-based (the NES has only 0).
    pub layer: u8,
    /// Cell side in pixels, 8 or 16.
    pub cell_px: u8,
}

/// One cell of a grid. The layer and cell size are the grid's, not repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TileCell {
    pub pattern: u64,
    pub palette: u16,
    /// [`Self::PRIORITY`] | [`Self::FLIP_X`] | [`Self::FLIP_Y`] | [`Self::PARTIAL`].
    pub flags: u8,
}

impl TileCell {
    /// The tilemap's priority bit (SNES bit 13). The NES has none per tile.
    pub const PRIORITY: u8 = 1;
    /// Flip needed to turn the keyed (canonical) tile into the one shown.
    pub const FLIP_X: u8 = 2;
    pub const FLIP_Y: u8 = 4;
    /// Some of the cell's tiles were off screen, so the pattern hash covers
    /// only part of a cell and will not match the same cell seen whole.
    pub const PARTIAL: u8 = 8;

    #[must_use]
    pub fn priority(&self) -> bool {
        self.flags & Self::PRIORITY != 0
    }

    #[must_use]
    pub fn flip(&self) -> (bool, bool) {
        (
            self.flags & Self::FLIP_X != 0,
            self.flags & Self::FLIP_Y != 0,
        )
    }

    #[must_use]
    pub fn partial(&self) -> bool {
        self.flags & Self::PARTIAL != 0
    }
}

/// A cell with its key spelled out, as the learner reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileObs {
    pub key: TileKey,
    pub priority: bool,
    pub flip: (bool, bool),
    pub partial: bool,
}

/// One band of one layer. Row-major `cells`, `cols * rows` long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileGrid {
    /// 0-based background layer.
    pub layer: u8,
    /// Cell side in pixels.
    pub cell_px: u8,
    /// The layer is a status bar / fixed band rather than the playfield, as
    /// far as the canvas model knew when the frame was built (false when it
    /// was not tracking the scene; it is a hint, never a verdict).
    pub hud: bool,
    /// The grid reproduces what the PPU drew. False where the capture could
    /// not follow the picture (SNES offset-per-tile modes, or a layer whose
    /// scroll changed on more lines than the band cap).
    pub exact: bool,
    /// The picture rows `[start, end)` this grid covers.
    pub rows_px: (u16, u16),
    /// World pixel position of cell (0, 0)'s top-left corner: the tilemap
    /// pixel on the SNES (the 10-bit scroll registers, unwrapped), the
    /// nametable-space pixel on the NES (a 512 x 480 space).
    pub origin_world: (i64, i64),
    /// How far cell (0, 0) starts left of / above the picture position
    /// `(0, rows_px.0)`; a cell's picture x is `col * cell_px - offset.0`.
    pub offset: (u8, u8),
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<TileCell>,
}

impl TileGrid {
    /// The cell at `(col, row)`.
    #[must_use]
    pub fn cell(&self, col: u16, row: u16) -> Option<&TileCell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    /// The identity of the cell at `(col, row)`.
    #[must_use]
    pub fn key_at(&self, col: u16, row: u16) -> Option<TileKey> {
        self.cell(col, row).map(|c| TileKey {
            pattern: c.pattern,
            palette: c.palette,
            layer: self.layer,
            cell_px: self.cell_px,
        })
    }

    /// The cell at `(col, row)` with its key.
    #[must_use]
    pub fn obs_at(&self, col: u16, row: u16) -> Option<TileObs> {
        let c = self.cell(col, row)?;
        Some(TileObs {
            key: self.key_at(col, row)?,
            priority: c.priority(),
            flip: c.flip(),
            partial: c.partial(),
        })
    }

    /// Where cell `(col, row)`'s top-left corner is in the picture (negative
    /// for the first column under sub-cell scroll).
    #[must_use]
    pub fn cell_picture_origin(&self, col: u16, row: u16) -> (i32, i32) {
        let p = i32::from(self.cell_px);
        (
            i32::from(col) * p - i32::from(self.offset.0),
            i32::from(self.rows_px.0) + i32::from(row) * p - i32::from(self.offset.1),
        )
    }

    /// The cell under picture pixel `(x, y)`, if this band covers it.
    #[must_use]
    pub fn cell_at_picture(&self, x: i32, y: i32) -> Option<(u16, u16)> {
        if y < i32::from(self.rows_px.0) || y >= i32::from(self.rows_px.1) || x < 0 {
            return None;
        }
        let p = i32::from(self.cell_px.max(1));
        let col = (x + i32::from(self.offset.0)) / p;
        let row = (y - i32::from(self.rows_px.0) + i32::from(self.offset.1)) / p;
        (col < i32::from(self.cols) && row < i32::from(self.rows)).then(|| {
            (
                u16::try_from(col).unwrap_or(0),
                u16::try_from(row).unwrap_or(0),
            )
        })
    }

    /// Bytes this grid adds to a frame message: the cells plus the header.
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        self.cells.len() * std::mem::size_of::<TileCell>() + std::mem::size_of::<Self>()
    }
}

/// 64-bit FNV-1a: stable across Rust versions and platforms, which
/// `std::hash::DefaultHasher` does not promise and a cache key needs.
#[derive(Debug, Clone, Copy)]
pub struct Fnv64(u64);

impl Default for Fnv64 {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    #[must_use]
    pub fn new() -> Self {
        Self(Self::OFFSET)
    }

    pub fn bytes(&mut self, data: &[u8]) -> &mut Self {
        for &b in data {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    #[must_use]
    pub fn finish(&self) -> u64 {
        self.0
    }

    /// The hash folded to 16 bits (xor of the four 16-bit lanes).
    #[must_use]
    pub fn finish16(&self) -> u16 {
        let v = self.0;
        (v ^ (v >> 16) ^ (v >> 32) ^ (v >> 48)) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> TileGrid {
        TileGrid {
            layer: 1,
            cell_px: 8,
            hud: false,
            exact: true,
            rows_px: (0, 24),
            origin_world: (-3, 0),
            offset: (3, 2),
            cols: 4,
            rows: 4,
            cells: (0..16)
                .map(|i| TileCell {
                    pattern: i,
                    palette: 7,
                    flags: TileCell::PRIORITY,
                })
                .collect(),
        }
    }

    #[test]
    fn a_cell_is_sixteen_bytes() {
        assert_eq!(std::mem::size_of::<TileCell>(), 16);
    }

    #[test]
    fn the_planned_worst_case_fits_the_budget() {
        // 33 x 29 cells x 4 layers (HD2D_PLAN 2.3: 61 KB a frame).
        assert!(33 * 29 * 4 * std::mem::size_of::<TileCell>() <= 61_500);
    }

    #[test]
    fn keys_carry_the_grids_layer_and_cell_size() {
        let g = grid();
        let k = g.key_at(2, 1).unwrap();
        assert_eq!((k.layer, k.cell_px, k.pattern, k.palette), (1, 8, 6, 7));
        assert!(g.obs_at(2, 1).unwrap().priority);
        assert!(g.key_at(4, 0).is_none() && g.key_at(0, 4).is_none());
    }

    #[test]
    fn picture_positions_map_to_cells_and_back() {
        let g = grid();
        assert_eq!(g.cell_picture_origin(0, 0), (-3, -2));
        assert_eq!(g.cell_picture_origin(1, 1), (5, 6));
        assert_eq!(g.cell_at_picture(0, 0), Some((0, 0)));
        assert_eq!(g.cell_at_picture(5, 6), Some((1, 1)));
        assert_eq!(g.cell_at_picture(4, 6), Some((0, 1)));
        assert_eq!(g.cell_at_picture(0, 24), None, "below the band");
    }

    #[test]
    fn fnv_is_the_published_function() {
        // FNV-1a 64 test vectors (draft-eastlake-fnv).
        assert_eq!(Fnv64::new().finish(), 0xcbf2_9ce4_8422_2325);
        assert_eq!(Fnv64::new().bytes(b"a").finish(), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(
            Fnv64::new().bytes(b"foobar").finish(),
            0x8594_4171_f739_67e8
        );
    }
}
