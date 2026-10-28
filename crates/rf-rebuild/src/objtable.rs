//! Object-table finder (ticket W38-02): slot stride, x/y/type fields and
//! sprite-to-object binding from OAM writes. Offsets only (law 5, D-021).
//!
//! Console-agnostic, like [`crate::dataregion`]: a driver samples the game at
//! the end of every frame (work RAM, the OAM the PPU holds, the background
//! scroll the picture used) into a [`Trace`]; [`infer`] is pure and turns a
//! trace into [`ObjectTable`] recipe rows plus the evidence behind them.
//!
//! The idea. A game keeps its objects in RAM as parallel byte arrays (one
//! array per field, a byte per slot) or as records, and every frame turns the
//! live ones into OAM entries. So a byte that *is* an object's x position
//! satisfies `oam_x == x - scroll + dx` for some sprite of that object, every
//! frame, with `dx` a constant of the game's drawing code. That is a strong
//! signature: a random byte matches an arbitrary sprite about one frame in six
//! (about as many x values are on screen), a position byte matches nearly
//! every frame the object exists.
//!
//! 1. **x columns.** Every varying RAM byte is scored against the sprites'
//!    x values; `dx` is the best of the 256 offsets and the score is a
//!    z-score against the chance rate ([`score_x`]). RAM that mirrors OAM (the
//!    shadow copy the game builds before the upload) is masked first.
//! 2. **Strides.** Bytes that score are grouped into chains of equal stride
//!    ([`chains`]); the chain with the most members wins its addresses.
//! 3. **y, x high, state, kind.** Given an x chain, the y column is the one
//!    whose bytes, added to the sprites that already share the object's x,
//!    give one constant `dy`; the x high byte is the one that steps by one
//!    when the low byte wraps; the state byte is non-zero exactly while a
//!    sprite is bound; the type byte holds still for as long as a slot lives.
//! 4. **Binding.** Each slot is bound to the OAM entries its position matches
//!    ([`Binding`]) and the table is checked against every frame
//!    ([`Binding::rate`]): the acceptance number.
//!
//! Hardware notes: the NES takes OAM from a 256-byte RAM page by `$4014` DMA
//! (nesdev "PPU OAM"), so object code builds sprites in RAM and the finder
//! sees the shadow copy; the SNES moves its 544-byte table by DMA to `$2104`
//! (fullsnes "OAM"). The OAM a PPU holds lags the RAM it was built from by up
//! to a frame, hence [`Binding::lag`].

// Slot-by-frame grids are indexed on both axes by design.
#![allow(clippy::needless_range_loop)]

use crate::recipe::{Grade, ObjectTable};

/// One sprite as the picture shows it. Only the low eight bits of x are kept:
/// object x bytes are eight bits wide, and a sprite beyond the edge is not
/// evidence of anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sprite {
    /// Position in the OAM, in entries.
    pub index: u16,
    pub x: u8,
    pub y: u8,
}

/// The game at the end of one frame.
#[derive(Debug, Clone, Default)]
pub struct Frame {
    /// Work RAM from its start, as many bytes as the caller wants searched.
    pub ram: Vec<u8>,
    /// The raw OAM bytes (used only to mask RAM that mirrors it).
    pub oam: Vec<u8>,
    /// The sprites that can be on screen.
    pub sprites: Vec<Sprite>,
    /// Horizontal scroll of the playfield in effect below any status bar.
    pub cam_x: u16,
    pub cam_y: u16,
}

/// A run of frames.
#[derive(Debug, Clone, Default)]
pub struct Trace {
    /// Added to a RAM index to make the address a recipe row carries (`0` on
    /// the NES, `0x7E_0000` on the SNES).
    pub ram_base: u32,
    /// The scroll's modulus: 512 on the NES (two nametables), 65536 on the
    /// SNES. Position arithmetic against the scroll is done in this ring.
    pub wrap: u32,
    pub frames: Vec<Frame>,
}

/// What we measured for one slot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SlotBinding {
    pub slot: u32,
    /// The OAM entry most often drawn at the slot's position.
    pub oam_index: Option<u16>,
    /// Frames the slot's position matched a sprite.
    pub bound_frames: u32,
    /// Frames the slot was live and its position on screen.
    pub live_frames: u32,
    /// Of those, the frames a sprite matched.
    pub matched_frames: u32,
}

/// How a found table lines up with the picture.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Binding {
    /// Frames between the RAM a sprite was built from and the OAM that shows
    /// it (`0` or `1`).
    pub lag: usize,
    /// Constant added to `x - scroll` to give the sprite x.
    pub dx: i32,
    pub dy: i32,
    pub slots: Vec<SlotBinding>,
    /// Slot-frames where a live slot's position was on screen.
    pub expected: u64,
    /// Of those, the ones whose x and y match a sprite exactly.
    pub matched: u64,
    /// Frames with at least one expected slot, and how many of them had every
    /// expected slot matched.
    pub frames_checked: u32,
    pub frames_clean: u32,
    /// How "live" was decided: from the state byte, or from the binding.
    pub live_from_state: bool,
}

impl Binding {
    /// The share of expected slot-frames that matched: the acceptance number.
    #[must_use]
    pub fn rate(&self) -> f64 {
        if self.expected == 0 {
            0.0
        } else {
            self.matched as f64 / self.expected as f64
        }
    }
}

/// A table with the evidence it was built from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Found {
    pub table: ObjectTable,
    pub binding: Binding,
    /// The z-score of the x column (the confidence in the table).
    pub z: f64,
}

/// A recipe row for a found table: `grade` is what the caller's experiment
/// earned; `experiment` is appended to the source note.
#[must_use]
pub fn to_row(f: &Found, grade: Grade, experiment: &str) -> ObjectTable {
    let mut t = f.table.clone();
    t.grade = grade;
    t.source = format!(
        "finder: {} slots stride {}, x z={:.0}, {}/{} live slot-frames match OAM (lag {}){}",
        f.table.slots,
        f.table.stride,
        f.z,
        f.binding.matched,
        f.binding.expected,
        f.binding.lag,
        experiment
    );
    t
}

/// The grade a result earns on its own evidence: observed when the table
/// follows the picture almost everywhere on a state-gated count, otherwise a
/// candidate.
#[must_use]
pub fn grade_of(f: &Found) -> Grade {
    if f.binding.live_from_state && f.binding.expected >= 200 && f.binding.rate() >= 0.95 {
        Grade::Observed
    } else {
        Grade::Candidate
    }
}

// ---------------------------------------------------------------------------
// preparation

/// Smallest z-score for a byte to count as an x position.
pub const MIN_Z: f64 = 12.0;
/// Fewest matching frames for the same.
const MIN_MATCHES: u32 = 60;
/// Largest stride searched.
const MAX_STRIDE: usize = 64;
/// Pixels a sprite column may sit from where the x byte puts it (see
/// [`Cols::bound`]).
pub const X_SLACK: i8 = 2;
/// Empty slots tolerated between two members of a chain.
const MAX_GAP: usize = 6;
/// Share of its live frames a trailing slot must match to be claimed.
const TRIM_BELOW: f64 = 0.7;
/// Fewest slots in a table.
const MIN_SLOTS: usize = 3;
/// Rows of frames sampled when searching the other columns.
const SAMPLE_FRAMES: usize = 900;

struct Pre<'a> {
    t: &'a Trace,
    /// RAM bytes searched.
    r: usize,
    /// Per frame: the distinct sprite x values.
    xs: Vec<Vec<u8>>,
    /// Addresses (indices) that mirror the OAM.
    masked: Vec<bool>,
    /// Addresses whose value never changes.
    constant: Vec<bool>,
}

fn distinct(it: impl Iterator<Item = u8>) -> Vec<u8> {
    let mut seen = [false; 256];
    let mut out = Vec::new();
    for v in it {
        if !seen[usize::from(v)] {
            seen[usize::from(v)] = true;
            out.push(v);
        }
    }
    out
}

impl<'a> Pre<'a> {
    fn new(t: &'a Trace) -> Self {
        let r = t.frames.iter().map(|f| f.ram.len()).min().unwrap_or(0);
        let xs = t
            .frames
            .iter()
            .map(|f| distinct(f.sprites.iter().map(|s| s.x)))
            .collect();
        let constant = (0..r)
            .map(|a| {
                t.frames
                    .first()
                    .is_none_or(|f0| t.frames.iter().all(|f| f.ram[a] == f0.ram[a]))
            })
            .collect();
        let mut pre = Self {
            t,
            r,
            xs,
            masked: vec![false; r],
            constant,
        };
        pre.mask_oam_mirror();
        pre
    }

    fn n(&self) -> usize {
        self.t.frames.len()
    }

    fn ram(&self, f: usize, a: usize) -> u8 {
        self.t.frames[f].ram[a]
    }

    /// Mask RAM that is a copy of the OAM: the window of OAM length whose
    /// non-zero bytes equal the OAM bytes in at least 40% of sampled frames'
    /// entries. Object arrays are not (their bytes are not in OAM order).
    fn mask_oam_mirror(&mut self) {
        let Some(len) = self.t.frames.iter().map(|f| f.oam.len()).max() else {
            return;
        };
        if len == 0 || self.r < len {
            return;
        }
        // Frames with the most sprites say the most.
        let mut order: Vec<usize> = (0..self.n()).collect();
        order.sort_by_key(|&f| std::cmp::Reverse(self.t.frames[f].sprites.len()));
        let sample: Vec<usize> = order.into_iter().take(16).collect();
        let mut best: Option<(usize, f64)> = None;
        for off in 0..=(self.r - len) {
            let (mut eq, mut tot) = (0u32, 0u32);
            for &f in &sample {
                let fr = &self.t.frames[f];
                for (k, &o) in fr.oam.iter().enumerate() {
                    if o != 0 {
                        tot += 1;
                        eq += u32::from(fr.ram[off + k] == o);
                    }
                }
            }
            if tot >= 64 {
                let share = f64::from(eq) / f64::from(tot);
                if share >= 0.4 && best.is_none_or(|(_, b)| share > b) {
                    best = Some((off, share));
                }
            }
        }
        if let Some((off, _)) = best {
            for m in &mut self.masked[off..off + len] {
                *m = true;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// stage 1: x columns

/// One byte that follows a sprite's x.
#[derive(Debug, Clone, Copy, PartialEq)]
struct XCand {
    addr: usize,
    z: f64,
    dx: u8,
    matches: u32,
}

/// The peak of an offset histogram. A metasprite is several sprites, so the
/// object's own coordinate also peaks 8 and 16 away; the coordinate is the
/// lowest peak nearly as strong as the highest (the top-left sprite).
fn peak(hist: &[u32; 256]) -> Option<(usize, u32)> {
    let (mut d, &mut_best) = hist.iter().enumerate().max_by_key(|&(_, &c)| c)?;
    for step in [8usize, 16, 24] {
        let lower = (d + 256 - step) % 256;
        if hist[lower] * 10 >= mut_best * 8 {
            d = lower;
        }
    }
    Some((d, hist[d]))
}

/// Score one byte as an x position at `lag`.
fn score_x(pre: &Pre<'_>, a: usize, lag: usize) -> Option<XCand> {
    let mut hist = [0u32; 256];
    let (mut info, mut exp, mut var) = (0u32, 0f64, 0f64);
    for f in (lag + 1)..pre.n() {
        let v = pre.ram(f - lag, a);
        // Evidence only when the on-screen position the byte implies moved: a
        // byte that tracks the scroll implies a fixed position and would
        // match any parked sprite (the status bar's) for free.
        let cam = pre.t.frames[f].cam_x as u8;
        let prev = pre
            .ram(f - lag - 1, a)
            .wrapping_sub(pre.t.frames[f - 1].cam_x as u8);
        let moved = v.wrapping_sub(cam) != prev;
        if !moved {
            continue;
        }
        info += 1;
        let p = pre.xs[f].len() as f64 / 256.0;
        exp += p;
        var += p * (1.0 - p);
        for &x in &pre.xs[f] {
            hist[usize::from(x.wrapping_sub(v).wrapping_add(cam))] += 1;
        }
    }
    if info == 0 {
        return None;
    }
    let (dx, best) = peak(&hist)?;
    let z = (f64::from(best) - exp) / (var + 1.0).sqrt();
    (best >= MIN_MATCHES && z >= MIN_Z).then_some(XCand {
        addr: a,
        z,
        dx: dx as u8,
        matches: best,
    })
}

/// Every byte that follows an x position at `lag`.
fn score_all_x(pre: &Pre<'_>, lag: usize) -> Vec<XCand> {
    (0..pre.r)
        .filter(|&a| !pre.masked[a] && !pre.constant[a])
        .filter_map(|a| score_x(pre, a, lag))
        .collect()
}

// ---------------------------------------------------------------------------
// stage 2: chains

/// Bytes at equal spacing: `base + k * stride`, with up to one missing between
/// two members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub base: usize,
    pub stride: usize,
    pub slots: usize,
    pub members: usize,
}

/// Group addresses into chains: the chain with the most members claims its
/// addresses, then the next one among what is left. Strides `1..=MAX_STRIDE`.
#[must_use]
pub fn chains(addrs: &[usize]) -> Vec<Chain> {
    let weighted: Vec<(usize, f64)> = addrs.iter().map(|&a| (a, 1.0)).collect();
    chains_weighted(&weighted)
}

/// [`chains`] with a weight per address (its z-score): the heaviest chain wins
/// its addresses. A table is rarely full in a short run, so up to
/// [`MAX_GAP`] empty slots may sit between two members.
#[must_use]
pub fn chains_weighted(addrs: &[(usize, f64)]) -> Vec<Chain> {
    let mut sorted: Vec<usize> = addrs.iter().map(|&(a, _)| a).collect();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.sort_unstable();
    sorted.dedup();
    let weight = |a: usize| {
        addrs
            .iter()
            .filter(|x| x.0 == a)
            .map(|x| x.1)
            .fold(0.0, f64::max)
    };
    let has = |a: usize| sorted.binary_search(&a).is_ok();
    let mut all: Vec<(Chain, f64, Vec<usize>)> = Vec::new();
    for g in 1..=MAX_STRIDE {
        for &a in &sorted {
            if a >= g && has(a - g) {
                continue; // not a start
            }
            let (mut mem, mut gap) = (vec![a], 0usize);
            // `k` strictly increases, so this ends when the table does.
            for k in 1..=64usize {
                let next = a + k * g;
                if has(next) {
                    mem.push(next);
                    gap = 0;
                } else {
                    gap += 1;
                    if gap > MAX_GAP {
                        break;
                    }
                }
            }
            if mem.len() >= MIN_SLOTS {
                let last = mem[mem.len() - 1];
                let w: f64 = mem.iter().map(|&m| weight(m)).sum();
                all.push((
                    Chain {
                        base: a,
                        stride: g,
                        slots: (last - a) / g + 1,
                        members: mem.len(),
                    },
                    w,
                    mem,
                ));
            }
        }
    }
    all.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(a.0.slots.cmp(&b.0.slots))
            .then(a.0.stride.cmp(&b.0.stride))
            .then(a.0.base.cmp(&b.0.base))
    });
    let mut taken = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (c, _, mem) in all {
        if mem.iter().any(|m| taken.contains(m)) {
            continue;
        }
        taken.extend(mem);
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// stage 3: the other columns

fn col(c: &Chain, base: usize, i: usize) -> usize {
    base + i * c.stride
}

/// The columns that fit in RAM for a chain.
fn bases(pre: &Pre<'_>, c: &Chain) -> impl Iterator<Item = usize> {
    let last = (c.slots - 1) * c.stride;
    let r = pre.r;
    (0..r).filter(move |&b| b + last < r)
}

fn signed(d: u8) -> i32 {
    i32::from(d as i8)
}

/// Frames to look at when scanning every column: an even spread.
fn sample(n: usize) -> Vec<usize> {
    let step = n.div_ceil(SAMPLE_FRAMES).max(1);
    (1..n).step_by(step).collect()
}

/// The y column: for slot-frames where a sprite shares the slot's x, the
/// column whose value gives one constant offset to that sprite's y.
fn find_y(
    pre: &Pre<'_>,
    c: &Chain,
    xs: &[usize],
    lag: usize,
    dx: u8,
) -> Option<(usize, u8, u32, u32)> {
    // (frame, slot, y of a sprite at the slot's x)
    let mut pairs: Vec<(usize, usize, u8)> = Vec::new();
    for f in sample(pre.n()) {
        if f < lag {
            continue;
        }
        let cam = pre.t.frames[f].cam_x as u8;
        for (i, &xa) in xs.iter().enumerate() {
            let sx = pre.ram(f - lag, xa).wrapping_sub(cam).wrapping_add(dx);
            pairs.extend(
                pre.t.frames[f]
                    .sprites
                    .iter()
                    .filter(|s| s.x == sx)
                    .map(|s| (f, i, s.y)),
            );
        }
    }
    if pairs.len() < 30 {
        return None;
    }
    let mut best: Option<(usize, u8, u32)> = None;
    for b in bases(pre, c) {
        if pre.masked[b] || xs.contains(&b) {
            continue;
        }
        let mut hist = [0u32; 256];
        for &(f, i, sy) in &pairs {
            let v = pre.ram(f - lag, col(c, b, i));
            let cam = pre.t.frames[f].cam_y as u8;
            hist[usize::from(sy.wrapping_sub(v).wrapping_add(cam))] += 1;
        }
        let (dy, p) = peak(&hist)?;
        if best.is_none_or(|(_, _, bp)| p > bp) {
            best = Some((b, dy as u8, p));
        }
    }
    let (b, dy, p) = best?;
    let total = pairs.len() as u32;
    (p >= 30 && f64::from(p) >= 0.1 * f64::from(total)).then_some((b, dy, p, total))
}

/// The high byte of a position: steps by one when the low byte wraps.
fn find_hi(pre: &Pre<'_>, c: &Chain, xs: &[usize], lag: usize, skip: &[usize]) -> Option<usize> {
    // (frame, slot, +1 or -1 as u8)
    let mut events: Vec<(usize, usize, u8)> = Vec::new();
    let mut steady = 0u32;
    for f in (lag + 1)..pre.n() {
        for (i, &xa) in xs.iter().enumerate() {
            let (p, q) = (pre.ram(f - lag - 1, xa), pre.ram(f - lag, xa));
            if p >= 224 && q < 32 {
                events.push((f, i, 1));
            } else if p < 32 && q >= 224 {
                events.push((f, i, 255));
            } else {
                steady += 1;
            }
        }
    }
    if events.len() < 4 {
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    for b in bases(pre, c) {
        if pre.masked[b] || skip.contains(&b) || xs.contains(&b) {
            continue;
        }
        let ok = events
            .iter()
            .filter(|&&(f, i, d)| {
                let a = col(c, b, i);
                pre.ram(f - lag, a).wrapping_sub(pre.ram(f - lag - 1, a)) == d
            })
            .count();
        if ok * 10 >= events.len() * 8 && best.is_none_or(|(_, o)| ok > o) {
            // How often it changes when the low byte did not wrap.
            let mut stray = 0u32;
            for f in (lag + 1)..pre.n() {
                for (i, &xa) in xs.iter().enumerate() {
                    let (p, q) = (pre.ram(f - lag - 1, xa), pre.ram(f - lag, xa));
                    let wrap = (p >= 224 && q < 32) || (p < 32 && q >= 224);
                    let a = col(c, b, i);
                    if !wrap && pre.ram(f - lag, a) != pre.ram(f - lag - 1, a) {
                        stray += 1;
                    }
                }
            }
            if f64::from(stray) <= 0.1 * f64::from(steady.max(1)) {
                best = Some((b, ok));
            }
        }
    }
    best.map(|(b, _)| b)
}

// ---------------------------------------------------------------------------
// stage 4: binding

struct Cols {
    c: Chain,
    x: usize,
    y: usize,
    hi: Option<usize>,
    /// The y high byte and the value it has while the object is in the
    /// vertical range of the screen.
    yhi: Option<(usize, u8)>,
    lag: usize,
    dx: i32,
    dy: i32,
}

impl Cols {
    /// Where slot `i` is drawn in frame `f`: `(screen x, screen y)` with the
    /// wrap applied, and whether that is inside the visible picture.
    fn screen(&self, pre: &Pre<'_>, f: usize, i: usize) -> (u8, u8, bool) {
        self.screen_v(pre, f, i, self.lag, 0)
    }

    /// [`Self::screen`] with the RAM taken `rl` frames back and the scroll `cl`.
    fn screen_v(&self, pre: &Pre<'_>, f: usize, i: usize, rl: usize, cl: usize) -> (u8, u8, bool) {
        let fr = &pre.t.frames[f - cl];
        let lo = i64::from(pre.ram(f - rl, col(&self.c, self.x, i)));
        let world = match self.hi {
            Some(h) => lo + 256 * i64::from(pre.ram(f - rl, col(&self.c, h, i))),
            None => lo,
        };
        let wrap = i64::from(pre.t.wrap.max(1));
        let mut sx = (world - i64::from(fr.cam_x) + i64::from(self.dx)).rem_euclid(wrap);
        if sx >= wrap / 2 {
            sx -= wrap;
        }
        let yv = i64::from(pre.ram(f - rl, col(&self.c, self.y, i)));
        let sy = (yv - i64::from(fr.cam_y as u8) + i64::from(self.dy)).rem_euclid(256);
        let high_ok = self
            .yhi
            .is_none_or(|(h, home)| pre.ram(f - rl, col(&self.c, h, i)) == home);
        let on = (0..248).contains(&sx) && sy < 224 && high_ok;
        (sx.rem_euclid(256) as u8, sy as u8, on)
    }

    /// The lowest-numbered sprite drawn at the slot's position. An object is
    /// several 8x8 or 16x16 sprites on an 8-pixel grid, and which of them the
    /// game's own coordinate names depends on the object's size and pose (a
    /// small and a big Mario differ in rows), so a sprite counts when it is in
    /// the object's first or second column and in one of the rows from one
    /// above to three below the coordinate. Rows are exact to the pixel; x
    /// allows [`X_SLACK`] pixels because a game may nudge a sprite pair by a
    /// pixel or two with the facing direction.
    fn bound(&self, pre: &Pre<'_>, f: usize, i: usize) -> Option<u16> {
        let (x, y, _) = self.screen(pre, f, i);
        pre.t.frames[f]
            .sprites
            .iter()
            .filter(|s| {
                let (dx, dy) = (s.x.wrapping_sub(x) as i8, s.y.wrapping_sub(y));
                (dx.abs() <= X_SLACK || (dx - 8).abs() <= X_SLACK)
                    && matches!(dy, 0 | 8 | 16 | 24 | 248)
            })
            .map(|s| s.index)
            .min()
    }
}

/// A byte that is non-zero while a slot is drawn and zero otherwise.
fn find_state(
    pre: &Pre<'_>,
    cols: &Cols,
    bound: &[Vec<Option<u16>>],
    skip: &[usize],
) -> Option<usize> {
    let c = &cols.c;
    let mut best: Option<(usize, f64)> = None;
    for b in bases(pre, c) {
        if pre.masked[b] || skip.contains(&b) {
            continue;
        }
        let (mut hit, mut nb, mut zero, mut nn) = (0u32, 0u32, 0u32, 0u32);
        for (f, row) in bound.iter().enumerate() {
            if f < cols.lag + 1 {
                continue;
            }
            for (i, &b_i) in row.iter().enumerate() {
                let v = pre.ram(f - cols.lag, col(c, b, i));
                if b_i.is_some() {
                    nb += 1;
                    hit += u32::from(v != 0);
                } else {
                    nn += 1;
                    zero += u32::from(v == 0);
                }
            }
        }
        if nb < 30 || nn == 0 {
            continue;
        }
        let recall = f64::from(hit) / f64::from(nb);
        let spec = f64::from(zero) / f64::from(nn);
        let j = recall + spec - 1.0;
        if recall >= 0.95 && j >= 0.25 && best.is_none_or(|(_, bj)| j > bj) {
            best = Some((b, j));
        }
    }
    best.map(|(b, _)| b)
}

/// A byte that holds still while a slot is drawn and takes several values
/// over the run: the type id.
fn find_kind(
    pre: &Pre<'_>,
    cols: &Cols,
    bound: &[Vec<Option<u16>>],
    skip: &[usize],
) -> Option<usize> {
    let c = &cols.c;
    let mut best: Option<(usize, usize)> = None;
    for b in bases(pre, c) {
        if pre.masked[b] || skip.contains(&b) {
            continue;
        }
        let (mut same, mut pairs, mut zero, mut nb) = (0u32, 0u32, 0u32, 0u32);
        let mut seen = [false; 256];
        for f in (cols.lag + 1)..bound.len() {
            for i in 0..c.slots {
                if bound[f][i].is_none() {
                    continue;
                }
                let v = pre.ram(f - cols.lag, col(c, b, i));
                nb += 1;
                zero += u32::from(v == 0);
                seen[usize::from(v)] = true;
                if bound[f - 1][i].is_some() {
                    pairs += 1;
                    same += u32::from(v == pre.ram(f - cols.lag - 1, col(c, b, i)));
                }
            }
        }
        let variety = seen.iter().filter(|&&s| s).count();
        if nb >= 30
            && pairs > 0
            && f64::from(same) >= 0.98 * f64::from(pairs)
            && f64::from(zero) <= 0.5 * f64::from(nb)
            && (3..=128).contains(&variety)
            && best.is_none_or(|(_, bv)| variety > bv)
        {
            best = Some((b, variety));
        }
    }
    best.map(|(b, _)| b)
}

// ---------------------------------------------------------------------------
// the driver of the stages

/// Find the object tables a trace shows. Best first.
#[must_use]
pub fn infer(t: &Trace) -> Vec<Found> {
    let pre = Pre::new(t);
    if pre.n() < 60 || pre.r == 0 {
        return Vec::new();
    }
    // The lag that explains more bytes.
    let by_lag: Vec<Vec<XCand>> = (0..=1).map(|l| score_all_x(&pre, l)).collect();
    let strength = |l: usize| by_lag[l].iter().map(|c| c.z).sum::<f64>();
    let lag = usize::from(strength(1) > strength(0));
    let cands = &by_lag[lag];
    let addrs: Vec<(usize, f64)> = cands.iter().map(|c| (c.addr, c.z)).collect();
    let mut out = Vec::new();
    for ch in chains_weighted(&addrs) {
        if let Some(f) = build(&pre, &ch, cands, lag) {
            out.push(f);
        }
    }
    out.sort_by(|a, b| {
        quality(b)
            .total_cmp(&quality(a))
            .then(a.table.base.cmp(&b.table.base))
    });
    out
}

/// How much to believe a table: the slot-frames it explains, scaled by how
/// well it explains the ones it claims, halved when "live" had to be read off
/// the binding itself (no state byte found).
fn quality(f: &Found) -> f64 {
    let gate = if f.binding.live_from_state { 1.0 } else { 0.5 };
    f.binding.matched as f64 * f.binding.rate() * gate
}

fn build(pre: &Pre<'_>, ch: &Chain, cands: &[XCand], lag: usize) -> Option<Found> {
    let xs: Vec<usize> = (0..ch.slots).map(|i| col(ch, ch.base, i)).collect();
    // dx: the offset most of the chain's members agree on.
    let mut votes = [0u32; 256];
    let mut z = 0f64;
    for c in cands.iter().filter(|c| xs.contains(&c.addr)) {
        votes[usize::from(c.dx)] += c.matches;
        z = z.max(c.z);
    }
    let (dx, _) = votes.iter().enumerate().max_by_key(|&(_, &v)| v)?;
    let dx = dx as u8;
    let yy = find_y(pre, ch, &xs, lag, dx);
    let (yb, dy, _, _) = yy?;
    let hi = find_hi(pre, ch, &xs, lag, &[yb]);
    let ys: Vec<usize> = (0..ch.slots).map(|i| col(ch, yb, i)).collect();
    let mut skip_hi = vec![ch.base];
    skip_hi.extend(hi);
    let yhi = find_hi(pre, ch, &ys, lag, &skip_hi);
    let mut cols = Cols {
        c: ch.clone(),
        x: ch.base,
        y: yb,
        hi,
        yhi: None,
        lag,
        dx: signed(dx),
        dy: signed(dy),
    };
    let n = pre.n();
    let mut bound: Vec<Vec<Option<u16>>> = vec![vec![None; ch.slots]; n];
    for f in (lag + 1)..n {
        for i in 0..ch.slots {
            bound[f][i] = cols.bound(pre, f, i);
        }
    }
    // The y high byte reads, among slots drawn, one value: the screen's own.
    if let Some(yh) = yhi {
        let mut count = [0u32; 256];
        for f in (lag + 1)..n {
            for i in 0..ch.slots {
                if bound[f][i].is_some() {
                    count[usize::from(pre.ram(f - lag, col(ch, yh, i)))] += 1;
                }
            }
        }
        if let Some((home, _)) = count.iter().enumerate().max_by_key(|&(_, &c)| c) {
            cols.yhi = Some((yh, home as u8));
        }
    }
    let mut skip = vec![ch.base, yb];
    skip.extend(hi);
    skip.extend(yhi);
    let state = find_state(pre, &cols, &bound, &skip);
    skip.extend(state);
    let kind = find_kind(pre, &cols, &bound, &skip);

    // Count the match, per slot and per frame.
    let mut b = Binding {
        lag,
        dx: cols.dx,
        dy: cols.dy,
        live_from_state: state.is_some(),
        ..Binding::default()
    };
    let mut per: Vec<SlotBinding> = (0..ch.slots)
        .map(|i| SlotBinding {
            slot: i as u32,
            ..SlotBinding::default()
        })
        .collect();
    let mut idx_count: Vec<std::collections::BTreeMap<u16, u32>> =
        vec![std::collections::BTreeMap::new(); ch.slots];
    for f in (lag + 1)..n {
        let (mut exp_f, mut ok_f) = (0u32, 0u32);
        for i in 0..ch.slots {
            if let Some(ix) = bound[f][i] {
                per[i].bound_frames += 1;
                *idx_count[i].entry(ix).or_insert(0) += 1;
            }
            let live = match state {
                Some(s) => pre.ram(f - lag, col(ch, s, i)) != 0,
                None => bound[f][i].is_some(),
            };
            let (_, _, on) = cols.screen(pre, f, i);
            if live && on {
                per[i].live_frames += 1;
                b.expected += 1;
                exp_f += 1;
                if bound[f][i].is_some() {
                    per[i].matched_frames += 1;
                    b.matched += 1;
                    ok_f += 1;
                }
            }
        }
        if exp_f > 0 {
            b.frames_checked += 1;
            b.frames_clean += u32::from(ok_f == exp_f);
        }
    }
    for (p, m) in per.iter_mut().zip(&idx_count) {
        p.oam_index = m.iter().max_by_key(|&(_, &c)| c).map(|(&k, _)| k);
    }
    // A table claims only the slots that verifiably follow the picture: cut
    // trailing slots that were never live, or matched under `TRIM_BELOW`.
    // (A chain with gaps can run on into a neighbouring array.)
    if state.is_some() {
        let keep = per
            .iter()
            .rposition(|p| {
                p.live_frames >= 20
                    && f64::from(p.matched_frames) >= TRIM_BELOW * f64::from(p.live_frames)
            })
            .map_or(0, |i| i + 1);
        if keep >= MIN_SLOTS && keep < ch.slots {
            let shorter = Chain {
                slots: keep,
                members: ch.members.min(keep),
                ..ch.clone()
            };
            return build(pre, &shorter, cands, lag);
        }
    }
    b.slots = per;

    let fields = [Some(ch.base), Some(yb), hi, yhi, state, kind];
    let lowest = fields.iter().flatten().copied().min()?;
    let off = |a: Option<usize>| a.map(|a| (a - lowest) as u32);
    let base = t_addr(pre, lowest);
    Some(Found {
        table: ObjectTable {
            name: format!("objects_{base:x}"),
            base,
            slots: ch.slots as u32,
            stride: ch.stride as u32,
            x_lo: off(Some(ch.base)),
            x_hi: off(hi),
            y: off(Some(yb)),
            y_hi: off(yhi),
            kind: off(kind),
            state: off(state),
            source: String::new(),
            grade: Grade::Candidate,
        },
        binding: b,
        z,
    })
}

fn t_addr(pre: &Pre<'_>, idx: usize) -> u32 {
    pre.t.ram_base + idx as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator (the tests need no `rand`).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }
    }

    const SLOTS: usize = 6;
    const X: usize = 0x40;
    const HI: usize = 0x50;
    const Y: usize = 0x60;
    const ST: usize = 0x70;
    const KIND: usize = 0x80;

    /// A game with `SLOTS` objects in columns, a scrolling camera, and a
    /// 16-wide object drawn as two sprites; the OAM shadow lives at `0x100`.
    /// `record` lays the same fields out as stride-`rec` records instead.
    fn game(frames: usize, shadow: bool, lag: usize) -> Trace {
        let mut rng = Lcg(7);
        let mut world = [0u32; SLOTS];
        let mut yv = [0u8; SLOTS];
        let mut kind = [0u8; SLOTS];
        let mut live = [false; SLOTS];
        let mut cam = 0u32;
        let mut hist: Vec<Frame> = Vec::new();
        for f in 0..frames {
            cam += 1 + (f as u32 / 40) % 3;
            for i in 0..SLOTS {
                if !live[i] && rng.below(30) == 0 {
                    live[i] = true;
                    world[i] = cam + 20 + rng.below(220);
                    yv[i] = 40 + rng.below(150) as u8;
                    kind[i] = 1 + rng.below(9) as u8;
                } else if live[i] {
                    world[i] = world[i].wrapping_sub(rng.below(3));
                    if rng.below(8) == 0 {
                        yv[i] = yv[i].wrapping_add(rng.below(5) as u8).wrapping_sub(2);
                    }
                    if world[i] + 16 <= cam || rng.below(150) == 0 {
                        live[i] = false;
                    }
                }
            }
            let mut ram = vec![0u8; 0x400];
            let mut oam = vec![0u8; 256];
            let mut sprites = Vec::new();
            let mut k = 0usize;
            for i in 0..SLOTS {
                if !live[i] {
                    ram[X + i] = world[i] as u8; // stale position
                    ram[HI + i] = (world[i] >> 8) as u8;
                    ram[Y + i] = yv[i];
                    continue;
                }
                ram[X + i] = world[i] as u8;
                ram[HI + i] = (world[i] >> 8) as u8;
                ram[Y + i] = yv[i];
                ram[ST + i] = 1;
                ram[KIND + i] = kind[i];
                let sx = world[i] as i64 - cam as i64;
                if (-8..256).contains(&sx) {
                    for part in 0..2i64 {
                        let x = sx + 8 * part;
                        if (0..256).contains(&x) && k < 60 {
                            // dx = 0; the sprite's y is the byte plus 1
                            oam[k * 4] = yv[i].wrapping_add(1);
                            oam[k * 4 + 3] = x as u8;
                            sprites.push(Sprite {
                                index: k as u16,
                                x: x as u8,
                                y: yv[i].wrapping_add(1),
                            });
                            k += 1;
                        }
                    }
                }
            }
            if shadow {
                ram[0x100..0x200].copy_from_slice(&oam);
            }
            hist.push(Frame {
                ram,
                oam,
                sprites,
                cam_x: (cam & 0x1FF) as u16,
                cam_y: 0,
            });
        }
        // Sprites lag the RAM by `lag`: shift RAM later.
        let mut out = hist.clone();
        for f in 0..frames {
            out[f].ram = hist[(f + lag).min(frames - 1)].ram.clone();
        }
        // The OAM of frame f was built from RAM of frame f - lag, so keep the
        // shadow in step: the mask test runs on this frame's RAM and OAM.
        Trace {
            ram_base: 0,
            wrap: 512,
            frames: out,
        }
    }

    #[test]
    fn chains_finds_stride_and_slot_count() {
        let a: Vec<usize> = (0..6).map(|i| 0x40 + i * 4).collect();
        let c = chains(&a);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].base, c[0].stride, c[0].slots), (0x40, 4, 6));
        // A hole of one slot stays inside the table.
        let c = chains(&[0x10, 0x11, 0x13, 0x14]);
        assert_eq!((c[0].base, c[0].stride, c[0].slots), (0x10, 1, 5));
        // Two members are not a table.
        assert!(chains(&[0x10, 0x20]).is_empty());
    }

    #[test]
    fn chains_prefers_the_denser_stride() {
        let a: Vec<usize> = (0x20..0x28).collect();
        let c = chains(&a);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].stride, c[0].slots), (1, 8));
    }

    #[test]
    fn finds_the_columns_of_a_synthetic_game() {
        let t = game(900, false, 0);
        let found = infer(&t);
        assert!(!found.is_empty(), "no table");
        let f = &found[0];
        let tb = &f.table;
        assert_eq!(tb.slots as usize, SLOTS);
        assert_eq!(tb.stride, 1);
        let at = |o: Option<u32>| o.map(|o| tb.base + o);
        assert_eq!(at(tb.x_lo), Some(X as u32));
        assert_eq!(at(tb.y), Some(Y as u32));
        assert_eq!(at(tb.x_hi), Some(HI as u32));
        assert_eq!(at(tb.state), Some(ST as u32));
        assert_eq!(at(tb.kind), Some(KIND as u32));
        assert_eq!(f.binding.lag, 0);
        assert_eq!(f.binding.dx, 0);
        assert!(f.binding.expected > 200, "{:?}", f.binding);
        assert!(f.binding.rate() > 0.99, "{:?}", f.binding);
        assert_eq!(grade_of(f), Grade::Observed);
    }

    #[test]
    fn the_oam_shadow_is_not_a_table() {
        let t = game(900, true, 0);
        let found = infer(&t);
        for f in &found {
            // Whatever is found, none of it is the 0x100 page.
            let lo = f.table.base;
            assert!(!(0x100..0x200).contains(&lo), "{lo:#x}");
        }
        assert_eq!(found[0].table.base as usize, X);
    }

    #[test]
    fn a_frame_of_lag_is_measured() {
        let t = game(900, false, 1);
        let f = &infer(&t)[0];
        assert_eq!(f.binding.lag, 1);
        assert!(f.binding.rate() > 0.99, "{:?}", f.binding);
    }

    #[test]
    fn noise_alone_has_no_table() {
        let mut rng = Lcg(99);
        let frames = (0..400)
            .map(|_| {
                let sprites = (0..20)
                    .map(|k| Sprite {
                        index: k,
                        x: rng.below(256) as u8,
                        y: rng.below(200) as u8,
                    })
                    .collect();
                Frame {
                    ram: (0..0x200).map(|_| rng.below(256) as u8).collect(),
                    oam: Vec::new(),
                    sprites,
                    cam_x: 0,
                    cam_y: 0,
                }
            })
            .collect();
        let t = Trace {
            ram_base: 0,
            wrap: 512,
            frames,
        };
        assert!(infer(&t).is_empty());
    }

    #[test]
    fn rows_carry_the_evidence() {
        let t = game(900, false, 0);
        let f = &infer(&t)[0];
        let row = to_row(f, grade_of(f), "; synthetic");
        assert!(row.source.starts_with("finder: 6 slots stride 1"));
        assert_eq!(row.grade, Grade::Observed);
    }
}
