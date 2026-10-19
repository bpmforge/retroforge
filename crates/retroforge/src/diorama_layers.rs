//! Layer separation and the flat HUD of the Diorama (ticket W34-08, D3, D4
//! and D7; `docs/design/HD2D_PLAN.md` section 4).
//!
//! The picture a console draws is one flat composite. To stand it up as a
//! scene this module pulls it apart again, from what the core thread
//! recorded about every pixel (which background layer drew it, `LayerInfo`):
//!
//! - **The playfield plane** (the layer the learner reads collision from,
//!   plus every layer that scrolls with it): the plane the blocks grow from.
//! - **Back planes**: a layer that scrolled slower than the playfield stands
//!   behind it by `1 - ratio` of [`SET_BACK_PX`] (Super Mario World's hills),
//!   and the pixels nothing drew (the backdrop colour, the sky) are the
//!   furthest plane of all.
//! - **The HUD** is not in the scene at all: its pixels are kept apart as a
//!   [`HudOverlay`] that is drawn on top of the finished 3D image in screen
//!   space, so it is never extruded, never pitched and never cropped by the
//!   preset's zoom.
//!
//! All planes are slots of one texture, an atlas of picture-sized slots
//! stacked vertically (`rf_renderer::diorama_side::Atlas`), so the pass keeps
//! its two textures.
//!
//! ## Holes, and what fills them
//!
//! A pixel a sprite or the HUD covered has no layer of its own in the picture
//! (the console only kept what won), so it is filled from the nearest pixel in
//! its row that is neither (a row with none takes the nearest row that has
//! one): the player is not drawn twice, once flat on the plane and once as a
//! billboard, and the HUD leaves no hole behind it.
//!
//! A back plane is hidden by whatever is in front of it where the layers
//! overlap, so its own texture has holes exactly there, and a camera that
//! moves would show them as slivers of the plane behind. Each back plane's
//! texture is therefore carried down through the pixels of the layers in front
//! of it (the nearest pixel of its own above, else below, in the same column):
//! the hills simply go on under the ground.
//!
//! No loop here is a hand-rolled walk (law 8): every one is a range or an
//! iterator over a buffer of known size.

use rf_enhance::heights::{cluster, Entity, SpriteBox};
use rf_renderer::diorama_mesh::SideBillboard;
use rf_renderer::diorama_side::{Atlas, BackPlane};

use crate::hud_watch::{HudInfo, LAYER_BACKDROP, LAYER_SPRITE};

/// How far behind the playfield a layer that does not scroll at all stands,
/// picture pixels. A layer with ratio `r` stands `(1 - r)` of it back.
pub const SET_BACK_PX: f32 = 48.0;
/// How much further than the furthest layer the sky stands.
pub const SKY_EXTRA_PX: f32 = 24.0;
/// A layer scrolling at least this fraction of the playfield's speed is
/// drawn on the playfield plane (it moves with it).
pub const MERGE_RATIO: f32 = 0.9;
/// Transparent texel rows between two slots of the atlas.
pub const SLOT_GAP: u32 = 2;
/// The atlas never has fewer slots than this, so a layer appearing or
/// leaving does not change the texture's size every few frames.
pub const MIN_SLOTS: u32 = 4;

/// What [`split`] reads.
pub struct LayerSplit<'a> {
    /// The picture as displayed, `width * height * 4` (at least `rows` rows).
    pub rgba: &'a [u8],
    /// The sprite layer of the same frame (alpha where a sprite won).
    pub sprite_rgba: &'a [u8],
    /// The layer of each pixel (`LayerInfo::ids`), `width * height` long.
    pub ids: &'a [u8],
    pub width: u32,
    /// The rows of the picture the game shows.
    pub rows: u32,
    /// The playfield's background layer.
    pub playfield: u8,
    pub ratios: [Option<f32>; 4],
    pub hud: &'a HudInfo,
}

/// The planes of one frame.
pub struct Planes {
    pub atlas_rgba: Vec<u8>,
    pub atlas_size: (u32, u32),
    pub atlas: Atlas,
    pub back: Vec<BackPlane>,
    pub hud: HudOverlay,
}

/// The HUD pixels of a frame, kept apart from the picture.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HudOverlay {
    /// `size.0 * size.1 * 4`; alpha is 255 where a HUD pixel is, else 0.
    pub rgba: Vec<u8>,
    /// The picture's size.
    pub size: (u32, u32),
    /// The rows `[first, last)` that hold any HUD pixel.
    pub rows: (u32, u32),
}

impl HudOverlay {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.0 >= self.rows.1
    }

    /// Draw the HUD over `dst` (a straight-alpha RGBA image of `dst_size`),
    /// scaled so the whole picture fills the image: each destination pixel
    /// takes the HUD pixel under its centre (nearest). Where the HUD has no
    /// pixel the image is untouched.
    pub fn composite(&self, dst: &mut [u8], dst_size: (u32, u32)) {
        let (pw, ph) = (self.size.0 as usize, self.size.1 as usize);
        let (dw, dh) = (dst_size.0 as usize, dst_size.1 as usize);
        if self.is_empty() || pw == 0 || ph == 0 || dw == 0 || dh == 0 || dst.len() < dw * dh * 4 {
            return;
        }
        let at = |o: usize, d: usize, p: usize| ((2 * o + 1) * p / (2 * d)).min(p - 1);
        for oy in 0..dh {
            let sy = at(oy, dh, ph);
            if (sy as u32) < self.rows.0 || (sy as u32) >= self.rows.1 {
                continue;
            }
            let src_row = &self.rgba[sy * pw * 4..(sy + 1) * pw * 4];
            let dst_row = &mut dst[oy * dw * 4..(oy + 1) * dw * 4];
            for (ox, out) in dst_row.chunks_exact_mut(4).enumerate() {
                let s = at(ox, dw, pw) * 4;
                if src_row[s + 3] != 0 {
                    out.copy_from_slice(&[src_row[s], src_row[s + 1], src_row[s + 2], 255]);
                }
            }
        }
    }
}

/// Fill every `covered` pixel of `rgba` and `ids` from the nearest uncovered
/// pixel in its row (the left one on a tie); a row with none takes the whole
/// of the nearest row that has one.
fn fill_covered(rgba: &mut [u8], ids: &mut [u8], covered: &[bool], w: usize, h: usize) {
    let mut has_source = vec![false; h];
    // One scratch row of each kind, reused for every row.
    let mut src_rgba = vec![0u8; w * 4];
    let mut src_ids = vec![0u8; w];
    let mut left = vec![usize::MAX; w];
    for (y, source) in has_source.iter_mut().enumerate() {
        let row = y * w..(y + 1) * w;
        let cov = &covered[row.clone()];
        if cov.iter().all(|c| *c) {
            continue;
        }
        *source = true;
        if !cov.iter().any(|c| *c) {
            continue;
        }
        src_rgba.copy_from_slice(&rgba[row.start * 4..row.end * 4]);
        src_ids.copy_from_slice(&ids[row.clone()]);
        let mut last = usize::MAX;
        for (x, slot) in left.iter_mut().enumerate() {
            if !cov[x] {
                last = x;
            }
            *slot = last;
        }
        let mut next = usize::MAX;
        for x in (0..w).rev() {
            if !cov[x] {
                next = x;
                continue;
            }
            let pick = match (left[x], next) {
                (usize::MAX, usize::MAX) => continue,
                (l, usize::MAX) => l,
                (usize::MAX, r) => r,
                (l, r) => {
                    if x - l <= r - x {
                        l
                    } else {
                        r
                    }
                }
            };
            let o = (row.start + x) * 4;
            rgba[o..o + 4].copy_from_slice(&src_rgba[pick * 4..pick * 4 + 4]);
            ids[row.start + x] = src_ids[pick];
        }
    }
    // Rows nothing could fill: copy the nearest row that has a source.
    for y in 0..h {
        if has_source[y] {
            continue;
        }
        let nearest = (1..h)
            .flat_map(|d| [y.checked_sub(d), (y + d < h).then_some(y + d)])
            .flatten()
            .find(|r| has_source[*r]);
        if let Some(r) = nearest {
            rgba.copy_within(r * w * 4..(r + 1) * w * 4, y * w * 4);
            ids.copy_within(r * w..(r + 1) * w, y * w);
        }
    }
}

/// The HUD pixels of a `w x rows` picture: the overlay that keeps them and
/// the mask of the pixels to take out of the scene.
fn take_hud(
    rgba: &[u8],
    ids: &[u8],
    info: &HudInfo,
    w: usize,
    rows: usize,
) -> (HudOverlay, Vec<bool>) {
    let mut mask = vec![false; w * rows];
    if !info.any() || w == 0 {
        return (HudOverlay::default(), mask);
    }
    let mut hud = HudOverlay {
        rgba: vec![0; w * rows * 4],
        size: (w as u32, rows as u32),
        rows: (0, 0),
    };
    let (mut first, mut last) = (u32::MAX, 0u32);
    for y in 0..rows {
        let band = info.row(u16::try_from(y).unwrap_or(u16::MAX));
        for x in 0..w {
            let i = y * w + x;
            let layer = info
                .layers
                .get(usize::from(ids[i]))
                .copied()
                .unwrap_or(false);
            if band || layer {
                hud.rgba[i * 4..i * 4 + 4].copy_from_slice(&[
                    rgba[i * 4],
                    rgba[i * 4 + 1],
                    rgba[i * 4 + 2],
                    255,
                ]);
                mask[i] = true;
                first = first.min(y as u32);
                last = last.max(y as u32 + 1);
            }
        }
    }
    if first < last {
        hud.rows = (first, last);
    }
    (hud, mask)
}

/// The picture with the HUD taken out (filled from its row, as [`split`]
/// does) and the HUD kept apart, for a view that is not layered (the top-down
/// floor). `None` when there is no HUD or the buffers do not fit.
#[must_use]
pub fn without_hud(
    rgba: &[u8],
    ids: &[u8],
    hud: &HudInfo,
    width: u32,
    rows: u32,
) -> Option<(Vec<u8>, HudOverlay)> {
    let (w, rows) = (width as usize, rows as usize);
    let n = w * rows;
    if !hud.any() || n == 0 || ids.len() < n || rgba.len() < n * 4 {
        return None;
    }
    let mut out = rgba[..n * 4].to_vec();
    let mut ids = ids[..n].to_vec();
    let (overlay, mask) = take_hud(&out, &ids, hud, w, rows);
    if overlay.is_empty() {
        return None;
    }
    fill_covered(&mut out, &mut ids, &mask, w, rows);
    Some((out, overlay))
}

/// Which slot a layer id draws on, `playfield` for anything not given its own.
struct SlotMap {
    of: [u8; 256],
    /// Per slot, how far behind the playfield plane it stands.
    back: Vec<f32>,
}

impl SlotMap {
    fn slot(&self, id: u8) -> usize {
        usize::from(self.of[usize::from(id)])
    }
}

/// Pull the frame apart. `None` when the buffers do not fit each other.
#[must_use]
pub fn split(s: &LayerSplit<'_>) -> Option<Planes> {
    let (w, rows) = (s.width as usize, s.rows as usize);
    if w == 0 || rows == 0 {
        return None;
    }
    let n = w * rows;
    if s.ids.len() < n || s.rgba.len() < n * 4 || s.sprite_rgba.len() < n * 4 {
        return None;
    }
    let mut rgba = s.rgba[..n * 4].to_vec();
    let mut ids = s.ids[..n].to_vec();
    // The HUD first: its pixels are kept, and hidden from the scene.
    let (hud, mut covered) = take_hud(&rgba, &ids, s.hud, w, rows);
    for (i, c) in covered.iter_mut().enumerate() {
        *c |= ids[i] == LAYER_SPRITE || s.sprite_rgba[i * 4 + 3] != 0;
    }
    fill_covered(&mut rgba, &mut ids, &covered, w, rows);

    // Which layers are drawn at all, and the planes they stand on.
    let mut seen = [false; 4];
    let mut backdrop = false;
    for id in &ids {
        match *id {
            0..=3 => seen[usize::from(*id)] = true,
            LAYER_BACKDROP => backdrop = true,
            _ => {}
        }
    }
    let mut map = SlotMap {
        of: [0; 256],
        back: vec![0.0],
    };
    for l in 0..4u8 {
        let li = usize::from(l);
        let ratio = s.ratios[li];
        let separate = l != s.playfield
            && seen[li]
            && !s.hud.layers[li]
            && ratio.is_some_and(|r| r < MERGE_RATIO);
        if separate {
            let r = ratio.unwrap_or(1.0).clamp(0.0, 1.0);
            // Lower layers are nearer (BG2 over BG3 at the same speed).
            map.back.push((1.0 - r) * SET_BACK_PX + 0.5 * f32::from(l));
            map.of[li] = (map.back.len() - 1) as u8;
        }
    }
    if backdrop {
        let furthest = map.back.iter().copied().fold(0.0_f32, f32::max);
        map.back.push(furthest + SKY_EXTRA_PX);
        let slot = (map.back.len() - 1) as u8;
        map.of[usize::from(LAYER_BACKDROP)] = slot;
    }
    let used = map.back.len() as u32;
    let slots = used.max(MIN_SLOTS);
    let pitch = s.rows + SLOT_GAP;
    let atlas_size = (s.width, slots * pitch);
    let mut atlas = vec![0u8; (atlas_size.0 * atlas_size.1 * 4) as usize];
    // Every slot carries the picture's colours (so linear filtering at a
    // hole's edge never pulls in black); alpha is set below.
    for slot in 0..used as usize {
        let base = slot * pitch as usize * w * 4;
        atlas[base..base + n * 4].copy_from_slice(&rgba);
        for px in atlas[base..base + n * 4].chunks_exact_mut(4) {
            px[3] = 0;
        }
    }
    for (i, id) in ids.iter().enumerate() {
        let o = (map.slot(*id) * pitch as usize * w + i) * 4;
        atlas[o + 3] = 255;
    }
    // Carry each back plane down through what stands in front of it.
    let mut pending: Vec<usize> = Vec::new();
    let mut had = vec![false; w];
    let mut left = vec![usize::MAX; w];
    for slot in 1..used as usize {
        let mine = |i: usize| map.slot(ids[i]) == slot;
        let nearer = |i: usize| map.back[map.slot(ids[i])] < map.back[slot];
        for x in 0..w {
            let mut above: Option<usize> = None;
            pending.clear();
            for y in 0..rows {
                let i = y * w + x;
                if mine(i) {
                    above = Some(i);
                } else if nearer(i) {
                    match above {
                        Some(a) => paint(&mut atlas, slot, pitch, w, i, &rgba, a),
                        None => pending.push(i),
                    }
                }
            }
            // Nothing of its own above: take the first of its own below.
            if !pending.is_empty() {
                if let Some(b) = (0..rows).map(|y| y * w + x).find(|i| mine(*i)) {
                    for i in pending.drain(..) {
                        paint(&mut atlas, slot, pitch, w, i, &rgba, b);
                    }
                }
            }
        }
        // A column the plane has nothing in at all (the hills fill it from
        // top to bottom, so the sky has no dot there): take the nearest
        // filled texel in the same row, so a moving camera never shows a
        // hole where the plane should be.
        let base = slot * pitch as usize * w;
        for y in 0..rows {
            let row = base + y * w;
            for (x, h) in had.iter_mut().enumerate() {
                *h = atlas[(row + x) * 4 + 3] != 0;
            }
            let filled = |x: usize| had[x];
            let mut last = usize::MAX;
            for (x, slot_left) in left.iter_mut().enumerate() {
                if filled(x) {
                    last = x;
                }
                *slot_left = last;
            }
            let mut next = usize::MAX;
            for x in (0..w).rev() {
                if filled(x) {
                    next = x;
                    continue;
                }
                if !nearer(y * w + x) {
                    continue;
                }
                let pick = match (left[x], next) {
                    (usize::MAX, usize::MAX) => continue,
                    (l, usize::MAX) => l,
                    (usize::MAX, r) => r,
                    (l, r) => {
                        if x - l <= r - x {
                            l
                        } else {
                            r
                        }
                    }
                };
                let (src, dst) = ((row + pick) * 4, (row + x) * 4);
                let colour: [u8; 3] = [atlas[src], atlas[src + 1], atlas[src + 2]];
                atlas[dst..dst + 3].copy_from_slice(&colour);
                atlas[dst + 3] = 255;
            }
        }
    }
    let back = (1..used as usize)
        .map(|slot| BackPlane {
            slot: slot as u32,
            back_px: map.back[slot],
        })
        .collect();
    Some(Planes {
        atlas_rgba: atlas,
        atlas_size,
        atlas: Atlas {
            slots,
            pitch_px: pitch as f32,
            playfield: 0,
        },
        back,
        hud,
    })
}

/// Opaque texel `dst` of `slot` with the colour of picture pixel `src`.
fn paint(atlas: &mut [u8], slot: usize, pitch: u32, w: usize, dst: usize, rgba: &[u8], src: usize) {
    let o = (slot * pitch as usize * w + dst) * 4;
    atlas[o..o + 3].copy_from_slice(&rgba[src * 4..src * 4 + 3]);
    atlas[o + 3] = 255;
}

/// The sprites standing together as the game draws them: touching sprites
/// are one entity (a 2x2 cluster of 8x8 OAM entries is one billboard), and a
/// sprite the game draws behind the background does not join one drawn in
/// front. Each with whether it is behind the background.
#[must_use]
pub fn entities_of(sprites: &[SpriteBox]) -> Vec<(Entity, bool)> {
    let mut out = Vec::new();
    let mut found = Vec::new();
    // Behind first: they are emitted first, so the ones in front draw last.
    for behind in [true, false] {
        let group: Vec<SpriteBox> = sprites
            .iter()
            .copied()
            .filter(|s| s.behind_bg == behind)
            .collect();
        cluster(&group, &mut found);
        out.extend(found.iter().map(|e| (*e, behind)));
    }
    out
}

/// Side-view billboards for the entities among `sprites` on a `screen`-sized
/// picture, feet at the entity's bottom edge; those wholly off it are dropped.
#[must_use]
pub fn entity_billboards(sprites: &[SpriteBox], screen: (u32, u32)) -> Vec<SideBillboard> {
    let (sw, sh) = (screen.0.max(1) as f32, screen.1.max(1) as f32);
    entities_of(sprites)
        .into_iter()
        .filter_map(|(e, behind_bg)| {
            let (x, y) = (e.x0 as f32, e.y0 as f32);
            let (w, h) = (e.width() as f32, e.height() as f32);
            if w <= 0.0 || h <= 0.0 || y >= sh || y + h <= 0.0 || x >= sw || x + w <= 0.0 {
                return None;
            }
            Some(SideBillboard {
                center_x_px: x + w / 2.0,
                feet_y_px: y + h,
                width_px: w,
                height_px: h,
                uv: [x / sw, y / sh, (x + w) / sw, (y + h) / sh],
                behind_bg,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 8;
    const H: usize = 6;

    fn px(c: u8) -> [u8; 4] {
        [c, c, c, 255]
    }

    /// A picture whose pixel colour is its layer id (`10 * id + 1`), rows of
    /// ids as given.
    fn frame(rows: [&str; H]) -> (Vec<u8>, Vec<u8>) {
        let mut rgba = Vec::new();
        let mut ids = Vec::new();
        for r in rows {
            for ch in r.chars() {
                let id = match ch {
                    '0'..='3' => ch as u8 - b'0',
                    's' => LAYER_SPRITE,
                    _ => LAYER_BACKDROP,
                };
                ids.push(id);
                rgba.extend_from_slice(&px(id.wrapping_mul(10).wrapping_add(1)));
            }
        }
        (rgba, ids)
    }

    fn run(
        rows: [&str; H],
        playfield: u8,
        ratios: [Option<f32>; 4],
        hud: &HudInfo,
    ) -> Option<Planes> {
        let (rgba, ids) = frame(rows);
        let mut sprite = vec![0u8; W * H * 4];
        for (i, id) in ids.iter().enumerate() {
            if *id == LAYER_SPRITE {
                sprite[i * 4..i * 4 + 4].copy_from_slice(&[9, 9, 9, 255]);
            }
        }
        split(&LayerSplit {
            rgba: &rgba,
            sprite_rgba: &sprite,
            ids: &ids,
            width: W as u32,
            rows: H as u32,
            playfield,
            ratios,
            hud,
        })
    }

    fn texel(p: &Planes, slot: u32, x: usize, y: usize) -> [u8; 4] {
        let pitch = p.atlas.pitch_px as usize;
        let o = ((slot as usize * pitch + y) * W + x) * 4;
        p.atlas_rgba[o..o + 4].try_into().unwrap()
    }

    const NO_HUD: HudInfo = HudInfo {
        layers: [false; 4],
        rows: Vec::new(),
    };

    #[test]
    fn a_slower_layer_gets_its_own_plane_and_the_backdrop_is_furthest() {
        let p = run(
            [
                "........", "...11...", "..1111..", "..1111..", "00000000", "00000000",
            ],
            0,
            [Some(1.0), Some(0.5), None, None],
            &NO_HUD,
        )
        .unwrap();
        // Slot 0 playfield, slot 1 layer 1 (half speed), slot 2 the sky.
        assert_eq!(p.back.len(), 2);
        assert_eq!(p.back[0].slot, 1);
        assert_eq!(p.back[1].slot, 2);
        assert!((p.back[0].back_px - (0.5 * SET_BACK_PX + 0.5)).abs() < 1e-4);
        assert!(p.back[1].back_px > p.back[0].back_px, "sky furthest");
        // Playfield pixels are opaque on slot 0 only; others transparent there.
        assert_eq!(texel(&p, 0, 0, 4)[3], 255);
        assert_eq!(texel(&p, 0, 3, 1)[3], 0);
        // The hills' own pixels are on slot 1, in their own colour.
        assert_eq!(texel(&p, 1, 3, 1), px(11));
        // The sky is the backdrop's pixels.
        assert_eq!(
            texel(&p, 2, 0, 0),
            px(LAYER_BACKDROP.wrapping_mul(10).wrapping_add(1))
        );
        assert_eq!(p.atlas.slots, MIN_SLOTS);
        assert_eq!(p.atlas_size.1, MIN_SLOTS * (H as u32 + SLOT_GAP));
    }

    #[test]
    fn a_back_plane_is_carried_down_under_the_layer_in_front_of_it() {
        let p = run(
            [
                "........", "..1111..", "..1111..", "00110000", "00000000", "00000000",
            ],
            0,
            [Some(1.0), Some(0.5), None, None],
            &NO_HUD,
        )
        .unwrap();
        // Under the ground (rows 3 to 5) the hills' texture goes on with the
        // colour of the hill pixel above it, opaque.
        assert_eq!(texel(&p, 1, 2, 4), px(11));
        assert_eq!(texel(&p, 1, 2, 5), px(11));
        // A column of the plane with nothing in it at all (column 0) takes the
        // nearest filled texel of its row, so a moving camera sees no hole.
        assert_eq!(texel(&p, 1, 0, 4), px(11));
        // Dots a layer behind the plane drew stay transparent: the hills are
        // not painted over the sky where the sky is what the picture shows.
        assert_eq!(texel(&p, 1, 0, 0)[3], 0);
        // The sky goes down under everything, too.
        assert_eq!(texel(&p, 2, 3, 5)[3], 255);
    }

    #[test]
    fn a_layer_with_no_measured_ratio_or_a_fast_one_stays_on_the_playfield_plane() {
        let p = run(["11110000"; H], 0, [Some(1.0), None, None, None], &NO_HUD).unwrap();
        assert_eq!(
            p.back.len(),
            0,
            "no backdrop pixels, no unknown-ratio layer"
        );
        assert_eq!(texel(&p, 0, 0, 0)[3], 255, "layer 1 drawn on slot 0");
        let p = run(
            ["11110000"; H],
            0,
            [Some(1.0), Some(0.95), None, None],
            &NO_HUD,
        )
        .unwrap();
        assert_eq!(p.back.len(), 0);
    }

    #[test]
    fn sprites_are_taken_out_of_every_plane_and_filled_from_their_row() {
        let p = run(
            [
                "00000000", "000ss000", "000ss000", "00000000", "00000000", "00000000",
            ],
            0,
            [Some(1.0), None, None, None],
            &NO_HUD,
        )
        .unwrap();
        // Layer 0's colour (1) fills where the sprite was; no sprite colour.
        assert_eq!(texel(&p, 0, 3, 1), px(1));
        assert_eq!(texel(&p, 0, 4, 2), px(1));
        let sprite_colour = LAYER_SPRITE.wrapping_mul(10).wrapping_add(1);
        assert!(
            p.atlas_rgba.chunks_exact(4).all(|t| t[0] != sprite_colour),
            "no sprite pixel anywhere in the atlas"
        );
    }

    #[test]
    fn hud_pixels_leave_the_scene_and_land_in_the_overlay() {
        let hud = HudInfo {
            layers: [false, false, true, false],
            rows: Vec::new(),
        };
        let p = run(
            [
                "22222222", "22..2222", "00000000", "00000000", "00000000", "00000000",
            ],
            0,
            [Some(1.0), None, Some(0.0), None],
            &hud,
        )
        .unwrap();
        // The overlay holds the layer-2 pixels of rows 0 and 1 only.
        assert_eq!(p.hud.rows, (0, 2));
        assert_eq!(p.hud.rgba[3], 255);
        assert_eq!(p.hud.rgba[..3], px(21)[..3]);
        let hole = (W + 2) * 4;
        assert_eq!(
            p.hud.rgba[hole + 3],
            0,
            "no HUD pixel where the layer drew nothing"
        );
        // The scene has no HUD pixel: the HUD's own pixels are filled from
        // the row's backdrop (the sky plane), nothing is left on the
        // playfield plane there.
        let sky = px(LAYER_BACKDROP.wrapping_mul(10).wrapping_add(1));
        assert_eq!(texel(&p, 0, 0, 0)[3], 0);
        assert_eq!(texel(&p, 1, 0, 0), sky);
        assert_eq!(texel(&p, 1, 7, 1), sky);
        // Layer 2 gets no plane of its own (it is HUD): only the sky.
        assert_eq!(p.back.len(), 1);
    }

    #[test]
    fn a_hud_band_of_rows_takes_whatever_drew_it() {
        let hud = HudInfo {
            layers: [false; 4],
            rows: vec![(4, 6)],
        };
        let p = run(["00000000"; H], 0, [Some(1.0), None, None, None], &hud).unwrap();
        assert_eq!(p.hud.rows, (4, 6));
        assert_eq!(p.hud.rgba[(4 * W) * 4 + 3], 255);
        assert_eq!(p.hud.rgba[(3 * W) * 4 + 3], 0);
        // The scene under the band is the row above it.
        assert_eq!(texel(&p, 0, 5, 5), px(1));
    }

    #[test]
    fn the_hud_lands_at_the_same_place_in_every_image_whatever_the_scene_does() {
        let hud = HudOverlay {
            rgba: {
                let mut v = vec![0u8; 4 * 4 * 4];
                // One opaque red pixel at (1, 0) of a 4x4 picture.
                v[4..8].copy_from_slice(&[255, 0, 0, 255]);
                v
            },
            size: (4, 4),
            rows: (0, 1),
        };
        // Two different 3D images of the same size: the HUD is the same.
        for fill in [10u8, 200] {
            let mut img = vec![fill; 8 * 8 * 4];
            hud.composite(&mut img, (8, 8));
            // Source pixel (1, 0) covers destination x 2..4, y 0..2.
            for y in 0..8 {
                for x in 0..8 {
                    let p = &img[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
                    let red = (2..4).contains(&x) && y < 2;
                    assert_eq!(p == [255, 0, 0, 255], red, "({x}, {y}) over {fill}: {p:?}");
                    if !red {
                        assert_eq!(p, [fill; 4]);
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_overlay_changes_nothing() {
        let mut img = vec![7u8; 16];
        HudOverlay::default().composite(&mut img, (2, 2));
        assert_eq!(img, vec![7u8; 16]);
    }

    fn spr(x: i16, y: i16, w: u8, h: u8, behind: bool) -> SpriteBox {
        SpriteBox {
            x,
            y,
            w,
            h,
            behind_bg: behind,
            tile: 0,
        }
    }

    /// D3: a 2x2 cluster of 8x8 OAM entries is one billboard.
    #[test]
    fn a_two_by_two_cluster_of_oam_entries_is_one_billboard() {
        let s = [
            spr(40, 100, 8, 8, false),
            spr(48, 100, 8, 8, false),
            spr(40, 108, 8, 8, false),
            spr(48, 108, 8, 8, false),
        ];
        let b = entity_billboards(&s, (256, 224));
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].width_px, b[0].height_px), (16.0, 16.0));
        assert_eq!((b[0].center_x_px, b[0].feet_y_px), (48.0, 116.0));
        assert!(!b[0].behind_bg);
    }

    #[test]
    fn a_sprite_behind_the_background_is_its_own_billboard() {
        let s = [spr(40, 100, 8, 8, false), spr(44, 100, 8, 8, true)];
        let b = entity_billboards(&s, (256, 224));
        assert_eq!(b.len(), 2);
        assert!(b[0].behind_bg, "behind ones are emitted first");
        assert!(!b[1].behind_bg);
    }

    #[test]
    fn separate_sprites_stay_separate_and_off_screen_ones_go() {
        let s = [
            spr(10, 10, 8, 8, false),
            spr(100, 10, 8, 8, false),
            spr(300, 10, 8, 8, false),
        ];
        assert_eq!(entity_billboards(&s, (256, 224)).len(), 2);
    }

    #[test]
    fn mismatched_buffers_give_no_planes() {
        let hud = HudInfo::default();
        assert!(split(&LayerSplit {
            rgba: &[0; 16],
            sprite_rgba: &[0; 16],
            ids: &[0; 4],
            width: 4,
            rows: 4,
            playfield: 0,
            ratios: [None; 4],
            hud: &hud,
        })
        .is_none());
    }

    /// Cost of [`split`] and of drawing the HUD on a 1170 x 896 image, on a
    /// frame shaped like Super Mario World's (status bar, hills on a sub
    /// screen, sky, ground, sprites). Timing, run by hand:
    /// `cargo test --release -p retroforge --lib layers_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing report, run by hand"]
    fn layers_cost() {
        let (w, h) = (256usize, 224usize);
        let ids: Vec<u8> = (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| match y {
                    0..=36 => {
                        if x % 5 < 2 {
                            2
                        } else {
                            LAYER_BACKDROP
                        }
                    }
                    37..=79 => LAYER_BACKDROP,
                    80..=175 => {
                        if (x / 40) % 2 == 0 {
                            1
                        } else {
                            LAYER_BACKDROP
                        }
                    }
                    176..=190 => {
                        if x % 7 == 0 {
                            LAYER_SPRITE
                        } else {
                            0
                        }
                    }
                    _ => 0,
                })
            })
            .collect();
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let sprite: Vec<u8> = ids
            .iter()
            .flat_map(|id| [9, 9, 9, if *id == LAYER_SPRITE { 255 } else { 0 }])
            .collect();
        let hud = HudInfo {
            layers: [false, false, true, false],
            rows: Vec::new(),
        };
        let input = LayerSplit {
            rgba: &rgba,
            sprite_rgba: &sprite,
            ids: &ids,
            width: w as u32,
            rows: h as u32,
            playfield: 0,
            ratios: [Some(1.0), Some(0.3), Some(0.0), None],
            hud: &hud,
        };
        let mut times = Vec::new();
        let mut overlay = HudOverlay::default();
        for _ in 0..300 {
            let t = std::time::Instant::now();
            overlay = split(&input).expect("planes").hud;
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "split 256x224: p50 {:.2} ms p95 {:.2} ms",
            times[150], times[285]
        );
        let mut img = vec![0u8; 1170 * 896 * 4];
        let mut times = Vec::new();
        for _ in 0..300 {
            let t = std::time::Instant::now();
            overlay.composite(&mut img, (1170, 896));
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "hud composite 1170x896: p50 {:.2} ms p95 {:.2} ms",
            times[150], times[285]
        );
    }
}
