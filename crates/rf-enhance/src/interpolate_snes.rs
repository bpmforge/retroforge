//! Layer-level frame interpolation for the SNES (ticket W37-06; `docs/design/FRAME_INTERPOLATION.md`
//! section 9, D-028). The NES composer ([`crate::interpolate`]) shifts one background; a SNES picture is up
//! to four scrolling backgrounds and sprites interleaved by priority, so this one moves each LAYER by its
//! own scroll and puts the result back together in the mode's own priority order.
//!
//! **What is known per dot** is what the PPU sent the sink, nothing more: the colour shown, which layer
//! drew it (a background `0..=3`, a sprite, or the backdrop) at what priority, and which OAM entry (for a
//! sprite). **What is known per line** is the scroll of every background, the main-screen designation,
//! the brightness, the backdrop colour and the mode. Nothing about what a front layer or a sprite HID is
//! captured, so a layer is moved by gathering: the dot at `(x, y)` in the intermediate shows layer `L` if
//! frame N's dot at `(x + sx_L, y + sy_L)` was layer `L` (`s_L` is `alpha` of `L`'s own per-line scroll
//! step to N+1), else if frame N+1's dot at the same world position was; the front-most such layer wins
//! by the mode's priority order. A dot nothing claims (a front layer or a sprite moved off what only it
//! covered in both frames) takes N+1's picture there, then N's, then the backdrop: the one place the
//! result is a guess about what was hidden, and it is the sliver a sprite or a layer edge moved over.
//!
//! The colour of a dot is the colour the console showed at the dot it came from, so brightness, colour
//! math and the backdrop gradient travel with their layer; a pair whose colour changes (brightness or
//! backdrop differing, a layer's dots not matching their shifted selves) is refused, not blended.
//!
//! **A HUD is fixed by construction**: a layer whose scroll does not change between the frames (BG3's
//! status bar, an IRQ-split band whose scroll register is rewritten to the same value) is gathered from
//! the same dot, so it does not move. [`SnesStats::fixed_rows`] counts the rows where every layer stood
//! still while others moved.
//!
//! Refusals (the pair is repeated; the caller shows frame N), each from the line facts the PPU latched:
//! mode 7, hires/pseudo-hires, mosaic, a window hiding part of a main-screen layer, interlace,
//! force-blank, direct colour, a mid-line register write. A line with one of those is held (copied from
//! N) when the lines that are not are the majority; otherwise the pair is refused with that reason.
//! Also refused: not consecutive, brightness or backdrop colour changed (a fade, a flash), a layer's
//! scroll jumping further than motion does, and layers that are not their shifted selves (a cut).
//!
//! Law 8: every walk is a `for` over a range or an iterator.

use crate::interpolate::{entry_deltas, MAX_ENTRIES, MAX_SCROLL_STEP, NO_SPRITE};
use crate::interpolation::Refusal;

/// BG scroll registers are 10 bits: positions wrap at 1024.
pub const SCROLL_PERIOD: i32 = 1024;
/// The share (per mille) of the dots of each layer that must be the layer's frame N dots shifted.
pub const MIN_LAYER_MATCH_PERMILLE: u32 = 800;
/// A layer with fewer visible dots than this in a frame is too small to judge a cut by.
pub const MIN_LAYER_DOTS: u32 = 64;

/// [`SnesLayers::codes`] layer for a sprite dot.
pub const CODE_SPRITE: u8 = 4;
/// [`SnesLayers::codes`] layer for a backdrop dot.
pub const CODE_BACKDROP: u8 = 5;
/// [`SnesLayers::codes`] layer for a widescreen margin dot (see [`SnesLayers::widened`]).
pub const CODE_MARGIN: u8 = 6;
/// [`SnesLayers::codes`] flag (bit 5): the dot is the main screen's backdrop with the sub-screen
/// background named in bits 0-2 added by colour math (Super Mario World's far hills): it moves with that
/// background, behind every main-screen layer.
pub const CODE_SUB: u8 = 0x20;
/// Where a [`CODE_SUB`] dot sits in the order: behind every main-screen slot of any mode.
pub const RANK_SUB: u8 = 99;

/// Line hazard bits, as `rf_snes::ppu::line_hazard` (mirrored here because this crate does not depend on
/// the core; a test in `retroforge` checks the two agree).
pub mod hazard {
    pub const MODE7: u8 = 1;
    pub const HIRES: u8 = 2;
    pub const MOSAIC: u8 = 4;
    pub const WINDOW: u8 = 8;
    pub const INTERLACE: u8 = 16;
    pub const MIDLINE: u8 = 32;
    pub const FORCED_BLANK: u8 = 64;
    pub const DIRECT_COLOR: u8 = 128;
}

/// What is known about one picture line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SnesLine {
    /// BG1-BG4 scroll as latched for the line (10-bit register values).
    pub hofs: [u16; 4],
    pub vofs: [u16; 4],
    /// `$212C`: backgrounds (bits 0-3) and OBJ (bit 4) on the main screen.
    pub tm: u8,
    /// Master brightness `0..=15` (0 when blanked).
    pub brightness: u8,
    /// CGRAM entry 0 as the line was drawn (BGR555).
    pub backdrop: u16,
    /// Index into [`SnesLayers::ranks`]: `bg_mode | (bg3_priority << 3)`.
    pub mode_key: u8,
    /// [`hazard`] bits.
    pub hazards: u8,
}

/// One console frame as far as SNES interpolation needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnesLayers {
    pub width: usize,
    pub height: usize,
    /// The frame as shown, `width * height * 4` RGBA.
    pub rgba: Vec<u8>,
    /// Per dot: layer in bits 0-2 ([`CODE_SPRITE`], [`CODE_BACKDROP`], [`CODE_MARGIN`], else a background
    /// `0..=3`), the pixel's priority in bits 3-4, [`CODE_SUB`] in bit 5 (`rf_renderer::frame::layer_code`).
    pub codes: Vec<u8>,
    /// Per dot: the OAM entry that won it ([`NO_SPRITE`] for none).
    pub ids: Vec<u8>,
    /// Per picture line.
    pub lines: Vec<SnesLine>,
    /// Per mode key: `ranks[key][layer][priority]` is how far back that (layer, priority) is in the
    /// mode's order, 0 frontmost (layer 4 is OBJ); [`RANK_ABSENT`] where the mode has no such slot.
    pub ranks: Vec<[[u8; 4]; 5]>,
    /// Completed-frame count.
    pub index: u64,
    /// W39-01: what the game's tick hook made of this frame's tick ([`crate::extrapolation::Tracker`]);
    /// `None` when no recipe supplies the facts, which keeps the delayed interpolation.
    pub tick: Option<crate::extrapolation::Verdict>,
}

/// A (layer, priority) the mode's order does not contain.
pub const RANK_ABSENT: u8 = 100;

impl SnesLayers {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let n = self.width * self.height;
        n > 0
            && self.rgba.len() == n * 4
            && self.codes.len() == n
            && self.ids.len() == n
            && self.lines.len() == self.height
            && self.ranks.len() >= 16
    }

    /// Embed the layers of a console frame in the wider picture widescreen made of it: the console's 256
    /// dots in the middle (`pad = (wide_width - width) / 2` columns in), the margins on either side as
    /// [`CODE_MARGIN`] dots, which are gathered along with the layer that has the most visible dots (the
    /// scenery the margins show is that layer's world). `None` when the picture does not fit.
    #[must_use]
    pub fn widened(&self, wide_rgba: &[u8], wide_width: usize, rows: usize) -> Option<Self> {
        let (cw, rows) = (self.width, rows.min(self.height));
        if wide_width < cw
            || rows == 0
            || wide_rgba.len() != wide_width * rows * 4
            || !self.is_well_formed()
        {
            return None;
        }
        let pad = (wide_width - cw) / 2;
        let n = wide_width * rows;
        let mut codes = vec![CODE_MARGIN; n];
        let mut ids = vec![NO_SPRITE; n];
        let mut rgba = wide_rgba.to_vec();
        for y in 0..rows {
            let core = &self.codes[y * cw..(y + 1) * cw];
            let at = y * wide_width + pad;
            codes[at..at + cw].copy_from_slice(core);
            ids[at..at + cw].copy_from_slice(&self.ids[y * cw..(y + 1) * cw]);
            // The picture as shown is the truth for the console's own dots too.
            rgba[at * 4..(at + cw) * 4].copy_from_slice(&self.rgba[y * cw * 4..(y + 1) * cw * 4]);
        }
        Some(Self {
            width: wide_width,
            height: rows,
            rgba,
            codes,
            ids,
            lines: self.lines[..rows].to_vec(),
            ranks: self.ranks.clone(),
            index: self.index,
            tick: self.tick.clone(),
        })
    }
}

/// What a composed SNES frame did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnesStats {
    /// Rows that were rebuilt (the rest were held).
    pub rows_built: usize,
    /// Rows held from frame N because a hazard made them unmovable.
    pub rows_held: usize,
    /// Rows where every layer stood still while another row moved (a HUD band or a fixed layer).
    pub fixed_rows: usize,
    /// OAM entries drawn at a moved position.
    pub sprites_moved: usize,
    /// OAM entries drawn where frame N had them (no counterpart in N+1, or too far).
    pub sprites_held: usize,
    /// Dots no layer claimed (taken from the neighbouring frame or the backdrop).
    pub unclaimed: usize,
    /// Backgrounds that moved, as a bit set (bit `n` for BG`n+1`).
    pub layers_moved: u8,
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

/// `delta * alpha / 256`, rounded to nearest.
fn scaled(delta: i32, alpha: i32) -> i32 {
    (delta * alpha + 128).div_euclid(256)
}

fn layer_of(code: u8) -> u8 {
    code & 7
}

fn priority_of(code: u8) -> usize {
    usize::from((code >> 3) & 3)
}

/// The first hazard bit set in `bits`, as the refusal that names it.
fn refusal_of(bits: u8) -> Refusal {
    if bits & hazard::MODE7 != 0 {
        Refusal::Mode7
    } else if bits & hazard::HIRES != 0 {
        Refusal::Hires
    } else if bits & hazard::MOSAIC != 0 {
        Refusal::Mosaic
    } else if bits & hazard::WINDOW != 0 {
        Refusal::Window
    } else if bits & hazard::INTERLACE != 0 {
        Refusal::Interlace
    } else if bits & hazard::MIDLINE != 0 {
        Refusal::MidLine
    } else {
        Refusal::Blanked
    }
}

/// How many visible dots of each background are in row `y` of `l`.
fn visible_per_layer(l: &SnesLayers, y: usize) -> [u32; 4] {
    let mut n = [0u32; 4];
    for &c in &l.codes[y * l.width..(y + 1) * l.width] {
        if let Some(slot) = n.get_mut(usize::from(layer_of(c))) {
            *slot += 1;
        }
    }
    n
}

/// Compose the intermediate frame `alpha`/256 of the way from `prev` to `next` into `out`
/// (`width * height * 4` RGBA). `alpha == 0` is `prev.rgba` exactly. On `Err` nothing useful is in `out`
/// and the caller repeats `prev`.
///
/// # Errors
/// [`Refusal`] when the pair must not be interpolated; see the module doc for the list.
pub fn compose_snes(
    prev: &SnesLayers,
    next: &SnesLayers,
    alpha: u8,
    out: &mut Vec<u8>,
) -> Result<SnesStats, Refusal> {
    if next.index != prev.index.wrapping_add(1) {
        return check_pair(prev, next, Err(Refusal::NotConsecutive));
    }
    compose_between(prev, next, i32::from(alpha), out)
}

/// W39-01: the frame `alpha`/256 of one step PAST `cur`, in the direction `cur` moved since `older`
/// (the frame before it), built from `cur`'s own pixels: the frame is `cur` moved forward, so nothing
/// waits for a later frame and no latency is added. `alpha == 0` is `cur.rgba` exactly.
///
/// It is [`compose_snes`] run backwards: `cur` is the base, `older` plays the "other" frame, so every
/// per-layer and per-entry step it measures is `older - cur` (minus the real velocity) and a negative
/// alpha turns it into `+alpha` of the real velocity. What it cannot know (a dot that scrolls in from
/// beyond both frames' edges) falls back as it does for interpolation: the frame's own dot.
///
/// # Errors
/// [`Refusal`] as for [`compose_snes`]; `older` must be the frame immediately before `cur`.
pub fn extrapolate_snes(
    older: &SnesLayers,
    cur: &SnesLayers,
    alpha: u8,
    out: &mut Vec<u8>,
) -> Result<SnesStats, Refusal> {
    if cur.index != older.index.wrapping_add(1) {
        return check_pair(cur, older, Err(Refusal::NotConsecutive));
    }
    compose_between(cur, older, -i32::from(alpha), out)
}

/// The shape check that outranks "not consecutive": a malformed pair is `Malformed` either way.
fn check_pair(
    a: &SnesLayers,
    b: &SnesLayers,
    otherwise: Result<SnesStats, Refusal>,
) -> Result<SnesStats, Refusal> {
    if !a.is_well_formed() || !b.is_well_formed() || a.width != b.width || a.height != b.height {
        return Err(Refusal::Malformed);
    }
    otherwise
}

/// The composer: `prev` is the base the output is gathered from, `next` the other frame, `alpha` how
/// far (in 256ths, signed) along `next - prev` the output sits. The caller has checked the indices.
fn compose_between(
    prev: &SnesLayers,
    next: &SnesLayers,
    alpha: i32,
    out: &mut Vec<u8>,
) -> Result<SnesStats, Refusal> {
    if !prev.is_well_formed()
        || !next.is_well_formed()
        || prev.width != next.width
        || prev.height != next.height
    {
        return Err(Refusal::Malformed);
    }
    let (w, h) = (prev.width, prev.height);

    // Colour changes between the frames are not motion.
    let mut backdrop_changed = 0usize;
    for (a, b) in prev.lines.iter().zip(&next.lines) {
        if a.brightness != b.brightness {
            return Err(Refusal::Fade);
        }
        backdrop_changed += usize::from(a.backdrop != b.backdrop);
    }
    if backdrop_changed * 8 > h {
        return Err(Refusal::Fade);
    }

    // Rows a hazard makes unmovable are held; too many and the pair is refused with the commonest reason.
    let mut allowed = vec![true; h];
    let mut all_bits = 0u8;
    let mut held = 0usize;
    for (y, (a, b)) in prev.lines.iter().zip(&next.lines).enumerate() {
        let bits = a.hazards | b.hazards;
        if bits != 0 {
            allowed[y] = false;
            all_bits |= bits;
            held += 1;
        }
    }
    if held * 2 > h {
        return Err(refusal_of(all_bits));
    }
    if held == h {
        return Err(Refusal::HudOnly);
    }

    out.clear();
    out.extend_from_slice(&prev.rgba);
    if alpha == 0 {
        return Ok(SnesStats::default());
    }

    // The background the margin dots (widescreen) belong to: the one most visible in frame N.
    let mut totals = [0u32; 4];
    for y in 0..h {
        for (t, n) in totals.iter_mut().zip(visible_per_layer(prev, y)) {
            *t += n;
        }
    }
    let primary = totals
        .iter()
        .enumerate()
        .filter(|(_, &n)| n > 0)
        .max_by_key(|(i, &n)| (n, usize::MAX - i))
        .map(|(i, _)| i as u8);

    // Per row, per layer: the scroll step N to N+1 (zero for a layer neither frame shows on the row).
    let mut step = vec![[(0i32, 0i32); 4]; h];
    let mut active = vec![0u8; h];
    let mut any_motion = vec![false; h];
    for y in (0..h).filter(|&y| allowed[y]) {
        let (a, b) = (prev.lines[y], next.lines[y]);
        let (va, vb) = (visible_per_layer(prev, y), visible_per_layer(next, y));
        for l in 0..4usize {
            if va[l] + vb[l] == 0 {
                continue;
            }
            let d = (
                wrap_delta(a.hofs[l], b.hofs[l], SCROLL_PERIOD),
                wrap_delta(a.vofs[l], b.vofs[l], SCROLL_PERIOD),
            );
            if d.0.abs() > MAX_SCROLL_STEP || d.1.abs() > MAX_SCROLL_STEP {
                return Err(Refusal::LargeMotion);
            }
            step[y][l] = d;
            active[y] |= 1 << l;
            any_motion[y] |= d != (0, 0);
        }
    }
    let moving_rows = any_motion.iter().filter(|&&m| m).count();

    // The step a dot of a given layer code moves by on row `y` (a margin dot moves with the primary).
    let step_of = |y: usize, layer: u8| -> Option<(i32, i32)> {
        let l = if layer == CODE_MARGIN {
            primary?
        } else {
            layer
        };
        (l < 4 && active[y] & (1 << l) != 0).then(|| step[y][usize::from(l)])
    };
    // Whether a source dot's layer is `want` (the primary layer also owns the margin dots).
    let owns = |code: u8, want: u8| {
        let l = layer_of(code);
        l == want || (l == CODE_MARGIN && primary == Some(want))
    };
    let rank = |l: &SnesLayers, y: usize, code: u8| -> u8 {
        if code & CODE_SUB != 0 {
            return RANK_SUB;
        }
        let key = usize::from(l.lines[y].mode_key & 15);
        let layer = layer_of(code);
        let layer = if layer == CODE_MARGIN {
            primary.unwrap_or(0)
        } else {
            layer
        };
        l.ranks
            .get(key)
            .and_then(|t| t.get(usize::from(layer)))
            .map_or(RANK_ABSENT, |p| p[priority_of(code)])
    };

    // Is N+1 the same scene as N? Each layer's dots in N+1 against the dots they came from in N.
    let (mut seen, mut matched, mut same) = (0u32, 0u32, 0u32);
    for y in (0..h).filter(|&y| allowed[y]) {
        for x in 0..w {
            let at = y * w + x;
            let code = next.codes[at];
            let layer = layer_of(code);
            if layer >= CODE_SPRITE && layer != CODE_MARGIN {
                continue;
            }
            seen += 1;
            let Some((dx, dy)) = step_of(y, layer) else {
                continue;
            };
            let (px, py) = (x as i32 + dx, y as i32 + dy);
            if !(0..w as i32).contains(&px) || !(0..h as i32).contains(&py) {
                continue;
            }
            let src = py as usize * w + px as usize;
            let want = if layer == CODE_MARGIN {
                primary.unwrap_or(0)
            } else {
                layer
            };
            if owns(prev.codes[src], want) {
                matched += 1;
                same += u32::from(prev.rgba[src * 4..src * 4 + 3] == next.rgba[at * 4..at * 4 + 3]);
            }
        }
    }
    if seen >= MIN_LAYER_DOTS
        && (matched * 2 < seen || same * 1000 < matched * MIN_LAYER_MATCH_PERMILLE)
    {
        return Err(Refusal::Discontinuity);
    }

    // The sprites: frame N's own pixels at their moved places, kept by priority then lower OAM index.
    let moves = entry_deltas(&prev.ids, &next.ids, w);
    let mut sp_rank = vec![u8::MAX; w * h];
    let mut sp_id = vec![NO_SPRITE; w * h];
    let mut sp_src = vec![0u32; w * h];
    let mut drawn = [false; MAX_ENTRIES];
    for (i, &id) in prev.ids.iter().enumerate() {
        let entry = usize::from(id);
        if entry >= MAX_ENTRIES || layer_of(prev.codes[i]) != CODE_SPRITE {
            continue;
        }
        let (x, y) = ((i % w) as i32, i / w);
        if !allowed[y] {
            continue;
        }
        let (mx, my) = moves[entry].map_or((0, 0), |d| (scaled(d.0, alpha), scaled(d.1, alpha)));
        let (tx, ty) = (x + mx, y as i32 + my);
        if !(0..w as i32).contains(&tx) || !(0..h as i32).contains(&ty) || !allowed[ty as usize] {
            continue;
        }
        let t = ty as usize * w + tx as usize;
        let r = rank(prev, y, prev.codes[i]);
        if (r, id) < (sp_rank[t], sp_id[t]) {
            sp_rank[t] = r;
            sp_id[t] = id;
            sp_src[t] = i as u32;
            drawn[entry] = true;
        }
    }

    // The row's backdrop colour, for dots nothing claims.
    let row_backdrop = |y: usize| -> Option<[u8; 3]> {
        [prev, next].iter().find_map(|l| {
            l.codes[y * w..(y + 1) * w]
                .iter()
                .position(|&c| layer_of(c) == CODE_BACKDROP)
                .map(|x| {
                    let at = (y * w + x) * 4;
                    [l.rgba[at], l.rgba[at + 1], l.rgba[at + 2]]
                })
        })
    };

    let mut stats = SnesStats {
        rows_held: held,
        fixed_rows: if moving_rows > 0 {
            (0..h).filter(|&y| allowed[y] && !any_motion[y]).count()
        } else {
            0
        },
        ..SnesStats::default()
    };
    for y in (0..h).filter(|&y| allowed[y]) {
        stats.rows_built += 1;
        let mut backdrop: Option<Option<[u8; 3]>> = None;
        let layers_here: Vec<u8> = (0..4u8).filter(|l| active[y] & (1 << l) != 0).collect();
        for x in 0..w {
            let t = y * w + x;
            // The front-most claim: (rank, colour source frame, dot).
            let mut best: Option<(u8, bool, usize)> = None;
            if sp_rank[t] != u8::MAX {
                best = Some((sp_rank[t], false, sp_src[t] as usize));
            }
            for &l in &layers_here {
                let (dx, dy) = step[y][usize::from(l)];
                let (sx, sy) = (scaled(dx, alpha), scaled(dy, alpha));
                // The world position is (x + sx, y + sy) in N; off N's picture it is that less the step in N+1.
                let (px, py) = (x as i32 + sx, y as i32 + sy);
                let mut found = None;
                if (0..w as i32).contains(&px) && (0..h as i32).contains(&py) {
                    let s = py as usize * w + px as usize;
                    if owns(prev.codes[s], l) {
                        found = Some((rank(prev, py as usize, prev.codes[s]), false, s));
                    }
                }
                if found.is_none() {
                    let (qx, qy) = (px - dx, py - dy);
                    if (0..w as i32).contains(&qx) && (0..h as i32).contains(&qy) {
                        let s = qy as usize * w + qx as usize;
                        if owns(next.codes[s], l) {
                            found = Some((rank(next, qy as usize, next.codes[s]), true, s));
                        }
                    }
                }
                if let Some(f) = found {
                    if best.is_none_or(|b| f.0 < b.0) {
                        best = Some(f);
                    }
                }
            }
            let at = t * 4;
            match best {
                Some((_, from_next, s)) => {
                    let src = if from_next { &next.rgba } else { &prev.rgba };
                    out[at..at + 3].copy_from_slice(&src[s * 4..s * 4 + 3]);
                }
                None => {
                    // Nothing claims the dot. A backdrop dot stays backdrop; otherwise what the
                    // neighbouring frame shows there, then this one's, then the row's backdrop.
                    let is = |l: &SnesLayers, code_layer: u8| layer_of(l.codes[t]) == code_layer;
                    if is(prev, CODE_BACKDROP) {
                        // already N's pixel
                    } else {
                        stats.unclaimed += 1;
                        let ok = |l: &SnesLayers| {
                            !matches!(layer_of(l.codes[t]), CODE_SPRITE | CODE_BACKDROP)
                        };
                        if ok(next) {
                            out[at..at + 3].copy_from_slice(&next.rgba[at..at + 3]);
                        } else if ok(prev) {
                            // already N's pixel
                        } else if let Some(rgb) = *backdrop.get_or_insert_with(|| row_backdrop(y)) {
                            out[at..at + 3].copy_from_slice(&rgb);
                        }
                    }
                }
            }
        }
        for (l, d) in step[y].iter().enumerate() {
            if *d != (0, 0) {
                stats.layers_moved |= 1 << l;
            }
        }
    }
    for (entry, was_drawn) in drawn.iter().enumerate() {
        if *was_drawn {
            if moves[entry].is_some() {
                stats.sprites_moved += 1;
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

    const W: usize = 24;
    const H: usize = 8;
    /// Mode 1 with BG3 priority: BG3 high tiles, OBJ3, BG1 p1, BG2 p1, OBJ2, BG1 p0, BG2 p0, OBJ1, OBJ0, BG3 p0.
    const KEY: u8 = 9;

    fn ranks() -> Vec<[[u8; 4]; 5]> {
        let mut t = vec![[[RANK_ABSENT; 4]; 5]; 16];
        let order: [(usize, usize); 10] = [
            (2, 1),
            (4, 3),
            (0, 1),
            (1, 1),
            (4, 2),
            (0, 0),
            (1, 0),
            (4, 1),
            (4, 0),
            (2, 0),
        ];
        for (rank, (layer, pri)) in order.iter().enumerate() {
            t[usize::from(KEY)][*layer][*pri] = rank as u8;
        }
        t
    }

    fn colour(layer: u8, col: usize) -> [u8; 3] {
        [layer * 40 + 10, (col % 97) as u8 + 1, 9]
    }

    /// A scene: BG2 everywhere (the back), BG1 a front comb on rows 2..6, a BG3 bar on rows 0..2 that never
    /// scrolls, backdrop where nothing is (none here), and a 2x2 sprite `(id, x, y)` of priority level 2.
    fn scene(index: u64, s1: u16, s2: u16, sprite: Option<(u8, usize, usize)>) -> SnesLayers {
        let r = ranks();
        let mut rgba = vec![0u8; W * H * 4];
        let mut codes = vec![0u8; W * H];
        let mut ids = vec![NO_SPRITE; W * H];
        for y in 0..H {
            for x in 0..W {
                let (layer, pri, col) = if y < 2 {
                    (2u8, 1u8, x)
                } else if (2..6).contains(&y) && (usize::from(s1) + x) % 8 < 4 {
                    (0, 1, usize::from(s1) + x)
                } else {
                    (1, 0, usize::from(s2) + x)
                };
                let at = y * W + x;
                codes[at] = layer | (pri << 3);
                rgba[at * 4..at * 4 + 3].copy_from_slice(&colour(layer, col));
                rgba[at * 4 + 3] = 255;
            }
        }
        if let Some((id, sx, sy)) = sprite {
            for y in sy..sy + 2 {
                for x in sx..sx + 2 {
                    let at = y * W + x;
                    codes[at] = CODE_SPRITE | (2 << 3);
                    ids[at] = id;
                    rgba[at * 4..at * 4 + 4].copy_from_slice(&[200, 1, 2, 255]);
                }
            }
        }
        let line = SnesLine {
            hofs: [s1, s2, 0, 0],
            vofs: [0; 4],
            tm: 0x17,
            brightness: 15,
            backdrop: 0,
            mode_key: KEY,
            hazards: 0,
        };
        SnesLayers {
            width: W,
            height: H,
            rgba,
            codes,
            ids,
            lines: vec![line; H],
            ranks: r,
            index,
            tick: None,
        }
    }

    fn run(p: &SnesLayers, n: &SnesLayers, alpha: u8) -> Result<(Vec<u8>, SnesStats), Refusal> {
        let mut out = Vec::new();
        compose_snes(p, n, alpha, &mut out).map(|s| (out, s))
    }

    fn px(out: &[u8], x: usize, y: usize) -> [u8; 3] {
        let at = (y * W + x) * 4;
        [out[at], out[at + 1], out[at + 2]]
    }

    /// W39-01: extrapolating from the last two frames agrees, away from the edge the scroll uncovers, with
    /// interpolating towards the frame that would have followed (a steady run: BG1 4, BG2 2, sprite 4).
    #[test]
    fn extrapolation_continues_a_steady_run_like_the_next_frame_would() {
        let older = scene(1, 8, 20, Some((3, 5, 6)));
        let cur = scene(2, 12, 22, Some((3, 9, 6)));
        let next = scene(3, 16, 24, Some((3, 13, 6)));
        for alpha in [1u8, 64, 128, 200, 255] {
            let mut ex = Vec::new();
            let stats = extrapolate_snes(&older, &cur, alpha, &mut ex).expect("extrapolates");
            let (want, _) = run(&cur, &next, alpha).expect("composes");
            let differ = |rows: &dyn Fn(usize) -> bool| {
                (0..H)
                    .filter(|&y| rows(y))
                    .flat_map(|y| (0..W - 6).map(move |x| (x, y)))
                    .filter(|&(x, y)| px(&ex, x, y) != px(&want, x, y))
                    .count()
            };
            // Rows with no front layer moving over another (the bar, the back layer, the sprite): exact.
            assert_eq!(differ(&|y| !(2..6).contains(&y)), 0, "alpha {alpha}");
            // The comb rows: what the moving front layer UNCOVERS was hidden in the current frame and
            // cannot be known (the next frame would show it); only a sliver at its edges may differ.
            assert!(differ(&|y| (2..6).contains(&y)) <= 16, "alpha {alpha}");
            assert!(stats.sprites_moved >= 1, "the sprite moves on");
        }
        // alpha 0 is the current frame, not the older one.
        let mut ex = Vec::new();
        extrapolate_snes(&older, &cur, 0, &mut ex).expect("composes");
        assert_eq!(ex, cur.rgba);
    }

    /// The sprite is drawn AHEAD of the current frame's position: the first extrapolated frame already
    /// shows where input just sent it, which the delayed interpolation does not.
    #[test]
    fn the_extrapolated_sprite_is_ahead_of_the_current_frame() {
        let older = scene(1, 8, 20, Some((3, 5, 6)));
        let cur = scene(2, 8, 20, Some((3, 9, 6)));
        let mut ex = Vec::new();
        extrapolate_snes(&older, &cur, 128, &mut ex).expect("extrapolates");
        assert_eq!(px(&ex, 11, 6), [200, 1, 2], "half a step (2 dots) past x 9");
        assert_ne!(px(&ex, 9, 6), [200, 1, 2]);
        // Interpolating older -> cur at the same alpha is BEHIND the current frame.
        let (mid, _) = run(&older, &cur, 128).expect("composes");
        assert_eq!(px(&mid, 7, 6), [200, 1, 2]);
        assert_ne!(px(&mid, 11, 6), [200, 1, 2]);
    }

    #[test]
    fn extrapolation_refuses_what_interpolation_refuses_and_a_wrong_order() {
        let older = scene(1, 8, 20, None);
        let cur = scene(2, 12, 22, None);
        let mut out = Vec::new();
        assert_eq!(
            extrapolate_snes(&cur, &older, 64, &mut out).err(),
            Some(Refusal::NotConsecutive)
        );
        let jump = scene(2, 8 + 100, 20, None);
        assert_eq!(
            extrapolate_snes(&older, &jump, 64, &mut out).err(),
            Some(Refusal::LargeMotion)
        );
        let mut bad = scene(2, 12, 22, None);
        bad.rgba.pop();
        assert_eq!(
            extrapolate_snes(&older, &bad, 64, &mut out).err(),
            Some(Refusal::Malformed)
        );
    }

    /// Every alpha, both orders, odd pairs: terminates.
    #[test]
    fn extrapolation_terminates_for_every_alpha() {
        let (a, b) = (scene(1, 0, 0, Some((0, 0, 0))), scene(2, 1023, 1020, None));
        for alpha in 0..=255u8 {
            let mut out = Vec::new();
            let _ = extrapolate_snes(&a, &b, alpha, &mut out);
            let _ = extrapolate_snes(&b, &a, alpha, &mut out);
        }
    }

    #[test]
    fn alpha_zero_and_a_still_pair_reproduce_frame_n() {
        let (p, n) = (
            scene(1, 8, 20, Some((3, 5, 3))),
            scene(2, 12, 22, Some((3, 9, 3))),
        );
        assert_eq!(run(&p, &n, 0).expect("composes").0, p.rgba);
        let (p, n) = (
            scene(1, 8, 20, Some((3, 5, 3))),
            scene(2, 8, 20, Some((3, 5, 3))),
        );
        for alpha in [1u8, 64, 128, 255] {
            let (out, stats) = run(&p, &n, alpha).expect("composes");
            assert_eq!(out, p.rgba, "alpha {alpha}");
            assert_eq!(stats.unclaimed, 0);
        }
    }

    /// Each layer moves by its OWN scroll: BG1 four dots, BG2 two, at the midpoint two and one; the BG3 bar
    /// does not move at all.
    #[test]
    fn each_layer_shifts_by_its_own_scroll_and_the_hud_layer_stays() {
        let (p, n) = (scene(1, 8, 20, None), scene(2, 12, 22, None));
        let (out, stats) = run(&p, &n, 128).expect("composes");
        let mid = scene(0, 10, 21, None);
        // Where the comb is visible in both the intermediate's truth and frame N, the shifted dot is exact.
        let mut checked = (0, 0, 0);
        for y in 0..H {
            for x in 0..W {
                let want = px(&mid.rgba, x, y);
                let got = px(&out, x, y);
                match layer_of(mid.codes[y * W + x]) {
                    2 => {
                        checked.0 += 1;
                        assert_eq!(got, want, "HUD dot ({x},{y})");
                        assert_eq!(got, px(&p.rgba, x, y), "the bar did not move");
                    }
                    0 => {
                        checked.1 += 1;
                        assert_eq!(got, want, "front layer dot ({x},{y})");
                    }
                    _ => {
                        // BG2 dots not hidden by the comb in frame N at the source dot.
                        if x + 1 < W && layer_of(p.codes[y * W + x + 1]) == 1 {
                            checked.2 += 1;
                            assert_eq!(got, want, "back layer dot ({x},{y})");
                        }
                    }
                }
            }
        }
        assert!(
            checked.0 > 20 && checked.1 > 10 && checked.2 > 10,
            "{checked:?}"
        );
        assert_eq!(stats.layers_moved & 0b11, 0b11);
        assert_eq!(
            stats.fixed_rows, 2,
            "the bar's two rows show only BG3, which stood still"
        );
    }

    /// Super Mario World's far hills: BG2 is on the SUB screen and colour math adds it to the main screen's
    /// backdrop, so the sink sees backdrop dots. Recorded as BG2 with [`CODE_SUB`], they move with BG2's own
    /// scroll, behind everything on the main screen.
    #[test]
    fn a_sub_screen_layer_added_to_the_backdrop_moves_with_its_own_scroll() {
        let sub = |mut l: SnesLayers| {
            for c in &mut l.codes {
                if layer_of(*c) == 1 {
                    *c = 1 | CODE_SUB;
                }
            }
            l
        };
        let (p, n) = (sub(scene(1, 8, 20, None)), sub(scene(2, 12, 22, None)));
        let mid = scene(0, 10, 21, None);
        let (out, stats) = run(&p, &n, 128).expect("composes");
        assert_eq!(
            stats.layers_moved & 0b10,
            0b10,
            "BG2 moved though it is on no main screen row"
        );
        let mut checked = 0;
        for y in 0..H {
            for x in 0..W - 1 {
                // BG2 dots that are not hidden by the front comb at the source in frame N.
                if layer_of(mid.codes[y * W + x]) == 1 && layer_of(p.codes[y * W + x + 1]) == 1 {
                    checked += 1;
                    assert_eq!(
                        px(&out, x, y),
                        px(&mid.rgba, x, y),
                        "sub-screen dot ({x},{y})"
                    );
                }
            }
        }
        assert!(checked > 20);
        // A sub-screen dot is behind a main-screen sprite.
        let (p, n) = (
            sub(scene(1, 0, 0, Some((3, 5, 3)))),
            sub(scene(2, 0, 0, Some((3, 9, 3)))),
        );
        let (out, _) = run(&p, &n, 128).expect("composes");
        assert_eq!(px(&out, 7, 4), [200, 1, 2]);
    }

    /// An IRQ-split status band: rows whose every layer keeps its scroll are fixed while the rest move, and
    /// are counted.
    #[test]
    fn a_split_band_whose_scroll_does_not_change_stays_put() {
        let (mut p, mut n) = (scene(1, 8, 20, None), scene(2, 12, 22, None));
        for l in [&mut p, &mut n] {
            for y in 0..2 {
                l.lines[y].hofs = [0; 4];
            }
        }
        let (out, stats) = run(&p, &n, 128).expect("composes");
        assert_eq!(&out[..2 * W * 4], &p.rgba[..2 * W * 4], "the band is N's");
        assert_eq!(stats.fixed_rows, 2);
    }

    /// A sprite moves by the fractional delta and draws over the layers by its level; behind a front-layer dot
    /// it is hidden.
    #[test]
    fn a_sprite_moves_and_obeys_the_mode_s_priority_order() {
        // Row 3 has the BG1 comb (dots 0..4 of every 8 are BG1, priority 1, in front of OBJ level 2? No:
        // BG1 p1 is rank 2 and OBJ2 rank 4, so the comb is in front of the sprite).
        let (p, n) = (
            scene(1, 0, 0, Some((3, 5, 3))),
            scene(2, 0, 0, Some((3, 9, 3))),
        );
        let (out, stats) = run(&p, &n, 128).expect("composes");
        assert_eq!(stats.sprites_moved, 1);
        // Moved two dots: covers x 7..9 of rows 3..5. x 8 is a comb dot (8 % 8 = 0 < 4): the comb is in front.
        assert_eq!(
            px(&out, 7, 3),
            [200, 1, 2],
            "visible where the layer behind it is the back layer"
        );
        assert_ne!(px(&out, 8, 3), [200, 1, 2], "hidden by the front layer");
        assert_eq!(px(&out, 8, 3), colour(0, 8));
        // The old place shows a layer again, not a hole of sprite colour.
        assert_ne!(px(&out, 5, 3), [200, 1, 2]);
        assert_ne!(px(&out, 5, 4), [200, 1, 2]);
    }

    /// A sprite with no counterpart in N+1 stays where it was.
    #[test]
    fn an_unmatched_sprite_is_held() {
        let (p, n) = (scene(1, 0, 0, Some((3, 14, 3))), scene(2, 0, 0, None));
        let (out, stats) = run(&p, &n, 128).expect("composes");
        assert_eq!((stats.sprites_moved, stats.sprites_held), (0, 1));
        assert_eq!(px(&out, 14, 3), [200, 1, 2]);
    }

    #[test]
    fn hazards_refuse_or_hold_rows() {
        let (p, n) = (scene(1, 8, 20, None), scene(2, 12, 22, None));
        for (bit, want) in [
            (hazard::MODE7, Refusal::Mode7),
            (hazard::HIRES, Refusal::Hires),
            (hazard::MOSAIC, Refusal::Mosaic),
            (hazard::WINDOW, Refusal::Window),
            (hazard::INTERLACE, Refusal::Interlace),
            (hazard::MIDLINE, Refusal::MidLine),
            (hazard::FORCED_BLANK, Refusal::Blanked),
            (hazard::DIRECT_COLOR, Refusal::Blanked),
        ] {
            // Every row: refused with the reason named.
            let mut hit = n.clone();
            hit.lines.iter_mut().for_each(|l| l.hazards = bit);
            assert_eq!(run(&p, &hit, 128).err(), Some(want), "bit {bit}");
            // A minority of rows (here 2 of 8): held from N, the rest moves.
            let mut some = n.clone();
            some.lines[6].hazards = bit;
            some.lines[7].hazards = bit;
            let (out, stats) = run(&p, &some, 128).expect("a few held rows do not refuse");
            assert_eq!(stats.rows_held, 2, "bit {bit}");
            assert_eq!(&out[6 * W * 4..], &p.rgba[6 * W * 4..], "held rows are N's");
            // The hazard in N, not N+1, holds them too.
            let mut in_n = p.clone();
            in_n.lines[7].hazards = bit;
            assert_eq!(run(&in_n, &n, 128).expect("composes").1.rows_held, 1);
        }
    }

    #[test]
    fn fades_jumps_cuts_and_gaps_are_refused() {
        let p = scene(1, 8, 20, None);
        // A fade: the brightness differs on any line.
        let mut fade = scene(2, 12, 22, None);
        fade.lines[3].brightness = 14;
        assert_eq!(run(&p, &fade, 128).err(), Some(Refusal::Fade));
        // A flash: the backdrop colour differs on more than an eighth of the lines.
        let mut flash = scene(2, 12, 22, None);
        flash.lines.iter_mut().for_each(|l| l.backdrop = 0x7FFF);
        assert_eq!(run(&p, &flash, 128).err(), Some(Refusal::Fade));
        // One line's backdrop (a gradient step) is not.
        let mut one = scene(2, 12, 22, None);
        one.lines[0].backdrop = 5;
        assert!(run(&p, &one, 128).is_ok());
        // A scroll jump on one layer.
        assert_eq!(
            run(&p, &scene(2, 200, 22, None), 128).err(),
            Some(Refusal::LargeMotion)
        );
        // A wrapped delta is small: 1023 to 3 is four dots, not a jump.
        assert_eq!(wrap_delta(1023, 3, SCROLL_PERIOD), 4);
        // A cut: the same small scroll, a different picture.
        let mut cut = scene(2, 12, 22, None);
        for px in cut.rgba.chunks_exact_mut(4) {
            px[0] = px[0].wrapping_add(77);
            px[1] = px[1].wrapping_add(33);
        }
        assert_eq!(run(&p, &cut, 128).err(), Some(Refusal::Discontinuity));
        // Not consecutive.
        assert_eq!(
            run(&p, &scene(5, 12, 22, None), 128).err(),
            Some(Refusal::NotConsecutive)
        );
    }

    /// Zero sizes, mismatched buffers, two sizes, every alpha: terminate and refuse or compose.
    #[test]
    fn degenerate_inputs_terminate() {
        let mut empty = scene(1, 0, 0, None);
        empty.width = 0;
        empty.rgba.clear();
        empty.codes.clear();
        empty.ids.clear();
        assert_eq!(run(&empty, &empty, 128).err(), Some(Refusal::Malformed));
        let mut short = scene(1, 0, 0, None);
        short.codes.pop();
        assert_eq!(
            run(&short, &scene(2, 0, 0, None), 128).err(),
            Some(Refusal::Malformed)
        );
        let mut few_ranks = scene(1, 0, 0, None);
        few_ranks.ranks.truncate(3);
        assert_eq!(
            run(&few_ranks, &scene(2, 0, 0, None), 128).err(),
            Some(Refusal::Malformed)
        );
        // Every row hazardous: refused, not looped.
        let mut all = scene(2, 0, 0, None);
        all.lines.iter_mut().for_each(|l| l.hazards = 0xFF);
        assert!(run(&scene(1, 0, 0, None), &all, 128).is_err());
        // Scroll values at the ends of their ranges, a layer of nothing, every alpha.
        let (mut a, mut b) = (
            scene(1, 0, 0, Some((3, 2, 2))),
            scene(2, 0, 0, Some((3, 5, 3))),
        );
        a.lines
            .iter_mut()
            .for_each(|l| l.hofs = [1023, 1023, 1023, 1023]);
        b.lines.iter_mut().for_each(|l| l.hofs = [2, 2, 2, 2]);
        for alpha in 0..=255u8 {
            let _ = run(&a, &b, alpha);
            let _ = run(&b, &a, alpha);
        }
        let (p, n) = (
            scene(1, 10, 20, Some((3, 2, 2))),
            scene(2, 13, 22, Some((3, 5, 3))),
        );
        for alpha in 0..=255u8 {
            assert!(run(&p, &n, alpha).is_ok(), "alpha {alpha}");
        }
        // A sprite id of 200 (past any entry) and a layer code of 7 are skipped, not indexed.
        let mut odd = scene(2, 10, 20, None);
        odd.ids[5] = 200;
        odd.codes[6] = 7 | (3 << 3);
        let _ = run(&p, &odd, 128);
    }

    /// Widescreen: the margins are gathered along with the most visible layer, so the scenery out there
    /// moves with the same scroll step.
    #[test]
    fn widened_margins_move_with_the_primary_layer() {
        let pad = 4;
        let wide = |l: &SnesLayers| {
            let ww = W + 2 * pad;
            let mut rgba = vec![0u8; ww * H * 4];
            let s2 = usize::from(l.lines[0].hofs[1]);
            for y in 0..H {
                for x in 0..ww {
                    let at = (y * ww + x) * 4;
                    // The same world as BG2 (the back layer, most visible in the rows).
                    rgba[at..at + 3].copy_from_slice(&colour(1, (s2 + x + 97 - pad) % 97));
                    rgba[at + 3] = 255;
                }
            }
            l.widened(&rgba, ww, H).expect("fits")
        };
        let (p, n) = (wide(&scene(1, 8, 20, None)), wide(&scene(2, 12, 24, None)));
        assert!(p.codes.contains(&CODE_MARGIN));
        let (out, _) = run(&p, &n, 128).expect("composes");
        let ww = W + 2 * pad;
        // Row 7 is BG2 only: margins there show the world at the intermediate scroll (22) exactly.
        for x in 0..ww {
            let at = (7 * ww + x) * 4;
            let want = colour(1, (22 + x + 97 - pad) % 97);
            assert_eq!(&out[at..at + 3], &want, "x {x}");
        }
        assert!(scene(1, 0, 0, None).widened(&[], 10, 8).is_none());
        assert!(scene(1, 0, 0, None).widened(&[], W - 2, 8).is_none());
    }
}
