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
//! * **Start gate.** A byte whose value tells the game's states apart (W35-05): the level and
//!   the rooms the player played in (one state: a value that changes from room to room while
//!   play goes on is the room's own data), each other scene that held still for a while (title,
//!   map, menus), the frames in the cuts' zones (doors, fades, loads), a **pause** (Start pressed,
//!   the picture frozen and the game's memory quiet, up to the next press) and a **speech** (a
//!   direction held while the picture stands still). The score is `I(value; state) / H(state)`
//!   with the states weighed equally, held against a byte whose values mostly belong to no state
//!   (a counter), a value that covers less than 60 % of the frames of play, and a byte that
//!   changes all the time away from the cuts. The byte must hold one value on at least 95 % of
//!   the level's frames (not counting the cut zones, the pauses, the speeches and the values it
//!   reads on a quarter of those: a door, a text box). Equal scores (to a fiftieth) are told
//!   apart by what the byte does at the cuts (W35-03: cover, sync, steps, calm). Where a pause
//!   was seen and the best byte does not leave its value in it (Super Mario World's mode stays
//!   `0x14`), the best byte that does (a flag that reads `0` through play) is the second.
//!   (`retroforge::scout_discovery` then keeps only the gates that stay true through a scouting
//!   run and, where a vertical room was met, through one there.)
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
use rf_core_api::PpuPixel;

/// The horizontal direction the pad held on a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pad {
    #[default]
    Idle,
    Left,
    Right,
}

/// The vertical direction the pad held on a frame (W35-04: a top-down game's player is found by
/// walking up and down).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Vert {
    #[default]
    Idle,
    Up,
    Down,
}

/// One frame of play.
#[derive(Debug, Clone, Copy, Default)]
pub struct Frame<'a> {
    /// Work RAM at the frame's end.
    pub ram: &'a [u8],
    /// The horizontal pad direction held this frame.
    pub pad: Pad,
    /// The vertical pad direction held this frame.
    pub vert: Vert,
    /// The face buttons held this frame, one bit each (any of them might be
    /// the jump button; a run button held all the time must not hide the
    /// press of another).
    pub jump: u8,
    /// Per background layer, in preference order: every low byte written to
    /// its x and y scroll registers this frame ([`CameraFinder::observe`]).
    pub scroll: &'a [(&'a [u8], &'a [u8])],
    /// The canvas model's scene for the frame; `None` while a cut settles.
    pub scene: Option<u64>,
    /// W35-05: Start is held this frame.
    pub start: bool,
    /// W35-05: a hash of the picture ([`picture_hash`]); `0` = not known. A pause is Start pressed
    /// and the picture frozen; a speech box is a pad held and the picture frozen.
    pub shown: u64,
}

/// A hash of the indexed picture of a frame (never `0`): two frames with the same picture have
/// the same hash.
#[must_use]
pub fn picture_hash(video: &[PpuPixel]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in video {
        h = (h ^ u64::from(p.palette_index) ^ (u64::from(p.sprite_id.unwrap_or(0xFF)) << 8))
            .wrapping_mul(0x0100_0000_01b3);
    }
    h | 1
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
/// W35-04: a camera step of at most this many pixels is a walk's; a bigger one is a scripted scroll
/// (A Link to the Past's screen-edge transition moves the camera 8 px a frame while the player
/// stands). The player is also judged over the walk's frames alone.
const SLOW_STEP: i16 = 4;
/// W35-04: camera-moving frames on the vertical axis the walk-found player Y wants.
pub const MIN_VERT_MOVING: u32 = 60;
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
/// W35-05: the share of all the frames of play (the level and the rooms beside it) a mode byte's
/// value must hold on.
pub const MIN_PLAY_COVER: f32 = 0.6;
/// Scenes whose frames the gate keeps a histogram of.
pub const MAX_SCENES: usize = 6;
/// Frames of the level the gate wants.
pub const MIN_LEVEL_FRAMES: u32 = 300;
/// Frames outside the level the gate wants.
pub const MIN_OTHER_FRAMES: u32 = 60;
/// How many candidates of each kind are kept.
pub const KEEP: usize = 4;
/// W35-05: the least `mode_score` a candidate needs (a byte that is the same in every state
/// scores 0).
pub const MIN_MODE_SCORE: f32 = 0.15;
/// W35-05: frames a state needs before the mode score counts it as a label.
pub const MIN_LABEL_FRAMES: u32 = 20;
/// W35-05: frames of a pause or a speech box needed before the guard test counts.
pub const MIN_FROZEN_FRAMES: u32 = 12;
/// W35-05: frames a direction is held in a scene before it is one the player has played in, with
/// or without the screen moving (a pause comes before the camera has started).
const MIN_WALK_FRAMES: u32 = 30;
/// W35-05: frames walked in a scene that make it a room played in, though the screen never
/// scrolled.
const MIN_ROOM_WALK: u32 = 100;
/// W35-05: a direction held counts as walking while the picture changed within this many frames.
const WALK_LIVE: u32 = 8;
/// W35-05: the busiest the game's memory has to be (bytes changing per frame, on average) for a
/// drop to be a pause (a game that ticks no counter at all tells nothing).
const MIN_ACTIVITY: f32 = 6.0;
/// W35-05: frames the picture must have stood still for a Start press to have paused the game.
const PAUSE_FROZEN: u32 = 4;
/// W35-05: frames after a Start press the picture has to freeze within.
const PAUSE_WITHIN: u64 = 14;
/// W35-05: frames the picture changes on every one of that end a pause window (a game that
/// runs on, whatever Start did).
const PAUSE_LIVE: u32 = 8;
/// W35-05: a pause the player never leaves (the cap on a window).
const PAUSE_MAX: u64 = 900;
/// W35-05: frames a direction is held and the picture is frozen before the game is "ignoring the
/// pad" (a speech box, a lock).
const IGNORED_AFTER: u32 = 24;
/// W35-05: the share of a pause's or speech's frames a guard's value must be gone from.
pub const MIN_FREEZE_APART: f32 = 0.9;

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
    /// W35-05: symmetric uncertainty between the byte's value and the states the session
    /// showed (play, each menu or map, the cut zones, a pause, a speech box), 0..=1: 1 when
    /// the value tells every state apart. 0 when the session showed fewer than two.
    pub mode_score: f32,
    /// W35-05: the share of the frames of a pause (a Start press, the picture frozen, a second
    /// press) the play value is gone from (`1.0` when none were seen).
    pub freeze_apart: f32,
    /// W35-05: the pause frames behind `freeze_apart`.
    pub frozen_frames: u32,
    /// W35-05: the same for the frames a direction was held on a frozen picture (a speech box, a
    /// lock; also a wall in a game whose player stands still against it).
    pub speech_apart: f32,
    pub speech_frames: u32,
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
    /// W35-04: the Y candidates were named by walking up and down with the camera's Y (no jump
    /// moved any byte): a top-down game. A hold that floats the player above the ground means
    /// nothing in one.
    pub top_down: bool,
}

impl Observed {
    /// Whether the observations are complete enough to try the candidates on
    /// a machine: a camera, a player word whose high half is known, a Y.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.camera.is_some()
            && self.players.first().is_some_and(|p| p.hi.is_some())
            && !self.ys.is_empty()
            && (self.jump_windows >= MIN_JUMP_WINDOWS || self.top_down)
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
    /// W35-05: the frames of `frames` that were inside a cut's zone, and their counters (empty
    /// until the first one): the states a session labels by the cuts.
    zone_frames: u32,
    zone: Vec<u16>,
}

impl SceneHist {
    fn new(len: usize) -> Self {
        Self {
            frames: 0,
            bins: vec![0; len * 256],
            zone_frames: 0,
            zone: Vec::new(),
        }
    }

    fn add(bins: &mut [u16], ram: &[u8]) {
        for (b, &v) in ram.iter().enumerate() {
            let slot = &mut bins[b * 256 + usize::from(v)];
            *slot = slot.saturating_add(1);
        }
    }

    /// Count one frame; `zone`: it was inside a cut's zone.
    fn count(&mut self, ram: &[u8], zone: bool) {
        self.frames += 1;
        Self::add(&mut self.bins, ram);
        if zone {
            self.zone_frames += 1;
            if self.zone.is_empty() {
                self.zone = vec![0; self.bins.len()];
            }
            Self::add(&mut self.zone, ram);
        }
    }

    /// Fold `gone` into this histogram.
    fn absorb(&mut self, gone: &Self) {
        self.frames += gone.frames;
        for (o, g) in self.bins.iter_mut().zip(&gone.bins) {
            *o = o.saturating_add(*g);
        }
        if !gone.zone.is_empty() {
            self.zone_frames += gone.zone_frames;
            if self.zone.is_empty() {
                self.zone = vec![0; self.bins.len()];
            }
            for (o, g) in self.zone.iter_mut().zip(&gone.zone) {
                *o = o.saturating_add(*g);
            }
        }
    }

    /// The frames outside the cut zones.
    const fn clean_frames(&self) -> u32 {
        self.frames - self.zone_frames
    }

    /// Add the counts of byte `b` outside the zones (`zone: false`) or inside them to `out`.
    fn tally(&self, b: usize, zone: bool, out: &mut [u32; 256]) {
        for (v, o) in out.iter_mut().enumerate() {
            let all = self.bins[b * 256 + v];
            let z = self.zone.get(b * 256 + v).copied().unwrap_or(0);
            *o += u32::from(if zone { z } else { all.saturating_sub(z) });
        }
    }
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
    prev_vert: Vert,
    prev_jump: u8,
    // player Y by walking (W35-04)
    moving_y: u32,
    moving_y_slow: u32,
    ystep_slow: Vec<[u32; 2]>,
    up_frames: u32,
    down_frames: u32,
    ystep: Vec<[u32; 2]>,
    down_up: Vec<u32>,
    up_down: Vec<u32>,
    ywrong: Vec<u32>,
    ywall: Vec<u32>,
    // player X
    moving: u32,
    /// W35-04: the camera-moving frames of a walk's pace (see [`SLOW_STEP`]), and per byte the
    /// matches among them.
    moving_slow: u32,
    step_slow: Vec<[u32; 2]>,
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
    /// W35-05: per scene, the frames a direction was held in it.
    scene_walk: HashMap<Option<u64>, u32>,
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
    // frozen states (W35-05)
    prev_shown: u64,
    prev_start: bool,
    /// Frames the picture has stood still, and a direction has been held.
    still_run: u32,
    /// Frames the picture has changed on every one of.
    live_run: u32,
    /// W35-05: a moving average of the bytes of work RAM that change in a frame, and the frames in
    /// a row far below it.
    activity: f32,
    hush_run: u32,
    /// Frames since the picture last changed.
    since_change: u32,
    dir_run: u32,
    /// The frame of a Start press the picture has not yet frozen after.
    press: Option<u64>,
    /// The scene and frame a pause began in.
    pause: Option<(Option<u64>, u64)>,
    /// The work RAM on the frames of a pause that ended with a second press in its own scene,
    /// of the pause now running, and of the frames a speech held the pad.
    paused: Option<SceneHist>,
    staging: Option<SceneHist>,
    ignored: Option<SceneHist>,
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
            prev_vert: Vert::Idle,
            prev_jump: 0,
            moving_y: 0,
            moving_y_slow: 0,
            ystep_slow: vec![[0; 2]; len],
            up_frames: 0,
            down_frames: 0,
            ystep: vec![[0; 2]; len],
            down_up: vec![0; len],
            up_down: vec![0; len],
            ywrong: vec![0; len],
            ywall: vec![0; len],
            moving: 0,
            moving_slow: 0,
            step_slow: vec![[0; 2]; len],
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
            scene_walk: HashMap::new(),
            scene_zone: HashMap::new(),
            cut_key: None,
            cut_ring: std::collections::VecDeque::new(),
            settle_start: None,
            zone_until: 0,
            cuts: Vec::new(),
            g_changes: vec![0; len],
            g_cut_changes: vec![0; len],
            g_last_off: vec![0; len],
            prev_shown: 0,
            prev_start: false,
            still_run: 0,
            live_run: 0,
            activity: 0.0,
            hush_run: 0,
            since_change: 0,
            dir_run: 0,
            press: None,
            pause: None,
            paused: None,
            staging: None,
            ignored: None,
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
        let armed = self.camera.armed() || f.pad != Pad::Idle || f.vert != Vert::Idle;
        self.camera.observe(f.scroll, f.ram, armed);
        let cd = self.camera_delta(f.scroll);
        let cdy = self.camera_delta_y(f.scroll);
        if self.prev.is_empty() {
            self.prev = f.ram[..n].to_vec();
            self.prev_pad = f.pad;
            self.prev_vert = f.vert;
            self.prev_jump = f.jump;
            return;
        }
        self.gate_frame(f, n, (cd, cdy), armed && (cd != 0 || cdy != 0));
        if armed {
            self.player_frame(f, n, cd);
            self.vert_frame(f, n, cdy);
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
        self.prev_vert = f.vert;
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
        let slow = cam_moved && cd.abs() <= SLOW_STEP;
        self.moving_slow += u32::from(slow);
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
            if slow {
                self.step_slow[b][0] += u32::from(d == cd);
                self.step_slow[b][1] += u32::from(pd == cd);
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

    /// W35-04: the player's Y by walking, the vertical twin of the X test: a byte that steps with
    /// the camera's Y when it scrolls, rises on Down and falls on Up, and keeps moving in the
    /// direction held while the camera's Y stands still (the camera's own shadows do not).
    fn vert_frame(&mut self, f: &Frame<'_>, n: usize, cdy: i16) {
        let down_now = f.vert == Vert::Down;
        let up_now = f.vert == Vert::Up;
        // A press can take a frame to reach the game's variables.
        let down = down_now || self.prev_vert == Vert::Down;
        let up = up_now || self.prev_vert == Vert::Up;
        self.down_frames += u32::from(down_now);
        self.up_frames += u32::from(up_now);
        // Only a camera that moved the way the pad was held counts: one that eases back the other
        // way while the player pushes against a wall is not following him.
        let cam_moved = cdy != 0 && cdy.abs() <= MAX_STEP && ((cdy > 0 && down) || (cdy < 0 && up));
        self.moving_y += u32::from(cam_moved);
        let slow = cam_moved && cdy.abs() <= SLOW_STEP;
        self.moving_y_slow += u32::from(slow);
        let wall_frame = (down || up) && cdy == 0;
        let steady_down = down_now && self.prev_vert == Vert::Down;
        let steady_up = up_now && self.prev_vert == Vert::Up;
        for b in 0..n {
            let d = i16::from(f.ram[b].wrapping_sub(self.prev[b]) as i8);
            let pd = i16::from(self.last_delta[b]);
            if cam_moved {
                self.ystep[b][0] += u32::from(d == cdy);
                self.ystep[b][1] += u32::from(pd == cdy);
            }
            if slow {
                self.ystep_slow[b][0] += u32::from(d == cdy);
                self.ystep_slow[b][1] += u32::from(pd == cdy);
            }
            if d == 0 || d.abs() > MAX_STEP {
                continue;
            }
            if down && d > 0 {
                self.down_up[b] += 1;
            }
            if up && d < 0 {
                self.up_down[b] += 1;
            }
            if (steady_down && d < 0) || (steady_up && d > 0) {
                self.ywrong[b] += 1;
            }
            if wall_frame && ((down && d > 0) || (up && d < 0)) {
                self.ywall[b] += 1;
            }
        }
    }

    /// The Y candidates a walk names (best first): bytes that matched the camera's Y on at least
    /// [`MIN_PLAYER_STEP`] of its scrolling frames and passed the wall and direction tests.
    fn walk_ys(&self) -> Vec<YCandidate> {
        if self.moving_y < MIN_VERT_MOVING {
            return Vec::new();
        }
        let dir_frames = self.down_frames + self.up_frames;
        let mut out: Vec<(usize, f32, u32)> = (0..self.len)
            .filter_map(|b| {
                let steps = self.ystep[b][0].max(self.ystep[b][1]);
                #[allow(clippy::cast_precision_loss)]
                let exact = steps as f32 / self.moving_y as f32;
                // Over the walk's frames alone (a screen-edge transition scrolls the camera
                // without the player).
                #[allow(clippy::cast_precision_loss)]
                let paced = if self.moving_y_slow >= MIN_VERT_MOVING {
                    self.ystep_slow[b][0].max(self.ystep_slow[b][1]) as f32
                        / self.moving_y_slow as f32
                } else {
                    0.0
                };
                let ratio = exact.max(paced);
                let ok = ratio >= MIN_PLAYER_STEP
                    && self.ywall[b] >= MIN_WALL
                    && self.down_up[b] >= 10
                    && (self.up_frames < 30 || self.up_down[b] >= 2)
                    && self.ywrong[b] * 8 <= dir_frames;
                ok.then_some((b, ratio, self.ywall[b]))
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
        out.into_iter()
            .take(4 * KEEP)
            .map(|(b, ratio, wall)| YCandidate {
                offset: b,
                standing: self.prev.get(b).copied().unwrap_or(0),
                up: -1,
                hits: self.ystep[b][0].max(self.ystep[b][1]),
                windows: self.moving_y,
                confidence: ratio.min(1.0) * (wall as f32 / (wall as f32 + 10.0)).min(1.0),
            })
            .collect()
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
                // W35-04: over the walk's frames alone, when there were enough of them.
                #[allow(clippy::cast_precision_loss)]
                let paced = if self.moving_slow >= min_moving {
                    self.step_slow[b][0].max(self.step_slow[b][1]) as f32 / self.moving_slow as f32
                } else {
                    0.0
                };
                let ratio = exact.max(lagged).max(paced);
                let follows =
                    exact >= MIN_PLAYER_STEP || lagged >= MIN_LAG_RATIO || paced >= MIN_PLAYER_STEP;
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
    /// either axis). `camera`: the signed change of the camera on each axis.
    fn gate_frame(&mut self, f: &Frame<'_>, n: usize, camera: (i16, i16), played: bool) {
        let in_zone = self.cut_frame(f, n);
        let key = f.scene;
        let cd = camera.0;
        if cd != 0 && cd.abs() <= MAX_STEP {
            *self.scene_moving.entry(key).or_insert(0) += 1;
        }
        if played {
            *self.scene_play.entry(key).or_insert(0) += 1;
        }
        let len = self.len;
        // W35-05: a pause and a speech are states of their own: their frames go to their own
        // histograms, and the level's histogram stays the level's play.
        let changed = f.ram[..n]
            .iter()
            .zip(&self.prev)
            .filter(|(a, b)| a != b)
            .count();
        if let Some(frozen) = self.frozen_frame(f, key, camera == (0, 0), changed) {
            let slot = if frozen {
                &mut self.staging
            } else {
                &mut self.ignored
            };
            slot.get_or_insert_with(|| SceneHist::new(len))
                .count(&f.ram[..n], false);
            return;
        }
        if in_zone {
            *self.scene_zone.entry(key).or_insert(0) += 1;
        }
        // A scene beyond the cap takes the place of the one that scrolled
        // least, whose frames fold into the shared "other" histogram: a
        // session with many scenes before the level (a title, menus, a story
        // told in pages) must not leave the level without a histogram.
        if !self.scenes.contains_key(&key) && self.scenes.len() >= MAX_SCENES {
            self.evict_quietest_scene();
        }
        self.scenes
            .entry(key)
            .or_insert_with(|| SceneHist::new(len))
            .count(&f.ram[..n], in_zone);
    }

    /// Whether this frame is part of a pause (`Some(true)`) or of a speech that holds the pad
    /// (`Some(false)`). A pause is a Start press after which the picture freezes, up to the next
    /// press in the same scene (a press after which the scene goes, or the screen moves, was not
    /// one: its frames go back to their scene); a speech is a direction held while the picture
    /// stands still, inside a scene the player has played in.
    fn frozen_frame(
        &mut self,
        f: &Frame<'_>,
        key: Option<u64>,
        camera_still: bool,
        changed: usize,
    ) -> Option<bool> {
        let frame = self.frames;
        let (known, same) = (f.shown != 0, f.shown == self.prev_shown);
        self.still_run = if known && same {
            self.still_run.saturating_add(1)
        } else {
            0
        };
        // How busy the game's memory is: a paused game stops counting its frames.
        #[allow(clippy::cast_precision_loss)]
        let ch = changed as f32;
        let quiet = self.activity >= MIN_ACTIVITY && ch * 4.0 <= self.activity;
        self.hush_run = if quiet {
            self.hush_run.saturating_add(1)
        } else {
            0
        };
        if !quiet && self.pause.is_none() {
            self.activity += (ch - self.activity) / 16.0;
        }
        self.live_run = if known && !same {
            self.live_run.saturating_add(1)
        } else {
            0
        };
        self.prev_shown = f.shown;
        let dir = f.pad != Pad::Idle || f.vert != Vert::Idle;
        self.dir_run = if dir {
            self.dir_run.saturating_add(1)
        } else {
            0
        };
        let edge = f.start && !self.prev_start;
        self.prev_start = f.start;
        if let Some((scene, since)) = self.pause {
            if edge && scene == key {
                // The press that ends it: a pause. The frame is the game's again.
                self.close_pause(true);
                self.press = None;
                return None;
            }
            if !camera_still
                || scene != key
                || frame - since > PAUSE_MAX
                || self.live_run >= PAUSE_LIVE
            {
                self.close_pause(false);
            } else {
                return Some(true);
            }
        }
        // Only in a scene the player has played in (a menu has Start pressed on a static
        // screen all the time).
        self.since_change = if known && !same {
            0
        } else {
            self.since_change.saturating_add(1)
        };
        // Walking counts when the picture is alive (a black screen, a fade or a text box holds
        // the pad too).
        if dir && known && self.since_change < WALK_LIVE {
            *self.scene_walk.entry(key).or_insert(0) += 1;
        }
        let played_in = key.is_some()
            && (self.scene_play.get(&key).copied().unwrap_or(0) >= MIN_TRANSIT_FRAMES
                || self.scene_walk.get(&key).copied().unwrap_or(0) >= MIN_WALK_FRAMES);
        if played_in {
            if edge {
                self.press = Some(frame);
            }
            if let Some(at) = self.press {
                if self.still_run >= PAUSE_FROZEN && self.hush_run >= PAUSE_FROZEN && camera_still {
                    self.press = None;
                    self.pause = Some((key, frame));
                    return Some(true);
                }
                if frame - at > PAUSE_WITHIN {
                    self.press = None;
                }
            }
        } else {
            self.press = None;
        }
        if played_in
            && dir
            && self.dir_run >= IGNORED_AFTER
            && self.still_run >= IGNORED_AFTER
            && camera_still
        {
            return Some(false);
        }
        None
    }

    /// End the pause being staged: keep its frames as a pause, or give them back to their scene.
    fn close_pause(&mut self, kept: bool) {
        let Some((scene, _)) = self.pause.take() else {
            return;
        };
        let Some(staged) = self.staging.take() else {
            return;
        };
        let len = self.len;
        let into = if kept {
            Some(self.paused.get_or_insert_with(|| SceneHist::new(len)))
        } else {
            self.scenes.get_mut(&scene)
        };
        if let Some(into) = into {
            into.absorb(&staged);
        }
    }

    /// Frames of a scene the player had the pad in hand and the screen answered: by moving, or
    /// (W35-05, a room of one screen) by a picture that lived while a direction was held, for
    /// at least a third of the scene.
    fn played_frames(&self, k: &Option<u64>) -> u32 {
        let scrolled = self.scene_play.get(k).copied().unwrap_or(0);
        let walked = self.scene_walk.get(k).copied().unwrap_or(0);
        let frames = self.scenes.get(k).map_or(0, |h| h.frames);
        if walked >= MIN_ROOM_WALK && walked.saturating_mul(3) >= frames {
            scrolled.max(walked)
        } else {
            scrolled
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
            .min_by_key(|k| (self.played_frames(k), *k));
        let Some(victim) = victim else { return };
        let Some(gone) = self.scenes.remove(&victim) else {
            return;
        };
        let into = if self.played_frames(&victim) >= MIN_SIBLING_FRAMES
            && gone.frames >= MIN_ROOM_FRAMES
        {
            OTHER_PLAY
        } else {
            OTHER_SCENE
        };
        let len = self.len;
        self.scenes
            .entry(into)
            .or_insert_with(|| SceneHist::new(len))
            .absorb(&gone);
    }

    /// The values of byte `b` in each state the session showed, as text (diagnostics for the
    /// discovery tests): per scene its frames, the frames inside cut zones and the commonest
    /// values with their counts; then the pause and the speech.
    #[must_use]
    pub fn state_probe(&self, b: usize) -> String {
        let top = |bins: &[u16]| -> String {
            let mut v: Vec<(u16, usize)> = bins[b * 256..b * 256 + 256]
                .iter()
                .enumerate()
                .filter(|(_, c)| **c > 0)
                .map(|(i, c)| (*c, i))
                .collect();
            v.sort_by(|a, b| b.cmp(a));
            let more = v.len().saturating_sub(4);
            let shown: Vec<String> = v
                .iter()
                .take(4)
                .map(|(c, i)| format!("{i:02x}x{c}"))
                .collect();
            format!(
                "{}{}",
                shown.join(" "),
                if more > 0 {
                    format!(" +{more}")
                } else {
                    String::new()
                }
            )
        };
        let mut keys: Vec<_> = self.scenes.keys().copied().collect();
        keys.sort_by_key(|k| k.map_or(0, |k| k % 100_000));
        let mut out = format!("state {b:#06x}:");
        for k in keys {
            let h = &self.scenes[&k];
            let z = if h.zone.is_empty() {
                String::new()
            } else {
                format!(" zone[{}]", top(&h.zone))
            };
            out += &format!(
                "\n   scene {:?} frames {} zone {}: {}{z}",
                k.map(|k| k % 100_000),
                h.frames,
                h.zone_frames,
                top(&h.bins)
            );
        }
        for (name, h) in [("pause", &self.paused), ("speech", &self.ignored)] {
            if let Some(h) = h {
                out += &format!("\n   {name} frames {}: {}", h.frames, top(&h.bins));
            }
        }
        out
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
             jump_hit {}/{} control_hit {}/{} | frames {} camera moving {} | lag windows {}/{} | \
             slow {:?}/{} | y: moving {} step {:?} slow {:?}/{} wall {} down_up {} up_down {} wrong {}",
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
            self.step_slow[b],
            self.moving_slow,
            self.moving_y,
            self.ystep[b],
            self.ystep_slow[b],
            self.moving_y_slow,
            self.ywall[b],
            self.down_up[b],
            self.up_down[b],
            self.ywrong[b],
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
        let (ys, top_down) = self.ys_and_kind();
        Observed {
            camera: self.camera.verdict(),
            players: self.players(),
            ys,
            gates: self.gates(),
            jump_windows: self.jump_windows,
            top_down,
        }
    }

    fn players(&self) -> Vec<PlayerCandidate> {
        // W35-04: the camera's own word is not the player (a game whose scroll mirror happens to
        // stand still a few frames with a direction held passes the wall test: The Legend of
        // Zelda's `$FD`, which a scout then "walked" by poking the scroll, stitching the same
        // screen's nametable over and over into a canvas 8,000 wide).
        let camera = self.camera.verdict();
        let is_camera = |b: usize| {
            camera.is_some_and(|c| {
                [Some(c.x), c.y]
                    .into_iter()
                    .flatten()
                    .any(|a| b == a.offset)
            })
        };
        let lows = self.low_candidates(MIN_PLAYER_MOVING);
        lows.into_iter()
            .filter(|&(lo, _, _)| !is_camera(lo))
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

    /// The Y candidates and whether the game is top-down. A byte that answers a face button (a
    /// text box's cursor: A pages through a speech) looks like a jump's Y, so a game where
    /// walking up and down moves some other byte with the camera's Y is top-down, and the walk's
    /// bytes are the only Y candidates. A platformer's ladder moves the same byte a jump does:
    /// both lists are kept.
    fn ys_and_kind(&self) -> (Vec<YCandidate>, bool) {
        let mut jumped = self.jump_ys();
        let walked = self.walk_ys();
        if walked.is_empty() {
            return (jumped, false);
        }
        if !jumped
            .iter()
            .any(|j| walked.iter().any(|w| w.offset == j.offset))
        {
            return (walked, true);
        }
        for w in walked {
            if !jumped.iter().any(|j| j.offset == w.offset) {
                jumped.push(w);
            }
        }
        (jumped, false)
    }

    fn jump_ys(&self) -> Vec<YCandidate> {
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
                || (k.is_some() && *k != OTHER_SCENE && self.played_frames(k) >= MIN_SIBLING_FRAMES)
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
        // W35-05: the states the session showed, before the fallback below folds the rooms
        // into the rest.
        let states = States::new(self, level, &siblings, &rest, &transit);
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
                || (self.played_frames(&Some(k)) >= MIN_SIBLING_FRAMES
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
            let excused: u32 = states
                .frozen_values(b, mode)
                .iter()
                .enumerate()
                .filter(|(_, f)| **f)
                .map(|(v, _)| u32::from(bins[v]))
                .sum();
            // (A fifth of the level at most: a byte that reads the paused value for half of the
            // level is not in control then, it is something else.)
            let excused = excused.min(level.frames / 5);
            let unexplained = off_mode.saturating_sub(level_zone).saturating_sub(excused);
            #[allow(clippy::cast_precision_loss)]
            let steady = 1.0 - unexplained as f32 / level.frames as f32;
            if steady < MIN_CONSTANCY {
                continue;
            }
            // W35-05: one value covers nearly all of the frames the player played on, in every
            // room (a value that is a room's own data covers one room's share; a mode byte that
            // takes another value indoors still covers nearly all of the session).
            let score = states.score(b, mode);
            if score.cover < MIN_PLAY_COVER {
                continue;
            }
            // Few changes away from the cuts: a stack byte, a timer or a pointer changes dozens
            // of times, a mode byte a handful.
            let off_cut = self.g_changes[b].saturating_sub(self.g_cut_changes[b]);
            #[allow(clippy::cast_precision_loss)]
            let calm = 1.0 / (1.0 + off_cut as f32 / 16.0);
            // A counter changes all the time away from the states' changes: the score is held
            // against it a little.
            let mode_score = score.mode * (0.8 + 0.2 * calm);
            let (freeze_apart, frozen_frames) = score.pause;
            let (speech_apart, speech_frames) = score.speech;
            let same: u32 = rest.iter().map(|h| u32::from(h.bins[b * 256 + mode])).sum();
            #[allow(clippy::cast_precision_loss)]
            let differs = 1.0 - same as f32 / other_frames as f32;
            // W35-05: a byte that is gone in every pause and speech may be a guard even when
            // its value is also the menus' (a pause flag is 0 in a menu too).
            let guard_fit = frozen_frames >= MIN_FROZEN_FRAMES && freeze_apart >= MIN_FREEZE_APART;
            // (W35-05: a byte that says nothing about the states is out; that a mode byte holds
            // its value in a room the player cannot move in, a title or a speech room, must not
            // count against it, so `differs` is no longer a bar of its own.)
            if mode_score < MIN_MODE_SCORE && !guard_fit {
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
            // (W35-05: the values that matter, a hundredth of the frames played at least, and
            // not the ones of a text box or a pause, which a game reads in a room too.)
            let frozen_here = states.frozen_values(b, mode);
            let played_total: u32 = (0..256)
                .map(|v| {
                    u32::from(level.bins[b * 256 + v])
                        + siblings
                            .iter()
                            .chain(&transit)
                            .map(|h| u32::from(h.bins[b * 256 + v]))
                            .sum::<u32>()
                })
                .sum();
            let played_values = (0..256)
                .filter(|&v| {
                    let n = u32::from(level.bins[b * 256 + v])
                        + siblings
                            .iter()
                            .chain(&transit)
                            .map(|h| u32::from(h.bins[b * 256 + v]))
                            .sum::<u32>();
                    n > 0 && (v == mode || (!frozen_here[v] && n * 100 >= played_total))
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
            if transit_differs < MIN_TRANSIT_DIFFERS && !guard_fit {
                continue;
            }
            let regular = mode_score >= MIN_MODE_SCORE && transit_differs >= MIN_TRANSIT_DIFFERS;
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
            let contrast = (0.35 * cut_cover
                + 0.2 * cut_sync
                + 0.25 * cut_steps
                + 0.1 * differs
                + 0.1 * transit_differs)
                * (0.5 + 0.5 * calm);
            // A byte that tells the session's states apart is a mode byte, whatever it does at
            // the cuts; a guard is trusted for what it does in the pauses.
            let quality = if guard_fit {
                freeze_apart.max(if regular {
                    contrast.max(mode_score)
                } else {
                    0.0
                })
            } else {
                contrast.max(mode_score)
            };
            out.push((
                GateCandidate {
                    offset: b,
                    value: mode_u8,
                    constancy,
                    differs,
                    // Held through steady play, and told from the rest of the
                    // session by what it does at the cuts.
                    confidence: steady.min(1.0) * (0.5 + 0.5 * quality),
                    cut_sync,
                    cut_cover,
                    cut_steps,
                    contrast,
                    off_cut,
                    cuts,
                    mode_score,
                    freeze_apart,
                    frozen_frames,
                    speech_apart,
                    speech_frames,
                },
                (reacts, distinct, transit_differs, regular),
            ));
        }
        // Contrast first (W35-03): a mode byte changes at the cuts between
        // playing and not playing, and away from them it stays put. Bytes that
        // merely sit still through play also change in menus and animations
        // with no cut near. Equally good bytes (to a tenth) are told apart by
        // which of them lets go in a door or a fade: a mode byte does, a flag
        // set once at the start of the game does not.
        out.sort_by(|(a, (ar, ad, at, _)), (b, (br, bd, bt, _))| {
            // W35-05: the byte that tells the states apart first (to a fiftieth: the evidence of a
            // short session is thin and nearly equal scores are told apart by what follows).
            let (sa, sb) = ((a.mode_score * 50.0).round(), (b.mode_score * 50.0).round());
            let (xa, xb) = ((a.contrast * 20.0).round(), (b.contrast * 20.0).round());
            let (ac, bc) = ((a.confidence * 10.0).round(), (b.confidence * 10.0).round());
            sb.total_cmp(&sa)
                .then(xb.total_cmp(&xa))
                .then(br.cmp(ar))
                .then(bc.total_cmp(&ac))
                .then(ad.cmp(bd))
                .then(bt.total_cmp(at))
                .then(a.offset.cmp(&b.offset))
        });
        // The bytes that are not mode bytes at all (the guards) come after the best of them,
        // not among them.
        let (regular, guards): (Vec<_>, Vec<_>) = out.into_iter().partition(|(_, (.., r))| *r);
        let guard_wanted = regular.first().is_some_and(|(g, _)| {
            g.frozen_frames >= MIN_FROZEN_FRAMES && g.freeze_apart < MIN_FREEZE_APART
        });
        let mut ranked: Vec<GateCandidate> = Vec::new();
        if guard_wanted {
            // Best guard, regular candidates that happen to be gone in the pauses included: the
            // one the pause leaves furthest away, that holds through play, the calmest.
            // (A few speech frames say nothing: neutral.)
            let speech_gone = |g: &GateCandidate| {
                if g.speech_frames >= MIN_FROZEN_FRAMES {
                    g.speech_apart
                } else {
                    1.0
                }
            };
            let fits = |g: &GateCandidate| {
                g.frozen_frames >= MIN_FROZEN_FRAMES && g.freeze_apart >= MIN_FREEZE_APART
            };
            let mut pool: Vec<(GateCandidate, usize, bool)> = guards
                .iter()
                .map(|(g, (_, d, ..))| (*g, *d, false))
                .chain(
                    regular
                        .iter()
                        .filter(|(g, _)| fits(g))
                        .map(|(g, (_, d, ..))| (*g, *d, true)),
                )
                .collect();
            pool.sort_by(|(a, ad, _), (b, bd, _)| {
                b.freeze_apart
                    .total_cmp(&a.freeze_apart)
                    .then(b.constancy.total_cmp(&a.constancy))
                    .then(speech_gone(b).total_cmp(&speech_gone(a)))
                    .then(a.off_cut.cmp(&b.off_cut))
                    .then(ad.cmp(bd))
                    .then(a.offset.cmp(&b.offset))
            });
            let guard = pool.first().map(|(g, ..)| *g);
            let mut it = regular
                .into_iter()
                .map(|(g, _)| g)
                .filter(|g| Some(g.offset) != guard.map(|g| g.offset));
            ranked.extend(it.next());
            ranked.extend(guard);
            ranked.extend(it);
        } else {
            ranked.extend(regular.into_iter().map(|(g, _)| g));
        }
        ranked.truncate(keep);
        ranked
    }
}

/// W35-05: the states a session showed, as histograms of the work RAM, to score a byte by how well its
/// value tells them apart. Play is the level and the rooms the player scrolled through (one state:
/// a value that changes from room to room while play goes on tells nothing); every other scene
/// that held still for a while is a state; the frames in the cuts' zones (doors, fades, loads) are
/// one; a pause and a speech are one each.
struct States<'a> {
    /// The scenes that make up play.
    play: Vec<&'a SceneHist>,
    /// Each other scene with frames of its own, and the transitions' scenes.
    scenes: Vec<&'a SceneHist>,
    /// The scenes the player crossed between rooms.
    transit: Vec<&'a SceneHist>,
    /// Every scene, for the zones.
    all: Vec<&'a SceneHist>,
    paused: Option<&'a SceneHist>,
    ignored: Option<&'a SceneHist>,
}

impl<'a> States<'a> {
    fn new(
        f: &'a ScoutFinder,
        level: &'a SceneHist,
        siblings: &[&'a SceneHist],
        rest: &[&'a SceneHist],
        transit: &[&'a SceneHist],
    ) -> Self {
        let mut play = vec![level];
        play.extend(siblings.iter().copied());
        let is_transit = |h: &&SceneHist| transit.iter().any(|t| std::ptr::eq(*t, *h));
        Self {
            play,
            scenes: rest
                .iter()
                .copied()
                .filter(|h| !is_transit(h) && h.clean_frames() >= MIN_LABEL_FRAMES)
                .collect(),
            transit: transit.to_vec(),
            all: f.scenes.values().collect(),
            paused: f.paused.as_ref(),
            ignored: f.ignored.as_ref(),
        }
    }

    /// The values (other than `mode`) byte `b` read on at least a quarter of the frames of a
    /// pause or a speech: when the byte reads one of them in the middle of play, the game was
    /// not in control then either (a door, a text box that does not freeze the picture).
    fn frozen_values(&self, b: usize, mode: usize) -> [bool; 256] {
        let mut out = [false; 256];
        let hists: Vec<&SceneHist> = [self.paused, self.ignored].into_iter().flatten().collect();
        let total: u32 = hists.iter().map(|h| h.frames).sum();
        if total < MIN_FROZEN_FRAMES {
            return out;
        }
        for (v, o) in out.iter_mut().enumerate() {
            let n: u32 = hists.iter().map(|h| u32::from(h.bins[b * 256 + v])).sum();
            *o = v != mode && n * 4 >= total;
        }
        out
    }

    /// The symmetric uncertainty of byte `b`'s value against the states, and how far its play value
    /// `mode` is from the values of the pauses and speeches (with their frames).
    fn score(&self, b: usize, mode: usize) -> Score {
        let mut counts: Vec<[u32; 256]> = Vec::new();
        let mut keep = |c: [u32; 256]| {
            if c.iter().sum::<u32>() >= MIN_LABEL_FRAMES {
                counts.push(c);
            }
        };
        let mut play = [0u32; 256];
        self.play.iter().for_each(|h| h.tally(b, false, &mut play));
        for (c, _) in play
            .iter_mut()
            .zip(self.frozen_values(b, mode))
            .filter(|(_, f)| *f)
        {
            *c = 0;
        }
        // The share of play the value holds on (a value that changes from room to room while
        // play goes on is data of the room).
        let play_all: u32 = play.iter().sum();
        #[allow(clippy::cast_precision_loss)]
        let play_cover = if play_all == 0 {
            1.0
        } else {
            play[mode] as f32 / play_all as f32
        };
        keep(play);
        for h in &self.scenes {
            let mut c = [0u32; 256];
            h.tally(b, false, &mut c);
            keep(c);
        }
        let mut cuts = [0u32; 256];
        self.all.iter().for_each(|h| h.tally(b, true, &mut cuts));
        self.transit
            .iter()
            .for_each(|h| h.tally(b, false, &mut cuts));
        keep(cuts);
        let mut apart = [(1.0f32, 0u32); 2];
        for (slot, h) in apart.iter_mut().zip([self.paused, self.ignored]) {
            let Some(h) = h else { continue };
            let mut c = [0u32; 256];
            h.bins[b * 256..b * 256 + 256]
                .iter()
                .zip(c.iter_mut())
                .for_each(|(&x, y)| *y = u32::from(x));
            let all: u32 = c.iter().sum();
            if all > 0 {
                #[allow(clippy::cast_precision_loss)]
                {
                    *slot = ((all - c[mode]) as f32 / all as f32, all);
                }
            }
            if all >= MIN_FROZEN_FRAMES {
                counts.push(c);
            }
        }
        let slices: Vec<&[u32]> = counts.iter().map(<[u32; 256]>::as_slice).collect();
        let (nmi, purity) = state_information(&slices);
        Score {
            mode: nmi * play_cover * (0.7 + 0.3 * purity),
            cover: play_cover,
            pause: apart[0],
            speech: apart[1],
        }
    }
}

/// W35-05: what the states say of one byte.
struct Score {
    /// How well its value tells the states apart (0..=1).
    mode: f32,
    /// The share of the frames of play its play value holds on.
    cover: f32,
    /// The share of the pause's frames its play value is gone from, and the frames.
    pause: (f32, u32),
    /// The same for the speeches (frames the pad was held on a frozen picture).
    speech: (f32, u32),
}

/// W35-05: how well a byte's value `V` tells the states `L` it was read in apart, one histogram of
/// values per state, the states weighed equally (a pause of a hundred frames counts as much as a
/// level of ten thousand). Returns `(nmi, purity)`: `nmi = I(V; L) / H(L)` is 1 when the value
/// tells every state apart and 0 when it says nothing (a constant, a value that is the same
/// everywhere but in one state tells one state apart and scores low); `purity = I(V; L) / H(V)` is
/// 1 when every value the byte takes belongs to one state, and low for a counter (many values, a
/// few frames of each in every state). `(0, 0)` below two states.
#[must_use]
pub fn state_information(states: &[&[u32]]) -> (f32, f32) {
    let states: Vec<(&[u32], f64)> = states
        .iter()
        .map(|c| (*c, c.iter().map(|&x| f64::from(x)).sum::<f64>()))
        .filter(|(_, n)| *n > 0.0)
        .collect();
    if states.len() < 2 {
        return (0.0, 0.0);
    }
    #[allow(clippy::cast_precision_loss)]
    let k = states.len() as f64;
    let width = states.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
    let (mut mi, mut hv) = (0.0f64, 0.0f64);
    for v in 0..width {
        let at = |c: &[u32], n: f64| c.get(v).map_or(0.0, |&x| f64::from(x)) / n;
        let p_v: f64 = states.iter().map(|(c, n)| at(c, *n) / k).sum();
        if p_v <= 0.0 {
            continue;
        }
        hv -= p_v * p_v.log2();
        for (c, n) in &states {
            let p_vl = at(c, *n);
            if p_vl > 0.0 {
                mi += p_vl / k * (p_vl / p_v).log2();
            }
        }
    }
    let nmi = mi / k.log2();
    let purity = if hv > 0.0 { mi / hv } else { 0.0 };
    #[allow(clippy::cast_possible_truncation)]
    (nmi.clamp(0.0, 1.0) as f32, purity.clamp(0.0, 1.0) as f32)
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
                vert: Vert::Idle,
                jump: u8::from(jump),
                scroll: &[(&scroll, &[0])],
                scene: Some(scene),
                ..Frame::default()
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
                vert: Vert::Idle,
                jump,
                scroll: &[(&scroll, &[0])],
                scene: Some(scene),
                ..Frame::default()
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

    /// W35-04: a top-down game. The player's X at 0x10/0x11 and Y at 0x12/0x13 move 2 px a frame
    /// with the pad (no jump moves anything), the camera (X 0x20/0x21, Y 0x22/0x23) follows him
    /// once he is past the middle of the screen, a half-speed layer's word (0x30) wobbles by
    /// itself, and every 250 frames a screen-edge transition scrolls the camera 8 px a frame for
    /// 24 frames while the player stands (then drops him a page further on).
    struct TopDown {
        ram: Vec<u8>,
        px: i32,
        py: i32,
        cx: i32,
        cy: i32,
        t: u32,
        burst: u32,
        /// A text box's byte that answers a button: it rises by 2 a frame for 8 frames and falls
        /// back (frames since the press, `None` when idle).
        junk: Option<u8>,
    }

    impl TopDown {
        fn new() -> Self {
            let mut g = Self {
                ram: vec![0; 0x100],
                px: 40,
                py: 40,
                cx: 0,
                cy: 0,
                t: 0,
                burst: 0,
                junk: None,
            };
            g.store();
            g
        }

        fn store(&mut self) {
            self.ram[0x70] =
                self.junk
                    .map_or(0, |k| if k < 8 { 2 * (k + 1) } else { 16 - 2 * (k - 7) });
            for (at, v) in [
                (0x10, self.px),
                (0x12, self.py),
                (0x20, self.cx),
                (0x22, self.cy),
            ] {
                self.ram[at] = v as u8;
                self.ram[at + 1] = (v >> 8) as u8;
            }
            self.ram[0x30] = (self.cx / 2) as u8;
            self.ram[0x60] = (self.t / 7) as u8;
        }

        fn frame(&mut self, pad: Pad, vert: Vert, press: bool) {
            self.t += 1;
            self.junk = match self.junk {
                None if press => Some(0),
                Some(k) if k < 15 => Some(k + 1),
                _ => None,
            };
            let (dx, dy) = (
                match pad {
                    Pad::Right => 2,
                    Pad::Left => -2,
                    Pad::Idle => 0,
                },
                match vert {
                    Vert::Down => 2,
                    Vert::Up => -2,
                    Vert::Idle => 0,
                },
            );
            if self.burst > 0 {
                self.burst -= 1;
                self.cx += if pad == Pad::Left { -8 } else { 8 };
                if self.burst == 0 {
                    self.px += if pad == Pad::Left { -256 } else { 256 };
                }
            } else if self.t.is_multiple_of(250) && dx != 0 {
                self.burst = 24;
            } else {
                self.px = (self.px + dx).max(0);
                self.py = (self.py + dy).max(0);
                if (dx > 0 && self.px - self.cx > 120) || (dx < 0 && self.px - self.cx < 100) {
                    self.cx = (self.cx + dx).max(0);
                }
                if (dy > 0 && self.py - self.cy > 110) || (dy < 0 && self.py - self.cy < 90) {
                    self.cy = (self.cy + dy).max(0);
                }
            }
            self.store();
        }
    }

    fn run_top_down() -> ScoutFinder {
        let mut finder = ScoutFinder::new(0x100, 2);
        let mut g = TopDown::new();
        let mut feed = |g: &TopDown, pad: Pad, vert: Vert, press: bool| {
            // Layer 0 is switched off (no writes fed), layer 1 is the map.
            let (x, y) = ([g.ram[0x20]], [g.ram[0x22]]);
            finder.observe(&Frame {
                ram: &g.ram,
                pad,
                vert,
                jump: u8::from(press),
                scroll: &[(&[], &[]), (&x, &y)],
                scene: Some(1),
                ..Frame::default()
            });
        };
        for _ in 0..40 {
            g.frame(Pad::Idle, Vert::Idle, false);
            feed(&g, Pad::Idle, Vert::Idle, false);
        }
        // Walk in each direction, in pure directions; between two legs a speech, paged with A
        // seven times, standing still.
        for (pad, vert, n) in [
            (Pad::Left, Vert::Idle, 30),
            (Pad::Right, Vert::Idle, 320),
            (Pad::Idle, Vert::Idle, 7 * 60),
            (Pad::Idle, Vert::Down, 420),
            (Pad::Idle, Vert::Up, 140),
            (Pad::Idle, Vert::Up, 120),
            (Pad::Left, Vert::Idle, 330),
            (Pad::Right, Vert::Idle, 330),
            (Pad::Idle, Vert::Down, 100),
        ] {
            for i in 0..n {
                let press = pad == Pad::Idle && vert == Vert::Idle && i % 60 < 3;
                g.frame(pad, vert, press);
                feed(&g, pad, vert, press);
            }
        }
        finder
    }

    #[test]
    fn a_top_down_game_names_the_player_by_walking_in_both_axes() {
        let o = run_top_down().observed();
        let cam = o.camera.expect("camera");
        assert_eq!((cam.x.offset, cam.y.map(|y| y.offset)), (0x20, Some(0x22)));
        // The half-speed word and the timer are not the player.
        let p = o.players.first().expect("a player");
        assert_eq!((p.lo, p.hi), (0x10, Some(0x11)), "{:?}", o.players);
        assert!(
            o.top_down,
            "the walk's Y is not the byte a button moves: a top-down game"
        );
        let y = o.ys.first().expect("a Y by walking");
        assert_eq!((y.offset, y.up), (0x12, -1));
        assert!(o.ready(), "ready with no jump window at all");
        assert!(!o
            .ys
            .iter()
            .any(|y| y.offset == 0x30 || y.offset == 0x60 || y.offset == 0x70));
    }

    /// A screen-edge transition scrolls the camera without the player; it must not hide him.
    /// W35-04: a camera word that stood still a few frames while a direction was held (a screen
    /// that flips: the scroll mirror holds while Link walks to the edge) passes the wall test but
    /// is not the player.
    #[test]
    fn the_cameras_own_word_is_not_the_player() {
        let mut finder = ScoutFinder::new(0x100, 1);
        let mut cam: i32 = 0;
        let mut ram = vec![0u8; 0x100];
        // A player whose X changes every frame but never with the camera (it flips by 256 in 64
        // frames when he reaches the edge): only the camera word follows the scroll register.
        for f in 0..2400u32 {
            let walking = f % 400 < 300;
            let flip = (300..364).contains(&(f % 400)) && f >= 300;
            if flip {
                cam += 4;
            }
            ram[0x10] = if walking { (f % 300) as u8 } else { 0 };
            // The game updates its mirror a frame before the register is written.
            let scroll = [ram[0xFD]];
            ram[0xFD] = cam as u8;
            finder.observe(&Frame {
                ram: &ram,
                pad: if walking || flip {
                    Pad::Right
                } else {
                    Pad::Idle
                },
                scroll: &[(&scroll, &[0])],
                scene: Some(1),
                ..Frame::default()
            });
        }
        let o = finder.observed();
        assert_eq!(o.camera.map(|c| c.x.offset), Some(0xFD));
        assert!(
            o.players.iter().all(|p| p.lo != 0xFD),
            "the camera word named as the player: {:?}",
            o.players
        );
    }

    #[test]
    fn a_scripted_scroll_does_not_hide_the_player() {
        let f = run_top_down();
        // 250-frame transitions put ~2 bursts of 24 camera-moving frames beside ~600 walking ones;
        // the player still clears the bar over the walk's frames.
        assert!(
            f.moving > f.moving_slow,
            "the bursts are camera-moving frames too"
        );
        let p = f.observed().players;
        assert_eq!(p.first().map(|p| p.lo), Some(0x10));
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
                vert: Vert::Idle,
                jump: 0,
                scroll: &[(&scroll, &[0])],
                scene: Some(scene),
                ..Frame::default()
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

    // ---- W35-05: the mode byte --------------------------------------------------------------

    /// A game with a mode byte (0x40: title 1, map 2, play 3), a pause flag (0x41) that leaves the
    /// mode alone, the data of a level (0x42: 7 in the first, 9 in the second), a byte that never
    /// changes (0x44) and twelve that tick while the game runs (0x60..0x6C: its frame counters).
    struct Modes {
        ram: Vec<u8>,
        t: u32,
        cam: u32,
        shown: u64,
    }

    impl Modes {
        fn new() -> Self {
            Self {
                ram: vec![0; 0x80],
                t: 0,
                cam: 0,
                shown: 1,
            }
        }

        /// One frame. `running`: the game ticks its counters and the picture moves. `scene`: the
        /// mode and the level data to write; `walking`: the camera follows.
        fn frame(&mut self, mode: u8, level: u8, paused: bool, running: bool, walking: bool) {
            self.t += 1;
            self.ram[0x40] = mode;
            self.ram[0x41] = u8::from(paused);
            self.ram[0x42] = level;
            self.ram[0x44] = 5;
            if running {
                for i in 0..12 {
                    self.ram[0x60 + i] = (self.t as u8).wrapping_mul(i as u8 + 3);
                }
                self.shown = self.shown.wrapping_add(0x9E37_79B9) | 1;
            } else {
                self.ram[0x60] = 0;
            }
            if walking && running {
                self.cam += 2;
            }
            self.ram[0x20] = self.cam as u8;
            self.ram[0x21] = (self.cam >> 8) as u8;
        }
    }

    /// Feed `n` frames of one state to the finder.
    #[allow(clippy::too_many_arguments)]
    fn feed_modes(
        f: &mut ScoutFinder,
        g: &mut Modes,
        n: u32,
        scene: u64,
        (mode, level): (u8, u8),
        (pad, start): (Pad, bool),
        (paused, running): (bool, bool),
    ) {
        for _ in 0..n {
            g.frame(mode, level, paused, running, pad == Pad::Right);
            let scroll = [g.ram[0x20]];
            f.observe(&Frame {
                ram: &g.ram,
                pad,
                scroll: &[(&scroll, &[0])],
                scene: Some(scene),
                start,
                shown: g.shown,
                ..Frame::default()
            });
        }
    }

    /// The whole session: a title (Start pressed), a map, a level walked in with a pause in the
    /// middle, a second level walked in, a speech in it.
    fn run_modes(pause: bool, speech: bool) -> ScoutFinder {
        let mut f = ScoutFinder::new(0x80, 1);
        let mut g = Modes::new();
        let go = (Pad::Right, false);
        let idle = (Pad::Idle, false);
        let live = (false, true);
        feed_modes(&mut f, &mut g, 150, 1, (1, 0), idle, live);
        feed_modes(&mut f, &mut g, 6, 1, (1, 0), (Pad::Idle, true), live);
        feed_modes(&mut f, &mut g, 120, 2, (2, 0), idle, live);
        feed_modes(&mut f, &mut g, 400, 3, (3, 7), go, live);
        if pause {
            // Start; the game freezes and stops ticking; Start again.
            feed_modes(
                &mut f,
                &mut g,
                6,
                3,
                (3, 7),
                (Pad::Idle, true),
                (true, false),
            );
            feed_modes(&mut f, &mut g, 80, 3, (3, 7), idle, (true, false));
            feed_modes(&mut f, &mut g, 6, 3, (3, 7), (Pad::Idle, true), live);
        }
        feed_modes(&mut f, &mut g, 300, 3, (3, 7), go, live);
        feed_modes(&mut f, &mut g, 100, 4, (3, 9), idle, live);
        feed_modes(&mut f, &mut g, 400, 4, (3, 9), go, live);
        if speech {
            // A speech box: the pad is held, the picture does not move, the memory idles.
            feed_modes(&mut f, &mut g, 80, 4, (3, 9), go, (false, false));
        }
        feed_modes(&mut f, &mut g, 200, 4, (3, 9), go, live);
        f
    }

    #[test]
    fn state_information_tells_a_separator_from_a_constant_and_a_counter() {
        // Three states, one value each: the value tells them apart and says nothing else.
        let (a, b, c): (Vec<u32>, Vec<u32>, Vec<u32>) =
            (vec![50, 0, 0, 0], vec![0, 50, 0, 0], vec![0, 0, 50, 0]);
        let (nmi, purity) = state_information(&[&a, &b, &c]);
        assert!(nmi > 0.99 && purity > 0.99, "{nmi} {purity}");
        // The same value in every state: nothing.
        let same = vec![50, 0, 0, 0];
        assert_eq!(state_information(&[&same, &same, &same]), (0.0, 0.0));
        // One state apart from two that are alike: a third of the answer.
        let (nmi, _) = state_information(&[&a, &b, &b]);
        assert!(nmi > 0.3 && nmi < 0.7, "{nmi}");
        // A counter: every state spreads over all the values; no value belongs to a state.
        let flat = vec![5; 32];
        let (nmi, purity) = state_information(&[&flat, &flat, &flat]);
        assert!(nmi < 0.01 && purity < 0.01, "{nmi} {purity}");
        // A counter that lands on different values in different states tells states apart by
        // accident, but most of its values belong to no state: low purity.
        let (x, y): (Vec<u32>, Vec<u32>) = (
            (0..32).map(|i| u32::from(i < 16)).collect(),
            (0..32).map(|i| u32::from(i >= 16)).collect(),
        );
        let (nmi, purity) = state_information(&[&x, &y]);
        assert!(nmi > 0.99, "{nmi}");
        assert!(purity < 0.25, "{purity}");
        // Fewer than two states: no information.
        assert_eq!(state_information(&[&a]), (0.0, 0.0));
        assert_eq!(state_information(&[]), (0.0, 0.0));
    }

    #[test]
    fn the_mode_byte_outranks_the_data_of_a_level() {
        let o = run_modes(true, false).observed();
        let first = o.gates.first().expect("a gate");
        // 0x40 reads 3 in both levels; 0x42 reads 7 in one and 9 in the other (a room's own
        // data), 0x44 never changes.
        assert_eq!((first.offset, first.value), (0x40, 3), "{:?}", o.gates);
        let data = o.gates.iter().find(|g| g.offset == 0x42);
        assert!(
            data.is_none_or(|d| d.mode_score + 0.1 < first.mode_score),
            "{:?}",
            o.gates
        );
        assert!(o.gates.iter().all(|g| g.offset != 0x44), "{:?}", o.gates);
        // The counters tick all the time: none is a gate.
        assert!(o.gates.iter().all(|g| !(0x60..0x6C).contains(&g.offset)));
    }

    #[test]
    fn a_pause_that_leaves_the_mode_alone_gets_a_guard_next() {
        let o = run_modes(true, false).observed();
        let (mode, guard) = (&o.gates[0], &o.gates[1]);
        assert_eq!(mode.offset, 0x40);
        // The mode byte does not change in the pause; the flag does, and nothing else is a
        // flag that holds 0 through play.
        assert!(mode.freeze_apart < MIN_FREEZE_APART, "{mode:?}");
        assert!(mode.frozen_frames >= MIN_FROZEN_FRAMES, "{mode:?}");
        assert_eq!((guard.offset, guard.value), (0x41, 0), "{:?}", o.gates);
        assert!(guard.freeze_apart >= MIN_FREEZE_APART, "{guard:?}");
        // Written as a row: its confidence clears the discovery's bar.
        assert!(guard.confidence >= 0.7, "{guard:?}");
        assert!(mode.confidence >= 0.7, "{mode:?}");
    }

    #[test]
    fn no_pause_means_no_guard() {
        let o = run_modes(false, false).observed();
        assert_eq!(o.gates[0].offset, 0x40);
        assert!(
            o.gates.iter().all(|g| g.frozen_frames == 0),
            "{:?}",
            o.gates
        );
        assert!(o.gates.iter().all(|g| g.offset != 0x41), "{:?}", o.gates);
    }

    #[test]
    fn the_frames_of_a_pause_are_not_held_against_the_mode_byte() {
        // 80 frames of pause inside the level, where a byte that reads the paused value
        // (0x41 = 1) would break the 95 % bar if they were counted as the level's.
        let with = run_modes(true, false).observed();
        let flag = with.gates.iter().find(|g| g.offset == 0x41).expect("0x41");
        assert!((flag.constancy - 1.0).abs() < 0.01, "{flag:?}");
    }

    #[test]
    fn start_in_a_menu_is_not_a_pause() {
        // Start pressed on a static title, twice: the picture holds, the memory idles; but no
        // one has played in that scene.
        let mut f = ScoutFinder::new(0x80, 1);
        let mut g = Modes::new();
        let idle = (Pad::Idle, false);
        feed_modes(&mut f, &mut g, 100, 1, (1, 0), idle, (false, true));
        feed_modes(
            &mut f,
            &mut g,
            6,
            1,
            (1, 0),
            (Pad::Idle, true),
            (false, false),
        );
        feed_modes(&mut f, &mut g, 60, 1, (1, 0), idle, (false, false));
        feed_modes(
            &mut f,
            &mut g,
            6,
            1,
            (1, 0),
            (Pad::Idle, true),
            (false, true),
        );
        assert!(
            f.paused.is_none() && f.staging.is_none(),
            "a pause in a menu"
        );
    }

    #[test]
    fn a_start_the_game_ignores_is_not_a_pause() {
        // Start in a level: the picture happens to hold (a standing player), but the game keeps
        // counting its frames: it did not pause.
        let mut f = ScoutFinder::new(0x80, 1);
        let mut g = Modes::new();
        let (go, idle) = ((Pad::Right, false), (Pad::Idle, false));
        feed_modes(&mut f, &mut g, 200, 3, (3, 7), go, (false, true));
        // The picture stands still but the counters tick.
        for _ in 0..60 {
            g.frame(3, 7, false, true, false);
            let shown = g.shown;
            f.observe(&Frame {
                ram: &g.ram,
                scene: Some(3),
                start: g.t % 60 < 6,
                shown: 77,
                ..Frame::default()
            });
            g.shown = shown;
        }
        feed_modes(&mut f, &mut g, 60, 3, (3, 7), idle, (false, true));
        assert!(f.paused.is_none() && f.staging.is_none(), "{:?}", f.pause);
    }

    #[test]
    fn a_speech_that_holds_the_pad_is_a_state_of_its_own() {
        let f = run_modes(false, true);
        let speech = f.ignored.as_ref().map_or(0, |h| h.frames);
        assert!(speech >= 40, "speech frames {speech}");
        // Its frames are not the level's: the mode byte still holds on all of them.
        let o = f.observed();
        let mode = o.gates.iter().find(|g| g.offset == 0x40).expect("0x40");
        assert!(mode.speech_frames >= MIN_FROZEN_FRAMES, "{mode:?}");
    }

    #[test]
    fn a_picture_hash_follows_the_picture() {
        use rf_core_api::PixelLayer;
        let px = |i: u8| PpuPixel {
            palette_index: i,
            layer: PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
        };
        let (a, b) = (vec![px(1), px(2), px(3)], vec![px(1), px(2), px(4)]);
        assert_eq!(picture_hash(&a), picture_hash(&a.clone()));
        assert_ne!(picture_hash(&a), picture_hash(&b));
        assert_ne!(picture_hash(&[]), 0);
    }
}
