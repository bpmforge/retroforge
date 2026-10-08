//! Find a game's camera by watching it play (tickets W25-01, W27-01).
//!
//! Every scrolling game copies its camera position into the console's
//! scroll registers each frame — NES `$2005`/`$2006`
//! ([nesdev PPU scrolling](https://www.nesdev.org/wiki/PPU_scrolling)),
//! SNES `$210D`/`$210E` BG1HOFS/VOFS (fullsnes "PPU Scrolling"). So the
//! byte a game keeps its camera in holds the low byte of the frame's
//! scroll on almost every frame the screen moves, across many distinct
//! values, and nothing else in work RAM does.
//!
//! [`CameraFinder`] scores every work-RAM byte that way, frame by frame,
//! and [`CameraFinder::verdict`] says when one clears the bar. It is an
//! observer: it is fed copies of the scroll and of RAM, and never touches
//! a core. One implementation serves the in-app finder and the profile
//! census (`crates/rf-harness/tests/profile_census.rs`).
//!
//! # Bounded memory
//!
//! Nothing is kept per frame. Each offset carries a fixed handful of
//! counters, and the 16-bit check (does the next byte carry when this one
//! wraps?) is counted as it happens rather than from a stored history —
//! sixty 8 KiB copies a second, kept, would be the RF-L-09 shape.

/// A camera axis needs this many frames of the screen moving.
pub const MIN_MOVING: u32 = 150;
/// ...and its byte must match the scroll on this share of them.
pub const MIN_RATIO: f32 = 0.70;
/// ...across at least this many distinct values (a byte stuck at 0
/// cannot win by sitting under a scroll of 0).
pub const MIN_DISTINCT: u32 = 64;
/// The vertical axis moves less in most games; its bar is lower.
pub const MIN_MOVING_Y: u32 = 100;
/// See [`MIN_MOVING_Y`].
pub const MIN_DISTINCT_Y: u32 = 32;

/// Whether a found axis is one byte or a little-endian pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    U8,
    U16,
}

impl Width {
    /// The profile schema's spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Width::U8 => "u8",
            Width::U16 => "u16",
        }
    }
}

/// One axis that cleared the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoundAxis {
    /// Offset into the work RAM the finder was fed.
    pub offset: usize,
    pub width: Width,
    /// Share of moving frames it matched.
    pub ratio: f32,
    /// How many frames the screen moved.
    pub moving: u32,
}

/// The camera, once the horizontal axis clears the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Found {
    pub x: FoundAxis,
    pub y: Option<FoundAxis>,
}

/// One candidate's evidence: `(offset, hits, distinct, wraps, carried)`.
pub type Candidate = (usize, u32, u32, u32, u32);

/// Per-offset evidence for one axis.
#[derive(Debug, Clone)]
pub struct Axis {
    /// Matches on moving frames, against same-frame and one-frame-late RAM
    /// (a game may update the variable after the NMI that wrote the
    /// register, or before).
    hits: Vec<[u32; 2]>,
    /// Which low-byte values matched.
    seen: Vec<[u64; 4]>,
    /// Wraps of this byte seen on moving frames, and how many of them the
    /// next byte carried.
    wraps: Vec<u32>,
    carried: Vec<u32>,
    moving: u32,
    last: Vec<u8>,
}

impl Axis {
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self {
            hits: vec![[0; 2]; len],
            seen: vec![[0; 4]; len],
            wraps: vec![0; len],
            carried: vec![0; len],
            moving: 0,
            last: Vec::new(),
        }
    }

    /// Frames the screen moved on this axis while armed.
    #[must_use]
    pub const fn moving(&self) -> u32 {
        self.moving
    }

    /// Score one frame. `scroll` is every low byte the frame wrote to this
    /// axis's register, `ram` the RAM at the frame's end and `prev` at the
    /// previous frame's end. Returns whether the screen moved.
    pub fn frame(&mut self, scroll: &[u8], ram: &[u8], prev: &[u8]) -> bool {
        let mut now = scroll.to_vec();
        now.sort_unstable();
        now.dedup();
        // Moving means different from the previous frame's scroll; the
        // first frame after a pause has nothing to differ from.
        if now.is_empty() || self.last.is_empty() || now == self.last {
            self.last = now;
            return false;
        }
        self.moving += 1;
        for at in 0..self.hits.len() {
            for (lag, src) in [ram, prev].into_iter().enumerate() {
                let Some(&b) = src.get(at) else { continue };
                if now.binary_search(&b).is_ok() {
                    self.hits[at][lag] += 1;
                    self.seen[at][usize::from(b >> 6)] |= 1 << (b & 63);
                }
            }
            // The carry check, counted as it happens: a wrap of this byte
            // between the two frames, and whether the next byte stepped
            // with it.
            if let (Some(&lo0), Some(&lo1)) = (prev.get(at), ram.get(at)) {
                let fwd = lo0 >= 0xC0 && lo1 < 0x40;
                let back = lo0 < 0x40 && lo1 >= 0xC0;
                if fwd || back {
                    self.wraps[at] += 1;
                    let hi0 = prev.get(at + 1).copied().unwrap_or(0);
                    let hi1 = ram.get(at + 1).copied().unwrap_or(0);
                    let want = if fwd {
                        hi0.wrapping_add(1)
                    } else {
                        hi0.wrapping_sub(1)
                    };
                    if hi1 == want {
                        self.carried[at] += 1;
                    }
                }
            }
        }
        self.last = now;
        true
    }

    /// Forget the last scroll seen, so the next frame is not compared with
    /// one from before a pause in scoring.
    pub fn forget_last(&mut self) {
        self.last.clear();
    }

    /// The best `n` offsets by matches (ties by distinct values), among
    /// those with at least 16 distinct values.
    #[must_use]
    pub fn best(&self, n: usize) -> Vec<Candidate> {
        let mut v: Vec<Candidate> = (0..self.hits.len())
            .map(|at| {
                let distinct = self.seen[at].iter().map(|w| w.count_ones()).sum();
                (
                    at,
                    self.hits[at][0].max(self.hits[at][1]),
                    distinct,
                    self.wraps[at],
                    self.carried[at],
                )
            })
            .filter(|&(_, _, distinct, _, _)| distinct >= 16)
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    /// The best offset, if it clears a bar.
    #[must_use]
    pub fn pick(&self, min_moving: u32, min_distinct: u32) -> Option<FoundAxis> {
        pick(
            self.moving,
            self.best(1).first().copied(),
            min_moving,
            min_distinct,
        )
    }
}

/// The bar, as a pure function of the evidence (the census's Python
/// reader applies the same numbers to the same cells).
#[must_use]
pub fn pick(
    moving: u32,
    best: Option<Candidate>,
    min_moving: u32,
    min_distinct: u32,
) -> Option<FoundAxis> {
    let (offset, hits, distinct, wraps, carried) = best?;
    if moving < min_moving || distinct < min_distinct {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let ratio = hits as f32 / moving as f32;
    if ratio < MIN_RATIO {
        return None;
    }
    // 16-bit when the next byte carried on every wrap seen; a camera whose
    // page lives elsewhere is read as its low byte, which is what the
    // scroll register holds anyway.
    let width = if wraps > 0 && carried == wraps {
        Width::U16
    } else {
        Width::U8
    };
    Some(FoundAxis {
        offset,
        width,
        ratio,
        moving,
    })
}

/// Both axes for each background layer, armed at the game's first Start
/// press.
///
/// Layers are scored separately and preferred in order: a game's
/// parallax layer keeps its own variable that tracks its own register,
/// and pooling the two would let it tie with the real camera (Super Mario
/// World's layer 2 at `$1E` against its camera at `$1A`). The NES has one
/// layer; the SNES finder is fed BG1 then BG2 (the Kirby titles scroll
/// the playfield on BG2).
#[derive(Debug, Clone)]
pub struct CameraFinder {
    /// `(x, y)` per layer, in preference order.
    pub layers: Vec<(Axis, Axis)>,
    armed: bool,
    prev: Vec<u8>,
}

impl CameraFinder {
    /// A finder over `len` bytes of work RAM and `layers` backgrounds.
    #[must_use]
    pub fn new(len: usize, layers: usize) -> Self {
        Self {
            layers: (0..layers.max(1))
                .map(|_| (Axis::new(len), Axis::new(len)))
                .collect(),
            armed: false,
            prev: vec![0; len],
        }
    }

    /// Feed one frame: per layer, every low byte written to its x and y
    /// registers. Scoring starts at the first frame with Start held: a
    /// title screen that scrolls in keeps its own variable, and its frames
    /// would dilute the game's camera. Returns whether any layer moved
    /// while armed.
    pub fn observe(&mut self, scroll: &[(&[u8], &[u8])], ram: &[u8], start_held: bool) -> bool {
        self.armed |= start_held;
        let mut moved = false;
        for ((x, y), (xs, ys)) in self.layers.iter_mut().zip(scroll) {
            if self.armed {
                moved |= x.frame(xs, ram, &self.prev);
                moved |= y.frame(ys, ram, &self.prev);
            } else {
                x.forget_last();
                y.forget_last();
            }
        }
        self.prev.clear();
        self.prev.extend_from_slice(ram);
        moved
    }

    /// Whether scoring has started.
    #[must_use]
    pub const fn armed(&self) -> bool {
        self.armed
    }

    /// Frames the first layer moved horizontally (progress for a UI).
    #[must_use]
    pub fn moving(&self) -> u32 {
        self.layers
            .iter()
            .map(|(x, _)| x.moving())
            .max()
            .unwrap_or(0)
    }

    /// The camera from the first layer whose x clears the bar. y is kept
    /// only when it clears its own bar and is not the same byte (or x's
    /// high byte).
    #[must_use]
    pub fn verdict(&self) -> Option<Found> {
        self.layers.iter().find_map(|(ax, ay)| {
            let x = ax.pick(MIN_MOVING, MIN_DISTINCT)?;
            let y = ay.pick(MIN_MOVING_Y, MIN_DISTINCT_Y).filter(|y| {
                !(y.offset == x.offset
                    || (x.width == Width::U16 && y.offset == x.offset + 1)
                    || (y.width == Width::U16 && x.offset == y.offset + 1))
            });
            Some(Found { x, y })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic game: a camera at `cam` (16-bit at `cam`/`cam+1`, or a
    /// byte), a byte that wanders, a byte stuck at zero; the scroll
    /// register gets the camera's low byte.
    fn play(finder: &mut CameraFinder, frames: u32, wide: bool, start_at: u32) {
        let mut ram = vec![0u8; 64];
        let mut pos: u16 = 0;
        for f in 0..frames {
            pos = pos.wrapping_add(3);
            if wide {
                ram[10..12].copy_from_slice(&pos.to_le_bytes());
            } else {
                ram[10] = pos as u8;
                ram[11] = 0x55; // not a page byte
            }
            ram[30] = (f.wrapping_mul(7) % 251) as u8; // noise
            let scroll = [pos as u8];
            finder.observe(&[(&scroll[..], &[0][..])], &ram, f == start_at);
        }
    }

    #[test]
    fn finds_a_sixteen_bit_camera() {
        let mut f = CameraFinder::new(64, 1);
        play(&mut f, 400, true, 0);
        let found = f.verdict().expect("a camera");
        assert_eq!(found.x.offset, 10);
        assert_eq!(found.x.width, Width::U16);
        assert!(found.x.ratio > 0.99);
        assert!(found.y.is_none(), "y never moved");
    }

    #[test]
    fn finds_an_eight_bit_camera() {
        let mut f = CameraFinder::new(64, 1);
        play(&mut f, 400, false, 0);
        let found = f.verdict().expect("a camera");
        assert_eq!((found.x.offset, found.x.width), (10, Width::U8));
    }

    #[test]
    fn nothing_before_start_and_not_enough_after() {
        let mut f = CameraFinder::new(64, 1);
        // Start pressed late: only the last 100 frames count, under the bar.
        play(&mut f, 400, true, 300);
        assert!(f.armed());
        assert!(f.moving() < MIN_MOVING);
        assert!(f.verdict().is_none());
    }

    #[test]
    fn one_byte_is_not_both_axes() {
        let mut f = CameraFinder::new(64, 1);
        let mut ram = vec![0u8; 64];
        for i in 0..400u32 {
            let v = (i * 3) as u8;
            ram[5] = v;
            f.observe(&[(&[v][..], &[v][..])], &ram, i == 0);
        }
        let found = f.verdict().expect("x");
        assert_eq!(found.x.offset, 5);
        assert!(found.y.is_none());
    }

    /// A parallax layer's variable must not beat the camera: the first
    /// layer that clears the bar wins.
    #[test]
    fn the_first_layer_wins_over_a_parallax_layer() {
        let mut f = CameraFinder::new(64, 2);
        let mut ram = vec![0u8; 64];
        for i in 0..400u32 {
            let cam = (i * 3) as u8;
            let far = (i * 3 / 2) as u8;
            ram[10] = cam;
            ram[20] = far;
            f.observe(
                &[(&[cam][..], &[0][..]), (&[far][..], &[0][..])],
                &ram,
                i == 0,
            );
        }
        assert_eq!(f.verdict().expect("x").x.offset, 10);
    }

    #[test]
    fn a_still_screen_finds_nothing() {
        let mut f = CameraFinder::new(64, 1);
        let ram = vec![0u8; 64];
        for i in 0..400 {
            f.observe(&[(&[0][..], &[0][..])], &ram, i == 0);
        }
        assert_eq!(f.moving(), 0);
        assert!(f.verdict().is_none());
    }
}
