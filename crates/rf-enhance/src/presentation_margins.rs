//! Ticket W38-11 (D-032): presentation-only margins for scrolling games, learned from play.
//!
//! Simulation margins (patching the game's culling) did not generalise, so a scrolling game's
//! 32:9 margins are drawn without touching the game at all:
//!
//! * the **level** beyond the 256-px view comes from a [`LearnedWorld`]: the background the
//!   player has already seen, stitched at the camera's world position (the recipe's camera
//!   rows) with sprite dots left out;
//! * the **objects** come from [`ObjectMemory`]: each object-table slot's sprite dots as last
//!   seen, drawn where the slot's world x now falls in the margin.
//!
//! Anything never seen stays what the caller already has (the VRAM margin). Nothing here reads
//! or writes a core: the shell hands in the frame, a sprite mask and the camera and object
//! values it peeked, so the original picture and the machine stay byte-identical (law 6). The
//! UI labels the mode [`LABEL`].
//!
//! **Honest limits.** A camera with no vertical row stitches at y = 0 (a vertically scrolling
//! room smears: the caller should then not offer the mode); an object seen only in motion
//! keeps one pose; a sprite touching an object's box (the player's) can be taken for part of
//! it. A game with no object table (Super Metroid) shows its level only.

use std::collections::{BTreeMap, HashMap};

use crate::widescreen::{fill_margins_keeping, NARROW_WIDTH};

/// What the UI calls this mode wherever it is selected or shown.
pub const LABEL: &str = "learned from play";

/// Side of one stored chunk of the learned world, in pixels.
const CHUNK: i64 = 32;
/// Most chunks a world keeps (a few levels' worth; the oldest-inserted are not evicted, the
/// world just stops growing, so memory is bounded).
const MAX_CHUNKS: usize = 16_384;
/// Radius (pixels) around an object's spot within which sprite dots are its own.
const OBJECT_RADIUS: i64 = 24;
/// Fewest dots a stamp needs to be kept.
const MIN_STAMP_DOTS: usize = 4;
/// Most dots one stamp keeps.
const MAX_STAMP_DOTS: usize = 2048;

/// The background seen so far, in world coordinates; one entry per `scene`.
#[derive(Debug, Clone, Default)]
pub struct LearnedWorld {
    scene: u64,
    chunks: HashMap<(i64, i64), Box<[[u8; 4]]>>,
}

impl LearnedWorld {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything if the scene (level/room key) changed.
    pub fn set_scene(&mut self, scene: u64) {
        if scene != self.scene {
            self.scene = scene;
            self.chunks.clear();
        }
    }

    /// Number of stored chunks.
    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Learn the 256-wide picture `rgba` (`w` x `h`, 4 bytes a dot) seen with its left edge at
    /// world `cam`. Dots where `is_sprite(x, y)` is true, and rows where `skip_row(y)` is
    /// true (a HUD band, which does not scroll), are not learned. Returns dots learned.
    pub fn observe(
        &mut self,
        rgba: &[u8],
        w: usize,
        h: usize,
        cam: (i64, i64),
        is_sprite: &dyn Fn(usize, usize) -> bool,
        skip_row: &dyn Fn(usize) -> bool,
    ) -> usize {
        let mut learned = 0;
        for y in 0..h {
            if skip_row(y) {
                continue;
            }
            for x in 0..w {
                if is_sprite(x, y) {
                    continue;
                }
                let Some(p) = rgba.get((y * w + x) * 4..(y * w + x) * 4 + 3) else {
                    continue;
                };
                let (wx, wy) = (cam.0 + x as i64, cam.1 + y as i64);
                let key = (wx.div_euclid(CHUNK), wy.div_euclid(CHUNK));
                if !self.chunks.contains_key(&key) && self.chunks.len() >= MAX_CHUNKS {
                    continue;
                }
                let chunk = self
                    .chunks
                    .entry(key)
                    .or_insert_with(|| vec![[0u8; 4]; (CHUNK * CHUNK) as usize].into());
                let i = (wy.rem_euclid(CHUNK) * CHUNK + wx.rem_euclid(CHUNK)) as usize;
                chunk[i] = [p[0], p[1], p[2], 0xFF];
                learned += 1;
            }
        }
        learned
    }

    /// The colour learned at a world position, if any.
    #[must_use]
    pub fn sample(&self, wx: i64, wy: i64) -> Option<[u8; 3]> {
        let c = self
            .chunks
            .get(&(wx.div_euclid(CHUNK), wy.div_euclid(CHUNK)))?;
        let p = c[(wy.rem_euclid(CHUNK) * CHUNK + wx.rem_euclid(CHUNK)) as usize];
        (p[3] != 0).then_some([p[0], p[1], p[2]])
    }
}

/// One object-table slot as read from RAM this frame (the shell peeks; nothing here does).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectFact {
    pub slot: u16,
    /// A kind byte if the table has one (0 otherwise); a stamp belongs to (slot, kind).
    pub kind: u8,
    pub alive: bool,
    pub world_x: i64,
    pub y: i64,
}

/// One remembered sprite pose: coloured dots relative to the object's (x, y).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    dots: Vec<(i16, i16, [u8; 3])>,
}

/// Each slot's sprite dots as last seen.
#[derive(Debug, Clone, Default)]
pub struct ObjectMemory {
    stamps: BTreeMap<(u16, u8), Stamp>,
}

impl ObjectMemory {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.stamps.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.stamps.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stamps.is_empty()
    }

    /// Remember the sprite dots around every live object that stands inside the picture.
    /// Each sprite dot belongs to the nearest object within [`OBJECT_RADIUS`].
    pub fn observe(
        &mut self,
        rgba: &[u8],
        w: usize,
        h: usize,
        cam_x: i64,
        objects: &[ObjectFact],
        is_sprite: &dyn Fn(usize, usize) -> bool,
    ) {
        let visible: Vec<(&ObjectFact, i64)> = objects
            .iter()
            .filter(|o| o.alive)
            .map(|o| (o, o.world_x - cam_x))
            .filter(|(_, sx)| (0..w as i64).contains(sx))
            .collect();
        if visible.is_empty() {
            return;
        }
        let mut dots: Vec<Vec<(i16, i16, [u8; 3])>> = vec![Vec::new(); visible.len()];
        for y in 0..h {
            for x in 0..w {
                if !is_sprite(x, y) {
                    continue;
                }
                let best = visible
                    .iter()
                    .enumerate()
                    .map(|(i, (o, sx))| {
                        let (dx, dy) = (x as i64 - sx, y as i64 - o.y);
                        (i, dx, dy, dx * dx + dy * dy)
                    })
                    .filter(|&(_, dx, dy, _)| {
                        dx.abs() <= OBJECT_RADIUS && dy.abs() <= OBJECT_RADIUS
                    })
                    .min_by_key(|&(_, _, _, d)| d);
                let Some((i, dx, dy, _)) = best else { continue };
                let at = (y * w + x) * 4;
                if let Some(p) = rgba.get(at..at + 3) {
                    if dots[i].len() < MAX_STAMP_DOTS {
                        // |dx|,|dy| <= OBJECT_RADIUS fits i16.
                        dots[i].push((dx as i16, dy as i16, [p[0], p[1], p[2]]));
                    }
                }
            }
        }
        for ((o, _), d) in visible.iter().zip(dots) {
            if d.len() >= MIN_STAMP_DOTS {
                self.stamps.insert((o.slot, o.kind), Stamp { dots: d });
            }
        }
    }

    /// Paint the remembered pose of every live object whose world x falls in the margin
    /// (outside the 256-dot view, inside the output) onto `out` (`out_w` x `h`, RGBA, the
    /// 256-dot view centred). Returns objects drawn.
    pub fn paint_margins(
        &self,
        out: &mut [u8],
        out_w: usize,
        h: usize,
        cam_x: i64,
        objects: &[ObjectFact],
    ) -> usize {
        let pad = (out_w as i64 - i64::from(NARROW_WIDTH)).div_euclid(2);
        let mut drawn = 0;
        for o in objects.iter().filter(|o| o.alive) {
            let sx = o.world_x - cam_x;
            if (0..i64::from(NARROW_WIDTH)).contains(&sx) {
                continue;
            }
            let Some(stamp) = self.stamps.get(&(o.slot, o.kind)) else {
                continue;
            };
            let mut any = false;
            for &(dx, dy, c) in &stamp.dots {
                let (ox, oy) = (pad + sx + i64::from(dx), o.y + i64::from(dy));
                // Only the margins: the centre 256 dots are the game's own picture.
                let centre = pad..pad + i64::from(NARROW_WIDTH);
                if centre.contains(&ox) || ox < 0 || ox >= out_w as i64 || oy < 0 || oy >= h as i64
                {
                    continue;
                }
                let at = (oy as usize * out_w + ox as usize) * 4;
                if let Some(px) = out.get_mut(at..at + 4) {
                    px.copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
                    any = true;
                }
            }
            drawn += usize::from(any);
        }
        drawn
    }
}

/// The whole presentation state of one session.
#[derive(Debug, Clone, Default)]
pub struct Presentation {
    pub world: LearnedWorld,
    pub objects: ObjectMemory,
}

impl Presentation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::too_many_arguments)]
    /// Learn from this frame (`rgba` `w` x `h`, `sprite` a per-dot flag, empty = none).
    pub fn observe(
        &mut self,
        rgba: &[u8],
        w: usize,
        h: usize,
        scene: u64,
        cam: (i64, i64),
        objects: &[ObjectFact],
        sprite: &[u8],
        skip_row: &dyn Fn(usize) -> bool,
    ) {
        let is_sprite = |x: usize, y: usize| sprite.get(y * w + x).copied().unwrap_or(0) != 0;
        if scene != self.world.scene {
            self.objects.clear();
        }
        self.world.set_scene(scene);
        self.world.observe(rgba, w, h, cam, &is_sprite, skip_row);
        self.objects.observe(rgba, w, h, cam.0, objects, &is_sprite);
    }

    #[allow(clippy::too_many_arguments)]
    /// The `out_w`-wide picture: the frame's own dots in the middle, the learned level beyond
    /// them (VRAM margin dots of `rgba` where the world is unknown), then remembered objects.
    #[must_use]
    pub fn compose(
        &self,
        rgba: &[u8],
        w: usize,
        h: usize,
        out_w: usize,
        cam: (i64, i64),
        objects: &[ObjectFact],
        skip_row: &dyn Fn(usize) -> bool,
    ) -> Vec<u8> {
        let mut out = fill_margins_keeping(
            rgba,
            w,
            h,
            out_w,
            cam,
            &|x, y| self.world.sample(x, y),
            skip_row,
            usize::from(NARROW_WIDTH),
        );
        self.objects
            .paint_margins(&mut out, out_w, h, cam.0, objects);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, c: [u8; 3]) -> Vec<u8> {
        (0..w * h).flat_map(|_| [c[0], c[1], c[2], 255]).collect()
    }
    const NO_SKIP: &dyn Fn(usize) -> bool = &|_| false;

    #[test]
    fn world_stitches_two_camera_positions_and_forgets_on_scene_change() {
        let mut w = LearnedWorld::new();
        let none: &dyn Fn(usize, usize) -> bool = &|_, _| false;
        w.observe(&solid(256, 4, [1, 2, 3]), 256, 4, (0, 0), none, NO_SKIP);
        w.observe(&solid(256, 4, [9, 9, 9]), 256, 4, (256, 0), none, NO_SKIP);
        assert_eq!(w.sample(10, 1), Some([1, 2, 3]));
        assert_eq!(w.sample(300, 1), Some([9, 9, 9]));
        assert_eq!(w.sample(-1, 0), None);
        assert_eq!(w.sample(0, 4), None);
        w.set_scene(7);
        assert_eq!(w.sample(10, 1), None);
    }

    #[test]
    fn sprites_and_hud_rows_are_not_learned() {
        let mut w = LearnedWorld::new();
        let sp: &dyn Fn(usize, usize) -> bool = &|x, _| x < 8;
        w.observe(&solid(256, 4, [5, 5, 5]), 256, 4, (0, 0), sp, &|y| y == 0);
        assert_eq!(w.sample(3, 1), None, "sprite dot");
        assert_eq!(w.sample(20, 1), Some([5, 5, 5]));
        assert_eq!(w.sample(20, 0), None, "hud row");
    }

    #[test]
    fn negative_world_positions_work() {
        let mut w = LearnedWorld::new();
        let none: &dyn Fn(usize, usize) -> bool = &|_, _| false;
        w.observe(&solid(256, 2, [4, 4, 4]), 256, 2, (-300, -1), none, NO_SKIP);
        assert_eq!(w.sample(-300, -1), Some([4, 4, 4]));
        assert_eq!(w.sample(-45, 0), Some([4, 4, 4]));
    }

    /// A 256x16 frame whose sprite dots form a 4x4 block at (sx, 4).
    fn frame_with_block(sx: usize) -> (Vec<u8>, Vec<u8>) {
        let mut rgba = solid(256, 16, [0, 0, 0]);
        let mut mask = vec![0u8; 256 * 16];
        for y in 4..8 {
            for x in sx..sx + 4 {
                rgba[(y * 256 + x) * 4..(y * 256 + x) * 4 + 3].copy_from_slice(&[200, 10, 10]);
                mask[y * 256 + x] = 1;
            }
        }
        (rgba, mask)
    }

    fn obj(world_x: i64, alive: bool) -> ObjectFact {
        ObjectFact {
            slot: 3,
            kind: 1,
            alive,
            world_x,
            y: 4,
        }
    }

    #[test]
    fn object_seen_then_drawn_in_the_margin() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(100);
        p.observe(&rgba, 256, 16, 1, (0, 0), &[obj(100, true)], &mask, NO_SKIP);
        assert_eq!(p.objects.len(), 1);
        // Later the camera is at 0 and the slot's world x is 300: 44 px past the right edge.
        let (rgba2, _) = frame_with_block(0);
        let out_w = 256 + 2 * 64;
        let out = p.compose(&rgba2, 256, 16, out_w, (0, 0), &[obj(300, true)], NO_SKIP);
        let pad = 64;
        let at = |x: usize, y: usize| out[(y * out_w + x) * 4..(y * out_w + x) * 4 + 3].to_vec();
        assert_eq!(
            at(pad + 300, 4),
            vec![200, 10, 10],
            "stamp lands at its world x"
        );
        assert_eq!(at(pad + 300 + 3, 7), vec![200, 10, 10]);
        assert_eq!(at(pad + 300 + 4, 7), vec![0, 0, 0], "outside the stamp");
    }

    #[test]
    fn dead_unseen_and_inside_objects_draw_nothing() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(100);
        p.observe(&rgba, 256, 16, 1, (0, 0), &[obj(100, true)], &mask, NO_SKIP);
        let mut out = vec![0u8; (256 + 128) * 16 * 4];
        let n = |p: &Presentation, o: ObjectFact, out: &mut Vec<u8>| {
            p.objects.paint_margins(out, 384, 16, 0, &[o])
        };
        assert_eq!(n(&p, obj(300, false), &mut out), 0, "dead");
        assert_eq!(n(&p, obj(100, true), &mut out), 0, "inside the picture");
        let mut other = obj(300, true);
        other.slot = 9;
        assert_eq!(n(&p, other, &mut out), 0, "never seen");
        assert_eq!(n(&p, obj(300, true), &mut out), 1);
    }

    #[test]
    fn compose_keeps_the_centre_byte_identical() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(100);
        p.observe(&rgba, 256, 16, 1, (0, 0), &[obj(100, true)], &mask, NO_SKIP);
        let out = p.compose(&rgba, 256, 16, 384, (0, 0), &[obj(300, true)], NO_SKIP);
        for y in 0..16 {
            assert_eq!(
                out[(y * 384 + 64) * 4..(y * 384 + 64 + 256) * 4],
                rgba[y * 256 * 4..(y + 1) * 256 * 4],
                "row {y}"
            );
        }
    }

    #[test]
    fn learned_level_fills_the_margin() {
        let mut p = Presentation::new();
        let none = vec![0u8; 256 * 4];
        p.observe(
            &solid(256, 4, [7, 7, 7]),
            256,
            4,
            1,
            (256, 0),
            &[],
            &none,
            NO_SKIP,
        );
        let out = p.compose(&solid(256, 4, [1, 1, 1]), 256, 4, 384, (0, 0), &[], NO_SKIP);
        // Right margin starts at output x 64+256 = world 256.
        assert_eq!(&out[(320) * 4..(320) * 4 + 3], &[7, 7, 7]);
        assert_eq!(
            &out[(10) * 4..(10) * 4 + 3],
            &[0, 0, 0],
            "unknown left stays fallback black"
        );
    }
}
