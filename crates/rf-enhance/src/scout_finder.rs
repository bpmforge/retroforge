//! Find a game's scout settings by watching it played (ticket W33-07,
//! `docs/design/STITCHING_PLAN.md` §3.1).
//!
//! The level scout (`retroforge::scout`) needs a handful of facts about a
//! game: which RAM words are the camera and the player's X, a value for the
//! player's Y that keeps the scout's player out of harm's way, and a byte that
//! says "a level is running". The first three profiles got theirs from one
//! research session of RAM probing each. [`ScoutFinder`] gets them from play:
//! it is fed copies of the work RAM, the pad and the scroll registers, one
//! frame at a time, and never touches a core. Everything it reports is an
//! observation (the rows carry `source = "observed: auto (W33-07)"`).
//!
//! * **Camera.** [`crate::camera_finder::CameraFinder`], unchanged: the byte
//!   that holds the low byte of the scroll register while the screen moves,
//!   and the carry check for its high byte.
//! * **Player X.** A byte (the word's low half) that
//!   1. steps by the camera's step on at least 85 % of the frames the camera
//!      moves, and
//!   2. goes UP while Right is held and DOWN while Left is held, and
//!   3. **keeps changing on frames the camera does not move while a direction
//!      is held** (the player walking towards a wall the camera stops at).
//!
//!   The third test is the discriminator against camera mirrors: a copy of
//!   the scroll steps like the camera whenever the camera does, and sits still
//!   whenever it does. The high half is the byte that steps +1 / -1 in the
//!   frames the low half wraps (adjacent or not: Super Mario Bros. 3 keeps it
//!   a page away).
//! * **Player Y.** A byte that deviates by 6..=110 within 40 frames of a jump
//!   button press made while standing, comes back, and does NOT do so in
//!   control windows opened at random while the pad is idle (timers, animation
//!   counters).
//! * **Start gate.** A byte that holds one value on at least 98 % of the
//!   frames of the level (the scene with the most scrolling frames) and holds
//!   a different value on the frames of every other scene (title, map, fades).
//!
//! The hold value for Y and the choice between the candidates are NOT made
//! here: they need a machine to try them on (`retroforge::scout_discovery`).
//!
//! # Bounded memory
//!
//! No per-frame history. Each offset carries a fixed handful of counters, and
//! the gate keeps one 256-bin histogram per offset for each of the (at most
//! [`MAX_SCENES`]) scenes it has seen.

use std::collections::HashMap;

use crate::camera_finder::{CameraFinder, Found};

/// The horizontal direction the pad held on a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pad {
    #[default]
    Idle,
    Left,
    Right,
}

/// One frame of play.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Work RAM at the frame's end.
    pub ram: &'a [u8],
    /// The horizontal pad direction held this frame.
    pub pad: Pad,
    /// The face buttons held this frame, one bit each (any of them might be
    /// the jump button; a run button held all the time must not hide the
    /// press of another).
    pub jump: u8,
    /// Per background layer, in preference order: every low byte written to
    /// its x and y scroll registers this frame ([`CameraFinder::observe`]).
    pub scroll: &'a [(&'a [u8], &'a [u8])],
    /// The canvas model's scene for the frame; `None` while a cut settles.
    pub scene: Option<u64>,
}

/// A camera step bigger than this is a cut, not scrolling.
const MAX_STEP: i16 = 16;
/// Camera-moving frames the player finder wants before it names anyone.
pub const MIN_PLAYER_MOVING: u32 = 90;
/// ...and the share of them a player word must match.
pub const MIN_PLAYER_STEP: f32 = 0.85;
/// Frames a player word must keep changing, in the direction held, while the
/// camera stood still.
pub const MIN_WALL: u32 = 6;
/// Frames a Y window watches after a press (a hop is about 40).
const Y_WINDOW: u32 = 48;
/// A Y byte starts to move within this many frames of the jump press.
const FIRST_DEVIATION: u8 = 6;
/// A standing player's Y has held its value this long before a jump.
const QUIET_FRAMES: u8 = 8;
/// A control window is opened at most this often.
const CONTROL_EVERY: u64 = 200;
/// Jump windows needed before a Y byte is named.
pub const MIN_JUMPS: u32 = 2;
/// Jump windows seen before the finder calls itself ready (more evidence,
/// fewer wrong Y candidates to try).
pub const MIN_JUMP_WINDOWS: u32 = 6;
/// Scenes whose frames the gate keeps a histogram of.
pub const MAX_SCENES: usize = 6;
/// Frames of the level the gate wants.
pub const MIN_LEVEL_FRAMES: u32 = 300;
/// Frames outside the level the gate wants.
pub const MIN_OTHER_FRAMES: u32 = 60;
/// How many candidates of each kind are kept.
pub const KEEP: usize = 4;

/// A player X word.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerCandidate {
    /// Offset of the low byte.
    pub lo: usize,
    /// Offset of the high byte, when a byte stepped with the low one's wraps.
    pub hi: Option<usize>,
    /// Share of camera-moving frames the byte matched.
    pub step: f32,
    /// Frames it kept moving, in the direction held, with the camera still.
    pub wall: u32,
    pub moving: u32,
    /// 0..=1.
    pub confidence: f32,
}

/// A player Y byte.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct YCandidate {
    pub offset: usize,
    /// Its value standing.
    pub standing: u8,
    /// `-1` when a jump makes the value smaller (screen y grows downwards).
    pub up: i8,
    pub hits: u32,
    pub windows: u32,
    pub confidence: f32,
}

/// A game-mode byte.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateCandidate {
    pub offset: usize,
    /// What it reads in the level.
    pub value: u8,
    /// Share of the level's frames it read `value` on.
    pub constancy: f32,
    /// Share of the other frames it read something else.
    pub differs: f32,
    pub confidence: f32,
}

/// Everything the session has shown so far.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Observed {
    pub camera: Option<Found>,
    pub players: Vec<PlayerCandidate>,
    pub ys: Vec<YCandidate>,
    pub gates: Vec<GateCandidate>,
    /// Jumps the Y evidence comes from.
    pub jump_windows: u32,
}

impl Observed {
    /// Whether the observations are complete enough to try the candidates on
    /// a machine: a camera, a player word whose high half is known, a Y.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.camera.is_some()
            && self.players.first().is_some_and(|p| p.hi.is_some())
            && !self.ys.is_empty()
            && self.jump_windows >= MIN_JUMP_WINDOWS
    }
}

#[derive(Debug, Clone)]
struct Window {
    jump: bool,
    frames: u32,
    base: Vec<u8>,
    maxdev: Vec<u8>,
    sign: Vec<i8>,
    /// The byte came back to within 3 of its base after deviating by 6+.
    returned: Vec<bool>,
    /// The window frame a byte first deviated by 2 or more (`u8::MAX`: never).
    first: Vec<u8>,
    /// The byte had not changed for 8 frames when the window opened.
    quiet: Vec<bool>,
}

/// One scene's histograms of every offset's value.
#[derive(Debug, Clone)]
struct SceneHist {
    frames: u32,
    /// `len * 256` counters.
    bins: Vec<u16>,
}

/// Most (low, high) pairs kept: a counter that wraps while a dozen others
/// tick is a pair per tick, and a long session must not grow without bound.
const MAX_PAIRS: usize = 200_000;

/// The finder. Feed it frames with [`ScoutFinder::observe`], read
/// [`ScoutFinder::observed`].
#[derive(Debug, Clone)]
pub struct ScoutFinder {
    len: usize,
    camera: CameraFinder,
    frames: u64,
    prev: Vec<u8>,
    last_delta: Vec<i8>,
    /// Frames each byte has held its value (capped).
    quiet_run: Vec<u8>,
    prev_scroll: Vec<Option<u8>>,
    prev_pad: Pad,
    prev_jump: u8,
    // player X
    moving: u32,
    right_frames: u32,
    left_frames: u32,
    step: Vec<[u32; 2]>,
    right_up: Vec<u32>,
    left_down: Vec<u32>,
    wrong: Vec<u32>,
    wall: Vec<u32>,
    changes: Vec<u32>,
    // the high half of the provisional best low byte
    /// Per byte, how often it wrapped forward / back (to or from the top of
    /// its range), and the frames a byte stepped by exactly +1 / -1 in the same
    /// frame as such a wrap, per `(wrapping byte, stepping byte)`.
    wrap_fwd: Vec<u32>,
    wrap_back: Vec<u32>,
    pair_fwd: HashMap<(u16, u16), u32>,
    pair_back: HashMap<(u16, u16), u32>,
    // player Y
    window: Option<Window>,
    last_control: u64,
    jump_windows: u32,
    control_windows: u32,
    jump_hit: Vec<u32>,
    /// Jump windows the byte was quiet at the opening of (the ones that could
    /// have shown it).
    jump_quiet: Vec<u32>,
    /// Jumps after which the byte came back to its value.
    jump_back: Vec<u32>,
    control_hit: Vec<u32>,
    up_sum: Vec<i32>,
    standing: Vec<u8>,
    // gate
    scenes: HashMap<Option<u64>, SceneHist>,
    scene_moving: HashMap<Option<u64>, u32>,
}

impl ScoutFinder {
    /// A finder over `len` bytes of work RAM and `layers` background layers.
    #[must_use]
    pub fn new(len: usize, layers: usize) -> Self {
        Self {
            len,
            camera: CameraFinder::new(len, layers),
            frames: 0,
            prev: Vec::new(),
            last_delta: vec![0; len],
            quiet_run: vec![0; len],
            prev_scroll: vec![None; layers.max(1)],
            prev_pad: Pad::Idle,
            prev_jump: 0,
            moving: 0,
            right_frames: 0,
            left_frames: 0,
            step: vec![[0; 2]; len],
            right_up: vec![0; len],
            left_down: vec![0; len],
            wrong: vec![0; len],
            wall: vec![0; len],
            changes: vec![0; len],
            wrap_fwd: vec![0; len],
            wrap_back: vec![0; len],
            pair_fwd: HashMap::new(),
            pair_back: HashMap::new(),
            window: None,
            last_control: 0,
            jump_windows: 0,
            control_windows: 0,
            jump_hit: vec![0; len],
            jump_quiet: vec![0; len],
            jump_back: vec![0; len],
            control_hit: vec![0; len],
            up_sum: vec![0; len],
            standing: vec![0; len],
            scenes: HashMap::new(),
            scene_moving: HashMap::new(),
        }
    }

    /// Frames fed so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// Frames the camera moved (progress for a UI).
    #[must_use]
    pub const fn moving(&self) -> u32 {
        self.moving
    }

    /// Feed one frame.
    pub fn observe(&mut self, f: &Frame<'_>) {
        let n = self.len.min(f.ram.len());
        self.frames += 1;
        // The camera finder arms on the first horizontal press: the menus
        // before it hold Start and A, and a title that scrolls in would dilute
        // the game's camera.
        let armed = self.camera.armed() || f.pad != Pad::Idle;
        self.camera.observe(f.scroll, f.ram, armed);
        let cd = self.camera_delta(f.scroll);
        if self.prev.is_empty() {
            self.prev = f.ram[..n].to_vec();
            self.prev_pad = f.pad;
            self.prev_jump = f.jump;
            return;
        }
        self.gate_frame(f, n, cd);
        if armed {
            self.player_frame(f, n, cd);
        }
        self.y_frame(f, n);
        for ((d, q), (&now, &before)) in self
            .last_delta
            .iter_mut()
            .zip(self.quiet_run.iter_mut())
            .zip(f.ram.iter().zip(&self.prev))
        {
            *d = now.wrapping_sub(before) as i8;
            *q = if *d == 0 { q.saturating_add(1) } else { 0 };
        }
        self.prev.clear();
        self.prev.extend_from_slice(&f.ram[..n]);
        self.prev_pad = f.pad;
        self.prev_jump = f.jump;
    }

    /// The signed change of the first layer whose x scroll byte changed this
    /// frame (0 when none did).
    fn camera_delta(&mut self, scroll: &[(&[u8], &[u8])]) -> i16 {
        let mut out = 0i16;
        for (prev, (xs, _)) in self.prev_scroll.iter_mut().zip(scroll) {
            let Some(&now) = xs.last() else { continue };
            if let Some(before) = prev.replace(now) {
                let d = i16::from(now.wrapping_sub(before) as i8);
                if out == 0 && d != 0 {
                    out = d;
                }
            }
        }
        out
    }

    fn player_frame(&mut self, f: &Frame<'_>, n: usize, cd: i16) {
        let right_now = f.pad == Pad::Right;
        let left_now = f.pad == Pad::Left;
        // A press can take a frame to reach the game's variables.
        let right = right_now || self.prev_pad == Pad::Right;
        let left = left_now || self.prev_pad == Pad::Left;
        self.right_frames += u32::from(right_now);
        self.left_frames += u32::from(left_now);
        let cam_moved = cd != 0 && cd.abs() <= MAX_STEP;
        self.moving += u32::from(cam_moved);
        let wall_frame = (right || left) && cd == 0;
        let steady_right = right_now && self.prev_pad == Pad::Right;
        let steady_left = left_now && self.prev_pad == Pad::Left;
        for b in 0..n {
            let d = i16::from(f.ram[b].wrapping_sub(self.prev[b]) as i8);
            let pd = i16::from(self.last_delta[b]);
            if d != 0 {
                self.changes[b] += 1;
            }
            if cam_moved {
                self.step[b][0] += u32::from(d == cd);
                self.step[b][1] += u32::from(pd == cd);
            }
            if d == 0 || d.abs() > MAX_STEP {
                continue;
            }
            if right && d > 0 {
                self.right_up[b] += 1;
            }
            if left && d < 0 {
                self.left_down[b] += 1;
            }
            if (steady_right && d < 0) || (steady_left && d > 0) {
                self.wrong[b] += 1;
            }
            if wall_frame && ((right && d > 0) || (left && d < 0)) {
                self.wall[b] += 1;
            }
        }
        self.wrap_pairs(f, n);
    }

    /// Count the wraps of every byte and the bytes that stepped with them.
    fn wrap_pairs(&mut self, f: &Frame<'_>, n: usize) {
        let (mut up, mut down, mut wrapped_fwd, mut wrapped_back) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for b in 0..n {
            let (before, now) = (self.prev[b], f.ram[b]);
            match now.wrapping_sub(before) {
                1 => up.push(b as u16),
                0xFF => down.push(b as u16),
                _ => {}
            }
            if before >= 0xC0 && now < 0x40 {
                wrapped_fwd.push(b);
            } else if before < 0x40 && now >= 0xC0 {
                wrapped_back.push(b);
            }
        }
        for (wrapped, stepped, wraps, pairs) in [
            (&wrapped_fwd, &up, &mut self.wrap_fwd, &mut self.pair_fwd),
            (
                &wrapped_back,
                &down,
                &mut self.wrap_back,
                &mut self.pair_back,
            ),
        ] {
            for &lo in wrapped {
                wraps[lo] += 1;
                for &hi in stepped.iter().filter(|&&h| usize::from(h) != lo) {
                    let key = (lo as u16, hi);
                    if let Some(c) = pairs.get_mut(&key) {
                        *c += 1;
                    } else if pairs.len() < MAX_PAIRS {
                        pairs.insert(key, 1);
                    }
                }
            }
        }
    }

    /// Low-byte candidates `(offset, step ratio, wall)`, best first, among
    /// those that clear the bar with `min_moving` camera-moving frames.
    fn low_candidates(&self, min_moving: u32) -> Vec<(usize, f32, u32)> {
        self.low_candidates_with(min_moving, true)
    }

    /// [`Self::low_candidates`], optionally without the wall test.
    fn low_candidates_with(&self, min_moving: u32, wall_test: bool) -> Vec<(usize, f32, u32)> {
        if self.moving < min_moving {
            return Vec::new();
        }
        let dir_frames = self.right_frames + self.left_frames;
        let min_wall = if min_moving < MIN_PLAYER_MOVING {
            2
        } else {
            MIN_WALL
        };
        let mut out: Vec<(usize, f32, u32)> = (0..self.len)
            .filter_map(|b| {
                let steps = self.step[b][0].max(self.step[b][1]);
                #[allow(clippy::cast_precision_loss)]
                let ratio = steps as f32 / self.moving as f32;
                let ok = ratio >= MIN_PLAYER_STEP
                    && (!wall_test || self.wall[b] >= min_wall)
                    && self.right_up[b] >= 10
                    && (self.left_frames < 30 || self.left_down[b] >= 2)
                    && self.wrong[b] * 8 <= dir_frames;
                ok.then_some((b, ratio, self.wall[b]))
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
        out
    }

    /// The high half of the word whose low half is `lo`, from the bytes that
    /// stepped when it wrapped.
    fn high_half(&self, lo: usize) -> Option<usize> {
        let (wf, wb) = (self.wrap_fwd[lo], self.wrap_back[lo]);
        if wf == 0 {
            return None;
        }
        let total = wf + wb;
        let mut best: Option<(usize, u32)> = None;
        let candidates = self
            .pair_fwd
            .iter()
            .filter(|((l, _), _)| usize::from(*l) == lo)
            .map(|((_, h), &c)| (usize::from(*h), c));
        for (hi, fwd) in candidates {
            let back = self
                .pair_back
                .get(&(lo as u16, hi as u16))
                .copied()
                .unwrap_or(0);
            let fwd_ok = fwd * 10 >= wf * 9;
            let back_ok = wb == 0 || back * 10 >= wb * 9;
            // A byte that steps only when the low half wraps changes about as
            // often as the low half wraps; a free-running counter much more.
            // (Allow a tenth of the session for the frames the game moved it
            // for its own reasons: a death, a map.)
            let still = u64::from(self.changes[hi]) <= u64::from(4 * total + 16) + self.frames / 10;
            if !(fwd_ok && back_ok && still) {
                continue;
            }
            let score = fwd + back;
            let better = match best {
                None => true,
                // Ties go to the byte next to the low half, then the lowest.
                Some((cur, s)) => {
                    let adjacent_cur = cur == lo + 1;
                    score > s || (score == s && (hi == lo + 1 || (!adjacent_cur && hi < cur)))
                }
            };
            if better {
                best = Some((hi, score));
            }
        }
        best.map(|(b, _)| b)
    }

    fn y_frame(&mut self, f: &Frame<'_>, n: usize) {
        let press = f.jump & !self.prev_jump != 0;
        if press && self.window.as_ref().is_some_and(|w| !w.jump) {
            // A control window that sees a jump is not a control: the jump
            // gets the window.
            self.window = None;
        }
        if self.window.is_some() {
            self.window_frame(f, n);
            return;
        }
        // Windows count only once the session is under way (the menus before
        // the first direction press are full of A and Start presses that
        // jump nothing).
        if !self.camera.armed() {
            return;
        }
        // A jump is a window whether the player stood or walked: what makes
        // a byte the Y is that it held still before the press, moved at once
        // and came back.
        let open_jump = press;
        let open_control = !press
            && self.frames.saturating_sub(self.last_control) >= CONTROL_EVERY
            && self.frames > CONTROL_EVERY / 2;
        if open_jump || open_control {
            if open_control {
                self.last_control = self.frames;
            }
            self.window = Some(Window {
                jump: open_jump,
                frames: 0,
                base: self.prev[..n].to_vec(),
                maxdev: vec![0; n],
                sign: vec![0; n],
                returned: vec![false; n],
                first: vec![u8::MAX; n],
                quiet: self.quiet_run[..n]
                    .iter()
                    .map(|&q| q >= QUIET_FRAMES)
                    .collect(),
            });
        }
    }

    /// One more frame of the open window.
    fn window_frame(&mut self, f: &Frame<'_>, n: usize) {
        let Some(w) = self.window.as_mut() else {
            return;
        };
        w.frames += 1;
        for b in 0..n.min(w.base.len()) {
            let d = f.ram[b].wrapping_sub(w.base[b]) as i8;
            let dev = d.unsigned_abs().min(127);
            if dev > w.maxdev[b] {
                if w.maxdev[b] == 0 {
                    w.sign[b] = d.signum();
                }
                w.maxdev[b] = dev;
            }
            if dev >= 2 && w.first[b] == u8::MAX {
                w.first[b] = w.frames.min(254) as u8;
            }
            if w.maxdev[b] >= 6 && dev <= 3 {
                w.returned[b] = true;
            }
        }
        if w.frames >= Y_WINDOW {
            if let Some(w) = self.window.take() {
                self.close_window(&w, n);
            }
        }
    }

    fn close_window(&mut self, w: &Window, n: usize) {
        if w.jump {
            self.jump_windows += 1;
        } else {
            self.control_windows += 1;
        }
        for b in 0..n.min(w.base.len()) {
            // A jump starts the frame it is pressed: a byte that wandered off
            // later on its own (a timer, an enemy) is not the player's Y. It
            // may come back to the same value only when the player lands at
            // the height he left, which a walking player often does not.
            if w.jump && w.quiet[b] {
                self.jump_quiet[b] += 1;
            }
            let moved = (6..=127).contains(&w.maxdev[b])
                && w.quiet[b]
                && (!w.jump || w.first[b] <= FIRST_DEVIATION);
            if !moved {
                continue;
            }
            if w.jump {
                self.jump_hit[b] += 1;
                self.jump_back[b] += u32::from(w.returned[b]);
                self.up_sum[b] += i32::from(w.sign[b]);
                if w.returned[b] {
                    self.standing[b] = w.base[b];
                }
            } else {
                self.control_hit[b] += 1;
            }
        }
    }

    fn gate_frame(&mut self, f: &Frame<'_>, n: usize, cd: i16) {
        let key = f.scene;
        if cd != 0 && cd.abs() <= MAX_STEP {
            *self.scene_moving.entry(key).or_insert(0) += 1;
        }
        // The shared "other" histogram takes any scene beyond the cap.
        let key = if self.scenes.contains_key(&key) || self.scenes.len() < MAX_SCENES {
            key
        } else {
            Some(u64::MAX - 1)
        };
        let len = self.len;
        let hist = self.scenes.entry(key).or_insert_with(|| SceneHist {
            frames: 0,
            bins: vec![0; len * 256],
        });
        hist.frames += 1;
        for (b, &v) in f.ram[..n].iter().enumerate() {
            let slot = &mut hist.bins[b * 256 + usize::from(v)];
            *slot = slot.saturating_add(1);
        }
    }

    /// The wrap counts of an offset as text (diagnostics for the discovery
    /// tests).
    #[must_use]
    pub fn probe_slots(&self, b: usize) -> String {
        let best = self
            .pair_fwd
            .iter()
            .filter(|((l, _), _)| usize::from(*l) == b)
            .max_by_key(|(_, c)| **c)
            .map(|((_, h), c)| format!("best +1 partner {h:#x} x{c}"))
            .unwrap_or_default();
        format!(
            "wraps of {b:#x}: +{} -{} {best}",
            self.wrap_fwd[b], self.wrap_back[b]
        )
    }

    /// One offset's counters as text (diagnostics for the discovery tests).
    #[must_use]
    pub fn probe(&self, b: usize) -> String {
        format!(
            "off {b:#06x}: moving {} step {:?} right_up {} left_down {} wrong {} wall {} changes {} | \
             jump_hit {}/{} control_hit {}/{} | frames {} camera moving {}",
            self.moving,
            self.step[b],
            self.right_up[b],
            self.left_down[b],
            self.wrong[b],
            self.wall[b],
            self.changes[b],
            self.jump_hit[b],
            self.jump_windows,
            self.control_hit[b],
            self.control_windows,
            self.frames,
            self.camera.moving(),
        )
    }

    /// Bytes that held the scroll register's low byte on at least 70 % of the
    /// frames the screen moved: the camera and its mirrors, by the camera
    /// finder's own evidence (for measuring the player finder's false
    /// positives).
    #[must_use]
    pub fn camera_mirrors(&self) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for (ax, _) in &self.camera.layers {
            let moving = ax.moving();
            if moving == 0 {
                continue;
            }
            for (offset, hits, ..) in ax.best(64) {
                #[allow(clippy::cast_precision_loss)]
                if hits as f32 / moving as f32 >= 0.70 && !out.contains(&offset) {
                    out.push(offset);
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// The offsets that pass the step and direction tests with the wall test
    /// left out (what the wall test has to remove).
    #[must_use]
    pub fn players_without_wall_test(&self) -> Vec<usize> {
        self.low_candidates_with(MIN_PLAYER_MOVING, false)
            .into_iter()
            .map(|c| c.0)
            .collect()
    }

    /// What the session has shown so far.
    #[must_use]
    pub fn observed(&self) -> Observed {
        Observed {
            camera: self.camera.verdict(),
            players: self.players(),
            ys: self.ys(),
            gates: self.gates(),
            jump_windows: self.jump_windows,
        }
    }

    fn players(&self) -> Vec<PlayerCandidate> {
        let lows = self.low_candidates(MIN_PLAYER_MOVING);
        lows.into_iter()
            .take(KEEP)
            .map(|(lo, step, wall)| {
                let hi = self.high_half(lo);
                #[allow(clippy::cast_precision_loss)]
                let confidence = step.min(1.0)
                    * (wall as f32 / (wall as f32 + 10.0)).min(1.0)
                    * if hi.is_some() { 1.0 } else { 0.6 };
                PlayerCandidate {
                    lo,
                    hi,
                    step,
                    wall,
                    moving: self.moving,
                    confidence,
                }
            })
            .collect()
    }

    fn ys(&self) -> Vec<YCandidate> {
        if self.jump_windows < MIN_JUMPS {
            return Vec::new();
        }
        let mut out: Vec<YCandidate> = (0..self.len)
            .filter(|&b| {
                self.jump_hit[b] >= MIN_JUMPS
                    && self.jump_hit[b] * 10 >= self.jump_quiet[b] * 5
                    && self.up_sum[b].unsigned_abs() * 10 >= self.jump_hit[b] * 8
                    && self.jump_back[b] >= 1
                    && self.control_hit[b] * 5 <= self.control_windows
            })
            .map(|b| {
                #[allow(clippy::cast_precision_loss)]
                let confidence = self.jump_hit[b] as f32 / self.jump_quiet[b].max(1) as f32;
                YCandidate {
                    offset: b,
                    standing: self.standing[b],
                    up: if self.up_sum[b] >= 0 { 1 } else { -1 },
                    hits: self.jump_hit[b],
                    windows: self.jump_quiet[b],
                    confidence,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.hits
                .cmp(&a.hits)
                .then(self.jump_back[b.offset].cmp(&self.jump_back[a.offset]))
                .then(a.offset.cmp(&b.offset))
        });
        out.truncate(4 * KEEP);
        out
    }

    fn gates(&self) -> Vec<GateCandidate> {
        let Some((level_key, _)) = self
            .scene_moving
            .iter()
            .filter(|(k, _)| k.is_some())
            .max_by_key(|(k, v)| (**v, std::cmp::Reverse(k.unwrap_or(0))))
        else {
            return Vec::new();
        };
        let Some(level) = self.scenes.get(level_key) else {
            return Vec::new();
        };
        let other_frames: u32 = self
            .scenes
            .iter()
            .filter(|(k, _)| *k != level_key)
            .map(|(_, h)| h.frames)
            .sum();
        if level.frames < MIN_LEVEL_FRAMES || other_frames < MIN_OTHER_FRAMES {
            return Vec::new();
        }
        let mut out = Vec::new();
        for b in 0..self.len {
            let bins = &level.bins[b * 256..b * 256 + 256];
            let Some((mode, &count)) = bins.iter().enumerate().max_by_key(|(v, c)| (**c, *v))
            else {
                continue;
            };
            #[allow(clippy::cast_precision_loss)]
            let constancy = f32::from(count) / level.frames as f32;
            if constancy < 0.98 {
                continue;
            }
            let same: u32 = self
                .scenes
                .iter()
                .filter(|(k, _)| *k != level_key)
                .map(|(_, h)| u32::from(h.bins[b * 256 + mode]))
                .sum();
            #[allow(clippy::cast_precision_loss)]
            let differs = 1.0 - same as f32 / other_frames as f32;
            if differs < 0.6 {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            out.push(GateCandidate {
                offset: b,
                value: mode as u8,
                constancy,
                differs,
                confidence: constancy * differs,
            });
        }
        out.sort_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then(a.offset.cmp(&b.offset))
        });
        out.truncate(KEEP);
        out
    }
}

/// What a skeleton row is for (the profile's `[[scout.addr]]` roles).
pub type Role = &'static str;

/// One `[[scout.addr]]` row of the skeleton.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub role: Role,
    pub addr: u32,
    pub value: Option<u8>,
    pub notes: String,
    pub confidence: f32,
}

/// The `source` every discovered row carries.
pub const SOURCE: &str = "observed: auto (W33-07)";

/// A `[scout]` table found by observation.
#[derive(Debug, Clone, PartialEq)]
pub struct Skeleton {
    pub strategy: &'static str,
    pub step_px: u16,
    pub frame_budget: u32,
    pub stall_frames: u32,
    pub settle_frames: u32,
    pub dirs: Vec<&'static str>,
    pub rows: Vec<Row>,
    /// How sure the whole table is: the lowest confidence of the rows that
    /// the scout cannot run without.
    pub confidence: f32,
}

impl Skeleton {
    /// The table as TOML text, ready to append to a profile.
    #[must_use]
    pub fn to_toml(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "# W33-07: found by observation while the game was played (confidence {:.0}%).",
            self.confidence * 100.0
        );
        let _ = writeln!(s, "[scout]");
        let _ = writeln!(s, "strategy = \"{}\"", self.strategy);
        let _ = writeln!(s, "step_px = {}", self.step_px);
        let _ = writeln!(s, "frame_budget = {}", self.frame_budget);
        let _ = writeln!(s, "stall_frames = {}", self.stall_frames);
        let _ = writeln!(s, "settle_frames = {}", self.settle_frames);
        let dirs: Vec<String> = self.dirs.iter().map(|d| format!("\"{d}\"")).collect();
        let _ = writeln!(s, "dirs = [{}]", dirs.join(", "));
        for r in &self.rows {
            let _ = writeln!(s, "\n[[scout.addr]]");
            let _ = writeln!(s, "role = \"{}\"", r.role);
            let _ = writeln!(s, "addr = 0x{:X}", r.addr);
            if let Some(v) = r.value {
                let _ = writeln!(s, "value = {v}");
            }
            let _ = writeln!(
                s,
                "notes = \"{}\"",
                r.notes.replace('\\', "\\\\").replace('"', "'")
            );
            let _ = writeln!(s, "source = \"{SOURCE}\"");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic game: the player word at 0x10/0x11 (or 0x10 and 0x40 apart),
    /// a camera at 0x20/0x21 that follows him, a mirror of it at 0x30, a
    /// screen-relative X at 0x32, his Y at 0x50 with a jump, a timer at 0x60,
    /// and a mode byte at 0x70 (1 in the level).
    struct Game {
        ram: Vec<u8>,
        px: i32,
        cam: i32,
        vy: i32,
        y: i32,
        split_hi: bool,
        t: u32,
    }

    impl Game {
        fn new(split_hi: bool, px: i32) -> Self {
            let mut g = Self {
                ram: vec![0; 0x100],
                px,
                cam: 0,
                vy: 0,
                y: 0x80,
                split_hi,
                t: 0,
            };
            g.store();
            g
        }

        fn store(&mut self) {
            let px = self.px as u16;
            self.ram[0x10] = px as u8;
            if self.split_hi {
                self.ram[0x40] = (px >> 8) as u8;
            } else {
                self.ram[0x11] = (px >> 8) as u8;
            }
            let c = self.cam as u16;
            self.ram[0x20] = c as u8;
            self.ram[0x21] = (c >> 8) as u8;
            self.ram[0x30] = c as u8;
            self.ram[0x32] = (self.px - self.cam) as u8;
            self.ram[0x50] = self.y as u8;
            self.ram[0x60] = (self.t / 7) as u8;
        }

        fn frame(&mut self, pad: Pad, jump: bool, level: bool) {
            self.t += 1;
            if level {
                let dx = match pad {
                    Pad::Right => 2,
                    Pad::Left => -2,
                    Pad::Idle => 0,
                };
                self.px = (self.px + dx).clamp(0, 6000);
                // The camera follows past screen x 120 and never goes back.
                self.cam = self.cam.max((self.px - 120).max(0));
                if jump && self.y == 0x80 && self.vy == 0 {
                    self.vy = -6;
                }
                if self.vy != 0 || self.y != 0x80 {
                    self.y += self.vy;
                    self.vy += 1;
                    if self.y >= 0x80 {
                        self.y = 0x80;
                        self.vy = 0;
                    }
                }
                self.ram[0x70] = 1;
            } else {
                self.ram[0x70] = 0;
            }
            self.store();
        }
    }

    fn run(split_hi: bool, mirror_only: bool) -> Observed {
        run_finder(split_hi, mirror_only).observed()
    }

    fn run_finder(split_hi: bool, mirror_only: bool) -> ScoutFinder {
        let mut finder = ScoutFinder::new(0x100, 1);
        // Without a wall the player starts on the camera's follow line, so
        // the camera moves with his first step.
        let mut g = Game::new(split_hi, if mirror_only { 120 } else { 40 });
        let mut feed = |g: &Game, pad: Pad, jump: bool, scene: u64| {
            let jump = u8::from(jump);
            let scroll = [g.ram[0x20]];
            finder.observe(&Frame {
                ram: &g.ram,
                pad,
                jump,
                scroll: &[(&scroll, &[0])],
                scene: Some(scene),
            });
        };
        // A title, then a level: walk into the left wall (the session is under
        // way once a direction is pressed), stand, jump four times, then walk
        // right a long way.
        for _ in 0..120 {
            g.frame(Pad::Idle, false, false);
            feed(&g, Pad::Idle, false, 1);
        }
        for _ in 0..40 {
            g.frame(Pad::Idle, false, true);
            feed(&g, Pad::Idle, false, 2);
        }
        if !mirror_only {
            for _ in 0..40 {
                g.frame(Pad::Left, false, true);
                feed(&g, Pad::Left, false, 2);
            }
        }
        for _ in 0..6 {
            for i in 0..80 {
                g.frame(Pad::Idle, i < 8, true);
                feed(&g, Pad::Idle, i < 8, 2);
            }
        }
        for _ in 0..900 {
            g.frame(Pad::Right, false, true);
            feed(&g, Pad::Right, false, 2);
        }
        finder
    }

    #[test]
    fn finds_every_part_of_a_game_with_an_adjacent_high_byte() {
        let o = run(false, false);
        let cam = o.camera.expect("camera");
        assert_eq!(cam.x.offset, 0x20);
        let p = o.players.first().expect("a player");
        assert_eq!((p.lo, p.hi), (0x10, Some(0x11)), "{o:?}");
        let y = o.ys.iter().find(|y| y.offset == 0x50).expect("y");
        assert_eq!((y.standing, y.up), (0x80, -1));
        assert_eq!(
            o.gates.first().map(|g| (g.offset, g.value)),
            Some((0x70, 1))
        );
        assert!(o.ready());
    }

    #[test]
    fn a_high_byte_a_page_away_is_found_by_its_wraps() {
        let o = run(true, false);
        let p = o.players.first().expect("a player");
        assert_eq!((p.lo, p.hi), (0x10, Some(0x40)), "{o:?}");
    }

    /// The scroll mirror steps like the camera and never moves at a wall; the
    /// screen-relative X never steps with the camera.
    #[test]
    fn camera_mirrors_and_screen_relative_x_are_not_the_player() {
        let o = run(false, false);
        for p in &o.players {
            assert!(![0x20, 0x21, 0x30, 0x32].contains(&p.lo), "{p:?}");
        }
    }

    /// With no frame of the player walking while the camera stands still, no
    /// word is named: the test that separates him from the mirrors has no
    /// evidence either way.
    #[test]
    fn without_a_wall_there_is_no_player() {
        let o = run(false, true);
        assert!(o.players.is_empty(), "{:?}", o.players);
    }

    #[test]
    fn a_timer_is_not_the_player_y_and_the_skeleton_names_its_source() {
        let o = run(false, false);
        assert!(o.ys.iter().all(|y| y.offset != 0x60));
        let sk = Skeleton {
            strategy: "player_move",
            step_px: 4,
            frame_budget: 1500,
            stall_frames: 60,
            settle_frames: 60,
            dirs: vec!["right"],
            rows: vec![Row {
                role: "hold",
                addr: 0x50,
                value: Some(0x28),
                notes: "a \"quoted\" note".into(),
                confidence: 0.9,
            }],
            confidence: 0.9,
        };
        let text = sk.to_toml();
        assert!(text.contains("source = \"observed: auto (W33-07)\""));
        assert!(text.contains("value = 40"));
    }

    /// The camera's mirrors pass the step and direction tests (they step like
    /// the camera and never go the wrong way); only the wall test, which asks
    /// for movement while the camera stands still, takes them out.
    #[test]
    fn the_wall_test_is_what_removes_the_camera_mirrors() {
        let f = run_finder(false, true);
        let mirrors = f.camera_mirrors();
        assert!(
            mirrors.contains(&0x20) && mirrors.contains(&0x30),
            "{mirrors:?}"
        );
        let passing = f.players_without_wall_test();
        assert!(
            passing.contains(&0x30),
            "the mirror passes without the wall test: {passing:?}"
        );
        assert!(
            f.observed().players.is_empty(),
            "and with it nobody is named"
        );
    }
}
