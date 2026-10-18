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
            // Tagged Scout (W33-06): a live frame landing here later
            // overwrites it and makes it Live again.
            live.blit_row_scout(x0, wy, &row);
            filled += n;
        }
    }
    filled
}

/// Ticket W33-04 / W33-06: how many cells the scout and the live canvas BOTH
/// know, and how many of those hold the same pixel, inside `within` (`None` =
/// everywhere). The W32-07 note measured 39 110 of 39 110; this is that
/// measurement as a function (STITCHING_PLAN §3.3). Returns `(agreed,
/// shared)`. Reads only.
///
/// Only cells a LIVE frame wrote count on the `live` side (an earlier scout
/// merge is not evidence about a later one). The palette index must be equal;
/// the palette id is compared only when BOTH cells carry one, because the
/// scout stitches with no per-row palettes on the SNES yet, so its cells carry
/// none (a cell with no id says nothing about the palette snapshot).
#[must_use]
pub fn scout_agreement(live: &Canvas, scout: &Canvas, within: Option<Rect>) -> (usize, usize) {
    agreement_at(live, scout, within, 0)
}

/// [`scout_agreement`] with the scout read `dy` rows lower than the live
/// canvas (a cell of the scout at `(x, y)` against the live one at
/// `(x, y + dy)`).
fn agreement_at(live: &Canvas, scout: &Canvas, within: Option<Rect>, dy: i64) -> (usize, usize) {
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
            let (Some(a), Some(b)) = (live.get_live(wx, wy + dy), scout.get(wx, wy)) else {
                continue;
            };
            shared += 1;
            let ids = match (a.sprite_id, b.sprite_id) {
                (Some(x), Some(y)) => x == y,
                _ => true,
            };
            agreed += usize::from(a.palette_index == b.palette_index && ids);
        }
    }
    (agreed, shared)
}

/// The most rows the scout may sit off the live canvas and still be read as
/// the same picture ([`scout_agreement_aligned`]).
pub const MAX_ALIGN_ROWS: i64 = 1;

/// What [`scout_agreement_aligned`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alignment {
    pub agreed: usize,
    pub shared: usize,
    /// The row offset it was measured at (0 = none).
    pub dy: i64,
}

/// [`scout_agreement`] at the best vertical offset within [`MAX_ALIGN_ROWS`].
/// The NES scout stitches a game whose vertical scroll the tracker does not
/// follow (Super Mario Bros. 3 with the player held near the top of the
/// screen) one row off the live canvas: measured on 1-1, 90 % agreement at
/// offset 0 and 99.3 % at +1, on a texture where one row of error flips every
/// edge. The offset is reported, never applied.
#[must_use]
pub fn scout_agreement_aligned(live: &Canvas, scout: &Canvas, within: Option<Rect>) -> Alignment {
    let mut best = Alignment {
        agreed: 0,
        shared: 0,
        dy: 0,
    };
    // Offset 0 first: a tie keeps it.
    for dy in std::iter::once(0).chain((1..=MAX_ALIGN_ROWS).flat_map(|d| [d, -d])) {
        let (agreed, shared) = agreement_at(live, scout, within, dy);
        // Compare ratios without division: agreed/shared > best.agreed/best.shared.
        let better =
            shared > 0 && (best.shared == 0 || agreed * best.shared > best.agreed * shared);
        if better {
            best = Alignment { agreed, shared, dy };
        }
    }
    best
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
/// cells to judge. The scout worker applies it before every delta (W33-06).
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
