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
//!      moves, **or** (W35-02) adds up to the camera's travel over at least
//!      half of the 16-frame windows the camera moved 8 px or more in (a
//!      camera that eases toward the player and a player who walks in
//!      fractions of a pixel never step together frame by frame: Super
//!      Metroid; windows that hold a cut are skipped), and
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
//!   button press made while standing, on at least 5 of those frames (a flag a
//!   press sets changes once), comes back, and does NOT do so in more than
//!   two of five control windows opened at random while the pad is idle
//!   (timers, animation counters; a player who walks off a ledge falls in
//!   some).
//! * **Start gate.** A byte that holds one value on at least 95 % of the
//!   frames of the level (the scene with the most scrolling frames) and holds
//!   a different value on the frames of every other scene (title, map, fades).
//!   Since W35-02: the other rooms the player scrolled through must hold it
//!   too, where doors were seen it must be gone in them, it takes at most
//!   three values while the player plays and 24 over the session, and a byte
//!   that lets go in a door ranks first. (`retroforge::scout_discovery` then
//!   keeps only the gates that stay true through a scouting run and, where a
//!   vertical room was met, through one there.)
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
/// Frames of one lag-tolerant window (see [`ScoutFinder`]'s `wstep`): a camera
/// that eases toward the player and a player that moves in fractions of a pixel
/// step together over a window, not frame by frame.
const LAG_WINDOW: u32 = 16;
/// Share of the camera-moving windows a word must match: lower than the frame
/// by frame bar, because the camera's own catching up (a window where it starts
/// to follow, or the player turns) and a scripted scroll (a door) move it
/// without the player.
const MIN_LAG_RATIO: f32 = 0.5;
/// A window counts only when the camera moved at least this far in it.
const LAG_MIN_CAMERA: i16 = 8;
/// Windows the lag-tolerant test needs before it can name a word.
const LAG_MIN_WINDOWS: u32 = 6;
/// Frames a Y byte must change in a window (an arc changes nearly every frame;
/// a flag a button press sets changes once).
const MIN_Y_CHANGES: u8 = 5;
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
/// The key of the shared histogram of the scenes the gate stopped tracking.
const OTHER_SCENE: Option<u64> = Some(u64::MAX - 1);
/// The same for the scenes the player scrolled through.
const OTHER_PLAY: Option<u64> = Some(u64::MAX - 2);
/// Camera-moving frames that make another scene a room the player played in.
pub const MIN_SIBLING_FRAMES: u32 = 60;
/// Most distinct values a mode byte takes while the player plays.
pub const MAX_PLAYED_VALUES: usize = 8;
/// Most distinct values a mode byte takes over the whole session.
pub const MAX_MODE_VALUES: usize = 24;
/// The share of a transition's frames a gate's value must be gone from, where
/// transitions were seen.
pub const MIN_TRANSIT_DIFFERS: f32 = 0.5;
/// Frames a scene needs to be a room, not a transition between two.
pub const MIN_ROOM_FRAMES: u32 = 200;
/// Camera-moving frames, at the pad, that make a short scene a transition.
pub const MIN_TRANSIT_FRAMES: u32 = 20;
/// The share of the level's frames a mode byte must hold its value on (the
/// rest: a door's frames inside the scene).
pub const MIN_CONSTANCY: f32 = 0.95;
/// The share of such a room's frames a mode byte must hold its value on (the
/// rest: a door's frames).
pub const SIBLING_SHARE: f32 = 0.6;
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
    /// W35-03: of the byte's changes, the share made at a scene cut (the
    /// frames the tracker settled, the cut itself, a few frames either side).
    /// A mode byte changes there; a value that merely sits still through play
    /// also changes in menus and animations, away from any cut.
    pub cut_sync: f32,
    /// W35-03: of the scene cuts, the share the byte's value differs across
    /// (compared 120 frames before and 60 after). A byte that was set once
    /// when the game began is across none of them but the first.
    pub cut_cover: f32,
    /// W35-03: how many times the byte changed per cut it changed across,
    /// less one, over two, capped at 1 (a state machine steps through several
    /// states at a transition: load, fade, run; a datum a level sets
    /// changes once).
    pub cut_steps: f32,
    /// W35-03: changes made away from any cut (a stack byte, a timer, a
    /// pointer: thousands; a mode byte: a handful).
    pub off_cut: u32,
    /// W35-03: the evidence above, with `differs` and the doors, as one score
    /// in 0..=1 (the ranking key).
    pub contrast: f32,
    /// How many cuts the cover is over.
    pub cuts: u32,
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
    /// The byte's value on the previous window frame.
    last: Vec<u8>,
    /// Window frames on which the byte changed (capped).
    changed: Vec<u8>,
}

/// One scene's histograms of every offset's value.
#[derive(Debug, Clone)]
struct SceneHist {
    frames: u32,
    /// `len * 256` counters.
    bins: Vec<u16>,
}

/// Frames after a cut (and around the frames the tracker was still settling)
/// in which a byte's change counts as made AT the cut (W35-03).
const CUT_ZONE: u64 = 30;
/// A change this many frames before the settling began belongs to the cut too
/// (a mode byte is written a few frames before the picture goes).
const CUT_PRE: u64 = 20;
/// Most cuts kept (each holds the work RAM on both sides); the oldest go first.
const MAX_CUTS: usize = 40;
/// A mode byte moves before the picture does (Super Mario World writes the
/// "leave the level" mode ~100 frames before the scene key changes) and after
/// it (a fade-in): the RAM compared across a cut is the RAM this many frames
/// before it was noticed and this many after.
const CUT_LEAD: usize = 120;
const CUT_LAG: u64 = 60;

/// One cut between two scenes the tracker named, with the work RAM
/// [`CUT_LEAD`] frames before it was noticed and [`CUT_LAG`] frames after.
#[derive(Debug, Clone)]
struct Cut {
    from: u64,
    to: u64,
    due: u64,
    pre: Vec<u8>,
    post: Option<Vec<u8>>,
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
    prev_scroll_y: Vec<Option<u8>>,
    /// Every x write of the previous frame, per layer.
    prev_scroll_all: Vec<Vec<u8>>,
    prev_pad: Pad,
    prev_jump: u8,
    // player X
    moving: u32,
    right_frames: u32,
    left_frames: u32,
    step: Vec<[u32; 2]>,
    /// The lag-tolerant window: per byte the sum of its small changes since
    /// the window opened, the camera's sum, the frames so far; per byte the
    /// windows it matched; the windows judged.
    win_sum: Vec<i16>,
    win_cam: i16,
    win_frames: u32,
    /// The scene the window opened in, and whether a cut or a settling frame
    /// fell inside it (a door's scroll moves the camera and not the player).
    win_scene: Option<Option<u64>>,
    win_dirty: bool,
    wstep: Vec<u32>,
    wwindows: u32,
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
    /// Per scene, the frames the camera moved after the player first pressed a
    /// direction (a title that scrolls by itself is not play).
    scene_play: HashMap<Option<u64>, u32>,
    // gate contrast (W35-03)
    /// The last scene the tracker named, the work RAM on that scene's last
    /// frame, the frame the tracker began settling (if it is), and the last
    /// frame of the zone around a cut.
    /// Per scene, the frames inside a cut's zone (a mode byte is allowed to be
    /// anything on them).
    scene_zone: HashMap<Option<u64>, u32>,
    cut_key: Option<u64>,
    cut_ring: std::collections::VecDeque<Vec<u8>>,
    settle_start: Option<u64>,
    zone_until: u64,
    cuts: Vec<Cut>,
    /// Per byte: frames it changed on, those inside a cut's zone, and the
    /// frame of its last change outside one (0: none yet).
    g_changes: Vec<u32>,
    g_cut_changes: Vec<u32>,
    g_last_off: Vec<u64>,
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
            prev_scroll_y: vec![None; layers.max(1)],
            prev_scroll_all: Vec::new(),
            prev_pad: Pad::Idle,
            prev_jump: 0,
            moving: 0,
            right_frames: 0,
            left_frames: 0,
            step: vec![[0; 2]; len],
            win_sum: vec![0; len],
            win_cam: 0,
            win_frames: 0,
            win_scene: None,
            win_dirty: false,
            wstep: vec![0; len],
            wwindows: 0,
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
            scene_play: HashMap::new(),
            scene_zone: HashMap::new(),
            cut_key: None,
            cut_ring: std::collections::VecDeque::new(),
            settle_start: None,
            zone_until: 0,
            cuts: Vec::new(),
            g_changes: vec![0; len],
            g_cut_changes: vec![0; len],
            g_last_off: vec![0; len],
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
        let cdy = self.camera_delta_y(f.scroll);
        if self.prev.is_empty() {
            self.prev = f.ram[..n].to_vec();
            self.prev_pad = f.pad;
            self.prev_jump = f.jump;
            return;
        }
        self.gate_frame(f, n, cd, armed && (cd != 0 || cdy != 0));
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
        if self.prev_scroll_all.len() < scroll.len() {
            self.prev_scroll_all.resize(scroll.len(), Vec::new());
        }
        for ((prev, all), (xs, _)) in self
            .prev_scroll
            .iter_mut()
            .zip(&mut self.prev_scroll_all)
            .zip(scroll)
        {
            let Some(&now) = xs.last() else { continue };
            let mut d = prev
                .replace(now)
                .map_or(0, |before| i16::from(now.wrapping_sub(before) as i8));
            // W35-03: a game with a status bar writes the scroll register
            // again after the game's own split (Castlevania: four writes of
            // the camera, two of the bar's zero, every frame), and the last
            // write is the bar's. The write in the same place in the sequence
            // as last frame's, when the frame wrote as many, is the one that
            // moved.
            if d == 0 && all.len() == xs.len() {
                d = xs
                    .iter()
                    .zip(all.iter())
                    .map(|(&n, &b)| i16::from(n.wrapping_sub(b) as i8))
                    .find(|&d| d != 0)
                    .unwrap_or(0);
            }
            all.clear();
            all.extend_from_slice(xs);
            if out == 0 && d != 0 {
                out = d;
            }
        }
        out
    }

    /// The same for the y scroll byte.
    fn camera_delta_y(&mut self, scroll: &[(&[u8], &[u8])]) -> i16 {
        let mut out = 0i16;
        for (prev, (_, ys)) in self.prev_scroll_y.iter_mut().zip(scroll) {
            let Some(&now) = ys.last() else { continue };
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
            self.win_sum[b] += d;
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
        if cam_moved {
            self.win_cam += cd;
        }
        self.win_frames += 1;
        match self.win_scene {
            None => self.win_scene = Some(f.scene),
            Some(first) => self.win_dirty |= first != f.scene,
        }
        self.win_dirty |= f.scene.is_none();
        if self.win_frames >= LAG_WINDOW {
            self.close_lag_window(n);
        }
        self.wrap_pairs(f, n);
    }

    /// Judge the lag window: every byte whose changes added up to the camera's
    /// travel (give or take a pixel and an eighth) matched it.
    fn close_lag_window(&mut self, n: usize) {
        if self.win_cam.abs() >= LAG_MIN_CAMERA && !self.win_dirty {
            self.wwindows += 1;
            let tol = 2 + self.win_cam.abs() / 8;
            for b in 0..n {
                if (self.win_sum[b] - self.win_cam).abs() <= tol {
                    self.wstep[b] += 1;
                }
            }
        }
        self.win_sum[..n].fill(0);
        self.win_cam = 0;
        self.win_frames = 0;
        self.win_scene = None;
        self.win_dirty = false;
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
                let exact = steps as f32 / self.moving as f32;
                #[allow(clippy::cast_precision_loss)]
                let lagged = if self.wwindows >= LAG_MIN_WINDOWS {
                    self.wstep[b] as f32 / self.wwindows as f32
                } else {
                    0.0
                };
                let ratio = exact.max(lagged);
                let follows = exact >= MIN_PLAYER_STEP || lagged >= MIN_LAG_RATIO;
                let ok = follows
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
                last: self.prev[..n].to_vec(),
                changed: vec![0; n],
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
            if f.ram[b] != w.last[b] {
                w.changed[b] = w.changed[b].saturating_add(1);
                w.last[b] = f.ram[b];
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
                && w.changed[b] >= MIN_Y_CHANGES
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

    /// Track the scene cuts and, per byte, whether its changes fall at them.
    fn cut_frame(&mut self, f: &Frame<'_>, n: usize) -> bool {
        let frame = self.frames;
        match f.scene {
            None => {
                self.settle_start.get_or_insert(frame);
                self.zone_until = frame + CUT_ZONE;
            }
            Some(k) => {
                if let Some(from) = self.cut_key.filter(|&from| from != k) {
                    self.record_cut(from, k, n);
                }
                self.cut_key = Some(k);
                self.settle_start = None;
            }
        }
        // The RAM of the last CUT_LEAD frames, and the "after" of the cuts
        // whose lag has passed.
        let mut snap = if self.cut_ring.len() > CUT_LEAD {
            self.cut_ring.pop_front().unwrap_or_default()
        } else {
            Vec::new()
        };
        snap.clear();
        snap.extend_from_slice(&f.ram[..n]);
        self.cut_ring.push_back(snap);
        for c in self.cuts.iter_mut().filter(|c| c.post.is_none()) {
            if frame >= c.due {
                c.post = Some(f.ram[..n].to_vec());
            }
        }
        let in_zone = f.scene.is_none() || frame <= self.zone_until;
        let m = n.min(self.prev.len());
        for b in 0..m {
            if f.ram[b] != self.prev[b] {
                self.g_changes[b] = self.g_changes[b].saturating_add(1);
                if in_zone {
                    self.g_cut_changes[b] = self.g_cut_changes[b].saturating_add(1);
                } else {
                    self.g_last_off[b] = frame;
                }
            }
        }
        in_zone
    }

    fn record_cut(&mut self, from: u64, to: u64, n: usize) {
        let frame = self.frames;
        self.zone_until = frame + CUT_ZONE;
        // Changes a few frames before the settling began were the cut's.
        let start = self.settle_start.unwrap_or(frame).saturating_sub(CUT_PRE);
        for b in 0..n.min(self.g_last_off.len()) {
            if self.g_last_off[b] != 0 && self.g_last_off[b] >= start {
                self.g_cut_changes[b] = self.g_cut_changes[b].saturating_add(1);
                self.g_last_off[b] = 0;
            }
        }
        if self.cuts.len() >= MAX_CUTS {
            self.cuts.remove(0);
        }
        self.cuts.push(Cut {
            from,
            to,
            due: frame + CUT_LAG,
            pre: self.cut_ring.front().cloned().unwrap_or_default(),
            post: None,
        });
    }

    /// `played`: the player has the pad in hand and the screen moved (on
    /// either axis).
    fn gate_frame(&mut self, f: &Frame<'_>, n: usize, cd: i16, played: bool) {
        let in_zone = self.cut_frame(f, n);
        let key = f.scene;
        if in_zone {
            *self.scene_zone.entry(key).or_insert(0) += 1;
        }
        if cd != 0 && cd.abs() <= MAX_STEP {
            *self.scene_moving.entry(key).or_insert(0) += 1;
        }
        if played {
            *self.scene_play.entry(key).or_insert(0) += 1;
        }
        // A scene beyond the cap takes the place of the one that scrolled
        // least, whose frames fold into the shared "other" histogram: a
        // session with many scenes before the level (a title, menus, a story
        // told in pages) must not leave the level without a histogram.
        if !self.scenes.contains_key(&key) && self.scenes.len() >= MAX_SCENES {
            self.evict_quietest_scene();
        }
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

    /// Fold the histogram of the scene that scrolled least into the shared
    /// "other" one.
    fn evict_quietest_scene(&mut self) {
        let victim = self
            .scenes
            .keys()
            .copied()
            .filter(|k| k.is_some() && *k != OTHER_SCENE && *k != OTHER_PLAY)
            .min_by_key(|k| (self.scene_play.get(k).copied().unwrap_or(0), *k));
        let Some(victim) = victim else { return };
        let Some(gone) = self.scenes.remove(&victim) else {
            return;
        };
        let into = if self.scene_play.get(&victim).copied().unwrap_or(0) >= MIN_SIBLING_FRAMES
            && gone.frames >= MIN_ROOM_FRAMES
        {
            OTHER_PLAY
        } else {
            OTHER_SCENE
        };
        let len = self.len;
        let other = self.scenes.entry(into).or_insert_with(|| SceneHist {
            frames: 0,
            bins: vec![0; len * 256],
        });
        other.frames += gone.frames;
        for (o, g) in other.bins.iter_mut().zip(&gone.bins) {
            *o = o.saturating_add(*g);
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
             jump_hit {}/{} control_hit {}/{} | frames {} camera moving {} | lag windows {}/{}",
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
            self.wstep[b],
            self.wwindows,
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
                    && self.control_hit[b] * 5 <= self.control_windows * 2
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
        self.gates_ranked(4 * KEEP)
    }

    /// The gate candidates in rank order, `keep` of them (the discovery tests
    /// print more than [`Observed`] keeps, to see where a known byte ranks).
    #[must_use]
    pub fn gates_ranked(&self, keep: usize) -> Vec<GateCandidate> {
        let Some((level_key, _)) = self
            .scene_moving
            .iter()
            .filter(|(k, _)| k.is_some() && self.scenes.contains_key(k))
            .max_by_key(|(k, v)| (**v, std::cmp::Reverse(k.unwrap_or(0))))
        else {
            return Vec::new();
        };
        let Some(level) = self.scenes.get(level_key) else {
            return Vec::new();
        };
        // The other scenes the player also scrolled in (the next room, the
        // other half of a level) are play as well: a mode byte holds its value
        // there too, and a byte that does not is something about THIS room.
        // They are not evidence of what "not playing" looks like, so the
        // difference is measured against the rest (title, menus, cuts).
        let scrolled = |k: &Option<u64>| {
            *k == OTHER_PLAY
                || (k.is_some()
                    && *k != OTHER_SCENE
                    && self.scene_play.get(k).copied().unwrap_or(0) >= MIN_SIBLING_FRAMES)
        };
        let mut siblings: Vec<&SceneHist> = Vec::new();
        let mut rest: Vec<&SceneHist> = Vec::new();
        // The short scenes the camera moved in while the player was at the pad
        // (a door's scroll, a fade): what "not playing" looks like inside a
        // game.
        let mut transit: Vec<&SceneHist> = Vec::new();
        for (k, h) in self.scenes.iter().filter(|(k, _)| *k != level_key) {
            if scrolled(k) && (*k == OTHER_PLAY || h.frames >= MIN_ROOM_FRAMES) {
                siblings.push(h);
            } else {
                rest.push(h);
                if self.scene_play.get(k).copied().unwrap_or(0) >= MIN_TRANSIT_FRAMES {
                    transit.push(h);
                }
            }
        }
        let transit_frames: u32 = transit.iter().map(|h| h.frames).sum();
        let mut other_frames: u32 = rest.iter().map(|h| h.frames).sum();
        if other_frames < MIN_OTHER_FRAMES {
            // Nothing but play was seen: the old rule, against every scene.
            rest.append(&mut siblings);
            other_frames = rest.iter().map(|h| h.frames).sum();
        }
        if level.frames < MIN_LEVEL_FRAMES || other_frames < MIN_OTHER_FRAMES {
            return Vec::new();
        }
        // The cuts whose "after" has been seen (W35-03), and of them the ones
        // between playing (the level, the rooms the player scrolled in) and
        // not (menus, doors, fades): the transitions a start gate is about.
        let playing = |k: u64| {
            Some(k) == *level_key
                || (self.scene_play.get(&Some(k)).copied().unwrap_or(0) >= MIN_SIBLING_FRAMES
                    && self
                        .scenes
                        .get(&Some(k))
                        .is_none_or(|h| h.frames >= MIN_ROOM_FRAMES))
        };
        let judged_all: Vec<(&Vec<u8>, &Vec<u8>, bool)> = self
            .cuts
            .iter()
            .filter_map(|c| {
                c.post
                    .as_ref()
                    .map(|post| (&c.pre, post, playing(c.from) != playing(c.to)))
            })
            .collect();
        let level_zone = self.scene_zone.get(level_key).copied().unwrap_or(0);
        let mut out = Vec::new();
        for b in 0..self.len {
            let bins = &level.bins[b * 256..b * 256 + 256];
            let Some((mode, &count)) = bins.iter().enumerate().max_by_key(|(v, c)| (**c, *v))
            else {
                continue;
            };
            #[allow(clippy::cast_precision_loss)]
            let constancy = f32::from(count) / level.frames as f32;
            // W35-03: the frames of a cut's zone (a fade-in, a loading mode
            // that is still running when the scene key already says "level")
            // are not held against the byte.
            let off_mode = level.frames.saturating_sub(u32::from(count));
            let unexplained = off_mode.saturating_sub(level_zone);
            #[allow(clippy::cast_precision_loss)]
            let steady = 1.0 - unexplained as f32 / level.frames as f32;
            if steady < MIN_CONSTANCY {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let holds_in_siblings = siblings
                .iter()
                .all(|h| f32::from(h.bins[b * 256 + mode]) >= SIBLING_SHARE * h.frames as f32);
            if !holds_in_siblings {
                continue;
            }
            let same: u32 = rest.iter().map(|h| u32::from(h.bins[b * 256 + mode])).sum();
            #[allow(clippy::cast_precision_loss)]
            let differs = 1.0 - same as f32 / other_frames as f32;
            if differs < 0.6 {
                continue;
            }
            // A mode byte takes a handful of values over a whole session; a
            // scratch variable or a pointer takes dozens and happens to sit
            // still through the level.
            let distinct = (0..256)
                .filter(|&v| self.scenes.values().any(|h| h.bins[b * 256 + v] > 0))
                .count();
            if distinct > MAX_MODE_VALUES {
                continue;
            }
            // How often the value is gone on a transition's frames: 1.0 when
            // none were seen (no evidence either way).
            let in_transit: u32 = transit
                .iter()
                .map(|h| u32::from(h.bins[b * 256 + mode]))
                .sum();
            #[allow(clippy::cast_precision_loss)]
            let transit_differs = if transit_frames >= MIN_TRANSIT_FRAMES {
                1.0 - in_transit as f32 / transit_frames as f32
            } else {
                1.0
            };
            // Whether it took more than one value while the player played
            // (the level, the rooms beside it, the doors between them): a
            // mode byte lets go of its value at a door or a pause, a flag set
            // when the game began never does.
            let played_values = (0..256)
                .filter(|&v| {
                    level.bins[b * 256 + v] > 0
                        || siblings
                            .iter()
                            .chain(&transit)
                            .any(|h| h.bins[b * 256 + v] > 0)
                })
                .count();
            // ...but only a few values: a byte that takes many while the
            // player plays (a pose, a speed) sits at its level value only by
            // chance, and a gate that is false for a stretch of ordinary play
            // keeps the scout from ever starting.
            if played_values > MAX_PLAYED_VALUES {
                continue;
            }
            let reacts = played_values >= 2;
            // Where doors were seen, a gate is gone in them: that is what it
            // is for (the scout must not start in a transition).
            if transit_differs < MIN_TRANSIT_DIFFERS {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let mode_u8 = mode as u8;
            // The share of cuts the byte's value differs across: over all of
            // them, and over those between playing and not playing, which
            // count as much together as the rest do.
            let cover = |only_boundary: bool| -> Option<f32> {
                let (mut n, mut hit) = (0u32, 0u32);
                for (pre, post, boundary) in &judged_all {
                    if *boundary || !only_boundary {
                        n += 1;
                        hit += u32::from(pre.get(b) != post.get(b));
                    }
                }
                #[allow(clippy::cast_precision_loss)]
                (n > 0).then(|| hit as f32 / n as f32)
            };
            let all = cover(false).unwrap_or(0.0);
            let cut_cover = cover(true).map_or(all, |bd| 0.5 * (all + bd));
            #[allow(clippy::cast_possible_truncation)]
            let cuts = judged_all.len() as u32;
            #[allow(clippy::cast_precision_loss)]
            let cut_sync = if self.g_changes[b] == 0 {
                0.0
            } else {
                self.g_cut_changes[b].min(self.g_changes[b]) as f32 / self.g_changes[b] as f32
            };
            #[allow(clippy::cast_precision_loss)]
            let cut_steps = {
                let covered = (all * cuts as f32).round().max(1.0);
                ((self.g_cut_changes[b] as f32 / covered - 1.0) / 2.0).clamp(0.0, 1.0)
            };
            // Few changes away from the cuts: a stack byte, a timer or a
            // pointer changes dozens of times, a mode byte a handful.
            let off_cut = self.g_changes[b].saturating_sub(self.g_cut_changes[b]);
            #[allow(clippy::cast_precision_loss)]
            let calm = 1.0 / (1.0 + off_cut as f32 / 16.0);
            let contrast = (0.35 * cut_cover
                + 0.2 * cut_sync
                + 0.25 * cut_steps
                + 0.1 * differs
                + 0.1 * transit_differs)
                * (0.5 + 0.5 * calm);
            out.push((
                GateCandidate {
                    offset: b,
                    value: mode_u8,
                    constancy,
                    differs,
                    // Held through steady play, and told from the rest of the
                    // session by what it does at the cuts.
                    confidence: steady.min(1.0) * (0.5 + 0.5 * contrast),
                    cut_sync,
                    cut_cover,
                    cut_steps,
                    contrast,
                    off_cut,
                    cuts,
                },
                (reacts, distinct, transit_differs),
            ));
        }
        // Contrast first (W35-03): a mode byte changes at the cuts between
        // playing and not playing, and away from them it stays put. Bytes that
        // merely sit still through play also change in menus and animations
        // with no cut near. Equally good bytes (to a tenth) are told apart by
        // which of them lets go in a door or a fade: a mode byte does, a flag
        // set once at the start of the game does not.
        out.sort_by(|(a, (ar, ad, at)), (b, (br, bd, bt))| {
            let (xa, xb) = ((a.contrast * 10.0).round(), (b.contrast * 10.0).round());
            let (ac, bc) = ((a.confidence * 10.0).round(), (b.confidence * 10.0).round());
            xb.total_cmp(&xa)
                .then(br.cmp(ar))
                .then(bc.total_cmp(&ac))
                .then(ad.cmp(bd))
                .then(bt.total_cmp(at))
                .then(a.offset.cmp(&b.offset))
        });
        let mut out: Vec<GateCandidate> = out.into_iter().map(|(g, _)| g).collect();
        out.truncate(keep);
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
        /// A button flag the game sets while a face button is held (0x80).
        flag: u8,
        /// The player walks in fractions of a pixel (1, 3, 1, 2, 1...) and the
        /// camera follows two frames late: no frame-by-frame match between them.
        eased: bool,
        /// The player's X on the last frames (the eased camera follows 2 late).
        hist: Vec<i32>,
        /// Level frames so far, and how many of the first ones the mode byte
        /// at 0x73 reads 3 on (a fade-in) before it reads 5.
        lvl_t: u32,
        fade: u32,
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
                flag: 0,
                eased: false,
                hist: Vec::new(),
                lvl_t: 0,
                fade: 2,
            };
            g.store();
            g
        }

        fn store(&mut self) {
            self.ram[0x80] = self.flag;
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
            self.flag = if jump { 0x80 } else { 0 };
            if level {
                let step = if self.eased {
                    [1, 3, 1, 2, 1][(self.t % 5) as usize]
                } else {
                    2
                };
                let dx = match pad {
                    Pad::Right => step,
                    Pad::Left => -step,
                    Pad::Idle => 0,
                };
                self.px = (self.px + dx).clamp(0, 6000);
                // The camera follows past screen x 120 and never goes back.
                self.hist.push(self.px);
                let late = self.hist[self.hist.len().saturating_sub(3)];
                self.cam = if self.eased {
                    self.cam.max((late - 120).max(0))
                } else {
                    self.cam.max((self.px - 120).max(0))
                };
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
                // A byte a level sets once (0x72), and a mode that steps
                // through states at the cut (0x73: 3, 4, then 5).
                self.lvl_t += 1;
                self.ram[0x72] = 5;
                self.ram[0x73] = if self.lvl_t <= self.fade {
                    3
                } else if self.lvl_t <= self.fade + 2 {
                    4
                } else {
                    5
                };
            } else {
                self.ram[0x70] = 0;
                self.ram[0x72] = 0;
                self.ram[0x73] = 0;
            }
            self.store();
        }
    }

    /// The session of `run_finder`, with `pre` menu scenes before the level and
    /// optionally the eased game.
    fn run_scenario(pre: u64, eased: bool) -> ScoutFinder {
        let mut finder = ScoutFinder::new(0x100, 1);
        let mut g = Game::new(false, 40);
        g.eased = eased;
        let mut feed = |g: &Game, pad: Pad, jump: bool, scene: u64| {
            let scroll = [g.ram[0x20]];
            finder.observe(&Frame {
                ram: &g.ram,
                pad,
                jump: u8::from(jump),
                scroll: &[(&scroll, &[0])],
                scene: Some(scene),
            });
        };
        for s in 0..pre {
            for _ in 0..70 {
                g.frame(Pad::Idle, false, false);
                feed(&g, Pad::Idle, false, 100 + s);
            }
        }
        for _ in 0..40 {
            g.frame(Pad::Idle, false, true);
            feed(&g, Pad::Idle, false, 2);
        }
        for _ in 0..60 {
            g.frame(Pad::Left, false, true);
            feed(&g, Pad::Left, false, 2);
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

    fn run(split_hi: bool, mirror_only: bool) -> Observed {
        run_finder(split_hi, mirror_only).observed()
    }

    fn run_finder(split_hi: bool, mirror_only: bool) -> ScoutFinder {
        run_finder_with(split_hi, mirror_only, false)
    }

    /// `hud`: the game writes the scroll register four times with the camera
    /// and twice more with the status bar's zero, every frame (Castlevania).
    fn run_finder_with(split_hi: bool, mirror_only: bool, hud: bool) -> ScoutFinder {
        let mut finder = ScoutFinder::new(0x100, 1);
        // Without a wall the player starts on the camera's follow line, so
        // the camera moves with his first step.
        let mut g = Game::new(split_hi, if mirror_only { 120 } else { 40 });
        let mut feed = |g: &Game, pad: Pad, jump: bool, scene: u64| {
            let jump = u8::from(jump);
            let scroll: Vec<u8> = if hud {
                let c = g.ram[0x20];
                vec![c, c, c, c, 0, 0]
            } else {
                vec![g.ram[0x20]]
            };
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
        // The mode byte that steps at the cut (0x73) ranks first; the plain
        // level flag (0x70) is still a candidate.
        assert_eq!(
            o.gates.first().map(|g| (g.offset, g.value)),
            Some((0x73, 5))
        );
        assert!(o.gates.iter().any(|g| (g.offset, g.value) == (0x70, 1)));
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

    /// Ticket W35-02: a camera that eases toward the player, and a player who
    /// walks in fractions of a pixel, never step together frame by frame (Super
    /// Metroid). Over a window of frames their travel agrees.
    #[test]
    fn a_camera_that_eases_still_names_the_player() {
        let f = run_scenario(1, true);
        let o = f.observed();
        let p = o.players.first().expect("a player");
        assert_eq!(p.lo, 0x10, "{o:?}");
        let exact = f.step[0x10][0].max(f.step[0x10][1]) as f32 / f.moving as f32;
        assert!(
            exact < MIN_PLAYER_STEP,
            "the frame-by-frame test alone would not have named him: {exact}"
        );
        // ...and the mirrors are still not him.
        assert!(o
            .players
            .iter()
            .all(|p| ![0x20, 0x30, 0x32].contains(&p.lo)));
    }

    /// Ticket W35-02: a flag a button press sets and a release clears changes
    /// once, not along a jump's arc: not a Y.
    #[test]
    fn a_button_flag_is_not_the_player_y() {
        let o = run(false, false);
        assert!(o.ys.iter().any(|y| y.offset == 0x50), "{:?}", o.ys);
        assert!(o.ys.iter().all(|y| y.offset != 0x80), "{:?}", o.ys);
    }

    /// Ticket W35-02: ten menu scenes before the level must not leave the level
    /// without a histogram to find its gate in (the cap used to send it to the
    /// shared one).
    #[test]
    fn a_level_after_many_scenes_still_has_a_gate() {
        let o = run_scenario(10, false).observed();
        assert!(
            o.gates.iter().any(|g| (g.offset, g.value) == (0x70, 1)),
            "{:?}",
            o.gates
        );
    }

    /// Ticket W35-03: a mode byte steps through several states at a cut; a
    /// byte the level sets once changes once. Both sit still through play and
    /// differ in the title, so the old score could not tell them apart.
    #[test]
    fn a_byte_that_steps_at_the_cut_outranks_one_set_once() {
        let o = run_scenario(1, false).observed();
        let rank = |off: usize| o.gates.iter().position(|g| g.offset == off);
        let (stepping, once) = (rank(0x73), rank(0x72));
        assert!(stepping.is_some() && once.is_some(), "{:?}", o.gates);
        assert!(stepping < once, "{:?}", o.gates);
        let g = o.gates.iter().find(|g| g.offset == 0x73).expect("0x73");
        assert!(g.cut_steps > 0.9 && g.cut_cover > 0.9, "{g:?}");
    }

    /// Ticket W35-03: a mode byte still on its loading value for the first
    /// frames after the scene key says "level" (a fade-in) is not held against
    /// it: the frames of the cut's zone are excused.
    #[test]
    fn a_fade_in_after_the_cut_is_not_held_against_the_gate() {
        let mut finder = ScoutFinder::new(0x100, 1);
        let mut g = Game::new(false, 40);
        g.fade = 28;
        let mut feed = |g: &Game, pad: Pad, scene: u64| {
            let scroll = [g.ram[0x20]];
            finder.observe(&Frame {
                ram: &g.ram,
                pad,
                jump: 0,
                scroll: &[(&scroll, &[0])],
                scene: Some(scene),
            });
        };
        for _ in 0..100 {
            g.frame(Pad::Idle, false, false);
            feed(&g, Pad::Idle, 1);
        }
        for _ in 0..40 {
            g.frame(Pad::Left, false, true);
            feed(&g, Pad::Left, 2);
        }
        for _ in 0..300 {
            g.frame(Pad::Right, false, true);
            feed(&g, Pad::Right, 2);
        }
        let o = finder.observed();
        let gate = o.gates.iter().find(|g| g.offset == 0x73).expect("0x73");
        assert_eq!(gate.value, 5, "{gate:?}");
        assert!(gate.constancy < MIN_CONSTANCY, "raw share {gate:?}");
    }

    /// Ticket W35-03: a game whose status bar writes the scroll register after
    /// the camera's own split (the last write each frame is the bar's zero)
    /// still has its camera step seen, so the player is named.
    #[test]
    fn a_status_bar_write_after_the_camera_does_not_hide_the_camera() {
        let plain = run_finder_with(false, false, false).observed();
        let hud = run_finder_with(false, false, true).observed();
        let p = hud.players.first().expect("a player behind the status bar");
        assert_eq!((p.lo, p.hi), (0x10, Some(0x11)), "{hud:?}");
        assert_eq!(
            hud.players.first().map(|p| p.lo),
            plain.players.first().map(|p| p.lo)
        );
    }
}
