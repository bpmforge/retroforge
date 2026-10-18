//! Registration cross-check and placement confidence (ticket W33-04).
//!
//! Placement comes from the scroll registers (bands on the NES, per-line
//! scroll on the SNES), and that stays the only thing that decides where a
//! row lands. This module only CHECKS it: before a frame's rows are written,
//! the frame's own pixels are compared with the cells the canvas already
//! knows at the predicted position. Rows that disagree are withheld (a wrong
//! write corrupts the canvas for the rest of the session; a skipped one costs
//! one frame of coverage), and a per-layer [`Confidence`] (0-3) follows the
//! agreement history. It never moves a cell: a wrong auto-shift is worse than
//! a visible seam (STITCHING_PLAN §2.3).
//!
//! Cost is bounded by construction: every 4th picture row of a band x every
//! 4th column, at the predicted offset only (~3.5 k lookups for a 224-row
//! layer). The +-8 px search runs ONLY on a band that already disagrees, and
//! only to report where its content actually sits ([`Check::residual`]).
//!
//! Evidence rules (so a healthy game is never punished):
//! * a band with fewer than [`MIN_EVIDENCE`] samples that land on known cells
//!   gives NO verdict (the first frame of a scene, a frame revealing new
//!   ground): it is written and moves neither streak;
//! * only `palette_index` is compared (a palette swap changes the cell's
//!   palette id, not its index), and only pixels the layer itself drew
//!   (sprites and other layers are holes);
//! * the predicted offset wins unless fewer than [`AGREE_PERCENT`] % of its
//!   samples agree.
use rf_core_api::PpuPixel;

use super::placement::{layer_pixel, place_rows, Pick, RowPos};
use crate::persistence::Reader;
use crate::stitcher::{stitchable_pixel, Canvas};

/// The widest offset searched around the prediction, in pixels.
pub const MAX_OFFSET: i64 = 8;
/// One sample every this many picture rows / columns.
pub const ROW_STEP: usize = 4;
pub const COL_STEP: usize = 4;
/// The leftmost dots are skipped: the NES can clip them to the backdrop
/// (PPUMASK bit 1, nesdev "PPU registers").
const SKIP_COLUMNS: usize = 8;
/// Samples on known cells a band needs before it can be judged.
pub const MIN_EVIDENCE: u32 = 32;
/// A band agrees when at least this share of its samples match.
pub const AGREE_PERCENT: u32 = 90;
/// Consecutive disagreeing frames per confidence step down.
pub const BAD_FRAMES_PER_STEP: u32 = 3;
/// Consecutive frames without a disagreement per confidence step up (half a
/// second).
pub const GOOD_FRAMES_PER_STEP: u32 = 30;
/// Frames in a row on which a frame the canvas disagrees with entirely agrees
/// with the frames withheld before it (a CANDIDATE canvas): the picture is
/// consistent with itself and the old canvas is the stale one. A screen the
/// scene tracker did not cut (a game-over, the map after a death, a room that
/// looks like the one before) leaves a canvas the picture will never agree
/// with again; withholding every write for good would freeze it and the
/// margins would stay on VRAM until the game quit. A fade or a placement
/// fault is not self-consistent, so it never gets here.
pub const STALE_FRAMES: u32 = 45;
/// Full confidence: the margins may show history and scout cells.
pub const MAX_LEVEL: u8 = 3;

/// What one frame's check found for one layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Check {
    /// Samples that matched the canvas at the predicted position, over the
    /// bands that had enough evidence.
    pub agreed: u32,
    /// Samples that landed on known cells in those bands.
    pub compared: u32,
    /// Picture rows withheld because their band disagreed.
    pub withheld_rows: u32,
    /// Bands that had enough evidence to be judged, and their picture rows.
    pub judged_bands: u32,
    pub judged_rows: u32,
    /// The rows of the whole picture, for judging how much of it was judged.
    pub picture_rows: u32,
    /// Where the content of a disagreeing band matches the canvas best, in
    /// pixels from the prediction (`None` = it matches nowhere in range).
    pub residual: Option<i8>,
    /// What the check cost, in nanoseconds (presentation-side wall time,
    /// reported in the Enhancements panel).
    pub nanos: u32,
}

impl Check {
    /// A quarter of the picture or more was judged: small bands (a split
    /// strip, a parallax stripe on a single canvas) are withheld from writes
    /// when they do not register, but say nothing about the layer.
    fn significant(&self) -> bool {
        self.judged_rows * 4 >= self.picture_rows
    }

    /// Most of the judged rows disagreed, on a significant part of the
    /// picture.
    #[must_use]
    pub fn disagrees(&self) -> bool {
        self.significant() && self.withheld_rows * 2 > self.judged_rows
    }

    /// Every judged band agreed (and there was one to judge).
    #[must_use]
    pub fn agrees(&self) -> bool {
        self.judged_bands > 0 && self.withheld_rows == 0
    }

    /// Every judged band disagreed, and they cover half the picture: the
    /// canvas, not one band, is the odd one out.
    #[must_use]
    pub fn all_disagree(&self) -> bool {
        self.judged_rows > 0
            && self.withheld_rows == self.judged_rows
            && self.judged_rows * 2 >= self.picture_rows
    }
}

/// One layer's placement confidence in a scene: 3 = trusted (margins show
/// history and scout cells), 2 = history only, 1 = the row backdrop only,
/// 0 = VRAM margins as without the stitcher (STITCHING_PLAN §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confidence {
    level: u8,
    bad_streak: u32,
    good_streak: u32,
    /// Frames in a row on which the withheld frames agreed with each other
    /// ([`guard_rows`]).
    candidate_run: u32,
    /// How many times the canvas was dropped as stale in this scene.
    pub resets: u32,
    /// The latest frame's check.
    pub last: Check,
    /// Over the whole scene so far.
    pub total_agreed: u64,
    pub total_compared: u64,
    /// Frames checked and the time all their checks took, for the mean cost.
    pub frames: u64,
    pub total_nanos: u64,
}

impl Default for Confidence {
    /// Nothing has contradicted the registers yet.
    fn default() -> Self {
        Self {
            level: MAX_LEVEL,
            bad_streak: 0,
            good_streak: 0,
            candidate_run: 0,
            resets: 0,
            last: Check::default(),
            total_agreed: 0,
            total_compared: 0,
            frames: 0,
            total_nanos: 0,
        }
    }
}

impl Confidence {
    /// Bytes in [`Self::to_bytes`].
    pub const ENCODED_LEN: usize = 1 + 4 * 4 + 6 * 4 + 2 + 4 + 4 * 8;

    /// Fixed little-endian encoding of every field, for the cache (W33-05).
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::ENCODED_LEN] {
        let mut out = Vec::with_capacity(Self::ENCODED_LEN);
        out.push(self.level);
        for v in [
            self.bad_streak,
            self.good_streak,
            self.candidate_run,
            self.resets,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let c = &self.last;
        for v in [
            c.agreed,
            c.compared,
            c.withheld_rows,
            c.judged_bands,
            c.judged_rows,
            c.picture_rows,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[
            u8::from(c.residual.is_some()),
            c.residual.unwrap_or(0) as u8,
        ]);
        out.extend_from_slice(&c.nanos.to_le_bytes());
        for v in [
            self.total_agreed,
            self.total_compared,
            self.frames,
            self.total_nanos,
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let mut fixed = [0u8; Self::ENCODED_LEN];
        fixed.copy_from_slice(&out);
        fixed
    }

    /// The inverse of [`Self::to_bytes`]; `None` for another length, a level
    /// above [`MAX_LEVEL`] or a flag byte that is neither 0 nor 1.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::ENCODED_LEN {
            return None;
        }
        let mut r = Reader::new(bytes);
        let level = r.u8()?;
        if level > MAX_LEVEL {
            return None;
        }
        let (bad_streak, good_streak, candidate_run, resets) =
            (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        let (agreed, compared, withheld_rows) = (r.u32()?, r.u32()?, r.u32()?);
        let (judged_bands, judged_rows, picture_rows) = (r.u32()?, r.u32()?, r.u32()?);
        let (flag, raw) = (r.u8()?, r.u8()? as i8);
        let residual = match flag {
            0 if raw == 0 => None,
            1 => Some(raw),
            _ => return None,
        };
        let nanos = r.u32()?;
        let (total_agreed, total_compared, frames, total_nanos) =
            (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
        Some(Self {
            level,
            bad_streak,
            good_streak,
            candidate_run,
            resets,
            last: Check {
                agreed,
                compared,
                withheld_rows,
                judged_bands,
                judged_rows,
                picture_rows,
                residual,
                nanos,
            },
            total_agreed,
            total_compared,
            frames,
            total_nanos,
        })
    }

    #[must_use]
    pub fn level(&self) -> u8 {
        self.level
    }

    /// Fold one frame's check in.
    pub fn observe(&mut self, check: Check) {
        self.last = check;
        self.total_agreed += u64::from(check.agreed);
        self.total_compared += u64::from(check.compared);
        self.frames += 1;
        self.total_nanos += u64::from(check.nanos);
        if check.disagrees() {
            self.good_streak = 0;
            self.bad_streak += 1;
            if self.bad_streak >= BAD_FRAMES_PER_STEP {
                self.bad_streak = 0;
                self.level = self.level.saturating_sub(1);
            }
        } else {
            // Not disagreeing: the trouble is not consecutive any more, so an
            // isolated bad frame (the NES test fixture's two-frame band
            // misreads) never adds up to a confidence step.
            self.bad_streak = 0;
            // Nothing contradicted the registers, which is all a frame with no
            // verdict (a still camera: its band is HUD and not placed) can
            // say; counting it lets a score recover while the camera rests.
            self.good_streak += 1;
            if self.good_streak >= GOOD_FRAMES_PER_STEP {
                self.good_streak = 0;
                self.level = (self.level + 1).min(MAX_LEVEL);
            }
        }
    }

    /// The share of samples that agreed over the scene, in percent.
    #[must_use]
    pub fn agreement_percent(&self) -> Option<f32> {
        (self.total_compared > 0)
            .then(|| self.total_agreed as f32 * 100.0 / self.total_compared as f32)
    }
}

/// One layer's guarded write: check the frame ([`gate_rows`]), keep the layer's
/// [`Confidence`], and decide whether the canvas is stale.
///
/// A frame the canvas disagrees with ENTIRELY is also learned into a
/// `candidate` canvas, and checked against what earlier withheld frames put
/// there. Frames that agree with each other for [`STALE_FRAMES`] in a row are
/// a new picture the tracker did not cut: the candidate replaces the canvas
/// and the confidence starts over. Frames that do not (a jittering band split,
/// a fade) never promote. As soon as the canvas agrees again, the candidate is
/// forgotten. `rows` comes back with the withheld rows set to `None`, ready for
/// [`place_rows`].
pub fn guard_rows(
    canvas: &mut Canvas,
    candidate: &mut Canvas,
    confidence: &mut Confidence,
    video: &[PpuPixel],
    width: usize,
    rows: &mut [Option<RowPos>],
    pick: &Pick<'_>,
) {
    let wanted = rows.to_vec();
    let check = gate_rows(canvas, video, width, rows, pick);
    confidence.observe(check);
    if !check.all_disagree() {
        if candidate.width() > 0 {
            *candidate = Canvas::new();
        }
        confidence.candidate_run = 0;
        return;
    }
    let mut cand_rows = wanted;
    let seen = gate_rows(candidate, video, width, &mut cand_rows, pick);
    place_rows(candidate, video, width, &cand_rows, pick, &mut None);
    if seen.agrees() {
        confidence.candidate_run += 1;
    } else if seen.judged_bands > 0 {
        confidence.candidate_run = 0;
    }
    if confidence.candidate_run >= STALE_FRAMES {
        *canvas = std::mem::take(candidate);
        *confidence = Confidence {
            resets: confidence.resets + 1,
            ..Confidence::default()
        };
    }
}

/// Compare a frame with `canvas` at the predicted positions `rows` and WITHHOLD
/// (set to `None`) the rows of every band that disagrees. A band is a run of
/// consecutive rows sharing one world x and stepping one world y per row (a
/// NES scroll band, or SNES rows under one scroll value).
pub fn gate_rows(
    canvas: &Canvas,
    video: &[PpuPixel],
    width: usize,
    rows: &mut [Option<RowPos>],
    pick: &Pick<'_>,
) -> Check {
    let mut check = Check::default();
    if width == 0 || canvas.width() == 0 {
        return check;
    }
    check.picture_rows = (video.len() / width).min(rows.len()) as u32;
    let started = std::time::Instant::now();
    let height = (video.len() / width).min(rows.len());
    // (world x, world y, palette index) of the band's samples.
    let mut samples: Vec<(i64, i64, u8)> = Vec::new();
    let mut start = 0usize;
    // Bounded: `start` is replaced by `end > start` on every pass (law 8).
    while start < height {
        let Some(first) = rows[start] else {
            start += 1;
            continue;
        };
        let mut end = start + 1;
        while end < height
            && rows[end].is_some_and(|p| p.x == first.x && p.y == first.y + (end - start) as i64)
        {
            end += 1;
        }
        samples.clear();
        for sy in (start..end).step_by(ROW_STEP) {
            let Some(pos) = rows[sy] else { continue };
            let line = &video[sy * width..(sy + 1) * width];
            for c in (SKIP_COLUMNS..width).step_by(COL_STEP) {
                if let Some(p) = sample(pick, sy, c, &line[c]) {
                    samples.push((pos.x + c as i64, pos.y, p.palette_index));
                }
            }
        }
        let (agreed, compared) = score(canvas, &samples, 0);
        if compared >= MIN_EVIDENCE {
            check.judged_bands += 1;
            check.judged_rows += (end - start) as u32;
            check.agreed += agreed;
            check.compared += compared;
            if agreed * 100 < compared * AGREE_PERCENT {
                check.withheld_rows += (end - start) as u32;
                check.residual = check.residual.or(best_offset(canvas, &samples));
                for slot in &mut rows[start..end] {
                    *slot = None;
                }
            }
        }
        start = end;
    }
    check.nanos = u32::try_from(started.elapsed().as_nanos()).unwrap_or(u32::MAX);
    check
}

/// The pixel of `pick` worth comparing at column `c` of picture row `sy`.
fn sample(pick: &Pick<'_>, sy: usize, c: usize, p: &PpuPixel) -> Option<PpuPixel> {
    match pick {
        Pick::Level => stitchable_pixel(p),
        Pick::Layer { wanted, sub, ids } => layer_pixel(
            *wanted,
            sub.get(sy).and_then(Option::as_ref),
            ids.get(sy).copied().flatten(),
            c,
            p,
        ),
    }
}

/// `(agreed, compared)` of `samples` against the canvas shifted by `dx`.
fn score(canvas: &Canvas, samples: &[(i64, i64, u8)], dx: i64) -> (u32, u32) {
    let (mut agreed, mut compared) = (0, 0);
    for &(x, y, idx) in samples {
        // A scout cell is not evidence about a live frame (W33-06).
        if let Some(cell) = canvas.get_live(x + dx, y) {
            compared += 1;
            agreed += u32::from(cell.palette_index == idx);
        }
    }
    (agreed, compared)
}

/// Where, within `MAX_OFFSET` of the prediction, the samples agree with the
/// canvas, if anywhere (nearest to the prediction on a tie). The result is
/// the frame content's true x minus the predicted x.
fn best_offset(canvas: &Canvas, samples: &[(i64, i64, u8)]) -> Option<i8> {
    let mut best: Option<(u32, i64)> = None;
    for dx in -MAX_OFFSET..=MAX_OFFSET {
        let (agreed, compared) = score(canvas, samples, dx);
        if dx == 0 || compared < MIN_EVIDENCE || agreed * 100 < compared * AGREE_PERCENT {
            continue;
        }
        let pct = agreed * 1000 / compared;
        if best.is_none_or(|(b, bdx)| pct > b || (pct == b && dx.abs() < bdx.abs())) {
            best = Some((pct, dx));
        }
    }
    best.and_then(|(_, dx)| i8::try_from(dx).ok())
}
