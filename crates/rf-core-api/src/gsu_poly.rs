//! Ticket W31-02: the Super FX polygon stream, as plain data.
//!
//! A *render job* is everything the GSU draws between two `STOP`s. A core
//! that is told which RAM addresses and code addresses a game's rasteriser
//! uses (a profile's `[superfx_polygons]`, found by observation: see
//! `docs/research/W31-01-starfox-polygon-map.md`) records the job as an
//! ordered list of [`PolyOp`]s. Nothing here knows about any cartridge or any
//! core; `rf-renderer` re-draws a job, `rf-snes` records one.

/// One thing a render job drew, in the GSU's own (painter) order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolyOp {
    /// A filled polygon: its corners after the game's clipping (8-bit screen
    /// coordinates, in the order the game's edge walker reads them), and the
    /// `COLR`/`POR` in force when its first pixel was plotted.
    Polygon {
        corners: Vec<(u8, u8)>,
        colr: u8,
        por: u8,
    },
    /// A single `PLOT` outside any polygon fill (a star, a line pixel...).
    Plot { x: u8, y: u8, colr: u8, por: u8 },
}

/// One completed render job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolyJob {
    /// The drawing, in order.
    pub ops: Vec<PolyOp>,
    /// Bits per pixel of the GSU bitmap (SCMR) when the job ended: 2, 4 or 8.
    /// Dither and the transparency test depend on it ([`plot_colour`]).
    pub bpp: u8,
    /// The 3D viewport, `(width, height)`.
    pub viewport: (u16, u16),
    /// Row-major, `viewport.0 * viewport.1`: `true` where a `PLOT` of this job
    /// actually drew a pixel (fill plots included). Empty when nothing drew.
    pub plotted: Vec<bool>,
    /// Only when the observer was asked to capture it (a proof/test hook):
    /// the GSU's own bitmap at `STOP`, row-major, `viewport.0 * viewport.1`
    /// palette indices.
    pub bitmap: Option<Vec<u8>>,
}

/// What one `PLOT` puts in the bitmap, or `None` when it draws nothing —
/// the single definition of `PLOT`'s colour rule, shared by the GSU core and
/// the re-rasteriser so they cannot drift.
///
/// fullsnes "GSU Bitmap Opcodes" / POR: bit 0 set also plots colour 0; bit 1
/// (dither, 4/16-colour modes only) uses `COLR / 16` when `x ^ y` is odd;
/// bit 3 (freeze-high) narrows the 256-colour transparency test to the low
/// 4 bits.
#[must_use]
pub fn plot_colour(colr: u8, por: u8, bpp: u8, x: u8, y: u8) -> Option<u8> {
    let plot_transparent = por & 0x01 != 0;
    let dither = por & 0x02 != 0;
    let freeze_high = por & 0x08 != 0;
    let mut colour = colr;
    if dither && bpp != 8 && (x ^ y) & 1 != 0 {
        colour >>= 4;
    }
    let check_bits = if freeze_high { bpp.min(4) } else { bpp };
    let check_mask: u8 = if check_bits >= 8 {
        0xFF
    } else {
        (1u8 << check_bits) - 1
    };
    if !plot_transparent && colour & check_mask == 0 {
        None
    } else {
        Some(colour)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_zero_is_transparent_unless_por_bit0() {
        assert_eq!(plot_colour(0, 0, 4, 0, 0), None);
        assert_eq!(plot_colour(0, 1, 4, 0, 0), Some(0));
        assert_eq!(plot_colour(5, 0, 4, 0, 0), Some(5));
    }

    #[test]
    fn dither_uses_the_high_nibble_on_odd_parity() {
        assert_eq!(plot_colour(0xA4, 2, 4, 0, 0), Some(0xA4));
        assert_eq!(plot_colour(0xA4, 2, 4, 1, 0), Some(0x0A));
        assert_eq!(
            plot_colour(0xA4, 2, 8, 1, 0),
            Some(0xA4),
            "no dither at 8bpp"
        );
    }

    #[test]
    fn freeze_high_narrows_the_256_colour_check() {
        assert_eq!(plot_colour(0x50, 0, 8, 0, 0), Some(0x50));
        assert_eq!(plot_colour(0x50, 0x08, 8, 0, 0), None);
    }
}
