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

/// Ticket W33-04: how many cells the scout and the live canvas BOTH know, and
/// how many of those hold the same pixel (palette index and palette id),
/// inside `within` (`None` = everywhere). The W32-07 note measured 39 110 of
/// 39 110; this is that measurement as a function (STITCHING_PLAN §3.3).
/// Returns `(agreed, shared)`. Reads only.
#[must_use]
pub fn scout_agreement(live: &Canvas, scout: &Canvas, within: Option<Rect>) -> (usize, usize) {
    let (w, h) = (scout.width(), scout.height());
    if w == 0 || h == 0 {
        return (0, 0);
    }
    let (ox, oy) = scout.origin();
    let (x0, y0, x1, y1) = within.map_or((ox, oy, ox + w as i64, oy + h as i64), |r| {
        (
            r.0.max(ox),
            r.1.max(oy),
            r.2.min(ox + w as i64),
            r.3.min(oy + h as i64),
        )
    });
    let (mut agreed, mut shared) = (0, 0);
    // Bounded `for` over the clipped box (law 8).
    for wy in y0..y1 {
        for wx in x0..x1 {
            let (Some(a), Some(b)) = (live.get(wx, wy), scout.get(wx, wy)) else {
                continue;
            };
            shared += 1;
            agreed += usize::from(a.palette_index == b.palette_index && a.sprite_id == b.sprite_id);
        }
    }
    (agreed, shared)
}

/// What to do with a scout delta, by [`scout_agreement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoutVerdict {
    /// 99 % or more agree: merge.
    Merge,
    /// 95 to 99 %: merge, and take smaller scout steps next pass.
    SlowDown,
    /// Under 95 %: discard the delta.
    Reject,
}

/// Fewer shared cells than this say nothing (STITCHING_PLAN §3.3).
pub const MIN_SHARED_CELLS: usize = 2000;

/// The §3.3 thresholds over `(agreed, shared)`; `None` = too few shared
/// cells to judge. Wiring it into the scout worker (reject/slow the pass)
/// belongs to W33-06.
#[must_use]
pub fn scout_verdict((agreed, shared): (usize, usize)) -> Option<ScoutVerdict> {
    if shared < MIN_SHARED_CELLS {
        return None;
    }
    Some(if agreed * 100 >= shared * 99 {
        ScoutVerdict::Merge
    } else if agreed * 100 >= shared * 95 {
        ScoutVerdict::SlowDown
    } else {
        ScoutVerdict::Reject
    })
}
