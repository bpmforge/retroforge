//! Merging one canvas into another: what a scout's cells do to the live ones
//! (ticket W32-07). Live always wins: only cells the live canvas has never
//! seen are filled.
use crate::stitcher::Canvas;
use rf_core_api::PpuPixel;

/// A world-space box: `(x0, y0, x1, y1)`, the end exclusive.
pub type Rect = (i64, i64, i64, i64);

/// Copy the cells of `from` inside the world box `rect` into `to` (what a
/// scout ships as a delta).
pub fn crop_into(to: &mut Canvas, from: &Canvas, rect: Rect) {
    let (w, h) = (from.width(), from.height());
    if w == 0 || h == 0 {
        return;
    }
    let (ox, oy) = from.origin();
    let (x0, x1) = (rect.0.max(ox), rect.2.min(ox + w as i64));
    let (y0, y1) = (rect.1.max(oy), rect.3.min(oy + h as i64));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    // Bounded `for` over the clipped rows (law 8).
    for wy in y0..y1 {
        let start = (wy - oy) as usize * w + (x0 - ox) as usize;
        to.blit_row(x0, wy, &from.cells()[start..start + (x1 - x0) as usize]);
    }
}

/// Ticket W32-07: copy into `live` every cell `scout` knows and `live` does
/// not (live always wins). Grows `live` to cover the scout's cells. Returns
/// the number of cells filled.
pub fn merge_unknown(live: &mut Canvas, scout: &Canvas) -> usize {
    merge_unknown_in(live, scout, None)
}

/// The smallest box holding both.
pub fn rect_union(a: Rect, b: Rect) -> Rect {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

/// [`merge_unknown`] restricted to the world box `within` (`None` = all).
pub fn merge_unknown_in(live: &mut Canvas, scout: &Canvas, within: Option<Rect>) -> usize {
    let (w, h) = (scout.width(), scout.height());
    if w == 0 || h == 0 {
        return 0;
    }
    let (ox, oy) = scout.origin();
    // The scout's own box, clipped to `within`, in canvas indices.
    let (x0, y0, x1, y1) = within.map_or((ox, oy, ox + w as i64, oy + h as i64), |r| {
        (
            r.0.max(ox),
            r.1.max(oy),
            r.2.min(ox + w as i64),
            r.3.min(oy + h as i64),
        )
    });
    if x0 >= x1 || y0 >= y1 {
        return 0;
    }
    let mut filled = 0;
    let mut row: Vec<Option<PpuPixel>> = Vec::with_capacity((x1 - x0) as usize);
    // Bounded `for` over the clipped rows (law 8).
    for wy in y0..y1 {
        let start = (wy - oy) as usize * w + (x0 - ox) as usize;
        let src = &scout.cells()[start..start + (x1 - x0) as usize];
        row.clear();
        row.extend(src.iter().enumerate().map(|(i, cell)| {
            let cell = (*cell)?;
            live.get(x0 + i as i64, wy).is_none().then_some(cell)
        }));
        let n = row.iter().flatten().count();
        if n > 0 {
            live.blit_row(x0, wy, &row);
            filled += n;
        }
    }
    filled
}
