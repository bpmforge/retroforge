//! Ticket W31-02: the 1x reference re-rasteriser for a Super FX render job.
//!
//! Takes the plain polygon stream a core observed (`rf_core_api::gsu_poly`)
//! and draws it the way the GSU itself did, at the GSU's own resolution:
//! the baseline a higher-resolution re-rasteriser (W31-03) must agree with.
//! Pure and CPU-only; no core, no GPU.
//!
//! The polygon rule was found by observation (`docs/research/
//! W31-01-starfox-polygon-map.md`): scanline fill over integer rows from the
//! top corner row up to, but **excluding**, the bottom corner row; on each
//! row the left and right edges are where the polygon's edges cross that
//! row, each `floor`ed. Whether the right-hand pixel itself is drawn is
//! [`RightEdge`]; measured against the real GSU's pixels (the proof test in
//! `rf-harness`) the right pixel **is** drawn (`Inclusive`: 0.996 of the GSU's
//! pixels reproduced exactly on Star Fox, `Exclusive`: 0.95-0.97). A polygon that is not convex is filled from its
//! leftmost to its rightmost crossing on each row.
//!
//! Colour is exactly the GSU's `PLOT`: [`rf_core_api::gsu_poly::plot_colour`]
//! (colour-0 transparency, dither by `(x ^ y) & 1`, freeze-high), keeping the
//! low `bpp` bits as the bitmap stores them, painter order.

use rf_core_api::gsu_poly::{plot_colour, PolyJob, PolyOp};

/// Output value for "nothing was drawn here". (An 8bpp colour 255 would
/// collide; the Star Fox 3D view is not 8bpp.)
pub const NONE: u8 = 0xFF;

/// Whether a scanline's rightmost pixel (`floor` of the right edge) is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightEdge {
    /// Draw `left..=right`.
    Inclusive,
    /// Draw `left..right`.
    Exclusive,
}

/// Rasterise `job` into a `viewport.0 * viewport.1` image of palette
/// indices ([`NONE`] where nothing was drawn).
#[must_use]
pub fn rasterise_job_1x(job: &PolyJob, viewport: (u16, u16)) -> Vec<u8> {
    rasterise_job_with(job, viewport, RightEdge::Inclusive)
}

/// [`rasterise_job_1x`] with the right-edge rule chosen explicitly.
#[must_use]
pub fn rasterise_job_with(job: &PolyJob, viewport: (u16, u16), rule: RightEdge) -> Vec<u8> {
    let (w, h) = (usize::from(viewport.0), usize::from(viewport.1));
    let mut img = vec![NONE; w * h];
    let keep: u8 = if job.bpp >= 8 {
        0xFF
    } else {
        (1u8 << job.bpp) - 1
    };
    let mut put = |x: i32, y: i32, colr: u8, por: u8| {
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return;
        }
        if let Some(c) = plot_colour(colr, por, job.bpp, x as u8, y as u8) {
            img[y as usize * w + x as usize] = c & keep;
        }
    };
    for op in &job.ops {
        match op {
            PolyOp::Plot { x, y, colr, por } => put(i32::from(*x), i32::from(*y), *colr, *por),
            PolyOp::Polygon { corners, colr, por } => {
                for (y, left, right) in spans(corners) {
                    let last = match rule {
                        RightEdge::Inclusive => right,
                        RightEdge::Exclusive => right - 1,
                    };
                    // `left..=last` is finite and bounded by the u8 corner
                    // range (law 8: a range, not a hand-rolled index).
                    for x in left..=last {
                        put(x, y, *colr, *por);
                    }
                }
            }
        }
    }
    img
}

/// `floor(a / b)` for `b > 0`.
fn floor_div(a: i32, b: i32) -> i32 {
    a.div_euclid(b)
}

/// The filled rows of a polygon: `(y, left, right)` with `left`/`right` the
/// floored crossings. An edge covers the rows `min(ya, yb) <= y < max(ya,
/// yb)` (top included, bottom excluded), so the polygon's lowest row is never
/// produced and a horizontal edge produces nothing itself.
fn spans(corners: &[(u8, u8)]) -> Vec<(i32, i32, i32)> {
    if corners.len() < 2 {
        return Vec::new();
    }
    let ys = corners.iter().map(|c| i32::from(c.1));
    let (top, bottom) = (ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0));
    let mut out = Vec::new();
    for y in top..bottom {
        let mut lo = i32::MAX;
        let mut hi = i32::MIN;
        for (i, a) in corners.iter().enumerate() {
            let b = corners[(i + 1) % corners.len()];
            // Orient the edge downwards so the divisor is positive.
            let (a, b) = if a.1 <= b.1 { (*a, b) } else { (b, *a) };
            let (xa, ya, xb, yb) = (
                i32::from(a.0),
                i32::from(a.1),
                i32::from(b.0),
                i32::from(b.1),
            );
            if y < ya || y >= yb {
                continue;
            }
            let x = floor_div(xa * (yb - ya) + (xb - xa) * (y - ya), yb - ya);
            lo = lo.min(x);
            hi = hi.max(x);
        }
        if lo <= hi {
            out.push((y, lo, hi));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(ops: Vec<PolyOp>, bpp: u8) -> PolyJob {
        PolyJob {
            ops,
            bpp,
            viewport: (16, 16),
            plotted: Vec::new(),
            bitmap: None,
        }
    }

    fn poly(corners: &[(u8, u8)], colr: u8, por: u8) -> PolyOp {
        PolyOp::Polygon {
            corners: corners.to_vec(),
            colr,
            por,
        }
    }

    fn rows(img: &[u8], y: usize) -> String {
        img[y * 16..y * 16 + 16]
            .iter()
            .map(|&c| if c == NONE { '.' } else { char::from(b'0' + c) })
            .collect()
    }

    #[test]
    fn rectangle_excludes_the_bottom_row() {
        let j = job(vec![poly(&[(2, 1), (5, 1), (5, 4), (2, 4)], 3, 0)], 4);
        let img = rasterise_job_1x(&j, (16, 16));
        assert_eq!(rows(&img, 0), "................");
        assert_eq!(rows(&img, 1), "..3333..........");
        assert_eq!(rows(&img, 3), "..3333..........");
        assert_eq!(rows(&img, 4), "................", "bottom row excluded");
        let ex = rasterise_job_with(&j, (16, 16), RightEdge::Exclusive);
        assert_eq!(rows(&ex, 2), "..333...........");
    }

    #[test]
    fn triangle_edges_are_floored() {
        // Apex (4,0); base (0,4)-(8,4). Row y: left = 4-y, right = 4+y.
        let j = job(vec![poly(&[(4, 0), (8, 4), (0, 4)], 1, 0)], 4);
        let img = rasterise_job_1x(&j, (16, 16));
        assert_eq!(rows(&img, 0), "....1...........");
        assert_eq!(rows(&img, 1), "...111..........");
        assert_eq!(rows(&img, 3), ".1111111........");
        // Shallow slope: edge from (0,0) to (3,2): at y=1 x = 1.5 -> 1.
        let j = job(vec![poly(&[(0, 0), (3, 2), (0, 2)], 1, 0)], 4);
        let img = rasterise_job_1x(&j, (16, 16));
        assert_eq!(rows(&img, 1), "11..............");
    }

    #[test]
    fn painter_order_dither_and_transparency() {
        let j = job(
            vec![
                poly(&[(0, 0), (4, 0), (4, 2), (0, 2)], 5, 0),
                // colour 0 is transparent: leaves the red underneath alone.
                poly(&[(1, 0), (3, 0), (3, 1), (1, 1)], 0, 0),
                // dither: 0x12 -> low nibble 2 on even parity, high nibble 1 on odd.
                poly(&[(8, 0), (12, 0), (12, 1), (8, 1)], 0x12, 2),
                PolyOp::Plot {
                    x: 15,
                    y: 15,
                    colr: 7,
                    por: 0,
                },
            ],
            4,
        );
        let img = rasterise_job_1x(&j, (16, 16));
        assert_eq!(rows(&img, 0), "55555...21212...");
        assert_eq!(img[15 * 16 + 15], 7);
    }

    #[test]
    fn clipped_degenerate_and_overflowing_shapes_do_not_panic() {
        let j = job(
            vec![
                poly(&[(0, 0)], 1, 0),
                poly(&[(3, 3), (3, 3), (3, 3)], 1, 0),
                poly(&[(250, 250), (255, 250), (255, 255)], 1, 0),
                PolyOp::Plot {
                    x: 200,
                    y: 3,
                    colr: 1,
                    por: 0,
                },
                poly(&[(0, 14), (15, 14), (15, 20)], 2, 0),
            ],
            4,
        );
        let img = rasterise_job_1x(&j, (16, 16));
        assert_eq!(img.len(), 256);
        assert_eq!(rows(&img, 14), "2222222222222222");
    }
}
