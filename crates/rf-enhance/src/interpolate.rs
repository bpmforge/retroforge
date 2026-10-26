//! Layer-level frame interpolation (ticket W36-07; `docs/design/FRAME_INTERPOLATION.md` section 8,
//! D-028).
//!
//! An intermediate frame at `alpha` (0..=255, fixed point: 0 is frame N, 256 would be N+1) is built
//! from the LAYERS of two consecutive console frames, never by blending pixels:
//!
//! * the **background** is frame N's background colour under every dot (`bg`, which includes the
//!   dots a sprite covered), shifted by `alpha` of the scroll the PPU actually applied between the
//!   two frames, line by line (so a status bar that does not scroll stays put while the playfield
//!   moves: split scroll is exact because the scroll is read per picture line, not once per
//!   frame). Columns the shift uncovers are taken from frame N+1's background, which is the same
//!   world drawn by the machine;
//! * the **sprites** are frame N's own sprite pixels, moved by `alpha` of the distance the same
//!   object moved by N+1. An object is a group of OAM entries: with a caller-supplied list of
//!   [`Anchor`]s (a recipe's object slots) the entries near one anchor move together by their
//!   median delta, without one each OAM entry moves by its own;
//! * rows the candidacy test excludes (HUD bands, [`crate::interpolation::candidacy`]) are copied
//!   from frame N untouched.
//!
//! Anything that is not smooth motion is refused and the caller repeats frame N: a palette change,
//! a scroll jump, a background that is not the previous one shifted. Nothing here feeds the
//! simulation; it reads two finished frames.
//!
//! **Colours are carried as RGB** (`0x00RRGGBB` keys), not as palette indices (W37-06): the picture
//! may be wider than the console's (widescreen margins have no palette index), and a pair whose
//! palette cycles still draws every dot with the colour it had in the frame it is taken from, which
//! is "frame N's pixels with N's palette" by construction. The NES palette rule is
//! [`palette_verdict`].
//!
//! Law 8: every walk is a `for` over a range or an iterator; the one hand-rolled helper
//! ([`median`]) sorts a slice.

use crate::hud::{HudRegion, HudSource};
use crate::interpolation::{candidacy, Candidacy, FrameContext, Refusal};
use crate::scene_identity::SceneId;

/// Horizontal scroll positions wrap at two nametables.
pub const SCROLL_PERIOD_X: i32 = 512;
/// Vertical scroll positions wrap at two nametables of 240 rows.
pub const SCROLL_PERIOD_Y: i32 = 480;
/// A background scroll step beyond this many dots between consecutive frames is a jump, not motion.
pub const MAX_SCROLL_STEP: i32 = 24;
/// An object that moved more than this many dots in a frame is not the same object.
pub const MAX_SPRITE_STEP: i32 = 32;
/// The share (per mille) of the background that must be frame N shifted for the pair to be one scene.
pub const MIN_BG_MATCH_PERMILLE: u32 = 800;
/// "No sprite won this dot" in [`Layers::ids`].
pub const NO_SPRITE: u8 = 0xFF;
/// OAM entries on the NES.
pub const OAM_ENTRIES: usize = 64;
/// OAM entries on the largest console handled (the SNES): sizes the shared movement helpers.
pub const MAX_ENTRIES: usize = 128;
/// Background entries in the NES palette (`$3F00-$3F0F`); the rest (`$3F10-$3F1F`) are sprite entries.
pub const NES_BG_ENTRIES: usize = 16;
/// Palette entries on the NES.
pub const NES_PALETTE_ENTRIES: usize = 32;
/// A pair whose background palette differs in at most this many entries (the backdrop untouched) is
/// palette cycling (a shimmer, a coin glint), not a fade or a flash: it is interpolated, every dot
/// drawn with the colour it had in the frame it came from.
pub const MAX_CYCLED_ENTRIES: usize = 2;

/// A place the caller knows an object to be on this frame (a recipe's object slot): sprites whose
/// pixels sit within `reach` of it are one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// Picture position of the object's centre.
    pub x: i32,
    pub y: i32,
    /// How far (per axis) a sprite's centre may be from it.
    pub reach_x: i32,
    pub reach_y: i32,
}

/// One console frame, as far as interpolation needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layers {
    pub width: usize,
    pub height: usize,
    /// The frame as the console showed it, `width * height * 4` RGBA.
    pub rgba: Vec<u8>,
    /// Background colour under every dot (`width * height`, `0x00RRGGBB`), sprites ignored.
    pub bg: Vec<u32>,
    /// OAM index that won each dot ([`NO_SPRITE`] for none), `width * height`.
    pub ids: Vec<u8>,
    /// Scroll at the start of each picture line: `(x in 0..512, vertical offset in 0..480)`. The offset
    /// is the nametable row the line shows less the line number, so it is constant across a band.
    pub scroll: Vec<(u16, u16)>,
    /// The colour shown where the background is transparent (`$3F00`), `0x00RRGGBB`.
    pub backdrop: u32,
    /// Per OAM entry: drawn behind opaque background.
    pub behind: [bool; OAM_ENTRIES],
    /// Object places the game's own tables give (empty without a recipe).
    pub anchors: Vec<Anchor>,
    /// Completed-frame count.
    pub index: u64,
    /// A digest of the palette: a change disqualifies the pair ([`FrameContext::palette_digest`]) unless
    /// [`palette_verdict`] finds it to be cycling.
    pub palette_digest: u64,
    /// The 32 NES palette entries as `0x00RRGGBB` (`$3F00-$3F1F`; empty when the caller has none, in
    /// which case any digest change refuses). See [`palette_verdict`].
    pub palette: Vec<u32>,
}

impl Layers {
    /// Whether every buffer has the size the picture says. A frame that fails this is refused rather
    /// than indexed.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let n = self.width * self.height;
        n > 0
            && self.rgba.len() == n * 4
            && self.bg.len() == n
            && self.ids.len() == n
            && self.scroll.len() == self.height
    }
}

/// What a composed frame did, for the readout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Rows that were rebuilt (the rest are frame N's).
    pub rows_built: usize,
    /// OAM entries drawn at a moved position.
    pub sprites_moved: usize,
    /// Of the moved ones, how many took the median movement of an object's entries (a recipe anchor)
    /// rather than their own.
    pub sprites_grouped: usize,
    /// OAM entries drawn where frame N had them (no counterpart in N+1, or too far).
    pub sprites_held: usize,
}

/// Signed shortest distance from `a` to `b` on a wrapping axis of `period`.
fn wrap_delta(a: u16, b: u16, period: i32) -> i32 {
    let d = (i32::from(b) - i32::from(a)).rem_euclid(period);
    if d > period / 2 {
        d - period
    } else {
        d
    }
}

/// `delta * alpha / 256`, rounded to nearest (half away from zero's neighbour: ties go up).
fn scaled(delta: i32, alpha: u8) -> i32 {
    (delta * i32::from(alpha) + 128).div_euclid(256)
}

/// The median of a non-empty list (the upper median for even lengths).
fn median(values: &mut [i32]) -> i32 {
    values.sort_unstable();
    values[values.len() / 2]
}

/// The top and bottom bands whose scroll does not move with the playfield: a leading (trailing) run of
/// lines whose scroll equals the first (last) line's in BOTH frames, when some other line differs from
/// it. That is the signature of a split-scroll status bar, taken from what the PPU did rather than
/// guessed from pixels (`crate::hud`'s own definition). A band is only a HUD when it is the smaller
/// part of the frame (at most half); a frame with no split has none.
#[must_use]
pub fn detect_hud(prev: &Layers, next: &Layers) -> Vec<HudRegion> {
    let h = prev
        .height
        .min(next.height)
        .min(prev.scroll.len())
        .min(next.scroll.len());
    if h < 2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let steady = |y: usize, anchor: usize| {
        prev.scroll[y] == prev.scroll[anchor] && next.scroll[y] == next.scroll[anchor]
    };
    // Top band: the lines from 0 that look like line 0.
    let top = (0..h).take_while(|&y| steady(y, 0)).count();
    if top > 0 && top * 2 <= h {
        out.push(HudRegion {
            start: 0,
            end: top as u16,
            source: HudSource::Heuristic,
        });
    }
    // Bottom band: the lines from the end that look like the last line, if not already the top one.
    let bottom = (0..h).rev().take_while(|&y| steady(y, h - 1)).count();
    if bottom > 0 && bottom * 2 <= h && h - bottom >= top {
        out.push(HudRegion {
            start: (h - bottom) as u16,
            end: h as u16,
            source: HudSource::Heuristic,
        });
    }
    out
}

/// `(min_x, min_y, max_x, max_y, count)` of one OAM entry's dots.
type DotBox = (i32, i32, i32, i32, u32);

/// Bounding box and dot count of each OAM entry's dots in a `width`-wide picture of per-dot ids.
fn boxes(ids: &[u8], width: usize) -> [Option<DotBox>; MAX_ENTRIES] {
    let mut out: [Option<DotBox>; MAX_ENTRIES] = [None; MAX_ENTRIES];
    let width = width.max(1);
    for (i, &id) in ids.iter().enumerate() {
        let Some(slot) = out.get_mut(usize::from(id)) else {
            continue;
        };
        let (x, y) = ((i % width) as i32, (i / width) as i32);
        *slot = Some(match *slot {
            None => (x, y, x, y, 1),
            Some((x0, y0, x1, y1, n)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y), n + 1),
        });
    }
    out
}

/// The per-OAM-entry movement N to N+1 from the per-dot ids of the two pictures: `Some((dx, dy))` when
/// the entry is drawn in both with a box of the same size and it moved a believable distance, `None`
/// otherwise. Shared by the NES and SNES composers.
#[must_use]
pub fn entry_deltas(
    prev_ids: &[u8],
    next_ids: &[u8],
    width: usize,
) -> [Option<(i32, i32)>; MAX_ENTRIES] {
    let (a, b) = (boxes(prev_ids, width), boxes(next_ids, width));
    let mut out = [None; MAX_ENTRIES];
    for (slot, (pa, pb)) in out.iter_mut().zip(a.iter().zip(b.iter())) {
        if let (Some((x0, y0, x1, y1, _)), Some((u0, v0, u1, v1, _))) = (pa, pb) {
            let same_size = x1 - x0 == u1 - u0 && y1 - y0 == v1 - v0;
            let (dx, dy) = (u0 - x0, v0 - y0);
            if same_size && dx.abs() <= MAX_SPRITE_STEP && dy.abs() <= MAX_SPRITE_STEP {
                *slot = Some((dx, dy));
            }
        }
    }
    out
}

/// The movement each OAM entry of `prev` is drawn with, after grouping entries near one anchor.
fn group_deltas(
    prev: &Layers,
    next: &Layers,
) -> ([Option<(i32, i32)>; MAX_ENTRIES], [bool; MAX_ENTRIES]) {
    let own = entry_deltas(&prev.ids, &next.ids, prev.width);
    let mut grouped = [false; MAX_ENTRIES];
    if prev.anchors.is_empty() {
        return (own, grouped);
    }
    let boxes_n = boxes(&prev.ids, prev.width);
    // Which anchor owns each entry: the nearest whose reach contains the entry's centre.
    let mut owner = [None::<usize>; MAX_ENTRIES];
    for (slot, b) in owner.iter_mut().zip(boxes_n.iter()) {
        let Some((x0, y0, x1, y1, _)) = b else {
            continue;
        };
        let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
        *slot = prev
            .anchors
            .iter()
            .enumerate()
            .filter(|(_, a)| (cx - a.x).abs() <= a.reach_x && (cy - a.y).abs() <= a.reach_y)
            .min_by_key(|(_, a)| (cx - a.x).abs() + (cy - a.y).abs())
            .map(|(k, _)| k);
    }
    let mut out = own;
    for k in 0..prev.anchors.len() {
        let members = || {
            owner
                .iter()
                .enumerate()
                .filter(move |(_, o)| **o == Some(k))
                .map(|(i, _)| i)
        };
        let mut dx: Vec<i32> = members().filter_map(|i| own[i]).map(|d| d.0).collect();
        let mut dy: Vec<i32> = members().filter_map(|i| own[i]).map(|d| d.1).collect();
        if dx.is_empty() {
            // Nothing of this object is in N+1: it is held where it was, as a whole.
            for i in members() {
                out[i] = None;
            }
            continue;
        }
        let shared = (median(&mut dx), median(&mut dy));
        for i in members() {
            out[i] = Some(shared);
            grouped[i] = true;
        }
    }
    (out, grouped)
}

/// Embed the layers of a console frame in the wider picture widescreen made of it (W37-06).
///
/// `wide_rgba` is the picture as shown (`wide_width * rows * 4`): the console's own dots in the middle
/// (`pad = (wide_width - core.width) / 2` columns in, widescreen keeps them) and the margins on either
/// side from the scout's canvas or the level image, with the margin enemy strips already drawn in
/// (W36-06b). The result describes THAT picture, so [`compose`] works on it unchanged:
///
/// * the margin dots are background: the same per-line scroll that moves the console's dots moves them,
///   so scenery stays where the world is. Objects drawn into the margin (the enemy strips) are part of
///   that background, so they move with the scroll too: an object standing in the world is exact, one
///   walking drifts from its true place by its own speed times `alpha` (under a dot at the speeds
///   these games use) until the next console frame redraws it;
/// * the console's dots keep their sprites (moved by their own OAM deltas as before); a console dot
///   that no sprite won takes its background colour from the picture as shown, so a dot widescreen
///   repainted (a wrapped copy of an edge object) is the picture's own;
/// * recipe anchors move with the picture's centre.
///
/// `None` when the picture does not fit: narrower than the console's, or the wrong number of bytes.
#[must_use]
pub fn widen(core: &Layers, wide_rgba: &[u8], wide_width: usize, rows: usize) -> Option<Layers> {
    let (cw, rows) = (core.width, rows.min(core.height));
    if wide_width < cw
        || rows == 0
        || wide_rgba.len() != wide_width * rows * 4
        || !core.is_well_formed()
    {
        return None;
    }
    let pad = (wide_width - cw) / 2;
    let n = wide_width * rows;
    let mut bg = vec![0u32; n];
    let mut ids = vec![NO_SPRITE; n];
    for y in 0..rows {
        for x in 0..wide_width {
            let at = y * wide_width + x;
            let shown = rgb_key([
                wide_rgba[at * 4],
                wide_rgba[at * 4 + 1],
                wide_rgba[at * 4 + 2],
            ]);
            bg[at] = shown;
            if x >= pad && x - pad < cw {
                let c = y * cw + (x - pad);
                ids[at] = core.ids[c];
                if core.ids[c] != NO_SPRITE {
                    bg[at] = core.bg[c]; // the background under a sprite is the console's
                }
            }
        }
    }
    Some(Layers {
        width: wide_width,
        height: rows,
        rgba: wide_rgba.to_vec(),
        bg,
        ids,
        scroll: core.scroll[..rows].to_vec(),
        backdrop: core.backdrop,
        behind: core.behind,
        anchors: core
            .anchors
            .iter()
            .map(|a| Anchor {
                x: a.x + pad as i32,
                ..*a
            })
            .collect(),
        index: core.index,
        palette_digest: core.palette_digest,
        palette: core.palette.clone(),
    })
}

/// Whether the layers describe the picture they came with: the background colour under every dot no
/// sprite won must be the colour the console frame shows there. A frame captured part-way (a state just
/// loaded, a rendering toggle) or drawn through a colour effect the layers cannot see (greyscale,
/// emphasis, a mid-frame palette fade) fails it, and is refused rather than rebuilt wrongly.
#[must_use]
pub fn describes_its_picture(l: &Layers) -> bool {
    let (mut total, mut bad) = (0usize, 0usize);
    for ((&key, &id), px) in l.bg.iter().zip(&l.ids).zip(l.rgba.chunks_exact(4)) {
        if id != NO_SPRITE {
            continue;
        }
        total += 1;
        if px[..3] != rgb_of(key) {
            bad += 1;
        }
    }
    bad * 200 <= total
}

/// `0x00RRGGBB` to its bytes.
#[must_use]
pub fn rgb_of(key: u32) -> [u8; 3] {
    [(key >> 16) as u8, (key >> 8) as u8, key as u8]
}

/// `[r, g, b]` to `0x00RRGGBB`.
#[must_use]
pub fn rgb_key(rgb: [u8; 3]) -> u32 {
    (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
}

/// What a change of palette between two frames is (`FRAME_INTERPOLATION.md` section 3.4, as built in
/// W37-06).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteVerdict {
    /// Nothing the picture is made of changed colour.
    Same,
    /// A few background entries cycled (or only sprite entries changed): interpolate. The listed
    /// colours are the ones the cycling moved between, so a dot that differs between the frames only
    /// by being one of them is the same dot (see [`compose_verified`]'s background-fit test).
    Cycling(Vec<u32>),
    /// A fade, a flash, a new palette: refuse.
    Changed,
}

/// Decide whether the palette change between two frames is cycling or a real change.
///
/// Cycling is the SMB question-block shimmer: one or two background entries step through colours while
/// everything else stays. It is allowed when at most [`MAX_CYCLED_ENTRIES`] of the 15 background entries
/// differ and neither backdrop entry (`$3F00`, mirrored at `$3F10`) does. The sprite entries (`$3F11-$3F1F`)
/// are free to change: sprites are drawn from frame N's own pixels, so a star-power cycle never shows a
/// colour of N+1 on an N sprite. A palette that is not 32 entries long cannot be judged: any digest change
/// is `Changed`.
#[must_use]
pub fn palette_verdict(prev: &Layers, next: &Layers) -> PaletteVerdict {
    if prev.palette_digest == next.palette_digest {
        return PaletteVerdict::Same;
    }
    let (a, b) = (&prev.palette, &next.palette);
    if a.len() != NES_PALETTE_ENTRIES || b.len() != NES_PALETTE_ENTRIES {
        return PaletteVerdict::Changed;
    }
    let differs = |i: usize| a[i] != b[i];
    if differs(0) || differs(NES_BG_ENTRIES) {
        return PaletteVerdict::Changed;
    }
    let bg: Vec<usize> = (1..NES_BG_ENTRIES).filter(|&i| differs(i)).collect();
    if bg.len() > MAX_CYCLED_ENTRIES {
        return PaletteVerdict::Changed;
    }
    let mut colours: Vec<u32> = bg.iter().flat_map(|&i| [a[i], b[i]]).collect();
    colours.sort_unstable();
    colours.dedup();
    PaletteVerdict::Cycling(colours)
}

/// Compose the intermediate frame `alpha`/256 of the way from `prev` to `next` into `out`
/// (`width * height * 4` RGBA).
///
/// `alpha == 0` is `prev.rgba` exactly. On `Err` nothing useful is in `out` and the caller repeats
/// `prev`.
///
/// # Errors
/// [`Refusal`] when the pair must not be interpolated: not consecutive, palette changed, a layer that
/// does not describe a picture, a scroll or object jump, or a background that is not frame N shifted.
pub fn compose(
    prev: &Layers,
    next: &Layers,
    alpha: u8,
    out: &mut Vec<u8>,
) -> Result<Stats, Refusal> {
    if !prev.is_well_formed() || !next.is_well_formed() {
        return Err(Refusal::Malformed);
    }
    if !describes_its_picture(prev) || !describes_its_picture(next) {
        return Err(Refusal::Malformed);
    }
    compose_verified(prev, next, alpha, out)
}

/// [`compose`] for layers the caller has already checked with [`describes_its_picture`] (once per console
/// frame rather than once per composed picture, which is the cost that matters at 120 Hz and above).
///
/// # Errors
/// As [`compose`].
pub fn compose_verified(
    prev: &Layers,
    next: &Layers,
    alpha: u8,
    out: &mut Vec<u8>,
) -> Result<Stats, Refusal> {
    if !prev.is_well_formed()
        || !next.is_well_formed()
        || prev.width != next.width
        || prev.height != next.height
    {
        return Err(Refusal::Malformed);
    }
    let (w, h) = (prev.width, prev.height);
    // A palette that cycles (a few background entries, never the backdrop) is not a palette change.
    let verdict = palette_verdict(prev, next);
    let excused: &[u32] = match &verdict {
        PaletteVerdict::Cycling(colours) => colours,
        _ => &[],
    };
    let cycling = !matches!(verdict, PaletteVerdict::Changed);
    let ctx = |l: &Layers| FrameContext {
        index: l.index,
        // The NES edge signature is flat (scene_identity module doc), so scene identity is not a
        // usable cut detector here; the background-fit test below is.
        scene: SceneId(0),
        palette_digest: if cycling {
            prev.palette_digest
        } else {
            l.palette_digest
        },
    };
    let hud = detect_hud(prev, next);
    let rows = match candidacy(ctx(prev), ctx(next), &hud, h as u16) {
        Candidacy::Refused(r) => return Err(r),
        Candidacy::Allowed { scanlines } => scanlines,
    };
    if rows.is_empty() {
        return Err(Refusal::HudOnly);
    }
    out.clear();
    out.extend_from_slice(&prev.rgba);
    if alpha == 0 {
        return Ok(Stats::default());
    }

    // Per-line scroll step; a jump anywhere in the permitted rows refuses the pair.
    let mut step = vec![(0i32, 0i32); h];
    let mut allowed = vec![false; h];
    for &(a, b) in &rows {
        for y in usize::from(a)..usize::from(b).min(h) {
            allowed[y] = true;
            let (p, n) = (prev.scroll[y], next.scroll[y]);
            let d = (
                wrap_delta(p.0, n.0, SCROLL_PERIOD_X),
                wrap_delta(p.1, n.1, SCROLL_PERIOD_Y),
            );
            if d.0.abs() > MAX_SCROLL_STEP || d.1.abs() > MAX_SCROLL_STEP {
                return Err(Refusal::LargeMotion);
            }
            step[y] = d;
        }
    }

    // Is the background of N+1 the background of N shifted? (Counts only dots whose source is in N.)
    let (mut seen, mut same) = (0u32, 0u32);
    for y in 0..h {
        if !allowed[y] {
            continue;
        }
        let (dx, dy) = step[y];
        let sy = y as i32 + dy;
        if !(0..h as i32).contains(&sy) {
            continue;
        }
        for x in 0..w {
            let sx = x as i32 + dx;
            if !(0..w as i32).contains(&sx) {
                continue;
            }
            seen += 1;
            let (a, b) = (prev.bg[sy as usize * w + sx as usize], next.bg[y * w + x]);
            // A dot that only changed between two colours the palette cycles through is the same dot.
            if a == b || (excused.contains(&a) && excused.contains(&b)) {
                same += 1;
            }
        }
    }
    if seen > 0 && same * 1000 < seen * MIN_BG_MATCH_PERMILLE {
        return Err(Refusal::Discontinuity);
    }

    // The background layer, row by row.
    let mut stats = Stats::default();
    let mut bg_row = vec![0u32; w * h]; // colours actually shown, for the behind-background test
    for y in 0..h {
        if !allowed[y] {
            continue;
        }
        stats.rows_built += 1;
        let (dx, dy) = step[y];
        let (sx, sy) = (scaled(dx, alpha), scaled(dy, alpha));
        for x in 0..w {
            // The world position is `x + sx` in N's picture; if that falls off N's picture it is
            // `x + sx - dx` in N+1's, where the same world is drawn.
            let (px, py) = (x as i32 + sx, y as i32 + sy);
            let from_prev = (0..w as i32).contains(&px) && (0..h as i32).contains(&py);
            let (qx, qy) = (px - dx, py - dy);
            let key = if from_prev {
                prev.bg[py as usize * w + px as usize]
            } else if (0..w as i32).contains(&qx) && (0..h as i32).contains(&qy) {
                next.bg[qy as usize * w + qx as usize]
            } else {
                prev.bg[y * w + x]
            };
            bg_row[y * w + x] = key;
            let at = (y * w + x) * 4;
            out[at..at + 3].copy_from_slice(&rgb_of(key));
            out[at + 3] = 0xFF;
        }
    }

    // The sprites: frame N's own pixels at their moved places, lowest OAM index on top.
    let (moves, grouped) = group_deltas(prev, next);
    let mut claimed = vec![NO_SPRITE; w * h];
    let mut drawn = [false; OAM_ENTRIES];
    for (i, &id) in prev.ids.iter().enumerate() {
        let entry = usize::from(id);
        if entry >= OAM_ENTRIES {
            continue;
        }
        let (x, y) = ((i % w) as i32, i / w);
        if !allowed[y] {
            continue; // part of a held band: already in `out` from frame N
        }
        let (mx, my) = moves[entry].map_or((0, 0), |d| (scaled(d.0, alpha), scaled(d.1, alpha)));
        let (tx, ty) = (x + mx, y as i32 + my);
        if !(0..w as i32).contains(&tx) || !(0..h as i32).contains(&ty) {
            continue;
        }
        let t = ty as usize * w + tx as usize;
        if !allowed[ty as usize] {
            continue;
        }
        // Behind-background art is only seen where the background is the backdrop.
        if prev.behind[entry] && bg_row[t] != prev.backdrop {
            continue;
        }
        if claimed[t] <= id {
            continue;
        }
        claimed[t] = id;
        drawn[entry] = true;
        out[t * 4..t * 4 + 4].copy_from_slice(&prev.rgba[i * 4..i * 4 + 4]);
    }
    for (entry, was_drawn) in drawn.iter().enumerate() {
        if *was_drawn {
            if moves[entry].is_some() {
                stats.sprites_moved += 1;
                stats.sprites_grouped += usize::from(grouped[entry]);
            } else {
                stats.sprites_held += 1;
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 16;
    const H: usize = 8;

    fn resolve(i: u8) -> [u8; 3] {
        [i, i.wrapping_mul(3), 7]
    }

    /// A frame whose background is a ramp in x (so a shift is visible) and whose scroll is `sx` on
    /// every line; `sprite` is `(oam, x, y)` of a 2x2 sprite of colour 200.
    fn frame(index: u64, sx: u16, sprite: Option<(u8, usize, usize)>) -> Layers {
        let mut bg = vec![0u32; W * H];
        for y in 0..H {
            for x in 0..W {
                // World column `sx + x`: the picture is a window onto a fixed world.
                bg[y * W + x] = rgb_key(resolve(((usize::from(sx) + x) % 100 + 10) as u8));
            }
        }
        let mut ids = vec![NO_SPRITE; W * H];
        let mut rgba = vec![0u8; W * H * 4];
        for i in 0..W * H {
            rgba[i * 4..i * 4 + 3].copy_from_slice(&rgb_of(bg[i]));
            rgba[i * 4 + 3] = 255;
        }
        if let Some((id, sx_, sy_)) = sprite {
            for y in sy_..sy_ + 2 {
                for x in sx_..sx_ + 2 {
                    ids[y * W + x] = id;
                    rgba[(y * W + x) * 4..(y * W + x) * 4 + 4].copy_from_slice(&[200, 1, 2, 255]);
                }
            }
        }
        Layers {
            width: W,
            height: H,
            rgba,
            bg,
            ids,
            scroll: vec![(sx, 0); H],
            backdrop: 0,
            behind: [false; OAM_ENTRIES],
            anchors: Vec::new(),
            index,
            palette_digest: 1,
            palette: Vec::new(),
        }
    }

    fn run(p: &Layers, n: &Layers, alpha: u8) -> Result<(Vec<u8>, Stats), Refusal> {
        let mut out = Vec::new();
        compose(p, n, alpha, &mut out).map(|s| (out, s))
    }

    /// t = 0 is frame N, byte for byte, whatever happened next.
    #[test]
    fn alpha_zero_is_frame_n_exactly() {
        let (p, n) = (frame(1, 4, Some((3, 2, 2))), frame(2, 6, Some((3, 4, 2))));
        let (out, _) = run(&p, &n, 0).expect("composes");
        assert_eq!(out, p.rgba);
    }

    /// With nothing moving, every alpha reproduces frame N (anti-vacuity for the rest: the layers
    /// rebuild N's own picture).
    #[test]
    fn a_still_pair_reproduces_the_frame_at_every_alpha() {
        let (p, n) = (frame(1, 4, Some((3, 2, 2))), frame(2, 4, Some((3, 2, 2))));
        for alpha in [1u8, 64, 128, 255] {
            let (out, _) = run(&p, &n, alpha).expect("composes");
            assert_eq!(out, p.rgba, "alpha {alpha}");
        }
    }

    /// Half a 4-dot scroll moves the background two dots, and the dots it uncovers on the right come
    /// from frame N+1 (the same world), not from a blend.
    #[test]
    fn the_background_shifts_by_the_fractional_scroll() {
        let (p, n) = (frame(1, 10, None), frame(2, 14, None));
        let (out, _) = run(&p, &n, 128).expect("composes");
        for x in 0..W {
            // World column 10 + 2 + x at the midpoint: the exact picture of scroll 12.
            let want = resolve(((12 + x) % 100 + 10) as u8);
            assert_eq!(&out[x * 4..x * 4 + 3], &want, "x {x}");
        }
    }

    /// A sprite moves by the same fraction, drawn with frame N's pixels.
    #[test]
    fn a_sprite_moves_by_the_fractional_delta() {
        let (p, n) = (frame(1, 4, Some((3, 2, 2))), frame(2, 4, Some((3, 6, 2))));
        let (out, s) = run(&p, &n, 128).expect("composes");
        let px = |x: usize, y: usize| out[(y * W + x) * 4..(y * W + x) * 4 + 3].to_vec();
        assert_eq!(px(4, 2), vec![200, 1, 2], "moved two dots");
        assert_eq!(px(5, 3), vec![200, 1, 2]);
        assert_ne!(
            px(2, 2),
            vec![200, 1, 2],
            "the old place shows the background again"
        );
        assert_eq!((s.sprites_moved, s.sprites_held), (1, 0));
    }

    /// Two entries near one anchor move together by their median, even when one of them is missing
    /// from N+1 (clipped): the object is one thing.
    #[test]
    fn a_sprite_group_moves_as_one() {
        let mut p = frame(1, 4, Some((3, 2, 2)));
        let mut n = frame(2, 4, Some((3, 6, 2)));
        // A second entry of the same object, present only in N.
        for y in 4..6 {
            for x in 2..4 {
                p.ids[y * W + x] = 4;
                p.rgba[(y * W + x) * 4..(y * W + x) * 4 + 4].copy_from_slice(&[201, 1, 2, 255]);
            }
        }
        p.anchors.push(Anchor {
            x: 3,
            y: 4,
            reach_x: 4,
            reach_y: 4,
        });
        n.anchors = p.anchors.clone();
        let (out, s) = run(&p, &n, 128).expect("composes");
        let px = |x: usize, y: usize| out[(y * W + x) * 4..(y * W + x) * 4 + 3].to_vec();
        assert_eq!(px(4, 2), vec![200, 1, 2]);
        assert_eq!(
            px(4, 4),
            vec![201, 1, 2],
            "the unmatched entry moved with its object"
        );
        assert_eq!((s.sprites_moved, s.sprites_grouped), (2, 2));
    }

    /// An entry that has no counterpart in N+1 and no anchor stays where it was.
    #[test]
    fn an_unmatched_sprite_is_held_where_it_was() {
        let (p, n) = (frame(1, 4, Some((3, 2, 2))), frame(2, 4, None));
        let (out, s) = run(&p, &n, 128).expect("composes");
        assert_eq!(&out[(2 * W + 2) * 4..(2 * W + 2) * 4 + 3], &[200, 1, 2]);
        assert_eq!((s.sprites_moved, s.sprites_held), (0, 1));
    }

    /// A status bar over a scrolling playfield: the bar's rows are frame N's, the playfield's move.
    #[test]
    fn hud_rows_are_untouched() {
        let mut p = frame(1, 10, Some((3, 2, 6)));
        let mut n = frame(2, 14, Some((3, 6, 6)));
        // Rows 0..2 do not scroll in either frame; the sprite in row 6 is playfield.
        for l in [&mut p, &mut n] {
            for y in 0..2 {
                l.scroll[y] = (0, 0);
            }
        }
        let hud = detect_hud(&p, &n);
        assert_eq!(hud.len(), 1);
        assert_eq!((hud[0].start, hud[0].end), (0, 2));
        let (out, _) = run(&p, &n, 128).expect("composes");
        assert_eq!(
            &out[..2 * W * 4],
            &p.rgba[..2 * W * 4],
            "HUD rows are frame N's"
        );
        assert_ne!(&out[3 * W * 4..4 * W * 4], &p.rgba[3 * W * 4..4 * W * 4]);
    }

    #[test]
    fn a_jump_a_palette_change_and_a_cut_are_refused() {
        let p = frame(1, 4, None);
        // A scroll jump.
        assert_eq!(
            run(&p, &frame(2, 100, None), 128).err(),
            Some(Refusal::LargeMotion)
        );
        // A palette change.
        let mut pal = frame(2, 6, None);
        pal.palette_digest = 2;
        assert_eq!(run(&p, &pal, 128).err(), Some(Refusal::PaletteChanged));
        // A cut: the same small scroll step, a different picture.
        let mut cut = frame(2, 6, None);
        for (i, b) in cut.bg.iter_mut().enumerate() {
            *b = b.wrapping_add(0x0032_1932);
            cut.rgba[i * 4..i * 4 + 3].copy_from_slice(&rgb_of(*b));
        }
        assert_eq!(run(&p, &cut, 128).err(), Some(Refusal::Discontinuity));
        // Not consecutive.
        assert_eq!(
            run(&p, &frame(5, 6, None), 128).err(),
            Some(Refusal::NotConsecutive)
        );
    }

    /// Degenerate inputs terminate and refuse: zero size, mismatched buffers, a frame that is all HUD,
    /// wrapped and huge scroll deltas.
    #[test]
    fn degenerate_inputs_terminate() {
        let mut empty = frame(1, 0, None);
        empty.width = 0;
        empty.rgba.clear();
        empty.bg.clear();
        empty.ids.clear();
        assert_eq!(run(&empty, &empty, 128).err(), Some(Refusal::Malformed));
        let mut short = frame(1, 0, None);
        short.bg.pop();
        assert_eq!(
            run(&short, &frame(2, 0, None), 128).err(),
            Some(Refusal::Malformed)
        );

        // Every line the same scroll: no split, so no HUD, and a still pair composes.
        let (p, n) = (frame(1, 0, None), frame(2, 0, None));
        assert!(detect_hud(&p, &n).is_empty());
        // Scroll values at the ends of their ranges wrap to a small step, not a huge one.
        let (mut a, mut b) = (frame(1, 0, None), frame(2, 0, None));
        a.scroll = vec![(511, 479); H];
        b.scroll = vec![(1, 1); H];
        assert_eq!(wrap_delta(511, 1, SCROLL_PERIOD_X), 2);
        assert_eq!(wrap_delta(479, 1, SCROLL_PERIOD_Y), 2);
        let _ = run(&a, &b, 255); // terminates; the answer (refused or not) is not the point
                                  // Maximum u16 scroll values.
        a.scroll = vec![(u16::MAX, u16::MAX); H];
        b.scroll = vec![(0, 0); H];
        let _ = run(&a, &b, 255);
        // Every alpha on a moving pair terminates.
        let (p, n) = (frame(1, 10, Some((3, 2, 2))), frame(2, 13, Some((3, 5, 3))));
        for alpha in 0..=255u8 {
            assert!(run(&p, &n, alpha).is_ok(), "alpha {alpha}");
        }
    }

    /// A 32-entry NES palette (distinct colours) for the palette rule tests.
    fn pal() -> Vec<u32> {
        (0..32u32).map(|i| 0x0010_2030 + i * 0x0001_0101).collect()
    }

    /// `frame` with a palette whose `changes` entries are replaced, digest following the change, and every
    /// dot of the left half of the picture painted with the colour of entry `paint` (to make a cycling
    /// area).
    fn with_palette(mut l: Layers, changes: &[(usize, u32)], paint: Option<usize>) -> Layers {
        let mut p = pal();
        for &(i, c) in changes {
            p[i] = c;
        }
        l.palette_digest = p
            .iter()
            .fold(7u64, |h, &c| h.wrapping_mul(31) ^ u64::from(c));
        if let Some(e) = paint {
            for i in 0..W * H {
                if i % W < W / 2 {
                    l.bg[i] = p[e];
                    l.rgba[i * 4..i * 4 + 3].copy_from_slice(&rgb_of(p[e]));
                }
            }
        }
        l.palette = p;
        l
    }

    /// W37-06: one background entry cycling (the question-block shimmer) is interpolated, and the dots
    /// of the cycled colour are drawn with frame N's colour, not N+1's and not a blend.
    #[test]
    fn a_cycling_palette_entry_is_interpolated_with_n_s_palette() {
        let p = with_palette(frame(1, 4, None), &[], Some(13));
        let n = with_palette(frame(2, 6, None), &[(13, 0x00AA_5500)], Some(13));
        assert_eq!(
            palette_verdict(&p, &n),
            PaletteVerdict::Cycling(vec![pal()[13], 0x00AA_5500])
        );
        let (out, _) = run(&p, &n, 128).expect("cycling is not a palette change");
        // Half of the picture is the cycled colour; at alpha 128 the scroll is 1 dot, so the dot at x = 2
        // (world column 3 of the left half, painted) shows N's colour for entry 13.
        let at = |x: usize| rgb_key([out[x * 4], out[x * 4 + 1], out[x * 4 + 2]]);
        assert_eq!(at(2), pal()[13], "frame N's palette");
        assert_ne!(at(2), 0x00AA_5500);
    }

    /// A cycled colour covering half the picture still passes the background-fit test (the dots that only
    /// changed between the cycled colours are the same dots), where the same change as a plain recolour does
    /// not.
    #[test]
    fn a_large_cycled_area_is_excused_but_a_recolour_is_not() {
        let p = with_palette(frame(1, 4, None), &[], Some(13));
        let cycled = with_palette(frame(2, 4, None), &[(13, 0x00AA_5500)], Some(13));
        assert!(run(&p, &cycled, 128).is_ok());
        // Same picture change but the palette says nothing cycled (digest differs, no entry listed): refused.
        let mut recoloured = cycled.clone();
        recoloured.palette.clear();
        assert_eq!(
            run(&p, &recoloured, 128).err(),
            Some(Refusal::PaletteChanged)
        );
    }

    /// Fades and flashes stay refused: more than two background entries, or either backdrop entry.
    #[test]
    fn fades_and_backdrop_changes_are_still_refused() {
        let p = with_palette(frame(1, 4, None), &[], None);
        let fade = with_palette(frame(2, 6, None), &[(1, 1), (2, 2), (3, 3)], None);
        assert_eq!(run(&p, &fade, 128).err(), Some(Refusal::PaletteChanged));
        let flash = with_palette(frame(2, 6, None), &[(0, 0x00FF_FFFF)], None);
        assert_eq!(run(&p, &flash, 128).err(), Some(Refusal::PaletteChanged));
        let mirror = with_palette(frame(2, 6, None), &[(16, 0x00FF_FFFF)], None);
        assert_eq!(run(&p, &mirror, 128).err(), Some(Refusal::PaletteChanged));
        // Exactly two cycling entries is the limit.
        let two = with_palette(frame(2, 6, None), &[(5, 1), (6, 2)], None);
        assert!(run(&p, &two, 128).is_ok());
    }

    /// Sprite-palette changes alone (star power) do not refuse: sprites are drawn from N's pixels.
    #[test]
    fn sprite_palette_changes_alone_are_not_refused() {
        let p = with_palette(frame(1, 4, Some((3, 2, 2))), &[], None);
        let n = with_palette(
            frame(2, 6, Some((3, 4, 2))),
            &[(17, 1), (18, 2), (19, 3), (21, 4), (22, 5), (23, 6)],
            None,
        );
        assert!(matches!(palette_verdict(&p, &n), PaletteVerdict::Cycling(c) if c.is_empty()));
        assert!(run(&p, &n, 128).is_ok());
    }

    /// The wide picture a widescreen shell makes of `core`: the same world, `pad` more columns each side.
    fn wide_of(core: &Layers, pad: usize) -> Vec<u8> {
        let sx = usize::from(core.scroll[0].0);
        let ww = W + 2 * pad;
        let mut rgba = vec![0u8; ww * H * 4];
        for y in 0..H {
            for x in 0..ww {
                let col = (sx + x + 100 - pad) % 100 + 10;
                let at = (y * ww + x) * 4;
                rgba[at..at + 3].copy_from_slice(&resolve(col as u8));
                rgba[at + 3] = 255;
            }
        }
        // The console's own dots (sprite included) are kept as they were.
        for y in 0..H {
            let dst = (y * ww + pad) * 4;
            rgba[dst..dst + W * 4].copy_from_slice(&core.rgba[y * W * 4..(y + 1) * W * 4]);
        }
        rgba
    }

    /// W37-06: with widescreen on, the margins move with the same scroll delta as the console's dots, so the
    /// midpoint is the exact wide picture of the middle scroll, margins included; the sprite still moves by
    /// its own delta.
    #[test]
    fn widescreen_margins_move_with_the_scroll_delta() {
        let pad = 4;
        let (p, n) = (
            frame(1, 10, Some((3, 6, 2))),
            frame(2, 14, Some((3, 10, 2))),
        );
        let (wp, wn) = (
            widen(&p, &wide_of(&p, pad), W + 2 * pad, H).expect("fits"),
            widen(&n, &wide_of(&n, pad), W + 2 * pad, H).expect("fits"),
        );
        assert_eq!((wp.width, wp.height), (W + 2 * pad, H));
        assert!(describes_its_picture(&wp));
        let mut out = Vec::new();
        let stats = compose(&wp, &wn, 128, &mut out).expect("composes");
        assert_eq!(stats.sprites_moved, 1);
        let ww = W + 2 * pad;
        for x in 0..ww {
            if (8 + pad..10 + pad).contains(&x) {
                continue; // the sprite's own dots
            }
            let want = resolve(((12 + x + 100 - pad) % 100 + 10) as u8);
            assert_eq!(&out[(3 * ww + x) * 4..(3 * ww + x) * 4 + 3], &want, "x {x}");
        }
        // Frame N's sprite sits at x 6..8 of the console picture; halfway to 10 it covers 8..10.
        assert_eq!(
            &out[(2 * ww + pad + 8) * 4..(2 * ww + pad + 8) * 4 + 3],
            &[200, 1, 2]
        );
        // alpha 0 is the wide frame N exactly.
        compose(&wp, &wn, 0, &mut out).expect("composes");
        assert_eq!(out, wp.rgba);
    }

    #[test]
    fn widen_refuses_pictures_that_do_not_fit() {
        let p = frame(1, 10, None);
        assert!(widen(&p, &wide_of(&p, 2), W + 3, H).is_none(), "bytes");
        assert!(
            widen(&p, &vec![0; (W - 2) * H * 4], W - 2, H).is_none(),
            "narrower"
        );
        assert!(widen(&p, &[], W, 0).is_none(), "no rows");
        // Exactly the console's width is the identity.
        let same = widen(&p, &p.rgba, W, H).expect("fits");
        assert_eq!(same.bg, p.bg);
    }

    /// A frame whose background layer is not the picture it came with (captured part-way) is refused.
    #[test]
    fn layers_that_do_not_describe_their_picture_are_refused() {
        let p = frame(1, 4, None);
        let mut n = frame(2, 4, None);
        for b in &mut n.bg[..W * 4] {
            *b = b.wrapping_add(1);
        }
        assert_eq!(run(&p, &n, 128).err(), Some(Refusal::Malformed));
    }

    #[test]
    fn a_frame_that_is_all_hud_permits_nothing() {
        // Scroll differs only on the last line, so the whole frame but it is the bottom band... make
        // the whole frame one band: top run covers everything but nothing else differs -> no HUD.
        let (p, n) = (frame(1, 4, None), frame(2, 4, None));
        let hud = [HudRegion {
            start: 0,
            end: H as u16,
            source: HudSource::Profile,
        }];
        let c = candidacy(
            FrameContext {
                index: 1,
                scene: SceneId(0),
                palette_digest: 1,
            },
            FrameContext {
                index: 2,
                scene: SceneId(0),
                palette_digest: 1,
            },
            &hud,
            H as u16,
        );
        assert_eq!(c, Candidacy::Allowed { scanlines: vec![] });
        let _ = (p, n);
    }

    #[test]
    fn median_is_robust() {
        assert_eq!(median(&mut [3, 3, 100]), 3);
        assert_eq!(median(&mut [5]), 5);
    }
}
