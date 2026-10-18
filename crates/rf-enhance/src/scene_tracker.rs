//! Scene identity (tickets W4-03d, W33-09). Which scene is the game showing:
//! the title, the map, a level, the room behind a pipe? The canvas model keeps
//! one canvas set per scene, so a wrong answer piles screens on top of each
//! other (the cause of the one NES scene id found on 2026-10-17, below).
//!
//! ## What W33-09 measured (Super Mario Bros. 3 and Mega Man 2, headless)
//!
//! `docs/research/W33-09-scenes.md` has the traces. Three separate faults made
//! the id constant `6570426d2c2e2fa3` over a whole session:
//!
//! 1. **The hash was constant.** [`crate::scene_identity::compute_scene_id`]
//!    thresholds adjacent-block differences of `palette_index` averages at 48;
//!    real NES indices never differ that much, so all 112 edges are "flat" and
//!    EVERY NES frame of EVERY game hashes the same. Even the jumps the tracker
//!    did see (SMB3 map to level, a 208 px cut) recomputed the same value.
//! 2. **Most cuts were never seen.** A cut was "world jump over 64 px or a
//!    mapper bank change". Title to map to level lands at scroll 0 each time,
//!    and `rf-nes` exposes no bank state (the feed is `&[]`). A fade through a
//!    black frame (no bands) was deliberately "no evidence".
//! 3. **A scroll-axis wrap was a cut.** MM2's vertical section writes Y
//!    `0..239` and wraps at 240; the tracker's Y axis wraps at 480, so
//!    `236 -> 0` was read as 236 px backwards (a "cut" every 240 px of climb,
//!    and a smeared canvas). [`crate::scroll_tracker::WorldAxis::advance_y`].
//!
//! ## The key of a scene
//!
//! 1. **The profile's level id**, when the profile declares one
//!    (`[[scene.level_id]]`, read by the caller every frame into
//!    [`SceneHints::level`]): `id = hash(bytes)`. Nothing about the picture, so
//!    a pipe room is one scene, the level it came from is another, and coming
//!    back to the level gives back the SAME id (its canvas is intact).
//!    `None` means "not in a level" (a map, a title) or "no rows": the visual
//!    rules below decide.
//! 2. **The picture**, otherwise: the set of dominant palette indices
//!    ([`crate::scene_identity::palette_scene_id`]). A title, a map and a level
//!    each draw with their own palette.
//! 3. **A section** of a scene: a switch of the scroll axis (MM2 leaves its
//!    horizontal section and climbs). Section `n` of a scene is a scene of its
//!    own and the two are joined by a continuous edge.
//!
//! ## What is a cut
//!
//! * the level key changes (or appears, or goes away);
//! * the mapper bank state changes (kept; no core supplies it yet);
//! * the colour histogram of the picture moves by more than [`CUT_PERMILLE`]
//!   between two frames (a title to a map, a black frame, a fade step);
//! * the scroll jumped more than [`MAX_CONTINUOUS_STEP_PX`] AND the picture
//!   is not the previous one moved by that scroll. A 64+ px fall in a vertical
//!   section moves the picture by exactly the measured scroll, so it matches
//!   and is NOT a cut (the old rule cut on the number alone). Where the jump
//!   leaves no overlap to compare, the colours decide;
//! * the scroll axis switches after a long run on the other one
//!   ([`SceneHints::axis_sections`]).
//!
//! A cut starts SETTLING: the frames are not any scene's (a fade, a palette
//! load, a level still writing its pointers) and are not stitched
//! ([`SETTLING`]); the new id is taken when the picture or the key has held for
//! [`SETTLE_FRAMES`] frames (at most [`SETTLE_CAP`]). The scene that was left
//! is untouched while this goes on.
//!
//! ## Edges
//!
//! Every accepted change records a [`SceneEdge`]: from, to, the scroll
//! position at each end, and whether the picture flowed on across the cut
//! (`continuous`: the new frame matched the old one moved by the measured
//! scroll). Only a continuous edge may show a neighbour scene in the margin;
//! a door, a pipe and a fade are never continuous. Edges are plain integers
//! ([`SceneEdge::to_bytes`]) so persistence (W33-05) can store them per ROM.
//!
//! ## History (W4-03d)
//!
//! Identity is stateful: while nothing cuts, the scene id is returned
//! unchanged, so a scrolling level is one scene (the raw per-frame hash
//! fragments about once per block width, measured then). NROM fixtures
//! (RF-Scroller) exercise only the scroll and picture halves.

use rf_core_api::{CoreEvent, PpuPixel};

use crate::scene_identity::{level_scene_id, palette_scene_id, section_scene_id, SceneId};
use crate::scroll_tracker::{BandWorldPos, ScanlineBand, ScrollTracker};
use crate::stitcher::stitchable_pixel;

/// The id returned while a cut has not settled: not a scene, nothing is
/// stitched under it. (`u64::MAX` is not a reachable FNV-1a result of any
/// input the tracker hashes in practice; callers compare with `==`.)
pub const SETTLING: SceneId = SceneId(u64::MAX);

/// Max plausible frame-to-frame world-position delta (either axis, pixels)
/// that is "smooth scrolling" without further evidence. Above it the picture
/// is compared with the previous one moved by the scroll (module doc).
pub const MAX_CONTINUOUS_STEP_PX: i64 = 64;

/// Per mille of the background pixels whose palette index differs between two
/// consecutive frames' histograms that makes a frame a cut.
pub const CUT_PERMILLE: u32 = 300;
/// Below this the picture counts as held while settling.
pub const STABLE_PERMILLE: u32 = 60;
/// Frames the picture (or the key) must hold before a new scene is named.
pub const SETTLE_FRAMES: u32 = 12;
/// A scene that never holds still is named anyway after this many frames.
pub const SETTLE_CAP: u32 = 120;
/// Share (per cent) of the compared pixels of the moved previous frame that
/// must equal the new frame for "the picture flowed on".
pub const MATCH_PERCENT: u32 = 70;
/// Net travel along one axis (px) before the other axis may start a section.
pub const AXIS_TRAVEL_PX: i64 = 256;
/// Frames of motion the axis rule looks at.
const AXIS_WINDOW: usize = 32;
/// Net motion over the window that names an axis, and what the other axis may
/// move meanwhile.
const AXIS_MOVE_PX: i64 = 16;
const AXIS_OTHER_PX: i64 = 8;
/// Every n-th pixel is compared when matching two frames.
const SAMPLE_STEP: usize = 4;
/// Fewer compared pixels than this is no evidence either way.
const MIN_COMPARED: u32 = 50;
const NOT_BACKGROUND: u16 = u16::MAX;

/// What the caller knows that the picture does not say.
#[derive(Debug, Clone, Copy, Default)]
pub struct SceneHints<'a> {
    /// The bytes the profile's `[[scene.level_id]]` rows read this frame;
    /// `None` when there are none, or the game is not in a level (a title, a
    /// map: the rows read their `idle` values).
    pub level: Option<&'a [u8]>,
    /// Whether a switch of the scroll axis starts a new section. `None`: yes
    /// when there is no level key (an unprofiled game), no when there is one.
    pub axis_sections: Option<bool>,
}

/// A change of scene, recorded where it happened. Plain data (module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneEdge {
    pub from: u64,
    pub to: u64,
    /// The primary band's world position in the last frame of `from`.
    pub from_pos: (i64, i64),
    /// ... and in the first settled frame of `to`.
    pub to_pos: (i64, i64),
    /// The picture flowed on across the cut: only such an edge may show the
    /// neighbour in a margin.
    pub continuous: bool,
}

impl SceneEdge {
    /// Bytes in [`Self::to_bytes`].
    pub const ENCODED_LEN: usize = 8 * 6 + 1;

    /// Fixed little-endian encoding, for the cache (W33-05).
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        let words = [
            self.from,
            self.to,
            self.from_pos.0 as u64,
            self.from_pos.1 as u64,
            self.to_pos.0 as u64,
            self.to_pos.1 as u64,
        ];
        for (chunk, word) in out.chunks_exact_mut(8).zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        out[Self::ENCODED_LEN - 1] = u8::from(self.continuous);
        out
    }

    /// The inverse of [`Self::to_bytes`]; `None` for another length or a flag
    /// byte that is neither 0 nor 1.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::ENCODED_LEN {
            return None;
        }
        let word = |i: usize| {
            let mut w = [0u8; 8];
            w.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
            u64::from_le_bytes(w)
        };
        let continuous = match bytes[Self::ENCODED_LEN - 1] {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(Self {
            from: word(0),
            to: word(1),
            from_pos: (word(2) as i64, word(3) as i64),
            to_pos: (word(4) as i64, word(5) as i64),
            continuous,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// The scroll-axis rule (module doc): a sliding window of the last frames'
/// world deltas names the axis the screen is moving along; a switch counts
/// only after [`AXIS_TRAVEL_PX`] of travel on the old one.
#[derive(Clone, Default)]
struct AxisTracker {
    window: [(i64, i64); AXIS_WINDOW],
    next: usize,
    axis: Option<Axis>,
    travel: i64,
}

impl AxisTracker {
    /// Feed one frame's world delta; `true` when the axis just switched.
    fn update(&mut self, delta: (i64, i64)) -> bool {
        self.window[self.next] = delta;
        self.next = (self.next + 1) % AXIS_WINDOW;
        let (sx, sy) = self
            .window
            .iter()
            .fold((0, 0), |(x, y), (dx, dy)| (x + dx, y + dy));
        let now = if sx.abs() >= AXIS_MOVE_PX && sy.abs() < AXIS_OTHER_PX {
            Some(Axis::Horizontal)
        } else if sy.abs() >= AXIS_MOVE_PX && sx.abs() < AXIS_OTHER_PX {
            Some(Axis::Vertical)
        } else {
            None
        };
        // Travel counts along the axis the section is on, whatever the
        // window says this frame: a slow walk (Mega Man 2 goes about 1.3
        // px a frame) is still a long walk.
        match self.axis {
            Some(Axis::Horizontal) => self.travel += delta.0.abs(),
            Some(Axis::Vertical) => self.travel += delta.1.abs(),
            None => {}
        }
        match (self.axis, now) {
            (None, Some(a)) => {
                self.axis = Some(a);
                self.travel = 0;
                false
            }
            (Some(old), Some(a)) if a != old && self.travel >= AXIS_TRAVEL_PX => {
                self.axis = Some(a);
                self.travel = 0;
                true
            }
            _ => false,
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    Key,
    Bank,
    Picture,
    Jump,
    Axis,
}

/// A cut waiting to settle.
#[derive(Clone, Copy)]
struct Pending {
    from: Option<SceneId>,
    from_pos: (i64, i64),
    continuous: bool,
    /// Every cause so far was an axis switch (the new scene is a SECTION of
    /// the old one, not a new level).
    axis_only: bool,
    stable: u32,
    total: u32,
}

/// Stateful scene identity (module doc). One instance tracks one play
/// session; construct a fresh one per ROM/run, same lifetime shape as
/// [`ScrollTracker`] itself.
#[derive(Clone)]
pub struct SceneTracker {
    scroll: ScrollTracker,
    committed: Option<SceneId>,
    pending: Option<Pending>,
    last_world: Option<(i64, i64)>,
    last_bank_state: Vec<u8>,
    key: Option<Vec<u8>>,
    last_histogram: Option<Vec<u32>>,
    last_bg: Vec<u16>,
    axis: AxisTracker,
    /// Sections of the base scene (axis switches since it began).
    sections: u32,
    /// The base id the sections hang off.
    base: SceneId,
    edges: Vec<SceneEdge>,
}

impl SceneTracker {
    #[must_use]
    pub fn new() -> Self {
        SceneTracker {
            scroll: ScrollTracker::new(),
            committed: None,
            pending: None,
            last_world: None,
            last_bank_state: Vec::new(),
            key: None,
            last_histogram: None,
            last_bg: Vec::new(),
            axis: AxisTracker::default(),
            sections: 0,
            base: SceneId(0),
            edges: Vec::new(),
        }
    }

    /// [`Self::observe`] with no hints: the picture and the scroll decide.
    pub fn observe_frame(
        &mut self,
        video: &[PpuPixel],
        width: u16,
        height: u16,
        events: &[CoreEvent],
        mapper_bank_state: &[u8],
    ) -> SceneId {
        self.observe(
            video,
            width,
            height,
            events,
            mapper_bank_state,
            SceneHints::default(),
        )
    }

    /// Observe one frame and return its scene, or [`SETTLING`] while a cut
    /// has not settled (module doc). `video`/`width`/`height` are
    /// `FrameBundle`'s own shape; `events` is that frame's
    /// `FrameBundle::events`; `mapper_bank_state` is the caller-supplied bank
    /// bytes (`&[]` when the core has none).
    pub fn observe(
        &mut self,
        video: &[PpuPixel],
        width: u16,
        _height: u16,
        events: &[CoreEvent],
        mapper_bank_state: &[u8],
        hints: SceneHints<'_>,
    ) -> SceneId {
        let bands = self.scroll.observe_frame(events);
        let world = primary_world_position(&bands);
        let band_rows = primary_rows(&bands);
        let (histogram, bg) = colours(video);
        let permille = self
            .last_histogram
            .as_deref()
            .map(|h| histogram_distance_permille(h, &histogram));
        let delta = match (self.last_world, world) {
            (Some(p), Some(c)) => Some((c.0 - p.0, c.1 - p.1)),
            _ => None,
        };
        let jump = delta.map_or(0, |(dx, dy)| dx.abs().max(dy.abs()));
        let matched = delta.and_then(|(dx, dy)| {
            moved_match_percent(&self.last_bg, &bg, usize::from(width), band_rows, dx, dy)
        });
        let flowed = matched.is_some_and(|m| m >= MATCH_PERCENT);

        let bank_changed = mapper_bank_state != self.last_bank_state.as_slice();
        let key_changed = hints.level != self.key.as_deref();
        let axis_on = hints.axis_sections.unwrap_or(hints.level.is_none());
        let axis_switched = self.axis.update(delta.unwrap_or((0, 0))) && axis_on;

        let cause = if self.committed.is_none() {
            None
        } else if key_changed {
            Some(Cause::Key)
        } else if bank_changed {
            Some(Cause::Bank)
        } else if hints.level.is_some() {
            // A level the profile names is the profile's to end.
            axis_switched.then_some(Cause::Axis)
        } else if permille.is_some_and(|p| p > CUT_PERMILLE) {
            Some(Cause::Picture)
        } else if jump > MAX_CONTINUOUS_STEP_PX && !flowed && matched.is_some() {
            Some(Cause::Jump)
        } else if jump > MAX_CONTINUOUS_STEP_PX
            && matched.is_none()
            && permille.is_some_and(|p| p > STABLE_PERMILLE)
        {
            // Nothing to compare (the jump left no overlap): the colours must
            // have moved at all.
            Some(Cause::Jump)
        } else {
            axis_switched.then_some(Cause::Axis)
        };

        let first = self.committed.is_none();
        let held = permille.is_none_or(|p| p < STABLE_PERMILLE);
        let stable_now = if hints.level.is_some() {
            !key_changed && !bank_changed
        } else {
            held && !bank_changed
        };

        if let Some(cause) = cause {
            // The screen was scrolling and the picture went on as the scroll
            // says: nothing on screen changed at the cut. A cursor flipping a
            // key on a still menu is not a scroll edge.
            let continuous = matches!(cause, Cause::Key | Cause::Axis)
                && flowed
                && (1..=MAX_CONTINUOUS_STEP_PX).contains(&jump);
            match self.pending.as_mut() {
                Some(p) => {
                    p.stable = 0;
                    p.total += 1;
                    p.continuous &= continuous;
                    p.axis_only &= cause == Cause::Axis;
                }
                None => {
                    self.pending = Some(Pending {
                        from: self.committed,
                        from_pos: self.last_world.unwrap_or((0, 0)),
                        continuous,
                        axis_only: cause == Cause::Axis,
                        stable: 0,
                        total: 0,
                    });
                }
            }
        } else if let Some(p) = self.pending.as_mut() {
            p.total += 1;
            p.stable = if stable_now { p.stable + 1 } else { 0 };
        }

        let settled = self
            .pending
            .is_some_and(|p| p.stable >= SETTLE_FRAMES || p.total >= SETTLE_CAP);
        if first || settled {
            let pending = self.pending.take();
            let axis_only = pending.is_some_and(|p| p.axis_only);
            let base = if first || !axis_only {
                self.sections = 0;
                self.axis.reset();
                match hints.level {
                    Some(level) => level_scene_id(level),
                    None => palette_scene_id(&histogram, mapper_bank_state),
                }
            } else {
                self.sections += 1;
                self.base
            };
            self.base = base;
            let id = section_scene_id(base, self.sections);
            if let Some(p) = pending {
                if let Some(from) = p.from.filter(|&f| f != id) {
                    self.record(SceneEdge {
                        from: from.0,
                        to: id.0,
                        from_pos: p.from_pos,
                        to_pos: world.unwrap_or(p.from_pos),
                        continuous: p.continuous,
                    });
                }
            }
            self.committed = Some(id);
        }

        self.last_world = world;
        self.last_bank_state.clear();
        self.last_bank_state.extend_from_slice(mapper_bank_state);
        self.key = hints.level.map(<[u8]>::to_vec);
        self.last_histogram = Some(histogram);
        self.last_bg = bg;

        if self.pending.is_some() {
            SETTLING
        } else {
            self.committed.unwrap_or(SETTLING)
        }
    }

    fn record(&mut self, edge: SceneEdge) {
        match self
            .edges
            .iter_mut()
            .find(|e| e.from == edge.from && e.to == edge.to)
        {
            Some(slot) => *slot = edge,
            None => self.edges.push(edge),
        }
    }

    /// Ticket W33-05: edges restored from the cache (the latest per
    /// `(from, to)` wins, as when they were recorded).
    pub fn restore_edges(&mut self, edges: &[SceneEdge]) {
        for edge in edges {
            self.record(*edge);
        }
    }

    /// Every scene change seen so far, oldest first; one per `(from, to)`
    /// pair (the latest wins). Plain data for the cache.
    #[must_use]
    pub fn edges(&self) -> &[SceneEdge] {
        &self.edges
    }

    /// The scene the margin of `scene` may show beyond its scroll edge: the
    /// other end of a CONTINUOUS edge, never of a door, a pipe or a fade.
    #[must_use]
    pub fn continuous_neighbours(&self, scene: SceneId) -> Vec<SceneEdge> {
        self.edges
            .iter()
            .filter(|e| e.continuous && (e.from == scene.0 || e.to == scene.0))
            .copied()
            .collect()
    }

    /// The primary-band world position as of the most recent
    /// [`Self::observe`] call (`None` before the first call, or if that frame
    /// had no band at all). Observability, and the scroll position an edge
    /// is recorded at.
    #[must_use]
    pub fn last_world_position(&self) -> Option<(i64, i64)> {
        self.last_world
    }
}

impl Default for SceneTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Histogram of the background pixels' palette indices, and the picture with
/// every non-background pixel marked (sprites move on their own).
fn colours(video: &[PpuPixel]) -> (Vec<u32>, Vec<u16>) {
    let mut histogram = vec![0u32; 256];
    let bg = video
        .iter()
        .map(|p| match stitchable_pixel(p) {
            Some(b) => {
                histogram[usize::from(b.palette_index)] += 1;
                u16::from(b.palette_index)
            }
            None => NOT_BACKGROUND,
        })
        .collect();
    (histogram, bg)
}

/// Half the L1 distance between two histograms, per mille of the larger
/// total: 0 for the same colours, 1000 for none in common.
fn histogram_distance_permille(a: &[u32], b: &[u32]) -> u32 {
    let ta: u64 = a.iter().map(|&c| u64::from(c)).sum();
    let tb: u64 = b.iter().map(|&c| u64::from(c)).sum();
    let total = ta.max(tb).max(1);
    let sum: u64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| u64::from(x.abs_diff(y)))
        .sum();
    // Pixels that are sprites in one frame and background in the other count
    // as differences in the histogram's total: the larger total is the scale.
    let extra = ta.abs_diff(tb);
    u32::try_from((sum.saturating_sub(extra) / 2 + extra) * 1000 / total).unwrap_or(1000)
}

/// Share (per cent) of the sampled background pixels of `now` that equal the
/// pixel of `before` the measured scroll `(dx, dy)` says they came from;
/// `None` when too few pixels overlap to say. Only the primary band's rows
/// are compared: a HUD does not move with the scroll.
fn moved_match_percent(
    before: &[u16],
    now: &[u16],
    width: usize,
    rows: Option<(usize, usize)>,
    dx: i64,
    dy: i64,
) -> Option<u32> {
    if width == 0 || before.len() != now.len() || before.is_empty() {
        return None;
    }
    let height = now.len() / width;
    let (first, last) = rows.unwrap_or((0, height));
    let (mut compared, mut same) = (0u32, 0u32);
    for y in (first..last.min(height)).step_by(SAMPLE_STEP) {
        let Some(py) = usize::try_from(y as i64 + dy).ok().filter(|&p| p < height) else {
            continue;
        };
        for x in (0..width).step_by(SAMPLE_STEP) {
            let Some(px) = usize::try_from(x as i64 + dx).ok().filter(|&p| p < width) else {
                continue;
            };
            let (a, b) = (now[y * width + x], before[py * width + px]);
            if a == NOT_BACKGROUND || b == NOT_BACKGROUND {
                continue;
            }
            compared += 1;
            same += u32::from(a == b);
        }
    }
    (compared >= MIN_COMPARED).then(|| same * 100 / compared)
}

/// The band that drives world-space continuity: the LARGEST (by row span)
/// band this frame, matching `ScrollTracker::observe_frame`'s own "primary"
/// selection (that field is private; duplicated here as one `max_by_key`
/// rather than widening `ScrollTracker`'s public surface for a single
/// caller). `saturating_sub`: a band whose `end < start` (the real-fixture
/// leaked-scanline case, `crate::scroll_tracker`'s module-level fix) must
/// never win by wrapping to a huge width.
fn primary_world_position(bands: &[(ScanlineBand, BandWorldPos)]) -> Option<(i64, i64)> {
    bands
        .iter()
        .max_by_key(|(b, _)| b.end.saturating_sub(b.start))
        .map(|(_, pos)| (pos.world_x, pos.world_y_at_start))
}

/// The row range of the primary band (the rows that scroll).
fn primary_rows(bands: &[(ScanlineBand, BandWorldPos)]) -> Option<(usize, usize)> {
    bands
        .iter()
        .filter(|(b, _)| b.end > b.start)
        .max_by_key(|(b, _)| b.end - b.start)
        .map(|(b, _)| (usize::from(b.start), usize::from(b.end)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::PixelLayer;

    const WIDTH: u16 = 256;
    const HEIGHT: u16 = 240;
    const HUD_ROWS: u16 = 16;

    fn scroll(x: u16, y: u16) -> CoreEvent {
        CoreEvent::ScrollWrite {
            x,
            y,
            layer: PixelLayer::Background(0),
        }
    }

    fn scanlines(range: std::ops::Range<u16>) -> Vec<CoreEvent> {
        range.map(CoreEvent::Scanline).collect()
    }

    fn bg_pixel(v: u8) -> PpuPixel {
        PpuPixel {
            palette_index: v,
            layer: PixelLayer::Background(0),
            sprite_id: None,
            priority: 0,
        }
    }

    /// A "world texture": 8x8 cells of one of 24 colours, pseudo-random in
    /// the cell, so the colour histogram of a screen is much the same
    /// wherever it looks while the picture itself moves with the scroll.
    /// `base` shifts the palette (a different scene has other colours).
    fn cell_colour(cx: i64, cy: i64, base: u8) -> u8 {
        let h = (cx.wrapping_mul(7919) ^ cy.wrapping_mul(104_729) ^ (cx * cy)).rem_euclid(24);
        base + h as u8
    }

    /// The screen at world scroll `(wx, wy)`: HUD rows fixed, the rest the
    /// texture seen through the scroll.
    fn frame_at(wx: i64, wy: i64, base: u8) -> Vec<PpuPixel> {
        let (width, height) = (WIDTH as usize, HEIGHT as usize);
        let mut video = vec![bg_pixel(0); width * height];
        for y in 0..height {
            for x in 0..width {
                video[y * width + x] = if y < HUD_ROWS as usize {
                    bg_pixel(60)
                } else {
                    let (ax, ay) = (x as i64 + wx, y as i64 + wy);
                    bg_pixel(cell_colour(ax.div_euclid(8), ay.div_euclid(8), base))
                };
            }
        }
        video
    }

    /// The same cells, laid out differently (same colours, other picture).
    fn frame_rearranged(wx: i64, wy: i64, base: u8) -> Vec<PpuPixel> {
        let (width, height) = (WIDTH as usize, HEIGHT as usize);
        let mut video = vec![bg_pixel(0); width * height];
        for y in 0..height {
            for x in 0..width {
                video[y * width + x] = if y < HUD_ROWS as usize {
                    bg_pixel(60)
                } else {
                    let (ax, ay) = (x as i64 + wx, y as i64 + wy);
                    bg_pixel(cell_colour(
                        ay.div_euclid(8) + 5,
                        ax.div_euclid(8) + 9,
                        base,
                    ))
                };
            }
        }
        video
    }

    fn uniform(v: u8) -> Vec<PpuPixel> {
        vec![bg_pixel(v); WIDTH as usize * HEIGHT as usize]
    }

    /// One frame's events: HUD band fixed at (0,0), the level band scrolled to
    /// `(wx, wy)` (raw registers, wrapped to the nametables).
    fn events_at(wx: i64, wy: i64) -> Vec<CoreEvent> {
        let mut events = vec![scroll(0, 0)];
        events.extend(scanlines(0..HUD_ROWS));
        events.push(scroll(wx.rem_euclid(512) as u16, wy.rem_euclid(480) as u16));
        events.extend(scanlines(HUD_ROWS..HEIGHT));
        events
    }

    fn step(t: &mut SceneTracker, wx: i64, wy: i64, base: u8) -> SceneId {
        t.observe_frame(
            &frame_at(wx, wy, base),
            WIDTH,
            HEIGHT,
            &events_at(wx, wy),
            &[],
        )
    }

    /// Feed `n` identical frames of a (still) scene; the last id.
    fn hold(t: &mut SceneTracker, wx: i64, wy: i64, base: u8, n: u32) -> SceneId {
        let mut id = SETTLING;
        for _ in 0..n {
            id = step(t, wx, wy, base);
        }
        id
    }

    fn level_step(t: &mut SceneTracker, wx: i64, wy: i64, level: Option<&[u8]>) -> SceneId {
        t.observe(
            &frame_at(wx, wy, 1),
            WIDTH,
            HEIGHT,
            &events_at(wx, wy),
            &[],
            SceneHints {
                level,
                axis_sections: None,
            },
        )
    }

    // --- Stability: a sustained scrolling session is ONE scene id -------

    #[test]
    fn a_sustained_3px_per_frame_scrolling_session_stays_one_scene_id() {
        use std::collections::HashSet;
        let mut tracker = SceneTracker::new();
        let mut seen = HashSet::new();
        for frame in 0..200i64 {
            seen.insert(step(&mut tracker, frame * 3, 0, 1));
        }
        assert_eq!(
            seen.len(),
            1,
            "a continuous 3px/frame session over 200 frames (597px of travel) must stay one \
             scene, not fragment like the raw per-frame hash did -- got {} ids",
            seen.len()
        );
        assert!(!seen.contains(&SETTLING));
        assert!(tracker.edges().is_empty());
    }

    #[test]
    fn fifty_frames_of_sprite_noise_layered_over_a_scrolling_session_stay_one_scene_id() {
        use std::collections::HashSet;
        let mut tracker = SceneTracker::new();
        let mut seen = HashSet::new();
        for frame in 0..50i64 {
            let shift = frame * 3;
            let mut video = frame_at(shift, 0, 1);
            // An 8x8 sprite sweeping the screen: not background, so neither
            // the colours nor the picture comparison count it.
            let (sx, sy) = (
                ((frame as u32 * 5) % (WIDTH as u32 - 8)) as usize,
                HUD_ROWS as usize
                    + ((frame as u32 * 3) % (HEIGHT as u32 - HUD_ROWS as u32 - 8)) as usize,
            );
            for y in 0..8 {
                for x in 0..8 {
                    video[(sy + y) * WIDTH as usize + sx + x] = PpuPixel {
                        palette_index: 255,
                        layer: PixelLayer::Sprite,
                        sprite_id: Some(0),
                        priority: 0,
                    };
                }
            }
            seen.insert(tracker.observe_frame(&video, WIDTH, HEIGHT, &events_at(shift, 0), &[]));
        }
        assert_eq!(seen.len(), 1, "ids: {seen:?}");
    }

    // --- The measured fault: the id was one constant ---------------------

    #[test]
    fn a_title_a_map_and_a_level_are_three_scenes_though_the_scroll_never_moves() {
        // W33-09: SMB3 and MM2 changed screens at scroll (0,0) every time, and
        // the old hash was one constant, so the whole session was one id.
        let mut t = SceneTracker::new();
        let title = hold(&mut t, 0, 0, 1, 30);
        assert_ne!(title, SETTLING);
        let map = hold(&mut t, 0, 0, 30, 30);
        let level = hold(&mut t, 0, 0, 40, 30);
        assert!(map != SETTLING && level != SETTLING);
        assert_ne!(title, map, "title and map share an id");
        assert_ne!(map, level, "map and level share an id");
        assert_ne!(title, level);
        // Going back to the title's colours gives the title's id again.
        assert_eq!(hold(&mut t, 0, 0, 1, 30), title);
    }

    #[test]
    fn a_cut_is_unsettled_until_the_picture_holds_and_leaves_no_scene_behind() {
        let mut t = SceneTracker::new();
        let before = hold(&mut t, 0, 0, 1, 20);
        // The first frame of the new picture is a cut: no scene yet.
        assert_eq!(step(&mut t, 0, 0, 30), SETTLING);
        for _ in 0..SETTLE_FRAMES - 1 {
            assert_eq!(step(&mut t, 0, 0, 30), SETTLING);
        }
        let after = step(&mut t, 0, 0, 30);
        assert!(after != SETTLING && after != before);
        // The scene that was left is the edge's `from`.
        let edge = t.edges().last().copied().expect("an edge");
        assert_eq!((edge.from, edge.to), (before.0, after.0));
    }

    #[test]
    fn a_fade_through_black_is_one_cut_not_many() {
        let mut t = SceneTracker::new();
        let before = hold(&mut t, 0, 0, 1, 20);
        // Black, then the next screen fading in (palette steps).
        for _ in 0..4 {
            assert_eq!(
                t.observe_frame(&uniform(15), WIDTH, HEIGHT, &events_at(0, 0), &[]),
                SETTLING
            );
        }
        let mut last = SETTLING;
        for fade in 0..4u8 {
            last = step(&mut t, 0, 0, 30 + fade);
        }
        assert_eq!(last, SETTLING, "the fade is still moving");
        let after = hold(&mut t, 0, 0, 34, 20);
        assert!(after != SETTLING && after != before);
        assert_eq!(t.edges().len(), 1, "one scene change: {:?}", t.edges());
    }

    // --- The false cut: a fall of 64+ px in a vertical section ----------

    #[test]
    fn a_fall_of_80_px_a_frame_in_a_vertical_section_is_not_a_cut() {
        let mut t = SceneTracker::new();
        let start = hold(&mut t, 0, 0, 1, 10);
        let mut seen = std::collections::HashSet::new();
        for frame in 1..=20i64 {
            // 80 px a frame: far over MAX_CONTINUOUS_STEP_PX, but the picture
            // is the old one moved by exactly the scroll the registers say.
            seen.insert(step(&mut t, 0, frame * 80, 1));
        }
        assert_eq!(seen, std::collections::HashSet::from([start]));
        assert!(t.edges().is_empty());
    }

    #[test]
    fn a_jump_of_80_px_to_another_arrangement_of_the_same_colours_is_a_cut() {
        let mut t = SceneTracker::new();
        let before = hold(&mut t, 0, 0, 1, 10);
        // A door: the camera lands 80 px away and shows another room drawn
        // with the SAME colours (no histogram evidence at all).
        let events = events_at(0, 80);
        for _ in 0..SETTLE_FRAMES + 1 {
            t.observe_frame(&frame_rearranged(0, 80, 1), WIDTH, HEIGHT, &events, &[]);
        }
        let edge = t.edges().last().copied().expect("the door made an edge");
        assert_eq!(edge.from, before.0);
        assert!(!edge.continuous, "a door is not a continuous edge");
    }

    #[test]
    fn the_y_register_wrapping_at_240_is_not_a_jump() {
        // MM2's vertical section: Y runs 0..239 and starts over. Read on the
        // 480 modulus that is 236 px backwards (W33-09).
        let mut t = SceneTracker::new();
        let id = hold(&mut t, 0, 220, 1, 4);
        let mut seen = std::collections::HashSet::new();
        for frame in 0..30i64 {
            let wy = 220 + frame * 4;
            let mut events = vec![scroll(0, 0)];
            events.extend(scanlines(0..HUD_ROWS));
            events.push(scroll(0, (wy % 240) as u16));
            events.extend(scanlines(HUD_ROWS..HEIGHT));
            seen.insert(t.observe_frame(&frame_at(0, wy, 1), WIDTH, HEIGHT, &events, &[]));
        }
        assert_eq!(
            seen,
            std::collections::HashSet::from([id]),
            "{:?}",
            t.edges()
        );
    }

    // --- Mapper bank state (kept) ---------------------------------------

    #[test]
    fn a_mapper_bank_change_alone_is_a_cut_even_with_continuous_scroll() {
        let mut t = SceneTracker::new();
        let mut id = SETTLING;
        for f in 0..10i64 {
            id = t.observe_frame(
                &frame_at(f * 3, 0, 1),
                WIDTH,
                HEIGHT,
                &events_at(f * 3, 0),
                &[1, 2, 3],
            );
        }
        let mut after = SETTLING;
        for f in 10..30i64 {
            after = t.observe_frame(
                &frame_at(f * 3, 0, 1),
                WIDTH,
                HEIGHT,
                &events_at(f * 3, 0),
                &[1, 2, 4],
            );
        }
        assert!(
            after != SETTLING && after != id,
            "bank change must be a scene change"
        );
    }

    // --- Continuity pins the id ------------------------------------------

    #[test]
    fn continuity_reuses_the_pinned_id_without_recomputing_from_content() {
        // The colours of a long scroll drift; the id must not.
        let mut t = SceneTracker::new();
        let first = hold(&mut t, 0, 0, 1, 5);
        let mut last = first;
        for f in 1..120i64 {
            last = step(&mut t, f * 2, 0, 1);
        }
        assert_eq!(first, last);
    }

    // --- The profile's level id ------------------------------------------

    #[test]
    fn a_level_id_names_the_scene_and_a_pipe_room_and_back_returns_the_same_id() {
        let mut t = SceneTracker::new();
        let mut id = SETTLING;
        for _ in 0..SETTLE_FRAMES + 4 {
            id = level_step(&mut t, 0, 0, Some(&[0x05]));
        }
        let level = id;
        assert_eq!(level, level_scene_id(&[0x05]));
        // The pipe: the id changes, the picture is irrelevant.
        let mut room = SETTLING;
        for _ in 0..SETTLE_FRAMES + 8 {
            room = level_step(&mut t, 0, 0, Some(&[0x0A]));
        }
        assert_eq!(room, level_scene_id(&[0x0A]));
        assert_ne!(room, level);
        // And out again: the SAME id, so the same canvases.
        let mut back = SETTLING;
        for _ in 0..SETTLE_FRAMES + 8 {
            back = level_step(&mut t, 0, 0, Some(&[0x05]));
        }
        assert_eq!(back, level);
    }

    #[test]
    fn a_level_id_that_settles_over_several_frames_names_one_scene() {
        // SMB3's pointer word is written over five frames after the level
        // starts: 0x82BB, 0x8EBB, 0xBCBB, 0x0FBC, 0x74BC, 0x92BC.
        let mut t = SceneTracker::new();
        hold(&mut t, 0, 0, 1, 10);
        let steps: [&[u8]; 6] = [
            &[0x82, 0xBB],
            &[0x8E, 0xBB],
            &[0xBC, 0xBB],
            &[0x0F, 0xBC],
            &[0x74, 0xBC],
            &[0x92, 0xBC],
        ];
        for s in steps {
            assert_eq!(level_step(&mut t, 0, 0, Some(s)), SETTLING);
        }
        let mut id = SETTLING;
        for _ in 0..SETTLE_FRAMES + 4 {
            id = level_step(&mut t, 0, 0, Some(&[0x92, 0xBC]));
        }
        assert_eq!(id, level_scene_id(&[0x92, 0xBC]));
        assert_eq!(t.edges().len(), 1, "the intermediate words made no scene");
    }

    #[test]
    fn leaving_a_level_for_the_map_falls_back_to_the_picture() {
        let mut t = SceneTracker::new();
        let mut level = SETTLING;
        for _ in 0..SETTLE_FRAMES + 4 {
            level = level_step(&mut t, 0, 0, Some(&[7]));
        }
        let mut map = SETTLING;
        for _ in 0..SETTLE_FRAMES + 8 {
            map = t.observe(
                &frame_at(0, 0, 30),
                WIDTH,
                HEIGHT,
                &events_at(0, 0),
                &[],
                SceneHints::default(),
            );
        }
        assert!(map != SETTLING && map != level);
        assert_ne!(map, level_scene_id(&[7]));
    }

    #[test]
    fn a_level_key_is_not_cut_by_the_picture_or_the_scroll() {
        // Dying and respawning, a fade inside the level: same key, same scene.
        let mut t = SceneTracker::new();
        let id = (0..SETTLE_FRAMES + 4)
            .map(|_| level_step(&mut t, 0, 0, Some(&[3])))
            .last()
            .unwrap();
        for _ in 0..4 {
            t.observe(
                &uniform(15),
                WIDTH,
                HEIGHT,
                &events_at(0, 0),
                &[],
                SceneHints {
                    level: Some(&[3]),
                    axis_sections: None,
                },
            );
        }
        assert_eq!(level_step(&mut t, 400, 0, Some(&[3])), id);
    }

    // --- Edges ------------------------------------------------------------

    #[test]
    fn a_level_change_the_picture_flows_across_is_a_continuous_edge() {
        // MM2's horizontal section into the vertical one: the key changes
        // while the picture keeps scrolling.
        let mut t = SceneTracker::new();
        for f in 0..20i64 {
            level_step(&mut t, f * 2, 0, Some(&[1]));
        }
        for f in 20..60i64 {
            level_step(&mut t, f * 2, 0, Some(&[2]));
        }
        let edge = t.edges().last().copied().expect("edge");
        assert!(edge.continuous);
        assert_eq!(edge.from_pos.0, 38);
        assert_eq!(
            t.continuous_neighbours(level_scene_id(&[1])).len(),
            1,
            "the neighbour across a continuous edge"
        );
    }

    #[test]
    fn a_pipe_a_door_and_a_fade_are_never_continuous_edges() {
        let mut t = SceneTracker::new();
        for _ in 0..SETTLE_FRAMES + 4 {
            level_step(&mut t, 0, 0, Some(&[1]));
        }
        // The key changes under a black frame (a pipe: fade out, new room).
        for _ in 0..SETTLE_FRAMES + 4 {
            t.observe(
                &uniform(15),
                WIDTH,
                HEIGHT,
                &events_at(0, 0),
                &[],
                SceneHints {
                    level: Some(&[2]),
                    axis_sections: None,
                },
            );
        }
        let edge = t.edges().last().copied().expect("edge");
        assert!(!edge.continuous);
        assert!(t.continuous_neighbours(level_scene_id(&[1])).is_empty());
        assert!(t.continuous_neighbours(level_scene_id(&[2])).is_empty());
    }

    #[test]
    fn an_edge_round_trips_through_its_bytes() {
        let edge = SceneEdge {
            from: 0xDEAD_BEEF_0123_4567,
            to: 42,
            from_pos: (-17, 480),
            to_pos: (0, i64::MIN),
            continuous: true,
        };
        assert_eq!(SceneEdge::from_bytes(&edge.to_bytes()), Some(edge));
        assert_eq!(SceneEdge::from_bytes(&edge.to_bytes()[1..]), None);
        let mut bad = edge.to_bytes();
        bad[SceneEdge::ENCODED_LEN - 1] = 2;
        assert_eq!(SceneEdge::from_bytes(&bad), None);
    }

    // --- Sections: the scroll axis switches --------------------------------

    #[test]
    fn climbing_after_a_long_walk_is_a_new_section_joined_by_a_continuous_edge() {
        // MM2: 2300 px of horizontal section, then the screen climbs.
        let mut t = SceneTracker::new();
        let mut first = SETTLING;
        for f in 0..120i64 {
            first = step(&mut t, f * 4, 0, 1);
        }
        assert_ne!(first, SETTLING);
        let mut last = first;
        for f in 0..60i64 {
            last = step(&mut t, 480, f * 4, 1);
        }
        assert_ne!(last, first, "the vertical section is a scene of its own");
        assert_ne!(last, SETTLING);
        let edge = t.edges().last().copied().expect("edge");
        assert_eq!((edge.from, edge.to), (first.0, last.0));
        assert!(edge.continuous, "the picture flowed on across the switch");
        assert_eq!(t.continuous_neighbours(first).len(), 1);
    }

    #[test]
    fn a_short_climb_after_a_short_walk_is_not_a_section() {
        let mut t = SceneTracker::new();
        let mut first = SETTLING;
        for f in 0..20i64 {
            first = step(&mut t, f * 4, 0, 1);
        }
        let mut last = first;
        for f in 0..30i64 {
            last = step(&mut t, 76, f * 4, 1);
        }
        assert_eq!(first, last);
    }

    #[test]
    fn a_profile_level_key_turns_the_axis_rule_off_unless_asked() {
        let mut t = SceneTracker::new();
        let mut id = SETTLING;
        for f in 0..120i64 {
            id = level_step(&mut t, f * 4, 0, Some(&[1]));
        }
        for f in 0..60i64 {
            assert_eq!(level_step(&mut t, 480, f * 4, Some(&[1])), id);
        }
    }
}
