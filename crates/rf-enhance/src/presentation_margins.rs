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

/// Sprite dots that left the view, kept where they were last seen (for a game whose recipe has
/// no object table but a full 2-D camera). Dots inside the current view are dropped each frame,
/// so only what scrolled out stays; entries older than [`TRAIL_FRAMES`] are not drawn.
#[derive(Debug, Clone, Default)]
pub struct SeenSprites {
    dots: BTreeMap<(i64, i64), ([u8; 3], u64)>,
}

/// How long (frames) a remembered sprite dot is still drawn.
pub const TRAIL_FRAMES: u64 = 300;
/// Most dots remembered.
const MAX_TRAIL_DOTS: usize = 65_536;

impl SeenSprites {
    #[must_use]
    pub fn len(&self) -> usize {
        self.dots.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dots.is_empty()
    }

    pub fn clear(&mut self) {
        self.dots.clear();
    }

    /// Replace what is remembered inside the view `[cam.0, cam.0 + w)` with this frame's sprite
    /// dots, and forget what is too old.
    pub fn observe(
        &mut self,
        rgba: &[u8],
        w: usize,
        h: usize,
        cam: (i64, i64),
        frame: u64,
        is_sprite: &dyn Fn(usize, usize) -> bool,
    ) {
        let in_view: Vec<(i64, i64)> = self
            .dots
            .range((cam.0, i64::MIN)..(cam.0 + w as i64, i64::MIN))
            .map(|(k, _)| *k)
            .collect();
        for k in in_view {
            self.dots.remove(&k);
        }
        self.dots
            .retain(|_, (_, f)| frame.saturating_sub(*f) <= TRAIL_FRAMES);
        for y in 0..h {
            for x in 0..w {
                if !is_sprite(x, y) || self.dots.len() >= MAX_TRAIL_DOTS {
                    continue;
                }
                if let Some(p) = rgba.get((y * w + x) * 4..(y * w + x) * 4 + 3) {
                    self.dots.insert(
                        (cam.0 + x as i64, cam.1 + y as i64),
                        ([p[0], p[1], p[2]], frame),
                    );
                }
            }
        }
    }

    /// Paint the remembered dots that lie in the margins of an `out_w`-wide picture whose
    /// 256-dot view starts at world `cam`. Returns dots painted.
    pub fn paint_margins(&self, out: &mut [u8], out_w: usize, h: usize, cam: (i64, i64)) -> usize {
        let pad = (out_w as i64 - i64::from(NARROW_WIDTH)).div_euclid(2);
        let view = cam.0..cam.0 + i64::from(NARROW_WIDTH);
        let (lo, hi) = (cam.0 - pad, cam.0 + i64::from(NARROW_WIDTH) + pad);
        let mut n = 0;
        for (&(wx, wy), &(c, _)) in self.dots.range((lo, i64::MIN)..(hi, i64::MIN)) {
            let oy = wy - cam.1;
            if view.contains(&wx) || oy < 0 || oy >= h as i64 {
                continue;
            }
            let at = (oy as usize * out_w + (wx - cam.0 + pad) as usize) * 4;
            if let Some(px) = out.get_mut(at..at + 4) {
                px.copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
                n += 1;
            }
        }
        n
    }
}

/// One frame as the shell hands it over.
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    pub rgba: &'a [u8],
    pub w: usize,
    pub h: usize,
    /// Level/room key (a change forgets the learned world).
    pub scene: u64,
    /// World position of the picture's top-left dot.
    pub cam: (i64, i64),
    /// The object table's slots; `None` = the recipe has no table.
    pub objects: Option<&'a [ObjectFact]>,
    /// Per-dot sprite flags (`w * h`), empty = none known.
    pub sprite: &'a [u8],
    /// Frame counter (ages the trail).
    pub frame: u64,
    /// Last-seen sprites may be placed: a full 2-D camera, or a horizontal one with an object table
    /// (that game scrolls sideways only); see the module docs.
    pub trail: bool,
}

/// Frames of evidence before the table's x is judged world- or camera-relative.
const DECIDE_AFTER: u32 = 40;
/// Frames with live slots and no explained sprite before a table is given up.
const UNUSABLE_AFTER: u32 = 240;

/// The whole presentation state of one session.
#[derive(Debug, Clone, Default)]
pub struct Presentation {
    pub world: LearnedWorld,
    pub objects: ObjectMemory,
    pub trail: SeenSprites,
    /// Frames in which the table read as world x / as camera-relative x explained sprites.
    score: (u32, u32),
    /// Frames that had a live slot, and whether any slot ever explained a sprite (to judge a
    /// table that never does).
    live_frames: u32,
    explained: bool,
}

impl Presentation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the table's x was judged camera-relative (`None` until decided).
    #[must_use]
    pub fn relative(&self) -> Option<bool> {
        (self.score.0 + self.score.1 >= DECIDE_AFTER).then_some(self.score.1 > self.score.0)
    }

    /// The table has had live slots for [`UNUSABLE_AFTER`] frames and not one explained a sprite
    /// in either reading: it is of no use here, and last-seen sprites stand in.
    #[must_use]
    pub fn table_unusable(&self) -> bool {
        !self.explained && self.live_frames >= UNUSABLE_AFTER
    }

    /// The object as placed in the world, given how the table's x reads.
    #[must_use]
    pub fn place(&self, o: &ObjectFact, cam_x: i64) -> ObjectFact {
        let mut p = *o;
        if self.relative() == Some(true) {
            p.world_x = cam_x + (o.world_x & 0xFF);
        }
        p
    }

    /// Learn from this frame.
    pub fn observe(&mut self, f: &Frame<'_>, skip_row: &dyn Fn(usize) -> bool) {
        let w = f.w;
        let is_sprite = |x: usize, y: usize| f.sprite.get(y * w + x).copied().unwrap_or(0) != 0;
        if f.scene != self.world.scene {
            self.objects.clear();
            self.trail.clear();
            self.score = (0, 0);
            self.live_frames = 0;
            self.explained = false;
        }
        self.world.set_scene(f.scene);
        self.world
            .observe(f.rgba, w, f.h, f.cam, &is_sprite, skip_row);
        if let Some(objs) = f.objects {
            self.live_frames += u32::from(objs.iter().any(|o| o.alive));
            // Judged every frame until a reading explains sprites, so a table whose objects
            // only show up later is not given up on for good.
            if self.relative().is_none() || !self.explained {
                let rel: Vec<ObjectFact> = objs
                    .iter()
                    .map(|o| ObjectFact {
                        world_x: f.cam.0 + (o.world_x & 0xFF),
                        ..*o
                    })
                    .collect();
                let (a, b) = (
                    ObjectMemory::new().explained(f, objs, &is_sprite),
                    ObjectMemory::new().explained(f, &rel, &is_sprite),
                );
                self.explained |= a + b > 0;
                self.score.0 += u32::from(a > b);
                self.score.1 += u32::from(b > a);
            }
        }
        if let Some(objs) = f.objects.filter(|_| !self.table_unusable()) {
            let placed: Vec<ObjectFact> = objs.iter().map(|o| self.place(o, f.cam.0)).collect();
            self.objects
                .observe(f.rgba, w, f.h, f.cam.0, &placed, &is_sprite);
        } else if f.trail {
            self.trail
                .observe(f.rgba, w, f.h, f.cam, f.frame, &is_sprite);
        }
    }

    /// The `out_w`-wide picture: the frame's own dots in the middle, the learned level beyond
    /// them (the frame's own VRAM margin dots where the world is unknown), then the
    /// remembered objects. `f.sprite`/`f.frame` are not used.
    #[must_use]
    pub fn compose(
        &self,
        f: &Frame<'_>,
        out_w: usize,
        skip_row: &dyn Fn(usize) -> bool,
    ) -> Vec<u8> {
        let mut out = fill_margins_keeping(
            f.rgba,
            f.w,
            f.h,
            out_w,
            f.cam,
            &|x, y| self.world.sample(x, y),
            skip_row,
            usize::from(NARROW_WIDTH),
        );
        if let Some(objs) = f.objects.filter(|_| !self.table_unusable()) {
            let placed: Vec<ObjectFact> = objs.iter().map(|o| self.place(o, f.cam.0)).collect();
            self.objects
                .paint_margins(&mut out, out_w, f.h, f.cam.0, &placed);
        } else if f.trail {
            self.trail.paint_margins(&mut out, out_w, f.h, f.cam);
        }
        out
    }
}

impl ObjectMemory {
    /// How many live objects inside the view have at least [`MIN_STAMP_DOTS`] sprite dots
    /// near them (a measure of how well `objects` explains this frame's sprites).
    fn explained(
        &self,
        f: &Frame<'_>,
        objects: &[ObjectFact],
        is_sprite: &dyn Fn(usize, usize) -> bool,
    ) -> usize {
        objects
            .iter()
            .filter(|o| o.alive)
            .filter(|o| (0..f.w as i64).contains(&(o.world_x - f.cam.0)))
            .filter(|o| {
                let sx = o.world_x - f.cam.0;
                let mut n = 0;
                for y in (o.y - OBJECT_RADIUS).max(0)..(o.y + OBJECT_RADIUS).min(f.h as i64) {
                    for x in (sx - OBJECT_RADIUS).max(0)..(sx + OBJECT_RADIUS).min(f.w as i64) {
                        n += usize::from(is_sprite(x as usize, y as usize));
                    }
                }
                n >= MIN_STAMP_DOTS
            })
            .count()
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

    fn fr<'a>(
        rgba: &'a [u8],
        sprite: &'a [u8],
        cam: (i64, i64),
        objects: Option<&'a [ObjectFact]>,
    ) -> Frame<'a> {
        Frame {
            rgba,
            w: 256,
            h: rgba.len() / 1024,
            scene: 1,
            cam,
            objects,
            sprite,
            frame: 0,
            trail: true,
        }
    }

    #[test]
    fn object_seen_then_drawn_in_the_margin() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(100);
        let seen = [obj(100, true)];
        p.observe(&fr(&rgba, &mask, (0, 0), Some(&seen)), NO_SKIP);
        assert_eq!(p.objects.len(), 1);
        // Later the camera is at 0 and the slot's world x is 300: 44 px past the right edge.
        let (rgba2, _) = frame_with_block(0);
        let out_w = 256 + 2 * 64;
        let now = [obj(300, true)];
        let out = p.compose(&fr(&rgba2, &[], (0, 0), Some(&now)), out_w, NO_SKIP);
        let pad = 64;
        let at = |x: usize, y: usize| out[(y * out_w + x) * 4..(y * out_w + x) * 4 + 3].to_vec();
        assert_eq!(at(pad + 300, 4), vec![200, 10, 10], "stamp at its world x");
        assert_eq!(at(pad + 300 + 3, 7), vec![200, 10, 10]);
        assert_eq!(at(pad + 300 + 4, 7), vec![0, 0, 0], "outside the stamp");
    }

    #[test]
    fn dead_unseen_and_inside_objects_draw_nothing() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(100);
        let seen = [obj(100, true)];
        p.observe(&fr(&rgba, &mask, (0, 0), Some(&seen)), NO_SKIP);
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
        let seen = [obj(100, true)];
        p.observe(&fr(&rgba, &mask, (0, 0), Some(&seen)), NO_SKIP);
        let now = [obj(300, true)];
        let out = p.compose(&fr(&rgba, &[], (0, 0), Some(&now)), 384, NO_SKIP);
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
        let seen = solid(256, 4, [7, 7, 7]);
        p.observe(&fr(&seen, &[], (256, 0), Some(&[])), NO_SKIP);
        let now = solid(256, 4, [1, 1, 1]);
        let out = p.compose(&fr(&now, &[], (0, 0), Some(&[])), 384, NO_SKIP);
        // Right margin starts at output x 64+256 = world 256.
        assert_eq!(&out[(320) * 4..(320) * 4 + 3], &[7, 7, 7]);
        assert_eq!(
            &out[10 * 4..10 * 4 + 3],
            &[0, 0, 0],
            "unknown stays fallback"
        );
    }

    #[test]
    fn trail_keeps_sprites_that_left_the_view_and_ages_them() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(200);
        p.observe(&fr(&rgba, &mask, (0, 0), None), NO_SKIP);
        // The camera moves right by 100: the block (world 200..204) is now at view x 100.
        // Moving left instead puts it past the right edge: camera -100 => view x 300.
        let plain = solid(256, 16, [0, 0, 0]);
        let mut f = fr(&plain, &[], (-100, 0), None);
        f.frame = 10;
        let out = p.compose(&f, 384, NO_SKIP);
        let at = (4 * 384 + 64 + 300) * 4;
        assert_eq!(&out[at..at + 3], &[200, 10, 10]);
        // A frame that sees the spot again without a sprite there forgets it.
        let mut g = fr(&plain, &[], (150, 0), None);
        g.frame = 11;
        p.observe(&g, NO_SKIP);
        assert!(p.trail.is_empty());
    }

    #[test]
    fn trail_off_without_a_full_camera() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(200);
        let mut f = fr(&rgba, &mask, (0, 0), None);
        f.trail = false;
        p.observe(&f, NO_SKIP);
        assert!(p.trail.is_empty());
    }

    #[test]
    fn camera_relative_tables_are_detected() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(100);
        // The table says x = 100 (screen coordinates); the camera is at 1000, so read as a
        // world x the object would be far off screen.
        for i in 0..DECIDE_AFTER {
            let o = [obj(100, true)];
            let mut f = fr(&rgba, &mask, (1000, 0), Some(&o));
            f.frame = u64::from(i);
            p.observe(&f, NO_SKIP);
        }
        assert_eq!(p.relative(), Some(true));
        assert_eq!(p.place(&obj(100, true), 1000).world_x, 1100);
    }

    #[test]
    fn world_tables_are_detected() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(100);
        for i in 0..DECIDE_AFTER {
            let o = [obj(1100, true)];
            let mut f = fr(&rgba, &mask, (1000, 0), Some(&o));
            f.frame = u64::from(i);
            p.observe(&f, NO_SKIP);
        }
        assert_eq!(p.relative(), Some(false));
    }

    #[test]
    fn a_table_that_explains_nothing_gives_way_to_last_seen_sprites() {
        let mut p = Presentation::new();
        let (rgba, mask) = frame_with_block(200);
        // A live slot far from any sprite, every frame.
        let o = [ObjectFact {
            slot: 1,
            kind: 0,
            alive: true,
            world_x: 5000,
            y: 0,
        }];
        for i in 0..=UNUSABLE_AFTER {
            let mut f = fr(&rgba, &mask, (0, 0), Some(&o));
            f.frame = u64::from(i);
            p.observe(&f, NO_SKIP);
        }
        assert!(p.table_unusable());
        assert!(!p.trail.is_empty(), "the trail took over");
    }
}
